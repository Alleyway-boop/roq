# 快捷子命令模糊过滤（tables/columns 可选 pattern）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `roq tables` / `roq columns` 支持可选过滤模式，模糊找表/列不再全量输出；顺带在 SKILL.md 补上"子命令必须紧跟 roq"的顺序规则反例。

**Architecture:** 全部改动集中在 `cli.rs` 快捷子命令层：新增 pattern→SQL 字面量辅助函数，`shortcut_tables`/`shortcut_columns` 接受可选位置参数后拼接 `SHOW ... LIKE`。生成 SQL 照走既有闸门+审计+存档链路，闸门零改动。

**Tech Stack:** Rust（单二进制 CLI），测试为 cli.rs 内嵌 `#[cfg(test)] mod tests`。

**Spec:** `docs/superpowers/specs/2026-11-03-shortcut-pattern-design.md`

## Global Constraints

- CLI 可见文本、注释一律中文；代码标识符英文
- 生成 SQL 必须走同一条闸门+审计+存档链路（红线 5），不得绕过；闸门零改动
- 转义只处理单引号（双写 `''`，SQL 标准写法，与 SQL mode 无关）
- 提交前 `cargo test` 全绿（基线 93 passed）
- 文档改动必须 README.md 与 README.zh-CN.md 双语同步
- 执行前用 superpowers:using-git-worktrees 创建隔离工作区（全局 CLAUDE.md 开发流程约定）

## Review Focus

最可能咬人的输入，各由 owning task 的测试钉死：

1. pattern 含单引号（`roq tables "a'b"`）→ 期望转义为 `'%a''b%'`，闸门放行、语义不变 → Task 1 测试
2. snake_case 模式（`roq tables t_user`，含 `_` 无 `%`）→ 期望按子串搜索包 `%...%`，而非退化成近似精确匹配 → Task 1 测试
3. 多余位置参数（`roq columns a b c`）→ 期望用法错误而非静默丢弃 → Task 1 测试
4. flag 与 pattern 混排（`roq tables mark --format json`）→ 期望 flag 归 flag、位置参数归 pattern → Task 1 测试
5. `SHOW ... LIKE` 结果的审计存档标注 → 期望正常写档不崩 → Task 4 真实运行冒烟验证

---

### Task 1: pattern 字面量辅助函数 + tables/columns 解析（TDD）

**Files:**
- Modify: `src/cli.rs:167-197`（shortcut_tables / shortcut_columns）
- Modify: `src/cli.rs:209-225` 附近（新增辅助函数，紧挨 quoted_table_name）
- Test: `src/cli.rs` `#[cfg(test)] mod tests`（743 行附近的 shortcut 测试区）

**Interfaces:**
- Consumes: `parse_query_parts(&[String]) -> Result<(Cli, Vec<String>), String>`（现有）、`quoted_table_name(&str) -> Result<String, String>`（现有）
- Produces: `fn like_pattern_literal(pattern: &str) -> String` —— 含 `%` 原样、否则自动包 `%...%`、单引号双写转义；Task 2/4 依赖其行为

- [ ] **Step 1: 写失败测试**（加入 tests 模块，风格对齐 759-793 行现有 shortcut 测试）

```rust
#[test]
fn tables_with_pattern_builds_show_tables_like() {
    match parse_command(&["tables".to_string(), "mark".to_string()]) {
        Ok(Command::Query(cli)) => assert_eq!(cli.sql.as_deref(), Some("SHOW TABLES LIKE '%mark%'")),
        _ => panic!("tables 带模式应产出 Query 命令"),
    }
}

#[test]
fn tables_with_percent_keeps_pattern_verbatim() {
    match parse_command(&["tables".to_string(), "t_report%".to_string()]) {
        Ok(Command::Query(cli)) => assert_eq!(cli.sql.as_deref(), Some("SHOW TABLES LIKE 't_report%'")),
        _ => panic!(),
    }
}

#[test]
fn tables_with_snake_case_searches_substring() {
    match parse_command(&["tables".to_string(), "t_user".to_string()]) {
        Ok(Command::Query(cli)) => assert_eq!(cli.sql.as_deref(), Some("SHOW TABLES LIKE '%t_user%'")),
        _ => panic!(),
    }
}

#[test]
fn tables_pattern_escapes_single_quote_and_passes_gate() {
    match parse_command(&["tables".to_string(), "a'b".to_string()]) {
        Ok(Command::Query(cli)) => {
            let sql = cli.sql.expect("应有 SQL");
            assert_eq!(sql, "SHOW TABLES LIKE '%a''b%'");
            assert!(crate::gate::gate(&sql).is_ok(), "生成 SQL 应能过闸门：{sql}");
        }
        _ => panic!(),
    }
}

#[test]
fn tables_rejects_extra_positional_args() {
    assert!(parse_command(&["tables".to_string(), "a".to_string(), "b".to_string()]).is_err());
}

#[test]
fn columns_with_pattern_appends_like() {
    match parse_command(&["columns".to_string(), "t_user".to_string(), "task_num".to_string()]) {
        Ok(Command::Query(cli)) => assert_eq!(
            cli.sql.as_deref(),
            Some("SHOW FULL COLUMNS FROM `t_user` LIKE '%task_num%'")
        ),
        _ => panic!(),
    }
}

#[test]
fn columns_without_pattern_unchanged() {
    match parse_command(&["columns".to_string(), "t_user".to_string()]) {
        Ok(Command::Query(cli)) => {
            assert_eq!(cli.sql.as_deref(), Some("SHOW FULL COLUMNS FROM `t_user`"))
        }
        _ => panic!(),
    }
}

#[test]
fn columns_rejects_zero_and_three_positional_args() {
    assert!(parse_command(&["columns".to_string()]).is_err());
    assert!(parse_command(&["columns".to_string(), "a".to_string(), "b".to_string(), "c".to_string()]).is_err());
}

#[test]
fn tables_pattern_coexists_with_flags() {
    match parse_command(&["tables".to_string(), "mark".to_string(), "--format".to_string(), "json".to_string()]) {
        Ok(Command::Query(cli)) => {
            assert_eq!(cli.sql.as_deref(), Some("SHOW TABLES LIKE '%mark%'"));
            assert!(matches!(cli.format, OutputFormat::Json));
        }
        _ => panic!(),
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test tables_with_pattern --lib 2>&1 | tail -5`
Expected: 编译失败或断言失败（pattern 尚未支持）

- [ ] **Step 3: 实现辅助函数与解析改动**

`like_pattern_literal`（放在 `quoted_table_name` 旁，注释中文）：

```rust
/// LIKE 模式进单引号字面量：显式含 % 时按 LIKE 通配符原样使用（t_report% 做前缀匹配），
/// 其余（含 snake_case 的 `_`）视为子串自动包 %...%；单引号双写转义（SQL 标准写法，与 SQL mode 无关）。
fn like_pattern_literal(pattern: &str) -> String {
    let escaped = pattern.replace('\'', "''");
    if pattern.contains('%') {
        format!("'{}'", escaped)
    } else {
        format!("'%{}%'", escaped)
    }
}
```

`shortcut_tables` 改为：

```rust
fn shortcut_tables(args: &[String]) -> Result<Command, String> {
    let (mut cli, positional) = parse_query_parts(args)?;
    cli.sql = Some(match positional.as_slice() {
        [] => "SHOW TABLES".to_string(),
        [pattern] => format!("SHOW TABLES LIKE {}", like_pattern_literal(pattern)),
        extra => {
            return Err(format!(
                "tables 至多接受一个位置参数（过滤模式）：{}",
                extra.join(" ")
            ))
        }
    });
    Ok(Command::Query(cli))
}
```

`shortcut_columns` 改为（match 两个分支共用 `quoted_table_name` 校验）：

```rust
fn shortcut_columns(args: &[String]) -> Result<Command, String> {
    let (mut cli, positional) = parse_query_parts(args)?;
    match positional.as_slice() {
        [table] => {
            cli.sql = Some(format!("SHOW FULL COLUMNS FROM {}", quoted_table_name(table)?));
        }
        [table, pattern] => {
            cli.sql = Some(format!(
                "SHOW FULL COLUMNS FROM {} LIKE {}",
                quoted_table_name(table)?,
                like_pattern_literal(pattern)
            ));
        }
        extra => {
            return Err(format!(
                "columns 需要 1-2 个位置参数（表名 [列模式]，可用 库.表 两段）：{}",
                extra.join(" ")
            ))
        }
    }
    Ok(Command::Query(cli))
}
```

- [ ] **Step 4: 跑全量测试确认通过（含旧测试无回归）**

Run: `cargo test 2>&1 | tail -4`
Expected: 全部 PASS（基线 93 + 新增 9）

- [ ] **Step 5: 提交**

```bash
git add src/cli.rs
git commit -m "feat: tables/columns 快捷子命令支持可选过滤模式（子串自动包 %，显式 % 原样）"
```

### Task 2: usage() 文本更新

**Files:**
- Modify: `src/cli.rs:126-129`（usage 快捷子命令区）

**Interfaces:**
- Consumes: Task 1 的 pattern 语义（文本描述用）
- Produces: `--help` 输出的最新用法说明

- [ ] **Step 1: 更新 usage 数组**（保持竖向对齐；tables 行加 `[模式]`，columns 行加 `[列模式]`，各补一句注释说明"子串自动包 %，显式 % 原样"）

- [ ] **Step 2: 验证输出**

Run: `cargo run -- --help 2>&1 | Select-String "tables|columns"`
Expected: 出现 `roq tables [模式]` 与 `roq columns <表|库.表> [列模式]`

- [ ] **Step 3: 提交**

```bash
git add src/cli.rs
git commit -m "docs: usage 更新 tables/columns 过滤模式用法"
```

### Task 3: 文档同步（SKILL.md + README 双语 + CHANGELOG）

**Files:**
- Modify: `skill/roq/SKILL.md`（:25 模板下顺序规则反例、:27 速览、:31 排查起点、:88-91 示例区）
- Modify: `README.md` 与 `README.zh-CN.md`（快捷子命令用法处，双语同步）
- Modify: `CHANGELOG.md`（[Unreleased]）
- Modify: `docs/superpowers/specs/2026-11-03-shortcut-pattern-design.md`（pattern 语义修订：仅 `%` 原样）

**Interfaces:**
- Consumes: Task 1 最终行为（文档描述以测试断言为准）
- Produces: 对外文档与实现一致

- [ ] **Step 1: SKILL.md 四处改动**

`:25` 模板正下方插入（反例与踩坑形态一字不差）：

```markdown
⚠ 顺序规则（勿混用）：tables/schema/columns/explain 子命令必须紧跟 roq，flag 一律放子命令之后
  ✓ roq columns t_xxx --profile dev
  ✗ roq --profile dev columns t_xxx（会被当作 SQL 送去闸门拒绝）
```

`:27` 速览行与 `:31` 排查起点补模糊过滤：`roq tables [模式]`（如 `roq tables mark` 即子串搜索；`roq tables t_report%` 前缀匹配）、`roq columns <表> [列模式]`。
`:88-91` 示例区加一行 `roq tables mark --profile dev`。
红线自查：SKILL.md 不出现本机路径或内网库拓扑。

- [ ] **Step 2: README 双语同步 + CHANGELOG**

在两份 README 的快捷子命令/用法处补 pattern 说明（英文版同步翻译）；CHANGELOG `[Unreleased]` 加一条（参考既有条目格式）。

- [ ] **Step 3: 修订设计文档 pattern 语义**

`2026-11-03-shortcut-pattern-design.md` 中"含 `%` 或 `_` 原样"改为"仅含 `%` 原样，其余（含 snake_case 的 `_`）自动包 `%...%`"，注明修订原因（snake_case 命名与 `_` 通配语义冲突）。

- [ ] **Step 4: 提交**

```bash
git add skill/roq/SKILL.md README.md README.zh-CN.md CHANGELOG.md docs/superpowers/specs/2026-11-03-shortcut-pattern-design.md
git commit -m "docs: 快捷子命令过滤模式文档（SKILL/README 双语/CHANGELOG）+ SKILL 顺序规则反例"
```

### Task 4: 端到端验证

**Files:** 无新改动（验证任务；发现问题则回到对应 Task 修复）

**Interfaces:**
- Consumes: Task 1-3 全部产出

- [ ] **Step 1: 全量测试**

Run: `cargo test 2>&1 | tail -4`
Expected: 全部 PASS

- [ ] **Step 2: 真实运行冒烟（--profile dev 实库）**

```powershell
cargo run -- tables mark --profile dev          # 期望只出 mark 相关表
cargo run -- tables t_report% --profile dev     # 期望前缀匹配
cargo run -- columns t_report_teaching_mark_stat task_num --profile dev  # 期望列过滤
```
Expected: 三条均走闸门+审计正常返回；stderr 尾行有审计摘要

- [ ] **Step 3: 确认存档标注**

Run: `Get-ChildItem .roq/results/ -Recurse -File | Sort-Object LastWriteTime | Select-Object -Last 3 -ExpandProperty Name`
Expected: 最新三条对应上述查询，文件名含表名/模式标注、无 panic、无空档

- [ ] **Step 4: SKILL 顺序规则负例冒烟**

Run: `cargo run -- --profile dev columns t_report_teaching_mark_stat 2>&1 | head -2`
Expected: 仍被闸门拒绝（顺序规则是文档层约定，解析行为不变属预期）

- [ ] **Step 5: 汇报**

向用户汇报验证结果；本机 `~/.cargo/bin` 的 roq 更新走既有约定（推送后 `cargo install --git`），发版另行决定。
