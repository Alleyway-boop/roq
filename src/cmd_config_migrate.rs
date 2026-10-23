//! `roq config migrate`：把历史明文密码就地迁移为 DPAPI 密文。
//!
//! 动机：`--password` 转为 DPAPI 加密落盘（`enc:v1:dpapi:` 前缀）之前录入的配置，
//! 密码以明文躺在 `~/.roq` 下；本命令提供一条显式、可验证的就地升级路径，
//! 密码全程不离开 roq 进程（不经命令行参数、不经 shell 历史、不进 AI 上下文）。
//!
//! 安全约束：
//! - 只替换明文 `password=` 行，注释/空行/行序/env 凭据行/已有密文逐字节保留；
//! - 写前每条新密文都真实解密比对原明文，全部通过才写该文件
//!   （`.tmp` 临时文件 + 改名原子替换，崩溃不损坏原文件）；
//! - 迁移幂等：已密文零改写，中断后重跑续传；
//! - 扫描范围与加载同口径（默认清单或 `--config` 指定的文件/目录），
//!   `config add --file` 登记在 ~/.roq 之外的配置须 `--config` 指定迁移；
//! - 非 Windows 无加密能力（明文落盘本就被禁止），fail-closed 拒绝。

use std::path::PathBuf;

#[cfg(windows)]
use std::fs;

/// 生产入口：--config 指定文件或目录（覆盖 ~/.roq 之外登记的配置），None 用默认清单。
pub fn migrate(config: Option<&PathBuf>) -> Result<String, String> {
    let sources = crate::config::resolve_config_sources(config)?;
    migrate_with_sources(&sources)
}

/// 单文件扫描结果：重写文本（无明文时与输入逐字节一致）与三类凭据计数。
#[cfg(windows)]
struct FileScanOutcome {
    text: String,
    migrated: usize,
    encrypted: usize,
    env_based: usize,
}

/// Windows 实现：逐文件扫描明文 `password=` 行，写前逐条真实解密比对原明文，
/// 全部通过才就地重写该文件（单文件一次写入原子生效）；跨文件靠幂等重跑续传。
#[cfg(windows)]
pub fn migrate_with_sources(sources: &[PathBuf]) -> Result<String, String> {
    let mut migrated = 0;
    let mut encrypted = 0;
    let mut env_based = 0;
    let mut untouched_files = 0;
    for source in sources {
        if !source.is_file() {
            continue;
        }
        let text = fs::read_to_string(source)
            .map_err(|e| format!("读取 {} 失败：{}", source.display(), e))?;
        let outcome = encrypt_plaintext_password_lines(&text)?;
        if outcome.migrated > 0 {
            crate::config::write_file_atomic(source, &outcome.text)?;
        } else {
            untouched_files += 1;
        }
        migrated += outcome.migrated;
        encrypted += outcome.encrypted;
        env_based += outcome.env_based;
    }
    Ok(format!(
        "密码迁移完成：迁移 {} 条，已是密文 {} 条，环境变量 {} 条（{} 个文件无需改写）",
        migrated, encrypted, env_based, untouched_files
    ))
}

/// 非 Windows：无加密能力（`--password` 明文落盘本就被禁止），无明文可迁移，fail-closed 拒绝。
#[cfg(not(windows))]
pub fn migrate_with_sources(_sources: &[PathBuf]) -> Result<String, String> {
    Err("密码迁移仅 Windows 支持：此平台明文密码落盘本就被禁止，无需迁移".to_string())
}

/// 逐行扫描：仅改写明文 `password=` 行，其余行（注释/空行/密文/env）原样保留。
/// 按 '\n' 手动切分以保留 CRLF 与末尾换行状态；行匹配与
/// [`crate::config::load_profiles`] 同口径（trim 后按首个 '=' 切 key/value，key 忽略大小写）。
#[cfg(windows)]
fn encrypt_plaintext_password_lines(text: &str) -> Result<FileScanOutcome, String> {
    let mut outcome = FileScanOutcome {
        text: String::with_capacity(text.len() + 64),
        migrated: 0,
        encrypted: 0,
        env_based: 0,
    };
    let mut rest = text;
    while let Some((line, remainder)) = rest.split_once('\n') {
        push_rewritten_line(&mut outcome, line)?;
        outcome.text.push('\n');
        rest = remainder;
    }
    if !rest.is_empty() {
        push_rewritten_line(&mut outcome, rest)?;
    }
    Ok(outcome)
}

/// 处理单行：明文凭据行改写为密文行并计数，密文/env 行计数后原样透传，其余行原样透传。
/// value 取 trim 后形态（与 [`crate::config::load_profiles`] 的 value.trim() 同口径），
/// 改写行保留原行 CRLF 行尾，不把行尾风格改混。
#[cfg(windows)]
fn push_rewritten_line(outcome: &mut FileScanOutcome, line: &str) -> Result<(), String> {
    let trimmed = line.trim();
    let rewritten = match trimmed.split_once('=') {
        Some((key, value)) if key.trim().eq_ignore_ascii_case("password") => {
            let value = value.trim();
            if value.starts_with(crate::config::ENC_PREFIX) {
                outcome.encrypted += 1;
                None
            } else {
                Some(encrypt_password_line(value)?)
            }
        }
        Some((key, _)) if key.trim().eq_ignore_ascii_case("password_env") => {
            outcome.env_based += 1;
            None
        }
        _ => None,
    };
    match rewritten {
        Some(new_line) => {
            outcome.text.push_str(&new_line);
            if line.ends_with('\r') {
                outcome.text.push('\r');
            }
            outcome.migrated += 1;
        }
        None => outcome.text.push_str(line),
    }
    Ok(())
}

/// 单条明文凭据行的加密改写：DPAPI 加密后立即真实解密比对原明文，
/// 校验不过即整体失败（fail-closed，不落任何未校验密文）。
#[cfg(windows)]
fn encrypt_password_line(plain: &str) -> Result<String, String> {
    let blob = crate::dpapi::protect(plain)?;
    let roundtrip = crate::dpapi::unprotect(&blob)?;
    if roundtrip != plain.as_bytes() {
        return Err("迁移校验失败：密文解开与原明文不一致，已中止（未写入）".to_string());
    }
    Ok(format!(
        "password={}{}",
        crate::config::ENC_PREFIX,
        crate::b64::encode(&blob)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_conf(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("roq-mig-{}-{}.conf", tag, std::process::id()))
    }

    /// migrate 核心行为：明文 password 行就地转 DPAPI 密文，
    /// 注释/空行/env 凭据行/已是密文的行原样保留，回读可解密为原明文；
    /// 报告含迁移与分类跳过计数。
    #[cfg(windows)]
    #[test]
    fn migrate_encrypts_plaintext_lines_and_preserves_layout() {
        let path = temp_conf("mig");
        let _ = std::fs::remove_file(&path);
        let plain = "old-plain-pwd";
        let encrypted_line = format!(
            "password={}{}",
            crate::config::ENC_PREFIX,
            crate::b64::encode(&crate::dpapi::protect("already-enc").unwrap())
        );
        std::fs::write(
            &path,
            format!(
                "# 头注释\n[plain-sec]\nhost=h1\nuser=u1\npassword={}\ndatabase=d1\n\n# 节间注释\n[env-sec]\nhost=h2\nuser=u2\npassword_env=V2\ndatabase=d2\n\n[enc-sec]\nhost=h3\nuser=u3\n{}\ndatabase=d3\n",
                plain, encrypted_line
            ),
        )
        .unwrap();
        let report = migrate_with_sources(&[path.clone()]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# 头注释"), "文件头注释应保留：{}", text);
        assert!(text.contains("# 节间注释"), "节间注释应保留：{}", text);
        assert!(
            !text.contains(plain),
            "明文不得残留（'-' 不在 base64 字母表内，不会与密文撞串）：{}",
            text
        );
        assert!(
            text.contains("password_env=V2"),
            "env 凭据行不得改动：{}",
            text
        );
        assert!(
            text.contains(&encrypted_line),
            "已是密文的行必须逐字节保留：{}",
            text
        );
        let profiles = crate::config::load_profiles(&path).unwrap();
        assert_eq!(
            profiles["plain-sec"]
                .resolved_password("plain-sec")
                .unwrap(),
            plain,
            "迁移后应能解密回原明文"
        );
        assert!(
            report.contains("迁移 1") && report.contains("密文 1") && report.contains("环境变量 1"),
            "报告应含分类计数：{}",
            report
        );
        let _ = std::fs::remove_file(&path);
    }

    /// 幂等：全部已是密文时重复迁移零改写，文件字节不变——迁移可安全重跑续传。
    #[cfg(windows)]
    #[test]
    fn migrate_is_idempotent_when_all_encrypted() {
        let path = temp_conf("mig-idem");
        let _ = std::fs::remove_file(&path);
        std::fs::write(
            &path,
            format!(
                "[a]\nhost=h\nuser=u\npassword={}{}\ndatabase=d\n",
                crate::config::ENC_PREFIX,
                crate::b64::encode(&crate::dpapi::protect("pwd-1").unwrap())
            ),
        )
        .unwrap();
        let first = migrate_with_sources(&[path.clone()]).unwrap();
        assert!(first.contains("迁移 0"), "全密文应零迁移：{}", first);
        let before = std::fs::read_to_string(&path).unwrap();
        let second = migrate_with_sources(&[path.clone()]).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(second.contains("迁移 0"), "重跑仍应零迁移：{}", second);
        assert_eq!(before, after, "幂等重跑不得改写字节");
        let _ = std::fs::remove_file(&path);
    }

    /// 非 Windows 无加密能力（明文落盘本就被禁止），migrate fail-closed 拒绝。
    #[cfg(not(windows))]
    #[test]
    fn migrate_rejected_on_nonwindows() {
        let err = migrate_with_sources(&[temp_conf("mig-nw")]).unwrap_err();
        assert!(err.contains("Windows"), "报错应说明仅 Windows 支持：{}", err);
    }

    /// load_profiles 对 value 做 trim（"password = x" 的生效密码是 "x"），
    /// migrate 的加密输入必须同口径，否则迁移前后生效密码不一致、认证失败。
    #[cfg(windows)]
    #[test]
    fn migrate_trims_value_before_encrypting() {
        let path = temp_conf("mig-trim");
        let _ = std::fs::remove_file(&path);
        std::fs::write(
            &path,
            "[p]\nhost=h\nuser=u\npassword = spaced-pwd\ndatabase=d\n",
        )
        .unwrap();
        migrate_with_sources(&[path.clone()]).unwrap();
        let profiles = crate::config::load_profiles(&path).unwrap();
        assert_eq!(
            profiles["p"].resolved_password("p").unwrap(),
            "spaced-pwd",
            "迁移后解密结果应与原生效密码（trim 后）一致"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// CRLF 文件中被改写的行也须保留 \r 行尾，不得把行尾风格改混。
    #[cfg(windows)]
    #[test]
    fn migrate_preserves_crlf_line_endings() {
        let path = temp_conf("mig-crlf");
        let _ = std::fs::remove_file(&path);
        std::fs::write(
            &path,
            "[a]\r\nhost=h\r\nuser=u\r\npassword=old-pwd\r\ndatabase=d\r\n",
        )
        .unwrap();
        migrate_with_sources(&[path.clone()]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let pieces: Vec<&str> = text.split('\n').collect();
        for line in &pieces[..pieces.len() - 1] {
            assert!(line.ends_with('\r'), "每个完整行都应以 CRLF 结尾：{:?}", line);
        }
        let _ = std::fs::remove_file(&path);
    }
}
