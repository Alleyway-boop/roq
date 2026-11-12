//! `roq q` 管理子命令：保存查询的增删查（add/list/show/remove）。
//! 执行面走 `Command::SavedQuery` 链路，不在本模块——这里只管 queries.d 下的模板文件。
//!
//! add 是唯一写路径，fail-closed 全序（顺序即安全）：
//! [`crate::saved_query::validate_query_name`]（合法名保证路径不逃逸）→
//! 重名/覆盖预检（同名拒绝并指引 --update、--update 必须既有）→ 模板非空 →
//! 占位符提取 → 哑参替换过 [`crate::gate::gate`]（静态禁词保存时早暴露，
//! 不必等到执行撞闸门；占位符换哑值后扫描，名字本身不误伤禁词）→
//! [`crate::saved_query::write_template_in`] → 回读：内容与写入逐字节一致，
//! 且对文件内容复跑一次预检（文件层损坏当场暴露，不带病确认）。
//! Ok 消息含路径与占位符清单。
//!
//! show/remove 只接受合法名（同名校验挡住 `../` 路径逃逸）；remove 须 --yes，
//! 解析层已拒一次，内核再拒一次（纵深防御）。可测内核 `*_in(base, ...)` 注入
//! base 目录；生产入口 [`run`] 用 [`crate::saved_query::queries_dir`] 取默认目录。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::cli::QCmd;
use crate::gate::gate;
use crate::saved_query::{
    dummy_substitute, extract_placeholder_names, list_queries_in, load_template_in, queries_dir,
    validate_query_name, write_template_in, QUERY_FILE_HEADER,
};

pub fn run(cmd: QCmd) -> ExitCode {
    match dispatch(cmd) {
        Ok(msg) => {
            if !msg.is_empty() {
                println!("{}", msg);
            }
            ExitCode::from(0)
        }
        Err(msg) => {
            eprintln!("[roq q] {}", msg);
            ExitCode::from(1)
        }
    }
}

/// 生产路径：base 取默认 queries 目录，其余与可测内核一致。
fn dispatch(cmd: QCmd) -> Result<String, String> {
    let base = queries_dir()?;
    match cmd {
        QCmd::Add {
            name,
            template,
            update,
        } => add_in(&base, &name, &template, update),
        QCmd::List => list_in(&base),
        QCmd::Show { name } => show_in(&base, &name),
        QCmd::Remove { name, yes } => remove_in(&base, &name, yes),
    }
}

/// 保存查询文件路径：与 saved_query 文件层同规则（`<base>/<名>.sql`，一查询一文件）。
fn query_file_path(base: &Path, name: &str) -> PathBuf {
    base.join(format!("{name}.sql"))
}

/// add 的可测内核，fail-closed 全序（顺序即安全）：名字校验 → 重名/覆盖预检 →
/// 模板非空 → 占位符提取 → 哑参替换过只读闸门 → 写入 → 回读内容一致且预检复跑通过。
/// 任一步失败即整体失败，绝不留下半写状态。
pub fn add_in(base: &Path, name: &str, template: &str, update: bool) -> Result<String, String> {
    validate_query_name(name)?;
    let path = query_file_path(base, name);
    if path.exists() && !update {
        return Err(format!(
            "保存查询 {name} 已存在于 {}；覆盖重录请加 --update",
            path.display()
        ));
    }
    if update && !path.exists() {
        return Err(format!(
            "保存查询 {name} 在 {} 中不存在，无法 --update 覆盖；去掉 --update 即为新增",
            path.display()
        ));
    }
    let template = template.trim();
    if template.is_empty() {
        return Err("模板不能为空：请提供 SQL 语句（{{名}} 为占位符）".to_string());
    }
    let names = extract_placeholder_names(template)?;
    let dummy = dummy_substitute(template)?;
    // 哑参预检：静态禁词在保存时早暴露（执行时才撞闸门是糟糕体验）；
    // 占位符先换成哑值 "1" 再过闸，占位符名本身不误伤禁词扫描
    gate(&dummy)?;
    let written = write_template_in(base, name, template)?;
    // 回读校验：文件内容与写入意图逐字节一致；再对文件内容复跑一次预检，
    // 文件层损坏（截断/编码/头注释缺失）当场暴露，不带病确认。
    // 预检只跑去头注释后的模板段——头注释里的 `{{名}}` 是文档说明文字，
    // 不是占位符，不能进占位符扫描。
    let content = load_template_in(base, name)?;
    if content != format!("{QUERY_FILE_HEADER}{template}") {
        return Err(format!(
            "回读校验失败：{} 内容与写入不一致",
            written.display()
        ));
    }
    let stored = content.strip_prefix(QUERY_FILE_HEADER).ok_or_else(|| {
        format!(
            "回读校验失败：{} 缺少文件头注释",
            written.display()
        )
    })?;
    gate(&dummy_substitute(stored)?)?;
    let placeholder_text = if names.is_empty() {
        "无占位符".to_string()
    } else {
        format!("占位符 {} 个：{}", names.len(), names.join(", "))
    };
    Ok(format!(
        "{name} 已写入 {}（回读校验通过；{placeholder_text}）",
        written.display()
    ))
}

/// 列举全部保存查询（名 + 摘要，按名字典序）；空态给保存指引而非空输出。
pub fn list_in(base: &Path) -> Result<String, String> {
    let queries = list_queries_in(base)?;
    if queries.is_empty() {
        return Ok(format!(
            "尚无保存查询（{}）：用 roq q add <名> \"SQL模板\" 保存第一条",
            base.display()
        ));
    }
    Ok(queries
        .iter()
        .map(|(name, summary)| format!("{name}  {summary}"))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// 原样输出模板文件内容（含头注释）。合法名校验在前，路径不逃逸。
pub fn show_in(base: &Path, name: &str) -> Result<String, String> {
    validate_query_name(name)?;
    load_template_in(base, name)
}

/// 删除保存查询文件。存在性预检 → --yes 确认（解析层已拒一次，内核再拒一次），
/// 非法名不得触达删除路径。
pub fn remove_in(base: &Path, name: &str, yes: bool) -> Result<String, String> {
    validate_query_name(name)?;
    let path = query_file_path(base, name);
    if !path.exists() {
        return Err(format!(
            "保存查询 {name} 不存在：运行 roq q list 查看已保存的查询"
        ));
    }
    if !yes {
        return Err(format!(
            "将删除保存查询 {name}（{}）；确认请加 --yes",
            path.display()
        ));
    }
    fs::remove_file(&path).map_err(|e| format!("删除 {} 失败：{e}", path.display()))?;
    Ok(format!("已删除 {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_qdir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("roq-qcmd-{tag}-{}", std::process::id()))
    }

    #[test]
    fn add_writes_and_readbacks() {
        let base = temp_qdir("add");
        let _ = fs::remove_dir_all(&base);
        let template = "SELECT * FROM t WHERE id='{{id}}'";
        let msg = add_in(&base, "saved", template, false).expect("add 应成功");
        assert!(msg.contains("回读校验"), "Ok 消息应含回读校验：{msg}");
        assert!(
            msg.contains("占位符 1 个：id"),
            "Ok 消息应含占位符清单：{msg}"
        );
        assert!(msg.contains("saved.sql"), "Ok 消息应含落盘路径：{msg}");
        // 落盘内容 = 头注释 + 模板，show 应原样吐回
        let content = fs::read_to_string(base.join("saved.sql")).expect("读取落盘文件应成功");
        assert_eq!(content, format!("{QUERY_FILE_HEADER}{template}"));
        // 无占位符模板的 Ok 消息用「无占位符」措辞
        let msg = add_in(&base, "plain", "SELECT 1", false).expect("无占位符 add 应成功");
        assert!(msg.contains("无占位符"), "无占位符消息应明确：{msg}");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn add_refuses_duplicate_then_update_works() {
        let base = temp_qdir("dup");
        let _ = fs::remove_dir_all(&base);
        add_in(&base, "dup-q", "SELECT 1", false).expect("首次 add 应成功");
        let err = add_in(&base, "dup-q", "SELECT 2", false).unwrap_err();
        assert!(err.contains("已存在"), "同名应拒绝：{err}");
        assert!(err.contains("--update"), "拒绝应指引 --update：{err}");
        assert!(
            fs::read_to_string(base.join("dup-q.sql"))
                .expect("读取应成功")
                .contains("SELECT 1"),
            "拒绝覆盖时旧内容不得被改写"
        );
        let msg = add_in(&base, "dup-q", "SELECT 2", true).expect("--update 覆盖应成功");
        assert!(msg.contains("回读校验"), "覆盖成功消息应含回读校验：{msg}");
        assert!(
            fs::read_to_string(base.join("dup-q.sql"))
                .expect("读取应成功")
                .contains("SELECT 2"),
            "--update 后内容应更新"
        );
        let err = add_in(&base, "ghost", "SELECT 3", true).unwrap_err();
        assert!(err.contains("不存在"), "--update 不存在应拒绝：{err}");
        assert!(
            !base.join("ghost.sql").exists(),
            "拒绝时不应写出目标文件"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn add_rejects_template_with_static_forbidden_word() {
        let base = temp_qdir("forbidden");
        let _ = fs::remove_dir_all(&base);
        let err = add_in(
            &base,
            "bad-q",
            "SELECT * FROM t WHERE note='UPDATE'",
            false,
        )
        .unwrap_err();
        assert!(err.contains("UPDATE"), "应透传闸门拒绝原因：{err}");
        assert!(
            !base.join("bad-q.sql").exists(),
            "预检拒绝时不应写出文件"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn add_rejects_malformed_placeholder() {
        let base = temp_qdir("malformed");
        let _ = fs::remove_dir_all(&base);
        let err = add_in(&base, "bad-ph", "SELECT '{{a-'", false).unwrap_err();
        assert!(err.contains("占位符"), "报错应说明占位符问题：{err}");
        assert!(
            !base.join("bad-ph.sql").exists(),
            "拒绝时不应写出文件"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn remove_needs_yes() {
        let base = temp_qdir("rm");
        let _ = fs::remove_dir_all(&base);
        add_in(&base, "gone", "SELECT 1", false).expect("add 应成功");
        let err = remove_in(&base, "gone", false).unwrap_err();
        assert!(err.contains("--yes"), "无 --yes 应拒绝并指引：{err}");
        assert!(base.join("gone.sql").exists(), "未确认不得删除");
        remove_in(&base, "gone", true).expect("--yes 删除应成功");
        assert!(!base.join("gone.sql").exists(), "确认后文件应消失");
        assert!(remove_in(&base, "../evil", true).is_err(), "非法名不得触达删除");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn list_and_show_outputs() {
        let base = temp_qdir("lsshow");
        let _ = fs::remove_dir_all(&base);
        let msg = list_in(&base).expect("空目录列举应成功");
        assert!(msg.contains("roq q add"), "空态应指引如何保存：{msg}");
        let err = show_in(&base, "nope").unwrap_err();
        assert!(err.contains("roq q list"), "show 不存在应指引 list：{err}");
        add_in(&base, "zz-last", "SELECT 2", false).expect("add zz 应成功");
        add_in(&base, "aa-first", "SELECT 1", false).expect("add aa 应成功");
        let listing = list_in(&base).expect("列举应成功");
        let aa_pos = listing
            .find("aa-first")
            .expect("列表应含 aa-first");
        let zz_pos = listing.find("zz-last").expect("列表应含 zz-last");
        assert!(aa_pos < zz_pos, "列表应按名字典序：{listing}");
        let shown = show_in(&base, "aa-first").expect("show 应成功");
        assert_eq!(
            shown,
            format!("{QUERY_FILE_HEADER}SELECT 1"),
            "show 应原样输出含头注释"
        );
        assert!(show_in(&base, "../evil").is_err(), "非法名不得触达读取");
        let _ = fs::remove_dir_all(&base);
    }
}
