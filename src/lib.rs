//! roq —— 只读 MySQL 查询工具（read-only query）。
//!
//! 三层只读保障：
//! 1. [`gate`]：本地语句闸门——只放行单条只读语句（SELECT/SHOW/EXPLAIN/DESC/DESCRIBE/WITH/TABLE/VALUES），
//!    拒绝多语句、块注释（防 `/*! */` 版本注释注入）、EXPLAIN ANALYZE（会真实执行），
//!    并对 SELECT/WITH/TABLE/VALUES 全句扫描写/管理类关键词（词边界：update_time 不误伤）。
//! 2. [`query`]：服务端会话只读——连接后执行 SET SESSION TRANSACTION READ ONLY 并回读断言，
//!    即使闸门被绕过，写入也会被服务器拒绝；另设 SELECT 执行时间上限。
//! 3. 输出限额——行数与单元格长度封顶（终端展示），避免大结果刷屏。
//!
//! 模块划分：[`cli`] 参数 / [`config`] 连接配置（目录扫描，一项目一文件）/
//! [`gate`] 只读闸门 / [`query`] 执行链路 / [`render`] TSV 渲染 / [`audit`] 审计日志与结果存档。

pub mod audit;
pub mod cli;
pub mod cmd_config;
pub mod config;
pub mod gate;
pub mod query;
pub mod render;
