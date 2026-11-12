# 保存查询（roq q）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 新增 `roq q` 子命令族——保存高频 SQL 模板（`{{名}}` 占位符）到 `~/.roq/queries.d/`，按名填参执行，替换后 SQL 走与手写完全相同的闸门+审计+存档链路。

**Architecture:** 纯函数层（`saved_query.rs`：占位符解析/单遍替换/参数校验/文件管理）+ 管理子命令（`cmd_q.rs`）+ 解析（`cli.rs` 的 `QCmd`）+ 执行接线（`main.rs` 薄胶水，镜像 `run_query` 既有顺序）。审计经既有 `extra` 自由片段扩展，存档 slug 链前置 saved 名。

**Tech Stack:** Rust 单二进制（无新依赖）；单元测试沿用各模块 `#[cfg(test)] mod tests` 惯例。

**Spec:** `docs/superpowers/specs/2026-11-12-saved-queries-design.md`（实现者必须与本文档一并阅读）

## Global Constraints

- 红线 5：替换后的 SQL 必须走 `crate::gate::gate()` 同一链路，不得绕过；保存时预检不豁免运行时闸门
- 红线 4：审计不可关闭；替换前失败记 `param-error`（新 outcome 类别），与 `gate-rejected` 同为被拦尝试
- 退出码：0 成功 / 1 用法配置（含 param-error）/ 2 闸门 / 3 连接执行 / 4 diff，不新增
- CLI 可见文本与代码注释一律中文，代码标识符英文
- 防覆盖：`q add` 同名拒绝静默覆盖（`--update` 重录），写后回读校验
- SKILL.md 不得写入本机路径或内网库拓扑（红线 6，仓库公开分发）
- README.md（英文）与 README.zh-CN.md（中文）两份必须同步改动
- `usage()` 注释竖向对齐至 56 列惯例（commit 715df64 先例）
- 每个任务收尾 `cargo test` 全绿才能提交（含闸门注入负例）

## Review Focus

规格隐含但无任务测试直接覆盖、最可能咬到使用者的五类输入（前四类已钉测试，第五类为终审走查项）：

1. **参数值含 `{{`（宏注入尝试）**——期望整体报错，绝不静默展开 → Task 2 `substitute_rejects_placeholder_syntax_in_value`
2. **漏传参数**——`'{{id}}'` 残留会静默空结果，期望缺参报错并列出所需参数名 → Task 2 `match_params_reports_missing_names`
3. **值含 `'` 或 `\`**——期望显式报错并提示 `''` 转义，绝不静默改义 → Task 2 `validate_param_value_rejects_quote_and_backslash`
4. **模板静态部分含禁词**——期望 `q add` 即拒绝（哑参预检）而非运行时才发现 → Task 5 `add_rejects_template_with_static_forbidden_word`
5. **保存 ≠ 永久放行**——闸门演进或参数值引入禁词（如值 `DELETE` 落入字符串字面量）时，运行时必须拦（exit 2 且审计带 `saved`）。执行胶水在 `main.rs`（本仓惯例无测试），**整支终审必须走查该函数的 gate 调用位于替换之后、审计带 saved 上下文**

---

### Task 1: `saved_query.rs`——查询名校验与占位符提取

**Files:**
- Create: `src/saved_query.rs`
- Modify: `src/lib.rs`（注册 `pub mod saved_query;`）

**Interfaces:**
- Consumes: 无（纯函数，无 I/O）
- Produces:
  - `pub fn validate_query_name(name: &str) -> Result<(), String>` —— 非空、仅 `[A-Za-z0-9_-]`、不得以 `-` 开头、不得在保留名表中
  - `pub const RESERVED_QUERY_NAMES: &[&str]` —— `["config","log","skill","diff","tables","schema","columns","explain","q","add","list","show","remove","help"]`
  - `pub fn extract_placeholder_names(template: &str) -> Result<Vec<String>, String>` —— 提取全部 `{{名}}`，名限 `[A-Za-z0-9_]+`；按首现顺序去重；`{{` 未闭合 / 名含非法字符 / 名为空 → Err（报错含位置描述）

- [ ] **Step 1: 写失败测试**（`src/saved_query.rs` 测试模块）

```rust
#[test] fn validate_query_name_accepts_safe_names() // "q-by-id"、"t_cnt"、"A_1" 通过
#[test] fn validate_query_name_rejects_unsafe()     // ""、"a/b"、"a b"、"-x"、保留名逐个、"帮助"（非 ASCII）
#[test] fn extract_placeholder_names_orders_and_dedups() // "SELECT {{b}} '{{a}}' {{b}}" → ["b","a"]
#[test] fn extract_placeholder_names_rejects_malformed() // "{{"（未闭合）、"{{ a}}"（空格）、"{{a-b}}"（非法字符）、"{{}}"（空名）→ Err
#[test] fn template_without_placeholders_ok()       // "SELECT 1" → Ok(vec![])
```

- [ ] **Step 2: 跑测试确认失败**：`cargo test saved_query::` → 编译失败（函数未定义）
- [ ] **Step 3: 实现两个函数与常量**（扫描逻辑：遇 `{{` 找 `}}`，名段校验字符集；无 I/O）
- [ ] **Step 4: 跑测试确认通过**：`cargo test saved_query::`
- [ ] **Step 5: Commit** `git add src/saved_query.rs src/lib.rs && git commit -m "feat: 保存查询名校验与占位符提取"`

### Task 2: `saved_query.rs`——单遍替换引擎与参数解析（安全核心）

**Files:**
- Modify: `src/saved_query.rs`

**Interfaces:**
- Consumes: Task 1 的 `extract_placeholder_names`；`crate::audit::json_escape`（pub）
- Produces:
  - `pub struct SavedContext { pub name: String, pub params_json: String }`（`params_json` 形如 `{"id":"123"}`，k 按模板序，值经 `json_escape`；无参数为 `{}`）
  - `pub fn substitute(template: &str, params: &[(String, String)]) -> Result<String, String>` —— **单遍**扫描替换，替换产物不再扫描；params 未命中的名 → Err；输出做 `{{` 残留断言（防值注入占位符语法）
  - `pub fn dummy_substitute(template: &str) -> Result<String, String>` —— 全部占位符以 `"1"` 替换（保存预检用）
  - `pub fn validate_param_value(name: &str, value: &str) -> Result<(), String>` —— 拒绝含 `'` 或 `\` 的值，报错提示 `''` 转义
  - `pub fn parse_param_arg(arg: &str) -> Result<(String, String), String>` —— 首个 `=` 切分，k/v 均非空
  - `pub fn match_params(names: &[String], params: &[(String, String)]) -> Result<Vec<(String, String)>, String>` —— 重复 k / 缺参（列出所需名）/ 多参（列出多余名）一律 Err；成功返回按模板序排列
  - `pub fn prepare_execution(template: &str, param_args: &[String]) -> Result<(String, String), String>` —— 编排：逐个 `parse_param_arg` → `match_params` → 逐个 `validate_param_value` → `substitute`；返回 `(替换后 SQL, params_json)`

- [ ] **Step 1: 写失败测试**

```rust
#[test] fn substitute_replaces_by_name_and_allows_reuse() // "WHERE id='{{id}}' OR ref='{{id}}'" + [("id","5")] → 两处均 5
#[test] fn substitute_rejects_placeholder_syntax_in_value() // 值 "{{x}}" → Err（输出残留断言拦截，宏注入不得静默展开）
#[test] fn substitute_reports_unknown_placeholder()          // 模板含 {{a}}，params 无 a → Err 含 "a"
#[test] fn dummy_substitute_yields_gate_clean_sql()          // dummy 后过 crate::gate::gate() 成功（用合法模板断言）
#[test] fn validate_param_value_rejects_quote_and_backslash() // "O'Brien"、"a\b" → Err 含转义提示；"a=b"（含=）与 "a b"（含空格）通过
#[test] fn parse_param_arg_splits_on_first_eq()              // "id=a=b" → ("id","a=b")；"noid"、"id="、"=v" → Err
#[test] fn match_params_reports_missing_names()              // names=["a","b"] 只给 a → Err 含 "b"
#[test] fn match_params_reports_extra_and_duplicate()        // 多余 k → Err；重复 k → Err
#[test] fn match_params_reorders_to_template_order()         // names=["a","b"] 给 [b,a] → 返回 [a,b]（params_json 的 k 序）
#[test] fn prepare_execution_end_to_end()                    // 模板+["id=5"] → (含 5 的 SQL, {"id":"5"})
```

- [ ] **Step 2: 跑测试确认失败**：`cargo test saved_query::`
- [ ] **Step 3: 实现**。`substitute` 的单遍扫描是本任务唯一非自明算法：

```
顺序扫描字符：遇 "{{" → 找最近的 "}}"，名段按 Task 1 同口径校验，
查 params 表得值原样拼接（不回扫）；查无此名 → Err。
扫描结束后断言产物不含 "{{"，含 → Err（值注入防线）。
```

- [ ] **Step 4: 跑测试确认通过**：`cargo test saved_query::`
- [ ] **Step 5: Commit** `git commit -am "feat: 保存查询单遍替换引擎与参数校验"`

### Task 3: `saved_query.rs`——queries.d 文件层

**Files:**
- Modify: `src/saved_query.rs`

**Interfaces:**
- Consumes: `crate::config::{default_config_base, write_file_atomic}`（均 pub）；Task 1 校验
- Produces:
  - `pub fn queries_dir() -> Result<PathBuf, String>` —— `default_config_base()?.join("queries.d")`
  - `pub fn load_template_in(base: &Path, name: &str) -> Result<String, String>`；`pub fn load_template(name: &str) -> Result<String, String>`（不存在 → Err 含 `roq q list` 指引）
  - `pub fn list_queries_in(base: &Path) -> Result<Vec<(String, String)>, String>` —— 读 `*.sql`，返回 `(文件stem, 摘要)` 按名字典序；目录不存在 → `Ok(vec![])`
  - `pub fn write_template_in(base: &Path, name: &str, template: &str) -> Result<PathBuf, String>` —— `create_dir_all` + `write_file_atomic`，内容 = `QUERY_FILE_HEADER` + 模板，返回路径
  - `pub const QUERY_FILE_HEADER: &str` —— `"-- roq 保存查询（可手工编辑，修改即时生效；{{名}} 为占位符）\n"`
  - `pub fn template_summary(content: &str) -> String` —— 首个 trim 后非空且不以 `--`/`#` 开头的行，截 48 字符补 `"…"`；无则 `"(空)"`

- [ ] **Step 1: 写失败测试**（临时目录注入，参照 `cmd_config::tests::temp_conf` 模式）

```rust
fn temp_qdir(tag: &str) -> PathBuf // std::env::temp_dir().join(format!("roq-q-{tag}-{}", std::process::id()))
#[test] fn write_then_load_roundtrips_with_header()   // 写后回读 == header + template
#[test] fn load_missing_query_errors_with_list_hint() // Err 含 "roq q list"
#[test] fn list_queries_sorted_and_summarized()       // 两个文件按名排序；摘要取首条非注释行；空目录 → Ok(vec![])
#[test] fn template_summary_skips_comments_and_truncates()
```

- [ ] **Step 2: 跑测试确认失败**
- [ ] **Step 3: 实现**（薄封装，`load_template`/`write_template_in` 之外不做任何校验——校验属 Task 5 的 add 流程）
- [ ] **Step 4: 跑测试确认通过**
- [ ] **Step 5: Commit** `git commit -am "feat: 保存查询文件层（queries.d 读写列举）"`

### Task 4: `cli.rs`——QCmd 解析与用法文本

**Files:**
- Modify: `src/cli.rs`

**Interfaces:**
- Consumes: `parse_query_parts`（既有，私有，同文件内复用）
- Produces:
  - `pub enum QCmd { Add { name: String, template: String, update: bool }, List, Show { name: String }, Remove { name: String, yes: bool } }`
  - `Command` 新变体：`SavedQuery { name: String, params: Vec<String>, cli: Cli }`
  - `fn parse_q_cmd(args: &[String]) -> Result<Command, String>`，顶层 `parse_command` 增 `Some("q") => parse_q_cmd(&args[1..])`
  - `fn q_usage() -> String`

**解析规则（钉死）：**
- `q add <名> "模板" [--update]`：名 = 首个位置参数；模板 = 其余位置参数 `join(" ")`（同 explain 先例）；模板为空 → Err
- `q list`：无额外参数（多余位置参数/未知 flag → Err）
- `q show <名>` / `q remove <名> --yes`：`remove` 缺 `--yes` → Err（含 `--yes` 提示）
- 执行面（首词非管理动词/非 flag）：`parse_query_parts` 收集位置参数与查询 flag；位置参数空 → `Err(q_usage())`；`positional[0]` = 名，`positional[1..]` 原样存入 `params`（**解析层不校验 k=v 形态**，校验在执行面 `prepare_execution`，保证 param-error 可被审计）
- `q --help` / `q -h` → `Err(q_usage())`（main.rs 以 "roq" 前缀打印零退出惯例）

- [ ] **Step 1: 写失败测试**

```rust
#[test] fn parse_q_add_reads_name_template_and_update()   // ["add","n","SELECT {{id}}","--update"] → Add{update:true}
#[test] fn parse_q_add_requires_name_and_template()
#[test] fn parse_q_remove_requires_yes()                  // Err 含 "--yes"
#[test] fn parse_q_execution_collects_name_params_flags() // ["foo","id=1","--format","json"] → SavedQuery{ name:"foo", params:["id=1"], cli.format==Json }
#[test] fn parse_q_management_verbs_dispatch()            // list/show/remove 三形态；show 缺名 Err
#[test] fn parse_q_execution_without_name_yields_usage()  // [] → Err 含 "roq q"
```

- [ ] **Step 2: 跑测试确认失败**
- [ ] **Step 3: 实现** `QCmd`、`parse_q_cmd`、`q_usage()`；`usage()` 主文本增两行（执行与管理各一，56 列对齐）：

```
roq q <名> [k=v ...] [--profile 名 ...]    # 执行保存查询（详情 roq q --help）
roq q <add|list|show|remove>               # 保存查询管理（详情 roq q --help）
```

- [ ] **Step 4: 跑测试确认通过**：`cargo test cli::`（确认既有测试无回归）
- [ ] **Step 5: Commit** `git commit -am "feat: roq q 子命令解析与用法文本"`

### Task 5: `cmd_q.rs`——add/list/show/remove 管理子命令

**Files:**
- Create: `src/cmd_q.rs`；Modify: `src/lib.rs`（`pub mod cmd_q;`）

**Interfaces:**
- Consumes: Task 1-3 全部 Produces；`crate::gate::gate`；`crate::cli::QCmd`
- Produces: `pub fn run(cmd: QCmd) -> ExitCode`（Err → `eprintln!("[roq q] {}", msg)` + ExitCode 1，镜像 `cmd_config::run`）；可测内核：

  - `pub fn add_in(base: &Path, name: &str, template: &str, update: bool) -> Result<String, String>`
  - `pub fn remove_in(base: &Path, name: &str, yes: bool) -> Result<String, String>`
  - `pub fn list_in(base: &Path) -> Result<String, String>` / `pub fn show_in(base: &Path, name: &str) -> Result<String, String>`

**add 流程（顺序钉死，全部 fail-closed）：**
`validate_query_name` → 既有文件存在且非 `--update` → Err（含 `--update` 指引）；`--update` 且不存在 → Err → 模板 trim 后非空 → `extract_placeholder_names` → `dummy_substitute` → `gate::gate(&dummy)`（禁词/语法早暴露）→ `write_template_in` → 回读：`load_template_in` 内容一致 + 预检复跑通过 → Ok 消息含路径与占位符清单（`占位符 1 个：id` / `无占位符`）。

- [ ] **Step 1: 写失败测试**

```rust
#[test] fn add_writes_and_readbacks()                 // 含占位符模板 add 成功，消息含 "回读校验" 与占位符名
#[test] fn add_refuses_duplicate_then_update_works()  // 同名拒绝；--update 覆盖；--update 不存在 → Err 含 "不存在"
#[test] fn add_rejects_template_with_static_forbidden_word() // "SELECT * FROM t WHERE note='UPDATE'" → Err（哑参预检拦截）
#[test] fn add_rejects_malformed_placeholder()        // "{{a-" → Err
#[test] fn remove_needs_yes()                         // 无 --yes → Err 含 "--yes"；--yes 后文件消失
#[test] fn list_and_show_outputs()                    // list 空目录提示；show 原样输出（含头注释）
```

- [ ] **Step 2: 跑测试确认失败**
- [ ] **Step 3: 实现** `run` 分发 + 四个 `*_in`
- [ ] **Step 4: 跑测试确认通过**：`cargo test cmd_q::`
- [ ] **Step 5: Commit** `git commit -am "feat: roq q 管理子命令（add/list/show/remove）"`

### Task 6: 执行链路接线——main.rs / query.rs / audit.rs / log 词表

**Files:**
- Modify: `src/main.rs`、`src/query.rs:88-94`（`execute` 签名）、`src/query.rs:193/256`（`save_result` 调用点）、`src/audit.rs:90`（`save_result` 签名）、`src/cli.rs`（`log_usage` outcome 词表）

**Interfaces:**
- Consumes: Task 1-5 全部；既有 `audit_line`（`extra` 自由片段）
- Produces:
  - `saved_query.rs` 增 `pub fn saved_audit_extra(name: &str, params_json: &str, reason: Option<&str>) -> String` → `,"saved":"名","params":{...}` + 可选尾部 `,"reason":"..."`（名/值经 `json_escape`）
  - `audit.rs`：`save_result` 增参 `saved: Option<&SavedContext>`；slug 提炼纯函数 `pub fn result_slug(sql: &str, saved: Option<&str>) -> String`（saved 名 → `table_slug` → `like_slug` → `"query"`）；文件头增两行：`# 保存查询: <名>`、`# 参数: k=v, k=v`（值经 `json_escape`，无参数省略参数行）
  - `query.rs`：`execute(cli, profile, sql, saved: Option<&SavedContext>)`；`log_exit` 的 extra = saved 片段（若有）+ reason（若有，reason 保持最后）；stderr 摘要尾行追加 `（模板 <名>）`（仅 Some 且非 quiet 场景）——summary 行与两处 `save_result` 调用点透传
  - `main.rs` 增 `fn run_saved_query(name: &str, param_args: &[String], cli: Cli) -> ExitCode`，`Command::SavedQuery` 分发至此。**顺序镜像 run_query（gate 先于 profile 查找），每条失败路径审计如下：**

| 失败点 | outcome | sql 字段 | extra |
| --- | --- | --- | --- |
| 配置加载失败 | （同 run_query 现状：不审计，exit 1） | — | — |
| 查询名不存在 | `param-error` | `""` | saved + reason |
| prepare_execution 失败（缺/多/重参、值非法、模板坏） | `param-error` | 模板原文 | saved + params + reason |
| gate 拒绝（替换后） | `gate-rejected` | 替换后 SQL | saved + params + reason |
| profile 不存在 | `config-error` | 规范化 SQL | saved + params |

  成功路径：`query::execute(&cli, profile, &gated, Some(&ctx))`

- [ ] **Step 1: 写失败测试**（纯函数部分）

```rust
// saved_query.rs
#[test] fn saved_audit_extra_shapes_json() // 无 reason/无 params/含 reason 三形态；引号值转义
// audit.rs
#[test] fn result_slug_prefers_saved_name()             // Some("q-by-id") → "q-by-id"
#[test] fn result_slug_falls_back_to_table_then_query() // None 时走既有链（table_slug→"query"）
```

- [ ] **Step 2: 跑测试确认失败**
- [ ] **Step 3: 实现**四处接线。`cmd_log` 无代码改动（outcome 为自由过滤），仅 `cli.rs` 的 `log_usage` 词表行补 `param-error`
- [ ] **Step 4: 跑测试确认通过**：`cargo test`（全量，确认 execute 签名变更无回归）
- [ ] **Step 5: Commit** `git commit -am "feat: 保存查询执行链路（param-error 审计、saved 存档标注）"`

### Task 7: 文档同步与全量回归

**Files:**
- Modify: `skill/roq/SKILL.md`、`README.md`、`README.zh-CN.md`、`CHANGELOG.md`（**不动 `Cargo.toml` 版本号**，随 v0.11.0 一并发布）

**内容钉死：**
- SKILL.md「基本用法」增 `roq q` 一行 + 独立小节（机制：保存/调用/占位符/参数规则/值约束/**保存 ≠ 永久放行**/审计带 saved；只写机制与通用示例名，不写本机查询名——红线 6）
- README 双语各增一节（英文版措辞与中文版语义一一对应）
- CHANGELOG `[Unreleased]` Added 条目（英文，含 `{{名}}` 占位符、k=v 调用、param-error 审计类别、存档标注）

- [ ] **Step 1: 改四份文档**（SKILL.md 为 `include_str!` 嵌入，改后二进制内容随之更新）
- [ ] **Step 2: `cargo test` 全绿 + `cargo build` 无 warning**
- [ ] **Step 3: 人工冒烟**（连接可用 profile）：`q add` → `q list` → `q show` → 带参执行 → 缺参报错 → 审计 `roq log --last 3` 见 `saved` 字段 → 存档文件头含模板名
- [ ] **Step 4: Commit** `git commit -am "docs: roq q 保存查询文档（SKILL/README 双语/CHANGELOG）"`
