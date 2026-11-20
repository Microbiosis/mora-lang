# `docs/` — 文档索引

> **规则：每个主题只有一份事实源。** 本文件是入口；写新文档前先在此登记，
> 避免再次出现「同一功能 6 份报告」的历史（见 `_archive/README.md`）。
>
> ⚠ `docs/` 整体在 `.gitignore` 里（`.gitignore:8`），故本目录内容**不入版本控制**，
> 仅 `docs/nine-layer-ir.md`、`docs/decisions/`、`docs/_archive/` 下的少数文件被强制跟踪。

## 规范与当前事实（写代码前优先看这两份）

| 文档 | 是什么 | 时效 |
|------|--------|------|
| [`mora-spec.md`](mora-spec.md) | **语言规范**（20 章）——语法 / 语义 / 标准库面 | 当前（已按 v0.110.0 Wave 1/2/3 校准） |
| [`architecture-diagram.md`](architecture-diagram.md) | **模块级架构图**——分层、依赖图、逐模块职责、单一 Control SSA 编译与执行主路径 | 当前（v0.110.0 Wave 1/2/3） |
| [`decisions/`](decisions/) | 架构决策记录（ADR 风格） | 当前 |
| [`nine-layer-ir.md`](nine-layer-ir.md) | **历史 5/9 层 IR 管线记录**——已被 v0.110.0 Wave 2 的 `MirWitness[] → ControlProgram` 单遍 Control SSA IR 取代 | 历史归档（v0.89–v0.104.6） |

## 设计立场与路线图

| 文档 | 是什么 | 时效 |
|------|--------|------|
| [`influences.md`](influences.md) | 语言设计影响源（9 个语言各贡献一个基因） | 稳定 |
| [`paradigm-shift-to-functional.md`](paradigm-shift-to-functional.md) | 为什么选函数式作 AI-native scripting language 的底色 | 稳定（立场） |
| [`learning-plan.md`](learning-plan.md) | 语言特性融入计划（v2，6 阶段） | 稳定（计划） |
| [`fp-impl-roadmap.md`](fp-impl-roadmap.md) | 函数式底层改造实施路线图（v0.78+ → v0.85） | 计划，部分已落地 |
| [`lisp-influences.md`](lisp-influences.md) | Lisp 系特质的系统分析 + 落地审计 + 路线图 | v0.88+ 路线图 |
| [`lisp-absorption-audit.md`](lisp-absorption-audit.md) | Lisp 特征吸收的**实现状态审计** | v0.87 快照 |
| [`lisp-inspiration-analysis.md`](lisp-inspiration-analysis.md) | Lisp 底层架构对比分析 | 分析文（非状态） |
| [`LEARNING_REPORT_MINIMAL_CHANGE_PRINCIPLE.md`](LEARNING_REPORT_MINIMAL_CHANGE_PRINCIPLE.md) | 「最小修改原则」的踩坑复盘（英文） | 经验教训 |

## 审计与报告（**带日期的快照**，非当前事实）

| 文档 | 快照基线 |
|------|----------|
| [`audit/MIR_COMPILER_AUDIT_2026-07-25.md`](audit/MIR_COMPILER_AUDIT_2026-07-25.md) | 2026-07-25；其前提描述的「前端产出 `Vec<MirExpr>`」已随 v0.92 作废 |
| [`reports/ARCHITECTURE_BUG_DETECTION_REPORT_2026-08-01.md`](reports/ARCHITECTURE_BUG_DETECTION_REPORT_2026-08-01.md) | v0.75.10 / 2026-08-01 |

## 归档

历史实施工件（完成报告 / 阶段计划 / 补丁 / 编码损坏文档）见
[`_archive/README.md`](_archive/README.md)。**不可作为实现依据。**

## 与仓库其它位置的关系

- `AGENTS.md` — **项目级 AI 约束的事实源**（已入版本控制）。§1.2 给出当前执行管线与模块职责。
- `CHANGELOG.md` — 变更日志。**版本号以 `Cargo.toml` 为准**；代码注释里的 `v0.104.6 D<n>` 是未发布批次的工作标签，不是已发布版本。
- `src/` — 代码事实的唯一来源。本目录任何内容与之冲突时，**以 `src/` 为准**。
