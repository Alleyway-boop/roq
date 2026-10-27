# DDL 免截断实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `SHOW CREATE TABLE` 终端 tsv 输出免单格截断，其余场景单格被截时 stderr 明示指路存档。

**Architecture:** 识别与豁免组合提炼为 render.rs 纯函数（与 `truncate_cell` 同地），cli.rs 记录 `--max-cell` 显式传参，query.rs 显示层接线并加截断提示；闸门/审计/存档链路零改动，与 `--out` 免截断先例同构。

**Tech Stack:** Rust（std only），cargo test。

**Spec:** `docs/superpowers/specs/2026-10-27-ddl-truncation-design.md`

## Global Constraints

- 用户可见文本（报错、提示、注释、文档）一律中文；代码标识符英文（项目 CLAUDE.md）
- 红线：不改 `gate.rs`、不动闸门/审计/存档链路；快捷命令仍只生成 SQL 走同一链路
- `SKILL.md` 由 `include_str!` 嵌入二进制，不得写入本机路径或内网库拓扑
- 每个任务提交前 `cargo test` 全绿（项目 CLAUDE.md：改任何代码前先跑，提交前必须全绿）
- 提交信息 conventional commits（`feat:` / `docs:`），中文描述

## Review Focus

1. 大小写混写 `show create table t`（agent 手写常态）——期望免截断；Task 1 测试钉住
2. 显式 `--max-cell 50` 与 SHOW CREATE TABLE 并存——期望尊重 50 不豁免；Task 1/2 测试钉住
3. `--quiet` 时截断提示必须静默——走既有 notice 闭包（与 prod 提醒同路径），无独立测试，审查时人工核对 Task 4 接线
4. 存档失败（`result_file` 为空）时提示不得打印空路径——文案回退为"失败"，Task 4 代码块钉死
5. json/csv / `--out` 模式查 SHOW CREATE TABLE——不截断也不打提示；由分支结构与 `terminal_max_cell` 测试保证

---

### Task 1: render.rs 截断策略纯函数

**Files:**
- Modify: `src/render.rs`（`table_slug` 之后、tests 之前；文件头 //! 职责注释同步）
- Test: `src/render.rs` tests 模块

**Interfaces:**
- Consumes: `crate::gate::extract_tokens`（已有 `use`，render.rs:4）
- Produces:
  - `pub fn is_show_create(sql: &str) -> bool`
  - `pub fn terminal_max_cell(is_out: bool, is_show_create_stmt: bool, max_cell_explicit: bool, max_cell: usize) -> usize`

- [ ] **Step 1: 写失败测试**（tests 模块内追加）

```rust
#[test]
fn is_show_create_matches_normalized_statements() {
    assert!(is_show_create("SHOW CREATE TABLE t_user"));
    // extract_tokens 归全大写，大小写混写免疫
    assert!(is_show_create("show create table t_user"));
    assert!(is_show_create("SHOW CREATE TABLE `mydb`.`t_user`"));
    assert!(!is_show_create("SELECT 1"));
    assert!(!is_show_create("SHOW TABLES"));
    assert!(!is_show_create("SHOW CREATE DATABASE x"));
    assert!(!is_show_create(""));
}

#[test]
fn terminal_max_cell_exemptions() {
    // --out 全量（既有先例）
    assert_eq!(terminal_max_cell(true, false, false, 200), usize::MAX);
    // SHOW CREATE TABLE 未显式限流 → 全量
    assert_eq!(terminal_max_cell(false, true, false, 200), usize::MAX);
    // 显式 --max-cell 优先于豁免
    assert_eq!(terminal_max_cell(false, true, true, 200), 200);
    assert_eq!(terminal_max_cell(false, true, true, 500), 500);
    // 普通语句按 --max-cell
    assert_eq!(terminal_max_cell(false, false, false, 200), 200);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test is_show_create_matches_normalized_statements`
Expected: FAIL（函数未定义，编译错）

- [ ] **Step 3: 实现两个函数**

```rust
/// 语句是否为 SHOW CREATE TABLE（输入应为闸门规范化后的 SQL）。
/// 词元由 extract_tokens 归全大写，大小写混写天然免疫；该语句的单格即交付物，终端不截断。
pub fn is_show_create(sql: &str) -> bool {
    let tokens = extract_tokens(sql);
    tokens.first().map(String::as_str) == Some("SHOW")
        && tokens.get(1).map(String::as_str) == Some("CREATE")
        && tokens.get(2).map(String::as_str) == Some("TABLE")
}

/// 终端 tsv 单格截断上限：--out 全量；SHOW CREATE TABLE（未显式限流时）全量；其余按 --max-cell。
pub fn terminal_max_cell(
    is_out: bool,
    is_show_create_stmt: bool,
    max_cell_explicit: bool,
    max_cell: usize,
) -> usize {
    if is_out || (is_show_create_stmt && !max_cell_explicit) {
        usize::MAX
    } else {
        max_cell
    }
}
```

同时把文件头 `//!` 注释补上截断策略职责。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test terminal_max_cell_exemptions && cargo test is_show_create`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add src/render.rs
git commit -m "feat: 截断策略纯函数——SHOW CREATE TABLE 识别与豁免组合"
```

---

### Task 2: cli.rs 记录 --max-cell 显式传参

**Files:**
- Modify: `src/cli.rs`（`Cli` 结构体 cli.rs:14-27；`parse_query_parts` 初始化 cli.rs:483-493；`--max-cell` 分支 cli.rs:524-530）
- Test: `src/cli.rs` tests 模块

**Interfaces:**
- Consumes: 无
- Produces: `Cli.max_cell_explicit: bool`（Task 3 消费；显式传 `--max-cell` 即 `true`）

- [ ] **Step 1: 写失败测试**（tests 模块内追加，沿用现有 `parse_command` 用法）

```rust
// --max-cell 显式传参置 explicit 标志（SHOW CREATE TABLE 的截断豁免让位于显式限流）
#[test]
fn max_cell_explicit_flag_tracks_cli_input() {
    let cli = match parse_command(&[
        "--max-cell".to_string(),
        "50".to_string(),
        "SELECT 1".to_string(),
    ])
    .unwrap()
    {
        Command::Query(cli) => cli,
        _ => panic!("应产出 Query 命令"),
    };
    assert!(cli.max_cell_explicit);
    assert_eq!(cli.max_cell, 50);
    let cli = match parse_command(&["SELECT 1".to_string()]).unwrap() {
        Command::Query(cli) => cli,
        _ => panic!("应产出 Query 命令"),
    };
    assert!(!cli.max_cell_explicit);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test max_cell_explicit_flag_tracks_cli_input`
Expected: FAIL（字段不存在，编译错）

- [ ] **Step 3: 实现**

`Cli` 加字段（`max_cell` 之后）：

```rust
/// 用户是否显式传了 --max-cell（true 时 SHOW CREATE TABLE 的截断豁免不生效）。
pub max_cell_explicit: bool,
```

`parse_query_parts` 初始化块加 `max_cell_explicit: false,`；`--max-cell` 分支在解析成功后加 `cli.max_cell_explicit = true;`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test max_cell_explicit_flag_tracks_cli_input`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add src/cli.rs
git commit -m "feat: --max-cell 显式传参标志"
```

---

### Task 3: query.rs 豁免接线

**Files:**
- Modify: `src/query.rs:15-17`（use 补 `is_show_create, terminal_max_cell`）、`src/query.rs:165-171`（display_max_cell 计算）

**Interfaces:**
- Consumes: `render::is_show_create(sql: &str) -> bool`、`render::terminal_max_cell(is_out: bool, is_show_create_stmt: bool, max_cell_explicit: bool, max_cell: usize) -> usize`（Task 1）、`Cli.max_cell_explicit: bool`（Task 2）
- Produces: 无新接口（显示行为变更）

- [ ] **Step 1: 接线**——`display_max_cell` 替换为：

```rust
let display_max_cell = terminal_max_cell(
    cli.out.is_some(),
    is_show_create(sql),
    cli.max_cell_explicit,
    cli.max_cell,
);
```

`use crate::render::{...}` 补入 `is_show_create, terminal_max_cell`。第 166 行注释更新为说明三种免截断来源（--out / SHOW CREATE TABLE / 显式 max_cell 优先级）。

- [ ] **Step 2: 全量测试 + 确认无警告**

Run: `cargo test`
Expected: 全绿（行为变更无既有测试覆盖，靠 Task 1 纯函数测试与 Review Focus 人工核对）

- [ ] **Step 3: 提交**

```bash
git add src/query.rs
git commit -m "feat: SHOW CREATE TABLE 终端输出免单格截断"
```

---

### Task 4: query.rs 截断明示

**Files:**
- Modify: `src/query.rs`（TSV 分支 query.rs:211-220 加截断检测；循环外 query.rs:242-247 后加提示行）

**Interfaces:**
- Consumes: 既有 `notice` 闭包（`--quiet` 抑制）、`result_file`（存档路径）
- Produces: stderr 提示行 `[roq] tsv 单元格超 --max-cell=N 已截断；全量见存档 <path>（或改用 --format json）`

- [ ] **Step 1: 接线**

`rows_out` 声明旁加 `let mut cell_truncated = false;`；TSV 分支改为检测式截断：

```rust
OutputFormat::Tsv => {
    let display: String = cells
        .into_iter()
        .map(|cell| {
            let escaped = tsv_cell(cell);
            let shown = truncate_cell(escaped.clone(), display_max_cell);
            if shown != escaped {
                cell_truncated = true;
            }
            shown
        })
        .collect::<Vec<_>>()
        .join("\t");
    let _ = writeln!(out, "{}", display);
}
```

`result_file` 确定之后、摘要 `notice` 之前（提示在前更醒目，摘要收尾）：

```rust
if cell_truncated {
    notice(format!(
        "[roq] tsv 单元格超 --max-cell={} 已截断；全量见存档 {}（或改用 --format json）",
        cli.max_cell,
        if result_file.is_empty() { "失败" } else { &result_file }
    ));
}
```

注意：`--out` 时 `display_max_cell` 为 `usize::MAX`，`cell_truncated` 恒 `false`，提示天然不出现；json/csv 不进 TSV 分支；`--quiet` 由 notice 闭包统一抑制。

- [ ] **Step 2: 全量测试**

Run: `cargo test`
Expected: 全绿

- [ ] **Step 3: 手工 smoke（有可用 profile 时）**

Run: `roq schema <某表>`——DDL 整格上屏无省略号，stderr 摘要正常；`roq --format tsv "SELECT 长文本列..."`——单格 200 处截断且 stderr 出现提示行。
Expected: 符合上述两条。无 profile 可用时跳过并在收尾说明。

- [ ] **Step 4: 提交**

```bash
git add src/query.rs
git commit -m "feat: tsv 单格截断 stderr 明示并指路存档"
```

---

### Task 5: SKILL.md 同步 + 收尾

**Files:**
- Modify: `skill/roq/SKILL.md:36`（限额行）、`skill/roq/SKILL.md:39`（stderr 摘要行）

**Interfaces:**
- Consumes: Task 3/4 的实际行为
- Produces: 无

- [ ] **Step 1: 改两行文案**（exact copy）

第 36 行改为：

```
- 默认限额 500 行（`--max-rows`）；tsv 终端单格 200 字符（`--max-cell`；json/csv 与存档不截断；`SHOW CREATE TABLE` 免截断，显式传 `--max-cell` 除外）
```

第 39 行改为：

```
stderr 尾行有审计摘要（profile、库、行数、耗时、"会话只读"、结果存档路径；`--quiet` 会抑制，人看场景保留它向用户报告）。tsv 单格被截时另有提示行指明存档路径（`--quiet` 同样抑制）。
```

- [ ] **Step 2: 全量测试**

Run: `cargo test`
Expected: 全绿（SKILL.md 为 include_str! 嵌入，确认无编译影响）

- [ ] **Step 3: 提交**

```bash
git add skill/roq/SKILL.md
git commit -m "docs: SKILL.md 同步 DDL 免截断与截断提示"
```

- [ ] **Step 4: 收尾说明**

提醒用户：本机生效需 `cargo install --path .`（含 SKILL.md 重装到 `~/.claude/skills/roq/`，需 `roq skill install --force` 或按工具提示）；仓库不放二进制副本。
