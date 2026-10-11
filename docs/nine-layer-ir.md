# IR 架构（v0.104.6 起为 5 层）

> v0.89-v0.90 实施（2026-08-21~23）；v0.104.6 移除不在产出路径上的三层。

## 概述

Mora 的编译管线当前**实际产出路径**是 5 层，每层负责单一维度的降维：

```
Token → FCFG → EHIR → Core → MIR
         语法    类型    基元    SSA
```

### 关于原「9 层」（v0.104.6 更正）

早期文档把 `CMIR → LMIR → RIR → JIR → Machine` 画进主链路，但**代码里它们不
在产出路径上**：管线产出的 `MirFunction` 来自 `lower_fcfg(&fcfg)`（直接由 FCFG
降维），CMIR/LMIR/RIR 的产出**只被 debug 打印统计**，JIR/Machine 则由
`mir/jit.rs` 独立承担。v0.104.6 已删除 CMIR / LMIR / RIR 三层（合计 1455 行，
其中 `lmir_to_mir.rs` 是**从未声明模块**的死代码，且含一处
`from_utf8_unchecked` —— `tests/unsafe_safety_docs.rs` 曾将其记为「定时炸弹」：
一旦有人补 `mod lmir_to_mir;` 即变成真实 UB）。

下文的 CMIR / LMIR / RIR / JIR 各节保留为**历史设计记录**，并标注其现状。

## 各层职责

### FCFG — Frontend Control Flow Graph
- **输入**：Token 流（ParserV3::compile 的 MirWitness 产出）
- **输出**：`Node<()>` 结构化控制流图
- **文件**：`src/mir/fcfg.rs` + `src/mir/witness_to_fcfg.rs`
- **职责**：语法→结构化 CFG，无类型信息
- **关键设计**：`Node<M>` 泛型，`M=()` 时为 FCFG，`M=TypeInfo` 时为 EHIR

### EHIR — Early High-level IR
- **输入**：`Node<()>` + `TypeTable`（影子表）
- **输出**：`Node<TypeInfo>`（带类型标签）
- **文件**：`src/typeck/annotate.rs` + `src/typeck/export.rs`
- **职责**：类型标注 + 效果行 + 泛型单态化
- **关键设计**：影子表策略（零侵入 typeck，export_type_table 导出 Substitution）

### Core — 函数式基元
- **输入**：`Node<TypeInfo>`
- **输出**：`CoreFunction`（~20 种 CoreInst）
- **文件**：`src/mir/core.rs` + `src/mir/ehir_to_core.rs`
- **职责**：闭包/ADT/Thunk/Handle 降维为基元
- **关键设计**：SSA 100% 覆盖（无 passthrough）

### MIR — Mid-level IR（已有）
- **输入**：`CoreFunction` 或 emit.rs 直出
- **输出**：`MirFunction`（50 种 MirInst）
- **文件**：`src/mir/mod.rs` + `src/mir/ssa.rs` + `src/mir/optimize/`
- **职责**：SSA 构造 + 优化（Cascades + SSA opt）

### ~~CMIR（并发中间表示）~~（v0.104.6 已删除）

> **现状**：不在产出路径上，已连同其余两层删除。保留本节为历史设计记录。

#### CMIR（并发中间表示）
- **输入**：`CoreFunction`
- **输出**：`CmirBlock`（并发感知节点）
- **文件**：`src/mir/cmir.rs` + `src/mir/core_to_cmir.rs`
- **职责**：串行→并发（BSP/Agent/SIMD 原语）

### ~~LMIR（低层内存布局）~~（v0.104.6 已删除）

> **现状**：不在产出路径上，已连同其余两层删除。保留本节为历史设计记录。

#### LMIR（低层内存布局）
- **输入**：`CmirBlock`
- **输出**：`LmirInst[]` + `LayoutTable`
- **文件**：`src/mir/lmir.rs` + `src/mir/cmir_to_lmir.rs`
- **职责**：Unboxed 原语 + 内存布局 + GC/RC + FFI

### ~~RIR（运行时 IR）~~（v0.104.6 已删除）

> **现状**：不在产出路径上，已连同其余两层删除。保留本节为历史设计记录。

#### RIR（运行时 IR）
- **输入**：`CoreFunction` + `LayoutTable`
- **输出**：`CompiledFunction` + `FunctionVersionTable`
- **文件**：`src/mir/rir.rs` + `src/mir/lmir_to_rir.rs`
- **职责**：函数表 + VTable + 效果表 + Agent 生命周期

### JIR — JIT IR
- **输入**：`CompiledFunction` + `LayoutTable`
- **输出**：`JitEntry`（可执行代码）
- **文件**：~~`src/mir/rir.rs`~~ → v0.104.6 起为 `src/mir/jit.rs`
- **职责**：热点分析 + 增量编译 + guard deopt

### Machine
- **输入**：`JitEntry`
- **输出**：x86-64 机器码
- **文件**：`src/mir/jit.rs`
- **职责**：copy-and-patch JIT（零 LLVM）

## 生产管线（v0.90.3+）

```
Token → ParserV3::compile → MirInst[] + MirWitness[]
                                    ↓              ↓
                            [原管线 fallback]   witness_to_fcfg → FCFG
                                                    ↓
                                              annotate + TypeTable → EHIR
                                                    ↓
                                              ehir_to_core → Core
                                                    ↓
                                              lower_fcfg → MirInst[]（管线产出）
                                                    ↓
                                              apply_rules + SSA opt
                                                    ↓
                                              run_mir / execute（执行器消费管线产出）
```

**切换逻辑**（`cli::compile_and_opt`）：
1. 管线在 apply_rules 之前运行（差分 raw-to-raw）
2. 类别级差分绿 → 管线产出经同一套优化后作为返回值
3. 差分红 → 自动回落 emit.rs 直出
4. `MORA_9LAYER=0` 禁用，`MORA_9LAYER_DEBUG=1` 输出差分诊断

## 差分验证

**两级差分**（`tests/nine_layer_differential.rs`）：
1. **类别级**：顶层指令序列逐条类别比较（忽略寄存器编号）
2. **执行级**：双管线各自 run_mir + run_main_task，last_expr 必须相等

**覆盖**：19 个 e2e fixture 全部通过双级差分。

## 已知限制（显式合法性登记）

v0.104.6 起，「哪些构造还没接进 9 层管线」不再是**沉默的二值回落**，
而是一张**显式、按名、带原因**的登记表（`src/mir/legalization.rs`），
语义对应 MLIR 的 `ConversionTarget` / `applyPartialConversion`：

| 状态 | 含义 | 生产行为 |
|------|------|----------|
| 合法 + 差分绿 | 全部构造已接入，管线产出可用 | 走 9 层快路径 |
| 含未 legalize 构造 | 功能覆盖缺口（**预期**） | 按**构造名**回落，stderr 点名 |
| 全合法 + 差分红 | 管线在已支持构造上出 bug（**回归**） | 回落 + stderr 打出「回归」醒目措辞 |

当前登记的**未 legalize 构造**（4 条，与
`tests/nine_layer_fallback_reasons.rs` 的 `FALLBACK_REASONS` 一一对应）：

| 构造 | 未 legalize 原因 |
|------|------|
| `worker` | 9 层降级链里没有 `Worker` 类别 |
| `transaction` | 9 层降级链里没有 `Transaction` 类别 |
| `perform_bare` | 裸 perform：emit 侧不发射该 `Perform` |
| `eval_bare` | `eval` 顶层裸形态：管线整条丢弃（产出 0 条指令） |

**登记判据是结构签名，不是裸类别** —— 合法程序
`handle Ai { g = perform Ai("hello") } { … }` 的 MIR 里同样含 `Perform`
指令，但它被 `Handle` 包裹、走 9 层差分通过，故**不得**被标成裸 perform。
判定因此区分「裸 perform」与「handle 包裹的 perform」。

三态回落与登记的一致性由三组测试共同锁住，任何一侧漂移 CI 即红：

- `tests/nine_layer_fallback_census.rs` — 哪些构造会回落（实测差分）
- `tests/nine_layer_fallback_reasons.rs` — 回落**原因**的判别标记
- `tests/nine_layer_legalization_registry.rs` — 登记与前两者**双向**一致
  （不虚报 / 不漏报 / 不误伤合法构造）

新增未 legalize 构造时，三张表必须一起改。

## 测试覆盖

| 测试组 | 数量 | 覆盖 |
|--------|------|------|
| lib tests | 836 | 全量单元测试 |
| e2e | 23 | 全部 fixture 端到端 |
| executor_switch | 17 | 生产路径执行验证 |
| nine_layer_differential | 19 | 双级差分等价 |

## 已知架构层级违反（技术债）

以下违反 AGENTS.md §0.2（Foundation→Kernel→Expression 层级），需要后续重构：

| 违反 | 位置 | 说明 |
|------|------|------|
| typeck 依赖 MirExpr | `typeck/check_mir.rs:18` | Foundation 层（类型检查）依赖 Expression 层（MirExpr） |
| handlers 用 MirExpr | `mir/handlers.rs:978,1047,1140` | Kernel 层（效果处理）用 Expression 层做 dispatch |
| interpreter 用 MirExpr | `interpreter/builtins/mod.rs:650` | Runtime 层直接引用 Expression 层 |
| 通配符 re-export | `mir/mod.rs:68` `pub use inst::*` | 命名空间污染 |

**根因**：MirExpr 是旧 parse→lower 路径的产物，9 层架构用 Node<M> 替代。MirExpr 跨层使用是历史遗留，不是设计意图。

**修复路径**：当 9 层管线完全替代旧路径后，MirExpr 可收缩为仅 orchestrate/pregel 数据构造类型，handlers/interpreter 改用 CoreInst 或 Node<TypeInfo>。
