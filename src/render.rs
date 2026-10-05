//! TSV 渲染：单元格转义、终端截断、存档文件名表名提取。

use crate::gate::extract_tokens;
use mysql::Value;

/// 单元格转义（不截断）：NULL -> \N；\t \n \r -> 可见转义序列，保真且保持 TSV 单行结构。
pub fn escape_cell(value: Value) -> String {
    let raw = match value {
        Value::NULL => "\\N".to_string(),
        Value::Int(v) => v.to_string(),
        Value::UInt(v) => v.to_string(),
        Value::Float(v) => v.to_string(),
        Value::Double(v) => v.to_string(),
        Value::Bytes(bytes) => String::from_utf8_lossy(&bytes).to_string(),
        other => other.as_sql(true),
    };
    raw.chars()
        .flat_map(|c| match c {
            '\t' => "\\t".chars().collect::<Vec<char>>(),
            '\r' => "\\r".chars().collect::<Vec<char>>(),
            '\n' => "\\n".chars().collect::<Vec<char>>(),
            _ => vec![c],
        })
        .collect()
}

/// 终端展示截断（存档保留全量，仅终端限流防刷屏）。
pub fn truncate_cell(escaped: String, max_cell: usize) -> String {
    if escaped.chars().count() <= max_cell {
        escaped
    } else {
        let cut: String = escaped.chars().take(max_cell).collect();
        format!("{}…", cut)
    }
}

/// 从 SQL 提取首个表名做存档文件名 slug（FROM/JOIN/TABLE 等后的首个非关键词标识符）。
pub fn table_slug(sql: &str) -> Option<String> {
    let is_keyword = |word: &str| {
        matches!(
            word,
            "SELECT" | "FROM" | "WHERE" | "JOIN" | "ON" | "GROUP" | "ORDER" | "BY" | "LIMIT"
                | "UNION" | "ALL" | "AS" | "AND" | "OR" | "NOT" | "IN" | "EXISTS" | "SET"
                | "VALUES" | "WITH" | "HAVING" | "OFFSET" | "DISTINCT" | "CASE" | "WHEN"
                | "THEN" | "ELSE" | "END" | "LEFT" | "RIGHT" | "INNER" | "OUTER" | "CROSS"
                | "TABLES" | "COLUMNS" | "IF" | "SUM" | "COUNT" | "AVG" | "MAX" | "MIN"
        )
    };
    let tokens = extract_tokens(sql);
    for (index, token) in tokens.iter().enumerate() {
        if matches!(token.as_str(), "FROM" | "JOIN" | "TABLE" | "DESCRIBE" | "DESC" | "EXPLAIN") {
            if let Some(next) = tokens.get(index + 1) {
                let starts_id = next
                    .chars()
                    .next()
                    .map(|c| c.is_ascii_alphabetic() || c == '_')
                    .unwrap_or(false);
                if starts_id && !is_keyword(next) {
                    return Some(next.to_lowercase().chars().take(40).collect());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_cell_keeps_single_line_and_null() {
        let escaped = escape_cell(Value::Bytes(b"a\tb\nc".to_vec()));
        assert_eq!(escaped, "a\\tb\\nc");
        assert_eq!(escape_cell(Value::NULL), "\\N");
        assert_eq!(escape_cell(Value::Int(42)), "42");
    }

    #[test]
    fn truncate_cell_marks_ellipsis_by_chars() {
        let kept = truncate_cell(" ABC ".to_string(), 5);
        assert_eq!(kept, " ABC ");
        let cut = truncate_cell("中文字符截断测试".to_string(), 4);
        assert_eq!(cut, "中文字符…"); // 按字符数而非字节数截断
    }

    #[test]
    fn table_slug_picks_first_table() {
        assert_eq!(table_slug("SELECT id FROM t_question WHERE id=1").as_deref(), Some("t_question"));
        assert_eq!(
            table_slug("SELECT a FROM x JOIN y ON x.id=y.id").as_deref(),
            Some("x")
        );
        // 子查询：跳过内层 SELECT 关键词后仍能命中内层 FROM
        assert_eq!(
            table_slug("SELECT * FROM (SELECT id FROM t_inner) t_outer").as_deref(),
            Some("t_inner")
        );
        assert_eq!(table_slug("SELECT 1").as_deref(), None);
    }
}
