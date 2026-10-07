//! `roq log` 子命令：查看/过滤审计日志（~/.roq/logs/roq-YYYYMM.jsonl）。
//!
//! 过滤基于 serde_json 解析后的字段值而非子串匹配——SQL 内容可能含
//! `"profile":"x"` 字样，子串匹配会误伤。

use std::fs;
use std::path::Path;
use std::process::ExitCode;

use crate::cli::LogArgs;
use crate::config::default_config_base;

/// 过滤条件。day_prefix 为 Some(如 "2026-10-07") 时只留 ts 以该日期开头的行。
pub struct LogFilter {
    pub profile: Option<String>,
    pub outcome: Option<String>,
    pub day_prefix: Option<String>,
}

/// 判断一行审计日志是否匹配过滤器；无法解析的行（手写/损坏）一律跳过。
pub fn line_matches(line: &str, filter: &LogFilter) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    if let Some(profile) = &filter.profile {
        if value.get("profile").and_then(|v| v.as_str()) != Some(profile.as_str()) {
            return false;
        }
    }
    if let Some(outcome) = &filter.outcome {
        if value.get("outcome").and_then(|v| v.as_str()) != Some(outcome.as_str()) {
            return false;
        }
    }
    if let Some(day) = &filter.day_prefix {
        let ts = value.get("ts").and_then(|v| v.as_str()).unwrap_or("");
        if !ts.starts_with(day.as_str()) {
            return false;
        }
    }
    true
}

/// 读取日志文件全部行；Some(n) 时仅保留末尾 n 条（最近优先场景）。
pub fn read_tail_lines(path: &Path, last: Option<usize>) -> Vec<String> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    if let Some(n) = last {
        let skip = lines.len().saturating_sub(n);
        lines.drain(..skip);
    }
    lines
}

/// 人类可读的一行摘要：时分秒 profile db outcome 行数 耗时 SQL(截 60 字符)。
pub fn format_line(value: &serde_json::Value) -> String {
    let ts = value.get("ts").and_then(|v| v.as_str()).unwrap_or("");
    let time = ts.get(11..19).unwrap_or(ts);
    let profile = value.get("profile").and_then(|v| v.as_str()).unwrap_or("?");
    let db = value.get("db").and_then(|v| v.as_str()).unwrap_or("?");
    let outcome = value.get("outcome").and_then(|v| v.as_str()).unwrap_or("?");
    let rows = value.get("rows").and_then(|v| v.as_i64()).unwrap_or(0);
    let ms = value.get("ms").and_then(|v| v.as_i64()).unwrap_or(0);
    let sql = value.get("sql").and_then(|v| v.as_str()).unwrap_or("");
    let sql_short: String = sql.chars().take(60).collect();
    format!(
        "{:<8} {:<10} {:<14} {:<13} {:>4}行 {:>6}ms  {}",
        time, profile, db, outcome, rows, ms, sql_short
    )
}

/// 入口：定位本月日志文件 → 过滤 → 输出（--json 原样行，否则人类可读摘要）。
pub fn run(args: LogArgs) -> ExitCode {
    let month = chrono::Local::now().format("%Y%m").to_string();
    let file = match default_config_base() {
        Ok(base) => base.join("logs").join(format!("roq-{}.jsonl", month)),
        Err(reason) => {
            eprintln!("[roq log] {}", reason);
            return ExitCode::from(1);
        }
    };
    if !file.exists() {
        println!("本月（{}）暂无审计日志。", file.display());
        return ExitCode::from(0);
    }
    // 默认只看今天；--month 看整月，--last N 取最近 N 条（不限日期）。
    let day_prefix = if args.month || args.last.is_some() {
        None
    } else {
        Some(chrono::Local::now().format("%Y-%m-%d").to_string())
    };
    let filter = LogFilter {
        profile: args.profile.clone(),
        outcome: args.outcome.clone(),
        day_prefix,
    };
    let mut shown = 0usize;
    for line in &read_tail_lines(&file, args.last) {
        if !line_matches(line, &filter) {
            continue;
        }
        if args.json {
            println!("{}", line);
        } else if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
            println!("{}", format_line(&value));
        }
        shown += 1;
    }
    if shown == 0 {
        println!(
            "无匹配记录（范围：{}）",
            if args.month {
                "本月"
            } else if args.last.is_some() {
                "最近 N 条"
            } else {
                "今天"
            }
        );
    }
    ExitCode::from(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE_DEV_OK: &str =
        "{\"ts\":\"2026-10-07T14:32:01.123+08:00\",\"profile\":\"dev\",\"db\":\"mydb\",\"sql\":\"SELECT 1\",\"rows\":3,\"truncated\":false,\"ms\":45,\"outcome\":\"ok\",\"result\":\"x.tsv\"}";
    const LINE_PROD_REJECTED: &str =
        "{\"ts\":\"2026-10-06T09:00:00.000+08:00\",\"profile\":\"prod\",\"db\":\"pdb\",\"sql\":\"DELETE FROM t\",\"rows\":0,\"truncated\":false,\"ms\":1,\"outcome\":\"gate-rejected\",\"reason\":\"\",\"result\":\"\"}";
    // 注入陷阱：SQL 文本里含 "profile":"dev" 字样,但实际 profile 是 prod
    const LINE_SMIUGGLED: &str =
        "{\"ts\":\"2026-10-07T15:00:00.000+08:00\",\"profile\":\"prod\",\"db\":\"pdb\",\"sql\":\"SELECT '{\\\"profile\\\":\\\"dev\\\"}' AS s\",\"rows\":1,\"truncated\":false,\"ms\":5,\"outcome\":\"ok\",\"result\":\"\"}";

    #[test]
    fn line_matches_filters_by_profile_and_outcome() {
        let dev_ok = LogFilter {
            profile: Some("dev".to_string()),
            outcome: Some("ok".to_string()),
            day_prefix: None,
        };
        assert!(line_matches(LINE_DEV_OK, &dev_ok));
        assert!(!line_matches(LINE_PROD_REJECTED, &dev_ok));
    }

    #[test]
    fn line_matches_ignores_substring_inside_sql() {
        let trap = LogFilter {
            profile: Some("dev".to_string()),
            outcome: None,
            day_prefix: None,
        };
        assert!(
            !line_matches(LINE_SMIUGGLED, &trap),
            "SQL 内的 profile 字样不得误匹配"
        );
    }

    #[test]
    fn line_matches_skips_invalid_json() {
        let any = LogFilter {
            profile: None,
            outcome: None,
            day_prefix: None,
        };
        assert!(!line_matches("not json at all", &any));
        assert!(!line_matches("", &any));
    }

    #[test]
    fn line_matches_filters_by_day_prefix() {
        let today = LogFilter {
            profile: None,
            outcome: None,
            day_prefix: Some("2026-10-07".to_string()),
        };
        assert!(line_matches(LINE_DEV_OK, &today));
        assert!(
            !line_matches(LINE_PROD_REJECTED, &today),
            "昨天(10-06)的行应被滤掉"
        );
    }

    #[test]
    fn format_line_keeps_columns_compact() {
        let v: serde_json::Value = serde_json::from_str(LINE_DEV_OK).unwrap();
        let text = format_line(&v);
        assert!(text.contains("14:32:01"), "应显示时分秒：{}", text);
        assert!(text.contains("dev"));
        assert!(text.contains("ok"));
        assert!(text.contains("SELECT 1"));
    }

    #[test]
    fn read_tail_lines_returns_last_n() {
        let path =
            std::env::temp_dir().join(format!("roq-log-{}-{}.jsonl", "tail", std::process::id()));
        let body: String = (1..=5)
            .map(|i| format!("{{\"i\":{}}}", i))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, format!("{}\n", body)).unwrap();
        let tail = read_tail_lines(&path, Some(3));
        assert_eq!(tail.len(), 3);
        assert!(tail[0].contains("\"i\":3"), "应从第 3 条开始：{}", tail[0]);
        assert!(tail[2].contains("\"i\":5"));
        let all = read_tail_lines(&path, None);
        assert_eq!(all.len(), 5);
        let _ = std::fs::remove_file(&path);
    }
}
