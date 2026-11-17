//! 执行链路：适配层建连（会话只读已断言）→ 流式输出 → 存档与审计。
//!
//! 每个失败路径都必须留审计痕迹，由 [`Self::execute`] 内的 log_exit 统一出口保证。
//! 连接与会话只读防护见 [`crate::conn`]，本模块只负责执行编排与输出。

use std::io::{BufWriter, Write};
use std::process::ExitCode;
use std::time::Instant;

use crate::audit::{append_audit_log, audit_line, json_escape, save_result};
use crate::cli::Cli;
use crate::config::Profile;
use crate::conn::connect_readonly;
use crate::render::{
    is_show_create, render_csv, render_json, terminal_max_cell, truncate_cell, tsv_cell,
    CellValue, OutputFormat,
};

/// 执行一条已过闸门的查询。所有退出路径（含失败）都会写审计日志。
pub fn execute(cli: &Cli, profile: &Profile, sql: &str) -> ExitCode {
    // --quiet 只抑制提示（prod 提醒/耗时统计等）；错误信息与退出码契约不受影响。
    let notice = |msg: String| {
        if !cli.quiet {
            eprintln!("{}", msg);
        }
    };
    if cli.profile == "prod" {
        notice(format!(
            "[roq] 注意：正在查询生产库 {}（只读会话）",
            profile.database
        ));
    }
    let start = Instant::now();
    // 统一的"记审计并退出"出口，保证失败路径无一漏记。
    let log_exit = |outcome: &str,
                    reason: &str,
                    rows: usize,
                    truncated: bool,
                    result_file: &str,
                    code: u8|
     -> ExitCode {
        let extra = if reason.is_empty() {
            String::new()
        } else {
            format!(",\"reason\":\"{}\"", json_escape(reason))
        };
        append_audit_log(&audit_line(
            &cli.profile,
            &profile.database,
            sql,
            rows,
            truncated,
            start.elapsed().as_millis(),
            outcome,
            &extra,
            result_file,
        ));
        ExitCode::from(code)
    };
    // 输出目标：--out 写文件（不可写即用法错误，查询不执行）；否则终端 stdout。
    let stdout = std::io::stdout();
    let mut out: Box<dyn Write> = match &cli.out {
        Some(path) => match std::fs::File::create(path) {
            Ok(file) => Box::new(BufWriter::new(file)),
            Err(e) => {
                eprintln!("[roq] 无法创建输出文件 {}：{}", path.display(), e);
                let reason = format!("create-out-file: {e}");
                return log_exit("config-error", &reason, 0, false, "", 1);
            }
        },
        None => Box::new(BufWriter::new(stdout.lock())),
    };
    // 密码在连接前解析：password_env 引用的变量未设置即中止（不回退明文）。
    let password = match profile.resolved_password(&cli.profile) {
        Ok(password) => password,
        Err(reason) => {
            eprintln!("[roq] {}", reason);
            return log_exit("config-error", &reason, 0, false, "", 1);
        }
    };
    let mut conn = match connect_readonly(profile, &password, &notice) {
        Ok(conn) => conn,
        Err(reason) => return log_exit("connect-error", &reason, 0, false, "", 3),
    };
    let mut result = match conn.query(sql) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("[roq] 执行失败：{}", e);
            return log_exit("exec-error", &e, 0, false, "", 3);
        }
    };
    let columns: Vec<String> = result.columns().to_vec();
    // 存档恒为 TSV（lines 收集全量转义行，不截断）；json/csv 额外收集归一单元格，结束时整体渲染。
    // --out 写文件即交付全量（--max-cell 仅终端显示限流），审计 result 记该路径（替代自动存档）；
    // SHOW CREATE TABLE 的单格即交付物，未显式限流时终端不截断。
    let display_max_cell = terminal_max_cell(
        cli.out.is_some(),
        is_show_create(sql),
        cli.max_cell_explicit,
        cli.max_cell,
    );
    let out_path = cli.out.as_ref().map(|p| p.display().to_string());
    let mut lines: Vec<String> = vec![columns.join("\t")];
    let mut raw_rows: Vec<Vec<CellValue>> = Vec::new();
    if cli.format == OutputFormat::Tsv {
        let _ = writeln!(out, "{}", columns.join("\t"));
    }
    let mut rows_out = 0usize;
    let mut truncated = false;
    let mut cell_truncated = false;
    while rows_out < cli.max_rows {
        let Some(row) = result.next() else { break };
        let cells: Vec<CellValue> = match row {
            Ok(cells) => cells,
            Err(e) => {
                let _ = out.flush();
                eprintln!("[roq] 读取行失败：{}", e);
                let partial = match &out_path {
                    Some(path) => path.clone(),
                    None => save_result(&cli.profile, sql, &profile.database, cli.max_rows, &lines)
                        .unwrap_or_default(),
                };
                return log_exit(
                    "exec-error",
                    &format!("read-row: {e}"),
                    rows_out,
                    false,
                    &partial,
                    3,
                );
            }
        };
        lines.push(
            cells
                .iter()
                .cloned()
                .map(tsv_cell)
                .collect::<Vec<_>>()
                .join("\t"),
        );
        match cli.format {
            // TSV 流式逐行写终端；--max-cell 仅此格式生效（json/csv 是交换格式，截断即造假数据）。
            OutputFormat::Tsv => {
                let display: String = cells
                    .into_iter()
                    .map(|cell| {
                        let escaped = tsv_cell(cell);
                        let shown = truncate_cell(escaped.clone(), display_max_cell);
                        if shown != escaped {
                            cell_truncated = true;
                        }
                        shown
                    })
                    .collect::<Vec<_>>()
                    .join("\t");
                let _ = writeln!(out, "{}", display);
            }
            OutputFormat::Json | OutputFormat::Csv => raw_rows.push(cells),
        }
        rows_out += 1;
    }
    if result.next().is_some() {
        truncated = true;
    }
    match cli.format {
        OutputFormat::Json => {
            let _ = writeln!(
                out,
                "{}",
                render_json(&columns, &raw_rows, truncated, start.elapsed().as_millis())
            );
        }
        OutputFormat::Csv => {
            let _ = writeln!(out, "{}", render_csv(&columns, &raw_rows));
        }
        OutputFormat::Tsv => {}
    }
    let _ = out.flush();
    // --out 替代自动存档（文件已是全量结果），审计 result 记该路径。
    let result_file = match &out_path {
        Some(path) => path.clone(),
        None => save_result(&cli.profile, sql, &profile.database, cli.max_rows, &lines)
            .unwrap_or_default(),
    };
    // 单格截断原本只有省略号暗示；明示并指路全量所在（提示在前，摘要收尾）。
    // --out 与 json/csv 模式不截断，此标志不会置位。
    if cell_truncated {
        notice(format!(
            "[roq] tsv 单元格超 --max-cell={} 已截断；全量见存档 {}（或改用 --format json）",
            cli.max_cell,
            if result_file.is_empty() { "失败" } else { &result_file }
        ));
    }
    notice(format!(
        "[roq] profile={} db={} 行数={}{} 耗时={}ms（会话只读）存档={}",
        cli.profile,
        profile.database,
        rows_out,
        if truncated {
            "（已达上限，结果被截断）"
        } else {
            ""
        },
        start.elapsed().as_millis(),
        if result_file.is_empty() {
            "失败"
        } else {
            &result_file
        }
    ));
    log_exit("ok", "", rows_out, truncated, &result_file, 0)
}
