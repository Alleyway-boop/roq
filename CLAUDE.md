# roq 开发守则

只读 MySQL 查询 CLI（Rust 单二进制，~2.5MB）。安全是产品本体：**任何改动不得削弱三层只读保障**。
用户可见文本、注释、文档一律中文；代码标识符英文。

## 常用命令

- `cargo test` —— 改任何代码前先跑，提交前必须全绿（含闸门注入负例）
- `cargo build --release` + `cp target/release/roq.exe ./roq.exe` —— 根目录 roq.exe 供本机调用（已 gitignore）
- 发版：改 `Cargo.toml` 版本 → `git tag vX.Y.Z` → push tag，CI 自动三平台构建并挂 GitHub Releases

## 红线（改动需最严格审查）

1. `gate.rs` 是安全核心，**fail-closed**：拿不准一律拒绝；修改闸门必须附带注入负例测试
2. 禁词扫描是词边界匹配（`update_time` 不误伤）；字符串字面量含禁词会被拒——这是有意行为
3. 多语句检测引号/注释感知；块注释一律拒绝（防 `/*! */` 版本注释注入）；`EXPLAIN ANALYZE` 拒绝（会真实执行）
4. 审计日志与结果存档**不可关闭**——这是只读工具的存在意义
5. 快捷子命令（tables/schema/explain）只生成 SQL，必须走同一条闸门+审计+存档链路，不得绕过
6. `skill/roq/SKILL.md` 由 `include_str!` 嵌入二进制，**不得写入本机路径或内网库拓扑**（仓库公开分发）

## 模块速览

| 文件 | 职责 |
| --- | --- |
| `cli.rs` | 参数解析与用法文本（手写解析，未知 flag 直接报用法错误） |
| `config.rs` | profile 加载（`~/.roq/profiles.d` 一项目一文件，跨文件同名节报错） |
| `gate.rs` | 本地只读闸门（白名单/多语句/禁词/注释） |
| `query.rs` | 连接 + 会话只读断言 + 执行 |
| `render.rs` | tsv/json/csv 渲染与行数、单元格限额 |
| `audit.rs` | 审计日志（月度 jsonl）与结果存档 |
| `cmd_config.rs` / `cmd_log.rs` / `cmd_skill.rs` / `cmd_diff.rs` | config / log / skill / diff 子命令 |
| `skill/roq/SKILL.md` | AI 使用技能规范源，随二进制分发（`roq skill install`） |

## 约定

- 新子命令遵循 `cmd_*.rs` 模式：解析在 `cli.rs`、执行在 `cmd_*.rs`、`Result<String, String>` 统一出口打印
- 退出码：0 成功 / 1 用法配置错 / 2 闸门拒绝 / 3 连接执行错 / 4 diff 比对不一致
- 防覆盖类写操作（config add、skill install）一律拒绝静默覆盖，回读校验
