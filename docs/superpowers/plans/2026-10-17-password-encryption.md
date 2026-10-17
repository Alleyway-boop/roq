# profiles.d 密码加密（DPAPI 内嵌）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** profile 密码在 Windows 上经 DPAPI 加密内嵌 INI，配套 `config add --update` 重录路径、非 Windows 平台引导 `password_env`、输出面永不回显密码。

**Architecture:** 读侧在 `Profile::resolved_password` 识别 `enc:v1:dpapi:` 前缀走解密；写侧 `cmd_config::add` 在 Windows 调手写 FFI（`src/dpapi.rs`，仅 `#[cfg(windows)]`）加密后写文件并回读解密校验；`--update` 与加密完全解耦先行落地。base64 编解码为跨平台纯函数模块 `src/b64.rs`（读侧非 Windows 也要识别坏 base64）。

**Tech Stack:** Rust（std-only + 新增 rpassword），Windows DPAPI（crypt32.dll 手写 FFI，不引 windows/keyring crate）。

**Spec:** `docs/superpowers/specs/2026-10-17-password-encryption-design.md`（计划从 spec 立论，执行者两份都读）

## 前置

从当前 HEAD 创建 feature 分支 `feat/password-encryption`（用户要求：不直进 master，合并与否验收后再定）。specs 与 plans 两份文档随本分支首个提交入库。每个任务开始前 `cargo test` 基线全绿。

## Global Constraints

- 用户可见文本一律中文；代码标识符英文（CLAUDE.md）
- **不触碰 gate.rs**；三层只读保障不得削弱（红线 1）
- 退出码契约不变：0 成功 / 1 用法配置错 / 2 闸门 / 3 连接 / 4 diff；解密失败走 1
- 密文格式字面量：`enc:v1:dpapi:`；DPAPI entropy 字节串 `b"roq-profile-v1"`；flags `CRYPTPROTECT_UI_FORBIDDEN`（0x1）；CurrentUser scope（**禁止** `CRYPTPROTECT_LOCAL_MACHINE`）
- DPAPI 内存释放必须 `LocalFree`；手写 FFI 不引 crate
- 非 Windows 代码必须可编译（CI 三平台）；加密逻辑全部 `#[cfg(windows)]`
- 唯一新依赖 rpassword（仅 Windows 交互路径使用）
- SKILL.md 由 `include_str!` 公开分发——增补只写通用建议，不写本机路径/内网拓扑（红线 6）
- 版本号进 0.10.0
- 每任务 TDD：先测后码，`cargo test` 全绿才提交；提交信息 `<type>: <描述>` 格式

## Review Focus

spec 未逐一列出、但最可能咬人的五类输入/失败模式（每条已钉进所属任务的测试）：

1. **base64 的 `=` padding 撞 INI 的 `=` 分隔**——`config.rs` 的 `split_once('=')` 只切第一个等号，密文值必须能无损往返。→ Task 3 测试 `enc_value_roundtrips_through_ini_parser`
2. **坏/篡改 blob 进 `unprotect`**——不得 panic 或 UB，必须干净 Err。→ Task 2 测试 `unprotect_garbage_blob_fails_cleanly` / `unprotect_tampered_blob_fails`
3. **`--update` 语义边界**——节不存在时不得静默新增（防打错名字），同名存在于别的文件时必须拒绝而非制造全局冲突。→ Task 1 测试 `update_without_existing_section_fails` / `add_rejects_name_existing_in_other_file`
4. **明文密码含 `=` 等特殊字符的既有行为不回归**。→ Task 3 测试 `plaintext_password_with_equals_survives_ini_roundtrip`
5. **密码经成功消息/错误文案不经意回显**。→ Task 5 回归测试 `no_output_surface_ever_contains_plaintext_password`

---

### Task 1: `config add --update` 与跨文件同名预检、0600 权限

**Files:**
- Modify: `src/cli.rs:95-98`（`ConfigCmd::Add` 结构体加 `update: bool` 字段）、`src/cli.rs:300-391`（`parse_config_cmd` 解析 `--update`）、`src/cli.rs:284-298`（`config_usage` 文本）
- Modify: `src/cmd_config.rs:27-47`（run 分发传参）、`src/cmd_config.rs:78-149`（`add` 函数）
- Test: `src/cmd_config.rs` tests 模块、`src/cli.rs:636` tests 模块

**Interfaces:**
- Produces: `fn add(name: &str, host: &str, port: u16, user: &str, password: Option<&str>, password_env: Option<&str>, database: &str, ssl: bool, file: Option<PathBuf>, update: bool) -> Result<String, String>`（`update` 为新增末位参数，后续任务直接沿用）
- Produces: `ConfigCmd::Add { ..., file, update }`
- Produces: 私有 `fn locate_existing_section(name: &str, target: &Path, extra_sources: &[PathBuf]) -> Result<Option<PathBuf>, String>`——返回同名节所在文件（`extra_sources` 供测试注入，生产调用传 `default_config_files()?`；任何源文件解析失败按"不存在"跳过，与 remove 的容错一致）

- [ ] **Step 1: 写失败测试**（`src/cmd_config.rs` tests）

```rust
#[test]
fn update_replaces_section_and_keeps_others() {
    // temp 文件先 add("a1",..) + add("a2",..)，再 add("a1", 新值, update=true)
    // 断言：load_profiles 后 a1 为新值、a2 不变、文件头 "# roq 连接配置" 注释保留
    // 断言：Ok 消息含 "回读校验通过"
}
#[test]
fn update_without_existing_section_fails() {
    // 对不含 [x] 的文件 add("x", .., update=true)
    // 断言 Err 含 "不存在" 且含 "去掉 --update"
}
#[test]
fn add_rejects_name_existing_in_other_file() {
    // 文件 A add("dup",..)；再向文件 B add("dup",.., update=false)
    // locate_existing_section 经 extra_sources=[A] 预检——add 需把预检结果暴露给测试：
    // 实现为 add 内部调用 locate_existing_section(name, &target, &sources)，sources 生产默认、测试经包装函数 add_with_sources 注入
    // 断言 Err 含 "已定义于"（指向 A 的路径）
}
```

注：为可测性加一个 `#[cfg(test)]` 可见的薄包装 `fn add_with_sources(..., sources: &[PathBuf])`，`add` 调它传默认源。

- [ ] **Step 2: 跑测试确认失败**：`cargo test --lib cmd_config` → 新测试 FAIL（无 update 参数）
- [ ] **Step 3: 实现**——`cli.rs`：match 加 `"--update" => update = true`；结构体与分发加字段；`config_usage` 的 add 行加 `[--update]`，说明行改为"同名已存在时拒绝，重录请用 config add --update 覆盖"。`cmd_config.rs`：目标文件存在且含同名节时，`update=true` 则做**节替换**（读原文，定位 `\n[名]` 行首截到下一个 `\n[` 或文件尾，替换为新节文本；文件在节前有内容时保首尾空白规整），`update=false` 维持现有报错；`update=true` 但节不存在 → Err；写入前调 `locate_existing_section`，同名在**非目标**文件 → Err 指明那个文件。写入后 `#[cfg(unix)]` `fs::set_permissions(&target, Permissions::from_mode(0o600))`（用 `std::os::unix::fs::PermissionsExt`）
- [ ] **Step 4: 跑测试确认通过**：`cargo test`（全量）
- [ ] **Step 5: 提交**：`git add -A && git commit -m "feat: config add --update 覆盖重录、跨文件同名预检、Unix 0600 权限"`

### Task 2: `src/dpapi.rs` 手写 DPAPI FFI

**Files:**
- Create: `src/dpapi.rs`（整文件 `#[cfg(windows)]`）
- Modify: `src/lib.rs:14-23`（`pub mod dpapi;`）
- Test: `src/dpapi.rs` 内 `#[cfg(test)] #[cfg(windows)]` 模块

**Interfaces:**
- Produces: `pub fn protect(plain: &str) -> Result<Vec<u8>, String>`（CRYPTPROTECT_UI_FORBIDDEN，entropy `b"roq-profile-v1"`，CurrentUser 默认 scope）
- Produces: `pub fn unprotect(blob: &[u8]) -> Result<Vec<u8>, String>`

- [ ] **Step 1: 写失败测试**

```rust
#[test] fn roundtrip_protect_unprotect() { assert_eq!(unprotect(&protect("s3cret-密码").unwrap()).unwrap(), b"s3cret-密码"); }
#[test] fn unprotect_garbage_blob_fails_cleanly() { assert!(unprotect(b"garbage").is_err()); } // 不 panic
#[test] fn unprotect_tampered_blob_fails() { /* protect 后翻转 1 字节再 unprotect → Err */ }
```

- [ ] **Step 2: 跑测试确认失败**：`cargo test --lib dpapi` → 模块不存在编译错
- [ ] **Step 3: 实现**——`#[repr(C)] struct Blob { cb_data: u32, pb_data: *mut u8 }`；`extern "system"` 声明 `CryptProtectData`/`CryptUnprotectData`/`LocalFree`（签名按 MSDN：pDataIn/pOptionalEntropy/pszDataDescr/pPromptStruct/dwFlags/pDataOut 六参形态）；错误用 `std::io::Error::last_os_error()` 转中文 Err；**所有出参 blob 用完必须 LocalFree**（RAII 小结构或 drop guard，防中途 return 泄漏）。Windows API 接受 UTF-16 描述串，`pszDataDescr` 传 null 即可
- [ ] **Step 4: 跑测试确认通过**：`cargo test`（Windows 本机与 CI windows runner 上 DPAPI CurrentUser 可用，真跑）
- [ ] **Step 5: 提交**：`git add src/dpapi.rs src/lib.rs && git commit -m "feat: 手写 DPAPI FFI（protect/unprotect，UI_FORBIDDEN + entropy）"`

### Task 3: 读侧——`enc:v1:dpapi:` 前缀解析 + base64 模块 + 明文警告

**Files:**
- Create: `src/b64.rs`（跨平台纯函数）
- Modify: `src/lib.rs`（`pub mod b64;`）
- Modify: `src/config.rs:26-34`（`resolved_password`）、`src/config.rs` tests（现有两处调用点补 name 参数）
- Modify: 调用点 `src/query.rs:141`、`src/cmd_diff.rs:153`、`src/cmd_config.rs:194-196`
- Test: `src/b64.rs`、`src/config.rs` tests 模块

**Interfaces:**
- Produces: `pub const ENC_PREFIX: &str = "enc:v1:dpapi:";`（`config.rs` 顶部常量，Task 4 写侧复用）
- Produces: `pub fn resolved_password(&self, name: &str) -> Result<String, String>`——password 值以 `ENC_PREFIX` 开头：Windows 上 `b64::decode` 剩余部分 → `dpapi::unprotect` → `String::from_utf8`；非 Windows 返回 Err("此密文由 Windows DPAPI 加密，当前平台无法解密，请在 Windows 上 config add --update 重录或改用 password_env")。无前缀：`eprintln!` 警告后返回明文。**警告豁免 quiet**（直接 eprintln，不走 notice 通道），文案由纯函数 `pub fn plaintext_warning(name: &str) -> String` 生成（"[roq] 警告：profile {name} 密码为明文存储，建议 config add --update 重新录入加密"）以便测试
- Produces: `pub fn encode(data: &[u8]) -> String` / `pub fn decode(text: &str) -> Result<Vec<u8>, String>`（RFC 4648 标准字母表 + padding）

- [ ] **Step 1: 写失败测试**

```rust
// b64.rs：RFC 4648 已知向量
#[test] fn known_vectors() { encode(b"foobar") == "Zm9vYmFy"; encode(b"foob") == "Zm9vYg=="; decode("Zm9vYg==") == b"foob"; decode("!!") is Err; }
// config.rs
#[test] fn enc_value_roundtrips_through_ini_parser() {
    // windows：load_profiles 读含 password=enc:v1:dpapi:<b64(protect("s3cret"))> 的 temp 文件 → resolved_password("e") == "s3cret"
}
#[test] fn enc_prefix_on_nonwindows_fails_closed() { /* cfg(not(windows))：假密文 → Err 含 "无法解密" */ }
#[test] fn bad_base64_in_enc_value_fails() { /* "enc:v1:dpapi:!!!" → Err 含 base64 报错 */ }
#[test] fn plaintext_warning_text() { /* 文案含 name 与 "--update" */ }
#[test] fn plaintext_password_with_equals_survives_ini_roundtrip() { /* password=a=b 的 INI 读回为 "a=b"（既有行为钉住） */ }
```

- [ ] **Step 2: 跑测试确认失败**
- [ ] **Step 3: 实现**——`resolved_password` 改签名并按上述逻辑分流；`password_env` 路径行为不变、不警告；三个调用点传 profile 名（各处手里均有 `name`/`cli.profile`）
- [ ] **Step 4: 跑测试确认通过**：`cargo test` 全量 + 目视确认非 Windows 编译路径（本地为 Windows 时靠 `cargo check --target` 不可行，则以 cfg 分支自查 + CI 兜底）
- [ ] **Step 5: 提交**：`git commit -m "feat: 读侧识别 enc:v1:dpapi 密文，明文警告与跨平台 fail-closed"`

### Task 4: 写侧——add 默认加密、回读解密校验、rpassword、非 Windows 拒绝

**Files:**
- Modify: `Cargo.toml`（+ `rpassword`；版本 0.10.0 留 Task 5 一起）
- Modify: `src/cmd_config.rs` add 函数与 `#[cfg(test)]` 包装、`src/cli.rs` usage
- Test: `src/cmd_config.rs` tests

**Interfaces:**
- Consumes: Task 1 `add(..., update)`、Task 2 `dpapi::protect`、Task 3 `ENC_PREFIX` / `b64::encode` / `resolved_password(&self, name)`
- Produces: 无新对外接口（行为变化）

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(windows)] #[test] fn add_writes_encrypted_password() {
    // add("enc1", password=Some("s3cret!"), file=temp)
    // 断言：文件文本含 "password=enc:v1:dpapi:" 且不含 "s3cret!"
    // 断言：load_profiles + resolved_password("enc1") == "s3cret!"
}
#[cfg(not(windows))] #[test] fn add_with_password_rejected_on_nonwindows() {
    // Err 含 "password_env"（引导文案）
}
#[cfg(windows)] #[test] fn update_reencrypts_in_place() { /* 明文旧节 --update 后变密文 */ }
```

- [ ] **Step 2: 跑测试确认失败**
- [ ] **Step 3: 实现**——add 内 `secret_line` 分流：非 Windows 收到 `Some(plain)` → `Err("此平台暂不支持密码加密，请改用 --password-env（环境变量名），密码不落盘")`；Windows：`format!("password={}{}\n", ENC_PREFIX, b64::encode(&dpapi::protect(plain)?))`。回读校验升级：Windows 且密码路径时，回读后调 `resolved_password(name)` 比对原明文相等（真解密校验）。rpassword：Windows 且 `(password.is_none() && password_env.is_none())` 且 `std::io::stdin().is_terminal()` 时 `rpassword::prompt_password("密码：")` 取值（空输入视为未提供走缺参报错）；非 Windows 无参 TTY 一律走缺参报错（文案同上引导）。`config_usage` 更新说明
- [ ] **Step 4: 跑测试确认通过**：`cargo test`
- [ ] **Step 5: 提交**：`git commit -m "feat: Windows config add 默认 DPAPI 加密+回读解密校验，非 Windows 引导 password_env"`

### Task 5: 输出面回归测试、文档、版本 0.10.0

**Files:**
- Test: `src/cmd_config.rs` tests（回归）
- Modify: `README.md`（配置节）、`skill/roq/SKILL.md`（注册节：--update 与 password_env 建议；新增"只读账号"建议段）、`Cargo.toml`（version = "0.10.0"）

**Interfaces:** Consumes 前四个任务全部产物。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(windows)] #[test] fn no_output_surface_ever_contains_plaintext_password() {
    // add("leak", password=Some("UNIQUE-PWD-xyz"), file=temp)
    // 断言 add 的 Ok 消息、format_profiles(load_profiles(..)) 输出、remove 的消息均不含 "UNIQUE-PWD-xyz"
    // 坏密文场景：手写 password=enc:v1:dpapi:AAAA 的节，resolved_password 的 Err 文案不含任何明文形态
}
```

- [ ] **Step 2: 跑测试确认失败/通过情况如实记录**
- [ ] **Step 3: 实现补漏**（若断言失败即输出面有回显，修对应输出；预期 add/list/remove 本就干净，此测试为钉住）
- [ ] **Step 4: 文档**——README 配置节增补：加密行为说明、"防顺手读取与文件泄漏，不防同用户蓄意提取，真正的墙是 GRANT SELECT ONLY 只读账号"、Linux/CI 推荐 `--password-env`、`--update` 用法行；SKILL.md 注册节同步 `--update` 与 password_env 描述，新增通用只读账号建议（不含任何本机/内网信息）；`Cargo.toml` 版本 0.10.0
- [ ] **Step 5: 全量验证**：`cargo test` 全绿 + `cargo build --release`，`cp target/release/roq.exe ./roq.exe`
- [ ] **Step 6: 提交**：`git commit -m "feat: 密码加密收尾——输出面回归钉住、文档与只读账号指引、v0.10.0"`

## Self-Review 记录

- 覆盖核对：spec 的密文格式/读写路径/--update/交互读入/0600/输出面/文档/版本/测试计划均有对应任务；"留给实现者"三条（FFI 封装位置→Task 2、解析层→Task 3、警告文案→Task 3）已收敛为明确决策
- 类型一致：`add` 末位 `update: bool` 在 Task 1 定义、Task 4 沿用；`resolved_password(&self, name)` 三调用点均在 Task 3 列出
- 体积对照：计划约为 spec 的 1.2 倍，无代码转录
