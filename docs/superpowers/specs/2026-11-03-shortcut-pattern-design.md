# 快捷子命令模糊过滤（tables/columns 可选 pattern）——设计

日期：2026-11-03
状态：已批准（对话中选定方案 A）

## 意图

排查起点路径（`roq tables` → `roq columns` 是 SKILL.md 明定的首选路径）目前只能全量输出：
`roq tables` 一次甩出全库表名，`roq tables mark` 直接报"不接受位置参数"。
模糊搜索能力其实已存在于 SQL 形态（`SHOW TABLES LIKE '%mark%'`，SHOW 开头有禁词豁免），
缺的是快捷子命令的便捷入口。本次把高频动作包进来。

## 目标

- `roq tables [模式]`：无参行为不变（`SHOW TABLES`）；有参 → `SHOW TABLES LIKE '<模式>'`
- `roq columns <表|库.表> [模式]`：第二参数为列过滤 → `SHOW FULL COLUMNS FROM \`表\` LIKE '<模式>'`
- SKILL.md 顺序规则修正（反例形式）一并交付：
  子命令必须紧跟 roq、flag 放子命令之后（`roq columns t_xxx --profile dev` ✓；
  `roq --profile dev columns t_xxx` ✗ 会被当作 SQL 送去闸门拒绝）

## pattern 语义

- 显式含 `%` → 按 LIKE 原样使用（`roq tables t_report%` 做前缀匹配）
- 其余（含 snake_case 的 `_`）→ 视为子串自动包 `%模式%`（`roq tables mark` ≡ `LIKE '%mark%'`）

> 修订（实现计划阶段）：原设计为"含 `%` 或 `_` 原样"，但表名/列名以 snake_case 为主流、
> 天然含 `_`，若按 `_` 原样则 `roq columns t task_num` 会退化为近似精确匹配，违背"子串搜索"
> 直觉。故收紧为仅 `%` 触发原样；`_` 在自动包装后仍保留 LIKE 单字符通配语义，结果集为超集。

## 约束与非目标

- 生成 SQL 照走同一条闸门+审计+存档链路（红线 5），不改闸门
- `schema` 不加过滤（看单表 DDL，无过滤需求）
- 不引入正则等新语法；只做 LIKE 语义
- 转义：模式进单引号字面量，`'` 双写为 `''`（SQL 标准转义，
  与 SQL mode 无关；且与闸门 strip_and_check 的引号状态机兼容）。
  即便漏转，闸门"引号未闭合"也是 fail-closed 兜底

## 备选方案与取舍

- 方案 B（只改文档教 SQL 形态）：零代码风险，但 `roq tables mark` 直觉尝试仍被拒——弃
- 全家族统一 flag 前置（方案二）：diff 双 --profile 与 config add 的 --profile
  存在语义歧义，代价超过收益——另行评估，不在本次范围

## 文档同步

- cli.rs usage()：`roq tables [模式]`、`roq columns <表|库.表> [模式]`
- SKILL.md：速览行、排查起点、示例区（含顺序规则反例）
- README.md + README.zh-CN.md 双语同步
- CHANGELOG 归 [Unreleased]

## 测试（TDD 先写）

- `tables mark` → `SHOW TABLES LIKE '%mark%'`
- `tables mark%` → `SHOW TABLES LIKE 'mark%'`（前缀匹配原样）
- `tables` → `SHOW TABLES`（回归）
- `columns t_xxx task_num` → `SHOW FULL COLUMNS FROM \`t_xxx\` LIKE '%task_num%'`
- `columns t_xxx` → 原行为（回归）
- `columns` 三个位置参数 → 用法错误
- pattern 含单引号 → 生成 SQL 转义正确且能过闸门（负例：原样拼接会因引号未闭合被拒）

## 留给实现者

- 审计/存档的表名标注对 `SHOW ... LIKE` 语句的提取行为需确认（现有 SHOW TABLES 同款语句，预期无回归）
