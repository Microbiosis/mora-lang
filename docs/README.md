# Mora 文档总索引

> **文档定位**：Mora 语言的设计、实现、规范和形式化语义文档
> **目录结构**：按文档类型和生命周期分类，不是按时间线排列

## 目录结构

```
docs/
├── README.md                  ← 本文档（总索引）
├── mora-spec.md               ← 语言规范（20 章，权威来源）
├── ARCHITECTURE.md            ← 架构快照（v0.50 baseline）
├── ARCHITECTURE_DESIGN_v2.md  ← 架构设计 v2
├── CODEBASE_ANALYSIS.md       ← 代码库分析（v0.51 baseline）
├── METAMORPHOSIS_ROADMAP.md   ← 蜕变路线（战略级）
├── PHASE_ALPHA_IR_DESIGN.md   ← IR 设计（Alpha 阶段）
├── multi-agent-design.md      ← 多 Agent 设计
├── orchestrate_v50_implementation_plan.md  ← 编排 v50 实现计划
├── influences.md              ← 语言影响分析（9 语言）
├── learning-plan.md           ← 学习计划（6 阶段）
│
├── semantics/                 ← 形式化语义（9 个核心构造）
│   ├── README.md              ← 语义文档索引
│   ├── value-equality.md      ← Value 相等性
│   ├── let-binding.md         ← Let 绑定
│   ├── binary-ops.md          ← 二元数值操作
│   ├── if-then-else.md        ← 条件语句
│   ├── for-loop.md            ← For 循环
│   ├── function-call.md       ← 函数调用
│   ├── pattern-match.md       ← 模式匹配
│   ├── tool-declaration.md    ← Tool 声明语法
│   └── tool-type-system.md    ← Tool 类型系统
│
├── reports/                   ← 审计报告 / 质量门禁报告
│   ├── README.md              ← 报告索引
│   ├── AUDIT_ARCHITECTURE_REPORT.md
│   ├── AUDIT_ZEROTRUST_V0_34.md
│   ├── CONCURRENCY_AUDIT_REPORT.md
│   └── ARCHITECTURE_BUG_DETECTION_REPORT_2026-07-11.md
│
├── research/                  ← 技术研究 / 竞品分析
│   ├── README.md              ← 研究索引
│   ├── RESEARCH_PRIMITIVES_*.md
│   ├── research-langgraph.md
│   ├── report_dive-into-langgraph.md
│   ├── langchain_analysis_report.md
│   ├── langgraph4j_analysis_report.md
│   ├── chatdev-analysis-report.md
│   └── agents-cli_analysis.md
│
├── _archive/                  ← 历史会话工作流（v0.08-v0.34 时代产物）
│   └── *.md
│
└── superpowers/               ← Superpowers 相关文档
```

## 文档类型规则

| 位置 | 类型 | 生命周期 | 举例 |
|------|------|---------|------|
| `docs/*.md` | 核心设计 / 规范 / 路线 | 长期维护 | `mora-spec.md`, `ARCHITECTURE.md` |
| `docs/semantics/*.md` | 形式化语义 | 随语言演进 | `value-equality.md` |
| `docs/reports/*.md` | 审计 / 质量报告 | 历史归档 | `AUDIT_ZEROTRUST_V0_34.md` |
| `docs/research/*.md` | 调研 / 竞品分析 | 探索性 | `research-langgraph.md` |
| `docs/_archive/*.md` | 历史会话工作流 | 已归档 | `workflow-v0.24-parser-migration.md` |
| 根目录 `*.md` | 仓库核心配置 | 长期维护 | `README.md`, `CHANGELOG.md` |
| 根目录 `AGENTS_*.md` | Agent 规则 | 长期维护 | `AGENTS_CODE_MODIFICATION.md` |

## 命名规则

- **核心设计文档**：`PASCAL_CASE.md` 或 `kebab-case.md`
- **语义文档**：`kebab-case.md`（描述性，如 `value-equality`）
- **报告**：`TYPE_DESCRIPTION_DATE.md`（如 `AUDIT_ARCHITECTURE_REPORT.md`）
- **研究**：`RESEARCH_TOPIC.md` 或 `topic_analysis_report.md`
- **历史工作流**：`vX.Y-description.md`（已在 `_archive/`）

## 文件归属约束

1. **`docs/` 根目录只能放核心设计文档** — 规格、架构、路线图、学习计划
2. **`docs/reports/` 只能放审计报告** — 不混入研究、不混入核心设计
3. **`docs/research/` 只能放调研分析** — 不混入报告、不混入核心设计
4. **`docs/semantics/` 只能放形式化语义** — 不混入其他类型
5. **根目录只能放仓库配置和 Agent 规则** — 不放报告、不放研究
6. **`docs/_archive/` 只放已归档的历史产物** — 不混入活跃文档
