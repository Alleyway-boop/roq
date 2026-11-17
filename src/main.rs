//! roq CLI 入口：参数解析 → 配置加载 → 只读闸门 → 执行。
//! 三层只读保障与模块划分见 lib.rs。

use std::env;
use std::process::ExitCode;

use roq::audit::{append_audit_log, audit_line, json_escape};
use roq::cli::{materialize_sql, parse_command, Cli, Command};
use roq::cmd_config;
use roq::cmd_diff;
use roq::cmd_log;
use roq::cmd_skill;
use roq::config::{load_config_sources, print_profiles};
use roq::gate::gate;
use roq::query;

/// 退出码：0 成功；1 用法/配置错误；2 语句被闸门拒绝；3 连接/执行错误。
const EXIT_USAGE: u8 = 1;
const EXIT_GATE_REJECTED: u8 = 2;

fn run() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let command = match parse_command(&args) {
        Ok(command) => command,
        Err(msg) => {
            // --help / --version / config --help 的文本以 "roq" 开头，打印后正常退出
            if msg.starts_with("roq") {
                println!("{}", msg);
                return ExitCode::from(0);
            }
            eprintln!("用法错误：{}", msg);
            return ExitCode::from(EXIT_USAGE);
        }
    };
    match command {
        Command::Config(config_cmd) => cmd_config::run(config_cmd),
        Command::Log(log_args) => cmd_log::run(log_args),
        Command::Skill(skill_cmd) => cmd_skill::run(skill_cmd),
        Command::Diff(diff_args) => cmd_diff::run(diff_args),
        Command::Query(cli) => run_query(cli),
    }
}

fn run_query(cli: Cli) -> ExitCode {
    let profiles = match load_config_sources(cli.config.as_ref()) {
        Ok(profiles) => profiles,
        Err(msg) => {
            eprintln!("{}", msg);
            return ExitCode::from(EXIT_USAGE);
        }
    };
    if cli.list {
        print_profiles(&profiles);
        return ExitCode::from(0);
    }
    // profile 查找前移：快捷子命令的物化与后续闸门的引擎感知都需要 profile
    let start = std::time::Instant::now();
    let Some(profile) = profiles.get(&cli.profile) else {
        eprintln!("配置 [{}] 不存在。--list 查看全部。", cli.profile);
        append_audit_log(&audit_line(
            &cli.profile,
            // profile 不存在，引擎无从得知，记空串（与 db 空串同一约定）
            "",
            "",
            cli.sql.as_deref().unwrap_or(""),
            0,
            false,
            start.elapsed().as_millis(),
            "config-error",
            ",\"reason\":\"profile-missing\"",
            "",
        ));
        return ExitCode::from(EXIT_USAGE);
    };
    // 手写 SQL 原样直取；快捷子命令按 profile 引擎物化为具体 SQL。
    // 物化失败（表名/过滤模式不合法、oracle explain）按用法错误处理并落审计。
    let sql = match &cli.sql {
        Some(sql) => sql.clone(),
        None => match cli.shortcut.as_ref() {
            Some(s) => match materialize_sql(s, profile.engine) {
                Ok(sql) => sql,
                Err(msg) => {
                    eprintln!("[roq] {}", msg);
                    append_audit_log(&audit_line(&cli.profile, profile.engine.as_str(), "", "", 0, false, 0,
                        "config-error", &format!(",\"reason\":\"{}\"", json_escape(&msg)), ""));
                    return ExitCode::from(EXIT_USAGE);
                }
            },
            None => { eprintln!("缺少 SQL 语句。运行 roq --help 查看用法。"); return ExitCode::from(EXIT_USAGE); }
        },
    };
    let sql = match gate(&sql, profile.engine) {
        Ok(sql) => sql,
        Err(reason) => {
            eprintln!("[roq 闸门拒绝] {}", reason);
            append_audit_log(&audit_line(
                &cli.profile,
                profile.engine.as_str(),
                &profile.database,
                &sql,
                0,
                false,
                0,
                "gate-rejected",
                &format!(",\"reason\":\"{}\"", json_escape(&reason)),
                "",
            ));
            return ExitCode::from(EXIT_GATE_REJECTED);
        }
    };
    query::execute(&cli, profile, &sql)
}

fn main() -> ExitCode {
    set_console_utf8();
    run()
}

/// Windows conhost（cmd / 旧版 PowerShell）默认代码页非 UTF-8，中文输出会乱码；
/// 切到 65001。失败静默——管道/重定向不经控制台，不影响数据正确性。
#[cfg(windows)]
fn set_console_utf8() {
    #[link(name = "kernel32")]
    extern "system" {
        fn SetConsoleOutputCP(code_page: u32) -> i32;
    }
    unsafe {
        let _ = SetConsoleOutputCP(65001);
    }
}

#[cfg(not(windows))]
fn set_console_utf8() {}
