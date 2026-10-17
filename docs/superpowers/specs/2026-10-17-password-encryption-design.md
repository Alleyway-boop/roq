# profiles.d 密码字段加密（DPAPI 内嵌）设计

日期：2026-10-17
状态：builder check 已过，待用户批准

## 意图与背景

roq 的 profile 密码目前以明文存于 `~/.roq/profiles.conf` / `~/.roq/profiles.d/*.conf`。两个目标威胁：

1. **上下文沾染**：AI agent 顺手读取配置（帮用户答疑、整理），明文密码随之进入其上下文（transcript / API 请求 / 会话存档）。
2. **文件泄漏**：home 目录被网盘同步、备份打包、dotfiles 误提交、配置误贴——文件漏出去即密码漏出去。

经 10 路 agent 发散 + 红队排序，用户选定**形态二：密文内嵌 INI**（放弃"密码进 OS 凭据库、INI 留引用"的形态一），理由是保留"单文件配置即完整配置"的产品惯例（换机/迁移时配置文件自包含）。

## 威胁模型边界（必须写进文档与 SKILL）

本设计**防住**：顺手读取（agent 被动 cat/grep 配置）、非定向文件泄漏（备份/同步/误贴——密文离开本机本用户即废）。

本设计**不防**：同用户同机的蓄意代码提取（一行 PowerShell `ProtectedData::Unprotect` 即解，OS 边界在账户不在进程）；配置密码经 `--password` argv 进入 agent 上下文（配置时刻的泄漏，静态加密无法覆盖）。

**真正的墙在 DB 端**：GRANT SELECT ONLY 的只读账号，密码全漏也写不动数据。文档需增补此建议（当前 README/SKILL 完全未提）。

## 目标

- Windows：`config add` 默认将密码以 DPAPI（CurrentUser）加密后内嵌写回 INI 的 `password` 字段
- 读侧兼容旧明文配置（连接时仅对实际使用的 profile 打 stderr 安全警告），解密失败 fail-closed 报错并指引 `config add --update` 重新录入
- 新增 `config add --update`：显式覆盖已存在配置，作为密文重录的官方路径（builder check 发现：防覆盖规则使存量同名配置无法"重新 add"，remove 仅支持单节文件，`--file` 换文件重加还会制造跨文件重名）
- roq 任何输出面（审计日志、错误信息、diff、config 展示）绝不回显已解密密码
- 补齐配套：profiles.d 文件权限 0600（Unix）、README/SKILL 增补"只读账号 + password_env"指引

## 非目标 / 反目标

- **不做** macOS/Linux 的加密写入（v1）：macOS 无 DPAPI 等价的 blob API（Keychain 走 KEK+AES 双层，经评估复杂度不匹配收益）；Linux headless 无可靠密钥来源（内核 keyring 重启即失，secret-service 服务器常缺）。两平台 v1 引导 `password_env`（CI/服务器天然契合），`config add` 在这些平台收到 `--password` 时明确报"此平台暂不支持密码加密，请改用 --password-env"，**禁止静默回退明文**
- **不做**常驻凭据代理 / systemd 服务（违反"不产生文件"与绿色单二进制定位）
- **不做**白盒、内置固定 pepper、机器指纹派生（开源仓库下等于零安全，且制造虚假安心）
- **不触碰** gate.rs 三层只读保障（本改动仅涉及凭据读写路径）
- 加密不得削弱现有可用性：老配置不解密也必须能被"看见"（--list 正常），只是查询时 fail-closed

## 约束

- 单二进制 ~2.5MB、跨平台 CI 三平台构建（非 Windows 平台代码必须可编译）
- 不新增重量级依赖：DPAPI 走手写 FFI（crypt32.dll，约 80 行 unsafe，与 roq 手写解析风格一致），不引 windows/keyring crate
- 用户可见文本一律中文；fail-closed 哲学与 gate.rs 一致
- 防覆盖约定（CLAUDE.md）：`config add` 写入密文后回读解密校验，拒绝静默覆盖
- SKILL.md 由 include_str! 嵌入公开分发——只写通用安全建议，不写本机路径与内网拓扑

## 已定设计

### 密文格式

```ini
password = enc:v1:dpapi:<base64(CryptProtectData 的 blob)>
```

- `enc:v1` 版本号 + `dpapi` 方法名，为将来其他平台/算法留路由空间（如 `enc:v2:aesgcm`）
- DPAPI blob 约 200–300 字节，base64 后约 400 字符，满足 INI 单行无换行约束
- 调用参数：`CryptProtectData` / `CryptUnprotectData`，`dwFlags = CRYPTPROTECT_UI_FORBIDDEN`（防服务/计划任务会话挂起弹框），CurrentUser scope（**不用** LOCAL_MACHINE——那会让同机其他账户也能解），附加 entropy `b"roq-profile-v1"`（防其他工具的 DPAPI blob 被误读/误解，开源下不构成对攻击者的防护，文档如实标注）
- 非 Windows 平台读到 `enc:v1:dpapi:` → 报错"此密文由 Windows DPAPI 加密，当前平台无法解密，请在 Windows 上 config add --update 重录或改用 password_env"

### 读写路径

- **写**（`cmd_config.rs`）：Windows 上 `--password` 收到明文 → DPAPI 加密 → base64 内嵌写文件 → 回读解密与原值比对（防覆盖回读校验约定）
- **读**（`config.rs::resolved_password`）：值带 `enc:v1:` 前缀 → 按方法解密；无前缀 → 视为明文，stderr 打一行安全警告（如"[roq] 警告：profile xxx 密码为明文存储，建议 config add --update 重新录入加密"）。警告挂在 resolved_password 层（**连接时**，只警告实际用到的 profile，`--list`/`config list` 不触发，避免 agent 场景 stderr 噪音）；profile 名由调用方传入（`query.rs`/`cmd_diff.rs` 手里有名），`Profile` 结构体不加字段。安全警告**豁免 --quiet**（quiet 只抑制提示，不抑制安全警告——与 prod 提醒语义不同，需在代码注释说明）
- 解密失败（换机/换用户/entropy 不符）→ fail-closed：报"此密文仅限原机原用户解密，请 config add --update 重新录入"，退出码走 1（配置类错误），不回退明文猜测

### 输出面收敛（公共底座）

- `format_profiles`（--list / config list）现状已不打印密码，保持
- 审计日志、错误信息、diff 输出一律不得出现已解密密码；新增测试断言：构造含已知密码的 profile，跑全子命令，断言所有输出不含该密码串
- MySQL 连接错误信息可能含账号上下文，检查 `query.rs` 错误路径不含 pass

### 交互式读入（小项，防 argv 治理）

**仅 Windows**：TTY 存在且未提供 `--password`/`--password-env` 时，`config add` 用 rpassword 隐藏回显提示输入（人类用户路径；agent 路径照旧传参）。rpassword 为小型纯终端 crate，依赖代价可接受。非 Windows 平台**不做**交互提示——收了密码也无法加密落盘（踩"禁止静默回退明文"红线），无参 TTY 场景与收到 `--password` 同样报错引导 `--password-env`。

### config add --update（重录官方路径）

- `--update` 显式覆盖已存在的同名节：防覆盖约定拒绝的是**静默**覆盖，`--update` 是知情操作，不违反哲学
- 覆盖写入后同样做回读解密校验
- add（含 `--update`）写入前做**全局**同名预检（`load_config_sources` 加载全部来源），堵住 `--file` 指到别的文件重加同名、之后所有命令因跨文件冲突报错的坑
- 所有 fail-closed 报错文案统一指引此命令

### 配套修补

- `cmd_config.rs` 写配置文件时 Unix 下 chmod 0600（现状默认 umask 022 → 0644，其他本机用户可读）
- README + SKILL.md 增补：**强烈建议为 roq 专用账号授予 GRANT SELECT ONLY**；`password_env` 提升为 Linux/CI 的推荐路径；加密防什么/不防什么的边界声明
- 版本号进 0.10.0

### 测试计划

- `#[cfg(windows)]` DPAPI round-trip（加密→解密等值）
- 解析：`enc:v1:dpapi:` 前缀识别、坏 base64、非 Windows 平台读 dpapi 密文的报错文案
- 明文兼容：无前缀走明文 + 警告出现且不被 --quiet 抑制
- `config add`：写密文、回读校验、重复 add 拒绝覆盖行为不变；`--update` 覆盖已存在节并回读校验；跨文件同名预检报错
- 非 Windows：`--password` 与无参 TTY 均报错引导 `--password-env`，不落盘明文
- 输出面断言：全子命令输出不含明文密码（安全回归测试，防未来改动回显）
- 三平台 CI 编译通过（非 Windows 分支 cfg 正确）

## 留给实现者

- FFI 具体封装位置（建议新文件 `dpapi.rs`，`#[cfg(windows)]`）与错误类型选择
- `enc:v1` 解析放 `config.rs` 哪一层（建议 `resolved_password` 内部，保持 `Profile` 结构体对外不变）
- 警告文案措辞（触发粒度已定：每次 resolve 一行，仅当前 profile）
- CHANGELOG/用法文本更新
