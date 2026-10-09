//! `roq skill` 子命令：安装随二进制打包的 AI 使用技能（SKILL.md）。
//!
//! skill 内容由 include_str! 嵌入，版本永远与二进制一致；目标已存在且内容不同时
//! 拒绝覆盖（--force 放行）——与 config add 的防覆盖纪律一致，保护手改过的 skill。
//! 写后回读校验，确保落盘内容完整。

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::cli::SkillCmd;
use crate::config::home_dir;

/// 仓内规范源 skill/roq/SKILL.md，编译期嵌入二进制；发布包亦同捆此文件。
const SKILL_MD: &str = include_str!("../skill/roq/SKILL.md");

pub fn run(cmd: SkillCmd) -> ExitCode {
    let SkillCmd::Install { global, project: _, force } = cmd;
    // 解析层已保证 global/project 互斥且至少一真，此处直接按 global 取目标
    let result = match resolve_target(global) {
        Ok(target) => install_to(&target, force),
        Err(msg) => Err(msg),
    };
    match result {
        Ok(msg) => {
            println!("{}", msg);
            ExitCode::from(0)
        }
        Err(msg) => {
            eprintln!("[roq skill] {}", msg);
            ExitCode::from(1)
        }
    }
}

/// 安装目标：--global 为 ~/.claude/skills/roq/，--project 为当前项目 .claude/skills/roq/。
fn resolve_target(global: bool) -> Result<PathBuf, String> {
    if global {
        Ok(home_dir()?
            .join(".claude")
            .join("skills")
            .join("roq")
            .join("SKILL.md"))
    } else {
        let cwd = std::env::current_dir().map_err(|e| format!("无法确定当前目录：{}", e))?;
        Ok(cwd.join(".claude").join("skills").join("roq").join("SKILL.md"))
    }
}

/// 安装决策：写入 / 已是最新 / 拒绝覆盖（存在且不同且未 --force）。
pub enum InstallDecision {
    Write,
    Identical,
    Refuse,
}

pub fn decide_install(existing: Option<&str>, incoming: &str, force: bool) -> InstallDecision {
    match existing {
        None => InstallDecision::Write,
        Some(text) if text == incoming => InstallDecision::Identical,
        Some(_) if force => InstallDecision::Write,
        Some(_) => InstallDecision::Refuse,
    }
}

/// 把 SKILL_MD 落到 target，返回给用户看的消息（含 PATH 就绪提示）。
fn install_to(target: &Path, force: bool) -> Result<String, String> {
    let existing = match fs::read_to_string(target) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("读取现有 skill {} 失败：{}", target.display(), e)),
    };
    match decide_install(existing.as_deref(), SKILL_MD, force) {
        InstallDecision::Identical => Ok(format!(
            "已是最新（roq v{}）：{}",
            env!("CARGO_PKG_VERSION"),
            target.display()
        )),
        InstallDecision::Refuse => Err(format!(
            "{} 已存在且内容不同（可能是手改或旧版本）；确认覆盖请加 --force",
            target.display()
        )),
        InstallDecision::Write => {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{}", e))?;
            }
            fs::write(target, SKILL_MD)
                .map_err(|e| format!("写入 {} 失败：{}", target.display(), e))?;
            let written =
                fs::read_to_string(target).map_err(|e| format!("回读 {} 失败：{}", target.display(), e))?;
            if written != SKILL_MD {
                return Err("写入后回读校验失败".to_string());
            }
            let mut msg = format!(
                "已安装 roq skill → {}（v{}，与二进制同版）",
                target.display(),
                env!("CARGO_PKG_VERSION")
            );
            if !is_on_path("roq") {
                msg.push_str("\n提示：roq 不在 PATH，skill 中的 `roq` 命令需可用才能生效——把二进制所在目录加入 PATH，或以完整路径调用");
            }
            Ok(msg)
        }
    }
}

/// `roq` 是否可在 PATH 上直接调用（Windows 检查 roq.exe）。
fn is_on_path(executable: &str) -> bool {
    match std::env::var_os("PATH") {
        Some(paths) => is_executable_in_paths(executable, &paths),
        None => false,
    }
}

fn is_executable_in_paths(executable: &str, paths: &OsStr) -> bool {
    let file_name = if cfg!(windows) {
        format!("{}.exe", executable)
    } else {
        executable.to_string()
    };
    std::env::split_paths(paths).any(|dir| dir.join(&file_name).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_installs_new_noops_identical_refuses_differs_without_force() {
        assert!(matches!(decide_install(None, SKILL_MD, false), InstallDecision::Write));
        assert!(matches!(
            decide_install(Some(SKILL_MD), SKILL_MD, false),
            InstallDecision::Identical
        ));
        assert!(matches!(decide_install(Some("手改内容"), SKILL_MD, false), InstallDecision::Refuse));
        assert!(matches!(decide_install(Some("手改内容"), SKILL_MD, true), InstallDecision::Write));
    }

    #[test]
    fn install_to_writes_refuses_overwrite_then_forces_over_temp_target() {
        let target = std::env::temp_dir().join(format!("roq-skill-{}.md", std::process::id()));
        let _ = fs::remove_file(&target);
        let msg = install_to(&target, false).unwrap();
        assert!(msg.contains("已安装"), "首装应写成功：{}", msg);
        assert_eq!(fs::read_to_string(&target).unwrap(), SKILL_MD);
        let msg = install_to(&target, false).unwrap();
        assert!(msg.contains("已是最新"), "同内容重装应无操作：{}", msg);
        fs::write(&target, "手改").unwrap();
        let err = install_to(&target, false).unwrap_err();
        assert!(err.contains("--force"), "内容不同应拒覆盖并提示 --force：{}", err);
        let msg = install_to(&target, true).unwrap();
        assert!(msg.contains("已安装"), "--force 应覆盖：{}", msg);
        assert_eq!(fs::read_to_string(&target).unwrap(), SKILL_MD);
        let _ = fs::remove_file(&target);
    }

    #[test]
    fn executable_scan_finds_file_only_in_given_paths() {
        let dir = std::env::temp_dir().join(format!("roq-path-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let exe_name = if cfg!(windows) { "fakebin.exe" } else { "fakebin" };
        fs::write(dir.join(exe_name), "").unwrap();
        let paths = std::env::join_paths([&dir]).unwrap();
        assert!(is_executable_in_paths("fakebin", &paths), "应找到刚放入的文件");
        assert!(!is_executable_in_paths("no-such-bin-roq", &paths), "不存在的名字不应命中");
        fs::remove_dir_all(&dir).unwrap();
    }
}
