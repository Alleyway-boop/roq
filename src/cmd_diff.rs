//! `roq diff` 子命令：同一条 SQL 在两个 profile 上执行并比对——两库对账的只读安全通道。
//!
//! 比对维度：列名（有序）、行数、行多重集摘要（顺序无关、重复敏感、单元格类型感知）；
//! 任一侧截断时摘要仅覆盖已取前缀并在输出标注。不一致时按 `--sample` 给出两侧
//! 差异行样本（多重集差集，各自原始顺序）。两侧各写一条审计（与手写查询同一底账），
//! 退出码 4 表示比对不一致，其余沿用查询契约（1 用法配置 / 2 闸门 / 3 连接执行）。

use std::collections::HashMap;
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
    let profiles = match load_config_sources(args.config.as_ref()) {
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
    let samples = collect_diff_samples(&sides[0], &sides[1], &report, args.sample);
    let text = if args.json {
        render_diff_json(&sides[0], &sides[1], &report, &samples)
    } else {
        render_diff_human(&sides[0], &sides[1], &report, &samples)
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

/// 单元格的类型感知编码：数值 n1 / 文本 s1 / NULL z。
/// tsv_cell 下 BIGINT 的 1 与 VARCHAR 的 '1' 同渲染为 "1"、NULL 与字面文本 "\N" 同渲染，
/// 摘要若直接用渲染文本会把类型差异判成一致——对账必须区分。
fn digest_cell(cell: &CellValue) -> String {
    match cell {
        CellValue::Null => "z".to_string(),
        CellValue::Num(n) => format!("n{}", n),
        CellValue::Text(t) => format!("s{}", t),
    }
}

/// 行 FNV-1a 摘要（对 digest_cell 拼接的整行计算）。
fn row_hash(row: &[CellValue]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let line = row
        .iter()
        .map(digest_cell)
        .collect::<Vec<_>>()
        .join("\x1f");
    let mut hash = FNV_OFFSET;
    for byte in line.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// 行多重集摘要：逐行哈希后按序求和——顺序无关且重复行敏感（XOR 会让重复行相互抵消，不可用）。
fn row_digest(rows: &[Vec<CellValue>]) -> u64 {
    rows.iter()
        .map(|row| row_hash(row))
        .fold(0u64, |acc, hash| acc.wrapping_add(hash))
}

/// 差异样本（--sample）：行数或行摘要不一致时的多重集差集展示。
#[derive(Debug)]
struct DiffSamples {
    left_only: Vec<Vec<CellValue>>,
    right_only: Vec<Vec<CellValue>>,
    limit: usize,
}

impl DiffSamples {
    fn is_empty(&self) -> bool {
        self.left_only.is_empty() && self.right_only.is_empty()
    }
}

/// 仅当行数或行摘要不一致时计算差集（列名不同但行完全相同时无行级差异可示）。
fn collect_diff_samples(
    left: &SideResult,
    right: &SideResult,
    report: &DiffReport,
    limit: usize,
) -> DiffSamples {
    if limit == 0 || (report.rows_equal && report.digest_equal) {
        return DiffSamples {
            left_only: Vec::new(),
            right_only: Vec::new(),
            limit,
        };
    }
    let (left_only, right_only) = multiset_diff(&left.rows, &right.rows, limit);
    DiffSamples {
        left_only,
        right_only,
        limit,
    }
}

/// 多重集差集：返回两侧各自"出现次数超出对方"的行（按该侧原始顺序，各至多 limit 行）。
fn multiset_diff(
    left: &[Vec<CellValue>],
    right: &[Vec<CellValue>],
    limit: usize,
) -> (Vec<Vec<CellValue>>, Vec<Vec<CellValue>>) {
    let count_rows = |rows: &[Vec<CellValue>]| {
        let mut counts: HashMap<u64, usize> = HashMap::new();
        for row in rows {
            *counts.entry(row_hash(row)).or_insert(0) += 1;
        }
        counts
    };
    let left_counts = count_rows(left);
    let right_counts = count_rows(right);
    (
        surplus_rows(left, &right_counts, limit),
        surplus_rows(right, &left_counts, limit),
    )
}

/// 挑出 rows 中超出 other_counts 计数的行（第 k 次出现且 k > 对侧出现次数即超出）。
fn surplus_rows(
    rows: &[Vec<CellValue>],
    other_counts: &HashMap<u64, usize>,
    limit: usize,
) -> Vec<Vec<CellValue>> {
    let mut seen: HashMap<u64, usize> = HashMap::new();
    let mut out = Vec::new();
    for row in rows {
        let hash = row_hash(row);
        let count = seen.entry(hash).or_insert(0);
        *count += 1;
        if *count > other_counts.get(&hash).copied().unwrap_or(0) && out.len() < limit {
            out.push(row.clone());
        }
    }
    out
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

fn render_diff_human(
    left: &SideResult,
    right: &SideResult,
    report: &DiffReport,
    samples: &DiffSamples,
) -> String {
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
    if !samples.is_empty() {
        lines.push(format!(
            "  差异样本  每侧至多 {} 行（--sample N 调整；行序为该侧结果原始顺序）",
            samples.limit
        ));
        let section = |lines: &mut Vec<String>, title: &str, side: &SideResult, rows: &[Vec<CellValue>]| {
            if rows.is_empty() {
                return;
            }
            lines.push(format!("    {}（{} 行）:", title, rows.len()));
            lines.push(format!("      {}", side.columns.join("\t")));
            for row in rows {
                let cells: Vec<String> = row.iter().map(|c| tsv_cell(c.clone())).collect();
                lines.push(format!("      {}", cells.join("\t")));
            }
        };
        section(
            &mut lines,
            &format!("仅在 {} 侧", left.name),
            left,
            &samples.left_only,
        );
        section(
            &mut lines,
            &format!("仅在 {} 侧", right.name),
            right,
            &samples.right_only,
        );
    }
    if left.truncated || right.truncated {
        lines.push("  注意    任一侧截断时，行摘要仅覆盖已取前缀".to_string());
    }
    lines.push(format!("结论: {}", yesno(report.is_equal())));
    lines.join("\n")
}

/// CellValue → JSON 值。整数优先按 i64/u64 精确解析（雪花 ID 超出 f64 精确范围），
/// 浮点回退 f64，均失败按字符串保底。
fn cell_json(cell: &CellValue) -> serde_json::Value {
    match cell {
        CellValue::Null => serde_json::Value::Null,
        CellValue::Num(n) => {
            if let Ok(i) = n.parse::<i64>() {
                serde_json::Value::from(i)
            } else if let Ok(u) = n.parse::<u64>() {
                serde_json::Value::from(u)
            } else if let Ok(f) = n.parse::<f64>() {
                serde_json::Value::from(f)
            } else {
                serde_json::Value::String(n.clone())
            }
        }
        CellValue::Text(t) => serde_json::Value::String(t.clone()),
    }
}

fn render_diff_json(
    left: &SideResult,
    right: &SideResult,
    report: &DiffReport,
    samples: &DiffSamples,
) -> String {
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
    let sample_rows = |rows: &[Vec<CellValue>]| -> Vec<serde_json::Value> {
        rows.iter()
            .map(|row| serde_json::Value::Array(row.iter().map(cell_json).collect()))
            .collect()
    };
    serde_json::json!({
        "left": side(left),
        "right": side(right),
        "columnsEqual": report.columns_equal,
        "rowsEqual": report.rows_equal,
        "digestEqual": report.digest_equal,
        "equal": report.is_equal(),
        "sample": {
            "limit": samples.limit,
            "leftOnly": sample_rows(&samples.left_only),
            "rightOnly": sample_rows(&samples.right_only),
        },
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
    fn digest_distinguishes_cell_types() {
        // BIGINT 1 与 VARCHAR '1' 的 tsv 渲染同为 "1"，摘要必须靠类型标记区分
        let num = [vec![CellValue::Num("1".into())]];
        let text = [vec![CellValue::Text("1".into())]];
        assert_ne!(row_digest(&num), row_digest(&text), "数值与文本不得同摘要");
        // NULL 渲染为 \N，与字面文本 "\N" 也不得碰撞
        let null_row = [vec![CellValue::Null]];
        let literal = [vec![CellValue::Text("\\N".into())]];
        assert_ne!(row_digest(&null_row), row_digest(&literal));
    }

    #[test]
    fn multiset_diff_finds_surplus_rows_in_original_order() {
        let left = [text_row(&["1"]), text_row(&["2"]), text_row(&["3"])];
        let right = [text_row(&["1"]), text_row(&["4"])];
        let (left_only, right_only) = multiset_diff(&left, &right, 5);
        let cell_text = |row: &Vec<CellValue>| {
            row.iter()
                .map(|c| tsv_cell(c.clone()))
                .collect::<Vec<_>>()
                .join(",")
        };
        assert_eq!(
            left_only.iter().map(cell_text).collect::<Vec<_>>(),
            vec!["2", "3"],
            "仅左侧应按原顺序给出 2、3"
        );
        assert_eq!(
            right_only.iter().map(cell_text).collect::<Vec<_>>(),
            vec!["4"]
        );
        // 重复行：左 [1,1] vs 右 [1] → 仅左多出 1 行
        let (lo, ro) = multiset_diff(&[text_row(&["1"]), text_row(&["1"])], &[text_row(&["1"])], 5);
        assert_eq!(lo.len(), 1);
        assert!(ro.is_empty());
    }

    #[test]
    fn multiset_diff_respects_limit() {
        let left = [text_row(&["2"]), text_row(&["3"]), text_row(&["4"])];
        let (lo, _) = multiset_diff(&left, &[], 2);
        assert_eq!(lo.len(), 2, "limit 应截断样本数");
    }

    #[test]
    fn collect_diff_samples_skips_when_rows_identical_or_disabled() {
        // 行完全一致（仅列名不同）→ 无样本
        let rows = vec![text_row(&["1"])];
        let l = side("l", "d", &["a"], rows.clone());
        let r = side("r", "d", &["b"], rows);
        let report = compare(&l, &r);
        assert!(!report.columns_equal && report.rows_equal && report.digest_equal);
        assert!(
            collect_diff_samples(&l, &r, &report, 5).is_empty(),
            "行一致时不应出样本"
        );
        // 行不一致但 --sample 0 → 关闭
        let l2 = side("l", "d", &["a"], vec![text_row(&["1"])]);
        let r2 = side("r", "d", &["a"], vec![text_row(&["2"])]);
        let report2 = compare(&l2, &r2);
        assert!(collect_diff_samples(&l2, &r2, &report2, 0).is_empty());
    }

    #[test]
    fn cell_json_keeps_big_integers_exact() {
        // 雪花 ID 量级的 u64 超出 f64 精确范围，必须走整数解析而非浮点
        let big = cell_json(&CellValue::Num("2104894858388815873".into()));
        assert_eq!(big.to_string(), "2104894858388815873");
        assert_eq!(cell_json(&CellValue::Null).to_string(), "null");
        assert_eq!(
            cell_json(&CellValue::Num("1.5".into())).to_string(),
            "1.5",
            "浮点数字符串应输出数值"
        );
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
    fn render_diff_human_and_json_carry_verdict_and_samples() {
        let l = side("dev", "db_a", &["id"], vec![text_row(&["1"])]);
        let r = side("prod-copy", "db_b", &["id"], vec![text_row(&["2"])]);
        let report = compare(&l, &r);
        assert!(!report.is_equal());
        let samples = collect_diff_samples(&l, &r, &report, 5);
        let human = render_diff_human(&l, &r, &report, &samples);
        assert!(human.contains("dev(db_a)"), "应含左标识：{}", human);
        assert!(human.contains("prod-copy(db_b)"), "应含右标识：{}", human);
        assert!(human.contains("结论: 不一致"), "应含结论：{}", human);
        assert!(human.contains("仅在 dev 侧"), "应含左独有样本段：{}", human);
        assert!(human.contains("仅在 prod-copy 侧"), "应含右独有样本段：{}", human);
        let json = render_diff_json(&l, &r, &report, &samples);
        assert!(json.contains("\"equal\":false"), "{}", json);
        assert!(json.contains("\"profile\":\"prod-copy\""), "{}", json);
        assert!(
            json.contains("\"leftOnly\":[[\"1\"]]") && json.contains("\"rightOnly\":[[\"2\"]]"),
            "差异样本应进 json：{}",
            json
        );
    }
}
