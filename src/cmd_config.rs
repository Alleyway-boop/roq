//! `roq config` 子命令：配置的增删测——AI 与人工都无需手写 INI。
//!
//! add 默认一配置一文件（~/.roq/profiles.d/<名>.conf），同名拒绝覆盖（重录用 --update），
//! 写入前做跨文件同名预检，写后回读校验；
//! remove 仅当目标文件只含该配置节时才删整个文件，且须 --yes，多节文件提示手动编辑。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use mysql::prelude::Queryable;
use mysql::Conn;

use crate::cli::ConfigCmd;
use crate::config::{default_config_base, load_config_sources, load_profiles, print_profiles};
use crate::query::build_opts;

pub fn run(cmd: ConfigCmd) -> ExitCode {
    let result = match cmd {
        ConfigCmd::List => match load_config_sources(None) {
            Ok(profiles) => {
                print_profiles(&profiles);
                return ExitCode::from(0);
            }
            Err(msg) => Err(msg),
        },
        // 其余分支返回 Result<String, String>，统一在下方出口打印/退出
        ConfigCmd::Add {
            name,
            host,
            port,
            user,
            password,
            password_env,
            database,
            ssl,
            file,
            update,
        } => add(
            &name,
            &host,
            port,
            &user,
            password.as_deref(),
            password_env.as_deref(),
            &database,
            ssl,
            file,
            update,
        ),
        ConfigCmd::Remove { name, file, yes } => remove(&name, file, yes),
        ConfigCmd::Test { name } => test(&name),
    };
    match result {
        Ok(msg) => {
            if !msg.is_empty() {
                println!("{}", msg);
            }
            ExitCode::from(0)
        }
        Err(msg) => {
            eprintln!("[roq config] {}", msg);
            ExitCode::from(1)
        }
    }
}

/// 配置名做默认文件名，必须局限在安全字符集内。
fn validate_name(name: &str) -> Result<(), String> {
    let safe = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if safe {
        Ok(())
    } else {
        Err(format!("配置名 {} 仅允许字母/数字/下划线/连字符", name))
    }
}

fn add(
    name: &str,
    host: &str,
    port: u16,
    user: &str,
    password: Option<&str>,
    password_env: Option<&str>,
    database: &str,
    ssl: bool,
    file: Option<PathBuf>,
    update: bool,
) -> Result<String, String> {
    let sources = crate::config::default_config_files()?;
    add_with_sources(
        name,
        host,
        port,
        user,
        password,
        password_env,
        database,
        ssl,
        file,
        update,
        &sources,
    )
}

/// add 的可测形态：跨文件同名预检的候选源由调用方注入（生产为默认配置清单，
/// 测试经临时文件注入以复现多文件场景）。
fn add_with_sources(
    name: &str,
    host: &str,
    port: u16,
    user: &str,
    password: Option<&str>,
    password_env: Option<&str>,
    database: &str,
    ssl: bool,
    file: Option<PathBuf>,
    update: bool,
    sources: &[PathBuf],
) -> Result<String, String> {
    // 密码两形式互斥：并存说明调用方有歧义，直接拒绝。
    let secret_line = match (password, password_env) {
        (Some(_), Some(_)) => {
            return Err("--password 与 --password-env 二选一，请只提供其一".to_string());
        }
        (Some(plain), _) => format!("password={}\n", plain),
        (None, Some(var)) => format!("password_env={}\n", var),
        (None, None) => return Err("缺少 --password 或 --password-env".to_string()),
    };
    let target = match file {
        Some(path) => path,
        None => {
            validate_name(name)?;
            default_config_base()?
                .join("profiles.d")
                .join(format!("{}.conf", name))
        }
    };
    let section = format!(
        "[{}]\nhost={}\nport={}\nuser={}\n{}database={}{}\n",
        name,
        host,
        port,
        user,
        secret_line,
        database,
        if ssl { "\nssl=true" } else { "" }
    );
    // 跨文件同名预检：同名节只允许存在一处，落在其它文件时无论 update 与否都拒绝
    match locate_existing_section(name, &target, sources)? {
        Some(existing) if existing != target => {
            return Err(format!(
                "[{}] 已定义于 {}；同名配置只允许一处，覆盖请对该文件执行 config add --update --file <该文件>",
                name,
                existing.display()
            ));
        }
        Some(_) if !update => {
            return Err(format!(
                "[{}] 已存在于 {}；如需修改请手动编辑该文件，重录请用 config add --update",
                name,
                target.display()
            ));
        }
        Some(_) => replace_section(&target, &section, name)?,
        None if update => {
            return Err(format!(
                "[{}] 在 {} 中不存在，无法 --update 覆盖；去掉 --update 即为新增，或确认 --file 指向正确文件",
                name,
                target.display()
            ));
        }
        None if target.exists() => {
            // 既有内容须可解析才允许追加，避免把坏文件越写越坏
            load_profiles(&target)?;
            let mut text = fs::read_to_string(&target)
                .map_err(|e| format!("读取 {} 失败：{}", target.display(), e))?;
            text = text.trim_end().to_string();
            text.push_str("\n\n");
            text.push_str(&section);
            fs::write(&target, text)
                .map_err(|e| format!("写入 {} 失败：{}", target.display(), e))?;
        }
        None => {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{}", e))?;
            }
            let header = "# roq 连接配置（roq config add 生成；含凭据，勿提交勿外传）\n";
            fs::write(&target, format!("{}{}", header, section))
                .map_err(|e| format!("写入 {} 失败：{}", target.display(), e))?;
        }
    }
    apply_private_permissions(&target)?;
    // 写后回读校验，确保落盘内容可被正常加载
    load_profiles(&target)?
        .get(name)
        .ok_or_else(|| "写入后回读校验失败".to_string())?;
    Ok(format!(
        "[{}] 已写入 {}（回读校验通过）",
        name,
        target.display()
    ))
}

/// 在目标文件与候选源中定位同名节所在文件（目标优先）。
/// 任何源文件解析失败按“不存在”跳过（与 remove 的容错一致），不阻断预检。
fn locate_existing_section(
    name: &str,
    target: &Path,
    extra_sources: &[PathBuf],
) -> Result<Option<PathBuf>, String> {
    let contains_section = |path: &Path| -> bool {
        load_profiles(&path.to_path_buf())
            .map(|profiles| profiles.contains_key(name))
            .unwrap_or(false)
    };
    if contains_section(target) {
        return Ok(Some(target.to_path_buf()));
    }
    for source in extra_sources {
        if contains_section(source) {
            return Ok(Some(source.clone()));
        }
    }
    Ok(None)
}

/// 用新节文本替换目标文件中的既有同名节：定位 `\n[名]` 行首，截到下一个 `\n[` 或文件尾。
/// 节前既有内容（含文件头注释）原样保留，节与后文之间的空行分隔也随原文保持。
fn replace_section(target: &Path, section: &str, name: &str) -> Result<(), String> {
    let text =
        fs::read_to_string(target).map_err(|e| format!("读取 {} 失败：{}", target.display(), e))?;
    let header = format!("[{}]", name);
    let start = if text.starts_with(&header) {
        0
    } else {
        text.find(&format!("\n{}", header))
            .map(|pos| pos + 1)
            .ok_or_else(|| format!("[{}] 在 {} 中未定位到节文本", name, target.display()))?
    };
    let end = text[start..]
        .find("\n[")
        .map(|rel| start + rel)
        .unwrap_or(text.len());
    let updated = format!("{}{}{}", &text[..start], section, &text[end..]);
    fs::write(target, updated).map_err(|e| format!("写入 {} 失败：{}", target.display(), e))?;
    Ok(())
}

/// 落盘后收紧权限为属主可读写（Unix 0600）；Windows 无对应模式位，交由默认 ACL。
#[cfg(unix)]
fn apply_private_permissions(target: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(target, fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("设置 {} 权限失败：{}", target.display(), e))
}

#[cfg(not(unix))]
fn apply_private_permissions(_target: &Path) -> Result<(), String> {
    Ok(())
}

fn remove(name: &str, file: Option<PathBuf>, yes: bool) -> Result<String, String> {
    let sources: Vec<PathBuf> = match file {
        Some(path) => vec![path],
        None => crate::config::default_config_files()?,
    };
    for source in sources {
        let profiles = match load_profiles(&source) {
            Ok(profiles) => profiles,
            Err(_) => continue,
        };
        if profiles.contains_key(name) {
            if profiles.len() > 1 {
                return Err(format!(
                    "[{}] 所在文件 {} 还包含其它 {} 个配置；为防误删请手动编辑该文件删除此节",
                    name,
                    source.display(),
                    profiles.len() - 1
                ));
            }
            if !yes {
                return Err(format!(
                    "将删除文件 {}（仅含 [{}]）；确认请加 --yes",
                    source.display(),
                    name
                ));
            }
            fs::remove_file(&source)
                .map_err(|e| format!("删除 {} 失败：{}", source.display(), e))?;
            return Ok(format!(
                "已删除 {}（含唯一配置 [{}]）",
                source.display(),
                name
            ));
        }
    }
    Err(format!("未找到 [{}]", name))
}

fn test(name: &str) -> Result<String, String> {
    let profiles = load_config_sources(None)?;
    let profile = profiles
        .get(name)
        .ok_or_else(|| format!("[{}] 不存在；roq config list 查看全部", name))?;
    let password = profile
        .resolved_password()
        .map_err(|e| format!("[{}] {}", name, e))?;
    let mut conn = Conn::new(build_opts(profile, &password))
        .map_err(|e| format!("[{}] 连接失败：{}", name, e))?;
    conn.query_drop("SET SESSION TRANSACTION READ ONLY")
        .map_err(|e| format!("[{}] 设置会话只读失败：{}", name, e))?;
    let flag: Option<String> = conn
        .query_first("SELECT @@session.transaction_read_only")
        .map_err(|e| format!("[{}] 回读只读标志失败：{}", name, e))?;
    if flag.as_deref() != Some("1") {
        return Err(format!("[{}] 服务端确认会话非只读（值={:?}）", name, flag));
    }
    let version: Option<String> = conn
        .query_first("SELECT VERSION()")
        .map_err(|e| format!("[{}] 查询版本失败：{}", name, e))?;
    Ok(format!(
        "[{}] 连接成功 db={} server={} 会话只读=1（来源 {}）",
        name,
        profile.database,
        version.unwrap_or_default(),
        profile.source
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_conf(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("roq-cfg-{}-{}.conf", tag, std::process::id()))
    }

    #[test]
    fn add_creates_parseable_file_and_refuses_duplicate() {
        let path = temp_conf("add");
        let _ = fs::remove_file(&path);
        let msg = add(
            "demo",
            "h",
            3307,
            "u",
            Some("p"),
            None,
            "d",
            true,
            Some(path.clone()),
            false,
        )
        .unwrap();
        assert!(msg.contains("回读校验通过"));
        let profiles = load_profiles(&path).unwrap();
        assert_eq!(profiles["demo"].port, 3307);
        assert!(profiles["demo"].ssl);
        let err = add(
            "demo",
            "h2",
            3306,
            "u2",
            Some("p2"),
            None,
            "d2",
            false,
            Some(path.clone()),
            false,
        )
        .unwrap_err();
        assert!(err.contains("已存在"));
        // 追加第二个配置到同一文件：既有内容与注释保留
        add(
            "other",
            "h2",
            3306,
            "u2",
            Some("p2"),
            None,
            "d2",
            false,
            Some(path.clone()),
            false,
        )
        .unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("# roq 连接配置"));
        let profiles = load_profiles(&path).unwrap();
        assert_eq!(profiles.len(), 2);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn add_rejects_unsafe_default_filename() {
        let err = add(
            "bad/name",
            "h",
            3306,
            "u",
            Some("p"),
            None,
            "d",
            false,
            None,
            false,
        )
        .unwrap_err();
        assert!(err.contains("仅允许"));
    }

    #[test]
    fn add_with_password_env_writes_env_key_without_requiring_env() {
        let path = temp_conf("penv");
        let _ = fs::remove_file(&path);
        add(
            "envy",
            "h",
            3306,
            "u",
            None,
            Some("MY_DB_PWD"),
            "d",
            false,
            Some(path.clone()),
            false,
        )
        .unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("password_env=MY_DB_PWD"));
        assert!(!text.contains("\npassword="));
        // 回读校验不依赖当前 shell 环境（未设置 MY_DB_PWD 也能通过）
        let profiles = load_profiles(&path).unwrap();
        assert_eq!(profiles["envy"].password_env.as_deref(), Some("MY_DB_PWD"));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn add_rejects_both_password_and_password_env() {
        let err = add(
            "both",
            "h",
            3306,
            "u",
            Some("p"),
            Some("V"),
            "d",
            false,
            Some(temp_conf("penv-x")),
            false,
        )
        .unwrap_err();
        assert!(err.contains("二选一"), "报错应说明互斥：{}", err);
    }

    #[test]
    fn update_replaces_section_and_keeps_others() {
        let path = temp_conf("upd");
        let _ = fs::remove_file(&path);
        add(
            "a1",
            "h1",
            3306,
            "u1",
            Some("p1"),
            None,
            "d1",
            false,
            Some(path.clone()),
            false,
        )
        .unwrap();
        add(
            "a2",
            "h2",
            3307,
            "u2",
            Some("p2"),
            None,
            "d2",
            true,
            Some(path.clone()),
            false,
        )
        .unwrap();
        let msg = add(
            "a1",
            "h9",
            3308,
            "u9",
            Some("p9"),
            None,
            "d9",
            true,
            Some(path.clone()),
            true,
        )
        .unwrap();
        assert!(msg.contains("回读校验通过"), "Ok 消息应含回读校验：{}", msg);
        let profiles = load_profiles(&path).unwrap();
        assert_eq!(profiles.len(), 2, "其余节不应丢失");
        assert_eq!(profiles["a1"].host, "h9");
        assert_eq!(profiles["a1"].port, 3308);
        assert!(profiles["a1"].ssl);
        assert_eq!(profiles["a2"].host, "h2", "a2 应保持不变");
        assert_eq!(profiles["a2"].port, 3307);
        assert!(profiles["a2"].ssl);
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# roq 连接配置"),
            "文件头注释应保留：{}",
            text
        );
        assert!(!text.contains("h1"), "旧值应被替换干净：{}", text);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn update_without_existing_section_fails() {
        let path = temp_conf("upd-miss");
        let _ = fs::remove_file(&path);
        let err = add(
            "x",
            "h",
            3306,
            "u",
            Some("p"),
            None,
            "d",
            false,
            Some(path.clone()),
            true,
        )
        .unwrap_err();
        assert!(err.contains("不存在"), "应说明节不存在：{}", err);
        assert!(err.contains("去掉 --update"), "应提示如何新增：{}", err);
        assert!(!path.exists(), "拒绝时不应写出目标文件");
    }

    #[test]
    fn add_rejects_name_existing_in_other_file() {
        let file_a = temp_conf("dup-a");
        let file_b = temp_conf("dup-b");
        let _ = fs::remove_file(&file_a);
        let _ = fs::remove_file(&file_b);
        add(
            "dup",
            "h",
            3306,
            "u",
            Some("p"),
            None,
            "d",
            false,
            Some(file_a.clone()),
            false,
        )
        .unwrap();
        // 预检候选源注入文件 A：向文件 B 新增同名节应拒绝并指向 A
        let err = add_with_sources(
            "dup",
            "h2",
            3306,
            "u2",
            Some("p2"),
            None,
            "d2",
            false,
            Some(file_b.clone()),
            false,
            &[file_a.clone()],
        )
        .unwrap_err();
        assert!(err.contains("已定义于"), "应指出跨文件同名：{}", err);
        assert!(
            err.contains(&file_a.display().to_string()),
            "应指向文件 A 的路径：{}",
            err
        );
        // --update 也一样拒绝：跨文件同名是冲突，不允许静默改到别处
        let err = add_with_sources(
            "dup",
            "h3",
            3306,
            "u3",
            Some("p3"),
            None,
            "d3",
            false,
            Some(file_b.clone()),
            true,
            &[file_a.clone()],
        )
        .unwrap_err();
        assert!(
            err.contains("已定义于"),
            "update 同样受跨文件预检约束：{}",
            err
        );
        assert!(!file_b.exists(), "被拒绝时不应写出文件 B");
        let _ = fs::remove_file(&file_a);
        let _ = fs::remove_file(&file_b);
    }

    #[test]
    fn locate_existing_section_skips_unparseable_sources() {
        let good = temp_conf("loc-good");
        let bad = temp_conf("loc-bad");
        let _ = fs::remove_file(&good);
        fs::write(&bad, "既无节头也非法的垃圾内容\n").unwrap();
        add(
            "here",
            "h",
            3306,
            "u",
            Some("p"),
            None,
            "d",
            false,
            Some(good.clone()),
            false,
        )
        .unwrap();
        // 目标解析失败按不含同名节跳过，候选源命中照常返回
        assert_eq!(
            locate_existing_section("here", &bad, &[good.clone()]).unwrap(),
            Some(good.clone())
        );
        // 全部源都解析失败或不存在时返回 None，不因容错阻断预检
        let nowhere = temp_conf("loc-nowhere");
        let _ = fs::remove_file(&nowhere);
        assert_eq!(
            locate_existing_section("here", &nowhere, &[bad.clone()]).unwrap(),
            None
        );
        let _ = fs::remove_file(&good);
        let _ = fs::remove_file(&bad);
    }

    #[test]
    fn remove_needs_yes_and_refuses_multi_section_file() {
        let path = temp_conf("rm");
        let _ = fs::remove_file(&path);
        add(
            "solo",
            "h",
            3306,
            "u",
            Some("p"),
            None,
            "d",
            false,
            Some(path.clone()),
            false,
        )
        .unwrap();
        let err = remove("solo", Some(path.clone()), false).unwrap_err();
        assert!(err.contains("--yes"));
        remove("solo", Some(path.clone()), true).unwrap();
        assert!(!path.exists());

        add(
            "a1",
            "h",
            3306,
            "u",
            Some("p"),
            None,
            "d",
            false,
            Some(path.clone()),
            false,
        )
        .unwrap();
        add(
            "a2",
            "h",
            3306,
            "u",
            Some("p"),
            None,
            "d",
            false,
            Some(path.clone()),
            false,
        )
        .unwrap();
        let err = remove("a1", Some(path.clone()), true).unwrap_err();
        assert!(err.contains("手动编辑"));
        let _ = fs::remove_file(&path);
    }
}
