# roq Oracle 支持 — 设计文档

日期：2026-11-16
状态：待批准

## 意图与动因

roq 目前是只读 MySQL 查询 CLI。使用者日常工作中也需要用同样的方式（三层只读保障 + 审计存档 + 快捷子命令）查询 Oracle 库——目标体验是"像查 MySQL 一样日常使用"，不是偶尔直查。

核心承诺不变：**任何改动不得削弱三层只读保障**；Oracle 路径必须走同一条闸门 + 审计 + 存档链路。

## 目标 / 非目标 / 反目标

### 目标

1. `engine=oracle` 的 profile 全链路可用：任意只读查询、`tables`、`columns`、`schema` 快捷子命令（`columns` 为 builder check 补入：日常高频入口，体验对齐要求覆盖它）。
2. 三层只读保障在 Oracle 上等价成立：
   - 第一层闸门：方言感知（见设计），fail-closed 精神不变；
   - 第二层会话只读：`SET TRANSACTION READ ONLY`，失败即中止；
   - 第三层（账号权限）与审计/存档链路：原样复用，不可关闭。
3. 保持单二进制、零本地依赖（选纯 Rust thin 驱动）。
4. 测试全本地可跑（闸门负例、配置解析、SQL 生成），不依赖真实 Oracle 实例。

### 非目标（明确不做）

- **`explain` 子命令在 Oracle 上不可用**：Oracle 的 `EXPLAIN PLAN` 会真实写入 PLAN_TABLE（DML），与会话只读断言正面冲突。在 Oracle profile 上调用时报错并解释原因（fail-closed）。
- 不支持 Oracle Database 11.2 及更早版本（thin 驱动下限 12）。
- 不支持 SID 连接方式，仅 `host:port/service_name`。
- 不支持 TCPS/TLS 连接（`engine=oracle` 且 `ssl=true` 时显式报错，不静默忽略）。
- 不支持 PL/SQL 匿名块、存储过程调用（闸门白名单天然拒绝）；`DBMS_METADATA.GET_DDL` 仅由 `schema` 子命令内部生成。
- 不做连接池、不做 async（roq 单次执行模型不变）。

### 反目标（即使技术上成功也算失败）

- Oracle 链路绕过审计/存档。
- 闸门为 Oracle 弱化（例如放行块注释/`/*+ hint */`、放行多语句）。
- 需要用户在每台机器安装 Oracle Instant Client 才能用 Oracle 功能（那就退化成 B 方案，违背选型初衷）。

## 约束

- 项目红线 1–6 全部适用（gate.rs fail-closed、禁词词边界语义、多语句检测、审计不可关、快捷子命令同链路、SKILL.md 不含本机信息）。
- 驱动：`oracledb` crate，26.0.0-beta.6（Oracle 官方 rust-oracledb，纯 Rust thin）。要求 Oracle Database 12+、Rust 1.89+。构建无 C 依赖，CI 三平台不受影响。
- 二进制体积目标：保持个位数 MB（现 ~2.5MB，预计 +1~2MB，发布前实测记录）。
- 语言约定：CLI 可见文本/代码注释/SKILL.md 中文；README 双语同步；CHANGELOG 英文。

## 方案取舍

| 方案 | 说明 | 结论 |
| --- | --- | --- |
| A. `oracledb` 纯 Rust thin（官方 beta） | 零本地依赖，保住产品叙事；代价：beta 成熟度、目标库须 12+ | **选定** |
| B. `oracle` crate（ODPI-C thick） | 稳定、支持 11.2+；代价：每台机器装 ~80MB Instant Client，分发/CI/换机全多一步，产品叙事破产 | 退路（见风险） |
| C. ORDS/网关桥接 | 客户端零依赖但要求服务端部署 ORDS，改变"直连数据库"工具性质 | 否决 |
| D. shell out sqlplus | 无闸门无审计，违反产品本体 | 否决 |

选 A 的理由：beta 驱动的风险在只读场景可控——闸门放行的全是 SELECT，驱动 bug 最坏是报错退出，不伤数据；Oracle 官方正在全力押 thin 路线（与 python-oracledb 同路径）。

## 设计

### 1. 配置（config.rs）

- `Profile` 增加 `engine: Engine`（enum：`Mysql` / `Oracle`）。
- INI 键 `engine=mysql|oracle`，缺省 `mysql`（向后兼容：现有配置零改动）。
- `engine=oracle` 时：`database` 键复用为 service_name；`port` 缺省改 1521（mysql 维持 3306）。
- `engine=oracle` 且 `ssl=true`：加载时显式报错（不静默忽略、不降级）。
- `config add` 增加 `--engine` 参数，默认 mysql；`--engine oracle` 时不接受 `--ssl`。
- 密码体系（DPAPI / password_env）引擎无关，不动。

### 2. 闸门（gate.rs）

`gate()` 增加 engine 参数（或拆出引擎相关的规范化策略）。引擎感知差异点：

| 差异点 | MySQL（现状） | Oracle |
| --- | --- | --- |
| `#` | 行注释 | **普通字符**（字典列名 `OBJ#`、`USER_OBJ$` 合法；现状会把 `#` 后整行当注释截掉，静默改写 SQL——必须修） |
| `--` | 须后跟空白才是注释 | **立即注释**（`5--3` 语义相反） |
| 字符串内反斜杠 | 转义符 | 字面量（Oracle 用单引号加倍 `''` 转义） |
| 反引号 | 标识符引用 | 非法字符，显式拒绝（报"Oracle 不使用反引号"优于含糊的引号未闭合） |
| 首关键词白名单 | SELECT/SHOW/EXPLAIN/DESCRIBE/DESC/WITH/TABLE/VALUES | **SELECT/WITH**（SHOW、DESC/DESCRIBE 是 MySQL/SQL*Plus 方言；EXPLAIN 会写 PLAN_TABLE；TABLE/VALUES 语句是 MySQL 方言） |
| 块注释 `/* */` | 一律拒绝 | 一律拒绝（含 `/*+ hint */`——hint 对只读自查非必需，fail-closed 一致性优先，文档注明） |
| 禁词表 | 共享一套全句扫描（现有表已覆盖：INSERT/UPDATE/DELETE/GRANT/REVOKE/LOCK/EXECUTE/SLEEP…），不新增 Oracle 专属词（`MERGE` 非白名单首词自然拒绝，`FOR UPDATE` 被禁词 UPDATE 挡住，`DBMS_LOCK.SLEEP` 被 SLEEP 挡住） | 同左 |

- `EXPLAIN ANALYZE` 拒绝逻辑保留（仅 MySQL 路径可达；Oracle 白名单无 EXPLAIN）。
- 多语句检测引号感知按引擎分流反斜杠语义后，其余逻辑共享。
- **测试**：oracle 模式负例注入全套（`OBJ#` 不被截断、`5--3` 语义、引号加倍字符串含分号、`BEGIN...END` 拒绝、`SELECT ... FOR UPDATE` 拒绝等），以及 mysql 模式回归测试全绿。

### 3. 连接与第二层（query.rs）

- 连接层抽象：`Connection` enum（`Mysql(mysql::Conn)` / `Oracle(oracledb::Connection)`）或等价 trait，执行链路分流但对外保持"连接 → 会话只读 → 流式取行"同一形状。
- Oracle 第二层：连接成功后立即 `SET TRANSACTION READ ONLY`（须为事务首语句）。与 MySQL 的差异及理由：MySQL 靠回读 `@@session.transaction_read_only` 防代理静默忽略 SET；Oracle 直连（无代理中间层），协议层保证 SET TRANSACTION 不被静默忽略（失败必报 ORA 错误），故不做回读断言。**若后续发现存在静默忽略场景，补回读（如 v$transaction 查询）而不是降级。**
- 慢查询保险丝：Oracle 无会话级 max_execution_time 等价物，靠客户端读超时（READ_TIMEOUT_SECS）兜底；连接超时待对照驱动 API 确认支持情况。
- 流式取行：驱动 cursor 逐行迭代，对齐 `query_iter` 用法；超 `max_rows` 截断语义不变。

### 4. 数据类型与渲染（render.rs）

- NUMBER：按字符串取出并归入 **Text** 类（保 38 位精度，不做 f64 有损转换；与 MySQL 侧 DECIMAL 走 Bytes→Text 的现状对齐，`--format json` 输出 `"123"` 字符串而非 JSON number——38 位大数进 JSON number 本身就有损）。
- DATE/TIMESTAMP：固定格式字符串（不用会话 NLS 格式，保证输出确定可存档）。
- CLOB：读为文本，走现有 max-cell / 存档限额体系。
- BLOB：不读内容，单元格显示 `<BLOB n bytes>`（二进制进 tsv 无意义，fail-safe）。
- NULL/转义：沿用现有单元格转义。
- `is_show_create` 语义扩展为"单格即交付物"判定，Oracle 的 `DBMS_METADATA.GET_DDL` 结果同享截断豁免。

### 5. 快捷子命令（cli.rs）

按 engine 生成 SQL，全部走同一条闸门+审计+存档链路（红线 5）：

| 子命令 | MySQL（现状） | Oracle |
| --- | --- | --- |
| `tables [模式]` | `SHOW TABLES [LIKE '...']` | `SELECT table_name FROM user_tables [WHERE UPPER(table_name) LIKE UPPER('...')] ORDER BY table_name` |
| `columns <表> [列模式]` | `SHOW FULL COLUMNS FROM 表 [LIKE '...']` | `SELECT ... FROM user_tab_columns LEFT JOIN user_col_comments ... [WHERE UPPER(column_name) LIKE UPPER('...')] ORDER BY column_name`（字段/类型/可空/默认值/注释对齐 MySQL 侧信息面；具体列集与 JOIN 细节留 builder） |
| `schema <表\|owner.表>` | `SHOW CREATE TABLE \`表\`` | `SELECT DBMS_METADATA.GET_DDL('TABLE', '表名'[, 'OWNER']) FROM dual`（单段名默认当前 schema；两段名映射第三参 OWNER；标识符大写化；无权限时走正常 ORA 报错路径） |
| `explain` | `EXPLAIN ...` | **生成前即拒绝**：报"Oracle 的 EXPLAIN PLAN 需写 PLAN_TABLE，违反只读保障，不支持"（退出码 1 用法错） |

- LIKE 过滤模式：沿用现有校验（拒绝反斜杠、单引号加倍）——该语义两引擎一致，Oracle 侧同样适用。
- 过滤模式两侧 `UPPER()`：Oracle 字典默认大写存储且 LIKE 大小写敏感，不做 UPPER 会让现有小写过滤习惯**静默返回空集**——对齐项目"拒绝静默空结果"的一贯立场（同 35db267 反斜杠提交的理由）。

### 6. 执行顺序调整（gate 后移）与可观察行为变化

闸门引擎感知使 gate 必须在 profile 加载之后执行（引擎取自 profile），现状"先闸门后查 profile"的顺序反转。三个可观察变化，均接受并在此注明：

- gate-rejected 审计行的 db 字段从 `""` 变为真实库名（审计信息更完整，消费者不破）；
- SQL 与 profile 同时出错时，报错优先级从"闸门拒绝（退出码 2）"变为"profile 问题（退出码 1）"；
- `explain` 在 oracle profile 上的拒绝发生在 profile 加载后，审计类别记 config-error、退出码 1。

红线校验：gate 后移不改变"先闸门后触网"的顺序本身，只改变它与配置加载的相对位置——SQL 在真正发给数据库之前必过闸门，链路性质不变。

### 7. roq diff（引擎感知下）——已确认

- **同引擎 diff**（mysql↔mysql / oracle↔oracle）：支持，两侧各过各自引擎的闸门，各侧独立审计；
- **跨引擎 diff**（mysql↔oracle）：显式拒绝，报"两侧引擎不同、闸门语义与类型口径不同，不支持跨引擎比对"（fail-closed，退出码 1）。

### 8. 审计与存档（audit.rs）

- 审计行 JSONL 增加 `engine` 字段（`"mysql"`/`"oracle"`）；旧行无此字段，`cmd_log` 解析需容忍缺失（按 mysql 解释或不显示）。
- 存档文件命名、表头、限额逻辑不变。

### 9. 文档与发布

- `skill/roq/SKILL.md`：支持引擎说明、Oracle 示例、explain 限制；红线：不含本机路径/内网拓扑。
- `README.md` / `README.zh-CN.md` 双语同步；CHANGELOG 英文记 [Unreleased]。
- 版本：**v1.0.0 大版本**（已确认）。双引擎支持是工具立项以来最大的功能跃迁，且含闸门顺序调整带来的可观察行为变化（第 6 节），以大版本发布；已在 [Unreleased] 的 DDL 免截断随之并入 v1.0.0，不再单发 v0.11.0。

### 10. 测试与验收

- 全部单测本地：gate oracle 负例、config oracle 解析（端口缺省、ssl 报错、engine 缺省 mysql）、cli SQL 生成、audit engine 字段。
- 容器化联调（开发期，已确认采用）：本机 Docker 起 `gvenzl/oracle-free`（23ai Free）实例，覆盖 thin 驱动运行时行为（LOB/NUMBER/cursor 流式/超时）与端到端链路，交付前自验；
- 真实内网实例验收（交付后一次性）：SELECT / tables / columns / schema / 审计落盘核对。
- `cargo test` 提交前全绿（含注入负例）。

## 风险与退路

1. **beta 驱动 bug**：连接层已隔离（enum 分流），退路是换 `oracle` crate（ODPI-C）重写连接层，gate/config/cli 层不动。代价：用户需装 Instant Client。
2. **目标库 < 12**：同上回退 B 方案。
3. **`SET TRANSACTION READ ONLY` 不生效的未知场景**：设计上承诺补回读断言而非降级。
4. **DBMS_METADATA 权限缺失**：`schema` 报 ORA 错误，属正常失败路径，不算功能缺陷。

## 留给 builder 的部分

- `oracledb` beta 的具体 API 形态（cursor 生命周期、LOB 读取方法、连接/读超时选项、`Connector` 字段）需对照 docs.rs 落实。
- NUMBER 按字符串取出的具体调用路径。
- `config add` 的 `--engine` 与既有 flag 校验顺序（先引擎后 ssl 判断）。
- 审计行新增字段的写入位置与 `cmd_log` 展示策略（容忍缺失的具体实现）。
- 连接层抽象的最终形态（enum vs trait）由 builder 按代码实际定，不改变对外行为。
