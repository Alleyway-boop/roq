//! roq —— 只读 MySQL 查询工具（read-only query）。
//!
//! 三层只读保障：
//! 1. 本地语句闸门：只放行单条只读语句（SELECT/SHOW/EXPLAIN/DESC/DESCRIBE/WITH/TABLE/VALUES），
//!    拒绝多语句、块注释（防 /*! */ 版本注释注入）、EXPLAIN ANALYZE（会真实执行），
//!    并对 SELECT/WITH/TABLE/VALUES 全句扫描写/管理类关键词（词边界：update_time 不误伤）。
//! 2. 服务端会话只读：连接后执行 SET SESSION TRANSACTION READ ONLY（仅会话状态，不改任何数据），
//!    即使闸门被绕过，写入也会被服务器拒绝；另设 30 秒 SELECT 执行上限。
//! 3. 输出限额：行数与单元格长度封顶，避免大结果刷屏。
//!
//! 退出码：0 成功；1 用法/配置错误；2 语句被闸门拒绝；3 连接/执行错误。

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use mysql::prelude::Queryable;
use mysql::{Conn, Opts, OptsBuilder, SslOpts, Value};

/// 语句首关键词白名单（全大写比较）。
const LEADING_ALLOWED: &[&str] = &[
    "SELECT", "SHOW", "EXPLAIN", "DESCRIBE", "DESC", "WITH", "TABLE", "VALUES",
];

/// 全句扫描禁词：写/管理/文件/锁类。词边界匹配，`update_time`、`create_time` 等列名不误伤。
const FORBIDDEN_TOKENS: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "REPLACE", "CREATE", "ALTER", "DROP", "TRUNCATE", "RENAME",
    "GRANT", "REVOKE", "LOCK", "UNLOCK", "CALL", "SET", "LOAD", "HANDLER", "DO", "INTO",
    "OUTFILE", "DUMPFILE", "KILL", "SHUTDOWN", "PREPARE", "EXECUTE", "DEALLOCATE", "SIGNAL",
    "RESIGNAL", "RESET", "PURGE", "ANALYZE", "OPTIMIZE", "REPAIR", "FLUSH", "INSTALL",
    "UNINSTALL", "IMPORT", "BINLOG", "CACHE", "START", "STOP", "XA", "SAVEPOINT", "ROLLBACK",
    "COMMIT", "CHANGE", "LOAD_FILE", "GET_LOCK", "RELEASE_LOCK", "BENCHMARK", "SLEEP",
];

const DEFAULT_MAX_ROWS: usize = 500;
const DEFAULT_MAX_CELL: usize = 200;
const MAX_SQL_LEN: usize = 100_000;
const CONNECT_TIMEOUT_SECS: u64 = 8;
const READ_TIMEOUT_SECS: u64 = 60;
/// 服务端 SELECT 执行时间上限（毫秒），防慢查询拖垮库。
const MAX_EXECUTION_TIME_MS: u32 = 30_000;

struct Profile {
    host: String,
    port: u16,
    user: String,
    password: String,
    database: String,
    /// 是否启用 TLS（RDS 部分端点不宣告 TLS 能力，强制启用会握手失败）。
    ssl: bool,
    /// 来源配置文件路径（多文件扫描时用于 --list 展示与冲突定位）。
    source: String,
}

struct Cli {
    profile: String,
    sql: Option<String>,
    max_rows: usize,
    max_cell: usize,
    list: bool,
    config: Option<PathBuf>,
}

fn usage() -> String {
    [
        "roq —— 只读 MySQL 查询工具".to_string(),
        String::new(),
        "用法: roq [--profile 名] [--max-rows N] [--max-cell N] [--config 路径] [--list] \"SQL语句\"".to_string(),
        "      roq --help".to_string(),
        String::new(),
        "  --profile, -p   连接配置名（默认 dev；生产库请显式 --profile prod）".to_string(),
        "  --max-rows      最多输出行数（默认 500）".to_string(),
        "  --max-cell      单元格最大字符数（默认 200，超出截断）".to_string(),
        "  --config        配置文件或目录（目录=扫描其中 *.conf；默认 ~/.roq/profiles.conf + profiles.d/*.conf）".to_string(),
        "  --list          列出可用配置名".to_string(),
        String::new(),
        "仅接受单条只读语句：SELECT / SHOW / EXPLAIN / DESC / DESCRIBE / WITH / TABLE / VALUES".to_string(),
        "输出为 TSV（制表符分隔）；NULL 显示为 \\N；控制字符替换为空格。".to_string(),
    ]
    .join("\n")
}

fn parse_args(args: &[String]) -> Result<Cli, String> {
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

fn default_config_base() -> Result<PathBuf, String> {
    Ok(home_dir()?.join(".roq"))
}

/// 用户主目录：优先 HOME（Unix 惯例），回退 USERPROFILE（Windows）。
fn home_dir() -> Result<PathBuf, String> {
    env::var("HOME")
        .or_else(|_| env::var("USERPROFILE"))
        .map(PathBuf::from)
        .map_err(|_| "无法确定用户主目录（HOME / USERPROFILE 均未设置）".to_string())
}

/// 解析极简 INI：[节名] + key=value。值不支持换行；# 或 ; 开头为注释行。
fn load_profiles(path: &PathBuf) -> Result<HashMap<String, Profile>, String> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("读取配置 {} 失败：{}", path.display(), e))?;
    let mut sections: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            current = Some(line[1..line.len() - 1].trim().to_string());
            sections.entry(current.clone().unwrap()).or_default();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("配置行无法解析（应为 key=value）：{}", line));
        };
        let section = current.as_deref().ok_or("键值对出现在任何 [节] 之前")?;
        sections
            .get_mut(section)
            .ok_or_else(|| format!("节 {} 不存在", section))?
            .push((key.trim().to_lowercase(), value.trim().to_string()));
    }
    let mut profiles = HashMap::new();
    for (name, kv) in sections {
        let get = |key: &str| -> Result<String, String> {
            kv.iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .ok_or_else(|| format!("配置 [{}] 缺少 {}", name, key))
        };
        let port: u16 = match kv.iter().find(|(k, _)| k == "port") {
            Some((_, value)) => value.parse().map_err(|_| "port 需为整数".to_string())?,
            None => 3306,
        };
        let profile = Profile {
            host: get("host")?,
            port,
            user: get("user")?,
            password: get("password")?,
            database: get("database")?,
            ssl: kv.iter().any(|(k, v)| k == "ssl" && v.eq_ignore_ascii_case("true")),
            source: path.display().to_string(),
        };
        if profiles.contains_key(&name) {
            return Err(format!("配置 [{}] 在 {} 中重复定义", name, path.display()));
        }
        profiles.insert(name, profile);
    }
    Ok(profiles)
}

/// 目录内按文件名排序的 *.conf 清单（跳过子目录与点开头文件）。
/// 排序保证多文件加载顺序稳定，便于冲突报错可复现。
fn list_conf_files(dir: &PathBuf) -> Result<Vec<PathBuf>, String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("读取目录 {} 失败：{}", dir.display(), e))?;
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| format!("遍历目录 {} 失败：{}", dir.display(), e))?.path();
        let is_conf = path
            .extension()
            .map(|ext| ext.eq_ignore_ascii_case("conf"))
            .unwrap_or(false);
        let is_hidden = path
            .file_name()
            .map(|name| name.to_string_lossy().starts_with('.'))
            .unwrap_or(true);
        if path.is_file() && is_conf && !is_hidden {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// 合并一批 profile；同名冲突即报错，避免不同项目的配置互相静默遮蔽。
fn merge_profiles(
    target: &mut HashMap<String, Profile>,
    incoming: HashMap<String, Profile>,
    file: &PathBuf,
) -> Result<(), String> {
    for (name, profile) in incoming {
        if let Some(existing) = target.get(&name) {
            return Err(format!(
                "配置冲突：[{}] 同时定义于 {} 与 {}",
                name,
                existing.source,
                file.display()
            ));
        }
        target.insert(name, profile);
    }
    Ok(())
}

/// 收集配置来源并加载全部 profile。
/// --config 指定文件或目录（目录则扫描其中 *.conf，一项目一文件）；
/// 默认加载 ~/.roq/profiles.conf（存在时）+ ~/.roq/profiles.d/*.conf（存在时）。
fn load_config_sources(cli: &Cli) -> Result<HashMap<String, Profile>, String> {
    let mut sources: Vec<PathBuf> = Vec::new();
    match &cli.config {
        Some(path) => {
            if !path.exists() {
                return Err(format!("--config 路径不存在：{}", path.display()));
            }
            sources.push(path.clone());
        }
        None => {
            let base = default_config_base()?;
            let single = base.join("profiles.conf");
            if single.is_file() {
                sources.push(single);
            }
            let dir = base.join("profiles.d");
            if dir.is_dir() {
                sources.push(dir);
            }
        }
    }
    if sources.is_empty() {
        let base = default_config_base()?;
        return Err(format!(
            "未找到任何配置。可创建 {} 或 {} 下的 *.conf（一项目一文件），或用 --config 指定文件/目录。",
            base.join("profiles.conf").display(),
            base.join("profiles.d").display()
        ));
    }
    let mut profiles: HashMap<String, Profile> = HashMap::new();
    for source in sources {
        if source.is_dir() {
            for file in list_conf_files(&source)? {
                merge_profiles(&mut profiles, load_profiles(&file)?, &file)?;
            }
        } else {
            merge_profiles(&mut profiles, load_profiles(&source)?, &source)?;
        }
    }
    Ok(profiles)
}

/// 提取语句中的词元（含引号/反引号内文本——宁可误拒，不可漏放）。
/// 仅保留含字母的词元，纯数字跳过。
fn extract_tokens(sql: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let flush = |current: &mut String, tokens: &mut Vec<String>| {
        if current.chars().any(|c| c.is_ascii_alphabetic()) {
            tokens.push(std::mem::take(current));
        } else {
            current.clear();
        }
    };
    for ch in sql.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch.to_ascii_uppercase());
        } else if !current.is_empty() {
            flush(&mut current, &mut tokens);
        }
    }
    if !current.is_empty() {
        flush(&mut current, &mut tokens);
    }
    tokens
}

/// 引号/注释感知扫描：剥离 `-- ` 与 `#` 行注释（返回的语句语义等价、无注释）；
/// 字符串与注释之外的 `;` 视为多语句注入；引号未闭合直接拒绝（fail-closed）。
/// 说明：MySQL 行注释要求 `--` 后跟空白或行尾；反斜杠转义在单/双引号内生效，反引号内不生效。
fn strip_and_check(sql: &str) -> Result<String, String> {
    #[derive(PartialEq)]
    enum ScanState {
        Normal,
        Single,
        Double,
        Backtick,
        LineComment,
    }
    let chars: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut state = ScanState::Normal;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match state {
            ScanState::Normal => match c {
                '\'' => {
                    state = ScanState::Single;
                    out.push(c);
                }
                '"' => {
                    state = ScanState::Double;
                    out.push(c);
                }
                '`' => {
                    state = ScanState::Backtick;
                    out.push(c);
                }
                '#' => state = ScanState::LineComment,
                '-' if i + 2 <= chars.len()
                    && chars.get(i + 1) == Some(&'-')
                    && chars.get(i + 2).map_or(true, |next| next.is_whitespace()) =>
                {
                    state = ScanState::LineComment;
                    i += 1;
                }
                ';' => return Err("检测到多条语句（分号）；仅允许单条只读语句".to_string()),
                _ => out.push(c),
            },
            ScanState::Single | ScanState::Double => {
                out.push(c);
                let quote = if state == ScanState::Single { '\'' } else { '"' };
                if c == '\\' && i + 1 < chars.len() {
                    out.push(chars[i + 1]);
                    i += 1;
                } else if c == quote {
                    state = ScanState::Normal;
                }
            }
            ScanState::Backtick => {
                out.push(c);
                if c == '`' {
                    state = ScanState::Normal;
                }
            }
            ScanState::LineComment => {
                if c == '\n' {
                    state = ScanState::Normal;
                    out.push(c);
                }
            }
        }
        i += 1;
    }
    if matches!(state, ScanState::Single | ScanState::Double | ScanState::Backtick) {
        return Err("引号未闭合".to_string());
    }
    Ok(out)
}

/// 本地只读闸门：通过则返回规范化 SQL（已剥离行注释），否则返回拒绝原因。
fn gate(sql_raw: &str) -> Result<String, String> {
    let mut sql = sql_raw.trim().to_string();
    if sql.is_empty() {
        return Err("空语句".to_string());
    }
    if sql.len() > MAX_SQL_LEN {
        return Err(format!("语句超过 {} 字符上限", MAX_SQL_LEN));
    }
    if sql.contains('\0') {
        return Err("包含 NUL 字符".to_string());
    }
    if sql.contains("/*") || sql.contains("*/") {
        return Err("包含块注释 /* */（版本注释可执行代码，一律拒绝）；请移除后重试".to_string());
    }
    // 仅允许去掉一个行尾分号；其余分号交由扫描器按引号/注释上下文判定。
    if sql.ends_with(';') {
        sql.pop();
        sql = sql.trim_end().to_string();
    }
    let sql = strip_and_check(&sql)?;
    let tokens = extract_tokens(&sql);
    let Some(first) = tokens.first() else {
        return Err("未识别到任何语句关键词".to_string());
    };
    if !LEADING_ALLOWED.contains(&first.as_str()) {
        return Err(format!(
            "首关键词 {} 不在只读白名单（SELECT/SHOW/EXPLAIN/DESC/DESCRIBE/WITH/TABLE/VALUES）内",
            first
        ));
    }
    // EXPLAIN ANALYZE 会真实执行底层语句，明确拒绝。
    if first == "EXPLAIN" && tokens.get(1).map(String::as_str) == Some("ANALYZE") {
        return Err("EXPLAIN ANALYZE 会真实执行底层语句，已拒绝".to_string());
    }
    // SHOW/EXPLAIN/DESC 后接对象名或执行计划，跳过禁词扫描（如 SHOW CREATE TABLE）；
    // SELECT/WITH/TABLE/VALUES 走全句禁词扫描（WITH 后可接 UPDATE/DELETE，必须查）。
    if matches!(first.as_str(), "SELECT" | "WITH" | "TABLE" | "VALUES") {
        for token in &tokens {
            if FORBIDDEN_TOKENS.contains(&token.as_str()) {
                return Err(format!("语句包含写/管理类关键词 {}；本工具仅允许只读查询", token));
            }
        }
    }
    Ok(sql)
}

/// 单元格渲染：NULL -> \N；控制字符替换为空格；超长截断。
fn render_cell(value: Value, max_cell: usize) -> String {
    let raw = match value {
        Value::NULL => "\\N".to_string(),
        Value::Int(v) => v.to_string(),
        Value::UInt(v) => v.to_string(),
        Value::Float(v) => v.to_string(),
        Value::Double(v) => v.to_string(),
        Value::Bytes(bytes) => String::from_utf8_lossy(&bytes).to_string(),
        other => other.as_sql(true),
    };
    // 控制字符转义为可见序列（保真，便于离档复查），保持 TSV 单行单元格。
    let clean: String = raw
        .chars()
        .flat_map(|c| match c {
            '\t' => "\\t".chars().collect::<Vec<char>>(),
            '\r' => "\\r".chars().collect::<Vec<char>>(),
            '\n' => "\\n".chars().collect::<Vec<char>>(),
            _ => vec![c],
        })
        .collect();
    if clean.chars().count() <= max_cell {
        clean
    } else {
        let cut: String = clean.chars().take(max_cell).collect();
        format!("{}…", cut)
    }
}

/// 本地时间戳（ISO 8601 含毫秒与时区）。
fn now_local() -> String {
    chrono::Local::now()
        .format("%Y-%m-%dT%H:%M:%S%.3f%:z")
        .to_string()
}

/// JSON 字符串转义（控制字符、引号、反斜杠；非 ASCII 原样保留 UTF-8）。
fn json_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    for c in raw.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// 组装一条审计日志（JSONL 一行）。extra 形如 `,"reason":"..."`，可为空。
fn audit_line(
    profile: &str,
    db: &str,
    sql: &str,
    rows: usize,
    truncated: bool,
    ms: u128,
    outcome: &str,
    extra: &str,
    result_file: &str,
) -> String {
    format!(
        "{{\"ts\":\"{}\",\"profile\":\"{}\",\"db\":\"{}\",\"sql\":\"{}\",\"rows\":{},\"truncated\":{},\"ms\":{},\"outcome\":\"{}\"{},\"result\":\"{}\"}}",
        now_local(),
        json_escape(profile),
        json_escape(db),
        json_escape(sql),
        rows,
        truncated,
        ms,
        outcome,
        extra,
        json_escape(result_file)
    )
}

/// 追加审计日志到 ~/.roq/logs/roq-YYYYMM.jsonl（按月分文件）。
/// 尽力而为：写入失败仅告警，绝不影响查询本身。
fn append_audit_log(entry: &str) {
    let dir = match default_config_base() {
        Ok(base) => base.join("logs"),
        Err(_) => return,
    };
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("[roq] 警告：审计日志目录创建失败（{}）", e);
        return;
    }
    let file = dir.join(chrono::Local::now().format("roq-%Y%m.jsonl").to_string());
    let mut f = match fs::OpenOptions::new().create(true).append(true).open(&file) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[roq] 警告：审计日志写入失败（{}）", e);
            return;
        }
    };
    if let Err(e) = std::io::Write::write_all(&mut f, format!("{}\n", entry).as_bytes()) {
        eprintln!("[roq] 警告：审计日志写入失败（{}）", e);
    }
}

/// 查询结果全文存档：<当前项目>\.roq\results\YYYYMMDD\HHMMSS-毫秒-<profile>.tsv
/// （随项目走，gitignore 加一行 `.roq/` 即可忽略）；当前目录是家目录或不可写时
/// 退回 ~/.roq/results/。返回路径；失败返回 None（仅影响存档，不影响查询结果输出）。
fn save_result(profile: &str, lines: &[String]) -> Option<String> {
    let day = chrono::Local::now().format("%Y%m%d").to_string();
    let stamp = chrono::Local::now().format("%H%M%S-%3f").to_string();
    // 候选目录：项目目录优先，工具目录兜底。
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = env::current_dir() {
        let is_home = home_dir().map(|h| h == cwd).unwrap_or(false);
        if !is_home {
            candidates.push(cwd.join(".roq").join("results"));
        }
    }
    if let Ok(base) = default_config_base() {
        candidates.push(base.join("results"));
    }
    for root in candidates {
        let dir = root.join(&day);
        if fs::create_dir_all(&dir).is_err() {
            continue;
        }
        let path = dir.join(format!("{}-{}.tsv", stamp, profile));
        if fs::write(&path, format!("{}\n", lines.join("\n"))).is_ok() {
            return Some(path.display().to_string());
        }
    }
    None
}

fn run() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let cli = match parse_args(&args) {
        Ok(cli) => cli,
        Err(msg) => {
            if msg.starts_with("roq") {
                println!("{}", msg);
                return ExitCode::from(0);
            }
            eprintln!("用法错误：{}", msg);
            return ExitCode::from(1);
        }
    };
    let profiles = match load_config_sources(&cli) {
        Ok(profiles) => profiles,
        Err(msg) => {
            eprintln!("{}", msg);
            return ExitCode::from(1);
        }
    };
    if cli.list {
        let mut entries: Vec<(&String, &Profile)> = profiles.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        println!("可用配置（{}）：", entries.len());
        for (name, profile) in entries {
            println!("  {:<12} <- {}", name, profile.source);
        }
        return ExitCode::from(0);
    }
    let Some(sql) = cli.sql else {
        eprintln!("缺少 SQL 语句。运行 roq --help 查看用法。");
        return ExitCode::from(1);
    };
    let sql = match gate(&sql) {
        Ok(sql) => sql,
        Err(reason) => {
            eprintln!("[roq 闸门拒绝] {}", reason);
            append_audit_log(&audit_line(
                &cli.profile,
                "",
                &sql,
                0,
                false,
                0,
                "gate-rejected",
                &format!(",\"reason\":\"{}\"", json_escape(&reason)),
                "",
            ));
            return ExitCode::from(2);
        }
    };
    let start = Instant::now();
    let Some(profile) = profiles.get(&cli.profile) else {
        eprintln!("配置 [{}] 不存在。--list 查看全部。", cli.profile);
        append_audit_log(&audit_line(
            &cli.profile,
            "",
            &sql,
            0,
            false,
            start.elapsed().as_millis(),
            "config-error",
            ",\"reason\":\"profile-missing\"",
            "",
        ));
        return ExitCode::from(1);
    };
    if cli.profile == "prod" {
        eprintln!("[roq] 注意：正在查询生产库 {}（只读会话）", profile.database);
    }
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
    let opts = Opts::from(builder);
    let mut conn = match Conn::new(opts) {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("[roq] 连接失败：{}", e);
            append_audit_log(&audit_line(
                &cli.profile,
                &profile.database,
                &sql,
                0,
                false,
                start.elapsed().as_millis(),
                "connect-error",
                &format!(",\"reason\":\"{}\"", json_escape(&e.to_string())),
                "",
            ));
            return ExitCode::from(3);
        }
    };
    // 第二层防护：会话级只读。失败即中止（不允许降级为可写会话）。
    if let Err(e) = conn.query_drop("SET SESSION TRANSACTION READ ONLY") {
        eprintln!("[roq] 设置会话只读失败（中止查询）：{}", e);
        append_audit_log(&audit_line(
            &cli.profile,
            &profile.database,
            &sql,
            0,
            false,
            start.elapsed().as_millis(),
            "connect-error",
            &format!(",\"reason\":\"session-readonly: {}\"", json_escape(&e.to_string())),
            "",
        ));
        return ExitCode::from(3);
    }
    // 回读断言：防止代理/内核静默忽略 SET，导致会话实际可写。
    let readonly_flag: Option<String> = match conn.query_first("SELECT @@session.transaction_read_only") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[roq] 回读只读标志失败（中止查询）：{}", e);
            append_audit_log(&audit_line(
                &cli.profile,
                &profile.database,
                &sql,
                0,
                false,
                start.elapsed().as_millis(),
                "connect-error",
                &format!(",\"reason\":\"verify-readonly: {}\"", json_escape(&e.to_string())),
                "",
            ));
            return ExitCode::from(3);
        }
    };
    if readonly_flag.as_deref() != Some("1") {
        eprintln!("[roq] 服务端确认会话非只读（值={:?}），中止查询", readonly_flag);
        append_audit_log(&audit_line(
            &cli.profile,
            &profile.database,
            &sql,
            0,
            false,
            start.elapsed().as_millis(),
            "connect-error",
            ",\"reason\":\"verify-readonly: flag!=1\"",
            "",
        ));
        return ExitCode::from(3);
    }
    // 慢查询保险丝：超时上限。个别内核不支持该变量时仅告警不阻断。
    if let Err(e) = conn.query_drop(format!("SET SESSION max_execution_time={}", MAX_EXECUTION_TIME_MS)) {
        eprintln!("[roq] 警告：未能设置 max_execution_time（{}）", e);
    }
    let mut result = match conn.query_iter(&sql) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("[roq] 执行失败：{}", e);
            append_audit_log(&audit_line(
                &cli.profile,
                &profile.database,
                &sql,
                0,
                false,
                start.elapsed().as_millis(),
                "exec-error",
                &format!(",\"reason\":\"{}\"", json_escape(&e.to_string())),
                "",
            ));
            return ExitCode::from(3);
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
    // 结果全文同步收集，结束时存档到 ~/.roq/results/（与终端所见一致，含截断）。
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
                let partial = save_result(&cli.profile, &lines).unwrap_or_default();
                append_audit_log(&audit_line(
                    &cli.profile,
                    &profile.database,
                    &sql,
                    rows_out,
                    false,
                    start.elapsed().as_millis(),
                    "exec-error",
                    &format!(",\"reason\":\"read-row: {}\"", json_escape(&e.to_string())),
                    &partial,
                ));
                return ExitCode::from(3);
            }
        };
        let cells: Vec<String> = row
            .unwrap()
            .into_iter()
            .map(|value| render_cell(value, cli.max_cell))
            .collect();
        let line = cells.join("\t");
        let _ = writeln!(out, "{}", line);
        lines.push(line);
        rows_out += 1;
    }
    if result.next().is_some() {
        truncated = true;
    }
    let _ = out.flush();
    let result_file = save_result(&cli.profile, &lines).unwrap_or_default();
    eprintln!(
        "[roq] profile={} db={} 行数={}{} 耗时={}ms（会话只读）存档={}",
        cli.profile,
        profile.database,
        rows_out,
        if truncated { "（已达上限，结果被截断）" } else { "" },
        start.elapsed().as_millis(),
        if result_file.is_empty() { "失败" } else { &result_file }
    );
    append_audit_log(&audit_line(
        &cli.profile,
        &profile.database,
        &sql,
        rows_out,
        truncated,
        start.elapsed().as_millis(),
        "ok",
        "",
        &result_file,
    ));
    ExitCode::from(0)
}

fn main() -> ExitCode {
    run()
}
