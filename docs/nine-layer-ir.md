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
- **输出**：`CoreFunction`（~25 种 CoreInst）
- **文件**：`src/mir/core.rs` + `src/mir/ehir_to_core.rs` + `src/mir/core_exec.rs`
- **职责**：闭包/ADT/Thunk/Handle 降维为基元
- **关键设计**：SSA 100% 覆盖（无 passthrough）
- **可执行**（v0.104.6 D482）：`src/mir/core_exec.rs` —— Core 层有**自己的
  执行器**（块调度，无 DAG、无优化），可与 MIR 执行器**对拍**

> **Core 层的可执行子集**（D482）：常量 / 拷贝 / 二元运算 / 环境读写 / 列表 /
> 字典 / 索引 / 索引赋值 / 分支 / 跳转 / 返回 / 具名调用 / 值调用 / 方法调用 /
> 模板 / match（含守卫）。
>
> **不支持**（执行器**响亮报错**，不静默降级）：闭包 / task（Mora 的
> `Value::Closure` 装的是 `Arc<MirFunction>`，装不下 Core 闭包）、**区域构造**
> （`Scope` 的并发 / 事务 / 可观测性语义在执行层，顺序执行它会与 MIR 层**静默
> 分叉**）、效果（需要 handler 栈与 `resume`）、ADT 构造 / thunk。
>
> **对拍**：`tests/core_exec_differential.rs` —— 同一程序，MIR 执行器
> （`dag_analyze → dag_optimize → BSP 超步`）与 Core 执行器（块调度）结果必须
> **完全一致**。这正是 D474 退役差分机制时缺的**语义级**对照：当年的差分
> **只比形状、不比语义**（`or`/`and` 短路不赋值就是**两边一起错**，差分抓不到）。
>
> **让 Core 层可执行，当场抓出两个只有执行才暴露的缺陷**：
>
> - **寄存器撞车** —— Core 层的 bump 分配器**没有跳过 FCFG 预分配的寄存器**
>   （MIR 层有这条「寄存器安全契约」）⇒ 顶层降维的 `alloc_reg()` 与预分配号
>   撞车 ⇒ 运行期越界（`index out of bounds: the len is 1 but the index is 1`）。
> - **循环块结构错误** —— 条件 / 初始化与「循环之前的语句」落在**同一块**，
>   而回边跳回那一块 ⇒ `let j = 0` 被**反复重跑** ⇒ 条件恒真 ⇒ **死循环**。
>   （MIR 层是**线性指令流 + pc 定位循环头**，天然没这问题；Core 是块结构 ⇒
>   跳不进块中间 ⇒ 条件 / 初始化必须自己成块。）

> **Core 层的两处结构缺陷均已修**（v0.104.6 D479 / D480），由
> `tests/core_cfg_wellformed.rs` 递归守卫（外层 + 全部嵌套代码区）：
> id 唯一连续 / 目标存在 / 指令与终结指令目标一致 / 无死块 / 无未回填 /
> 无环分支程序无环。
>
> - **D479：块号相撞** —— 分支块号此前是**预分配**的
>   （`next_block_id + 1 / +2 / +3`）而实际是**顺序分配**的，分支里再套分支时
>   外层预留的号与内层实际分配的号相撞 ⇒ 边指错、甚至成环（`if` 里套 `if`
>   实测 3 ↔ 4 成环、出口块不可达）。D479 改成「**先降维、后回填**」。
> - **D480：嵌套代码区被截断** —— `CoreInst::ClosureCreate.body` 与
>   `EnumMatchArm.body` 此前是**单个** `CoreBlock`，装不下多块 CFG ⇒
>   闭包体 / match 分支体里的嵌套分支被截断（体里只保留首块，分支目标指向
>   **并不存在**的块）。实测（D480 前）：
>
>   ```text
>   EnumMatchArm { body: CoreBlock { id: 0,
>       insts: [EnvLoad, Const, BinaryOp, Branch { true_bb: 1, false_bb: 2 }],
>       terminator: Branch { true_bb: 1, false_bb: 2 } } }
>                                    ↑ 目标 1、2 在该体里根本不存在
>   ```
>
>   D480 起 `body` 承载**完整子函数**（`CoreFunction` 形态：块集合 + 入口 +
>   自己的寄存器空间）⇒ 复用同一个「带入口的代码区」概念，不另立第二套类型。
>
> - **D481：保真度**（`tests/core_fidelity.rs`）—— Core 产物里**说谎**的
>   地方比「不完整」更糟，D481 把它们逐个修掉：
>
>   | 构造 | D481 之前 | 现在 |
>   |---|---|---|
>   | `match` 分支的**绑定名** | 恒为空 ⇒ `x when x > 0` 的 `x` 根本不存在 | 从模式里取出 |
>   | `match` 分支的**变体名** | `format!("{:?}", pattern)` ⇒ 连 `Span` 都进去了 | 只留可读形式 |
>   | `break` / `continue` | `CoreInst::Jump(0)` ⇒ 一条**假边**（目标是块 0） | 真的**终结跳转**（break→出口块、continue→条件块） |
>   | 管道 `\|>` | `Const(dst, Nil)` ⇒ 两端全丢 | `Call(dst, left, [right])` |
>   | 闭包的**捕获变量** | 恒为空，且注释谎称「由 EHIR 分析确定」 | 过近似 + 注释说真话 |
>   | **区域构造**（`parallel` / `observe` / `span` / `worker` / `transaction` / `with` / `solve` / `section` / `export`） | 原地摊平 ⇒ 构造**完全不可见** | `CoreInst::Scope`（体是独立嵌套代码区，形状镜像 MIR 层的嵌套 `MirFunction`） |
>   | `ThunkCreate.body` | 单个 `CoreBlock`（与 D480 前的闭包体同款截断） | 完整子函数 |
>
> 影响范围：**仅限 Core 层**。Core 函数**从不执行**（`probe_all_layers` 只统计
> 指令数），可执行的路径是 MIR 层 —— 它的块结构由 `mir/dag.rs` 独立构建，
> 不受此限制。
>
> #### Core 层的**有损登记**（D481 后剩下的）
>
> 下列构造在 Core 层**没有对应原语** ⇒ 只能写 Nil 占位 / 空区域。这是
> **模型**的限制（要整套执行层语义：字符串当代码执行、聚合贡献、模板片段、
> 事务提交/回滚、agent 图），不是降维的疏忽 ⇒ 登记在此，不擅自扩模型。
>
> | 构造 | Core 层表现 | 为什么 Core 装不下 |
> |---|---|---|
> | `eval` | `Const(dst, Nil)` | 「把字符串当代码执行」是执行层语义 |
> | `aggregate` | `Const(dst, Nil)` | 「向聚合器贡献一个值」是执行层语义 |
> | quasiquote | `Const(dst, Nil)`（片段丢失） | 「文本 + 插值」的混合序列不是任何基元 |
> | `commit` / `rollback` | `Const(dst, Nil)` | 事务提交/回滚没有 Core 原语 |
> | `orchestrate` | `Scope{kind:"orchestrate"}`（**空**区域） | agent 图 / 边条件 / state schema 要整套图语义 |
>
> 钉子：`tests/core_fidelity.rs::lossy_constructs_stay_registered` —— 哪天
> 它们变忠实了，本判据会失败并提示更新这张表。

### MIR — Mid-level IR（已有）
- **输入**：`FCFG`（`lower_fcfg` 降维）
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

## 生产管线（v0.104.6 D474 起：**唯一**路径）

```
Token → ParserV3::compile → MirWitness[]（parser 只产 witness，不发任何指令）
                                    ↓
                              witness_to_fcfg → FCFG
                                    ↓
                              lower_fcfg → MirInst[]（执行器消费它）
                                    ↓
                              apply_rules + SSA opt + regalloc
                                    ↓
                              execute（执行器）
```

**EHIR / Core 两层不在产出路径上**（只用于统计）。它们由
`mir::pipeline::probe_all_layers` 单独跑，供 CI 覆盖那两层的可运行性。

> ⚠ **v0.104.6 D474：没有「切换逻辑」了。**
>
> 此前 `cli::compile_and_opt` 的流程是「跑两条 lowering → 逐条差分 → 差分红就
> 回落 `emit.rs` 直出」。D474 把 parser 改成**只产 witness** ⇒ **参考实现这一侧
> 已经不存在** ⇒ 差分退化成**自己跟自己比**（恒真）⇒ 差分机制与
> `MORA_9LAYER=0` 开关**一并退役**。
>
> ⇒ 现在的生产路径是**一条**，没有环境变量分支。

## 等价性由「期望输出」保证（不是差分）

| 判据 | 能发现 | 发现不了 |
|---|---|---|
| 两条路径一致 | 两条路径**不一致** | 两条路径**一起错**到同一个错值 |
| 期望输出 | 值**不对** | —— |

⇒ CI 改为断言**具体且正确**的输出：

| 测试 | 覆盖 |
|---|---|
| `tests/nine_layer_conformance.rs` | 48 类语言构造 → 钉死的期望 stdout |
| `tests/nine_layer_full_census.rs` | 117 个程序（67 仓库真实 + 50 合成）→ 编译 + 运行成功 |
| `tests/nine_layer_unblocked.rs` | 20 种块/声明形态 × 3 优化档 + 57 个 e2e fixture |
| `tests/nine_layer_block_equivalence.rs` | 14 类构造 → 钉死的 `last_expr` |
| `tests/pipeline_conformance.rs` | 11 个代表性 fixture → 钉死的 stdout + 退出码 |
| `tests/pipeline_equivalence.rs` | 68 条语言特性 → 必须编译过 + 优化档位不变 |

> **历史记档**：改写判据时当场抓到**三个真缺陷**（`or`/`and` 短路不赋值、
> `--opt=1` 丢裸字面量的值、parser 手术误删两条解析语义）—— 它们**两边一样错**，
> 逐条差分**结构上抓不到**。这正是「差分只比形状、不比语义」的盲区。

## 短路求值：`or` / `and` 的 rhs 只在必要时求值（v0.104.6 D477）

`or` / `and` 的**求值级短路**是语言的目标语义（spec §4.3.1）。
D477 之前它**只在一条路径上成立**：

| 路径 | 入口 | rhs 的位置 | 是否短路**求值** |
|------|------|-----------|------------------|
| 生产路径 | `witness_to_fcfg` → `lower_fcfg` | `Or` 节点**之前**（rhs 是已算好的寄存器） | ✗ rhs 无条件求值 |
| 直连路径 | `mir::lower` | `JumpIf` **之后**（rhs 在分支里） | ✓ |

⇒ 同一个程序，两条编译路径的**副作用**不同 —— 生产路径上
`let x = true or bump()` 仍会执行 `bump()`。

**根因是节点形状**：`Node::Or { rhs: Reg }` 承载的是**已算好的寄存器**，
而寄存器在节点降级之前就已写好 ⇒ rhs 的副作用早已发生，**结构上无法**
表达「只在某条分支上求值」。

**D477 的修法**：`Node::Or` / `Node::And` 的 `rhs` 改成**未求值的块**
（`Block<M>`），降级时把 rhs 的指令放进「非短路分支」：

```text
JumpIf(l, short)          ; l 为真 ⇒ 走短路（rhs 不求值）
<rhs 块>                  ; ← 只在 fall-through 这条路上求值
BinaryOp(dst, l, .., r)
Jump(end)
short:
Copy(dst, l)
end:
```

⇒ 生产路径与直连路径的指令序列**逐条一致**，且真正兑现短路求值。
Core 层同步改成 `Branch` + 两块（此前它把 or/and 降成普通 `BinaryOp`，
**既不取值短路也不求值短路**）。

回归护栏：`tests/short_circuit_evaluation.rs`（行为 × 三个优化档位 +
两条路径的形状对照）+ `tests/nine_layer_conformance.rs` 的
`or_and_skip_rhs_evaluation`。

## 已知限制（显式合法性登记）

> ⚠ **v0.104.6 D473：本节连同「显式合法性登记」机制一起删除。**
>
> 登记表的用途是让**回落** legible（区分「已知缺口」与「管线回归」）。
> D473 补齐最后一批构造后，**回落机制本身被删除** —— 9 层是**唯一**生产路径，
> 不再存在「已知缺口 / 回归」之分。
>
> ⇒ 差分从**生产门**降为 **CI 等价性验证**：
> 9 层是**规范**，`emit.rs` 参考实现是**对照物**（golden model），
> 二者不一致 = 9 层的 bug，由 CI 抓住；生产不再为此付费。

**v0.104.6 D473 之前**的状态（保留作历史记录）：

v0.104.6 起，「哪些构造还没接进 9 层管线」不再��**沉默的二值回落**，
而是一张**显式、按名、带原因**的登记表（`src/mir/legalization.rs`），
语义对应 MLIR 的 `ConversionTarget` / `applyPartialConversion`：

| 状态 | 含义 | 生产行为 |
|------|------|----------|
| 合法 + 差分绿 | 全部构造已接入，管线产出可用 | 走 9 层快路径 |
| 含未 legalize 构造 | 功能覆盖缺口（**预期**） | 按**构造名**回落，stderr 点名 |
| 全合法 + 差分红 | 管线在已支持构造上出 bug（**回归**） | 回落 + stderr 打出「回归」醒目措辞 |

D472 之后登记项由 4 条减为 1 条（`orchestrate`），D473 之后减为 **0 条**：

| 阶段 | 登记项 | 说明 |
|------|--------|------|
| D472 前 | `worker` / `transaction` / `perform_bare` / `eval_bare` | witness 层**根本没有**这些构造的表示 |
| D472 后 | `orchestrate` | 9 层 FCFG 的编排表示承载不下 agent 体 / 边条件等运行时元数据 |
| D473 后 | **无** | 9 层覆盖全部语言构造，回落机制删除 |

> **D472 之前是 4 条**（`worker` / `transaction` / `perform_bare` /
> `eval_bare`）——它们的根因是**witness 层根本没有该构造的表示**（parser
> 只发射 MirInst、把结构压成扁平 `Sequence`），9 层降级链结构上就产不出对应
> 指令。D472 补齐 witness 变体 + FCFG 节点 + 降维 arm 后全部合法化。
>
> D472 一并修好了 4 类**原登记表漏掉**的构造（`aggregate` / `commit` /
> `rollback` / `pipe`）——它们此前同样差分必红，只是从未被登记，于是回落时
> 既不点名也不报错（**静默回落**）。

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
| nine_layer_conformance | 49 | 语言构造的**期望输出**（D474 建表，D477 增补 1 条） |
| nine_layer_full_census | 117 | 全部程序编译 + 运行成功 |
| nine_layer_unblocked | 20×3 + 57 | 块/声明形态 × 优化档 + 全部 e2e fixture |
| pipeline_conformance | 11 | 代表性 fixture 的**期望输出**（D474 新建） |
| short_circuit_evaluation | 3 | `or`/`and` 的**求值级短路**（行为 × 优化档 + 两路径形状，D477 新建） |
| execute_naming_hygiene | 3 | 执行入口命名一致性（旧名不得复活，D478 新建） |
| core_cfg_wellformed | 5 | Core 层 CFG **结构良构性** + 模型限制登记（D479/D480 新建） |
| core_fidelity | 6 | Core 层**保真度**（产物里该看见的东西不能丢，D481 新建） |
| core_exec_differential | 3 | Core 执行器 vs MIR 执行器**语义对拍**（D482 新建） |

## 已知遗留（技术债）

> ⚠ **本节此前整表失效，已于 v0.104.6 批次重写。**
>
> 原表列的 4 条「层级违反」全部以 `MirExpr` 跨层使用为根因，而
> **`MirExpr` 已随 v0.92 整体删除**（`src/mir/mod.rs`：
> 「v0.92: expr 模块已删除 —— MirExpr 平行 AST 世界完全消除」）。
> 原表引用的 `typeck/check_mir.rs:18`、`mir/handlers.rs:978,1047,1140`、
> `interpreter/builtins/mod.rs:650` 均已不成立——`check_mir.rs` 早已改为
> 「零 MirExpr 桥接」消费 witness，`mir/handlers.rs` 也已拆成 `mir/handlers/`
> 目录的五个子模块。表中那句「修复路径：MirExpr 可收缩为仅
> orchestrate/pregel 数据构造类型」实际已被**直接删除**执行掉了。
> ⇒ 保留它会让人以为还有一整条 MirExpr 迁移债要还，那是假的。

### 遗留 1：双编译管线并存 —— **v0.104.6 D474 已解决**

`cli::compile_and_opt` 此前每次编译都跑两条完整的 lowering 再逐条差分：

| 路径 | 入口 | 规模 |
|------|------|------|
| A（旧） | `parser_v3/emit*.rs` + `mir/lower.rs` | 6313 行 |
| B（9 层） | `witness_to_fcfg` → FCFG → EHIR → Core → `lower_fcfg` | 4757 行 |

差分绿且构造已 legalize ⇒ 走 9 层产出；否则**回落旧路径**。
实测代价（debug build，300 个 task 的程序）：9 层开 317 ms / 关 213 ms，
即影子管线 + 差分约占总耗时 **31%**。

**D472 补齐了最后一批构造**（`worker` / `transaction` / `eval` / `aggregate` /
`commit` / `rollback` / `pipe` / `orchestrate`），**D473 删除了回落机制**，
**D474 把 parser 改成只产 witness** ⇒ **路径 A 整个消失**：

- 生产路径 = **9 层唯一路径**（`mir::pipeline::compile_optimized`）——
  CLI、`import`、runtime `eval`、REPL **四处全部**走它；
  不做差分、不跑 EHIR / Core（那两层只用于统计）⇒ **31% 开销归零**；
- 差分机制与 `MORA_9LAYER=0` 开关**一并退役** —— 差分的对照物（参考实现）
  已不存在，保留它只会变成**恒真判据**；
- `src/mir/legalization.rs`（显式合法性登记）与三态回落门**一并删除** ——
  没有回落，就没有「已知缺口 vs 回归」需要区分；
- 等价性改由**「期望输出」**判据承担（见上节）—— 它比差分**更强**：
  差分只能发现「两条路径不一致」，期望输出直接钉住**正确值**。

> **历史记档**：D474 的判据改写当场抓到三个真缺陷（`or`/`and` 短路不赋值、
> `--opt=1` 丢裸字面量的值、parser 手术误删两条解析语义）—— 它们**两边一样错**，
> 差分**结构上抓不到**。这是「退役差分必须同时换上期望输出判据」的直接证据。

### 遗留 2：通配符 re-export —— **v0.104.6 D476 已解决**

`src/mir/mod.rs` 的 `pub use inst::*;`（注释写「保持 `crate::mir::dst()`
等旧路径」）是命名空间污染。

**D476 取证后删除**：它其实**什么都没导出** ——
`inst.rs` 里 MirInst 的元数据方法是 **inherent impl**
（`impl MirInst { pub fn dst() … }`），而 inherent 方法**不能**被 glob 再导出，
它们本来就随类型可达（`MirInst::dst()`）；模块级 pub 项只有 `dispatch` 一个，
它真正的出口是 `handlers::mod` 的 `pub use crate::mir::inst::dispatch;`。

⇒ 那行 glob 只是把 `dispatch` 额外提升到 `mir::` 根（**零消费者**），
同时让注释里的「保持 `crate::mir::dst()` 等旧路径」成为**假话**
（`mir::dst()` 从来不可用）。删除后 `mir::` 根只剩真正的 9 层 re-export。

### 遗留 3：执行入口命名 —— **v0.104.6 D478 已解决**

`run_mir` 这个名字暗示存在「线性执行器」，而自 v0.59 起仓库只有 DAG/BSP 一种
执行实现。v0.104.6 D467 已把主入口更名为 `execute`。

**D478 把残留收干净**（D467 只改了主入口，命名遗留散在别处）：

- **标识符**：`run_mir_dag` → `execute_program`（它跑的是**整个程序**：顶层 body
  + `main` task，比 `execute` 多一步）；`run_mir_fn` → `execute_fn`。
- **文件名**：`tests/run_mir_equiv_run_dag.rs` → `tests/execute_equiv_run_dag.rs`、
  `tests/run_mir_tail_value.rs` → `tests/execute_tail_value.rs`。
- **注释**：约 200 处引用逐条分类 —— **现时引用**改指现存的名字（`execute` /
  `run_with_signal` / `execute_program`），**历史记档**（v0.59 归并、v0.59
  「`run_mir ≡ run_dag`」假承诺、D467 改名说明）保留旧名但**明写「原名/旧名」**。
- **别名**：`use …::execute as run_mir` 这类把旧名重新引入作用域的写法一并去掉。
- **防回退守卫**：`tests/execute_naming_hygiene.rs` —— ① 代码里不得出现旧名；
  ② 注释里提到旧名必须带历史标记词；③ 文件名不得带旧前缀。

> D478 顺带修掉两处**批量替换会造成语义漂移**的地方：`vm/mod.rs` 模块文档里
> 「## 关于 `run_mir` 这个名字」整段（改名前它说「保留下来是为兼容既有 100+
> 调用点」—— 名字早已改掉，那段已是**假话**），以及 `vm/mod.rs` 测试里
> 「`run_mir_dag` 直调 `dag_analyze`、不跑 `dag_optimize`」的注释（它与
> `engine.rs` 的「改用 `MirHost::dag_cache()`，即 `dag_analyze → dag_optimize
> → prune_sequence_edges`」**互相矛盾**，以后者为准）。

