//! 执行链路：连接 → 会话只读（SET + 回读断言）→ 流式输出 → 存档与审计。
//!
//! 每个失败路径都必须留审计痕迹，由 [`Self::execute`] 内的 log_exit 统一出口保证。

use std::io::{BufWriter, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use mysql::prelude::Queryable;
use mysql::{Conn, Opts, OptsBuilder, SslOpts};

use crate::audit::{append_audit_log, audit_line, json_escape, save_result};
use crate::cli::Cli;
use crate::config::Profile;
use crate::render::{escape_cell, truncate_cell};

/// 连接超时（秒）。
const CONNECT_TIMEOUT_SECS: u64 = 8;
/// 单次读超时（秒），覆盖流式取数全程。
const READ_TIMEOUT_SECS: u64 = 60;
/// 服务端 SELECT 执行时间上限（毫秒），防慢查询拖垮库。
const MAX_EXECUTION_TIME_MS: u32 = 30_000;

/// 由 profile 构建连接参数（查询执行与 config test 共用）。
pub fn build_opts(profile: &Profile) -> Opts {
    let mut builder = OptsBuilder::new()
        .ip_or_hostname(Some(profile.host.clone()))
        .tcp_port(profile.port)
        .user(Some(profile.user.clone()))
        .pass(Some(profile.password.clone()))
        .db_name(Some(profile.database.clone()))
        .tcp_connect_timeout(Some(Duration::from_secs(CONNECT_TIMEOUT_SECS)))
        .read_timeout(Some(Duration::from_secs(READ_TIMEOUT_SECS)));
    // 若配置声明 ssl=true 则启用 TLS（不校验证书，等价 JDBC useSSL=true）。
    if profile.ssl {
        builder = builder.ssl_opts(Some(SslOpts::default()));
    }
    Opts::from(builder)
}

/// 执行一条已过闸门的查询。所有退出路径（含失败）都会写审计日志。
pub fn execute(cli: &Cli, profile: &Profile, sql: &str) -> ExitCode {
    if cli.profile == "prod" {
        eprintln!("[roq] 注意：正在查询生产库 {}（只读会话）", profile.database);
    }
    let start = Instant::now();
    // 统一的"记审计并退出"出口，保证失败路径无一漏记。
    let log_exit = |outcome: &str, reason: &str, rows: usize, truncated: bool, result_file: &str, code: u8| -> ExitCode {
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
    let mut conn = match Conn::new(build_opts(profile)) {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("[roq] 连接失败：{}", e);
            return log_exit("connect-error", &e.to_string(), 0, false, "", 3);
        }
    };
    // 第二层防护：会话级只读。失败即中止（不允许降级为可写会话）。
    if let Err(e) = conn.query_drop("SET SESSION TRANSACTION READ ONLY") {
        eprintln!("[roq] 设置会话只读失败（中止查询）：{}", e);
        return log_exit("connect-error", &format!("session-readonly: {e}"), 0, false, "", 3);
    }
    // 回读断言：防止代理/内核静默忽略 SET，导致会话实际可写。
    let readonly_flag: Option<String> = match conn.query_first("SELECT @@session.transaction_read_only") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[roq] 回读只读标志失败（中止查询）：{}", e);
            return log_exit("connect-error", &format!("verify-readonly: {e}"), 0, false, "", 3);
        }
    };
    if readonly_flag.as_deref() != Some("1") {
        eprintln!("[roq] 服务端确认会话非只读（值={:?}），中止查询", readonly_flag);
        return log_exit("connect-error", "verify-readonly: flag!=1", 0, false, "", 3);
    }
    // 慢查询保险丝：超时上限。个别内核不支持该变量时仅告警不阻断。
    if let Err(e) = conn.query_drop(format!("SET SESSION max_execution_time={}", MAX_EXECUTION_TIME_MS)) {
        eprintln!("[roq] 警告：未能设置 max_execution_time（{}）", e);
    }
    let mut result = match conn.query_iter(sql) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("[roq] 执行失败：{}", e);
            return log_exit("exec-error", &e.to_string(), 0, false, "", 3);
        }
    };
    let columns: Vec<String> = result
        .columns()
        .as_ref()
        .iter()
        .map(|col| col.name_str().to_string())
        .collect();
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    // 终端展示截断；lines 收集全量（不截断）供存档。
    let mut lines: Vec<String> = vec![columns.join("\t")];
    let _ = writeln!(out, "{}", columns.join("\t"));
    let mut rows_out = 0usize;
    let mut truncated = false;
    while rows_out < cli.max_rows {
        let Some(row) = result.next() else { break };
        let row = match row {
            Ok(row) => row,
            Err(e) => {
                let _ = out.flush();
                eprintln!("[roq] 读取行失败：{}", e);
                let partial = save_result(&cli.profile, sql, &profile.database, cli.max_rows, &lines)
                    .unwrap_or_default();
                return log_exit("exec-error", &format!("read-row: {e}"), rows_out, false, &partial, 3);
            }
        };
        let full: Vec<String> = row.unwrap().into_iter().map(escape_cell).collect();
        let display: String = full
            .iter()
            .map(|cell| truncate_cell(cell.clone(), cli.max_cell))
            .collect::<Vec<_>>()
            .join("\t");
        let _ = writeln!(out, "{}", display);
        lines.push(full.join("\t"));
        rows_out += 1;
    }
    if result.next().is_some() {
        truncated = true;
    }
    let _ = out.flush();
    let result_file = save_result(&cli.profile, sql, &profile.database, cli.max_rows, &lines)
        .unwrap_or_default();
    eprintln!(
        "[roq] profile={} db={} 行数={}{} 耗时={}ms（会话只读）存档={}",
        cli.profile,
        profile.database,
        rows_out,
        if truncated { "（已达上限，结果被截断）" } else { "" },
        start.elapsed().as_millis(),
        if result_file.is_empty() { "失败" } else { &result_file }
    );
    log_exit("ok", "", rows_out, truncated, &result_file, 0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn timeouts_are_sane() {
        // 常量仅自证存在与量级，防止误改单位（秒/毫秒）
        assert_eq!(super::CONNECT_TIMEOUT_SECS, 8);
        assert_eq!(super::READ_TIMEOUT_SECS, 60);
        assert_eq!(super::MAX_EXECUTION_TIME_MS, 30_000);
    }
}
