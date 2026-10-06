# docs/history —— 历史记录归档

> 本目录存放**已完成**的迁移、审阅与特定消费端接入实录。它们是某个时间点的事实
> （当时的证据、当时的缺口当时的处置），**不属于每轮必读**；当前行为与当前规则
> 以 `docs/` 下的活文档与 `docs/CONTRACT.md` 为准。
>
> 什么时候来查：当前文档解释不了某个行为、做回归调查、追历史决策、
> 或要审"当初从参考实现搬了什么"的时候。
> 如果本目录与活文档冲突，**以活文档为准**。

## 现有文件

| 文件 | 内容 |
|---|---|
| [`2026-10-05-review-resolution.md`](2026-10-05-review-resolution.md) | 2026-10-05 审阅发现归并与处置（原 `docs/11-REVIEW-RESOLUTION.md` 整文件迁移）：文档审阅 53 条按章节归并、Android 复用审阅 12 组风险、误报澄清与仍未验收边界。仍有效的结论已写回各活文档与 ADR，此处保留当时的处置记录 |
| [`mac-integration-2026-10/MIGRATION.md`](mac-integration-2026-10/MIGRATION.md) | `x-spider-mac`（第一个真实外壳）2026-10-01 接入实录：改动清单（可删/必留/新写）、外壳侧两处接线审计的修法、逐项接入状态与实测证据（原 `docs/06` §3、§5.1、§7 原文迁入） |
| [`mac-integration-2026-10/CAPABILITY-PARITY.md`](mac-integration-2026-10/CAPABILITY-PARITY.md) | 组件 ↔ `x-spider-mac`（tag `pre-component-integration`）逐项能力对照与迁移来源（原 `docs/08-CAPABILITY-MAP.md` 2026-10-05 版全文迁入）；当前能力地图见 [`docs/08`](../08-CAPABILITY-MAP.md) |
