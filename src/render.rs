//! 渲染：单元格类型归一 + TSV/JSON/CSV 三格式输出 + 存档文件名表名提取 + 终端截断策略。

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
/// {"columns":[...],"rows":[[...]],"rows_returned":N,"truncated":bool,"elapsedMs":N}
#[allow(clippy::too_many_arguments)]
pub fn render_json(
    columns: &[String],
    rows: &[Vec<CellValue>],
    truncated: bool,
    elapsed_ms: u128,
) -> String {
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
        "{{\"columns\":[{}],\"rows\":[{}],\"rows_returned\":{},\"truncated\":{},\"elapsedMs\":{}}}",
        cols.join(","),
        body.join(","),
        rows.len(),
        truncated,
        elapsed_ms
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
    // GET_DDL 语句：FROM dual 无表名语义，第二实参（词元位 4：SELECT DBMS_METADATA GET_DDL <类型> <对象名>）才是对象名
    if tokens.get(1).map(String::as_str) == Some("DBMS_METADATA")
        && tokens.get(2).map(String::as_str) == Some("GET_DDL")
    {
        return tokens
            .get(4)
            .map(|name| name.to_lowercase().chars().take(40).collect());
    }
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

/// 从 SQL 提取首个 `LIKE '<模式>'` 字面量内容（SHOW ... LIKE 等无表名可提取的场景），
/// 供存档文件名标注过滤模式：保留字母/数字/下划线/%，其余折叠为 -，限 40 字符。
/// 词级大小写不敏感扫描，不依赖 token 化（% 会被 extract_tokens 当分词符丢弃）。
pub fn like_slug(sql: &str) -> Option<String> {
    let chars: Vec<char> = sql.chars().collect();
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    for i in 0..chars.len() {
        let prev_is_word = i > 0 && is_word(chars[i - 1]);
        let matches_keyword = chars[i].to_ascii_lowercase() == 'l'
            && chars.get(i + 1).copied().is_some_and(|c| c.to_ascii_lowercase() == 'i')
            && chars.get(i + 2).copied().is_some_and(|c| c.to_ascii_lowercase() == 'k')
            && chars.get(i + 3).copied().is_some_and(|c| c.to_ascii_lowercase() == 'e');
        if prev_is_word || !matches_keyword {
            continue;
        }
        if chars.get(i + 4).copied().is_some_and(is_word) {
            continue;
        }
        let mut j = i + 4;
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        if chars.get(j) != Some(&'\'') {
            continue;
        }
        let mut pattern = String::new();
        let mut closed = false;
        let mut k = j + 1;
        while k < chars.len() {
            if chars[k] == '\'' {
                // 字面量内 '' 为转义单引号：取一个 ' 继续扫描，不在截断处提前收尾
                if chars.get(k + 1) == Some(&'\'') {
                    pattern.push('\'');
                    k += 2;
                    continue;
                }
                closed = true;
                break;
            }
            pattern.push(chars[k]);
            k += 1;
        }
        // 未闭合字面量（扫到串尾未见收尾引号）不产出，fail 侧拒绝
        if closed && !pattern.is_empty() {
            let slug: String = pattern
                .chars()
                .map(|c| if is_word(c) || c == '%' { c } else { '-' })
                .take(40)
                .collect();
            return Some(slug);
        }
    }
    None
}

/// 语句是否为 SHOW CREATE TABLE（输入应为闸门规范化后的 SQL）。
/// 词元由 extract_tokens 归全大写，大小写混写天然免疫；该语句的单格即交付物，终端不截断。
pub fn is_show_create(sql: &str) -> bool {
    let tokens = extract_tokens(sql);
    tokens.first().map(String::as_str) == Some("SHOW")
        && tokens.get(1).map(String::as_str) == Some("CREATE")
        && tokens.get(2).map(String::as_str) == Some("TABLE")
}

/// 语句是否为 Oracle `SELECT DBMS_METADATA.GET_DDL(...) FROM dual` 单格取 DDL（输入应为闸门规范化后的 SQL）。
/// 词元由 extract_tokens 归全大写（`.` 分词、`_` 保留），与 SHOW CREATE TABLE 同享单格交付豁免。
pub fn is_get_ddl(sql: &str) -> bool {
    let tokens = extract_tokens(sql);
    tokens.first().map(String::as_str) == Some("SELECT")
        && tokens.get(1).map(String::as_str) == Some("DBMS_METADATA")
        && tokens.get(2).map(String::as_str) == Some("GET_DDL")
}

/// 终端 tsv 单格截断上限：--out 全量；单格交付语句（SHOW CREATE TABLE / GET_DDL，未显式限流时）全量；其余按 --max-cell。
pub fn terminal_max_cell(
    is_out: bool,
    is_single_cell_stmt: bool,
    max_cell_explicit: bool,
    max_cell: usize,
) -> usize {
    if is_out || (is_single_cell_stmt && !max_cell_explicit) {
        usize::MAX
    } else {
        max_cell
    }
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
        let doc = render_json(&columns, &rows, false, 17);
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
        assert!(doc.contains("\"elapsedMs\":17"), "应含耗时：{}", doc);
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

    #[test]
    fn get_ddl_detection_and_slug() {
        assert!(is_get_ddl(
            "SELECT DBMS_METADATA.GET_DDL('TABLE', 'T_USER') FROM dual"
        ));
        assert!(!is_get_ddl("SELECT 1 FROM dual"));
        assert!(!is_get_ddl("SHOW CREATE TABLE t_user"));
        // 存档命名：GET_DDL 第二实参（词元位 4：SELECT DBMS_METADATA GET_DDL TABLE T_USER）作 slug
        assert_eq!(
            table_slug("SELECT DBMS_METADATA.GET_DDL('TABLE', 'T_USER') FROM dual"),
            Some("t_user".to_string())
        );
    }

    #[test]
    fn terminal_max_cell_exempts_get_ddl_like_show_create() {
        assert_eq!(terminal_max_cell(false, true, false, 200), usize::MAX);
        // 显式 --max-cell 让位（与 SHOW CREATE TABLE 同语义）
        assert_eq!(terminal_max_cell(false, true, true, 200), 200);
    }

    #[test]
    fn like_slug_extracts_pattern_literal() {
        assert_eq!(
            like_slug("SHOW TABLES LIKE '%mark%'").as_deref(),
            Some("%mark%")
        );
        // 大小写不敏感 + 前缀匹配模式
        assert_eq!(
            like_slug("show full columns from `t_user` like 'task%'").as_deref(),
            Some("task%")
        );
        // 字面量内 '' 转义穿透扫描不提前截断；' 不属文件名安全字符，折叠为 -
        assert_eq!(
            like_slug("SHOW TABLES LIKE '%a''b%'").as_deref(),
            Some("%a-b%")
        );
        // 空字面量与未闭合字面量不产出
        assert_eq!(like_slug("SHOW TABLES LIKE ''"), None);
        assert_eq!(like_slug("SHOW TABLES LIKE '%open"), None);
        // 非 LIKE 关键词（词内片段）不误触
        assert_eq!(like_slug("SELECT unlike_col FROM t"), None);
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
