//! 连接配置：极简 INI 解析 + 目录扫描（一项目一文件）+ 跨文件同名冲突检测。

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// 密文值前缀：password 以此开头即为 DPAPI 加密密文（base64 编码），写侧（Task 4）复用。
pub const ENC_PREFIX: &str = "enc:v1:dpapi:";

/// 明文密码安全警告文案（纯函数便于测试；eprintln 处只包一层）。
/// 建议按平台编译期分流：Windows 有 --update 重录路径；非 Windows 上
/// --password 一律被拒，--update 是死路，须指向该平台可执行的 password_env。
pub fn plaintext_warning(name: &str) -> String {
    let guidance = if cfg!(windows) {
        "建议 config add --update 重新录入加密"
    } else {
        "建议改用 password_env（或迁至 Windows 上 config add --update 重录）"
    };
    format!("[roq] 警告：profile {name} 密码为明文存储，{guidance}")
}

#[derive(Debug)]
pub struct Profile {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// 明文密码（与 password_env 二选一）。
    pub password: Option<String>,
    /// 密码所在环境变量名（与 password 二选一，凭据不落明文）。
    pub password_env: Option<String>,
    pub database: String,
    /// 是否启用 TLS（RDS 部分端点不宣告 TLS 能力，强制启用会握手失败）。
    pub ssl: bool,
    /// 来源配置文件路径（多文件扫描时用于 --list 展示与冲突定位）。
    pub source: String,
}

impl Profile {
    /// 连接前解析实际密码。`name` 是 profile 名，仅用于明文警告文案。
    /// password_env 存在则读环境变量（未设置即报错，不回退明文）；
    /// password 以 [`ENC_PREFIX`] 开头走 DPAPI 解密，否则视为明文并打印安全警告。
    pub fn resolved_password(&self, name: &str) -> Result<String, String> {
        match (&self.password_env, &self.password) {
            (Some(var), _) => {
                env::var(var).map_err(|_| format!("配置引用的环境变量 {} 未设置", var))
            }
            (None, Some(value)) => match value.strip_prefix(ENC_PREFIX) {
                // base64 解码是平台无关的公共校验，先行报错（坏编码两平台行为一致）
                Some(cipher) => {
                    let blob = crate::b64::decode(cipher)?;
                    decrypt_dpapi_blob(&blob)
                }
                None => {
                    // 安全警告豁免 --quiet：直接 eprintln! 不走 notice 通道——
                    // quiet 的语义是抑制提示类输出，而安全警告不是提示，必须始终可见。
                    eprintln!("{}", plaintext_warning(name));
                    Ok(value.clone())
                }
            },
            (None, None) => Err("缺少 password 或 password_env".to_string()),
        }
    }
}

/// DPAPI 解密 blob 为密码文本（仅 Windows）。
#[cfg(windows)]
fn decrypt_dpapi_blob(blob: &[u8]) -> Result<String, String> {
    // 换机/换用户是最常见的失败场景，统一文案直接给出可执行的出路；
    // unprotect 与 UTF-8 两处失败不区分、不回显 blob 内容与底层 OS 错误细节。
    const DECRYPT_GUIDANCE: &str =
        "此密文仅限原机原用户解密（换机/换用户后无法解开），请 config add --update 重新录入或改用 password_env";
    let plain = crate::dpapi::unprotect(blob).map_err(|_| DECRYPT_GUIDANCE.to_string())?;
    String::from_utf8(plain).map_err(|_| DECRYPT_GUIDANCE.to_string())
}

/// 非 Windows 平台无 DPAPI：fail-closed，密文一律拒绝而非当明文放行。
#[cfg(not(windows))]
fn decrypt_dpapi_blob(_blob: &[u8]) -> Result<String, String> {
    Err(
        "此密文由 Windows DPAPI 加密，当前平台无法解密，请在 Windows 上 config add --update 重录或改用 password_env"
            .to_string(),
    )
}

/// 用户主目录：优先 HOME（Unix 惯例），回退 USERPROFILE（Windows）。
pub fn home_dir() -> Result<PathBuf, String> {
    env::var("HOME")
        .or_else(|_| env::var("USERPROFILE"))
        .map(PathBuf::from)
        .map_err(|_| "无法确定用户主目录（HOME / USERPROFILE 均未设置）".to_string())
}

/// 工具配置根目录 ~/.roq（profiles.conf / profiles.d / logs / results 都在这里）。
pub fn default_config_base() -> Result<PathBuf, String> {
    Ok(home_dir()?.join(".roq"))
}

/// 解析极简 INI：[节名] + key=value。值不支持换行；# 或 ; 开头为注释行。
pub fn load_profiles(path: &PathBuf) -> Result<HashMap<String, Profile>, String> {
    let text =
        fs::read_to_string(path).map_err(|e| format!("读取配置 {} 失败：{}", path.display(), e))?;
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
        // password 与 password_env 二选一：并存报错（避免隐式优先级），全无也报错。
        let password = kv
            .iter()
            .find(|(k, _)| k == "password")
            .map(|(_, v)| v.clone());
        let password_env = kv
            .iter()
            .find(|(k, _)| k == "password_env")
            .map(|(_, v)| v.clone());
        if password.is_some() && password_env.is_some() {
            return Err(format!(
                "配置 [{}] 同时定义 password 与 password_env，请二选一",
                name
            ));
        }
        if password.is_none() && password_env.is_none() {
            return Err(format!("配置 [{}] 缺少 password 或 password_env", name));
        }
        let profile = Profile {
            host: get("host")?,
            port,
            user: get("user")?,
            password,
            password_env,
            database: get("database")?,
            ssl: kv
                .iter()
                .any(|(k, v)| k == "ssl" && v.eq_ignore_ascii_case("true")),
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
    let entries =
        fs::read_dir(dir).map_err(|e| format!("读取目录 {} 失败：{}", dir.display(), e))?;
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|e| format!("遍历目录 {} 失败：{}", dir.display(), e))?
            .path();
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

/// 默认配置文件清单：~/.roq/profiles.conf（存在时）+ ~/.roq/profiles.d/*.conf（按名排序）。
pub fn default_config_files() -> Result<Vec<PathBuf>, String> {
    let base = default_config_base()?;
    let mut files: Vec<PathBuf> = Vec::new();
    let single = base.join("profiles.conf");
    if single.is_file() {
        files.push(single);
    }
    let dir = base.join("profiles.d");
    if dir.is_dir() {
        files.extend(list_conf_files(&dir)?);
    }
    Ok(files)
}

/// 解析配置来源清单：`--config` 指定文件或目录（目录展开其中 *.conf），None 用默认清单
/// （profiles.conf + profiles.d/*.conf）。返回具体文件清单，加载与 migrate 共用同一套定位口径。
pub fn resolve_config_sources(config: Option<&PathBuf>) -> Result<Vec<PathBuf>, String> {
    let sources: Vec<PathBuf> = match config {
        Some(path) => {
            if !path.exists() {
                return Err(format!("--config 路径不存在：{}", path.display()));
            }
            vec![path.clone()]
        }
        None => default_config_files()?,
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for source in sources {
        if source.is_dir() {
            files.extend(list_conf_files(&source)?);
        } else {
            files.push(source);
        }
    }
    Ok(files)
}

/// 配置文件原子落盘：先写同目录 `.tmp` 临时文件再改名覆盖。
/// `fs::write` 直接覆写是先截断后写入，中途崩溃会把唯一副本截成半写状态；
/// 改名在临时文件完整落盘后一次完成，崩溃最多残留 `.tmp`，不损坏原文件。
pub fn write_file_atomic(path: &Path, text: &str) -> Result<(), String> {
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("{} 不是文件路径", path.display()))?;
    let tmp = path.with_file_name(format!("{}.tmp", file_name.to_string_lossy()));
    fs::write(&tmp, text).map_err(|e| format!("写入 {} 失败：{}", tmp.display(), e))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("替换 {} 失败：{}", path.display(), e)
    })?;
    Ok(())
}

/// 收集配置来源并加载全部 profile。
/// `config` 来自 --config（文件或目录，目录则扫描其中 *.conf，一项目一文件）；
/// None 时加载默认清单（profiles.conf + profiles.d/*.conf）。
pub fn load_config_sources(config: Option<&PathBuf>) -> Result<HashMap<String, Profile>, String> {
    let sources = resolve_config_sources(config)?;
    if sources.is_empty() {
        let base = default_config_base()?;
        return Err(format!(
            "未找到任何配置。可运行 roq config add <名> --host ... 创建，或手工建立 {} / {} 下的 *.conf，或用 --config 指定文件/目录。",
            base.join("profiles.conf").display(),
            base.join("profiles.d").display()
        ));
    }
    let mut profiles: HashMap<String, Profile> = HashMap::new();
    for file in sources {
        merge_profiles(&mut profiles, load_profiles(&file)?, &file)?;
    }
    Ok(profiles)
}

/// 打印 profile 清单（--list 与 config list 共用）。
/// --list/config list 的统一文本：每行 profile → host:port  db=库名  <- 来源文件。
/// 库名直接可见，配置名与实连库不符（如 test 连到 xxx_dev）一眼可辨。
pub fn format_profiles(profiles: &HashMap<String, Profile>) -> String {
    let mut entries: Vec<(&String, &Profile)> = profiles.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let lines: Vec<String> = entries
        .iter()
        .map(|(name, profile)| {
            format!(
                "  {:<12} {}:{}  db={}  <- {}",
                name, profile.host, profile.port, profile.database, profile.source
            )
        })
        .collect();
    format!("可用配置（{}）：\n{}", entries.len(), lines.join("\n"))
}

pub fn print_profiles(profiles: &HashMap<String, Profile>) {
    println!("{}", format_profiles(profiles));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::b64;

    /// 来源解析三态：文件直通、目录只展开 *.conf、不存在路径报错。
    #[test]
    fn resolve_sources_expands_dir_and_rejects_missing() {
        let file = write_temp(
            "resolve-f",
            "[a]\nhost=h\nuser=u\npassword_env=V\ndatabase=d\n",
        );
        assert_eq!(
            resolve_config_sources(Some(&file)).unwrap(),
            vec![file.clone()]
        );
        let dir = std::env::temp_dir().join(format!("roq-resolve-dir-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("a.conf"),
            "[a]\nhost=h\nuser=u\npassword_env=V\ndatabase=d\n",
        )
        .unwrap();
        fs::write(
            dir.join("b.conf"),
            "[b]\nhost=h\nuser=u\npassword_env=V\ndatabase=d\n",
        )
        .unwrap();
        fs::write(dir.join("ignored.txt"), "x").unwrap();
        let files = resolve_config_sources(Some(&dir)).unwrap();
        assert_eq!(files.len(), 2, "目录应只展开 *.conf：{:?}", files);
        let _ = fs::remove_dir_all(&dir);
        let nowhere =
            std::env::temp_dir().join(format!("roq-resolve-miss-{}.conf", std::process::id()));
        let _ = fs::remove_file(&nowhere);
        assert!(resolve_config_sources(Some(&nowhere)).is_err());
        let _ = fs::remove_file(&file);
    }

    /// 原子写正确落盘且不残留 .tmp（原子性本身无法在单测中模拟崩溃，钉行为契约）。
    #[test]
    fn atomic_write_replaces_content_and_leaves_no_tmp() {
        let path = write_temp("atomic", "old");
        write_file_atomic(&path, "new-content").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new-content");
        let tmp = path.with_file_name(format!(
            "{}.tmp",
            path.file_name().unwrap().to_string_lossy()
        ));
        assert!(!tmp.exists(), "原子写不得残留 .tmp 临时文件");
        let _ = fs::remove_file(&path);
    }

    fn write_temp(tag: &str, content: &str) -> PathBuf {
        // 并行测试各自独立文件，避免同名互踩
        let path =
            std::env::temp_dir().join(format!("roq-test-{}-{}.conf", tag, std::process::id()));
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn format_profiles_lists_host_db_and_source_per_line() {
        let path = write_temp(
            "listfmt",
            "[dev]\nhost=h1\nport=3307\nuser=u\npassword=p\ndatabase=db_a\n",
        );
        let profiles = load_profiles(&path).unwrap();
        let out = format_profiles(&profiles);
        assert!(out.contains("可用配置（1）"), "首行应计数：{}", out);
        let line = out.lines().nth(1).expect("应有 profile 行");
        assert!(line.contains("dev"), "应含配置名：{}", line);
        assert!(line.contains("h1:3307"), "应含 host:port：{}", line);
        assert!(line.contains("db=db_a"), "应含库名：{}", line);
        assert!(
            line.contains(&path.display().to_string()),
            "应含来源文件：{}",
            line
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn parse_profile_with_defaults_and_ssl() {
        let path = write_temp(
            "full",
            "# 注释\n[a]\nhost=h1\nuser=u\npassword=p\ndatabase=d\nssl=true\n",
        );
        let profiles = load_profiles(&path).unwrap();
        let profile = &profiles["a"];
        assert_eq!(profile.host, "h1");
        assert_eq!(profile.port, 3306); // port 缺省默认
        assert!(profile.ssl);
        assert_eq!(profile.source, path.display().to_string());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn missing_required_key_reports_section() {
        let path = write_temp("missing", "[b]\nhost=h\n");
        let err = load_profiles(&path).unwrap_err();
        assert!(err.contains("[b]"), "报错应指明节名：{}", err);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn password_env_resolves_from_environment() {
        let var = format!("ROQ_TEST_PWD_OK_{}", std::process::id());
        std::env::set_var(&var, "s3cret");
        let content = format!("[e]\nhost=h\nuser=u\npassword_env={}\ndatabase=d\n", var);
        let path = write_temp("penv-ok", &content);
        let profiles = load_profiles(&path).unwrap();
        let profile = &profiles["e"];
        assert_eq!(profile.password_env.as_deref(), Some(var.as_str()));
        assert_eq!(profile.resolved_password("e").unwrap(), "s3cret");
        std::env::remove_var(&var);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn resolved_password_fails_closed_when_env_missing() {
        let var = format!("ROQ_TEST_PWD_MISS_{}", std::process::id());
        std::env::remove_var(&var);
        let content = format!("[e]\nhost=h\nuser=u\npassword_env={}\ndatabase=d\n", var);
        let path = write_temp("penv-miss", &content);
        let profiles = load_profiles(&path).unwrap();
        let err = profiles["e"].resolved_password("e").unwrap_err();
        assert!(err.contains(&var), "报错应含变量名：{}", err);
        assert!(err.contains("未设置"), "报错应说明变量未设置：{}", err);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn password_and_password_env_are_exclusive() {
        let path = write_temp(
            "penv-both",
            "[e]\nhost=h\nuser=u\npassword=p\npassword_env=FOO\ndatabase=d\n",
        );
        let err = load_profiles(&path).unwrap_err();
        assert!(err.contains("[e]"), "报错应指明节名：{}", err);
        assert!(err.contains("password_env"), "报错应说明两键冲突：{}", err);
        let _ = fs::remove_file(&path);
    }

    /// 不经文件直构 Profile，专门测 resolved_password 的明文/密文分流。
    fn profile_with_password(password: &str) -> Profile {
        Profile {
            host: "h".to_string(),
            port: 3306,
            user: "u".to_string(),
            password: Some(password.to_string()),
            password_env: None,
            database: "d".to_string(),
            ssl: false,
            source: "test".to_string(),
        }
    }

    #[test]
    #[cfg(windows)]
    fn enc_value_roundtrips_through_ini_parser() {
        let cipher = b64::encode(&crate::dpapi::protect("s3cret").unwrap());
        let content = format!(
            "[e]\nhost=h\nuser=u\npassword={}{}\ndatabase=d\n",
            ENC_PREFIX, cipher
        );
        let path = write_temp("enc-rt", &content);
        let profiles = load_profiles(&path).unwrap();
        assert_eq!(profiles["e"].resolved_password("e").unwrap(), "s3cret");
        let _ = fs::remove_file(&path);
    }

    #[test]
    #[cfg(not(windows))]
    fn enc_prefix_on_nonwindows_fails_closed() {
        let profile = profile_with_password(&format!("{}QUJD", ENC_PREFIX));
        let err = profile.resolved_password("e").unwrap_err();
        assert!(err.contains("无法解密"), "报错应说明平台无法解密：{}", err);
    }

    #[test]
    fn bad_base64_in_enc_value_fails() {
        let profile = profile_with_password("enc:v1:dpapi:!!!");
        let err = profile.resolved_password("e").unwrap_err();
        assert!(err.contains("base64"), "报错应含 base64 解错信息：{}", err);
    }

    #[test]
    #[cfg(windows)]
    fn plaintext_warning_text() {
        let text = plaintext_warning("dev");
        assert!(text.contains("dev"), "文案应含 profile 名：{}", text);
        assert!(text.contains("--update"), "文案应提示 --update 重录：{}", text);
        assert!(text.contains("明文"), "文案应说明明文存储：{}", text);
    }

    #[test]
    #[cfg(not(windows))]
    fn plaintext_warning_text_nonwindows_suggests_password_env() {
        // 非 Windows 上 --password 一律被拒，--update 重录是死路，建议须可执行
        let text = plaintext_warning("dev");
        assert!(text.contains("dev"), "文案应含 profile 名：{}", text);
        assert!(
            text.contains("password_env"),
            "文案应提示改用 password_env：{}",
            text
        );
        assert!(text.contains("明文"), "文案应说明明文存储：{}", text);
    }

    #[test]
    #[cfg(windows)]
    fn bad_dpapi_blob_decrypt_error_mentions_update() {
        // QUJD 是合法 base64（解码为 3 字节）但不是合法 DPAPI blob——
        // Windows 上必走 unprotect 失败路径，文案须给出 --update 重录指引
        let profile = profile_with_password(&format!("{}QUJD", ENC_PREFIX));
        let err = profile.resolved_password("e").unwrap_err();
        assert!(
            err.contains("--update"),
            "解密失败文案应指引 --update 重录：{}",
            err
        );
        assert!(
            err.contains("原机原用户"),
            "文案应说明密文绑定原机原用户：{}",
            err
        );
    }

    #[test]
    fn plaintext_password_with_equals_survives_ini_roundtrip() {
        // 既有行为钉住：INI 按首个 '=' 切分，密码值内含 '=' 原样保留
        let path = write_temp("pwd-eq", "[e]\nhost=h\nuser=u\npassword=a=b\ndatabase=d\n");
        let profiles = load_profiles(&path).unwrap();
        assert_eq!(profiles["e"].password.as_deref(), Some("a=b"));
        let _ = fs::remove_file(&path);
    }
}
