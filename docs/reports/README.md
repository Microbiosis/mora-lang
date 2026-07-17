# Mora Reports

> **类型**：审计、bug 检测、质量门禁报告
> **来源**：CI 门禁输出 / 手动审计报告 / 会话遗留报告
> **生命周期**：历史产物，随问题修复而归档

## 目录

| 文件 | 日期 | 内容 |
|------|------|------|
| [`AUDIT_ARCHITECTURE_REPORT.md`](AUDIT_ARCHITECTURE_REPORT.md) | 2026-07-06 | 架构审计报告 — 代码组织、模块边界、依赖关系 |
| [`AUDIT_ZEROTRUST_V0_34.md`](AUDIT_ZEROTRUST_V0_34.md) | 2026-07-03 | v0.34 零信任审计 — 安全性、错误处理、并发 |
| [`CONCURRENCY_AUDIT_REPORT.md`](CONCURRENCY_AUDIT_REPORT.md) | 2026-07-07 | 并发审计报告 — 锁粒度、数据竞争、channel 使用 |
| [`ARCHITECTURE_BUG_DETECTION_REPORT_2026-07-11.md`](ARCHITECTURE_BUG_DETECTION_REPORT_2026-07-11.md) | 2026-07-11 | 架构 bug 检测 — 设计缺陷、API 一致性问题 |

## 命名规则

- `{TYPE}_{DESCRIPTION}_{DATE}.md`
- `TYPE` 可选值：`AUDIT`, `CONCURRENCY`, `ARCHITECTURE`, `QUALITY`, `BUG`
- 日期格式：`YYYY-MM-DD`
