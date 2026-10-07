//! roq CLI 入口：参数解析 → 配置加载 → 只读闸门 → 执行。
//! 三层只读保障与模块划分见 lib.rs。

use std::env;
use std::process::ExitCode;

use roq::audit::{append_audit_log, audit_line, json_escape};
use roq::cli::{parse_command, Cli, Command};
use roq::cmd_config;
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
    let Some(sql) = cli.sql.clone() else {
        eprintln!("缺少 SQL 语句。运行 roq --help 查看用法。");
        return ExitCode::from(EXIT_USAGE);
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
            return ExitCode::from(EXIT_GATE_REJECTED);
        }
    };
    let start = std::time::Instant::now();
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
        return ExitCode::from(EXIT_USAGE);
    };
    query::execute(&cli, profile, &sql)
}

fn main() -> ExitCode {
    run()
}
