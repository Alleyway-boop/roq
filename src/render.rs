//! 渲染：单元格类型归一 + TSV/JSON/CSV 三格式输出 + 存档文件名表名提取。

use crate::audit::json_escape;
use crate::gate::extract_tokens;
use mysql::Value;

/// 输出格式（--format）。json/csv 为交换格式，单元格不截断（截断即造假数据）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Tsv,
    Json,
    Csv,
}

impl OutputFormat {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "tsv" => Ok(OutputFormat::Tsv),
            "json" => Ok(OutputFormat::Json),
            "csv" => Ok(OutputFormat::Csv),
            other => Err(format!("--format 不支持 {}（可选 tsv/json/csv）", other)),
        }
    }
}

/// 单元格归一值：各输出格式的共同底座（类型信息在此保留，转义交给具体格式）。
#[derive(Debug, Clone)]
pub enum CellValue {
    Null,
    /// 数字（Int/UInt/有限浮点）：JSON 原样输出，TSV/CSV 直接拼接。
    Num(String),
    Text(String),
}

/// 把 mysql 值归一为 CellValue。非有限浮点（NaN/Infinity）归文本——JSON 数字位不容纳。
pub fn raw_cell(value: Value) -> CellValue {
    match value {
        Value::NULL => CellValue::Null,
        Value::Int(v) => CellValue::Num(v.to_string()),
        Value::UInt(v) => CellValue::Num(v.to_string()),
        Value::Float(v) if v.is_finite() => CellValue::Num(v.to_string()),
        Value::Double(v) if v.is_finite() => CellValue::Num(v.to_string()),
        Value::Bytes(bytes) => CellValue::Text(String::from_utf8_lossy(&bytes).to_string()),
        other => CellValue::Text(other.as_sql(true)),
    }
}

/// 单元格转义为 TSV 文本（不截断）：NULL -> \N；\t \n \r -> 可见转义序列，保真且保持 TSV 单行结构。
pub fn tsv_cell(cell: CellValue) -> String {
    let raw = match cell {
        CellValue::Null => "\\N".to_string(),
        CellValue::Num(n) => n,
        CellValue::Text(t) => t,
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

/// mysql Value 的 TSV 兼容入口：归一后转义（等价于 tsv_cell(raw_cell(value))）。
pub fn escape_cell(value: Value) -> String {
    tsv_cell(raw_cell(value))
}

/// 单元格渲染为 CSV 字段（RFC 4180）：NULL -> 空字段；含逗号/引号/换行的文本加引号。
pub fn csv_cell(cell: CellValue) -> String {
    match cell {
        CellValue::Null => String::new(),
        CellValue::Num(n) => n,
        CellValue::Text(t) => {
            if t.contains(',') || t.contains('"') || t.contains('\n') || t.contains('\r') {
                format!("\"{}\"", t.replace('"', "\"\""))
            } else {
                t
            }
        }
    }
}

/// 单元格渲染为 JSON 值：null / 原样数字 / 转义字符串。
pub fn json_cell(cell: CellValue) -> String {
    match cell {
        CellValue::Null => "null".to_string(),
        CellValue::Num(n) => n,
        CellValue::Text(t) => format!("\"{}\"", json_escape(&t)),
    }
}

/// 渲染完整 JSON 结果文档（单行紧凑，便于管道 jq）：
/// {"columns":[...],"rows":[[...]],"rows_returned":N,"truncated":bool}
pub fn render_json(columns: &[String], rows: &[Vec<CellValue>], truncated: bool) -> String {
    let cols: Vec<String> = columns
        .iter()
        .map(|c| format!("\"{}\"", json_escape(c)))
        .collect();
    let body: Vec<String> = rows
        .iter()
        .map(|row| {
            format!(
                "[{}]",
                row.iter()
                    .map(|cell| json_cell(cell.clone()))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        })
        .collect();
    format!(
        "{{\"columns\":[{}],\"rows\":[{}],\"rows_returned\":{},\"truncated\":{}}}",
        cols.join(","),
        body.join(","),
        rows.len(),
        truncated
    )
}

/// 渲染完整 CSV（表头 + 数据行，\n 换行；NULL 为空字段）。
pub fn render_csv(columns: &[String], rows: &[Vec<CellValue>]) -> String {
    let mut out = String::new();
    let header: Vec<String> = columns
        .iter()
        .map(|c| csv_cell(CellValue::Text(c.clone())))
        .collect();
    out.push_str(&header.join(","));
    for row in rows {
        out.push('\n');
        let cells: Vec<String> = row.iter().map(|cell| csv_cell(cell.clone())).collect();
        out.push_str(&cells.join(","));
    }
    out
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
            "SELECT"
                | "FROM"
                | "WHERE"
                | "JOIN"
                | "ON"
                | "GROUP"
                | "ORDER"
                | "BY"
                | "LIMIT"
                | "UNION"
                | "ALL"
                | "AS"
                | "AND"
                | "OR"
                | "NOT"
                | "IN"
                | "EXISTS"
                | "SET"
                | "VALUES"
                | "WITH"
                | "HAVING"
                | "OFFSET"
                | "DISTINCT"
                | "CASE"
                | "WHEN"
                | "THEN"
                | "ELSE"
                | "END"
                | "LEFT"
                | "RIGHT"
                | "INNER"
                | "OUTER"
                | "CROSS"
                | "TABLES"
                | "COLUMNS"
                | "IF"
                | "SUM"
                | "COUNT"
                | "AVG"
                | "MAX"
                | "MIN"
        )
    };
    let tokens = extract_tokens(sql);
    for (index, token) in tokens.iter().enumerate() {
        if matches!(
            token.as_str(),
            "FROM" | "JOIN" | "TABLE" | "DESCRIBE" | "DESC" | "EXPLAIN"
        ) {
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
    fn output_format_parses_known_values_and_rejects_others() {
        assert!(matches!(
            OutputFormat::parse("tsv").unwrap(),
            OutputFormat::Tsv
        ));
        assert!(matches!(
            OutputFormat::parse("json").unwrap(),
            OutputFormat::Json
        ));
        assert!(matches!(
            OutputFormat::parse("csv").unwrap(),
            OutputFormat::Csv
        ));
        let err = OutputFormat::parse("xml").unwrap_err();
        assert!(err.contains("xml"), "报错应回显非法值：{}", err);
    }

    #[test]
    fn raw_cell_maps_mysql_types() {
        assert!(matches!(raw_cell(Value::NULL), CellValue::Null));
        assert!(matches!(raw_cell(Value::Int(-5)), CellValue::Num(n) if n == "-5"));
        assert!(matches!(raw_cell(Value::UInt(7)), CellValue::Num(n) if n == "7"));
        assert!(matches!(raw_cell(Value::Float(1.5)), CellValue::Num(n) if n == "1.5"));
        // 非有限浮点不能进 JSON 数字位（JSON 无 NaN/Infinity），归文本
        assert!(matches!(
            raw_cell(Value::Float(f32::NAN)),
            CellValue::Text(_)
        ));
        assert!(matches!(
            raw_cell(Value::Bytes(b"bytes".to_vec())),
            CellValue::Text(t) if t == "bytes"
        ));
    }

    #[test]
    fn csv_cell_follows_rfc4180_and_null_is_empty() {
        assert_eq!(csv_cell(CellValue::Null), "");
        assert_eq!(csv_cell(CellValue::Num("42".into())), "42");
        assert_eq!(csv_cell(CellValue::Text("plain".into())), "plain");
        assert_eq!(csv_cell(CellValue::Text("a,b".into())), "\"a,b\"");
        assert_eq!(csv_cell(CellValue::Text("a\"b".into())), "\"a\"\"b\"");
        assert_eq!(csv_cell(CellValue::Text("a\nb".into())), "\"a\nb\"");
    }

    #[test]
    fn json_cell_renders_types_with_proper_escaping() {
        assert_eq!(json_cell(CellValue::Null), "null");
        assert_eq!(json_cell(CellValue::Num("-5".into())), "-5");
        assert_eq!(json_cell(CellValue::Text("a\"b\n".into())), "\"a\\\"b\\n\"");
        assert_eq!(json_cell(CellValue::Text("中文".into())), "\"中文\"");
    }

    #[test]
    fn render_json_document_shape() {
        let columns = vec!["id".to_string(), "name".to_string()];
        let rows = vec![
            vec![CellValue::Num("1".into()), CellValue::Text("alice".into())],
            vec![CellValue::Num("2".into()), CellValue::Null],
        ];
        let doc = render_json(&columns, &rows, false);
        assert!(
            doc.starts_with('{') && doc.ends_with('}'),
            "应为单行 JSON 文档：{}",
            doc
        );
        assert!(doc.contains("\"columns\":[\"id\",\"name\"]"));
        assert!(
            doc.contains("\"rows\":[[1,\"alice\"],[2,null]]"),
            "行数组应保真：{}",
            doc
        );
        assert!(doc.contains("\"rows_returned\":2"));
        assert!(doc.contains("\"truncated\":false"));
    }

    #[test]
    fn render_csv_has_header_and_escaped_body() {
        let columns = vec!["a".to_string(), "b".to_string()];
        let rows = vec![
            vec![CellValue::Text("x,y".into()), CellValue::Num("1".into())],
            vec![CellValue::Null, CellValue::Text("z".into())],
        ];
        let out = render_csv(&columns, &rows);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "a,b");
        assert_eq!(lines[1], "\"x,y\",1");
        assert_eq!(lines[2], ",z", "NULL 为空字段");
    }

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
        assert_eq!(
            table_slug("SELECT id FROM t_question WHERE id=1").as_deref(),
            Some("t_question")
        );
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
