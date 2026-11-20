# `docs/` — 文档索引（v0.110.0）

> **规则：每个主题只有一份事实源。** 本文件是 `docs/` 目录的总入口；所有核心文档均纳入 Git 版本控制，并由 CI 中的规范守卫测试（`spec_ebnf_census`、`spec_code_block_census`、`spec_ebnf_parity`、`spec_ebnf_wellformedness`、`pipe_multiline`、`control_quasiquote_and_match`）持续校验。

## 1. 语言规范与当前架构事实（写代码前优先查阅）

| 文档 | 内容定位 | 状态 |
| :--- | :--- | :--- |
| [`mora-spec.md`](mora-spec.md) | **Mora 语言规范（20 章 + 附录）**——词法、语法（§14.2 EBNF）、类型系统、一等 `Tensor` 与 5 项静态检查、自动微分（`grad`/`jvp`/高阶 AD）、`TensorComputeDag` 与 GPU/SIMD 后端、`KVPageAllocator`/`MemoryPool`、Go 式并发、纯 Mora `std/net/http` & `std/mcp`、`MORABC02` 单二进制构建 | **当前事实源（v0.110.0 Wave 1–4 + P1/P2）** |
| [`architecture-diagram.md`](architecture-diagram.md) | **模块级架构图与职责导览**——`src/` 12 个顶层模块、`std/` 标准库、单遍 `MirWitness[] → ControlProgram` SSA 编译管线、堆帧 VM、零依赖 CUDA/Vulkan/Metal/SIMD 后端与 `MORABC02` 字节码打包 | **当前事实源（v0.110.0 Wave 1–4 + P1/P2）** |

## 2. 设计基因、演进计划与架构决策

| 文档 | 内容定位 | 状态 |
| :--- | :--- | :--- |
| [`influences.md`](influences.md) | **语言设计影响源**——9 门语言设计基因在 Mora `v0.110.0` 中的落地映射与底层原语对照 | **已按 v0.110.0 校准** |
| [`learning-plan.md`](learning-plan.md) | **语言特性与架构演进蓝图**——从基础函数式/模式匹配/同像性到 Wave 1–4（一等张量、计算图融合、显存页池、Go 式并发、纯 Mora HTTP/MCP 与单二进制交付）的实现映射 | **已按 v0.110.0 校准** |
| [`decisions/no-borrowed-constraints.md`](decisions/no-borrowed-constraints.md) | **架构决策记录（ADR）**——项目自治原则与核心技术护城河 | **Active（v0.110.0 校准）** |

## 3. 与仓库其它顶层文档的关系

- [`../README.md`](../README.md) — 项目英文总览、安装指南、CLI 速查、语法速览与模块导图（由 `tests/readme_code_block_census.rs` 自动校验其中全部 `mora` 代码块的可解析性与 HM 类型检查）。
- [`../AGENTS.md`](../AGENTS.md) — **项目级工程与架构原则的最高约束**（§0 架构优先与从底层原语完整构建、§1.2 单一生产管线、§3 测试同步原则、§6 禁止兼容桥、§7 概念分类准则）。
- [`../CHANGELOG.md`](../CHANGELOG.md) — 版本演进与架构变更记录（版本号以 [`../Cargo.toml`](../Cargo.toml) 的 `0.110.0` 为唯一事实源）。
- [`../examples/hm_inference_examples.md`](../examples/hm_inference_examples.md) — 统一 `Span` 追踪 HM 类型推断、一等张量静态检查、并发类型与代数效果示例集。
