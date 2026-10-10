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
#[derive(Debug)]
pub enum Command {
    Query(Cli),
    Config(ConfigCmd),
    Log(LogArgs),
    Skill(SkillCmd),
    Diff(DiffArgs),
}

/// `roq diff` 子命令参数：同一条 SQL 在两个 profile 上比对。
#[derive(Debug)]
pub struct DiffArgs {
    /// 恰好两个：左/右参与比对的配置名。
    pub profiles: Vec<String>,
    pub sql: Option<String>,
    /// 每侧比对行数上限（任一侧截断时行摘要仅覆盖已取前缀）。
    pub max_rows: usize,
    /// 机器可读输出（人类可读为默认表格）。
    pub json: bool,
}

/// skill 安装目标：解析层保证三选一（缺省 Claude 全局），类型上杜绝非法组合。
#[derive(Debug, PartialEq, Eq)]
pub enum SkillTarget {
    /// ~/.claude/skills/roq/（缺省，跨项目生效）。
    ClaudeGlobal,
    /// 当前项目 .claude/skills/roq/（仅本项目生效）。
    ClaudeProject,
    /// ~/.codex/skills/roq/（OpenAI Codex CLI 全局）。
    CodexGlobal,
}

/// `roq skill` 子命令：安装随二进制打包的 AI 使用技能（SKILL.md）。
#[derive(Debug)]
pub enum SkillCmd {
    Install {
        /// 安装目标（Claude 全局/项目、Codex 全局）。
        target: SkillTarget,
        /// 目标已存在且内容不同时仍覆盖。
        force: bool,
    },
}

/// `roq log` 子命令参数（默认范围=今天）。
#[derive(Debug)]
pub struct LogArgs {
    pub month: bool,
    pub last: Option<usize>,
    pub profile: Option<String>,
    pub outcome: Option<String>,
    pub json: bool,
}

/// `roq config` 子命令集。
#[derive(Debug)]
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
        "      roq tables [--profile 名 ...]              # SHOW TABLES 快捷方式".to_string(),
        "      roq schema <表|库.表> [--profile 名 ...]   # SHOW CREATE TABLE 快捷方式".to_string(),
        "      roq explain \"SQL语句\" [--profile 名 ...]  # 自动加 EXPLAIN 前缀".to_string(),
        "      roq skill install [--global | --project | --codex] [--force]  # 安装 AI 使用技能".to_string(),
        "      roq diff --profile 左 --profile 右 \"SQL\" [--json]   # 同一条 SQL 两库对账比对".to_string(),
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

/// 顶层解析：`config` / `log` / `tables` / `schema` / `explain` 开头走对应子命令，其余走查询。
/// 快捷子命令只生成 SQL，随后与手写查询走同一条闸门+审计+存档链路。
pub fn parse_command(args: &[String]) -> Result<Command, String> {
    match args.first().map(String::as_str) {
        Some("config") => Ok(Command::Config(parse_config_cmd(&args[1..])?)),
        Some("log") => Ok(Command::Log(parse_log_cmd(&args[1..])?)),
        Some("skill") => Ok(Command::Skill(parse_skill_cmd(&args[1..])?)),
        Some("diff") => Ok(Command::Diff(parse_diff_cmd(&args[1..])?)),
        Some("tables") => shortcut_tables(&args[1..]),
        Some("schema") => shortcut_schema(&args[1..]),
        Some("explain") => shortcut_explain(&args[1..]),
        _ => Ok(Command::Query(parse_args(args)?)),
    }
}

fn shortcut_tables(args: &[String]) -> Result<Command, String> {
    let (mut cli, positional) = parse_query_parts(args)?;
    if !positional.is_empty() {
        return Err(format!("tables 不接受位置参数：{}", positional.join(" ")));
    }
    cli.sql = Some("SHOW TABLES".to_string());
    Ok(Command::Query(cli))
}

fn shortcut_schema(args: &[String]) -> Result<Command, String> {
    let (mut cli, positional) = parse_query_parts(args)?;
    let [table] = positional.as_slice() else {
        return Err("schema 需要恰好一个表名（可用 库.表 两段）".to_string());
    };
    cli.sql = Some(build_schema_sql(table)?);
    Ok(Command::Query(cli))
}

fn shortcut_explain(args: &[String]) -> Result<Command, String> {
    let (mut cli, positional) = parse_query_parts(args)?;
    if positional.is_empty() {
        return Err("explain 需要 SQL 语句".to_string());
    }
    cli.sql = Some(format!("EXPLAIN {}", positional.join(" ")));
    Ok(Command::Query(cli))
}

/// 生成 `SHOW CREATE TABLE`：表名限字母/数字/下划线/$，或 `库.表` 两段；
/// 各段反引号包裹（兼容 order 这类保留字表名）。校验失败即拒绝拼接。
fn build_schema_sql(table: &str) -> Result<String, String> {
    let valid_segment = |seg: &str| {
        !seg.is_empty()
            && seg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
    };
    let parts: Vec<&str> = table.split('.').collect();
    if parts.len() > 2 || !parts.iter().all(|p| valid_segment(p)) {
        return Err(format!(
            "表名 {} 不合法（仅允许字母/数字/下划线/$，或 库.表 两段）",
            table
        ));
    }
    let quoted: Vec<String> = parts.iter().map(|p| format!("`{}`", p)).collect();
    Ok(format!("SHOW CREATE TABLE {}", quoted.join(".")))
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

fn skill_usage() -> String {
    [
        "roq skill —— AI 使用技能（SKILL.md）分发".to_string(),
        String::new(),
        "用法:".to_string(),
        "  roq skill install [--global | --project | --codex] [--force]".to_string(),
        String::new(),
        "  --global    装到 ~/.claude/skills/roq/（默认，全部项目可用）".to_string(),
        "  --project   装到当前项目 .claude/skills/roq/（仅本项目可用）".to_string(),
        "  --codex     装到 ~/.codex/skills/roq/（OpenAI Codex CLI，全局）".to_string(),
        "  --force     目标已存在且内容不同时仍覆盖（内容相同则无需此 flag）".to_string(),
        String::new(),
        "skill 内容随二进制打包，版本永远与 roq 一致；更新 roq 后重跑 install 即可同步。".to_string(),
    ]
    .join("\n")
}

fn parse_skill_cmd(args: &[String]) -> Result<SkillCmd, String> {
    let mut global = false;
    let mut project = false;
    let mut codex = false;
    let mut force = false;
    let mut action: Option<&str> = None;
    for arg in args {
        match arg.as_str() {
            "--help" | "-h" => return Err(skill_usage()),
            "install" if action.is_none() => action = Some("install"),
            "--global" => global = true,
            "--project" => project = true,
            "--codex" => codex = true,
            "--force" => force = true,
            other => return Err(format!("无法识别的参数：{}", other)),
        }
    }
    if action.is_none() {
        return Err(skill_usage());
    }
    let target = match (global, project, codex) {
        // 缺省 --global：三个 flag 都没给时视为 Claude 全局
        (false, false, false) | (true, false, false) => SkillTarget::ClaudeGlobal,
        (false, true, false) => SkillTarget::ClaudeProject,
        (false, false, true) => SkillTarget::CodexGlobal,
        _ => return Err("--global、--project 与 --codex 三选一，一次只装一个目标".to_string()),
    };
    Ok(SkillCmd::Install { target, force })
}
/// 解析查询命令行参数。--help/--version 以 Err 返回完整文本（调用方直接打印后正常退出）。
pub fn parse_args(args: &[String]) -> Result<Cli, String> {
    let (mut cli, sql_parts) = parse_query_parts(args)?;
    if !sql_parts.is_empty() {
        cli.sql = Some(sql_parts.join(" "));
    }
    Ok(cli)
}

/// 查询 flag 集与位置参数的公共解析层（查询 / tables / schema / explain 共用）。
/// 返回填充了 flag 的 Cli 与按序收集的位置参数（查询模式即 SQL 片段）。
fn parse_query_parts(args: &[String]) -> Result<(Cli, Vec<String>), String> {
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
            _ => {
                // 未知 flag（"-" 开头、跳过连字符后首字符为字母）直接报用法错误，
                // 不再静默拼进 SQL 以莫名其妙的闸门拒绝收场；
                // "-3"、"-" 等非 flag 形态仍视作 SQL 片段（负数字面量）。
                if looks_like_flag(arg) {
                    return Err(format!("无法识别的参数：{}（--help 查看用法）", arg));
                }
                sql_parts.push(arg.clone());
            }
        }
        i += 1;
    }
    Ok((cli, sql_parts))
}

/// "形如 flag" 判定："-" 开头且跳过全部前导连字符后首字符为字母（--foo / -x）；
/// "-3"、"-" 等非 flag 形态不算，仍可作为 SQL 片段（负数字面量）。
fn looks_like_flag(arg: &str) -> bool {
    arg.starts_with('-')
        && arg
            .trim_start_matches('-')
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
}

fn diff_usage() -> String {
    [
        "roq diff —— 同一条 SQL 在两个 profile 上执行并比对（两库对账）".to_string(),
        String::new(),
        "用法:".to_string(),
        "  roq diff --profile 左 --profile 右 \"SQL\" [--json] [--max-rows N]".to_string(),
        String::new(),
        "  --profile ×2  参与比对的两个配置（必须显式给两个，不设默认，防对错库）".to_string(),
        "  --json        机器可读输出：{\"left\":{...},\"right\":{...},\"equal\":bool}".to_string(),
        "  --max-rows    每侧比对行数上限（默认 500；任一侧截断时行摘要仅覆盖已取前缀）".to_string(),
        String::new(),
        "比对维度：列名（有序）、行数、行多重集摘要（顺序无关、重复敏感）。".to_string(),
        "退出码：0 一致 / 4 不一致；其余同查询（1 用法配置 / 2 闸门 / 3 连接执行）。".to_string(),
        "两侧各写一条审计日志（与手写查询同一底账）。".to_string(),
    ]
    .join("\n")
}

/// `roq diff` 解析：恰好两个 --profile，剩余位置参数拼为 SQL。
fn parse_diff_cmd(args: &[String]) -> Result<DiffArgs, String> {
    let mut profiles: Vec<String> = Vec::new();
    let mut max_rows = DEFAULT_MAX_ROWS;
    let mut json = false;
    let mut sql_parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let next = || args.get(i + 1).cloned();
        match arg.as_str() {
            "--help" | "-h" => return Err(diff_usage()),
            "--profile" | "-p" => {
                let Some(v) = next() else {
                    return Err("--profile 缺少参数".into());
                };
                if profiles.len() >= 2 {
                    return Err("diff 只接受两个 --profile（左/右各一）".into());
                }
                profiles.push(v);
                i += 1;
            }
            "--max-rows" => {
                let Some(v) = next() else {
                    return Err("--max-rows 缺少参数".into());
                };
                max_rows = v.parse().map_err(|_| "--max-rows 需要正整数")?;
                i += 1;
            }
            "--json" => json = true,
            other => {
                if looks_like_flag(other) {
                    return Err(format!("无法识别的参数：{}（--help 查看用法）", other));
                }
                sql_parts.push(other.to_string());
            }
        }
        i += 1;
    }
    if profiles.is_empty() {
        return Err(diff_usage());
    }
    if profiles.len() != 2 {
        return Err("diff 需要恰好两个 --profile（左/右各一），不设默认防对错库".to_string());
    }
    if sql_parts.is_empty() {
        return Err("diff 需要 SQL 语句".to_string());
    }
    Ok(DiffArgs {
        profiles,
        sql: Some(sql_parts.join(" ")),
        max_rows,
        json,
    })
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

    #[test]
    fn parse_args_rejects_unknown_flags() {
        let err = parse_args(&["--foo".to_string(), "SELECT 1".to_string()]).unwrap_err();
        assert!(err.contains("--foo"), "未知 flag 应报用法错误：{}", err);
        let err = parse_args(&["-x".to_string()]).unwrap_err();
        assert!(err.contains("-x"), "短 flag 形态也应报错：{}", err);
    }

    #[test]
    fn parse_args_keeps_dash_prefixed_literals_as_sql() {
        // 负数字面量与单独 "-" 不是 flag，仍作为 SQL 片段拼接
        let cli = parse_args(&["SELECT".to_string(), "1-2".to_string(), "-3".to_string()]).unwrap();
        assert_eq!(cli.sql.as_deref(), Some("SELECT 1-2 -3"));
        let cli = parse_args(&["SELECT".to_string(), "-".to_string()]).unwrap();
        assert_eq!(cli.sql.as_deref(), Some("SELECT -"));
    }

    #[test]
    fn build_schema_sql_validates_and_backticks_segments() {
        assert_eq!(
            build_schema_sql("t_user").unwrap(),
            "SHOW CREATE TABLE `t_user`"
        );
        assert_eq!(
            build_schema_sql("mydb.t_user").unwrap(),
            "SHOW CREATE TABLE `mydb`.`t_user`"
        );
        for bad in ["", "t; DROP", "ta ble", "a.b.c", "`t`", "t-x"] {
            assert!(build_schema_sql(bad).is_err(), "应拒绝表名：{:?}", bad);
        }
    }

    #[test]
    fn parse_command_shortcuts_generate_gated_sql() {
        // tables → SHOW TABLES,且共用查询 flag
        let Command::Query(cli) = parse_command(&[
            "tables".to_string(),
            "--format".to_string(),
            "json".to_string(),
        ])
        .unwrap() else {
            panic!("tables 应产出 Query 命令");
        };
        assert_eq!(cli.sql.as_deref(), Some("SHOW TABLES"));
        assert_eq!(cli.format, OutputFormat::Json);
        // schema → SHOW CREATE TABLE(反引号防保留字)
        let Command::Query(cli) =
            parse_command(&["schema".to_string(), "t_user".to_string()]).unwrap()
        else {
            panic!("schema 应产出 Query 命令");
        };
        assert_eq!(cli.sql.as_deref(), Some("SHOW CREATE TABLE `t_user`"));
        // explain → EXPLAIN 前缀,片段拼接
        let Command::Query(cli) =
            parse_command(&["explain".to_string(), "SELECT".to_string(), "1".to_string()]).unwrap()
        else {
            panic!("explain 应产出 Query 命令");
        };
        assert_eq!(cli.sql.as_deref(), Some("EXPLAIN SELECT 1"));
        // 错误形态:参数个数与非法表名
        assert!(parse_command(&["tables".to_string(), "多余".to_string()]).is_err());
        assert!(parse_command(&["schema".to_string()]).is_err());
        assert!(parse_command(&["schema".to_string(), "bad name".to_string()]).is_err());
        assert!(parse_command(&["explain".to_string()]).is_err());
    }

    #[test]
    fn parse_skill_install_defaults_to_claude_global() {
        let Command::Skill(SkillCmd::Install { target, force }) =
            parse_command(&["skill".to_string(), "install".to_string()]).unwrap()
        else {
            panic!("skill install 应产出 Skill 命令");
        };
        assert_eq!(target, SkillTarget::ClaudeGlobal, "缺省应为 Claude 全局安装");
        assert!(!force);
    }

    #[test]
    fn parse_skill_install_targets_and_rejections() {
        let Command::Skill(SkillCmd::Install { target, force }) = parse_command(&[
            "skill".to_string(),
            "install".to_string(),
            "--project".to_string(),
            "--force".to_string(),
        ])
        .unwrap() else {
            panic!("应产出 Skill 命令");
        };
        assert_eq!(target, SkillTarget::ClaudeProject);
        assert!(force);
        let Command::Skill(SkillCmd::Install { target, .. }) = parse_command(&[
            "skill".to_string(),
            "install".to_string(),
            "--codex".to_string(),
        ])
        .unwrap() else {
            panic!("--codex 应产出 Skill 命令");
        };
        assert_eq!(target, SkillTarget::CodexGlobal);
        // 三目标两两互斥
        for [a, b] in [
            ["--global", "--project"],
            ["--global", "--codex"],
            ["--project", "--codex"],
        ] {
            let err = parse_command(&[
                "skill".to_string(),
                "install".to_string(),
                a.to_string(),
                b.to_string(),
            ])
            .unwrap_err();
            assert!(err.contains("三选一"), "{} 与 {} 应拒绝互斥：{}", a, b, err);
        }
        let err = parse_command(&["skill".to_string(), "install".to_string(), "--foo".to_string()])
            .unwrap_err();
        assert!(err.contains("--foo"), "未知 flag 应报错：{}", err);
        let err = parse_command(&["skill".to_string()]).unwrap_err();
        assert!(err.contains("roq skill"), "缺动作应回用法：{}", err);
    }

    #[test]
    fn parse_diff_takes_exactly_two_profiles_and_sql_parts() {
        let Command::Diff(args) = parse_command(&[
            "diff".to_string(),
            "--profile".to_string(),
            "dev".to_string(),
            "--profile".to_string(),
            "prod-copy".to_string(),
            "SELECT".to_string(),
            "1".to_string(),
            "--json".to_string(),
        ])
        .unwrap() else {
            panic!("diff 应产出 Diff 命令");
        };
        assert_eq!(args.profiles, vec!["dev", "prod-copy"]);
        assert_eq!(args.sql.as_deref(), Some("SELECT 1"));
        assert!(args.json);
        assert_eq!(args.max_rows, DEFAULT_MAX_ROWS);
    }

    #[test]
    fn parse_diff_rejects_wrong_profile_count_and_missing_sql() {
        // 缺动作回用法
        assert!(parse_command(&["diff".to_string()]).is_err());
        // 只给一个 --profile
        let err = parse_command(&[
            "diff".to_string(),
            "--profile".to_string(),
            "dev".to_string(),
            "SELECT 1".to_string(),
        ])
        .unwrap_err();
        assert!(err.contains("两个"), "单 profile 应报错：{}", err);
        // 三个 --profile
        let err = parse_command(&[
            "diff".to_string(),
            "--profile".to_string(),
            "a".to_string(),
            "--profile".to_string(),
            "b".to_string(),
            "--profile".to_string(),
            "c".to_string(),
            "SELECT 1".to_string(),
        ])
        .unwrap_err();
        assert!(err.contains("两个"), "三 profile 应报错：{}", err);
        // 缺 SQL
        let err = parse_command(&[
            "diff".to_string(),
            "--profile".to_string(),
            "a".to_string(),
            "--profile".to_string(),
            "b".to_string(),
        ])
        .unwrap_err();
        assert!(err.contains("SQL"), "缺 SQL 应报错：{}", err);
        // 未知 flag
        let err = parse_command(&[
            "diff".to_string(),
            "--profile".to_string(),
            "a".to_string(),
            "--profile".to_string(),
            "b".to_string(),
            "--foo".to_string(),
            "SELECT 1".to_string(),
        ])
        .unwrap_err();
        assert!(err.contains("--foo"), "未知 flag 应报错：{}", err);
    }
}
