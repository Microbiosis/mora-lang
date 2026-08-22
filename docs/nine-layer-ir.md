# 9 层 IR 架构

> v0.89-v0.90 实施，2026-08-21~23

## 概述

Mora 的编译管线采用 9 层 IR 架构，每层负责单一维度的降维：

```
Token → FCFG → EHIR → Core → MIR → CMIR → LMIR → RIR → JIR → Machine
         语法    类型    基元    SSA   并发    物理    运行时  JIT    原生
```

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

### CMIR — Concurrent MIR
- **输入**：`CoreFunction`
- **输出**：`CmirBlock`（并发感知节点）
- **文件**：`src/mir/cmir.rs` + `src/mir/core_to_cmir.rs`
- **职责**：串行→并发（BSP/Agent/SIMD 原语）

### LMIR — Low-level MIR
- **输入**：`CmirBlock`
- **输出**：`LmirInst[]` + `LayoutTable`
- **文件**：`src/mir/lmir.rs` + `src/mir/cmir_to_lmir.rs`
- **职责**：Unboxed 原语 + 内存布局 + GC/RC + FFI

### RIR — Runtime IR
- **输入**：`CoreFunction` + `LayoutTable`
- **输出**：`CompiledFunction` + `FunctionVersionTable`
- **文件**：`src/mir/rir.rs` + `src/mir/lmir_to_rir.rs`
- **职责**：函数表 + VTable + 效果表 + Agent 生命周期

### JIR — JIT IR
- **输入**：`CompiledFunction` + `LayoutTable`
- **输出**：`JitEntry`（可执行代码）
- **文件**：`src/mir/rir.rs`（JitBackend trait）
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
                                              lower_fcfg → MirInst[]（9 层产出）
                                                    ↓
                                              apply_rules + SSA opt
                                                    ↓
                                              run_mir（执行器消费管线产出）
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

## 已知限制

以下场景触发回落到原管线（emit.rs 直出）：

| 场景 | 原因 | 状态 |
|------|------|------|
| `commit` / `rollback` | witness 为占位空 Sequence | 回落 |
| `eval` / `aggregate` | witness 为占位空 Sequence | 回落 |
| Orchestrate（Loop/Pregel/MoA） | FCFG 降维丢弃配置 | 回落 |
| for 循环值传递 | 管线路径 for 循环返回值不同 | 待修复 |
| while continue 值传递 | 管线路径 while 块最后表达式识别 | 待修复 |

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
