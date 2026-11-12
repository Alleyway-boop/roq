//! 审计日志（JSONL，按月分文件）与查询结果存档（TSV，项目目录优先）。

use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use crate::config::{default_config_base, home_dir};
use crate::render::{like_slug, table_slug};
use crate::saved_query::SavedContext;

/// 本地时间戳（ISO 8601 含毫秒与时区）。
fn now_local() -> String {
    chrono::Local::now()
        .format("%Y-%m-%dT%H:%M:%S%.3f%:z")
        .to_string()
}

/// JSON 字符串转义（控制字符、引号、反斜杠；非 ASCII 原样保留 UTF-8）。
pub fn json_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    for c in raw.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// 组装一条审计日志（JSONL 一行）。extra 形如 `,"reason":"..."`，可为空。
pub fn audit_line(
    profile: &str,
    db: &str,
    sql: &str,
    rows: usize,
    truncated: bool,
    ms: u128,
    outcome: &str,
    extra: &str,
    result_file: &str,
) -> String {
    format!(
        "{{\"ts\":\"{}\",\"profile\":\"{}\",\"db\":\"{}\",\"sql\":\"{}\",\"rows\":{},\"truncated\":{},\"ms\":{},\"outcome\":\"{}\"{},\"result\":\"{}\"}}",
        now_local(),
        json_escape(profile),
        json_escape(db),
        json_escape(sql),
        rows,
        truncated,
        ms,
        outcome,
        extra,
        json_escape(result_file)
    )
}

/// 追加审计日志到 ~/.roq/logs/roq-YYYYMM.jsonl（按月分文件）。
/// 尽力而为：写入失败仅告警，绝不影响查询本身。
pub fn append_audit_log(entry: &str) {
    let dir = match default_config_base() {
        Ok(base) => base.join("logs"),
        Err(_) => return,
    };
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("[roq] 警告：审计日志目录创建失败（{}）", e);
        return;
    }
    let file = dir.join(chrono::Local::now().format("roq-%Y%m.jsonl").to_string());
    let mut f = match fs::OpenOptions::new().create(true).append(true).open(&file) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[roq] 警告：审计日志写入失败（{}）", e);
            return;
        }
    };
    if let Err(e) = f.write_all(format!("{}\n", entry).as_bytes()) {
        eprintln!("[roq] 警告：审计日志写入失败（{}）", e);
    }
}

/// 存档文件名 slug：saved 查询直接用保存名（存的是模板展开结果，表名提炼已失真），
/// 普通查询走既有链 table_slug → like_slug → "query"。
pub fn result_slug(sql: &str, saved: Option<&str>) -> String {
    if let Some(name) = saved {
        return name.to_string();
    }
    table_slug(sql)
        .or_else(|| like_slug(sql))
        .unwrap_or_else(|| "query".to_string())
}

/// 从 params_json（`{"k":"v",...}`，值已 json_escape）提取 `# 参数:` 行的 `k=v, k=v` 文本。
/// 键限占位符字符集无需转义；切分引号感知（值内的逗号/转义引号不误切），
/// 值保持 json 转义形态；无参数或形坏返回 None——调用方据此省略参数行。
fn saved_params_text(params_json: &str) -> Option<String> {
    let inner = params_json.strip_prefix('{')?.strip_suffix('}')?;
    let mut in_string = false;
    let mut escaped = false;
    let mut pairs: Vec<String> = Vec::new();
    let mut pair = String::new();
    for c in inner.chars() {
        if escaped {
            pair.push(c);
            escaped = false;
        } else {
            match c {
                '\\' if in_string => {
                    pair.push(c);
                    escaped = true;
                }
                '"' => {
                    in_string = !in_string;
                    pair.push(c);
                }
                ',' if !in_string => pairs.push(std::mem::take(&mut pair)),
                _ => pair.push(c),
            }
        }
    }
    if !pair.is_empty() {
        pairs.push(pair);
    }
    let rendered: Vec<String> = pairs
        .iter()
        .filter_map(|p| {
            let (key, value) = p.split_once(':')?;
            Some(format!(
                "{}={}",
                key.trim_matches('"'),
                value.trim_matches('"')
            ))
        })
        .collect();
    if rendered.is_empty() {
        return None;
    }
    Some(rendered.join(", "))
}

/// 查询结果全文存档：<当前项目>\.roq\results\YYYYMMDD\HHMMSS-毫秒-<slug>-<profile>.tsv
/// （slug：saved 保存名优先，普通查询提炼表名/LIKE 值，兜底 "query"，见 [`result_slug`]）
/// 文件头为 "# " 元信息块（时间/库/行数上限/SQL，saved 查询另附保存名与参数），
/// TSV 体从首个非 # 行开始；
/// 单元格不截断（终端展示才截断）。当前目录是家目录或不可写时退回 ~/.roq/results/。
/// 返回路径；失败返回 None（仅影响存档，不影响查询结果输出）。
pub fn save_result(
    profile: &str,
    sql: &str,
    db: &str,
    max_rows: usize,
    lines: &[String],
    saved: Option<&SavedContext>,
) -> Option<String> {
    let day = chrono::Local::now().format("%Y%m%d").to_string();
    let stamp = chrono::Local::now().format("%H%M%S-%3f").to_string();
    // 候选目录：项目目录优先，工具目录兜底。
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = env::current_dir() {
        let is_home = home_dir().map(|h| h == cwd).unwrap_or(false);
        if !is_home {
            candidates.push(cwd.join(".roq").join("results"));
        }
    }
    if let Ok(base) = default_config_base() {
        candidates.push(base.join("results"));
    }
    for root in candidates {
        let dir = root.join(&day);
        if fs::create_dir_all(&dir).is_err() {
            continue;
        }
        let slug = result_slug(sql, saved.map(|ctx| ctx.name.as_str()));
        let path = dir.join(format!("{}-{}-{}.tsv", stamp, slug, profile));
        let mut body = String::new();
        body.push_str(&format!("# 时间: {}\n", now_local()));
        body.push_str(&format!("# 库: {}（profile={}）\n", db, profile));
        body.push_str(&format!("# 行数上限: {}\n", max_rows));
        body.push_str(&format!("# SQL: {}\n", sql.replace('\n', " ")));
        if let Some(ctx) = saved {
            body.push_str(&format!("# 保存查询: {}\n", ctx.name));
            if let Some(params) = saved_params_text(&ctx.params_json) {
                body.push_str(&format!("# 参数: {params}\n"));
            }
        }
        body.push_str(&lines.join("\n"));
        if fs::write(&path, format!("{}\n", body)).is_ok() {
            return Some(path.display().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escape_covers_controls_and_quotes() {
        assert_eq!(json_escape("a\"b\\c"), "a\\\"b\\\\c");
        assert_eq!(json_escape("\n\t\r"), "\\n\\t\\r");
        assert_eq!(json_escape("\u{1}"), "\\u0001");
        assert_eq!(json_escape("中文保持原样"), "中文保持原样");
    }

    #[test]
    fn result_slug_prefers_saved_name() {
        assert_eq!(
            result_slug("SELECT * FROM t_user", Some("q-by-id")),
            "q-by-id",
            "saved 查询的存档名应直接用保存名，不做表名提炼"
        );
    }

    #[test]
    fn result_slug_falls_back_to_table_then_query() {
        assert_eq!(result_slug("SELECT * FROM t_user", None), "t_user");
        assert_eq!(result_slug("SELECT 1", None), "query");
    }

    #[test]
    fn saved_params_text_renders_k_v_pairs() {
        assert_eq!(saved_params_text("{\"id\":\"5\"}").as_deref(), Some("id=5"));
        // 值内逗号/转义引号不得切分参数对（引号感知扫描；值保持 json 转义形态）
        assert_eq!(
            saved_params_text("{\"id\":\"5\",\"note\":\"a,b\"}").as_deref(),
            Some("id=5, note=a,b")
        );
        assert_eq!(
            saved_params_text("{\"q\":\"a\\\"b\"}").as_deref(),
            Some("q=a\\\"b")
        );
        // 无参数（"{}"）与形坏输入返回 None——调用方据此省略参数行
        assert_eq!(saved_params_text("{}"), None);
        assert_eq!(saved_params_text("not-json"), None);
    }
}
