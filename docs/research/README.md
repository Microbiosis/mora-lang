# Mora Research

> **类型**：技术研究、竞品分析、设计调研、语言特性挖掘
> **来源**：MCP 搜索 / 人工研读 / 跨项目对比
> **生命周期**：探索性文档，部分结论可能融入 `docs/` 核心文档

## 目录

| 文件 | 主题 |
|------|------|
| [`RESEARCH_PRIMITIVES_MASTER.md`](RESEARCH_PRIMITIVES_MASTER.md) | 语言原语研究总纲 |
| [`RESEARCH_PRIMITIVES_MASTER_v2.md`](RESEARCH_PRIMITIVES_MASTER_v2.md) | 语言原语研究 v2 — 补充特性 |
| [`RESEARCH_PRIMITIVES_6NEW_PROJECTS.md`](RESEARCH_PRIMITIVES_6NEW_PROJECTS.md) | 6 个新项目的原语研究 |
| [`research-langgraph.md`](research-langgraph.md) | LangGraph 研究笔记 |
| [`report_dive-into-langgraph.md`](report_dive-into-langgraph.md) | LangGraph 深入分析 |
| [`langchain_analysis_report.md`](langchain_analysis_report.md) | LangChain 分析报告 |
| [`langgraph4j_analysis_report.md`](langgraph4j_analysis_report.md) | LangGraph4j 分析报告 |
| [`chatdev-analysis-report.md`](chatdev-analysis-report.md) | ChatDev 分析报告 |
| [`agents-cli_analysis.md`](agents-cli_analysis.md) | Agents CLI 分析 |

## 与 `docs/` 核心文档的关系

- 调研结论若被采纳为语言设计，应从本文档提炼到 `docs/mora-spec.md`
- 已定稿的设计文档留在 `docs/` 根目录（如 `ARCHITECTURE.md`）
- 仍在探索阶段的研究放在此处

## 命名规则

- `RESEARCH_{TOPIC}.md` — 系统性研究
- `{topic}_analysis_report.md` — 竞品/项目分析
- `report_{topic}.md` — 深入分析报告
- 小写 kebab-case 用于跨工具会话产物
