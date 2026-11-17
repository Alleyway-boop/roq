//! roq —— 只读数据库查询工具（read-only query，MySQL / Oracle 双引擎）。
//!
//! 三层只读保障：
//! 1. [`gate`]：本地语句闸门——只放行单条只读语句，首词白名单按引擎分流
//!    （mysql：SELECT/SHOW/EXPLAIN/DESC/DESCRIBE/WITH/TABLE/VALUES；oracle：仅 SELECT/WITH），
//!    拒绝多语句、块注释（防 `/*! */` 版本注释注入）、EXPLAIN ANALYZE（会真实执行），
//!    并对 SELECT/WITH/TABLE/VALUES 全句扫描写/管理类关键词（词边界：update_time 不误伤）；
//!    注释与转义语义亦按引擎分流（oracle：`#` 非注释、`--` 立即注释、引号内反斜杠为字面量、反引号拒绝）。
//! 2. [`conn`]：服务端会话只读——mysql 连接后执行 SET SESSION TRANSACTION READ ONLY 并回读断言，
//!    oracle 执行 SET TRANSACTION READ ONLY（协议保证不被静默忽略，无需回读），
//!    即使闸门被绕过，写入也会被服务器拒绝；另设执行时间保险丝（mysql max_execution_time / oracle call timeout）。
//! 3. 输出限额——行数与单元格长度封顶（终端展示），避免大结果刷屏。
//!
//! 模块划分：[`cli`] 参数 / [`config`] 连接配置（目录扫描，一项目一文件）/
//! [`gate`] 只读闸门 / [`conn`] 连接适配层 / [`query`] 执行链路 /
//! [`render`] TSV 渲染 / [`audit`] 审计日志与结果存档。

pub mod audit;
pub mod b64;
pub mod cli;
pub mod cmd_config;
pub mod cmd_config_migrate;
pub mod cmd_diff;
pub mod cmd_log;
pub mod cmd_skill;
pub mod config;
pub mod conn;
#[cfg(windows)]
pub mod dpapi;
pub mod gate;
pub mod query;
pub mod render;
