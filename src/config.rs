//! 连接配置：极简 INI 解析 + 目录扫描（一项目一文件）+ 跨文件同名冲突检测。

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Debug)]
pub struct Profile {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
    /// 是否启用 TLS（RDS 部分端点不宣告 TLS 能力，强制启用会握手失败）。
    pub ssl: bool,
    /// 来源配置文件路径（多文件扫描时用于 --list 展示与冲突定位）。
    pub source: String,
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
/// `config` 来自 --config（文件或目录，目录则扫描其中 *.conf，一项目一文件）；
/// None 时默认加载 ~/.roq/profiles.conf（存在时）+ ~/.roq/profiles.d/*.conf（存在时）。
pub fn load_config_sources(config: Option<&PathBuf>) -> Result<HashMap<String, Profile>, String> {
    let mut sources: Vec<PathBuf> = Vec::new();
    match config {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp(content: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("roq-test-{}.conf", std::process::id()));
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn parse_profile_with_defaults_and_ssl() {
        let path = write_temp("# 注释\n[a]\nhost=h1\nuser=u\npassword=p\ndatabase=d\nssl=true\n");
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
        let path = write_temp("[b]\nhost=h\n");
        let err = load_profiles(&path).unwrap_err();
        assert!(err.contains("[b]"), "报错应指明节名：{}", err);
        let _ = fs::remove_file(&path);
    }
}
