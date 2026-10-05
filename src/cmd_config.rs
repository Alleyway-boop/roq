//! `roq config` 子命令：配置的增删测——AI 与人工都无需手写 INI。
//!
//! add 默认一配置一文件（~/.roq/profiles.d/<名>.conf），同名拒绝覆盖、写后回读校验；
//! remove 仅当目标文件只含该配置节时才删整个文件，且须 --yes，多节文件提示手动编辑。

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use mysql::prelude::Queryable;
use mysql::Conn;

use crate::cli::ConfigCmd;
use crate::config::{default_config_base, load_config_sources, load_profiles, print_profiles};
use crate::query::build_opts;

pub fn run(cmd: ConfigCmd) -> ExitCode {
    let result = match cmd {
        ConfigCmd::List => {
            match load_config_sources(None) {
                Ok(profiles) => {
                    print_profiles(&profiles);
                    return ExitCode::from(0);
                }
                Err(msg) => Err(msg),
            }
        }
        // 其余分支返回 Result<String, String>，统一在下方出口打印/退出
        ConfigCmd::Add { name, host, port, user, password, database, ssl, file } => {
            add(&name, &host, port, &user, &password, &database, ssl, file)
        }
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
    password: &str,
    database: &str,
    ssl: bool,
    file: Option<PathBuf>,
) -> Result<String, String> {
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
        "[{}]\nhost={}\nport={}\nuser={}\npassword={}\ndatabase={}{}\n",
        name,
        host,
        port,
        user,
        password,
        database,
        if ssl { "\nssl=true" } else { "" }
    );
    if target.exists() {
        let existing = load_profiles(&target)?;
        if existing.contains_key(name) {
            return Err(format!(
                "[{}] 已存在于 {}；如需修改请手动编辑该文件",
                name,
                target.display()
            ));
        }
        let mut text = fs::read_to_string(&target)
            .map_err(|e| format!("读取 {} 失败：{}", target.display(), e))?;
        text = text.trim_end().to_string();
        text.push_str("\n\n");
        text.push_str(&section);
        fs::write(&target, text).map_err(|e| format!("写入 {} 失败：{}", target.display(), e))?;
    } else {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{}", e))?;
        }
        let header = "# roq 连接配置（roq config add 生成；含凭据，勿提交勿外传）\n";
        fs::write(&target, format!("{}{}", header, section))
            .map_err(|e| format!("写入 {} 失败：{}", target.display(), e))?;
    }
    // 写后回读校验，确保落盘内容可被正常加载
    load_profiles(&target)?
        .get(name)
        .ok_or_else(|| "写入后回读校验失败".to_string())?;
    Ok(format!("[{}] 已写入 {}（回读校验通过）", name, target.display()))
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
            return Ok(format!("已删除 {}（含唯一配置 [{}]）", source.display(), name));
        }
    }
    Err(format!("未找到 [{}]", name))
}

fn test(name: &str) -> Result<String, String> {
    let profiles = load_config_sources(None)?;
    let profile = profiles
        .get(name)
        .ok_or_else(|| format!("[{}] 不存在；roq config list 查看全部", name))?;
    let mut conn = Conn::new(build_opts(profile))
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
        let msg = add("demo", "h", 3307, "u", "p", "d", true, Some(path.clone())).unwrap();
        assert!(msg.contains("回读校验通过"));
        let profiles = load_profiles(&path).unwrap();
        assert_eq!(profiles["demo"].port, 3307);
        assert!(profiles["demo"].ssl);
        let err = add("demo", "h2", 3306, "u2", "p2", "d2", false, Some(path.clone())).unwrap_err();
        assert!(err.contains("已存在"));
        // 追加第二个配置到同一文件：既有内容与注释保留
        add("other", "h2", 3306, "u2", "p2", "d2", false, Some(path.clone())).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("# roq 连接配置"));
        let profiles = load_profiles(&path).unwrap();
        assert_eq!(profiles.len(), 2);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn add_rejects_unsafe_default_filename() {
        let err = add("bad/name", "h", 3306, "u", "p", "d", false, None).unwrap_err();
        assert!(err.contains("仅允许"));
    }

    #[test]
    fn remove_needs_yes_and_refuses_multi_section_file() {
        let path = temp_conf("rm");
        let _ = fs::remove_file(&path);
        add("solo", "h", 3306, "u", "p", "d", false, Some(path.clone())).unwrap();
        let err = remove("solo", Some(path.clone()), false).unwrap_err();
        assert!(err.contains("--yes"));
        remove("solo", Some(path.clone()), true).unwrap();
        assert!(!path.exists());

        add("a1", "h", 3306, "u", "p", "d", false, Some(path.clone())).unwrap();
        add("a2", "h", 3306, "u", "p", "d", false, Some(path.clone())).unwrap();
        let err = remove("a1", Some(path.clone()), true).unwrap_err();
        assert!(err.contains("手动编辑"));
        let _ = fs::remove_file(&path);
    }
}
