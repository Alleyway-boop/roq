//! 连接适配层：MySQL 与 Oracle 的驱动差异收敛在 [`RoqConn`] 之后。
//!
//! [`connect_readonly`] 统一"建连 + 第二层只读防护"（会话只读 SET、回读断言、慢查询保险丝）；
//! [`RoqConn::query`] / [`RoqConn::query_first_string`] 统一取行，行在迭代时归一为
//! [`CellValue`]。调用方（query / cmd_diff / cmd_config）只见引擎无关接口，不直接接触驱动类型。

use std::io::Read;
use std::time::Duration;

use mysql::prelude::Queryable;
use mysql::{Conn, Opts, OptsBuilder, SslOpts};
use oracledb::{Lob, Metadata as OracleMetadata, Row as OracleRow};

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
fn build_opts(profile: &Profile, password: &str) -> Opts {
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

// ---------------------------------------------------------------------------
// Oracle 适配：建连、类型映射（全部为适配层内部实现，对外只见 RoqConn）
// ---------------------------------------------------------------------------

/// 日期/时间戳固定格式化（不用会话 NLS，保证存档确定）：
/// `YYYY-MM-DD HH:MM:SS`，小数秒保留到纳秒并去尾零（0 视为无小数秒）。
fn format_oracle_datetime(
    year: i16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    nanoseconds: Option<u32>,
) -> String {
    let base = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        year, month, day, hour, minute, second
    );
    match nanoseconds {
        Some(ns) if ns > 0 => {
            let fraction = format!(".{:09}", ns);
            let fraction = fraction.trim_end_matches('0');
            format!("{}{}", base, fraction)
        }
        _ => base,
    }
}

/// BLOB 占位符：不读内容，仅标注长度（长度未知时用无长度形态）。
fn oracle_blob_placeholder(length: Option<usize>) -> String {
    match length {
        Some(n) => format!("<BLOB {} bytes>", n),
        None => "<BLOB>".to_string(),
    }
}

/// 字节的确定性文本化（RAW/LONG RAW 等二进制列的兜底）：大写 hex，与 Oracle 自身 RAW 显示一致。
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02X}", b)).collect()
}

/// Oracle 建连：host:port/service_name 直连（thin 驱动，无需 Instant Client）。
/// ssl 在 profile 加载时已 fail-closed 拒绝，这里只可能走明文 TCP。
/// 失败文案与 mysql 臂对齐（`[roq] 连接失败：{e}`）。
fn oracle_connect(profile: &Profile, password: &str) -> Result<oracledb::Connection, String> {
    let connect_string = format!("{}:{}/{}", profile.host, profile.port, profile.database);
    let config = oracledb::Config::default()
        .set_credentials(&profile.user, password)
        .set_connect_string(&connect_string)
        .map_err(|e| {
            eprintln!("[roq] 连接失败：{}", e);
            e.to_string()
        })?
        .set_transport_connect_timeout(Some(Duration::from_secs(CONNECT_TIMEOUT_SECS)));
    oracledb::connect(config).map_err(|e| {
        eprintln!("[roq] 连接失败：{}", e);
        e.to_string()
    })
}

/// 单元格映射：按列元数据 db_type 分派（NULL 统一经 Option 路径归 [`CellValue::Null`]）。
fn oracle_cell(
    row: &OracleRow,
    index: usize,
    db_type: &'static oracledb::DbType,
) -> Result<CellValue, String> {
    let t = db_type;
    // 二进制大对象不读内容：只取长度做占位（BFILE 同理，内容在服务端文件系统上）
    if t == oracledb::DB_TYPE_BLOB || t == oracledb::DB_TYPE_BFILE {
        return oracle_blob_cell(row, index);
    }
    if t == oracledb::DB_TYPE_CLOB || t == oracledb::DB_TYPE_NCLOB {
        return oracle_clob_cell(row, index);
    }
    // NUMBER 按字符串取出（驱动精确十进制转字符串），保 38 位精度，绝不走 f64
    if t == oracledb::DB_TYPE_NUMBER {
        return oracle_text_cell::<String>(row, index);
    }
    if t == oracledb::DB_TYPE_DATE
        || t == oracledb::DB_TYPE_TIMESTAMP
        || t == oracledb::DB_TYPE_TIMESTAMP_LTZ
        || t == oracledb::DB_TYPE_TIMESTAMP_TZ
    {
        return oracle_datetime_cell(row, index);
    }
    if t == oracledb::DB_TYPE_INTERVAL_DS {
        return oracle_text_cell::<oracledb::OracleIntervalDS>(row, index);
    }
    if t == oracledb::DB_TYPE_INTERVAL_YM {
        return oracle_text_cell::<oracledb::OracleIntervalYM>(row, index);
    }
    // 浮点非 NUMBER（IEEE 754）：有限值归 Num，NaN/Infinity 归 Text（JSON 数字位不容纳）
    if t == oracledb::DB_TYPE_BINARY_FLOAT {
        return oracle_binary_float_cell(row, index);
    }
    if t == oracledb::DB_TYPE_BINARY_DOUBLE {
        return oracle_binary_double_cell(row, index);
    }
    if t == oracledb::DB_TYPE_BOOLEAN {
        return oracle_text_cell::<bool>(row, index);
    }
    if t == oracledb::DB_TYPE_JSON {
        return oracle_json_cell(row, index);
    }
    // 其余（VARCHAR/CHAR/RAW/ROWID/UROWID 等）：先按字符串取；
    // 二进制列（RAW/LONG RAW）驱动不给字符串化，退到字节 hex——最坏信息不丢
    match row.get::<Option<String>>(index) {
        Ok(value) => Ok(value.map(CellValue::Text).unwrap_or(CellValue::Null)),
        Err(string_err) => match row.get::<Option<Vec<u8>>>(index) {
            Ok(bytes) => Ok(bytes
                .map(|b| CellValue::Text(hex_encode(&b)))
                .unwrap_or(CellValue::Null)),
            Err(_) => Err(string_err.to_string()),
        },
    }
}

/// 整行映射为 [`CellValue`] 序列（列顺序 = 元数据顺序）。
fn oracle_row_cells(row: &OracleRow, metas: &[OracleMetadata]) -> Result<Vec<CellValue>, String> {
    metas
        .iter()
        .enumerate()
        .map(|(index, meta)| oracle_cell(row, index, meta.db_type()))
        .collect()
}

/// 经 Display 字符串化的单元格（NUMBER 走 String 精确转换，INTERVAL/BOOLEAN 走各自 Display）。
fn oracle_text_cell<'a, T>(row: &'a OracleRow, index: usize) -> Result<CellValue, String>
where
    T: oracledb::FromDbValue<'a> + std::fmt::Display,
{
    match row.get::<Option<T>>(index) {
        Ok(value) => Ok(value
            .map(|v| CellValue::Text(v.to_string()))
            .unwrap_or(CellValue::Null)),
        Err(e) => Err(e.to_string()),
    }
}

/// DATE/TIMESTAMP 系列列：固定格式文本（小数秒去尾零，与会话 NLS 无关）。
fn oracle_datetime_cell(row: &OracleRow, index: usize) -> Result<CellValue, String> {
    match row.get::<Option<oracledb::OracleTimestamp>>(index) {
        Ok(value) => Ok(value
            .map(|ts| {
                CellValue::Text(format_oracle_datetime(
                    ts.year(),
                    ts.month(),
                    ts.day(),
                    ts.hour(),
                    ts.minute(),
                    ts.second(),
                    Some(ts.nanoseconds()),
                ))
            })
            .unwrap_or(CellValue::Null)),
        Err(e) => Err(e.to_string()),
    }
}

/// BINARY_FLOAT 列：有限值归 Num（与 mysql 浮点归一一致），NaN/Infinity 归 Text。
fn oracle_binary_float_cell(row: &OracleRow, index: usize) -> Result<CellValue, String> {
    match row.get::<Option<f32>>(index) {
        Ok(Some(v)) if v.is_finite() => Ok(CellValue::Num(v.to_string())),
        Ok(Some(v)) => Ok(CellValue::Text(v.to_string())),
        Ok(None) => Ok(CellValue::Null),
        Err(e) => Err(e.to_string()),
    }
}

/// BINARY_DOUBLE 列：有限值归 Num（与 mysql 浮点归一一致），NaN/Infinity 归 Text。
fn oracle_binary_double_cell(row: &OracleRow, index: usize) -> Result<CellValue, String> {
    match row.get::<Option<f64>>(index) {
        Ok(Some(v)) if v.is_finite() => Ok(CellValue::Num(v.to_string())),
        Ok(Some(v)) => Ok(CellValue::Text(v.to_string())),
        Ok(None) => Ok(CellValue::Null),
        Err(e) => Err(e.to_string()),
    }
}

/// BLOB/BFILE 单元格：只取 Lob 定位器的长度做占位，不读内容（长度取不到就用无长度形态）。
fn oracle_blob_cell(row: &OracleRow, index: usize) -> Result<CellValue, String> {
    match row.get::<Option<Lob>>(index) {
        Ok(lob) => match lob {
            None => Ok(CellValue::Null),
            Some(mut lob) => {
                let length = lob.get_size().ok();
                Ok(CellValue::Text(oracle_blob_placeholder(length)))
            }
        },
        Err(e) => Err(e.to_string()),
    }
}

/// CLOB/NCLOB 单元格：读为文本（驱动的 io::Read 对字符 LOB 已解码为 UTF-8），
/// 全文进入 CellValue::Text，截断交由下游 max-cell/存档限额体系。
fn oracle_clob_cell(row: &OracleRow, index: usize) -> Result<CellValue, String> {
    let lob: Option<Lob> = row.get(index).map_err(|e| e.to_string())?;
    let Some(mut lob) = lob else {
        return Ok(CellValue::Null);
    };
    let size = lob.get_size().map_err(|e| e.to_string())?;
    let mut bytes = Vec::with_capacity(size);
    lob.read_to_end(&mut bytes)
        .map_err(|e| format!("LOB 读取失败：{}", e))?;
    String::from_utf8(bytes)
        .map(CellValue::Text)
        .map_err(|e| format!("LOB 含非 UTF-8 文本：{}", e))
}

/// JSON 列单元格：驱动未提供 JsonValue 的文本化 API，经 serde_json 归一序列化，
/// 键按字典序输出（serde_json::Map 底座为 BTreeMap），保证存档确定。
fn oracle_json_cell(row: &OracleRow, index: usize) -> Result<CellValue, String> {
    match row.get::<Option<oracledb::JsonValue>>(index) {
        Ok(value) => Ok(value
            .map(|v| CellValue::Text(oracle_json_value(&v).to_string()))
            .unwrap_or(CellValue::Null)),
        Err(e) => Err(e.to_string()),
    }
}

/// [`oracledb::JsonValue`] 递归转 serde_json 值。内嵌 NUMBER 保持字符串形态（精确十进制，
/// 绝不走 f64）；二进制（JsonId/Raw）转大写 hex；Vector 无文本化 API，用占位保结构可读。
fn oracle_json_value(value: &oracledb::JsonValue) -> serde_json::Value {
    use oracledb::JsonValue as J;
    match value {
        J::BinaryDouble(v) => serde_json::json!(v),
        J::BinaryFloat(v) => serde_json::json!(v),
        J::Boolean(v) => serde_json::json!(v),
        J::IntervalDS(v) => serde_json::json!(v.to_string()),
        J::IntervalYM(v) => serde_json::json!(v.to_string()),
        J::JsonArray(items) => {
            serde_json::Value::Array(items.iter().map(oracle_json_value).collect())
        }
        J::JsonId(id) => serde_json::json!(hex_encode(id)),
        J::JsonObject(map) => {
            let mut obj = serde_json::Map::new();
            for (key, item) in map {
                obj.insert(key.clone(), oracle_json_value(item));
            }
            serde_json::Value::Object(obj)
        }
        J::Null => serde_json::Value::Null,
        J::Number(v) => serde_json::json!(v.to_string()),
        J::Raw(bytes) => serde_json::json!(hex_encode(bytes)),
        J::String(v) => serde_json::json!(v),
        J::Timestamp(v) => serde_json::json!(format_oracle_datetime(
            v.year(),
            v.month(),
            v.day(),
            v.hour(),
            v.minute(),
            v.second(),
            Some(v.nanoseconds())
        )),
        J::Vector(_) => serde_json::json!("<VECTOR>"),
    }
}

/// 引擎无关的连接句柄：驱动差异（建连、会话只读、取行）全部收敛在此 enum 之后。
pub enum RoqConn {
    Mysql(Conn),
    Oracle(oracledb::Connection),
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
        // Oracle 建连走 thin 驱动直连；第二层防护同样是会话只读，失败即中止。
        Engine::Oracle => {
            let conn = oracle_connect(profile, password)?;
            // 慢查询保险丝：call timeout 覆盖该会话所有 socket 读写（Oracle 无
            // max_execution_time 等价物，这是唯一保险丝），设置失败即中止，不留无限等待的会话。
            if let Err(e) = conn.set_call_timeout(Some(Duration::from_secs(READ_TIMEOUT_SECS))) {
                eprintln!("[roq] 设置会话超时失败（中止查询）：{}", e);
                return Err(format!("call-timeout: {e}"));
            }
            // 第二层防护：事务只读。Oracle 协议保证 SET TRANSACTION 不被静默忽略，
            // 失败即中止，无需 mysql 那样的回读断言（spec 第 3 节决策）。
            if let Err(e) = conn.execute("SET TRANSACTION READ ONLY", &[]) {
                eprintln!("[roq] 设置会话只读失败（中止查询）：{}", e);
                return Err(format!("session-readonly: {e}"));
            }
            Ok(RoqConn::Oracle(conn))
        }
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
            RoqConn::Oracle(conn) => {
                let cursor = conn.query(sql, &[]).map_err(|e| e.to_string())?;
                let columns: Vec<String> = cursor
                    .columns()
                    .iter()
                    .map(|meta| meta.name().to_string())
                    .collect();
                // 行错误原样透出；行内取值按列类型映射（见 oracle_cell），失败整行报 reason
                let rows = cursor.map(|row| match row {
                    Ok(row) => oracle_row_cells(&row, row.columns()),
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
            // 首格走与整行相同的映射（NUMBER 等类型同样精度保真），Text/Num 原样，Null 归 None
            RoqConn::Oracle(conn) => {
                let mut cursor = conn.query(sql, &[]).map_err(|e| e.to_string())?;
                match cursor.next() {
                    None => Ok(None),
                    Some(Err(e)) => Err(e.to_string()),
                    Some(Ok(row)) => {
                        let first = oracle_row_cells(&row, row.columns())?.into_iter().next();
                        match first {
                            Some(CellValue::Text(text)) | Some(CellValue::Num(text)) => {
                                Ok(Some(text))
                            }
                            _ => Ok(None),
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{format_oracle_datetime, oracle_blob_placeholder};

    #[test]
    fn timeouts_are_sane() {
        // 常量仅自证存在与量级，防止误改单位（秒/毫秒）
        assert_eq!(super::CONNECT_TIMEOUT_SECS, 8);
        assert_eq!(super::READ_TIMEOUT_SECS, 60);
        assert_eq!(super::MAX_EXECUTION_TIME_MS, 30_000);
    }

    #[test]
    fn oracle_datetime_format_is_fixed_and_deterministic() {
        // DATE 无小数秒；TIMESTAMP 保留到 6 位并去尾零（不用会话 NLS，保证存档确定）
        assert_eq!(
            format_oracle_datetime(2026, 11, 16, 8, 30, 5, None),
            "2026-11-16 08:30:05"
        );
        assert_eq!(
            format_oracle_datetime(2026, 11, 16, 8, 30, 5, Some(500_000_000)),
            "2026-11-16 08:30:05.5"
        );
        assert_eq!(
            format_oracle_datetime(2026, 11, 16, 8, 30, 5, Some(0)),
            "2026-11-16 08:30:05"
        );
    }

    #[test]
    fn oracle_blob_cell_is_placeholder_without_reading_content() {
        assert_eq!(oracle_blob_placeholder(Some(1024)), "<BLOB 1024 bytes>");
        assert_eq!(oracle_blob_placeholder(None), "<BLOB>");
    }
}
