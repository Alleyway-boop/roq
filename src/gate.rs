//! 本地只读闸门：语句在触网之前先在这里过审。
//!
//! fail-closed 设计：宁可误拒（提示改写），绝不漏放；
//! 词边界匹配保证 `update_time`、`create_time` 等列名不误伤。

/// 语句首关键词白名单（全大写比较）。
const LEADING_ALLOWED: &[&str] = &[
    "SELECT", "SHOW", "EXPLAIN", "DESCRIBE", "DESC", "WITH", "TABLE", "VALUES",
];

/// 全句扫描禁词：写/管理/文件/锁类。
const FORBIDDEN_TOKENS: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "REPLACE", "CREATE", "ALTER", "DROP", "TRUNCATE", "RENAME",
    "GRANT", "REVOKE", "LOCK", "UNLOCK", "CALL", "SET", "LOAD", "HANDLER", "DO", "INTO",
    "OUTFILE", "DUMPFILE", "KILL", "SHUTDOWN", "PREPARE", "EXECUTE", "DEALLOCATE", "SIGNAL",
    "RESIGNAL", "RESET", "PURGE", "ANALYZE", "OPTIMIZE", "REPAIR", "FLUSH", "INSTALL",
    "UNINSTALL", "IMPORT", "BINLOG", "CACHE", "START", "STOP", "XA", "SAVEPOINT", "ROLLBACK",
    "COMMIT", "CHANGE", "LOAD_FILE", "GET_LOCK", "RELEASE_LOCK", "BENCHMARK", "SLEEP",
];

/// 语句长度上限（字节），防止把整段内容塞进参数。
const MAX_SQL_LEN: usize = 100_000;

/// 提取语句中的词元（含引号/反引号内文本——宁可误拒，不可漏放）。
/// 仅保留含字母的词元，纯数字跳过。
pub fn extract_tokens(sql: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let flush = |current: &mut String, tokens: &mut Vec<String>| {
        if current.chars().any(|c| c.is_ascii_alphabetic()) {
            tokens.push(std::mem::take(current));
        } else {
            current.clear();
        }
    };
    for ch in sql.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch.to_ascii_uppercase());
        } else if !current.is_empty() {
            flush(&mut current, &mut tokens);
        }
    }
    if !current.is_empty() {
        flush(&mut current, &mut tokens);
    }
    tokens
}

/// 引号/注释感知扫描：剥离 `-- ` 与 `#` 行注释（返回的语句语义等价、无注释）；
/// 字符串与注释之外的 `;` 视为多语句注入；引号未闭合直接拒绝（fail-closed）。
/// 说明：MySQL 行注释要求 `--` 后跟空白或行尾；反斜杠转义在单/双引号内生效，反引号内不生效。
fn strip_and_check(sql: &str) -> Result<String, String> {
    #[derive(PartialEq)]
    enum ScanState {
        Normal,
        Single,
        Double,
        Backtick,
        LineComment,
    }
    let chars: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut state = ScanState::Normal;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match state {
            ScanState::Normal => match c {
                '\'' => {
                    state = ScanState::Single;
                    out.push(c);
                }
                '"' => {
                    state = ScanState::Double;
                    out.push(c);
                }
                '`' => {
                    state = ScanState::Backtick;
                    out.push(c);
                }
                '#' => state = ScanState::LineComment,
                '-' if i + 2 <= chars.len()
                    && chars.get(i + 1) == Some(&'-')
                    && chars.get(i + 2).map_or(true, |next| next.is_whitespace()) =>
                {
                    state = ScanState::LineComment;
                    i += 1;
                }
                ';' => return Err("检测到多条语句（分号）；仅允许单条只读语句".to_string()),
                _ => out.push(c),
            },
            ScanState::Single | ScanState::Double => {
                out.push(c);
                let quote = if state == ScanState::Single { '\'' } else { '"' };
                if c == '\\' && i + 1 < chars.len() {
                    out.push(chars[i + 1]);
                    i += 1;
                } else if c == quote {
                    state = ScanState::Normal;
                }
            }
            ScanState::Backtick => {
                out.push(c);
                if c == '`' {
                    state = ScanState::Normal;
                }
            }
            ScanState::LineComment => {
                if c == '\n' {
                    state = ScanState::Normal;
                    out.push(c);
                }
            }
        }
        i += 1;
    }
    if matches!(state, ScanState::Single | ScanState::Double | ScanState::Backtick) {
        return Err("引号未闭合".to_string());
    }
    Ok(out)
}

/// 本地只读闸门：通过则返回规范化 SQL（已剥离行注释），否则返回拒绝原因。
pub fn gate(sql_raw: &str) -> Result<String, String> {
    let mut sql = sql_raw.trim().to_string();
    if sql.is_empty() {
        return Err("空语句".to_string());
    }
    if sql.len() > MAX_SQL_LEN {
        return Err(format!("语句超过 {} 字符上限", MAX_SQL_LEN));
    }
    if sql.contains('\0') {
        return Err("包含 NUL 字符".to_string());
    }
    if sql.contains("/*") || sql.contains("*/") {
        return Err("包含块注释 /* */（版本注释可执行代码，一律拒绝）；请移除后重试".to_string());
    }
    // 仅允许去掉一个行尾分号；其余分号交由扫描器按引号/注释上下文判定。
    if sql.ends_with(';') {
        sql.pop();
        sql = sql.trim_end().to_string();
    }
    let sql = strip_and_check(&sql)?;
    let tokens = extract_tokens(&sql);
    let Some(first) = tokens.first() else {
        return Err("未识别到任何语句关键词".to_string());
    };
    if !LEADING_ALLOWED.contains(&first.as_str()) {
        return Err(format!(
            "首关键词 {} 不在只读白名单（SELECT/SHOW/EXPLAIN/DESC/DESCRIBE/WITH/TABLE/VALUES）内",
            first
        ));
    }
    // EXPLAIN ANALYZE 会真实执行底层语句，明确拒绝。
    if first == "EXPLAIN" && tokens.get(1).map(String::as_str) == Some("ANALYZE") {
        return Err("EXPLAIN ANALYZE 会真实执行底层语句，已拒绝".to_string());
    }
    // SHOW/EXPLAIN/DESC 后接对象名或执行计划，跳过禁词扫描（如 SHOW CREATE TABLE）；
    // SELECT/WITH/TABLE/VALUES 走全句禁词扫描（WITH 后可接 UPDATE/DELETE，必须查）。
    if matches!(first.as_str(), "SELECT" | "WITH" | "TABLE" | "VALUES") {
        for token in &tokens {
            if FORBIDDEN_TOKENS.contains(&token.as_str()) {
                return Err(format!("语句包含写/管理类关键词 {}；本工具仅允许只读查询", token));
            }
        }
    }
    Ok(sql)
}

#[cfg(test)]
mod tests {
    use super::gate;

    #[test]
    fn rejects_write_and_admin_statements() {
        for sql in [
            "DELETE FROM t WHERE id=1",
            "UPDATE t SET a=1",
            "INSERT INTO t VALUES (1)",
            "SET @a=1",
            "TRUNCATE TABLE t",
            "DROP TABLE t",
            "CALL p()",
        ] {
            assert!(gate(sql).is_err(), "应拒绝：{}", sql);
        }
    }

    #[test]
    fn rejects_smuggling_attempts() {
        for sql in [
            "SELECT 1; DELETE FROM t",
            "SELECT 1; SELECT 2",
            "SELECT /* DELETE FROM t */ 1",
            "EXPLAIN ANALYZE DELETE FROM t",
            "SELECT * FROM t INTO OUTFILE '/tmp/x'",
            "WITH c AS (SELECT 1) DELETE FROM t",
            "SELECT SLEEP(10)",
            "SELECT LOAD_FILE('/etc/passwd')",
            "SELECT 'unterminated",
        ] {
            assert!(gate(sql).is_err(), "应拒绝：{}", sql);
        }
    }

    #[test]
    fn accepts_readonly_statements() {
        for sql in [
            "SELECT 1",
            "SELECT COUNT(*) FROM t_question WHERE update_time > '2026-01-01'",
            "SHOW CREATE TABLE t_task",
            "EXPLAIN SELECT id FROM t",
            "DESC t_question",
            "WITH c AS (SELECT 1) SELECT * FROM c",
        ] {
            assert!(gate(sql).is_ok(), "应放行：{}", sql);
        }
    }

    #[test]
    fn semicolon_inside_string_literal_is_legal() {
        // HTML 数据查询高频形态：&nbsp; 含分号，不得误判多语句
        let gated = gate("SELECT id FROM t WHERE analysis LIKE '%&nbsp;%'").unwrap();
        assert!(gated.contains("&nbsp;"));
    }

    #[test]
    fn line_comments_are_stripped_and_harmless() {
        let gated = gate("SELECT 1 AS a -- 注释里写 DELETE 也没事").unwrap();
        assert!(!gated.contains("DELETE"));
        assert!(gated.starts_with("SELECT 1 AS a"));
        let gated = gate("-- 前置注释\nSELECT 2 AS b").unwrap();
        assert!(gated.contains("SELECT 2 AS b"));
    }

    #[test]
    fn double_minus_without_space_is_arithmetic() {
        // MySQL 规定 `--` 后必须跟空白才是注释；5--3 是减法，须原样保留
        let gated = gate("SELECT 5--3 AS x").unwrap();
        assert!(gated.contains("5--3"));
    }

    #[test]
    fn doubled_single_quote_escapes() {
        assert!(gate("SELECT 'it''s' AS q").is_ok());
    }

    #[test]
    fn trailing_semicolon_is_tolerated_once() {
        assert!(gate("SELECT 1 ;").is_ok() || gate("SELECT 1;").is_ok());
    }
}
