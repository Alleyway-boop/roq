# Oracle 支持（双引擎）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** roq 升级为双引擎只读查询 CLI：`engine=oracle` 的 profile 获得与 MySQL 完全对等的体验（任意只读查询 + tables/columns/schema 快捷子命令），三层只读保障与审计存档链路等价成立。

**Architecture:** 新增引擎感知的三处分流：配置（`Profile.engine`）、闸门（`gate(sql, engine)` 方言感知扫描）、SQL 生成（快捷子命令意图延迟到 profile 加载后按引擎物化）。新增 `conn.rs` 适配层隔离驱动差异，对上暴露统一的"连接 → 会话只读 → 流式取行"形状；`oracledb`（官方纯 Rust thin 驱动）为 Oracle 侧实现，MySQL 侧现状逻辑原样迁入。

**Tech Stack:** Rust 2021、`mysql 26`（现状不动）、`oracledb 26.0.0-beta.6`（Oracle 官方 thin，同步 API，零本地依赖）、`chrono`/`serde_json`/`rpassword`（现状不动）。

**Spec:** `docs/superpowers/specs/2026-11-16-oracle-support-design.md`（本计划从 spec 论证；执行者两份都要读）。

## Global Constraints

- 三层只读保障不得削弱：闸门 fail-closed（拿不准一律拒绝）；第二层会话只读失败即中止；审计与存档不可关闭。
- 快捷子命令只生成 SQL，必须走同一条闸门+审计+存档链路（红线 5）。
- `gate.rs` 每处改动必须附带注入负例测试（红线 1）。
- 驱动 `oracledb = "26.0.0-beta.6"`（若 crates.io 版本号有出入，取 26.0.0-beta 系最新）；目标库 ≥ Oracle 12；Rust ≥ 1.89。
- 版本号：Cargo.toml bump 至 `1.0.0`（大版本，spec 已确认）。
- 二进制体积保持个位数 MB（现 ~2.5MB，release 完成后 `ls -lh` 实测记录进 CHANGELOG）。
- 语言约定：CLI 文本/代码注释中文；`README.md` + `README.zh-CN.md` 双语同步；CHANGELOG 英文；`skill/roq/SKILL.md` 不得写入本机路径或内网库拓扑。
- 提交前 `cargo test` 全绿；conventional commits（`feat:`/`fix:`/`test:`/`docs:`，中文描述）。
- 新功能开发在隔离 worktree 进行（执行时按 superpowers:using-git-worktrees 创建）。

## Review Focus

spec 蕴含但常规测试不覆盖、最容易咬到使用者的五类输入（每条已钉进所属任务的测试步骤）：

1. **Oracle 字符串字面量含 `#` / `--` / 分号**（`'OBJ#'`、`'a--b;c'`）——不得被当注释截断或多语句误判（Task 3 gate 负例）。
2. **Oracle `5--3` 是"5 加行注释"而非减法**（`--` 立即注释，与 MySQL 相反）——闸门不得按 MySQL 规则静默改写语义（Task 3 负例）。
3. **旧审计行无 `engine` 字段**——`roq log` 必须正常展示且按 mysql 解释（Task 7 测试）。
4. **38 位 NUMBER 精度**——tsv/json 输出必须逐字符保真，绝不能走 f64（Task 11 e2e 逐字符断言）。
5. **Oracle 上 EXPLAIN 不论入口都到不了数据库**——`roq explain` 子命令在物化层拒绝（Task 2 测试）；手写 `EXPLAIN ...` 被首词白名单拒绝（Task 3 测试）。

---

### Task 1: Engine 枚举与 profile 解析

**Files:**
- Modify: `src/config.rs`
- Test: `src/config.rs`（`mod tests` 内）

**Interfaces:**
- Produces: `pub enum Engine { Mysql, Oracle }`（`Debug, Clone, Copy, PartialEq, Eq`）；`Engine::parse(value: &str) -> Result<Engine, String>`；`Engine::as_str(self) -> &'static str`（"mysql"/"oracle"）；`Engine::default_port(self) -> u16`（3306/1521）；`Profile` 新增字段 `pub engine: Engine`。后续所有任务消费 `profile.engine`。

- [ ] **Step 1: 写失败测试**（`src/config.rs` tests）

```rust
#[test]
fn engine_parses_and_defaults_to_mysql() {
    assert_eq!(Engine::parse("oracle").unwrap(), Engine::Oracle);
    assert_eq!(Engine::parse("MYSQL").unwrap(), Engine::Mysql);
    assert_eq!(Engine::parse("x").unwrap_err(), "engine 需为 mysql 或 oracle：x");
    assert_eq!(Engine::Oracle.default_port(), 1521);
    assert_eq!(Engine::Mysql.default_port(), 3306);
}

#[test]
fn oracle_profile_takes_1521_default_and_rejects_ssl() {
    // 无 port 无 ssl：oracle 缺省 1521
    let path = write_temp("eng-o", "[o]\nhost=h\nuser=u\npassword=p\ndatabase=d\nengine=oracle\n");
    let profiles = load_profiles(&path).unwrap();
    assert_eq!(profiles["o"].port, 1521);
    assert_eq!(profiles["o"].engine, Engine::Oracle);
    // ssl=true 与 oracle 互斥
    let path2 = write_temp("eng-ssl", "[o]\nhost=h\nuser=u\npassword=p\ndatabase=d\nengine=oracle\nssl=true\n");
    let err = load_profiles(&path2).unwrap_err();
    assert!(err.contains("[o]") && err.contains("ssl"), "应指明节名与 ssl 冲突：{}", err);
    // mysql profile 缺省 engine=Mysql、port 3306（回归既有行为）
    let path3 = write_temp("eng-m", "[m]\nhost=h\nuser=u\npassword=p\ndatabase=d\n");
    let profiles = load_profiles(&path3).unwrap();
    assert_eq!(profiles["m"].engine, Engine::Mysql);
    assert_eq!(profiles["m"].port, 3306);
}
```

（现有测试文件里所有 `Profile { ... }` 字面量构造处同步补 `engine: Engine::Mysql`——只让编译过，不改断言。）

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib config`
Expected: FAIL（`Engine` 未定义）

- [ ] **Step 3: 实现**

`config.rs`：定义 `Engine`（三方法文案照测试原文）；`Profile` 加 `engine` 字段；`load_profiles` 里**先解析 engine**（缺省 `Engine::Mysql`），再解析 port（缺省改为 `engine.default_port()`）；`engine == Oracle && ssl` 时 `return Err(format!("配置 [{}] engine=oracle 暂不支持 ssl（TCPS 连接未支持）", name))`。构造 `Profile` 处补 engine。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test`
Expected: 全部 PASS（既有 mysql 行为回归无变化）

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat: profile 支持 engine 引擎声明（oracle 端口缺省 1521、ssl 显式拒绝）"
```

---

### Task 2: 快捷子命令意图化与按引擎物化 SQL

**Files:**
- Modify: `src/cli.rs`、`src/main.rs`
- Test: `src/cli.rs`（tests 内）

**Interfaces:**
- Consumes: `Engine`（Task 1）
- Produces: `#[derive(Debug, Clone, PartialEq)] pub enum Shortcut { Tables(Option<String>), Schema(String), Columns(String, Option<String>), Explain(String) }`；`Cli` 新增字段 `pub shortcut: Option<Shortcut>`（文档注释注明：`sql` 与 `shortcut` 恰有其一，由解析层保证）；`pub fn materialize_sql(shortcut: &Shortcut, engine: Engine) -> Result<String, String>`。`main.rs::run_query` 消费。

- [ ] **Step 1: 改造解析层存意图 + 写物化失败测试**

`shortcut_tables`/`shortcut_schema`/`shortcut_columns`/`shortcut_explain` 改为在 `cli.shortcut` 存 `Shortcut`（不再写 `cli.sql`）。既有 cli.rs 测试中所有 `cli.sql.as_deref() == Some(...)` 断言改为对应 `cli.shortcut` 断言（mysql 期望字符串移入 Step 3 的物化测试）。新增物化测试：

```rust
#[test]
fn materialize_mysql_matches_legacy_output() {
    // 与改造前生成的 SQL 逐字节一致（快捷子命令 mysql 行为零变化）
    let cases = [
        (Shortcut::Tables(None), "SHOW TABLES"),
        (Shortcut::Tables(Some("%mark%".into())), "SHOW TABLES LIKE '%mark%'"),
        (Shortcut::Schema("t_user".into()), "SHOW CREATE TABLE `t_user`"),
        (Shortcut::Schema("mydb.t_user".into()), "SHOW CREATE TABLE `mydb`.`t_user`"),
        (Shortcut::Columns("t".into(), None), "SHOW FULL COLUMNS FROM `t`"),
        (Shortcut::Explain("SELECT 1".into()), "EXPLAIN SELECT 1"),
    ];
    for (s, expected) in cases {
        assert_eq!(materialize_sql(&s, Engine::Mysql).unwrap(), expected);
    }
    // 非法表名仍在物化层拒绝
    assert!(materialize_sql(&Shortcut::Schema("bad name".into()), Engine::Mysql).is_err());
    assert!(materialize_sql(&Shortcut::Tables(Some("a\\b".into())), Engine::Mysql).is_err());
}

#[test]
fn materialize_oracle_generates_dictionary_sql_and_refuses_explain() {
    let cases = [
        (Shortcut::Tables(None), "SELECT table_name FROM user_tables ORDER BY table_name"),
        (Shortcut::Tables(Some("mark".into())),
         "SELECT table_name FROM user_tables WHERE UPPER(table_name) LIKE UPPER('%mark%') ORDER BY table_name"),
        (Shortcut::Schema("t_user".into()),
         "SELECT DBMS_METADATA.GET_DDL('TABLE', 'T_USER') FROM dual"),
        (Shortcut::Schema("other.t_user".into()),
         "SELECT DBMS_METADATA.GET_DDL('TABLE', 'T_USER', 'OTHER') FROM dual"),
        (Shortcut::Columns("t_user".into(), None),
         "SELECT c.column_name, c.data_type, c.data_length, c.data_precision, c.data_scale, c.nullable, c.data_default, cc.comments FROM user_tab_columns c LEFT JOIN user_col_comments cc ON cc.table_name = c.table_name AND cc.column_name = c.column_name WHERE c.table_name = 'T_USER' ORDER BY c.column_id"),
        (Shortcut::Columns("t_user".into(), Some("obj".into())),
         "SELECT c.column_name, c.data_type, c.data_length, c.data_precision, c.data_scale, c.nullable, c.data_default, cc.comments FROM user_tab_columns c LEFT JOIN user_col_comments cc ON cc.table_name = c.table_name AND cc.column_name = c.column_name WHERE c.table_name = 'T_USER' AND UPPER(c.column_name) LIKE UPPER('%obj%') ORDER BY c.column_id"),
    ];
    for (s, expected) in cases {
        assert_eq!(materialize_sql(&s, Engine::Oracle).unwrap(), expected);
    }
    // Oracle 拒绝 explain（EXPLAIN PLAN 真实写 PLAN_TABLE）
    let err = materialize_sql(&Shortcut::Explain("SELECT 1".into()), Engine::Oracle).unwrap_err();
    assert!(err.contains("PLAN_TABLE"), "拒绝文案应说明原因：{}", err);
    // oracle 表名校验沿用同一字符集（反引号不存在于 oracle）
    assert!(materialize_sql(&Shortcut::Schema("bad name".into()), Engine::Oracle).is_err());
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib cli`
Expected: FAIL（`Shortcut`/`materialize_sql` 未定义）

- [ ] **Step 3: 实现物化层与解析层改造**

`cli.rs`：
- 定义 `Shortcut`（上述形状）。
- `materialize_sql`：mysql 分支输出与既有 `shortcut_*` 生成的字符串**逐字节一致**（复用 `quoted_table_name`/`like_pattern_literal`）；oracle 分支按测试原文——Schema/Columns 的 oracle 标识符校验复用 `quoted_table_name` 的字符集校验逻辑（抽 `fn valid_identifier(seg: &str) -> bool` 共用），段名 `to_ascii_uppercase()` 后进单引号字面量（oracle 无反引号，不包反引号；`''` 转义不需要——校验过的标识符只含字母数字下划线 $）；Explain + Oracle 返回 `Err("Oracle 的 EXPLAIN PLAN 需写入 PLAN_TABLE（真实写入，违反只读保障），不支持 explain 子命令".to_string())`。
- 四个 `shortcut_*` 函数改存 `Shortcut`。

`main.rs::run_query`：在 profile 查到之后、gate 之前插入：

```rust
let sql = match &cli.sql {
    Some(sql) => sql.clone(),
    None => match cli.shortcut.as_ref() {
        Some(s) => match materialize_sql(s, profile.engine) {
            Ok(sql) => sql,
            Err(msg) => {
                eprintln!("[roq] {}", msg);
                append_audit_log(&audit_line(&cli.profile, "", "", 0, false, 0,
                    "config-error", &format!(",\"reason\":\"{}\"", json_escape(&msg)), ""));
                return ExitCode::from(EXIT_USAGE);
            }
        },
        None => { eprintln!("缺少 SQL 语句。运行 roq --help 查看用法。"); return ExitCode::from(EXIT_USAGE); }
    },
};
```

（本任务 gate 仍是无引擎版 `gate(&sql)`，Task 3 再改签名。）usage 文本 `cli.rs:145` 行尾补一句：「engine=oracle 仅允许 SELECT / WITH（SHOW/DESC/EXPLAIN 为 MySQL/SQL*Plus 方言或会写入 PLAN_TABLE）」。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test`
Expected: 全部 PASS

- [ ] **Step 5: Commit**

```bash
git add src/cli.rs src/main.rs
git commit -m "feat: 快捷子命令按引擎物化 SQL（oracle 走字典视图，explain 显式拒绝）"
```

---

### Task 3: 闸门引擎感知（gate.rs）+ 主流程重排

**Files:**
- Modify: `src/gate.rs`、`src/main.rs`、`src/cmd_diff.rs`
- Test: `src/gate.rs`（tests 内）

**Interfaces:**
- Consumes: `Engine`（Task 1）
- Produces: `pub fn gate(sql_raw: &str, engine: Engine) -> Result<String, String>`（签名变更，全部调用方随改）。

- [ ] **Step 1: 写 mysql 回归改造 + oracle 负例/正例测试**

既有 gate 测试全部改为 `gate(sql, Engine::Mysql)`（断言不变）。新增：

```rust
#[test]
fn oracle_accepts_select_and_with_only() {
    for sql in ["SELECT 1 FROM dual", "WITH c AS (SELECT 1 FROM dual) SELECT * FROM c"] {
        assert!(gate(sql, Engine::Oracle).is_ok(), "应放行：{}", sql);
    }
    for sql in ["SHOW TABLES", "DESC t", "EXPLAIN SELECT 1", "TABLE t", "VALUES (1)"] {
        assert!(gate(sql, Engine::Oracle).is_err(), "应拒绝（非 oracle 白名单首词）：{}", sql);
    }
}

#[test]
fn oracle_hash_and_comment_semantics_differ_from_mysql() {
    // OBJ# 是合法列名：mysql 会把 # 后当注释截掉（旧行为），oracle 必须原样保留
    let gated = gate("SELECT OBJ#, COUNT(*) FROM USER_OBJ$ WHERE OBJ# > 1", Engine::Oracle).unwrap();
    assert!(gated.contains("OBJ# > 1"), "oracle 下 # 不得截断语句：{}", gated);
    // oracle 的 -- 立即注释：5--3 是 "5" 加注释，不是减法
    let gated = gate("SELECT 5--3 FROM dual", Engine::Oracle).unwrap();
    assert!(gated.starts_with("SELECT 5"), "oracle 的 -- 即注释，3 应被剥离：{}", gated);
    // mysql 回归：5--3 是减法，须保留
    let gated = gate("SELECT 5--3 AS x", Engine::Mysql).unwrap();
    assert!(gated.contains("5--3"));
}

#[test]
fn oracle_string_literals_survive_scan() {
    // 单引号加倍 + 字面量含分号/#/--：不得误判多语句、不得被改写
    let gated = gate("SELECT 'it''s; ok--fine' FROM dual", Engine::Oracle).unwrap();
    assert!(gated.contains("it''s; ok--fine"));
    assert!(gate("SELECT 'a' ; DELETE FROM t", Engine::Oracle).is_err());
    // oracle 反引号非法，报可读错误而非"引号未闭合"
    let err = gate("SELECT `col` FROM t", Engine::Oracle).unwrap_err();
    assert!(err.contains("反引号"), "应点名反引号：{}", err);
}

#[test]
fn oracle_rejects_plsql_and_for_update() {
    // PL/SQL 匿名块（BEGIN 非白名单首词）、FOR UPDATE（禁词 UPDATE 全句扫描）
    assert!(gate("BEGIN NULL; END;", Engine::Oracle).is_err());
    assert!(gate("SELECT 1 FROM t FOR UPDATE", Engine::Oracle).is_err());
    assert!(gate("SELECT 1 FROM dual; SELECT 2 FROM dual", Engine::Oracle).is_err());
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib gate`
Expected: FAIL（签名未带 engine）

- [ ] **Step 3: 实现**

`gate.rs`：
- `gate(sql_raw, engine)`；`strip_and_check(sql, engine)` 增参，按引擎分流四处：
  - `'#'`：Mysql → `LineComment`；Oracle → 普通字符 push；
  - `"--"`：Mysql → 现状（后随空白/行尾才注释）；Oracle → 无条件 `LineComment`；
  - `Single/Double` 内 `'\\'`：Mysql → 现状转义消费；Oracle → 字面量 push（不消费下一字符）；
  - `'\''`（反引号）：Oracle → `return Err("Oracle 不使用反引号（标识符用双引号包裹或直接书写）；请改写后重试".to_string())`；Mysql → 现状 `Backtick` 态。
- 首词白名单分流：`LEADING_ALLOWED_MYSQL`（现表）与 `LEADING_ALLOWED_ORACLE = &["SELECT", "WITH"]`；`EXPLAIN ANALYZE` 检查仅在 Mysql 分支可达（保留在 mysql 分支内）。
- 禁词表与 `SELECT|WITH` 全句扫描逻辑两引擎共享，不动。
- 模块头注释补引擎分流说明。

`main.rs::run_query` 重排为：load profiles → `--list` → profile 查找（查不到的 config-error 审计里 sql 改为原始串）→ gate（`gate(&sql, profile.engine)`，gate-rejected 审计的 db 字段从 `""` 改为 `&profile.database`——spec 第 6 节已接受的可观察变化）。删除原"gate 在 profile 之前"的顺序。

`cmd_diff.rs::run` 重排：load profiles → 双 profile 查找 → `gate(&raw_sql, left_profile.engine)`（跨引擎拒绝属 Task 8，本任务两侧均为 mysql 时代）。gate-rejected 审计 db 传 `&left_profile.database`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test`
Expected: 全部 PASS（含既有 mysql 负例回归）

- [ ] **Step 5: Commit**

```bash
git add src/gate.rs src/main.rs src/cmd_diff.rs
git commit -m "feat: 只读闸门按引擎分流（oracle 白名单/注释/转义/反引号语义）+ 闸门后移至 profile 之后"
```

---

### Task 4: 连接适配层（conn.rs，mysql 侧先落地，行为不变）

**Files:**
- Create: `src/conn.rs`
- Modify: `src/query.rs`、`src/cmd_diff.rs`、`src/cmd_config.rs`、`src/lib.rs`
- Test: `src/conn.rs`（纯函数部分）

**Interfaces:**
- Produces: `pub enum RoqConn { Mysql(mysql::Conn), Oracle(<Task 5 填入>) }`；`pub fn connect_readonly(profile: &Profile, password: &str, notice: &dyn Fn(String)) -> Result<RoqConn, String>`（自 query.rs 迁入）；`impl RoqConn { pub fn query(&mut self, sql: &str) -> Result<RoqResult<'_>, String>; pub fn query_first_string(&mut self, sql: &str) -> Result<Option<String>, String> }`；`pub struct RoqResult<'a>` 持有 `columns: Vec<String>`（`pub fn columns(&self) -> &[String]`）并 `impl Iterator<Item = Result<Vec<CellValue>, String>>`（每行为归一 CellValue，类型映射 mysql=`raw_cell`、oracle=Task 5）。`lib.rs` 加 `pub mod conn;`。

- [ ] **Step 1: 建立适配层骨架并把 mysql 逻辑原样迁入**

`conn.rs`：`RoqConn` 先只有 `Mysql(mysql::Conn)` 一个 arm；`connect_readonly` 的 mysql 逻辑从 query.rs **逐行迁入**（SET + 回读断言 + max_execution_time 告警，reason 字符串原样）；`query()` 包装 `conn.query_iter(sql)`，列名在构造时取出，行迭代 `result.next()` → `row.unwrap().into_iter().map(raw_cell)`；`query_first_string` 包装 `query_first::<Option<String>>`。`query.rs`/`cmd_diff.rs::fetch_side`/`cmd_config.rs::test` 的连接与取行代码改走适配层（`query.rs` 顶部 `use crate::conn::...`，删除 mysql 直连代码；`cmd_config.rs::test` 的 `Conn::new(build_opts...)` 及后续 SET/回读/版本查询改走 `connect_readonly` + `query_first_string("SELECT VERSION()")`，成功文案保持逐字节一致）。

- [ ] **Step 2: 跑全量测试确认行为不变**

Run: `cargo test`
Expected: 全部 PASS（纯重构，零行为变化；无新测试——本任务的"测试"是既有 93+ 测试全绿）

- [ ] **Step 3: Commit**

```bash
git add src/conn.rs src/query.rs src/cmd_diff.rs src/cmd_config.rs src/lib.rs
git commit -m "refactor: 连接与执行收敛到 conn 适配层（mysql 行为不变，为 oracle 接入隔离驱动差异）"
```

---

### Task 5: oracle 接入（驱动依赖 + 连接 + 类型映射）

**Files:**
- Modify: `Cargo.toml`、`src/conn.rs`、`src/cmd_config.rs`
- Test: `src/conn.rs`（可构造纯函数）、`src/cmd_config.rs`（oracle profile 解析回归）

**Interfaces:**
- Consumes: Task 4 的 `RoqConn` 骨架
- Produces: `RoqConn::Oracle(oracledb::Connection)` arm 完整可用；oracle 单元格映射函数 `fn oracle_row_cells(row: &oracledb::Row, metas: &[oracledb::Metadata]) -> Result<Vec<CellValue>, String>`（模块内私有亦可）；日期格式化纯函数 `fn format_oracle_datetime(components) -> String`。

- [ ] **Step 1: 加依赖并验证驱动 API 形态**

`Cargo.toml` 加：

```toml
# Oracle 官方纯 Rust thin 驱动（无需 Instant Client）；目标库须 ≥ 12
oracledb = "26.0.0-beta.6"
```

Run: `cargo build`
Expected: 编译通过。**若 `Connection::query` 等实际签名与下列用法不符（beta API 变动），以 docs.rs 为准调整适配层内部实现，但 `RoqConn` 对外接口不得改变**；若驱动实际是 async API（编译发现），用 `pollster = "0.3"` 的 `block_on` 在适配层内封装，对外保持同步接口。

- [ ] **Step 2: 写可构造部分的失败测试**

```rust
#[test]
fn oracle_datetime_format_is_fixed_and_deterministic() {
    // DATE 无小数秒；TIMESTAMP 保留到 6 位并去尾零（不用会话 NLS，保证存档确定）
    assert_eq!(format_oracle_datetime(2026, 11, 16, 8, 30, 5, None), "2026-11-16 08:30:05");
    assert_eq!(format_oracle_datetime(2026, 11, 16, 8, 30, 5, Some(500_000_000)), "2026-11-16 08:30:05.5");
    assert_eq!(format_oracle_datetime(2026, 11, 16, 8, 30, 5, Some(0)), "2026-11-16 08:30:05");
}

#[test]
fn oracle_blob_cell_is_placeholder_without_reading_content() {
    assert_eq!(oracle_blob_placeholder(Some(1024)), "<BLOB 1024 bytes>");
    assert_eq!(oracle_blob_placeholder(None), "<BLOB>");
}
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test --lib conn`
Expected: FAIL（函数未定义）

- [ ] **Step 4: 实现 oracle arm**

`conn.rs`：
- **连接**：`Config::new().set_credentials(user, password).set_connect_string(&format!("{}:{}/{}", profile.host, profile.port, profile.database)).map_err(...)?.set_transport_connect_timeout(Some(Duration::from_secs(CONNECT_TIMEOUT_SECS)))` → `oracledb::connect(config)`（失败文案对齐 mysql 臂：`[roq] 连接失败：{e}`）。
- **第二层**：连接成功后第一条语句执行 `SET TRANSACTION READ ONLY`（Oracle 协议保证不被静默忽略，失败即中止，不做回读断言——spec 第 3 节决策；失败 reason `"session-readonly: {e}"`）。慢查询保险丝：Oracle 无会话级等价物，不发 SET（驱动无该配置时静默跳过，读超时由 Cursor 迭代自然受网络超时约束；若 beta 版驱动暴露 call/读超时配置项，按 `READ_TIMEOUT_SECS = 60s` 设置）。
- **列名**：`cursor.columns()` → 每个 `Metadata::name().to_string()`。
- **单元格映射**（`oracle_row_cells`）：
  - NULL：`row.get::<Option<String>>(i)` 返回 `None` → `CellValue::Null`（`Option<T>` 路径统一处理 NULL）；
  - NUMBER：**按字符串取出归 Text**（保 38 位精度）——首选 `Option<String>`；若编译证实 `String` 未实现 NUMBER 的 `FromDbValue`，改 `Option<OracleNumber>` + 其字符串化 API（实现期确认，映射结果不变：`CellValue::Text`）；
  - DATE/TIMESTAMP：`format_oracle_datetime` 固定格式（不用会话 NLS）；
  - CLOB：读为文本归 `CellValue::Text`（走现有 max-cell/存档限额体系）；
  - BLOB：按 `Metadata::db_type()` 判定（`DB_TYPE_BLOB`），**不读内容**，输出 `oracle_blob_placeholder(...)`（长度取 Lob 长度 API，取不到就 `None` 形态）；
  - 其它类型（INTERVAL 等）：字符串化归 Text（最坏信息不丢）。
- `cmd_config.rs::test` 的版本查询分流：oracle 用 `query_first_string("SELECT banner FROM v$version WHERE ROWNUM = 1")`，成功文案模板不变。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test`
Expected: 全部 PASS（驱动相关路径由 Task 10 的容器端到端验证）

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/conn.rs src/cmd_config.rs
git commit -m "feat: oracle 引擎接入（thin 驱动直连 + SET TRANSACTION READ ONLY + 精度保真类型映射）"
```

---

### Task 6: 单格交付豁免扩展到 GET_DDL（render.rs）

**Files:**
- Modify: `src/render.rs`、`src/query.rs`
- Test: `src/render.rs`

**Interfaces:**
- Produces: `pub fn is_get_ddl(sql: &str) -> bool`（词元判定：首词 SELECT、次词 DBMS_METADATA、三词 GET_DDL）；`terminal_max_cell` 参数 `is_show_create_stmt: bool` 更名 `is_single_cell_stmt: bool`（语义扩展）；`table_slug` 增加 GET_DDL arm。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn get_ddl_detection_and_slug() {
    assert!(is_get_ddl("SELECT DBMS_METADATA.GET_DDL('TABLE', 'T_USER') FROM dual"));
    assert!(!is_get_ddl("SELECT 1 FROM dual"));
    assert!(!is_get_ddl("SHOW CREATE TABLE t_user"));
    // 存档命名：GET_DDL 第二实参（词元位 4：SELECT DBMS_METADATA GET_DDL TABLE T_USER）作 slug
    assert_eq!(table_slug("SELECT DBMS_METADATA.GET_DDL('TABLE', 'T_USER') FROM dual"), Some("t_user".to_string()));
}

#[test]
fn terminal_max_cell_exempts_get_ddl_like_show_create() {
    assert_eq!(terminal_max_cell(false, true, false, 200), usize::MAX);
    // 显式 --max-cell 让位（与 SHOW CREATE TABLE 同语义）
    assert_eq!(terminal_max_cell(false, true, true, 200), 200);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib render`
Expected: FAIL（`is_get_ddl` 未定义）

- [ ] **Step 3: 实现**

`render.rs`：`is_get_ddl` 用 `extract_tokens`（输入为闸门规范化 SQL，词元全大写）；`table_slug` 在函数开头加 GET_DDL arm（`tokens[1] == "DBMS_METADATA" && tokens[2] == "GET_DDL"` → `tokens.get(4)` 小写化返回）；`terminal_max_cell` 参数更名。`query.rs` 调用点改为 `terminal_max_cell(cli.out.is_some(), is_show_create(sql) || is_get_ddl(sql), ...)`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test`
Expected: 全部 PASS

- [ ] **Step 5: Commit**

```bash
git add src/render.rs src/query.rs
git commit -m "feat: DBMS_METADATA.GET_DDL 享单格交付豁免与存档命名（对齐 SHOW CREATE TABLE）"
```

---

### Task 7: 审计行 engine 字段 + roq log 展示

**Files:**
- Modify: `src/audit.rs`、`src/cmd_log.rs`、`src/main.rs`、`src/query.rs`、`src/cmd_diff.rs`
- Test: `src/audit.rs`、`src/cmd_log.rs`

**Interfaces:**
- Produces: `audit_line(profile: &str, engine: &str, db: &str, ...)`（第二参数插入；JSON 在 `"profile"` 后加 `"engine":"{}"`）。所有调用点传 `profile.engine.as_str()`（profile 未取到前不可能调 audit——Task 3 已重排；`cmd_diff` 两侧各传各自 engine）。

- [ ] **Step 1: 写失败测试**

`audit.rs`：

```rust
#[test]
fn audit_line_carries_engine_after_profile() {
    let line = audit_line("dev", "oracle", "db", 1, false, 5, "ok", "", "");
    assert!(line.contains("\"profile\":\"dev\",\"engine\":\"oracle\""), "engine 紧随 profile：{}", line);
}
```

`cmd_log.rs`：

```rust
#[test]
fn format_line_defaults_missing_engine_to_mysql() {
    // 旧审计行（无 engine 字段）按 mysql 展示，不破历史可读性
    let value: serde_json::Value =
        serde_json::from_str(LINE_DEV_OK).unwrap();
    let text = format_line(&value);
    assert!(text.contains("mysql"), "缺 engine 的旧行应显示 mysql：{}", text);
}
```

（`LINE_DEV_OK` 常量不加 engine 字段——正好当作旧行样本；另加一条含 `"engine":"oracle"` 的常量断言其显示 oracle。）

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib audit cmd_log`
Expected: FAIL

- [ ] **Step 3: 实现**

`audit.rs::audit_line` 加第二参数 `engine: &str`，格式串 `"profile":"{}","engine":"{}"` 位次照测试。全部调用点（`main.rs` ×3：物化错误/gate-rejected/config-error；`query.rs::log_exit`；`cmd_diff.rs` ×4）传 engine。`cmd_log.rs::format_line` 取 `engine`（缺失默认 `"mysql"`），列布局 `time profile engine db outcome ...`（profile 列宽收到 10，engine 列宽 7——文案微调以对齐为准）。`line_matches` 不动。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test`
Expected: 全部 PASS

- [ ] **Step 5: Commit**

```bash
git add src/audit.rs src/cmd_log.rs src/main.rs src/query.rs src/cmd_diff.rs
git commit -m "feat: 审计行记录 engine 字段，roq log 展示并兼容无 engine 的历史行"
```

---

### Task 8: diff 跨引擎拒绝

**Files:**
- Modify: `src/cmd_diff.rs`
- Test: `src/cmd_diff.rs`

**Interfaces:**
- Consumes: `Profile.engine`（Task 1）
- Produces: 引擎不一致的拒绝路径（退出码 1、config-error 审计）。

- [ ] **Step 1: 写失败测试**

`cmd_diff.rs` 测试目前多用纯函数；本分支新增纯函数便于测试：

```rust
#[test]
fn cross_engine_refusal_message_is_explicit() {
    let msg = cross_engine_refusal("mysql", "oracle");
    assert!(msg.contains("mysql") && msg.contains("oracle"));
    assert!(msg.contains("不支持跨引擎"), "文案应直说不可跨引擎：{}", msg);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib cmd_diff`
Expected: FAIL（`cross_engine_refusal` 未定义）

- [ ] **Step 3: 实现**

`cmd_diff.rs`：纯函数 `fn cross_engine_refusal(left: &str, right: &str) -> String { format!("两侧引擎不同（{} vs {}），闸门语义与类型口径不同，不支持跨引擎比对", left, right) }`；`run()` 在双 profile 查找成功后、gate 之前：`if left_profile.engine != right_profile.engine` → eprintln 文案 + `append_audit_log(&audit_line(&left_name, left_profile.engine.as_str(), "", 0, false, 0, "config-error", &format!(",\"reason\":\"cross-engine\""), ""))` → 退出码 1。gate 用共同 engine（Task 3 已接）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test`
Expected: 全部 PASS

- [ ] **Step 5: Commit**

```bash
git add src/cmd_diff.rs
git commit -m "feat: diff 拒绝跨引擎比对（fail-closed，闸门语义与类型口径不同）"
```

---

### Task 9: config add --engine 与 test 引擎分流收尾

**Files:**
- Modify: `src/cli.rs`、`src/cmd_config.rs`
- Test: `src/cli.rs`、`src/cmd_config.rs`

**Interfaces:**
- Consumes: `Engine`、`RoqConn`（Task 4/5 已使 `config test` 走适配层——本任务只补 add 写入与 flag）
- Produces: `ConfigCmd::Add` 新增 `engine: Engine` 字段；profile 文件写入 `engine=oracle` 行（仅 oracle 时写，缺省即 mysql 不写）。

- [ ] **Step 1: 写失败测试**

`cmd_config.rs`（沿用 `add_with_sources` 测试模式）：

```rust
#[test]
fn add_writes_engine_line_for_oracle_and_refuses_ssl() {
    let path = temp_conf("eng-add");
    let _ = fs::remove_file(&path);
    let msg = add_with_sources("demo", "h", 1521, "u", Some("p"), None, "FREEPDB1", false,
        Some(path.clone()), false, Engine::Oracle, &[]).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("engine=oracle"), "应写入 engine 行：{}", text);
    assert!(msg.contains("oracle"), "成功提示应含引擎：{}", msg);
    // mysql 显式 engine 不写行（缺省即 mysql）
    let path2 = temp_conf("eng-add-m");
    add_with_sources("m", "h", 3306, "u", Some("p"), None, "d", false,
        Some(path2.clone()), false, Engine::Mysql, &[]).unwrap();
    assert!(!fs::read_to_string(&path2).unwrap().contains("engine="));
    // oracle + ssl 拒绝
    let err = add_with_sources("s", "h", 1521, "u", Some("p"), None, "d", true,
        Some(temp_conf("eng-add-s")), false, Engine::Oracle, &[]).unwrap_err();
    assert!(err.contains("ssl"), "应拒绝 oracle+ssl：{}", err);
}
```

`cli.rs`：解析测试——`--engine oracle` 进 `ConfigCmd::Add { engine: Engine::Oracle, .. }`；`--engine bad` 报用法错误；`--engine oracle --ssl` 报用法错误。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib cmd_config cli`
Expected: FAIL

- [ ] **Step 3: 实现**

`cli.rs::parse_config_cmd` 的 Add 分支解析 `--engine <值>`（`Engine::parse` 失败即用法错误；`engine == Oracle && ssl` → `Err("--engine oracle 不支持 --ssl（TCPS 连接未支持）")`），缺省 `Engine::Mysql`。`cmd_config.rs`：`add`/`add_with_sources` 签名各加 `engine: Engine`；section 拼接在 `database=` 行后按 engine 追加 `engine=oracle\n`（mysql 不追加）；成功提示文案追加 ` engine=oracle`（仅 oracle）。`run()` 的 Add 解构补传。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test`
Expected: 全部 PASS

- [ ] **Step 5: Commit**

```bash
git add src/cli.rs src/cmd_config.rs
git commit -m "feat: config add 支持 --engine（oracle 拒 --ssl，mysql 缺省不写引擎行）"
```

---

### Task 10: 文档、版本与全量回归

**Files:**
- Modify: `Cargo.toml`、`src/lib.rs`、`src/cli.rs`（usage）、`skill/roq/SKILL.md`、`README.md`、`README.zh-CN.md`、`CHANGELOG.md`

**Interfaces:** 无代码接口；产出发布物文案。

- [ ] **Step 1: 版本与代码内文档**

`Cargo.toml` `version = "1.0.0"`；`lib.rs` 头注释三层保障描述补引擎分流一句话（第 1/2 层各补 oracle 对应物）；`cli.rs::usage` 的"仅接受单条只读语句"行按 Task 2 已加的 oracle 说明核对一致。

- [ ] **Step 2: SKILL.md**

补 Oracle 章节（中文）：engine=oracle 用法、白名单差异（仅 SELECT/WITH）、tables/columns/schema 快捷子在 oracle 的等价行为、explain 不支持及原因、需要 Oracle Database ≥ 12。**不写任何本机路径/内网拓扑**（红线 6）。

- [ ] **Step 3: README 双语 + CHANGELOG**

`README.md`/`README.zh-CN.md` 同步：特性矩阵加 Oracle 列；新增"Oracle 支持"节（thin 驱动零客户端依赖、DB ≥ 12、`--engine oracle` 配置示例、快捷子命令对照表、explain 限制）。顶部互链不变。`CHANGELOG.md`：把现有 [Unreleased] 内容与新条目合并为 `## [1.0.0] - <发版日>`（英文）：Oracle dual-engine support（gate/config/conn/shortcuts/diff refusal/audit engine field）、DDL 免截断等既有 Unreleased 条目保留并入。

- [ ] **Step 4: 全量回归 + release 构建**

Run: `cargo test && cargo build --release`
Expected: 测试全绿；release 二进制生成，`ls -lh target/release/roq` 记录体积（写进 CHANGELOG 的 1.0.0 条目或备注，保持个位数 MB）。

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/cli.rs skill/roq/SKILL.md README.md README.zh-CN.md CHANGELOG.md
git commit -m "docs: v1.0.0 双引擎发版文档（SKILL/README 双语/CHANGELOG）并 bump 版本"
```

---

### Task 11: 容器端到端验收（真实 thin 驱动联调）

**Files:**
- Create（不入库）: 临时 seed SQL 与脚本放 `%TEMP%` 下，**不得写进仓库**
- Modify: 无（纯验收；发现问题回修对应任务）

**Interfaces:** 无；产出验收结论（记入最终汇报与 CHANGELOG 备注）。

- [ ] **Step 1: 起容器并等待就绪**

```bash
docker run -d --name roq-oracle-e2e -p 1521:1521 -e ORACLE_PASSWORD=oracle gvenzl/oracle-free:23-slim
# 轮询日志直至出现 "DATABASE IS READY TO USE"（首次约 2-5 分钟）
docker logs -f roq-oracle-e2e
```

若 Docker 不可用：明确标注"端到端未执行，待人工验收"，不得谎称完成。

- [ ] **Step 2: 灌种子数据（容器内 sqlplus，不经 roq——roq 是只读工具）**

`docker exec` 进容器以 `system/oracle@localhost/FREEPDB1` 执行 seed SQL（脚本先写到宿主临时目录再 `docker cp`）：

```sql
CREATE TABLE roq_e2e (
  id NUMBER(10) PRIMARY KEY,
  big_num NUMBER(38,0),
  price NUMBER(12,2),
  name VARCHAR2(100 CHAR),
  notes CLOB,
  payload BLOB,
  created DATE,
  updated TIMESTAMP(6)
);
INSERT INTO roq_e2e VALUES (1, 12345678901234567890123456789012345678, 19.99,
  '中文含''引号', TO_CLOB('长文本…含;分号与--横线'), NULL, DATE '2026-01-02', TIMESTAMP '2026-01-02 03:04:05.123456');
-- 另插 3 行普通值；再建第二张表 roq_e2e_b 供 tables 过滤与 schema 用
COMMIT;
```

- [ ] **Step 3: 录 profile 并核对基础链路**

```bash
cargo run -q -- config add oradev --host localhost --port 1521 --user system --password oracle --database FREEPDB1 --engine oracle
cargo run -q -- config test oradev        # 期望：连接成功 + banner + 会话只读
cargo run -q -- -p oradev "SELECT id, name FROM roq_e2e ORDER BY id"   # 期望：正常 tsv
```

- [ ] **Step 4: 断言清单（逐条跑、逐条核对输出）**

1. **38 位精度**：`SELECT big_num FROM roq_e2e WHERE id = 1` → tsv 与 `--format json` 输出均逐字符等于 `12345678901234567890123456789012345678`（json 里是**带引号的字符串**）。
2. **类型映射**：DATE → `2026-01-02 00:00:00`；TIMESTAMP → `2026-01-02 03:04:05.123456`；CLOB 内容完整；BLOB → `<BLOB ...>` 占位（不读内容）。
3. **快捷子命令**：`tables`（含 `roq_e2e` 两张表）；`tables e2e`（小写过滤命中——UPPER 生效）；`columns roq_e2e`（8 列含 notes/payload 注释列）；`schema roq_e2e`（单格完整 DDL，终端不截断）。
4. **explain 拒绝**：`roq explain "SELECT 1 FROM dual" -p oradev` → 退出码 1，文案含 PLAN_TABLE；审计有 config-error 行。
5. **闸门活体负例**：`"SELECT 1 FROM dual; DROP TABLE roq_e2e"` → 退出码 2；`"SELECT OBJ# FROM roq_e2e"` 之类含 `#` 标识（若列名无 #，用字符串 `'OBJ#'`）→ 正常执行不截断；`"SELECT 'a;b' FROM dual"` → 正常（多语句不误判）。
6. **审计与存档**：`roq log --last 10` 显示 oracle 行（engine 列）；`~/.roq/logs/` 当月 jsonl 中该批行含 `"engine":"oracle"`；`.roq/results/` 有 GET_DDL 命名存档（slug=t_roq_e2e 形态）。
7. **diff**：`roq diff dev oradev "SELECT 1 FROM dual"`（dev 为任一 mysql profile）→ 退出码 1 拒绝文案；若手头无 mysql 实例可连，仅验证拒绝文案（连不上即失败路径，不影响断言）。
8. **收尾**：`docker rm -f roq-oracle-e2e`；确认 `~/.roq/profiles.d/oradev.conf` 含 `engine=oracle` 且密码已 DPAPI 加密。

任一断言失败：回修对应任务代码、补回归测试，重跑本清单。

- [ ] **Step 5: 验收结论记录**

把通过/偏差情况写进最终汇报；CHANGELOG 1.0.0 条目补充实测体积与驱动版本。不新建仓库文件。

---

## Self-Review 记录

- **Spec 覆盖**：配置(engine/端口/ssl)、闸门六处方言差异、第二层 SET TRANSACTION、快捷子命令四条(含 UPPER 与 explain 拒绝)、类型映射(NUMBER/DATE/CLOB/BLOB)、审计 engine 字段、diff 跨引擎拒绝、gate 后移三副作用(Task 2/3 实现)、文档双语、容器联调——均有归属任务。`ssl=true` 加载拒绝=Task 1，`config add` 侧=Task 9。
- **类型一致性**：`Engine`/`Shortcut`/`gate(sql, engine)`/`RoqConn`/`audit_line(+engine)` 各任务间签名一致；`cross_engine_refusal`/`materialize_sql`/`is_get_ddl`/`format_oracle_datetime`/`oracle_blob_placeholder` 均有定义处与测试处。
- **Review Focus**:五条各落 Task 2/3/7/11 的测试步骤。
- **比例**：计划以签名、SQL 原文、断言为主，无整函数转录。
