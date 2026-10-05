//! 审计日志（JSONL，按月分文件）与查询结果存档（TSV，项目目录优先）。

use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use crate::config::{default_config_base, home_dir};
use crate::render::table_slug;

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

/// 查询结果全文存档：<当前项目>\.roq\results\YYYYMMDD\HHMMSS-毫秒-<表名>-<profile>.tsv
/// 文件头为 "# " 元信息块（时间/库/行数上限/SQL），TSV 体从首个非 # 行开始；
/// 单元格不截断（终端展示才截断）。当前目录是家目录或不可写时退回 ~/.roq/results/。
/// 返回路径；失败返回 None（仅影响存档，不影响查询结果输出）。
pub fn save_result(
    profile: &str,
    sql: &str,
    db: &str,
    max_rows: usize,
    lines: &[String],
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
        let slug = table_slug(sql).unwrap_or_else(|| "query".to_string());
        let path = dir.join(format!("{}-{}-{}.tsv", stamp, slug, profile));
        let mut body = String::new();
        body.push_str(&format!("# 时间: {}\n", now_local()));
        body.push_str(&format!("# 库: {}（profile={}）\n", db, profile));
        body.push_str(&format!("# 行数上限: {}\n", max_rows));
        body.push_str(&format!("# SQL: {}\n", sql.replace('\n', " ")));
        body.push_str(&lines.join("\n"));
        if fs::write(&path, format!("{}\n", body)).is_ok() {
            return Some(path.display().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::json_escape;

    #[test]
    fn json_escape_covers_controls_and_quotes() {
        assert_eq!(json_escape("a\"b\\c"), "a\\\"b\\\\c");
        assert_eq!(json_escape("\n\t\r"), "\\n\\t\\r");
        assert_eq!(json_escape("\u{1}"), "\\u0001");
        assert_eq!(json_escape("中文保持原样"), "中文保持原样");
    }
}
