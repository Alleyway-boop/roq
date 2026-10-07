//! 命令行参数解析与用法说明。

use std::path::PathBuf;

use crate::render::OutputFormat;

/// 默认最多输出行数（终端与存档共用上限）。
pub const DEFAULT_MAX_ROWS: usize = 500;
/// 终端展示的默认单元格字符数上限（存档不截断）。
pub const DEFAULT_MAX_CELL: usize = 200;

#[derive(Debug)]
pub struct Cli {
    pub profile: String,
    pub sql: Option<String>,
    pub max_rows: usize,
    pub max_cell: usize,
    pub format: OutputFormat,
    /// 结果输出文件（--out）：替代终端输出与自动存档，审计 result 记此路径。
    pub out: Option<PathBuf>,
    /// 抑制 stderr 提示信息（--quiet）：错误与退出码不受影响，审计照写。
    pub quiet: bool,
    pub list: bool,
    pub config: Option<PathBuf>,
}

/// 顶层命令：查询（默认）或 config 子命令。
pub enum Command {
    Query(Cli),
    Config(ConfigCmd),
    Log(LogArgs),
}

/// `roq log` 子命令参数（默认范围=今天）。
pub struct LogArgs {
    pub month: bool,
    pub last: Option<usize>,
    pub profile: Option<String>,
    pub outcome: Option<String>,
    pub json: bool,
}

/// `roq config` 子命令集。
pub enum ConfigCmd {
    Add {
        name: String,
        host: String,
        port: u16,
        user: String,
        password: Option<String>,
        password_env: Option<String>,
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
        "  --max-cell      单元格最大字符数（默认 200，超出截断；仅 tsv 终端显示生效，存档全量）".to_string(),
        "  --format        输出格式 tsv/json/csv（默认 tsv）。json 为单文档对象，csv 遵循 RFC 4180".to_string(),
        "  --out           结果写入文件（纯数据，按 --format；替代终端输出与自动存档，审计记此路径）".to_string(),
        "  --quiet, -q     抑制 stderr 提示信息（错误与退出码不受影响；审计照写）".to_string(),
        "  --config        配置文件或目录（目录=扫描其中 *.conf；默认 ~/.roq/profiles.conf + profiles.d/*.conf）".to_string(),
        "  --list          列出可用配置名".to_string(),
        String::new(),
        "仅接受单条只读语句：SELECT / SHOW / EXPLAIN / DESC / DESCRIBE / WITH / TABLE / VALUES".to_string(),
        "tsv：NULL 显示为 \\N，\\t \\n \\r 为转义序列；json：NULL 为 null（单元格不截断）；csv：NULL 为空字段。".to_string(),
    ]
    .join("\n")
}

/// 顶层解析：`config` / `log` 开头走对应子命令，其余走查询。
pub fn parse_command(args: &[String]) -> Result<Command, String> {
    match args.first().map(String::as_str) {
        Some("config") => Ok(Command::Config(parse_config_cmd(&args[1..])?)),
        Some("log") => Ok(Command::Log(parse_log_cmd(&args[1..])?)),
        _ => Ok(Command::Query(parse_args(args)?)),
    }
}

fn log_usage() -> String {
    [
        "roq log —— 查看审计日志（本月 ~/.roq/logs/roq-YYYYMM.jsonl）".to_string(),
        String::new(),
        "用法:".to_string(),
        "  roq log [--today | --month | --last N] [--profile 名] [--outcome 类别] [--json]"
            .to_string(),
        String::new(),
        "  --today     只看今天（默认）".to_string(),
        "  --month     看本月全部".to_string(),
        "  --last N    最近 N 条（不限日期）".to_string(),
        "  --profile   按配置名过滤".to_string(),
        "  --outcome   按结果过滤：ok / gate-rejected / connect-error / exec-error / config-error"
            .to_string(),
        "  --json      原样输出匹配的 JSONL 行".to_string(),
    ]
    .join("\n")
}

fn parse_log_cmd(args: &[String]) -> Result<LogArgs, String> {
    let mut log_args = LogArgs {
        month: false,
        last: None,
        profile: None,
        outcome: None,
        json: false,
    };
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--help" | "-h" => return Err(log_usage()),
            "--today" => {} // 默认行为，显式写出也接受
            "--month" => log_args.month = true,
            "--last" => {
                let Some(v) = args.get(i + 1) else {
                    return Err("--last 缺少参数".into());
                };
                log_args.last = Some(v.parse().map_err(|_| "--last 需要正整数")?);
                i += 1;
            }
            "--profile" => {
                let Some(v) = args.get(i + 1) else {
                    return Err("--profile 缺少参数".into());
                };
                log_args.profile = Some(v.clone());
                i += 1;
            }
            "--outcome" => {
                let Some(v) = args.get(i + 1) else {
                    return Err("--outcome 缺少参数".into());
                };
                log_args.outcome = Some(v.clone());
                i += 1;
            }
            "--json" => log_args.json = true,
            other => return Err(format!("无法识别的参数：{}", other)),
        }
        i += 1;
    }
    Ok(log_args)
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
    let mut password_env: Option<String> = None;
    let mut database: Option<String> = None;
    let mut ssl = false;
    let mut file: Option<PathBuf> = None;
    let mut yes = false;
    let mut action: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let take = |slot: &mut Option<String>, flag: &str, i: usize| -> Result<(), String> {
            let Some(v) = args.get(i + 1) else {
                return Err(format!("{} 缺少参数", flag));
            };
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
            "--password-env" => {
                take(&mut password_env, arg, i)?;
                i += 1;
            }
            "--database" => {
                take(&mut database, arg, i)?;
                i += 1;
            }
            "--file" => {
                let Some(v) = args.get(i + 1) else {
                    return Err("--file 缺少参数".into());
                };
                file = Some(PathBuf::from(v));
                i += 1;
            }
            "--port" => {
                let Some(v) = args.get(i + 1) else {
                    return Err("--port 缺少参数".into());
                };
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
            password,
            password_env,
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
        format: OutputFormat::Tsv,
        out: None,
        quiet: false,
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
                let Some(v) = next() else {
                    return Err("--config 缺少参数".into());
                };
                cli.config = Some(PathBuf::from(v));
                i += 1;
            }
            "--profile" | "-p" => {
                let Some(v) = next() else {
                    return Err("--profile 缺少参数".into());
                };
                cli.profile = v;
                i += 1;
            }
            "--max-rows" => {
                let Some(v) = next() else {
                    return Err("--max-rows 缺少参数".into());
                };
                cli.max_rows = v.parse().map_err(|_| "--max-rows 需要正整数")?;
                i += 1;
            }
            "--max-cell" => {
                let Some(v) = next() else {
                    return Err("--max-cell 缺少参数".into());
                };
                cli.max_cell = v.parse().map_err(|_| "--max-cell 需要正整数")?;
                i += 1;
            }
            "--format" => {
                let Some(v) = next() else {
                    return Err("--format 缺少参数".into());
                };
                cli.format = OutputFormat::parse(&v)?;
                i += 1;
            }
            "--out" => {
                let Some(v) = next() else {
                    return Err("--out 缺少参数".into());
                };
                cli.out = Some(PathBuf::from(v));
                i += 1;
            }
            "--quiet" | "-q" => cli.quiet = true,
            _ => sql_parts.push(arg.clone()),
        }
        i += 1;
    }
    if !sql_parts.is_empty() {
        cli.sql = Some(sql_parts.join(" "));
    }
    Ok(cli)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_reads_out_quiet_and_sql_parts() {
        let cli = parse_args(&[
            "--out".to_string(),
            "data.tsv".to_string(),
            "--quiet".to_string(),
            "--format".to_string(),
            "json".to_string(),
            "SELECT".to_string(),
            "1".to_string(),
        ])
        .unwrap();
        assert_eq!(cli.out, Some(PathBuf::from("data.tsv")));
        assert!(cli.quiet);
        assert_eq!(cli.sql.as_deref(), Some("SELECT 1"), "SQL 片段应拼接");
        assert_eq!(cli.format, OutputFormat::Json);
        // 默认值不受影响
        assert!(!parse_args(&["SELECT 1".to_string()]).unwrap().quiet);
        assert_eq!(parse_args(&["SELECT 1".to_string()]).unwrap().out, None);
    }

    #[test]
    fn parse_args_rejects_missing_out_value() {
        let err = parse_args(&["--out".to_string()]).unwrap_err();
        assert!(err.contains("--out"), "报错应指出 --out：{}", err);
    }
}
