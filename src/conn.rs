//! 连接适配层：MySQL 与 Oracle 的驱动差异收敛在 [`RoqConn`] 之后。
//!
//! [`connect_readonly`] 统一"建连 + 第二层只读防护"（会话只读 SET、回读断言、慢查询保险丝）；
//! [`RoqConn::query`] / [`RoqConn::query_first_string`] 统一取行，行在迭代时归一为
//! [`CellValue`]。调用方（query / cmd_diff / cmd_config）只见引擎无关接口，不直接接触驱动类型。

use std::time::Duration;

use mysql::prelude::Queryable;
use mysql::{Conn, Opts, OptsBuilder, SslOpts};

use crate::config::{Engine, Profile};
use crate::render::{raw_cell, CellValue};

/// 连接超时（秒）。
const CONNECT_TIMEOUT_SECS: u64 = 8;
/// 单次读超时（秒），覆盖流式取数全程。
const READ_TIMEOUT_SECS: u64 = 60;
/// 服务端 SELECT 执行时间上限（毫秒），防慢查询拖垮库。
const MAX_EXECUTION_TIME_MS: u32 = 30_000;

/// 由 profile 构建 MySQL 连接参数（适配层内部共用）。
/// 密码由调用方先经 [`Profile::resolved_password`] 解析后传入。
pub fn build_opts(profile: &Profile, password: &str) -> Opts {
    let mut builder = OptsBuilder::new()
        .ip_or_hostname(Some(profile.host.clone()))
        .tcp_port(profile.port)
        .user(Some(profile.user.clone()))
        .pass(Some(password.to_string()))
        .db_name(Some(profile.database.clone()))
        .tcp_connect_timeout(Some(Duration::from_secs(CONNECT_TIMEOUT_SECS)))
        .read_timeout(Some(Duration::from_secs(READ_TIMEOUT_SECS)));
    // 若配置声明 ssl=true 则启用 TLS（加密但不校验证书，等价 JDBC useSSL=true 默认语义；
    // MySQL 服务端普遍自签证书，严格校验会挡掉绝大多数真实端点）。
    if profile.ssl {
        builder = builder
            .ssl_opts(Some(SslOpts::default().with_danger_accept_invalid_certs(true)));
    }
    Opts::from(builder)
}

/// 引擎无关的连接句柄：驱动差异（建连、会话只读、取行）全部收敛在此 enum 之后。
/// Oracle arm 由后续任务接入。
pub enum RoqConn {
    Mysql(Conn),
}

/// 建连 + 第二层只读防护：会话只读 SET、回读断言、慢查询保险丝。
/// 失败时直接向 stderr 报具体原因，并返回供审计的 reason（调用方统一记 connect-error 并退出）。
pub fn connect_readonly(
    profile: &Profile,
    password: &str,
    notice: &dyn Fn(String),
) -> Result<RoqConn, String> {
    match profile.engine {
        Engine::Mysql => {
            let mut conn = Conn::new(build_opts(profile, password)).map_err(|e| {
                eprintln!("[roq] 连接失败：{}", e);
                e.to_string()
            })?;
            // 第二层防护：会话级只读。失败即中止（不允许降级为可写会话）。
            if let Err(e) = conn.query_drop("SET SESSION TRANSACTION READ ONLY") {
                eprintln!("[roq] 设置会话只读失败（中止查询）：{}", e);
                return Err(format!("session-readonly: {e}"));
            }
            // 回读断言：防止代理/内核静默忽略 SET，导致会话实际可写。
            let readonly_flag: Option<String> = conn
                .query_first("SELECT @@session.transaction_read_only")
                .map_err(|e| {
                    eprintln!("[roq] 回读只读标志失败（中止查询）：{}", e);
                    format!("verify-readonly: {e}")
                })?;
            if readonly_flag.as_deref() != Some("1") {
                eprintln!(
                    "[roq] 服务端确认会话非只读（值={:?}），中止查询",
                    readonly_flag
                );
                return Err("verify-readonly: flag!=1".to_string());
            }
            // 慢查询保险丝：超时上限。个别内核不支持该变量时仅告警不阻断。
            if let Err(e) = conn.query_drop(format!(
                "SET SESSION max_execution_time={}",
                MAX_EXECUTION_TIME_MS
            )) {
                notice(format!("[roq] 警告：未能设置 max_execution_time（{}）", e));
            }
            Ok(RoqConn::Mysql(conn))
        }
        // Oracle 建连由后续任务接入；先 fail-closed 拒绝，不给绕道 mysql 驱动的半成品路径。
        Engine::Oracle => Err("oracle-unsupported: oracle 引擎连接尚未接入".to_string()),
    }
}

/// 查询结果：列名 + 已归一行的流式迭代（每行 `Result<Vec<CellValue>, String>`）。
/// 列名在构造时取出；行迭代保持驱动语义，延迟逐行取回。
pub struct RoqResult<'a> {
    columns: Vec<String>,
    rows: Box<dyn Iterator<Item = Result<Vec<CellValue>, String>> + 'a>,
}

impl<'a> RoqResult<'a> {
    /// 结果集列名（顺序即服务端返回顺序）。
    pub fn columns(&self) -> &[String] {
        &self.columns
    }
}

impl Iterator for RoqResult<'_> {
    type Item = Result<Vec<CellValue>, String>;

    fn next(&mut self) -> Option<Self::Item> {
        self.rows.next()
    }
}

impl RoqConn {
    /// 执行一条查询并返回流式结果。
    pub fn query(&mut self, sql: &str) -> Result<RoqResult<'_>, String> {
        match self {
            RoqConn::Mysql(conn) => {
                let result = conn.query_iter(sql).map_err(|e| e.to_string())?;
                let columns: Vec<String> = result
                    .columns()
                    .as_ref()
                    .iter()
                    .map(|col| col.name_str().to_string())
                    .collect();
                // 行错误原样透出（文本与驱动 Display 一致）；行内取值保持 unwrap 的驱动断言。
                let rows = result.map(|row| match row {
                    Ok(row) => Ok(row.unwrap().into_iter().map(raw_cell).collect()),
                    Err(e) => Err(e.to_string()),
                });
                Ok(RoqResult {
                    columns,
                    rows: Box::new(rows),
                })
            }
        }
    }

    /// 执行查询并取首行首列的字符串值（无行返回 None）。
    pub fn query_first_string(&mut self, sql: &str) -> Result<Option<String>, String> {
        match self {
            RoqConn::Mysql(conn) => conn.query_first(sql).map_err(|e| e.to_string()),
        }
    }
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
