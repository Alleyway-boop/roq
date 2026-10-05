# roq — 只读数据库查询工具（read-only query）

强制只读、低资源占用的命令行数据库查询工具。Rust 编写，静态二进制约 1.5MB，无运行时依赖。
适用于生产库/测试库的安全排查：**任何情况下都无法执行写操作**。

## 用法

```
roq.exe [--profile 名称] [--max-rows N] [--max-cell N] [--config 路径] [--list] "单条只读SQL"
```

- 仅接受**单条**语句：`SELECT / SHOW / EXPLAIN / DESC / DESCRIBE / WITH / TABLE / VALUES`
- 输出 TSV（制表符分隔）；NULL 显示为 `\N`；控制字符替换为空格
- 退出码：`0` 成功；`1` 用法/配置错误；`2` 语句被闸门拒绝；`3` 连接/执行错误

## 连接配置

配置**无需手写**，用内置子命令管理：

```
roq config add <名> --host 主机 --user 用户 --password 密码 --database 库 [--port 3306] [--ssl] [--file 路径]
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
不同文件里同名 `[节]` 直接报错（拒绝静默遮蔽），`--list` 会显示每个配置来自哪个文件。

```ini
[名称]
host=数据库主机
port=3306          ; 缺省 3306
user=用户
password=密码
database=库名
ssl=true           ; 可选，是否启用 TLS（部分云数据库端点不宣告 TLS，加了反而握手失败）
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
- **结果存档**：当前项目目录 `\.roq\results\YYYYMMDD\HHMMSS-毫秒-<表名>-<profile>.tsv`
  - 文件头为 `# ` 元信息块（时间 / 库 / 行数上限 / SQL），TSV 数据体从首个非 `#` 行开始
  - **单元格不截断**（全量保真；`--max-cell` 只影响终端显示）
  - 文件名自动带表名（取 FROM/JOIN 后首个表标识符），不打开文件也能认出查了哪张表
  - 在家目录等非项目位置运行时退回 `~\.roq\results\`；项目里 gitignore 加一行 `.roq/` 即可忽略

## 编译

```
cd C:\Data\tools\roq
cargo build --release
cp target\release\roq.exe .\roq.exe
```

## 泛化路线图（后续）

- [ ] 多数据库驱动：PostgreSQL / SQLite（profile 增加 `driver=` 字段，闸门按方言适配）
- [ ] 免配置直连：`--dsn "mysql://user:pass@host/db"` 一次性连接
- [ ] 输出格式：`--format json` / `--format csv`
- [ ] `--explain` 自动加 EXPLAIN 前缀查看执行计划
- [ ] 结果落盘：`--out 文件` 避免大结果过终端
