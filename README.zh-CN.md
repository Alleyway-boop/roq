# roq

> **可以放心指向生产库的只读 MySQL 客户端。**

其他语言：[English](README.md)

[![Release](https://img.shields.io/github/v/release/Alleyway-boop/roq)](https://github.com/Alleyway-boop/roq/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

`roq` 是一个命令行 MySQL 查询工具——**写操作在构造层面就不可能发生**，而非靠约定。终端与数据库
之间隔着三层相互独立的只读防护，每次调用都留审计与存档，整体以单个约 2.5 MB 的静态二进制交付，
无任何运行时依赖。

它为生产 / 测试 / 开发库的排查与数据核对而生——人直接用，AI agent 通过随附技能用。

## 亮点

- **构造级只读**——本地语句闸门、服务端 `READ ONLY` 会话（回读断言）、输出限额三层层层设防；
  被拒或失败的尝试同样留痕。
- **审计与存档**——每次调用写入按月滚动的 JSONL 审计日志；结果全量落盘，终端显示截断也不影响存档。
- **Agent 友好**——`roq skill install` 让 Claude Code / Codex CLI 在任意项目正确使用 roq；
  `--format json --quiet` 输出单文档便于机器消费。
- **凭据落盘即密**——Windows 上 DPAPI 加密绑定本机用户；Linux/macOS/CI 走环境变量凭据。
- **内置对账**——`roq diff` 同一条 SQL 跑两个 profile，不一致时给出差异行样本。
- **单静态二进制**——Rust 编写，约 2.5 MB，零运行时依赖。

## 安装

任选其一：

1. **下载 Release**（免 Rust 工具链）：从 [GitHub Releases](https://github.com/Alleyway-boop/roq/releases)
   下载对应平台压缩包（`windows-x64` / `macos-arm64` / `macos-x64` / `linux-x64`），解压即得
   `roq` 二进制与 `skill/` 目录。
2. **cargo 安装**：`cargo install --git https://github.com/Alleyway-boop/roq`
3. **源码编译**：`git clone` 本仓库后 `cargo build --release`。

把二进制所在目录加入 `PATH`，然后（可选）安装 AI 技能，让任意项目里的 agent 按正确姿势用 roq：

```bash
roq skill install --global    # Claude Code：~/.claude/skills/roq/
roq skill install --codex     # OpenAI Codex CLI：~/.codex/skills/roq/
```

技能随二进制打包、版本永远一致；升级 roq 后重跑 `roq skill install --force` 同步。

## 快速上手

```bash
# 1. 注册一个库（~/.roq/profiles.d/ 下一配置一文件）
roq config add myproj-prod --host db.example.com --user roq_ro --password '***' --database mydb

# 2. 验证连接（连接 + 只读断言 + 版本探测）
roq config test myproj-prod

# 3. 先看有哪些表，再查询
roq tables --profile myproj-prod
roq columns t_order --profile myproj-prod          # 字段、类型、注释
roq --profile myproj-prod --format json --quiet "SELECT COUNT(*) AS cnt FROM t_order"
```

Windows 上密码落盘那一刻即经 DPAPI 加密。生产 profile 命名含 `prod` 时，每次查询都会打印显式提醒。

## 用法

```
roq [--profile 名] [--format tsv|json|csv] [--max-rows N] [--max-cell N]
    [--out 文件] [--quiet] [--config 路径] [--list] "单条只读SQL"
roq tables [flags]                    # SHOW TABLES
roq schema <表|库.表> [flags]         # SHOW CREATE TABLE（表名严格校验）
roq columns <表|库.表> [flags]        # SHOW FULL COLUMNS（字段/类型/注释）
roq explain "SQL" [flags]             # 自动加 EXPLAIN 前缀
roq diff --profile 左 --profile 右 "SQL" [--json] [--sample N] [flags]
roq config add|test|remove|list|migrate ...
roq log [--today|--month|--last N] [--profile 名] [--outcome 类型] [--json]
roq skill install [--global|--project|--codex] [--force]
```

快捷子命令只生成 SQL——**与手写查询走同一条闸门 + 审计 + 存档链路**。

### 接受的语句

每次调用仅接受**单条**语句，首关键词必须是
`SELECT / SHOW / EXPLAIN / DESC / DESCRIBE / WITH / TABLE / VALUES`，
其余一律在触网前拒绝。

### 输出格式（`--format`，默认 `tsv`）

| 格式 | 说明 |
|---|---|
| `tsv` | 制表符分隔；NULL 显示为 `\N`；格内 `\t` `\n` `\r` 转义为可见序列，保持单行结构 |
| `json` | 单文档：`{"columns":[...],"rows":[[...]],"rows_returned":N,"truncated":bool,"elapsedMs":N}`；NULL 为 `null` |
| `csv` | RFC 4180；NULL 为空字段 |

json/csv 是交换格式——**单元格不截断**（`--max-cell` 仅影响 tsv 终端显示）。

### 限额与截断

- 默认上限：500 行（`--max-rows`）、tsv 单格 200 字符（`--max-cell`）——只为防终端刷屏，存档永远全量。
- `SHOW CREATE TABLE` 免单格截断：DDL 即交付物，整格上屏（显式传 `--max-cell` 时除外）。
- tsv 单格被截时 stderr 明示，并指路存档里的全量副本。
- `--out 文件`：结果按所选格式全量写文件，替代终端输出与自动存档；审计 `result` 记该路径。
- `--quiet` 抑制 stderr 提示（prod 提醒、耗时、截断提示）；错误与退出码不受影响，审计照写。

### 退出码

| 码 | 含义 |
|---|---|
| `0` | 成功 |
| `1` | 用法 / 配置错误 |
| `2` | 语句被只读闸门拒绝 |
| `3` | 连接 / 执行错误 |
| `4` | `roq diff` 比对不一致 |

### 用 `roq diff` 对账

同一条 SQL 在两个 profile 各跑一遍，比对列名（有序）、行数、类型感知的行多重集摘要（顺序无关、
重复敏感——数值 `1` 与文本 `'1'` 不会误判一致）。不一致时按多重集差集展示单侧独有行样本
（`--sample N`，默认每侧 5 行，`0` 关闭；json 输出在 `sample.leftOnly` / `sample.rightOnly`）。
恰好两个 `--profile` 必须显式给出，杜绝对错库；两侧各写一条审计。

## 连接配置

### 用 `roq config` 管理配置

```bash
roq config add <名> --host 主机 --user 用户 --password 密码 --database 库 [--port 3306] [--ssl] [--file 路径]
roq config add <名> ... --password-env 变量名     # 凭据不落盘
roq config add <名> ... --update [--password 新密码]  # 原地重录既有配置
roq config test <名>
roq config remove <名> --yes
roq config list
roq config migrate [--config 路径]                # 历史明文密码就地转 DPAPI 密文
```

配置在 `~/.roq/` 下：`profiles.conf`（单文件）或 `profiles.d/*.conf`（**一项目一文件**），两者可
并用；跨文件同名 `[节]` 直接报错（拒绝静默遮蔽）。`--list` 显示每个配置的 host:port、库名与来源
文件。`config add` 写后回读校验，同名已存在拒绝静默覆盖（重录用 `--update`）。

`--profile` 缺省为 `dev`；生产库建议显式命名 `prod`——工具每次查询都会打印提醒。

### 密码加密（Windows）

Windows 上 `--password` 落盘前即经 DPAPI 加密（`enc:v1:dpapi:` 前缀 + base64 密文），密文绑定当前
Windows 用户：配置文件拷到其它机器或其它用户名下无法解开。密码仅在录入一刻以明文存在。非 Windows
平台拒绝 `--password`，改用 `--password-env`（变量未设置时连接前报错，绝不回退明文）。
`roq config migrate` 把存量明文密码就地转密文：写前解密校验、只改 `password=` 行、幂等可重跑。

**加密不防什么**：以你的 Windows 用户身份运行的代码同样能解开密文。加密防的是顺手读取与文件泄漏
（截图、误传、归档外流）——真正的墙在数据库端：

### 使用只读数据库账号

工具的三层保障只约束"经 roq 发出的语句"；账号自身的权限是最后一道防线，也是客户端加固无法替代的
唯一一道。为 roq 建专用账号，只授 `SELECT`：

```sql
CREATE USER 'roq_ro'@'%' IDENTIFIED BY '强密码';
GRANT SELECT ON 目标库.* TO 'roq_ro'@'%';
-- 不授予 INSERT / UPDATE / DELETE / DROP 等任何写权限
```

凭据即使泄漏，也做不了任何写操作。

### 手工配置

直接手写 INI 文件仍然支持：

```ini
[名称]
host=db.example.com
port=3306            ; 可选，缺省 3306
user=roq_ro
password=密码        ; 或 password_env（两者互斥，并存报错）
password_env=变量名  ; 密码从该环境变量读；未设置时连接前报错（不回退明文）
database=库名
ssl=true             ; 可选：TLS 加密（不校验证书）
```

`--config 路径` 接受文件或目录（目录则扫描其中 `*.conf`，按文件名排序加载）。服务端不支持 TLS 报
`Client requires secure connection` 时，去掉 `ssl=true` 即可。

## 安全模型：三层只读保障

1. **本地语句闸门**——任何网络 I/O 之前先行拦截。
   - 首关键词白名单；**引号/注释感知**的多语句检测（字符串里的 `;` 合法，如 `LIKE '%&nbsp;%'`）。
   - 行注释（`-- `、`#`）自动剥离——注释里的禁词不影响放行；引号未闭合拒绝。
   - 块注释 `/* */` 一律拒绝（MySQL 版本注释 `/*! */` 可执行代码）。
   - 拒绝 `EXPLAIN ANALYZE`（会真实执行底层语句）。
   - `SELECT/WITH/TABLE/VALUES` 全句禁词扫描（`INSERT`、`UPDATE`、`DELETE`、`SET`、`INTO`、
     `LOAD_FILE`、`SLEEP` 等），词边界匹配——`update_time` 等列名不误伤。
2. **服务端会话只读**——连接后强制 `SET SESSION TRANSACTION READ ONLY` 并**回读断言**
   （`@@session.transaction_read_only` 必须为 `1`；SET 失败、回读失败、值非 1 一律中止查询），
   并设 30 秒 `max_execution_time` 慢查询保险丝。
3. **输出限额**——行数与单格上限防大结果刷屏；格内转义保真且维持 TSV 单行结构。

## 审计日志与结果存档

自动执行、**不可关闭**——这是只读工具的底账。

- **审计日志**：`~/.roq/logs/roq-YYYYMM.jsonl`，每次调用一行 JSON：
  `ts / profile / db / sql / rows / truncated / ms / outcome / reason / result`。`outcome` 含
  `ok`、`gate-rejected`（含拒绝原因）、`connect-error`、`exec-error`、`config-error`——被拒的
  尝试同样留痕。用 `roq log` 查询（默认今天；`--month` / `--last N` 扩大范围）。
- **结果存档**：当前项目目录 `.roq/results/YYYYMMDD/HHMMSS-毫秒-<表名>-<profile>.tsv`（非项目
  位置退回 `~/.roq/results/`）。文件头 `# ` 元信息块记录时间、库、行数上限与 SQL；**单元格不截
  断**。文件名自动带表名，不打开文件就能认出查了哪张表。项目 `.gitignore` 加一行 `.roq/` 即可。

## 路线图

- [ ] 多数据库驱动：PostgreSQL / SQLite（profile 增加 `driver=` 字段，闸门按方言适配）
- [ ] 免配置直连：`--dsn "mysql://user:pass@host/db"` 一次性连接

版本历史：见 [CHANGELOG.md](CHANGELOG.md)（英文）。

## 许可

[MIT](LICENSE)
