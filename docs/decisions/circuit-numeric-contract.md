# Decision Log: 电路后端 Layer 0 —— 数值契约（Numeric Contract）

**Date**: 2026-10-20
**Version**: 提案（未实施，v0.104.5 基线）
**Status**: Proposed —— 等待作者裁决

> ⚠ **本文件不入仓**：`.gitignore:8` 排除了整个 `docs/`，
> 故 `docs/decisions/` 下既有两份决策文档同样不在版本控制内。
> 这是仓库既有惯例，本文件遵循之。

## 背景

作者提供了三个外部参考：

| 来源 | 做法 | 目标产物 |
|---|---|---|
| [chenglou/handwired](https://github.com/chenglou/handwired) | 把 JS 的 if-else/loop/state 编译成 matmul + ReLU 电路 | 神经网络（手写权重，0 训练） |
| [percepta.ai/blog/can-llms-be-computers](https://www.percepta.ai/blog/can-llms-be-computers) | 把 WASM 解释器编译进 transformer 权重矩阵 | Transformer（7 层，d_model=36，18 头） |
| [chenglou.me/handwired](https://chenglou.me/handwired/) | 上述两者的 demo 站 | — |

三者的共同范式是**「把程序编译成神经网络」**，分歧在于编译成什么网络。
对 Mora 的意义是新增一条执行后端：MIR → 电路。

按 `AGENTS.md` §0「架构优先 + 完整实现」，任何新后端必须**先建地基**。
本文档定义那条路的**第 0 层**：电路的数值契约。

---

## 一、为什么 Layer 0 是「数值契约」而不是「类型系统」

初稿曾把第 0 层写成「值域/格点类型（Handwired 路线的手工补丁）」。**这个定位是错的**，理由：

- Handwired 的 `range` / `grid` 不是补丁，是**精确性的来源**。
  它的 `emitCompare` 之所以能与 JS 逐位一致，靠的是「操作数是声明范围内的整数」这个前提。
- Percepta 侧同理：复现方报告 JZ/JNZ 与参考解释器 token-for-bit 一致，
  同样依赖「寄存器里的值是范围内的整数」。

⇒ **值域约束是两条路线的共同底座**，且必须在**数值表示**这一层先定，
否则后面的「可编译子集判定」无从判定。

---

## 二、实测：Mora 现有数值语义（v0.104.5 基线）

以下全部由 `target/debug/mora.exe` 实测，探针见 `tests/` 与本文档附录。

### 2.1 事实表

| # | 事实 | 证据位置 |
|---|---|---|
| F1 | `Int + Int` = **精确 i64**（裸 `a + b`） | `src/flow.rs:231` |
| F2 | `Int - Int` / `Int * Int` = **f64 round-trip**（`(a as f64) op (b as f64)).round() as i64`） | `src/flow.rs:526` |
| F3 | `Int > Int` = **f64 舍入后比较** | `src/flow.rs:665` |
| F4 | `Int >= Int` 判据是 `o != NumOrd::Less`（非 `o == GreaterEqual`） | `src/flow.rs:381` |
| F5 | `Int == Float` 跨塔比较走 f64 | `src/flow.rs:736` `values_equal` |
| F6 | `Int / Int`、`Int % Int` = 整数向零截断 + 零检查（D19 已修） | `src/flow.rs:389,441` |
| F7 | BigInt 全程精确；Float⊕BigInt 越界报错（D198/D359） | `src/flow.rs:531-559` |
| F8 | JIT 的 Int×Int 模板按 f64 round-trip 发射 | `src/mir/jit.rs:16,571` |

### 2.2 实测输出（`a = 9007199254740993i` = 2^53+1，`b = 9007199254740992i` = 2^53）

```mora
let a = 9007199254740993i
let b = 9007199254740992i
print(a)        -- 9007199254740993
print(a + 0i)   -- 9007199254740993   ✅ Add 保真
print(a - 0i)   -- 9007199254740992   ❌ Sub 丢一位
print(a * 1i)   -- 9007199254740992   ❌ Mul 丢一位
print(a + 1i)   -- 9007199254740994   ✅
print(a - 1i)   -- 9007199254740991   ❌
print(a * 2i)   -- 18014398509481984  ✅（偶数仍可表示）
```

**⇒ 同一个值，`a - 0i ≠ a + 0i`。** Add 保真、Sub/Mul 静默丢最低位。

### 2.3 比较运算的自相矛盾

```mora
print(a > b)    -- false   ❌ a 明明大于 b
print(a == b)   -- false   （f64 舍入后确实不等）
print(a >= b)   -- true    ✅ 语义上正确
print(a < b)    -- false   ✅
```

**⇒ `a > b` 为 false 而 `a >= b` 为 true**，两个运算符对同一组值给出不自洽答案。
根因是 F3：比较把 Int 转 f64，2^53+1 舍入成 2^53，与 `b` 相等。

```mora
let f = 9007199254740992.0 + 1.0   -- f = 9007199254740992.0
print(a == f)  -- true   ❌ a=...993 与 f=...992.0 比较相等
print(f > b)   -- false
```

### 2.4 与既有审计的交叉验证

`tests/int_overflow_build_divergence.rs:44-45` 独立佐证了 F1/F2 的分叉：

> "不可达的原因：`i64::MIN` 只能由减法造出，而 `Sub`/`Mul` 走 `numeric_op`
> （`Fn(f64, f64) -> f64`）会先转 f64 ⇒ 造不出精确的 `i64::MIN`。"

同一文件还记录了另一条**构建配置相关的静默损坏**：
`Int + Int` 溢出在 debug 下 panic（exit 101）、release 下静默回绕成 `i64::MIN`（D299）。

### 2.5 结论

**Mora 当前的 `Int` 不是一个统一的数。** 它是三条不同算术的合流：

```
Int+Int  →  精确 i64        （F1）
Int-Int  →  f64 round-trip  （F2）
Int>Int  →  f64 比较        （F3/F4）
```

这条分叉**不是**电路后端引入的问题——它是**已经存在于解释器里的**问题。
但它对电路后端是**阻断级**的：电路后端要在编译期把「MIR 上的整数运算」
翻译成「电路上的定点/浮点运算」，若解释器本身对同一个 MIR 指令族给出
两种语义，电路后端就**没有唯一的目标语义可对拍**。

---

## 三、Layer 0 契约：三条候选

Layer 0 必须回答一个问题：**电路上的一个数值位，如何对应 Mora 的一个 `Value::Int`？**

### 方案 A：FP32 定点（Handwired 路线）

- 电路统一用 FP32；`Int` 落成 FP32 数值 + 一个独立的「是整数」位。
- 精确整数上界 **2^24**（Handwired 文档实测：第一个失真整数是 2^24+6）。
- 优点：与 Handwired 直接对标；WebGPU 原生 f32；精度界小 ⇒ 值域分析简单。
- 缺点：**Mora 现有 Int 语义远超 2^24**，绝大多数真实程序落在界外。
  ⇒ 需要**值域分析**证明「这个子程序的 Int 都在 2^24 内」，否则不能编译。

### 方案 B：FP64 定点（与现有 Float 对齐）

- 电路用 FP64；精确整数上界 **2^53**。
- 优点：与 Mora 现有 `Float`（`f64`）同精度；`Int` 在 2^53 内可无损表示。
- 缺点：WebGPU / 大多数 GPU **不原生支持 f64**，拿不到 Handwired 那个
  "JSON 搜索 10 万文档毫秒级" 的并行红利；且 2^53 边界**正是当前
  F2/F3 分叉发生的地方**（2^53+1 舍入）⇒ 契约必须把上界压到 2^53 − 1。

### 方案 C：定点整数 + 独立位宽（Percepta 路线）

- 电路的数值位是**定点整数**（N 位有符号），上界 2^(N-1)；
  选 N 使其覆盖业务值域；比较用抛物线/位序寻址（Percepta 的 2D attention）。
- 优点：整数语义天然精确，无舍入 ⇒ **可证明 exact**；
  与「Mora 的 Int 本应是精确整数」这一语言直觉一致；
  Percepta 复现证明最小配置仅需 964 个编译参数。
- 缺点：**不是神经网络**——没有矩阵乘结构，无法直接接 GPU/可微；
  本质是「把 VM 编成权重」，与 Handwired 是两件事。

### 三者关系（重要）

**A/B 与 C 不是同一条路。**

```
        编译目标是什么？
              │
    ┌─────────┴─────────┐
    │                   │
  数值电路            解释器/权重
（Handwired）        （Percepta）
    │                   │
  A: FP32            C: 定点整数
  B: FP64                  │
    │                      │
 可微 + 可 GPU         确定 + 可嵌入模型
 精确（需值域证明）      （但不可微、难并行）
```

**A/B 可微分、可 GPU 并行**；**C 精确、可嵌入 forward pass**。
Mora 当前的 IR 两条都能走，但**不能同时走**。

---

## 四、给作者裁决的三个问题

按 `docs/decisions/no-borrowed-constraints.md` 确立的元规则，
以下**只给信息与选项，不替作者拍板**。

### Q1：走 A/B（数值电路）还是 C（编译进权重）？

- **A/B** 的价值：可微分 + GPU 并行 + 与 Handwired 对标。
  代价：要**同时**解决「值域分析」和「控制流消解」，工程量大。
- **C** 的价值：把 Mora 变成可嵌入 forward pass 的确定性计算层，
  这是「AI-native」最硬的一种解释。代价：不可微、难并行、且
  Percepta 社区已质疑其工程价值（见 §5）。

**建议顺序**：先做 A/B 的**数值契约与值域分析**（Layer 0/1），
因为这部分**在 C 路线下同样需要**（定点整数的值域证明）；
若届时发现值域分析成本过高，可直接转向 C 而不浪费。

### Q2：Mora 的 `Int` 语义要不要先统一？

现状 F1/F2/F3 三条算术不一致，且 `a > b` 在 2^53 边界给出错误答案。
这**独立于**电路后端存在，是既有缺陷。三个选项：

| 选项 | 内容 | 影响面 |
|---|---|---|
| ① 统一为精确 i64 | `Sub`/`Mul` 去掉 f64 round-trip；比较走整数比较 | 改变现有程序的数值结果（2^53 外的行为），**breaking** |
| ② 统一为 f64 语义 | `Add` 也走 round-trip | 同上，**breaking**，且放弃 Int 的精确性 |
| ③ 保留现状，电路后端只支持 2^53 内的子集 | 承认分叉，值域分析把界压到 2^53−1 | 不改现有语义；**但分叉仍在** |

**关键**：无论选哪个，**电路后端的数值契约都必须显式写明它对齐哪一套语义**，
否则对拍测试无法给出可信判据。

### Q3：值域分析放在哪一层？

| 位置 | 形态 | 代价 |
|---|---|---|
| 类型系统 | `Type::Int` 携带 refinement（区间/位宽） | 侵入 HM 推断，row-polymorphic 联合要扩展 |
| MIR 分析 pass | 独立 pass，从常量传播推出区间 | 与 typeck 解耦，但类型信息用不上 |
| 显式标注 | 用户写 `int<24>` 之类 | 侵入语法，但最直接 |

**倾向**：独立 MIR pass（不动 typeck），因为
(a) 现有 `Type` 已有 20+ 变体，再加 refinement 会波及所有 match；
(b) 值域是**优化/准入条件**，不是类型正确性条件，放进 typeck 会污染错误信息。

---

## 五、必须一并记录的反面证据

不写下来会导致日后重复乐观：

### 5.1 Percepta 侧

- 博客声称的**可微性未被证明**（用 "average-hard attention"，对 key/query 不可微）。
- 33,000 tokens/sec 看着快，但 HN 有人估算**比原生 WASM 慢约 10,000 倍**，
  且**无任何 benchmark 对比**。
- 博客本身被多位 HN 评论者怀疑是 AI 生成（"repetitiveness"、"schmoozing salesman feel"）。
- 社区核心质疑：**"The model doesn't learn to compute — it has computation
  injected into its weights."** 编译进去 ≠ 学会了计算。
- 复现方自陈：花 5 个 phase 试图**训练** transformer 学会执行，撞死在算术上
  （"能学会何时加，但学不会怎么加"），改**编译**后立刻 token-for-bit 一致。
  ⇒ **精确性来自编译，不来自学习。**

### 5.2 Handwired 侧

- 官方明确：**没有工具能把已有 JS 源码自动转成网络**，需手写 builder。
- 循环必须改写成计数/树形合并结构；每次求值跑完所有操作，不能提前退出。
- 全部数是 FP32，每个值的范围要事先声明（越界即抛）。
- **训练解冻后的权重是否有用，尚未被证明。**
- 作者本人称之为 "toy project"。

### 5.3 共同教训

> **两个项目都把「控制流消解」的工作量转移给了人。**
> Handwired 转移给手写 builder；Percepta 转移给手写抛物线 key 寻址方案。
> 它们证明的是「这条路上存在精确解」，**不是**「这条路可以被自动化」。

⇒ Mora 若在这条路上有独特价值，**唯一可能的来源就是类型系统**：
用类型信息自动判定「哪段 MIR 可展平、值域多大、比较能否精确」，
从而**免掉那个人工转移**。这是 Handwired（JS，无类型）与 Percepta（C，无 effect 系统）
都拿不到的东西。

---

## 六、依赖顺序（后续层的地基）

```
Layer 0  数值契约            ← 本文档
   │   （精度表示 / 值域界 / 对齐哪套 Int 语义 / exactness 判据）
   ▼
Layer 1  值域分析 pass        ← 从常量传播推区间
   │   （依赖 Layer 0 的界；产出「此子图值域是否在界内」）
   ▼
Layer 2  可展平性判定        ← 纯度（effect row）+ 控制流形态
   │   （DAG 已有；缺 unroll/trip-count 分析——全仓 grep 为空）
   ▼
Layer 3  MIR → 电路 lowering
   │   （if → select 降级；纯循环 unroll；非线性算子如何表达）
   ▼
Layer 4  精确性对拍          ← 复用现有双管线差分机制
   │
   ▼
Layer 5  GPU / 权重后端 / 接进模型
```

**Layer 0 不定，Layer 1 无法定义值域界，Layer 2 的"可编译"无意义。**

---

## 附录 A：实测探针

```mora
-- tmp_probe/numprobe.mora
let a = 9007199254740993i
print(a)        -- 9007199254740993
print(a + 0i)   -- 9007199254740993
print(a - 0i)   -- 9007199254740992
print(a * 1i)   -- 9007199254740992
print(a + 1i)   -- 9007199254740994
print(a - 1i)   -- 9007199254740991
print(a * 2i)   -- 18014398509481984

-- tmp_probe/cmpprobe2.mora
let a = 9007199254740993i
let b = 9007199254740992i
let f = 9007199254740992.0 + 1.0
print(f)        -- 9007199254740992.0
print(a == f)   -- true
print(f == b)   -- true
print(f > b)    -- false
```

## 附录 B：与既有文档的交叉引用

| 文档 | 相关内容 |
|---|---|
| `tests/int_overflow_build_divergence.rs:44-45` | 独立佐证 Add 与 Sub/Mul 算术分叉 |
| `tests/int_overflow_build_divergence.rs:1-16` | `Int+Int` 溢出随构建配置漂移（debug panic / release 回绕） |
| `docs/audit/MIR_COMPILER_AUDIT_2026-07-25.md:1308` | 「i64 加法 debug panic」列为 P0，自 2026-07-25 未进入 D 序列 |
| `src/mir/jit.rs:16,571` | JIT Int×Int 按 f64 round-trip 发射（F2 的同源问题） |
| `src/flow.rs:682-702` | BigInt 比较不走 f64（D199）——已修的成例，可作修复范式 |
| `AGENTS.md` §0 | 架构优先 + 完整实现；禁止最小实现 |
| `docs/decisions/no-borrowed-constraints.md` | 借鉴项目不构成硬约束；给选项不替作者拍板 |
