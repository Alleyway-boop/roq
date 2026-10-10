---
name: roq
description: >-
  Use the roq CLI for ALL MySQL database lookups — row counts, data verification,
  schema peeks, prod/test/dev incident investigation.
  当需要查库、核对数据、排查生产/测试/开发库、执行 SELECT/SHOW/EXPLAIN 时使用本技能。
  Never write ad-hoc DB access scripts (jshell/Python/JDBC/mysql client) — roq enforces
  read-only at three layers and is the single sanctioned path for database reads.
---

# roq — 只读数据库查询

所有 MySQL 查询一律通过 `roq` 完成，禁止现写任何直连数据库的脚本（jshell/Python/JDBC 等）。
roq 在本地闸门、服务端会话只读、输出限额三层强制只读；安全行为由仓库内 `cargo test` 全量单测（含注入负例）常驻保障，改工具代码必须先过测试。

## 安装与就绪检查

- 安装：GitHub Releases 下载对应平台压缩包（含二进制与本 skill），或 `cargo install --git https://github.com/Alleyway-boop/roq`
- 就绪：`roq --version` 能出版本号即已在 PATH；不在则把二进制所在目录加入 PATH，或暂用完整路径调用
- 安装本技能：`roq skill install --global`（Claude，写入 `~/.claude/skills/roq/`）或 `--codex`（Codex CLI，写入 `~/.codex/skills/roq/`）；内容与二进制版本一致，目标已存在且不同时须加 `--force`

## 基本用法

```
roq [--profile 名] [--format tsv|json|csv] [--max-rows N] "单条只读SQL"
快捷子命令（生成 SQL 后走同一闸门+审计链路）：
  roq tables / roq schema <表|库.表> / roq explain "SQL" / roq log [--last N] ...
```

- **AI 消费数据默认 `--format json --quiet`**：stdout 是单文档 `{"columns":[...],"rows":[[...]],"rows_returned":N,"truncated":bool}`（NULL→null，单元格不截断），stderr 干净；给人看才用 tsv
- 排查起点用快捷命令：`roq tables`（看有哪些表）→ `roq schema t_xxx`（看结构）→ 再写查询；`roq explain "..."` 看执行计划
- 大结果 `--out 文件`（替代终端输出与自动存档）；查审计用 `roq log --last 20` / `--outcome gate-rejected`
- 仅接受单条：`SELECT / SHOW / EXPLAIN / DESC / DESCRIBE / WITH / TABLE / VALUES`
- tsv 格式：首行列名；`NULL` 显示为 `\N`；`\t \n \r` 转义为可见序列（保真且单行）
- 默认限额 500 行（`--max-rows`）；tsv 终端单格 200 字符（`--max-cell`；json/csv 与存档不截断）
- 退出码：`0` 成功 / `1` 用法配置错 / `2` 闸门拒绝 / `3` 连接执行错

stderr 尾行有审计摘要（profile、库、行数、耗时、"会话只读"、结果存档路径；`--quiet` 会抑制，人看场景保留它向用户报告）。

## 审计与存档（自动，不可关）

- 审计日志：`~/.roq/logs/roq-YYYYMM.jsonl`（Windows 在 `%USERPROFILE%` 下）——每次调用一行 JSON（时间/环境/库/SQL/行数/耗时/结果路径），被闸门拒绝的尝试也留痕。用户问"查过什么"时直接读这个文件。
- 结果存档：项目目录 `.roq/results/YYYYMMDD/时分秒-毫秒-<表名>-<profile>.tsv`（非项目目录则退回 `~/.roq/results/`）。
  文件头 `# ` 开头的行是元信息（时间/库/SQL），TSV 数据从首个非 `#` 行开始；单元格不截断、`\t \n \r \N` 为转义。
  复查历史查询结果优先读存档，避免重查数据库。

## 环境 profile

配置在 `~/.roq/`：`profiles.conf`（可选单文件）+ `profiles.d/*.conf`（**一项目一文件**，跨文件同名节会报错）。
本机有哪些环境、生产库对应哪个 profile，以 `roq --list` 输出为准（含每个配置的来源文件）。

为新项目接入（AI 可自行完成，写的是本机配置文件、非数据库操作）：

1. **取凭据**：从该项目后端配置读 datasource 段（Spring 项目在 `application-{env}.yml` 的 `spring.datasource`：host/port/username/password/url 里的库名）
2. **注册**：`roq config add <项目-环境> --host 主机 --user 用户 --password 密码 --database 库 [--port 3306] [--ssl] [--file 路径]`——命名建议 `<项目>-<dev|test|prod>`，生产必须独立命名含 `prod` 以触发显式提醒；凭据不想落明文用 `--password-env 环境变量名` 替代 `--password`（变量未设置时连接前报错，不回退）
3. **验证**：`roq config test <项目-环境>`——连接+只读断言+版本探测，通过后即可查询
4. 管理：`roq config list`（含来源文件）；`roq config remove <名> --yes`（仅删单节文件，多节文件须手动编辑）

要点：同名已存在会拒绝（改配置才需手动编辑文件）；`--ssl` 启用 TLS 加密（不校验证书），报 `Client requires secure connection` 说明服务端不支持 TLS，去掉配置里的 ssl=true 即可；无需手写 INI，手工方式（~/.roq/profiles.d/ 一项目一文件）仍支持。

## 硬性安全规则

1. **只查不改**：roq 只放行读语句。任何修复/订正 SQL 一律写成 `.sql` 文件放到对应项目的 `sql/` 目录交给用户，绝不尝试执行。
2. **闸门拒绝（exit=2）= 改写语句，不是绕过**：把被拒词从语句中去掉或换表达；绝不为通过闸门而拼接、编码、注释伪装语句。
3. 生产查询前先向用户说明要查什么；用户明令"只查不改"时，全程零写操作（roq 本身也写不了）。

## 闸门限制（fail-closed，按拒绝信息改写）

- 多语句检测是**引号/注释感知**的：字符串里的分号合法（`LIKE '%&nbsp;%'` 可用）；字符串外出现 `;` 才拒
- 行注释 `-- `/`#` 会被自动剥离（注释里写禁词不影响放行）；块注释 `/* */` 一律拒绝
- 引号未闭合直接拒绝
- 禁词全句扫描：INSERT/UPDATE/DELETE/SET/INTO/LOAD/LOCK/SLEEP 等；列名 `update_time` 不误伤，但字符串字面量里含禁词也会被拒——换措辞或缩小查询范围
- `EXPLAIN ANALYZE` 被拒（会真执行）；只可用普通 `EXPLAIN`
- `SHOW CREATE TABLE` 可用（SHOW 系列跳过禁词扫描）

## 常见模式

```bash
# 排查三步（快捷命令）
roq tables --profile dev --format json --quiet
roq schema t_question --profile dev
roq explain "SELECT COUNT(*) FROM t_question" --profile test

# 计数核对（AI 消费：json + quiet）
roq --profile dev --format json --quiet "SELECT COUNT(*) AS cnt FROM t_question"

# 按主键取数（id 多为雪花长整型，用引号包裹避免精度问题）
roq --profile prod --format json --quiet "SELECT id, no, LEFT(title,80) AS title FROM t_question WHERE id='2104894858388815873'"

# 大结果落盘不灌终端
roq --profile dev --out big.tsv "SELECT * FROM t_question LIMIT 10000"

# 查审计（谁查了什么/被闸门拒了什么）
roq log --last 20 --outcome gate-rejected
```

json 输出截断信息看顶层 `"truncated"`；长文本字段先 `LEFT(col, N)` 截断再查，避免整段 HTML 灌爆上下文。

## 故障排查

- exit=3 `Client requires secure connection`：该端点不支持 TLS，去掉配置里的 `ssl=true`
- 终端中文乱码：输出为 UTF-8，管道/重定向正常；个别控制台代码页不同导致显示乱码，不影响数据正确性
- profile 不存在：先 `--list` 确认名称与来源
