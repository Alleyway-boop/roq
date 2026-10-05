//! 命令行参数解析与用法说明。

use std::path::PathBuf;

/// 默认最多输出行数（终端与存档共用上限）。
pub const DEFAULT_MAX_ROWS: usize = 500;
/// 终端展示的默认单元格字符数上限（存档不截断）。
pub const DEFAULT_MAX_CELL: usize = 200;

pub struct Cli {
    pub profile: String,
    pub sql: Option<String>,
    pub max_rows: usize,
    pub max_cell: usize,
    pub list: bool,
    pub config: Option<PathBuf>,
}

pub fn usage() -> String {
    [
        "roq —— 只读 MySQL 查询工具".to_string(),
        String::new(),
        "用法: roq [--profile 名] [--max-rows N] [--max-cell N] [--config 路径] [--list] \"SQL语句\"".to_string(),
        "      roq --help".to_string(),
        String::new(),
        "  --profile, -p   连接配置名（默认 dev；生产库请显式 --profile prod）".to_string(),
        "  --max-rows      最多输出行数（默认 500）".to_string(),
        "  --max-cell      单元格最大字符数（默认 200，超出截断；仅影响终端，存档全量）".to_string(),
        "  --config        配置文件或目录（目录=扫描其中 *.conf；默认 ~/.roq/profiles.conf + profiles.d/*.conf）".to_string(),
        "  --list          列出可用配置名".to_string(),
        String::new(),
        "仅接受单条只读语句：SELECT / SHOW / EXPLAIN / DESC / DESCRIBE / WITH / TABLE / VALUES".to_string(),
        "输出为 TSV（制表符分隔）；NULL 显示为 \\N；\\t \\n \\r 为转义序列。".to_string(),
    ]
    .join("\n")
}

/// 解析命令行。--help/--version 以 Err 返回完整文本（调用方直接打印后正常退出）。
pub fn parse_args(args: &[String]) -> Result<Cli, String> {
    let mut cli = Cli {
        profile: "dev".to_string(),
        sql: None,
        max_rows: DEFAULT_MAX_ROWS,
        max_cell: DEFAULT_MAX_CELL,
        list: false,
        config: None,
    };
    let mut sql_parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let next = || args.get(i + 1).cloned();
        match arg.as_str() {
            "--help" | "-h" => return Err(usage()),
            "--version" | "-v" => return Err(format!("roq {}", env!("CARGO_PKG_VERSION"))),
            "--list" => cli.list = true,
            "--config" => {
                let Some(v) = next() else { return Err("--config 缺少参数".into()) };
                cli.config = Some(PathBuf::from(v));
                i += 1;
            }
            "--profile" | "-p" => {
                let Some(v) = next() else { return Err("--profile 缺少参数".into()) };
                cli.profile = v;
                i += 1;
            }
            "--max-rows" => {
                let Some(v) = next() else { return Err("--max-rows 缺少参数".into()) };
                cli.max_rows = v.parse().map_err(|_| "--max-rows 需要正整数")?;
                i += 1;
            }
            "--max-cell" => {
                let Some(v) = next() else { return Err("--max-cell 缺少参数".into()) };
                cli.max_cell = v.parse().map_err(|_| "--max-cell 需要正整数")?;
                i += 1;
            }
            _ => sql_parts.push(arg.clone()),
        }
        i += 1;
    }
    if !sql_parts.is_empty() {
        cli.sql = Some(sql_parts.join(" "));
    }
    Ok(cli)
}
