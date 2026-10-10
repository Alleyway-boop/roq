# roq — 只读数据库查询工具（read-only query）

强制只读、低资源占用的命令行数据库查询工具。Rust 编写，静态二进制约 2.5MB，无运行时依赖。
适用于生产库/测试库的安全排查：**任何情况下都无法执行写操作**。

## 用法

```
roq.exe [--profile 名称] [--max-rows N] [--max-cell N] [--format tsv|json|csv]
        [--out 文件] [--quiet] [--config 路径] [--list] "单条只读SQL"
roq tables [--profile 名称 ...]               # 快捷：SHOW TABLES
roq schema <表|库.表> [--profile 名称 ...]    # 快捷：SHOW CREATE TABLE（表名严格校验，反引号包裹）
roq explain "SQL语句" [--profile 名称 ...]    # 快捷：自动加 EXPLAIN 前缀
roq skill install [--global | --project | --codex] [--force]  # 安装 AI 使用技能（SKILL.md）到 Claude Code / Codex
roq diff --profile 左 --profile 右 "SQL" [--json] [--max-rows N]  # 同一条 SQL 两库对账比对
```

- 快捷子命令只生成 SQL，**与手写查询走同一条闸门+审计+存档链路**；同样支持 `--profile/--format/--out/--quiet` 等查询 flag
- 仅接受**单条**语句：`SELECT / SHOW / EXPLAIN / DESC / DESCRIBE / WITH / TABLE / VALUES`
- 输出格式（`--format`，默认 tsv）：
  - **tsv**：制表符分隔；NULL 显示为 `\N`；`\t` `\n` `\r` 为转义序列
  - **json**：单文档对象 `{"columns":[...],"rows":[[...]],"rows_returned":N,"truncated":bool,"elapsedMs":N}`；NULL 为 `null`
  - **csv**：RFC 4180；NULL 为空字段
  - json/csv 是交换格式，**单元格不截断**（`--max-cell` 仅 tsv 终端显示生效）
- `--out 文件`：结果写入文件（纯数据、按 `--format`、不截断），替代终端输出与自动存档，审计 `result` 记此路径
- `--quiet`：抑制 stderr 提示（prod 提醒/耗时统计）；错误与退出码不受影响，审计照写
- 未知 flag 直接报用法错误（不会静默拼进 SQL）
- 退出码：`0` 成功；`1` 用法/配置错误；`2` 语句被闸门拒绝；`3` 连接/执行错误；`4` diff 比对不一致
- **diff 对账**：同一条 SQL 在两个 profile 各跑一遍，比对列名（有序）、行数、行多重集摘要（顺序无关、重复敏感）；
  恰好两个 `--profile` 必须显式给出（防对错库）；任一侧截断时摘要仅覆盖已取前缀并标注；两侧各写一条审计

## 连接配置

配置**无需手写**，用内置子命令管理：

```
roq config add <名> --host 主机 --user 用户 --password 密码 --database 库 [--port 3306] [--ssl] [--file 路径]
roq config add <名> --host ... --password-env 环境变量名 ...   # 凭据不落明文，密码改从环境变量读
roq config test <名>        # 连接 + 只读断言 + 版本探测
roq config remove <名> --yes
roq config list
```

`add` 默认写入 `~/.roq/profiles.d/<名>.conf`（一配置一文件，含凭据勿外传），写后回读校验；
同名已存在时拒绝覆盖（修改请手动编辑）；`remove` 只删"仅含该配置节"的文件且须 `--yes`，多节文件提示手动编辑。

手工方式仍然支持。默认加载（存在即生效，两者可并用；Windows 在 `%USERPROFILE%` 下，Linux/macOS 在 `$HOME` 下）：

- `~/.roq/profiles.conf` —— 单文件，所有 [节] 混在一起
- `~/.roq/profiles.d/*.conf` —— **目录扫描，一项目一文件**（如 `project-a.conf`、`project-b.conf`）

`--config 路径` 可显式指定**文件或目录**（目录则扫描其中 `*.conf`，按文件名排序加载）。
不同文件里同名 `[节]` 直接报错（拒绝静默遮蔽），`--list` 会显示每个配置的 host:port、库名与来源文件（配置名与实连库不符一眼可辨）。

```ini
[名称]
host=数据库主机
port=3306          ; 缺省 3306
user=用户
password=密码      ; 与 password_env 二选一（同节并存会报错）
password_env=变量名 ; 可选，密码从该环境变量读，凭据不落明文；变量未设置时连接前报错（不回退明文）
database=库名
ssl=true           ; 可选，启用 TLS 加密（不校验证书）；服务端不支持时报 Client requires secure connection，去掉即可
```

`--profile` 缺省为 `dev`；建议生产库命名为 `prod` 并显式指定（工具会打印提醒）。

## 三层只读保障

1. **本地语句闸门**（不触网即拦截）
   - 首关键词白名单；**引号/注释感知**的多语句检测（字符串里的 `;` 合法，如 `LIKE '%&nbsp;%'`）
   - `-- `/`#` 行注释自动剥离（注释里的禁词不影响放行，剥离后发往服务器，语义等价）；引号未闭合拒绝
   - 拒绝块注释 `/* */`（防 MySQL 版本注释 `/*! */` 注入）
   - 拒绝 `EXPLAIN ANALYZE`（会真实执行底层语句）
   - SELECT/WITH/TABLE/VALUES 全句禁词扫描（INSERT/UPDATE/DELETE/SET/INTO/LOAD_FILE/SLEEP…），
     词边界匹配：`update_time`、`create_time` 等列名不误伤
2. **服务端会话只读**：连接后强制 `SET SESSION TRANSACTION READ ONLY` 并**回读断言**
   `@@session.transaction_read_only = 1`（SET 失败、回读失败、值非 1 三种情况一律中止查询），
   并设 30 秒 SELECT 执行上限防慢查询
3. **输出限额**：默认最多 500 行、单格 200 字符（`--max-rows` / `--max-cell` 可调），防大结果刷屏；
   单元格内 `\t` `\n` `\r` 转义为可见序列（保真且不破坏 TSV 单行结构）

## 审计日志与结果存档

每次调用自动记录（不可关闭——这是只读工具的审计底账）：

- **审计日志**：`~\.roq\logs\roq-YYYYMM.jsonl`（按月，集中跨项目）
  每行一条 JSON：`ts / profile / db / sql / rows / truncated / ms / outcome / reason / result`
  `outcome` 含 `ok`、`gate-rejected`（含拒绝原因）、`connect-error`、`exec-error`、`config-error`——被拒的尝试同样留痕
  用 `roq log` 查询（默认今天）：

  ```
  roq log [--today | --month | --last N] [--profile 名] [--outcome ok|gate-rejected|...] [--json]
  # --last N 为最近 N 条，跨月扫描全部日志文件；--today（默认）/--month 只看当月
  ```
- **结果存档**：当前项目目录 `\.roq\results\YYYYMMDD\HHMMSS-毫秒-<表名>-<profile>.tsv`
  - 文件头为 `# ` 元信息块（时间 / 库 / 行数上限 / SQL），TSV 数据体从首个非 `#` 行开始
  - **单元格不截断**（全量保真；`--max-cell` 只影响终端显示）
  - 文件名自动带表名（取 FROM/JOIN 后首个表标识符），不打开文件也能认出查了哪张表
  - 在家目录等非项目位置运行时退回 `~\.roq\results\`；项目里 gitignore 加一行 `.roq/` 即可忽略

## 安装

任选其一：

1. **下载 Release**（推荐，免 Rust 工具链）：[GitHub Releases](https://github.com/Alleyway-boop/roq/releases) 下载对应平台压缩包（windows-x64 / macos-arm64 / macos-x64 / linux-x64），解压即得 `roq` 二进制与 `skill/` 目录
2. **cargo 安装**：`cargo install --git https://github.com/Alleyway-boop/roq`
3. **源码编译**：见下节

装好后把二进制所在目录加入 PATH，然后安装 AI 使用技能（供 AI agent 在任意项目直接正确使用 roq）：

```
roq skill install --global    # Claude Code：写入 ~/.claude/skills/roq/（跨项目生效）
roq skill install --codex     # OpenAI Codex CLI：写入 ~/.codex/skills/roq/（全局）
```

skill 内容随二进制打包、版本永远一致；升级 roq 后重跑一次 `install --force` 即可同步。

## 编译

```
cd C:\Data\tools\roq
cargo build --release
cp target\release\roq.exe .\roq.exe
```

## 泛化路线图（后续）

- [ ] 多数据库驱动：PostgreSQL / SQLite（profile 增加 `driver=` 字段，闸门按方言适配）
- [ ] 免配置直连：`--dsn "mysql://user:pass@host/db"` 一次性连接
- [x] 输出格式：`--format tsv|json|csv`（v0.5.0）
- [x] `--explain` 自动加 EXPLAIN 前缀查看执行计划（v0.6.0，子命令形态 `roq explain`）
- [x] 结果落盘：`--out 文件` 避免大结果过终端（v0.5.0）
- [x] 排查快捷子命令：`roq tables` / `roq schema <表>`（v0.6.0）
- [x] 分发：tag 触发 CI 三平台 Release；`roq skill install` 随二进制分发 AI 使用技能（v0.7.0）
- [x] 对账：`roq diff` 同 SQL 双 profile 比对；`--list` 显示库名；json 带耗时（v0.8.0，源自使用者反馈）
