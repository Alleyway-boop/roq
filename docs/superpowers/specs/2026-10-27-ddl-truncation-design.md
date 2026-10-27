# 设计：DDL 查询不再被 tsv 单格截断（2026-10-27）

## 意图

agent 用 `roq schema <表>` 拿完整 DDL 是 SKILL.md 明文引导的路径，但默认 tsv 终端输出把
单格切到 200 字符加 `…`——DDL 几乎必然超过，agent 每次都要"发现截断 → 从 stderr 翻存档
路径 → 读存档文件"绕一圈。本次让 DDL 交付一步到位，并把残余的截断从静默变为明示。

## 现状事实

- tsv 终端单格截断发生在 `query.rs` 显示层（`--max-cell` 默认 200）；`--out` / json / csv
  与存档均不截断。
- `--out` 已有先例：写文件即全量（`display_max_cell = usize::MAX`）。本次豁免与之同构。
- 单格截断无任何明示：stderr 的"结果被截断"仅在**行数**超限时出现。
- 闸门 `gate()` 返回规范化 SQL（剥注释、去尾分号），`extract_tokens` 全大写比较，
  execute 内可按 token 序列识别语句类型，快捷命令与手写 SQL 两种入口统一覆盖。

## 目标

1. `SHOW CREATE TABLE`（含 `roq schema` 快捷命令）终端 tsv 输出免单格截断。
2. tsv 单格确被截断时，stderr 明示提示并指路存档。
3. SKILL.md 与实际行为同步。

## 非目标

- 不动闸门 / 审计 / 存档链路（红线：快捷命令只生成 SQL、走同一链路；豁免仅作用于终端
  显示层）。
- 不改 json / csv 行为（本就不截断）。
- `roq columns` 的长字段注释（>200 字符）暂不豁免——注释超 200 字符属少数，留观察。

## 方案取舍

- **A（选定）：语句级豁免 + 截断明示 + 文档同步。** 豁免只对 `SHOW CREATE TABLE` 生效，
  防刷屏的 200 字符上限对其余数据单元格原样保留。改动小、直击痛点。
- B（否决）：只改 SKILL.md 把"读存档"正规化。每次拿 DDL 仍两步，agent 不守文档时依旧
  踩坑。
- C（否决）：调大或取消全局单格上限。防刷屏设计初衷回退，大 JSON 字段会灌爆上下文。

## 决定

1. **识别**：`render.rs` 新增 `is_show_create(sql) -> bool`，与 `table_slug` 并列
   （同属"从 SQL 提取信息"），基于 `extract_tokens` 判定首三个 token 为
   SHOW / CREATE / TABLE。
2. **显式参数优先**：`Cli` 增加 `max_cell_explicit`（解析到 `--max-cell` 即置位）。
   显式传了 `--max-cell` 时豁免不生效——明确的限流要求不该被静默覆盖。
3. **豁免**：`query.rs` 的 `display_max_cell` 改为
   `--out` 或（SHOW CREATE TABLE 且未显式传 `--max-cell`）→ 不截断。
4. **明示**：TSV 终端流中记录是否有单格被截（渲染前后长度比较），查询完成后（存档路径
   已知时）stderr 打一行：
   `[roq] tsv 单元格超 --max-cell=N 已截断；全量见存档 <path>（或改用 --format json）`。
   归 notice（`--quiet` 抑制）；`--out` / json / csv 模式天然无此提示。
5. **SKILL.md 同步**：限额描述补"SHOW CREATE TABLE 免截断"；stderr 摘要描述提截断提示。
6. **测试**：`is_show_create` 判定（大小写/空白/反引号/非 SHOW 语句）、
   `--max-cell` 显式置位、豁免组合逻辑（提炼为纯函数加单测）。
   闸门未改动，不触发注入负例红线。

## 留给实现者

- 截断提示与既有摘要 notice 的先后顺序（提示在前更醒目，摘要收尾）。
- `display_max_cell` 组合逻辑提炼纯函数的位置（`render.rs` 或 `query.rs`，就近即可）。
