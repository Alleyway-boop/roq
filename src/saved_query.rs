//! 保存查询（roq q）的功能层：查询名校验、占位符提取与单遍替换引擎为纯函数；
//! 另有执行链路失败路径共用的审计片段组装（[`saved_audit_extra`] / [`params_json_loose`]）；
//! 末尾的文件层负责 queries.d 目录下模板的写入、读取与列举（薄 I/O 封装，
//! 名字合法性与重名覆盖等校验属管理子命令流程）。
//!
//! [`validate_query_name`] 约束保存名可安全用作文件名与 CLI 参数：
//! 非空、仅 `[A-Za-z0-9_-]`、不以 `-` 开头（防被误认为 flag）、
//! 不得占用 [`RESERVED_QUERY_NAMES`] 中的子命令保留名。
//! [`extract_placeholder_names`] 扫描模板提取全部 `{{名}}` 占位符：
//! 名限 `[A-Za-z0-9_]+`，按首现顺序去重；`{{` 未闭合、名为空或含非法字符
//! 一律报错并附位置描述（fail-closed）。
//! [`substitute`] 做 `{{名}}` 宏替换：单遍扫描，值只拼接不回扫，
//! 产物残留 `{{` 一律报错（宏注入防线）。

use crate::config::{default_config_base, write_file_atomic};
use std::fs;
use std::path::{Path, PathBuf};

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

/// 执行保存查询时透传给审计与存档的上下文。
///
/// `params_json` 形如 `{"id":"123"}`（k 按模板序，值经 [`crate::audit::json_escape`]），
/// 无参数为 `{}`。
pub struct SavedContext {
    pub name: String,
    pub params_json: String,
}

/// 单遍扫描替换模板中的 `{{名}}` 占位符（宏替换，非绑定参数）。
///
/// 安全要点：
/// - **单遍扫描**：只顺序扫描模板本身，替换值原样拼接进产物、绝不回扫——
///   值中的 `{{b}}` 不会在后续轮次被展开（宏注入防线）；
/// - **残留断言**：扫描结束后产物若仍含 `{{` 一律报错——残留占位符落在引号内
///   是合法字符串字面量，查询会静默返回空结果，宁可显式失败；
/// - 名段校验与 [`extract_placeholder_names`] 同口径（`[A-Za-z0-9_]+`），
///   模板坏形（未闭合/名含非法字符/空名）与占位符查无此名均报错。
pub fn substitute(template: &str, params: &[(String, String)]) -> Result<String, String> {
    let chars: Vec<char> = template.chars().collect();
    let mut out = String::with_capacity(template.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '{' || i + 1 >= chars.len() || chars[i + 1] != '{' {
            out.push(chars[i]);
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
            let Some((_, value)) = params.iter().find(|(k, _)| *k == name) else {
                return Err(format!("占位符 {{{{{name}}}}} 没有对应的参数值"));
            };
            out.push_str(value);
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
    if out.contains("{{") {
        return Err("替换产物仍残留 '{{'：参数值不得包含占位符语法，请改写参数值或模板".to_string());
    }
    Ok(out)
}

/// 以哑参值 `"1"` 替换模板全部占位符（保存预检用：哑参产物必须能过只读闸门）。
pub fn dummy_substitute(template: &str) -> Result<String, String> {
    let params: Vec<(String, String)> = extract_placeholder_names(template)?
        .into_iter()
        .map(|name| (name, "1".to_string()))
        .collect();
    substitute(template, &params)
}

/// 校验参数值：含单引号或反斜杠一律拒绝——直接替换会改变 SQL 语义
/// （提前截断字符串字面量/触发转义序列），显式报错优于静默语义改变。
pub fn validate_param_value(name: &str, value: &str) -> Result<(), String> {
    if value.contains('\'') || value.contains('\\') {
        return Err(format!(
            "参数 {name} 的值含单引号或反斜杠，直接替换会改变 SQL 语义；如需在查询中表示此类字符，请改写模板并在其中转义：单引号写作 ''，反斜杠写作 \\\\"
        ));
    }
    Ok(())
}

/// 解析单个 `k=v` 参数：在首个 `=` 处切分（值可含 `=`），键与值均不得为空。
pub fn parse_param_arg(arg: &str) -> Result<(String, String), String> {
    let Some((key, value)) = arg.split_once('=') else {
        return Err(format!("参数 {arg} 缺少 '='，应为 k=v 形式"));
    };
    if key.is_empty() || value.is_empty() {
        return Err(format!("参数 {arg} 的键与值均不能为空，应为 k=v 形式"));
    }
    Ok((key.to_string(), value.to_string()))
}

/// 把调用方参数对齐到模板占位符序。
///
/// 重复键、缺参（列出所需名）、多参（列出多余名）一律报错；
/// 成功时返回按模板序排列的 (名, 值) 列表（[`SavedContext`] 的 params_json 键序来源）。
pub fn match_params(
    names: &[String],
    params: &[(String, String)],
) -> Result<Vec<(String, String)>, String> {
    for (idx, (key, _)) in params.iter().enumerate() {
        if params[..idx].iter().any(|(k, _)| k == key) {
            return Err(format!("参数 {key} 重复给出"));
        }
    }
    let missing: Vec<String> = names
        .iter()
        .filter(|name| !params.iter().any(|(k, _)| k == *name))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "缺少参数：{}（该查询需要：{}）",
            missing.join("、"),
            names.join("、")
        ));
    }
    let extra: Vec<String> = params
        .iter()
        .map(|(k, _)| k.clone())
        .filter(|k| !names.contains(k))
        .collect();
    if !extra.is_empty() {
        return Err(format!(
            "多余参数：{}（该查询需要：{}）",
            extra.join("、"),
            names.join("、")
        ));
    }
    Ok(names
        .iter()
        .filter_map(|name| {
            params
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| (name.clone(), v.clone()))
        })
        .collect())
}

/// 把按模板序排列的参数渲染为紧凑 JSON 对象文本：`{"id":"123"}`，无参数为 `{}`。
///
/// 占位符名字符集受限无需转义；值经 [`crate::audit::json_escape`]。
fn params_json_text(params: &[(String, String)]) -> String {
    let pairs: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("\"{k}\":\"{}\"", crate::audit::json_escape(v)))
        .collect();
    format!("{{{}}}", pairs.join(","))
}

/// 执行前编排：模板占位符校验 → 逐个解析 `k=v` → 对齐模板序 → 逐个校验值 → 单遍替换。
///
/// 返回 `(替换后 SQL, params_json)`；任一步失败即整体失败（fail-closed）。
pub fn prepare_execution(
    template: &str,
    param_args: &[String],
) -> Result<(String, String), String> {
    let names = extract_placeholder_names(template)?;
    let mut params = Vec::with_capacity(param_args.len());
    for arg in param_args {
        params.push(parse_param_arg(arg)?);
    }
    let ordered = match_params(&names, &params)?;
    for (name, value) in &ordered {
        validate_param_value(name, value)?;
    }
    let sql = substitute(template, &ordered)?;
    Ok((sql, params_json_text(&ordered)))
}

/// 失败路径审计用：宽松渲染调用方原始 `k=v` 参数为 params_json 形态（`{"k":"v"}`）。
///
/// [`prepare_execution`] 失败时拿不到对齐后的规范参数，此函数尽力保留可解析的
/// 参数（含重复——重参本身就是要审计的场景），坏形参数（缺 `=`、空键值）跳过；
/// 仅用于审计 extra，不是执行数据路径。
pub fn params_json_loose(param_args: &[String]) -> String {
    let pairs: Vec<(String, String)> = param_args
        .iter()
        .filter_map(|arg| parse_param_arg(arg).ok())
        .collect();
    params_json_text(&pairs)
}

/// 组装 saved 查询的审计 extra 片段：`,"saved":"名","params":{...}` + 可选尾部 `,"reason":"..."`。
///
/// 形态契约：无参数（params_json 为 `{}`）省略 params 键；reason 恒居 extra 尾部。
/// 名与 reason 经 [`crate::audit::json_escape`]；params_json 由 [`prepare_execution`]
/// 生成、值已转义，原样嵌入。
pub fn saved_audit_extra(name: &str, params_json: &str, reason: Option<&str>) -> String {
    let mut extra = format!(",\"saved\":\"{}\"", crate::audit::json_escape(name));
    if params_json != "{}" {
        extra.push_str(&format!(",\"params\":{params_json}"));
    }
    if let Some(reason) = reason {
        extra.push_str(&format!(
            ",\"reason\":\"{}\"",
            crate::audit::json_escape(reason)
        ));
    }
    extra
}

// —— queries.d 文件层：模板的写入、读取与列举（薄 I/O 封装）——

/// 摘要最大字符数（按 Unicode 字符计，非字节）：列表展示用，超长截断补省略号。
const TEMPLATE_SUMMARY_MAX_CHARS: usize = 48;
/// 摘要截断后追加的省略号。
const TEMPLATE_SUMMARY_ELLIPSIS: &str = "…";
/// 模板无有效摘要行（空文件/纯注释）时展示的占位文本。
const TEMPLATE_SUMMARY_EMPTY: &str = "(空)";

/// 保存查询文件头注释：写入时置顶，向手工编辑者说明格式与占位符语法；
/// 执行时作为 SQL 行注释对语义无害。
pub const QUERY_FILE_HEADER: &str =
    "-- roq 保存查询（可手工编辑，修改即时生效；{{名}} 为占位符）\n";

/// 保存查询目录：`~/.roq/queries.d`，一个查询一个 `.sql` 文件（文件名 stem 即查询名）。
pub fn queries_dir() -> Result<PathBuf, String> {
    Ok(default_config_base()?.join("queries.d"))
}

/// 保存查询文件路径：`<base>/<name>.sql`。
fn query_file_path(base: &Path, name: &str) -> PathBuf {
    base.join(format!("{name}.sql"))
}

/// 读取保存查询模板原文（含文件头注释；`--` 行注释对 SQL 语义无害）。
///
/// 查询不存在报错并指引 `roq q list`——与列举的"目录不存在返回空"相反：
/// 读取是点名行为，查无此名就是错误。
pub fn load_template_in(base: &Path, name: &str) -> Result<String, String> {
    let path = query_file_path(base, name);
    fs::read_to_string(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("保存查询 {name} 不存在：运行 roq q list 查看已保存的查询")
        } else {
            format!("读取保存查询 {} 失败：{e}", path.display())
        }
    })
}

/// 从默认目录读取保存查询模板（生产入口；测试用 [`load_template_in`] 注入目录）。
pub fn load_template(name: &str) -> Result<String, String> {
    load_template_in(&queries_dir()?, name)
}

/// 列举全部保存查询：读 `<base>/*.sql`，返回 `(查询名, 摘要)` 按名字典序。
/// 目录不存在返回空列表——空态是正常态（尚未保存过任何查询）。
pub fn list_queries_in(base: &Path) -> Result<Vec<(String, String)>, String> {
    if !base.exists() {
        return Ok(Vec::new());
    }
    let mut queries = Vec::new();
    for entry in fs::read_dir(base).map_err(|e| format!("读取目录 {} 失败：{e}", base.display()))?
    {
        let entry = entry.map_err(|e| format!("读取目录 {} 失败：{e}", base.display()))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sql") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let content = fs::read_to_string(&path)
            .map_err(|e| format!("读取保存查询 {} 失败：{e}", path.display()))?;
        queries.push((name.to_string(), template_summary(&content)));
    }
    queries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(queries)
}

/// 写入保存查询模板：自动建目录、原子写（临时文件改名），内容 = [`QUERY_FILE_HEADER`] + 模板，
/// 返回写入路径。本层不做任何校验——名字合法性、重名覆盖防护属 add 流程（管理子命令）。
pub fn write_template_in(base: &Path, name: &str, template: &str) -> Result<PathBuf, String> {
    fs::create_dir_all(base).map_err(|e| format!("创建目录 {} 失败：{e}", base.display()))?;
    let path = query_file_path(base, name);
    write_file_atomic(&path, &format!("{QUERY_FILE_HEADER}{template}"))?;
    Ok(path)
}

/// 从模板原文提取摘要行：首个 trim 后非空且不以 `--`/`#` 开头的行，
/// 超过 [`TEMPLATE_SUMMARY_MAX_CHARS`] 字符截断补省略号；无有效行返回 [`TEMPLATE_SUMMARY_EMPTY`]。
pub fn template_summary(content: &str) -> String {
    let Some(line) = content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("--") && !line.starts_with('#'))
    else {
        return TEMPLATE_SUMMARY_EMPTY.to_string();
    };
    if line.chars().count() <= TEMPLATE_SUMMARY_MAX_CHARS {
        return line.to_string();
    }
    let truncated: String = line.chars().take(TEMPLATE_SUMMARY_MAX_CHARS).collect();
    format!("{truncated}{TEMPLATE_SUMMARY_ELLIPSIS}")
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

    // —— 单遍替换引擎与参数解析（安全核心）——

    #[test]
    fn substitute_replaces_by_name_and_allows_reuse() {
        let sql = substitute(
            "SELECT * FROM t WHERE id='{{id}}' OR ref='{{id}}'",
            &[("id".to_string(), "5".to_string())],
        )
        .expect("同名占位符应全部替换");
        assert_eq!(sql, "SELECT * FROM t WHERE id='5' OR ref='5'");
    }

    #[test]
    fn substitute_rejects_placeholder_syntax_in_value() {
        let err = substitute(
            "SELECT * FROM t WHERE id='{{id}}'",
            &[("id".to_string(), "{{b}}".to_string())],
        )
        .expect_err("值含占位符语法应整体报错");
        assert!(err.contains("{{"), "报错应说明产物残留占位符：{err}");
        // 即使被注入的占位符名恰好也有参数，也绝不展开（单遍扫描不回扫产物）。
        let err = substitute(
            "SELECT * FROM t WHERE id='{{id}}'",
            &[
                ("id".to_string(), "{{b}}".to_string()),
                ("b".to_string(), "999".to_string()),
            ],
        )
        .expect_err("值注入的占位符即使有参数也不得展开");
        assert!(err.contains("{{"), "报错应说明产物残留占位符：{err}");
    }

    #[test]
    fn substitute_reports_unknown_placeholder() {
        let err = substitute("SELECT * FROM t WHERE id='{{a}}'", &[])
            .expect_err("未提供参数的占位符应报错");
        assert!(err.contains("{{a}}"), "报错应含占位符原文：{err}");
    }

    #[test]
    fn dummy_substitute_yields_gate_clean_sql() {
        let dummy =
            dummy_substitute("SELECT * FROM t WHERE id='{{id}}' AND n={{n}}").expect("哑参替换应成功");
        assert_eq!(dummy, "SELECT * FROM t WHERE id='1' AND n=1");
        crate::gate::gate(&dummy).expect("哑参替换后的合法模板应通过只读闸门");
    }

    #[test]
    fn validate_param_value_rejects_quote_and_backslash() {
        let err = validate_param_value("k", "O'Brien").expect_err("含单引号的值应报错");
        assert!(err.contains("''"), "报错应提示单引号写作 ''：{err}");
        let err = validate_param_value("k", "a\\b").expect_err("含反斜杠的值应报错");
        assert!(err.contains("\\\\"), "报错应提示反斜杠写作 \\\\：{err}");
        assert_eq!(validate_param_value("k", "a=b"), Ok(()));
        assert_eq!(validate_param_value("k", "a b"), Ok(()));
    }

    #[test]
    fn parse_param_arg_splits_on_first_eq() {
        assert_eq!(
            parse_param_arg("id=a=b"),
            Ok(("id".to_string(), "a=b".to_string()))
        );
        for bad in ["noid", "id=", "=v"] {
            assert!(parse_param_arg(bad).is_err(), "参数 {bad} 应报错");
        }
    }

    #[test]
    fn match_params_reports_missing_names() {
        let err = match_params(
            &["a".to_string(), "b".to_string()],
            &[("a".to_string(), "1".to_string())],
        )
        .expect_err("缺参应报错");
        assert!(err.contains("b"), "报错应列出缺失的参数名：{err}");
    }

    #[test]
    fn match_params_reports_extra_and_duplicate() {
        let names = ["a".to_string()];
        let extra = match_params(
            &names,
            &[
                ("a".to_string(), "1".to_string()),
                ("x".to_string(), "9".to_string()),
            ],
        )
        .expect_err("多余参数应报错");
        assert!(extra.contains("x"), "报错应列出多余的参数名：{extra}");
        let dup = match_params(
            &names,
            &[
                ("a".to_string(), "1".to_string()),
                ("a".to_string(), "2".to_string()),
            ],
        )
        .expect_err("重复参数应报错");
        assert!(dup.contains("a"), "报错应含重复的参数名：{dup}");
    }

    #[test]
    fn match_params_reorders_to_template_order() {
        let ordered = match_params(
            &["a".to_string(), "b".to_string()],
            &[
                ("b".to_string(), "2".to_string()),
                ("a".to_string(), "1".to_string()),
            ],
        )
        .expect("参数齐全应成功");
        assert_eq!(
            ordered,
            [
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string())
            ]
        );
    }

    #[test]
    fn prepare_execution_end_to_end() {
        let (sql, params_json) =
            prepare_execution("SELECT * FROM t WHERE id='{{id}}'", &["id=5".to_string()])
                .expect("编排应成功");
        assert_eq!(sql, "SELECT * FROM t WHERE id='5'");
        assert_eq!(params_json, "{\"id\":\"5\"}");
    }

    // —— 审计片段组装（执行链路失败路径共用）——

    #[test]
    fn saved_audit_extra_shapes_json() {
        // 形态一：含 params、无 reason
        assert_eq!(
            saved_audit_extra("q-by-id", "{\"id\":\"5\"}", None),
            ",\"saved\":\"q-by-id\",\"params\":{\"id\":\"5\"}"
        );
        // 形态二：无参数（params_json 为 "{}"）省略 params 键
        assert_eq!(saved_audit_extra("plain", "{}", None), ",\"saved\":\"plain\"");
        // 形态三：含 reason——reason 恒居 extra 尾部
        assert_eq!(
            saved_audit_extra("q-by-id", "{\"id\":\"5\"}", Some("缺少参数 b")),
            ",\"saved\":\"q-by-id\",\"params\":{\"id\":\"5\"},\"reason\":\"缺少参数 b\""
        );
        // 引号值转义：reason 内的引号不得破坏 JSON 结构
        assert_eq!(
            saved_audit_extra("q", "{}", Some("值含 \"引号\"")),
            ",\"saved\":\"q\",\"reason\":\"值含 \\\"引号\\\"\""
        );
    }

    #[test]
    fn params_json_loose_keeps_parseable_args() {
        // 失败路径审计的宽松渲染：可解析的 k=v 原样保留（含重复——重参本身是要审计的场景），
        // 坏形参数跳过；全不可解析回落 "{}"
        assert_eq!(
            params_json_loose(&["id=5".to_string(), "bad".to_string(), "id=6".to_string()]),
            "{\"id\":\"5\",\"id\":\"6\"}"
        );
        assert_eq!(params_json_loose(&["=v".to_string()]), "{}");
        assert_eq!(params_json_loose(&[]), "{}");
    }

    // —— queries.d 文件层：模板的写入、读取与列举 ——

    fn temp_qdir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("roq-q-{tag}-{}", std::process::id()))
    }

    #[test]
    fn write_then_load_roundtrips_with_header() {
        let base = temp_qdir("roundtrip");
        let _ = fs::remove_dir_all(&base);
        let template = "SELECT * FROM t WHERE id='{{id}}'";
        let path = write_template_in(&base, "by-id", template).expect("写入保存查询应成功");
        assert_eq!(path, base.join("by-id.sql"));
        let text = load_template_in(&base, "by-id").expect("回读保存查询应成功");
        assert_eq!(text, format!("{QUERY_FILE_HEADER}{template}"));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn load_missing_query_errors_with_list_hint() {
        let base = temp_qdir("missing");
        let _ = fs::remove_dir_all(&base);
        let err = load_template_in(&base, "no-such").expect_err("查询不存在应报错");
        assert!(
            err.contains("roq q list"),
            "报错应含 roq q list 指引：{err}"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn list_queries_sorted_and_summarized() {
        let base = temp_qdir("list");
        let _ = fs::remove_dir_all(&base);
        // 目录不存在是正常空态（尚未保存过任何查询）
        assert_eq!(
            list_queries_in(&base).expect("目录不存在应返回空列表"),
            vec![]
        );
        fs::create_dir_all(&base).expect("创建临时目录应成功");
        assert_eq!(list_queries_in(&base).expect("空目录应返回空列表"), vec![]);
        write_template_in(&base, "beta", "-- 模板自带注释\nSELECT 2").expect("写入 beta 应成功");
        write_template_in(&base, "alpha", "SELECT 1").expect("写入 alpha 应成功");
        // 非 .sql 文件不入列
        fs::write(base.join("notes.txt"), "SELECT 9").expect("写入 notes.txt 应成功");
        assert_eq!(
            list_queries_in(&base).expect("列举应成功"),
            vec![
                ("alpha".to_string(), "SELECT 1".to_string()),
                ("beta".to_string(), "SELECT 2".to_string()),
            ]
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn template_summary_skips_comments_and_truncates() {
        // 摘要取首个 trim 后非空且不以 --/# 开头的行
        assert_eq!(template_summary("  \n-- 头注释\nSELECT 1"), "SELECT 1");
        assert_eq!(
            template_summary("# shell 风格注释\n  SELECT 2  "),
            "SELECT 2"
        );
        assert_eq!(template_summary("-- 纯注释\n# 也是注释"), "(空)");
        assert_eq!(template_summary(""), "(空)");
        // 恰 48 字符不补省略号；超限按字符截断补省略号
        let full48: String = "x".repeat(48);
        assert_eq!(template_summary(&full48), full48);
        let long: String = "x".repeat(60);
        assert_eq!(template_summary(&long), format!("{}…", "x".repeat(48)));
        let zh: String = "查".repeat(60);
        assert_eq!(template_summary(&zh), format!("{}…", "查".repeat(48)));
    }
}
