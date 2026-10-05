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

/// 顶层命令：查询（默认）或 config 子命令。
pub enum Command {
    Query(Cli),
    Config(ConfigCmd),
}

/// `roq config` 子命令集。
pub enum ConfigCmd {
    Add {
        name: String,
        host: String,
        port: u16,
        user: String,
        password: String,
        database: String,
        ssl: bool,
        file: Option<PathBuf>,
    },
    Remove {
        name: String,
        file: Option<PathBuf>,
        yes: bool,
    },
    Test {
        name: String,
    },
    List,
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

/// 顶层解析：`config` 开头走配置子命令，其余走查询。
pub fn parse_command(args: &[String]) -> Result<Command, String> {
    if args.first().map(String::as_str) == Some("config") {
        return Ok(Command::Config(parse_config_cmd(&args[1..])?));
    }
    Ok(Command::Query(parse_args(args)?))
}

fn config_usage() -> String {
    [
        "roq config —— 连接配置管理".to_string(),
        String::new(),
        "用法:".to_string(),
        "  roq config add <名> --host 主机 --user 用户 --password 密码 --database 库 [--port 3306] [--ssl] [--file 路径]".to_string(),
        "  roq config remove <名> [--file 路径] --yes".to_string(),
        "  roq config test <名>".to_string(),
        "  roq config list".to_string(),
        String::new(),
        "add 默认写入 ~/.roq/profiles.d/<名>.conf（一配置一文件）；同名已存在时拒绝，修改请手动编辑。".to_string(),
        "remove 仅当目标文件只含这一个配置节时才删文件，否则提示手动编辑；须 --yes 确认。".to_string(),
    ]
    .join("\n")
}

fn parse_config_cmd(args: &[String]) -> Result<ConfigCmd, String> {
    let mut name: Option<String> = None;
    let mut host: Option<String> = None;
    let mut port: u16 = 3306;
    let mut user: Option<String> = None;
    let mut password: Option<String> = None;
    let mut database: Option<String> = None;
    let mut ssl = false;
    let mut file: Option<PathBuf> = None;
    let mut yes = false;
    let mut action: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let take = |slot: &mut Option<String>, flag: &str, i: usize| -> Result<(), String> {
            let Some(v) = args.get(i + 1) else { return Err(format!("{} 缺少参数", flag)) };
            *slot = Some(v.clone());
            Ok(())
        };
        match arg.as_str() {
            "--help" | "-h" => return Err(config_usage()),
            "add" | "remove" | "test" | "list" if action.is_none() => action = Some(arg),
            "--host" => {
                take(&mut host, arg, i)?;
                i += 1;
            }
            "--user" => {
                take(&mut user, arg, i)?;
                i += 1;
            }
            "--password" => {
                take(&mut password, arg, i)?;
                i += 1;
            }
            "--database" => {
                take(&mut database, arg, i)?;
                i += 1;
            }
            "--file" => {
                let Some(v) = args.get(i + 1) else { return Err("--file 缺少参数".into()) };
                file = Some(PathBuf::from(v));
                i += 1;
            }
            "--port" => {
                let Some(v) = args.get(i + 1) else { return Err("--port 缺少参数".into()) };
                port = v.parse().map_err(|_| "--port 需要整数")?;
                i += 1;
            }
            "--ssl" => ssl = true,
            "--yes" => yes = true,
            other if action.is_some() && name.is_none() && !other.starts_with('-') => {
                name = Some(other.to_string())
            }
            other => return Err(format!("无法识别的参数：{}", other)),
        }
        i += 1;
    }
    match action {
        Some("list") => Ok(ConfigCmd::List),
        Some("test") => Ok(ConfigCmd::Test {
            name: name.ok_or("config test 需要 <名>")?,
        }),
        Some("remove") => Ok(ConfigCmd::Remove {
            name: name.ok_or("config remove 需要 <名>")?,
            file,
            yes,
        }),
        Some("add") => Ok(ConfigCmd::Add {
            name: name.ok_or("config add 需要 <名>")?,
            host: host.ok_or("config add 缺少 --host")?,
            port,
            user: user.ok_or("config add 缺少 --user")?,
            password: password.ok_or("config add 缺少 --password")?,
            database: database.ok_or("config add 缺少 --database")?,
            ssl,
            file,
        }),
        _ => Err(config_usage()),
    }
}
/// 解析查询命令行参数。--help/--version 以 Err 返回完整文本（调用方直接打印后正常退出）。
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
