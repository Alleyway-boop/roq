//! roq CLI 入口：参数解析 → 配置加载 → 只读闸门 → 执行。
//! 三层只读保障与模块划分见 lib.rs。

use std::env;
use std::process::ExitCode;

use roq::audit::{append_audit_log, audit_line, json_escape};
use roq::cli::{parse_command, Cli, Command};
use roq::cmd_config;
use roq::cmd_diff;
use roq::cmd_log;
use roq::cmd_q;
use roq::cmd_skill;
use roq::config::{load_config_sources, print_profiles};
use roq::gate::gate;
use roq::query;
use roq::saved_query::{
    load_template, params_json_loose, prepare_execution, saved_audit_extra, SavedContext,
    QUERY_FILE_HEADER,
};

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
        Command::Q(qcmd) => cmd_q::run(qcmd),
        Command::SavedQuery { name, params, cli } => run_saved_query(&name, &params, cli),
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
    query::execute(&cli, profile, &sql, None)
}

/// 保存查询执行链路（`roq q <名> [k=v ...]`）：顺序镜像 run_query——配置 → 模板 →
/// 参数替换 → 只读闸门 → profile 查找。闸门作用于参数替换后的 SQL（保存 ≠ 永久放行，
/// 红线 5），每条失败路径的审计都带 saved 上下文，reason 恒居 extra 尾部：
/// - 查询名不存在 → param-error（sql=""，extra=saved+reason）
/// - 参数替换失败 → param-error（sql=模板原文，extra=saved+params+reason）
/// - 闸门拒绝     → gate-rejected（sql=替换后 SQL，extra=saved+params+reason），exit 2
/// - profile 不存在 → config-error（sql=规范化 SQL，extra=saved+params）
fn run_saved_query(name: &str, param_args: &[String], cli: Cli) -> ExitCode {
    let profiles = match load_config_sources(cli.config.as_ref()) {
        Ok(profiles) => profiles,
        Err(msg) => {
            eprintln!("{}", msg);
            return ExitCode::from(EXIT_USAGE);
        }
    };
    // 失败审计统一出口：库未可知记空串；config-error 需计时，在调用处直接走 audit_line。
    let audit_failure = |sql: &str, outcome: &str, extra: &str| {
        append_audit_log(&audit_line(
            &cli.profile, "", sql, 0, false, 0, outcome, extra, "",
        ));
    };
    let template = match load_template(name) {
        Ok(template) => template,
        Err(reason) => {
            // 查询名不存在：参数尚未解析，extra 仅 saved + reason。
            eprintln!("[roq] {}", reason);
            audit_failure("", "param-error", &saved_audit_extra(name, "{}", Some(&reason)));
            return ExitCode::from(EXIT_USAGE);
        }
    };
    // 头注释是文档不是数据：剥掉写入时置顶的文件头再进替换引擎；
    // 手工编辑删了头（无前缀）不算错误，按原样继续。
    let template = template
        .strip_prefix(QUERY_FILE_HEADER)
        .unwrap_or(&template)
        .to_string();
    let (sql, params_json) = match prepare_execution(&template, param_args) {
        Ok(result) => result,
        Err(reason) => {
            // 缺/多/重参、值非法、模板坏形：sql 记模板原文，params 尽力保留可解析部分。
            eprintln!("[roq] {}", reason);
            let extra = saved_audit_extra(name, &params_json_loose(param_args), Some(&reason));
            audit_failure(&template, "param-error", &extra);
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let ctx = SavedContext {
        name: name.to_string(),
        params_json,
    };
    // 红线 5：闸门作用于替换后的 SQL，拒绝审计带 saved 上下文。
    let gated = match gate(&sql) {
        Ok(gated) => gated,
        Err(reason) => {
            eprintln!("[roq 闸门拒绝] {}", reason);
            let extra = saved_audit_extra(&ctx.name, &ctx.params_json, Some(&reason));
            audit_failure(&sql, "gate-rejected", &extra);
            return ExitCode::from(EXIT_GATE_REJECTED);
        }
    };
    let start = std::time::Instant::now();
    let Some(profile) = profiles.get(&cli.profile) else {
        eprintln!("配置 [{}] 不存在。--list 查看全部。", cli.profile);
        append_audit_log(&audit_line(
            &cli.profile,
            "",
            &gated,
            0,
            false,
            start.elapsed().as_millis(),
            "config-error",
            &saved_audit_extra(&ctx.name, &ctx.params_json, None),
            "",
        ));
        return ExitCode::from(EXIT_USAGE);
    };
    query::execute(&cli, profile, &gated, Some(&ctx))
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
