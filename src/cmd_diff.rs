//! `roq diff` 子命令：同一条 SQL 在两个 profile 上执行并比对——两库对账的只读安全通道。
//!
//! 比对维度：列名（有序）、行数、行多重集摘要（顺序无关、重复敏感）；
//! 任一侧截断时摘要仅覆盖已取前缀并在输出标注。两侧各写一条审计（与手写查询同一底账），
//! 退出码 4 表示比对不一致，其余沿用查询契约（1 用法配置 / 2 闸门 / 3 连接执行）。

use std::process::ExitCode;
use std::time::Instant;

use mysql::prelude::Queryable;

use crate::audit::{append_audit_log, audit_line, json_escape};
use crate::cli::DiffArgs;
use crate::config::{load_config_sources, Profile};
use crate::gate::gate;
use crate::query::connect_readonly;
use crate::render::{raw_cell, tsv_cell, CellValue};

/// 比对不一致的专用退出码。
const EXIT_DIFF_MISMATCH: u8 = 4;

/// 单侧执行结果（已按 max_rows 截断收集，digest 为行多重集摘要）。
struct SideResult {
    name: String,
    database: String,
    columns: Vec<String>,
    rows: Vec<Vec<CellValue>>,
    truncated: bool,
    elapsed_ms: u128,
    digest: u64,
}

/// 比对结论：三个维度独立判定，全真才一致。
struct DiffReport {
    columns_equal: bool,
    rows_equal: bool,
    digest_equal: bool,
}

impl DiffReport {
    fn is_equal(&self) -> bool {
        self.columns_equal && self.rows_equal && self.digest_equal
    }
}

pub fn run(args: DiffArgs) -> ExitCode {
    let [left_name, right_name]: [String; 2] =
        args.profiles.try_into().expect("解析层已保证恰好两个 profile");
    let raw_sql = args.sql.unwrap_or_default();
    // 闸门一次（同一 SQL 两侧共用），拒绝即整体拒绝并留痕
    let sql = match gate(&raw_sql) {
        Ok(sql) => sql,
        Err(reason) => {
            eprintln!("[roq 闸门拒绝] {}", reason);
            append_audit_log(&audit_line(
                &left_name,
                "",
                &raw_sql,
                0,
                false,
                0,
                "gate-rejected",
                &format!(",\"reason\":\"{}\"", json_escape(&reason)),
                "",
            ));
            return ExitCode::from(2);
        }
    };
    let profiles = match load_config_sources(None) {
        Ok(profiles) => profiles,
        Err(msg) => {
            eprintln!("[roq diff] {}", msg);
            return ExitCode::from(1);
        }
    };
    let (left_profile, right_profile) = match (profiles.get(&left_name), profiles.get(&right_name))
    {
        (Some(left), Some(right)) => (left, right),
        (missing_side, _) => {
            let missing = if missing_side.is_none() {
                &left_name
            } else {
                &right_name
            };
            let reason = format!("profile-missing: {}", missing);
            eprintln!("[roq diff] 配置 [{}] 不存在。--list 查看全部。", missing);
            append_audit_log(&audit_line(
                &left_name, "", &sql, 0, false, 0, "config-error", &reason, "",
            ));
            return ExitCode::from(1);
        }
    };
    // 逐侧执行并各记一条审计；任一侧失败即中止（对账无意义）
    let mut sides: Vec<SideResult> = Vec::new();
    for (name, profile) in [(&left_name, left_profile), (&right_name, right_profile)] {
        match fetch_side(profile, name, &sql, args.max_rows) {
            Ok(side) => {
                append_audit_log(&audit_line(
                    name,
                    &side.database,
                    &sql,
                    side.rows.len(),
                    side.truncated,
                    side.elapsed_ms,
                    "ok",
                    ",\"reason\":\"diff-side\"",
                    "",
                ));
                sides.push(side);
            }
            Err((outcome, reason)) => {
                eprintln!("[roq diff] {} 侧执行失败：{}", name, reason);
                append_audit_log(&audit_line(
                    name,
                    &profile.database,
                    &sql,
                    0,
                    false,
                    0,
                    &outcome,
                    &format!(",\"reason\":\"{}\"", json_escape(&reason)),
                    "",
                ));
                return ExitCode::from(if outcome == "config-error" { 1 } else { 3 });
            }
        }
    }
    let report = compare(&sides[0], &sides[1]);
    let text = if args.json {
        render_diff_json(&sides[0], &sides[1], &report)
    } else {
        render_diff_human(&sides[0], &sides[1], &report)
    };
    println!("{}", text);
    if report.is_equal() {
        ExitCode::from(0)
    } else {
        ExitCode::from(EXIT_DIFF_MISMATCH)
    }
}

/// 单侧执行：建连 + 会话只读 + 限量收集（闸门已由调用方完成）。
fn fetch_side(
    profile: &Profile,
    name: &str,
    sql: &str,
    max_rows: usize,
) -> Result<SideResult, (String, String)> {
    let start = Instant::now();
    let password = profile
        .resolved_password()
        .map_err(|e| ("config-error".to_string(), e))?;
    let mut conn =
        connect_readonly(profile, &password, &|msg| eprintln!("{}", msg))
            .map_err(|e| ("connect-error".to_string(), e))?;
    let mut result = conn
        .query_iter(sql)
        .map_err(|e| ("exec-error".to_string(), e.to_string()))?;
    let columns: Vec<String> = result
        .columns()
        .as_ref()
        .iter()
        .map(|col| col.name_str().to_string())
        .collect();
    let mut rows: Vec<Vec<CellValue>> = Vec::new();
    while rows.len() < max_rows {
        let Some(row) = result.next() else { break };
        let row = row.map_err(|e| ("exec-error".to_string(), format!("read-row: {e}")))?;
        rows.push(row.unwrap().into_iter().map(raw_cell).collect());
    }
    let truncated = result.next().is_some();
    let digest = row_digest(&rows);
    Ok(SideResult {
        name: name.to_string(),
        database: profile.database.clone(),
        columns,
        rows,
        truncated,
        elapsed_ms: start.elapsed().as_millis(),
        digest,
    })
}

/// 行多重集摘要：逐行 FNV-1a 后按序求和——顺序无关且重复行敏感（XOR 会让重复行相互抵消，不可用）。
fn row_digest(rows: &[Vec<CellValue>]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    rows.iter()
        .map(|row| {
            let line = row
                .iter()
                .map(|cell| tsv_cell(cell.clone()))
                .collect::<Vec<_>>()
                .join("\x1f");
            let mut hash = FNV_OFFSET;
            for byte in line.as_bytes() {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(FNV_PRIME);
            }
            hash
        })
        .fold(0u64, |acc, hash| acc.wrapping_add(hash))
}

fn compare(left: &SideResult, right: &SideResult) -> DiffReport {
    DiffReport {
        columns_equal: left.columns == right.columns,
        rows_equal: left.rows.len() == right.rows.len(),
        digest_equal: left.digest == right.digest,
    }
}

fn digest_hex(digest: u64) -> String {
    format!("{:016x}", digest)
}

fn render_diff_human(left: &SideResult, right: &SideResult, report: &DiffReport) -> String {
    let yesno = |ok: bool| if ok { "一致" } else { "不一致" };
    let trunc_mark = |s: &SideResult| {
        if s.truncated {
            format!("（截断，前 {} 行）", s.rows.len())
        } else {
            String::new()
        }
    };
    let mut lines = vec![
        format!(
            "[roq diff] {}({}) vs {}({})",
            left.name, left.database, right.name, right.database
        ),
        format!("  列名    {}", yesno(report.columns_equal)),
        format!(
            "  行数    左 {}{} / 右 {}{} → {}",
            left.rows.len(),
            trunc_mark(left),
            right.rows.len(),
            trunc_mark(right),
            yesno(report.rows_equal)
        ),
        format!(
            "  行摘要  {} / {} → {}",
            digest_hex(left.digest),
            digest_hex(right.digest),
            yesno(report.digest_equal)
        ),
    ];
    if left.truncated || right.truncated {
        lines.push("  注意    任一侧截断时，行摘要仅覆盖已取前缀".to_string());
    }
    lines.push(format!("结论: {}", yesno(report.is_equal())));
    lines.join("\n")
}

fn render_diff_json(left: &SideResult, right: &SideResult, report: &DiffReport) -> String {
    let side = |s: &SideResult| {
        serde_json::json!({
            "profile": s.name,
            "database": s.database,
            "columns": s.columns,
            "rows_returned": s.rows.len(),
            "truncated": s.truncated,
            "digest": digest_hex(s.digest),
            "elapsedMs": s.elapsed_ms,
        })
    };
    serde_json::json!({
        "left": side(left),
        "right": side(right),
        "columnsEqual": report.columns_equal,
        "rowsEqual": report.rows_equal,
        "digestEqual": report.digest_equal,
        "equal": report.is_equal(),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造测试侧结果（digest 按行内容现算）。
    fn side(name: &str, db: &str, columns: &[&str], rows: Vec<Vec<CellValue>>) -> SideResult {
        SideResult {
            name: name.to_string(),
            database: db.to_string(),
            columns: columns.iter().map(|s| s.to_string()).collect(),
            digest: row_digest(&rows),
            rows,
            truncated: false,
            elapsed_ms: 5,
        }
    }

    fn text_row(values: &[&str]) -> Vec<CellValue> {
        values.iter().map(|v| CellValue::Text(v.to_string())).collect()
    }

    #[test]
    fn digest_is_order_insensitive_and_duplicate_sensitive() {
        let a = [text_row(&["1"]), text_row(&["2"])];
        let b = [text_row(&["2"]), text_row(&["1"])];
        assert_eq!(row_digest(&a), row_digest(&b), "顺序不同应同摘要");
        let c = [text_row(&["1"]), text_row(&["1"])];
        assert_ne!(row_digest(&a), row_digest(&c), "重复行分布不同应不同摘要");
        let d = [text_row(&["1"])];
        assert_ne!(row_digest(&a), row_digest(&d), "行数不同应不同摘要");
    }

    #[test]
    fn compare_judges_each_dimension_independently() {
        let same = (
            side("l", "db_l", &["id", "name"], vec![text_row(&["1", "a"])]),
            side("r", "db_r", &["id", "name"], vec![text_row(&["1", "a"])]),
        );
        assert!(compare(&same.0, &same.1).is_equal(), "完全相同应一致");
        // 列名不同（顺序敏感）
        let (l, r) = (
            side("l", "d", &["id", "name"], vec![]),
            side("r", "d", &["name", "id"], vec![]),
        );
        let report = compare(&l, &r);
        assert!(!report.columns_equal);
        // 行数不同
        let (l, r) = (side("l", "d", &["id"], vec![text_row(&["1"])]), side("r", "d", &["id"], vec![]));
        let report = compare(&l, &r);
        assert!(!report.rows_equal && report.columns_equal);
    }

    #[test]
    fn render_diff_human_and_json_carry_verdict() {
        let l = side("dev", "db_a", &["id"], vec![text_row(&["1"])]);
        let r = side("prod-copy", "db_b", &["id"], vec![text_row(&["2"])]);
        let report = compare(&l, &r);
        assert!(!report.is_equal());
        let human = render_diff_human(&l, &r, &report);
        assert!(human.contains("dev(db_a)"), "应含左标识：{}", human);
        assert!(human.contains("prod-copy(db_b)"), "应含右标识：{}", human);
        assert!(human.contains("结论: 不一致"), "应含结论：{}", human);
        let json = render_diff_json(&l, &r, &report);
        assert!(json.contains("\"equal\":false"), "{}", json);
        assert!(json.contains("\"profile\":\"prod-copy\""), "{}", json);
    }
}
