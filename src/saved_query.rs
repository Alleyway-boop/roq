//! 保存查询（roq q）的纯函数层：查询名校验与占位符提取，无任何 I/O。
//!
//! [`validate_query_name`] 约束保存名可安全用作文件名与 CLI 参数：
//! 非空、仅 `[A-Za-z0-9_-]`、不以 `-` 开头（防被误认为 flag）、
//! 不得占用 [`RESERVED_QUERY_NAMES`] 中的子命令保留名。
//! [`extract_placeholder_names`] 扫描模板提取全部 `{{名}}` 占位符：
//! 名限 `[A-Za-z0-9_]+`，按首现顺序去重；`{{` 未闭合、名为空或含非法字符
//! 一律报错并附位置描述（fail-closed）。

/// 保存名保留字：roq 既有子命令与 q 族管理子命令，保存名不得占用，
/// 避免 `roq q show` 之类的调用无法区分"跑名为 show 的查询"与"列出查询"。
pub const RESERVED_QUERY_NAMES: &[&str] = &[
    "config", "log", "skill", "diff", "tables", "schema", "columns", "explain", "q", "add", "list",
    "show", "remove", "help",
];

/// 校验保存查询名：非空、仅 `[A-Za-z0-9_-]`、不以 `-` 开头、不在保留名表中。
pub fn validate_query_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("查询名不能为空".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("查询名 {name} 仅允许字母/数字/下划线/连字符"));
    }
    if name.starts_with('-') {
        return Err(format!("查询名 {name} 不能以 '-' 开头"));
    }
    if RESERVED_QUERY_NAMES.contains(&name) {
        return Err(format!("查询名 {name} 是 roq 保留名，不允许使用"));
    }
    Ok(())
}

/// 占位符名仅允许 `[A-Za-z0-9_]`，不含 `-`（比保存名字符集更严）。
fn is_placeholder_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// 提取模板中全部 `{{名}}` 占位符名，按首现顺序去重。
///
/// 名段仅允许 `[A-Za-z0-9_]+`；`{{` 未闭合、名为空或名含非法字符
/// 一律报错并附模板中的位置描述（fail-closed，不做宽松解析）。
pub fn extract_placeholder_names(template: &str) -> Result<Vec<String>, String> {
    let chars: Vec<char> = template.chars().collect();
    let mut names: Vec<String> = Vec::new();
    let mut i = 0;
    while i + 1 < chars.len() {
        if chars[i] != '{' || chars[i + 1] != '{' {
            i += 1;
            continue;
        }
        let name_start = i + 2;
        let mut end = name_start;
        while end < chars.len() && is_placeholder_char(chars[end]) {
            end += 1;
        }
        if end + 1 < chars.len() && chars[end] == '}' && chars[end + 1] == '}' {
            let name: String = chars[name_start..end].iter().collect();
            if name.is_empty() {
                return Err(format!(
                    "占位符名为空：模板第 {} 个字符处的 '{{{{}}}}' 不含名字",
                    i + 1
                ));
            }
            if !names.contains(&name) {
                names.push(name);
            }
            i = end + 2;
            continue;
        }
        if end >= chars.len() || chars[end] == '}' {
            return Err(format!(
                "占位符未闭合：模板第 {} 个字符处的 '{{{{' 没有匹配的 '}}}}'",
                i + 1
            ));
        }
        return Err(format!(
            "占位符名含非法字符 '{}'（模板第 {} 个字符）：仅允许字母/数字/下划线",
            chars[end],
            end + 1
        ));
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_query_name_accepts_safe_names() {
        assert_eq!(validate_query_name("q-by-id"), Ok(()));
        assert_eq!(validate_query_name("t_cnt"), Ok(()));
        assert_eq!(validate_query_name("A_1"), Ok(()));
    }

    #[test]
    fn validate_query_name_rejects_unsafe() {
        assert!(validate_query_name("").is_err());
        assert!(validate_query_name("a/b").is_err());
        assert!(validate_query_name("a b").is_err());
        assert!(validate_query_name("-x").is_err());
        for reserved in RESERVED_QUERY_NAMES {
            assert!(
                validate_query_name(reserved).is_err(),
                "保留名 {reserved} 应被拒绝"
            );
        }
        assert!(validate_query_name("帮助").is_err());
    }

    #[test]
    fn extract_placeholder_names_orders_and_dedups() {
        assert_eq!(
            extract_placeholder_names("SELECT {{b}} '{{a}}' {{b}}"),
            Ok(vec!["b".to_string(), "a".to_string()])
        );
    }

    #[test]
    fn extract_placeholder_names_rejects_malformed() {
        // 四类坏模板：未闭合、名含空格、名含非法字符、空名
        for bad in ["{{", "{{ a}}", "{{a-b}}", "{{}}"] {
            let err = extract_placeholder_names(bad).expect_err("坏模板应报错");
            assert!(err.contains("第"), "报错应含位置描述：{err}");
        }
    }

    #[test]
    fn template_without_placeholders_ok() {
        assert_eq!(extract_placeholder_names("SELECT 1"), Ok(vec![]));
    }
}
