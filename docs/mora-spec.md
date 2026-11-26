# Mora 语言规范 (v0.110.0)

> **Mora** — AI-native 静态类型语言，专为大模型底层与计算图及算子以及大模型推理与训练及资源动态调控而设计，同时具备 Go 的云原生属性，原生具备对 HTTP 与 MCP 服务的友好支持。
> 融合多范式语言设计：Prolog、StreamIt、APL、Clojure、Lisp、Smalltalk、Common Lisp、Ballerina、Logo。

---

## 目录

1. [哲学](#1-哲学)
2. [范式](#2-范式)
3. [类型](#3-类型)
4. [执行](#4-执行)
5. [内存](#5-内存)
6. [作用域](#6-作用域)
7. [控制流](#7-控制流)
8. [错误](#8-错误)
9. [并发](#9-并发)
10. [模块](#10-模块)
11. [元编程](#11-元编程)
12. [stdlib](#12-stdlib)
13. [形式化](#13-形式化)
14. [语法](#14-语法)
15. [相等性](#15-相等性)
16. [文本模型](#16-文本模型)
17. [安全](#17-安全)
18. [互操作](#18-互操作)
19. [工具链](#19-工具链)
20. [版本](#20-版本)

---

## 1. 哲学

### 1.1 设计目标

Mora 是一门 **AI-native 静态类型语言**，核心定位是服务于**大模型底层与计算图及算子**以及**大模型推理与训练及资源动态调控**，同时具备 **Go 的云原生属性**与对 **HTTP / MCP 服务编写的语言级友好支持**，核心哲学：

| 原则 | 含义 |
|------|------|
| **AI-native 底层友好** | 精确数值塔（`Int`/`Float` 与低位宽后缀 `f16`/`bf16`/`fp8`/`i4`..`i64`）、矩阵乘 `@`、幂运算 `**`、代数效果（`perform`/`handle`/多发 `resume`）与持久化块列表 |
| **静态类型** | 编译期单一 Span 追踪 Hindley-Milner 约束求解 + 刚性泛型 + 值限制 + 效果行包含检查，注解可选（局部类型推断） |
| **渐进复杂度** | 简单脚本直接运行，复杂系统可扩展到闭包、泛型、trait/`dyn`、Control SSA IR 优化与堆帧状态机 VM |
| **云原生并发与事务** | 语言级 Go 式并发（`spawn` / 不变 `channel<T>` / 公平 `select` / 分层取消 `context` / `sleep`）、`parallel` / `worker` / `transaction` / `atom` 与**自研**多线程调度器 / Reactor |
| **可组合与同像性** | `eval`/`quote`/`read`/`quasiquote` + `macro` 支持编译期与运行期元编程 |

### 1.2 设计哲学

```
简单的事简单做，复杂的事可以做。
```

- **单遍编译 + Control SSA IR + 堆帧 VM**：最小核心语义，通过类型系统、代数效果与 `FsPolicy` / `CheckedPath` 强制安全边界
- **核心与库解耦**（v0.110.0 Wave 1）：编译器核心只保留语言原语与 7 个基础内建模块（`json`/`file`/`exec`/`math`/`stats`/`linalg`/`random`），扩展能力（如 `std/math/bigint`）下沉为独立标准库包
- 借鉴：Rust 的类型安全、Go 的并发工程感、Lisp 的同像性、APL/StreamIt 的向量表达

### 1.3 命名

**Mora** = 拉丁语 "一小段时间"，暗示：
- 轻量（单二进制）
- 精准时机（推理/训练调度节奏感）
- 组合性（pipeline/flow）

---

## 2. 范式

### 2.1 多范式融合

| 范式 | 体现 |
|------|------|
| **命令式** | `let`/`assign`/`for`/`if` |
| **函数式** | 闭包、管道 `\|>`、`map`/`filter`/`reduce`、代数效果 `perform`/`handle` |
| **名义与特征抽象** | 名义泛型 `struct`（含默认值与 `@tag` 元数据）/ `enum`、源码级 `trait` / `impl` / `dyn Trait` |
| **计算图与自动微分** | 一等 `Tensor<DType, Shape, Device>`、`grad` / `value_and_grad` / `jvp` / `no_grad`、`trace_dag` 4 遍算子融合、`kv_allocator` / `memory_pool` |
| **云原生并发与事务** | Go 式 `spawn` / `channel<T>` / `select` / `context` / `sleep`、非阻塞异步 TCP、`parallel` / `worker` / `transaction` / `atom` |
| **元编程与同像性** | `macro`、`quote` / `eval` / `read` / `macroexpand` / `quasiquote` |

### 2.2 一切皆表达式

大多数构造是表达式（有返回值）：

```mora
let x = if cond then "a" else "b" end
let y = match val with
  1 -> "one"
  _ -> "other"
end
let z = [1, 2, 3] |> map(fn(x) x * 2 end)
```

### 2.3 不可变优先

`let` 默认不可变。没有 `var` 关键字。要修改状态，用 `assign`（显式）：

```mora
let x = 10
assign x = 20    -- 显式修改
```

---

## 3. 类型

### 3.1 类型种类

| 类别 | 类型 | 语法 |
|------|------|------|
| **原语** | `string` | `"hello"` |
| | `char` | `'a'` |
| | `number` | `42`, `3.14`（数值联合类型缩写） |
| | `int` | `42`, `42i`, `1_000_000`（精确 64 位整数，支持 `i`/`i4`..`i64`/`u`/`u8`..`u64` 位宽后缀） |
| | `float` | `3.14`, `1e-4`, `1.0f32`（双精度浮点，支持 `f`/`f16`/`f32`/`f64`/`bf16`/`fp8` 位宽后缀） |
| | `bool` | `true`, `false` |
| | `nil` | `nil` |
| **容器** | `list<T>` | `[1, 2, 3]` |
| | `dict<K, V>` | `{a: 1, b: 2}` |
| **可调用** | `task` | `task name() ... end` |
| | `closure` | `fn(x) x + 1 end` |
| **错误** | `result<T, E>` | `Ok(v)` / `Err(e)` |
| **张量与计算图** | `tensor<DType, Shape, Device>` | `Tensor::zeros([m, n])`, `Tensor::from_list(...)`（64B 对齐紧凑存储，支持 `cpu`/`cuda:N`/`vulkan:N`/`metal:N`） |
| | `compute_dag` | `trace_dag(fn, args)`（4 遍算子融合 + 静态 Buffer 复用 + 硬件 GPU/SIMD 后端） |
| | `kv_allocator` | `kv_allocator(...)`（PagedAttention 页表、CoW 前缀共享、硬配额、抢占与水位线回调） |
| | `memory_pool` | `memory_pool(max_bytes, device?)`（64B 对齐 2 次幂 Slab 张量复用池） |
| **名义与特征系统** | `struct` | `struct Name<T> ... end`（支持字段默认值、`@tag(...)` 编译期元数据与 `Struct::schema()`） |
| | `enum` | `enum Name<T> ... end`（带载荷代数数据类型与 `Enum::variants()`） |
| | `trait` / `concrete` | `trait Name<T> ... end` / `impl Trait for Type ... end` |
| | `union` | `string \| number` |
| | `dyn Trait` | 动态分派 `expr as dyn Trait` |
| | `arrow` | `fn(x) x + 1 end`（函数类型） |
| **并发与网络** | `channel<T>` | `channel(cap)`（不变通道） |
| | `task_handle<T>` | `spawn expr`（协变任务句柄） |
| | `context` | `context()`（分层协作取消上下文） |
| | `tcp_listener` / `tcp_stream` | `tcp_listen(addr)` / `tcp_connect(addr)`（非阻塞异步 TCP） |
| | `atom` | `atom(0)` |
| | `compose` | `compose(f, g)` |
| | `partial` | `partial(fn, args)` |
| | `macro` | `macro name() ... end` |
| **代数效果与宿主** (v0.80+) | `effect` | `perform E(arg)` |
| | `handler` | `handle E { body } { handler }` |
| | `continuation` | 多发定界续延 `resume(v)` (`Value::Continuation`) |
| | `external` | 宿主扩展对象 (`Value::External(Arc<dyn HostObject>)`) |
| **同像性** (v0.86) | `code` | `quote(expr)` / `read("...")` |

> `BigInt` 任意精度整数已于 v0.110.0 从语言核心迁至 `std/math/bigint`（**纯 Mora** 标准库模块 `std/math/bigint.mora`，非 Rust crate）。

### 3.2 类型推断

Mora 支持局部类型推断（v0.110.0 Stage 3A 起无后缀整数字面量推断为 `int`，浮点字面量推断为 `float`）：

```mora
let x = 42          -- 推断为 int
let s = "hello"     -- 推断为 string
let l = [1, 2, 3]   -- 推断为 list<int>
```

函数参数和返回类型可选：

```mora
task add(a, b)      -- 参数类型推断
  return a + b
end

task greet(name: string): string   -- 显式注解（注意用 : 不是 ->）
  return "Hello, " + name
end
```

### 3.3 泛型

```mora
trait Container<T>
  fn push(self, item: T) -> Self
  fn len(self) -> number
end

impl Container<T> for list<T>
  fn push(self, item: T) -> Self
    return self |> push(item)
  end

  fn len(self) -> number
    return len(self)
  end
end
```

### 3.4 Union 类型

```mora
task process(input: string | number): string
  return match input with
    s: string -> s.upper()
    n: number -> json.stringify(n)
  end
end
```

### 3.5 Trait 系统

```mora
trait Named
  fn name(self) -> string
end

trait Aged
  fn age(self) -> number
end

trait Person: Named, Aged    -- 继承
  fn greet(self) -> string
    return self.name()
  end
end

impl Person for dict
  fn name(self) -> string
    return self.get("name")
  end

  fn age(self) -> number
    return self.get("age")
  end
end

let p: dyn Person = {"name": "Alice", "age": 30}
print(p.greet())
```

### 3.6 数值塔（v0.110.0 核心收敛）

Mora 核心运行时内置两个数值类型，构成一个**带可表示性检查**的数值塔（原任意精度 `BigInt` 已于 v0.110.0 迁出核心运行时至独立纯 Mora 标准库模块 `std/math/bigint.mora`）：

```text
       Int              i64（`42i`）
         │  提升（有损 ⇒ 需可表示性检查）
       Float            f64（`3.14`）
```

#### 每个域拥有自己的算术

**同一对操作数，不同运算，结果必须一致。** 这是 D468 确立的核心不变式。

`Int` 的算术与比较是**精确 i64**：

```mora
let a = 9007199254740993i      -- 2^53 + 1
print(a + 0i)                  -- 9007199254740993
print(a - 0i)                  -- 9007199254740993   ← 与上一行相同
print(a > 9007199254740992i)   -- true
```

`Int` 的溢出**报运行时错误**（不 panic、不回绕、不饱和）：

```mora
let a = 9223372036854775807i   -- i64::MAX
print(a + 1i)                  -- Runtime error: integer overflow in Add
```

`Float` 是 IEEE 754 双精度（除零给 `inf`/`NaN`，`NaN` 参与比较一律 false）。

#### 跨塔提升

| 左 | 右 | 结果 | 精确性 |
|---|---|---|---|
| `Int` | `Int` | `Int` | 精确；溢出报错 |
| `Int` | `Float` | `Float` | `Int` 须装得下 f64（见下） |
| `Float` | `Float` | `Float` | IEEE 754 双精度 |

#### 装不下就报错，不静默降级

`Float` 只有 53 位尾数 ⇒ `|i| > 2^53` 的 `Int` 转成 f64 会**静默丢位**。
Mora 在跨塔运算前做**可表示性检查**，装不下就报错：

```mora
let a = 9007199254740993i      -- 2^53 + 1，装不下 f64
print(a + 1.0)                 -- Runtime error（不静默给错值）
print(a == 9007199254740992.0)  -- false（相等是谓词，判「不相等」）
print(a > 9007199254740992.0)  -- Runtime error（顺序比较需要正确的序）
```

**为什么相等判 false、比较却报错**：相等是**布尔谓词**，契约是「给一个答案」；
而顺序比较是**关系**，契约是「给出正确的序」。装不下的两个数**必然不相等**
—— 那是正确答案，不是错误；而给出可能自相矛盾的序才是错误。

#### 比较运算符自洽

`>` / `>=` / `<` / `<=` 统一由「先求序，再由序推导关系」实现，
因此对同一组值**必然自洽**：`a > b` 为真 ⇒ `a >= b` 必为真。

#### 形式化：值语义不变式

以 `⊑` 表示「数值相等」（跨塔精确），`≺` 表示「数值序」。

```
Int(i) ⊕ Int(j) = Int(i + j)          当 i + j 不溢出
Int(i) ⊕ Int(j) = ⊥ (overflow)          否则

Int(i) ≺ Int(j)  ⟺  i < j              （有符号整数序，非 f64 序）
Float(f) ≺ Float(g) ⟺ f <_ieee g
Int(i) ≺ Float(f) ⟺ i <_exact f        （需 i 可被 f64 精确表示）

Int(i) ⊑ Float(f) ⟺ i =_exact f        （i 装不下 f64 时恒为 false）
```

**关键不变式**（D468 确立）：

```
(i)  同类型运算与比较共用同一套值语义
       Int(i) ⊕ Int(j) 与 Int(i) ≺ Int(j) 的序由**同一个** i64 序决定

(ii) 顺序关系自洽
       a ≻ b  ⟹  a ≽ b，且 ¬(a = b)
       a ≺ b  ⟹  a ≼ b，且 ¬(a = b)

(iii) 跨塔：装不下 ⇒ 不等（谓词）/ 报错（关系）
       |i| > 2^53 ⇒ Int(i) ⊑ Float(f) 为 false
       |i| > 2^53 ⇒ Int(i) ≺ Float(f) 无定义（报运行时错误）
```

不变式 (ii) 是旧实现被破坏的那一条：旧路径下 `a > b` 为 false
而 `a >= b` 为 true（`>=` 的判据是 `≠ Less`，而 f64 舍入后两者相等）。

### 3.7 一等 Tensor、5 项 AI-Native 静态检查、自动微分与计算图 (v0.110.0 Wave 3B/3C & Wave 4)

1. **硬件位宽不擦除与 64 字节对齐存储**（`src/common/dtype.rs`、`src/value/tensor.rs`）：
   - 原生支持 `f64`、`f32`、`f16`（IEEE 754 binary16）、`bf16`、`fp8` / `fp8_e4m3`、`fp8_e5m2`、`i64`、`i32`、`i16`、`i8`、`i4`（半字节 2 元素/字节紧凑打包）、`u64`、`u32`、`u16`、`u8`、`bool`。
   - 底层存储 `AlignedByteBuffer` 采用 64 字节缓存行对齐分配；支持零拷贝步幅视图（`transpose`、`reshape`、多维切片 `t[0..4, :, None, ::-1]`）。
2. **编译期 5 项 AI-Native 静态检查**（`src/typeck/hm/`）：
   - **DType 检查**：禁止不同精度张量隐式混算，算子输入满足硬件位宽契约。
   - **Shape 检查**：符号维度合一（`[M, K] @ [K, N] -> [M, N]`）、广播兼容性与静态切片越界拦截。
   - **Device 检查**：静态追踪设备位置（`cpu`、`cuda:N`、`vulkan:N`、`metal:N`），禁止未显式 `.to_device(...)` 的跨设备混算。
   - **Resource 检查**：作用域资源配额与自动微分磁带期间的原位修改（`tensor_mutate`）/ 别名安全校验。
   - **Effect 检查**：封闭效果行包含校验，静态拦截对含原位变异或非纯副作用函数求导。
3. **反向、前向与高阶自动微分**（`src/runtime/autograd.rs`、`src/runtime/autograd_higher_order.rs`）：
   - 反向模式：`grad(f, argnums?)`、`value_and_grad(f, argnums?)`、`no_grad ... end`。
   - 前向模式与高阶微分：`jvp(f, primals, tangents)`、多磁带任意阶微分 `grad(grad(f))` 及精确 Hessian-Vector Product（`jvp(grad(f), x, v)`）。
4. **计算图算子融合与零依赖硬件 GPU 后端**（`src/runtime/compute_dag.rs`、`src/runtime/gpu/`）：
   - `trace_dag(fn, example_inputs)` 追踪构建强类型 `TensorComputeDag`，经 `.compile()` 执行 4 遍算子融合（`FusedFlashAttention` 分块在线 Softmax、`FusedRmsNormLinear`、`FusedLinearAct`、`FusedElementwise`）、`DagMemoryPlan` 静态草稿区复用与 `DagPlanCache` 编译缓存。
   - 零外部编译依赖动态加载硬件驱动：**NVIDIA CUDA Driver API**（PTX ISA 7.0 5 大 `f64` 融合核 + `CudaVramArena`）、**Vulkan 1.0**（纯 Rust SPIR-V 1.0 计算着色器）、**macOS Metal**（运行期 MSL 编译）及 **CPU AVX2+FMA / NEON 多线程分块核**；可通过 `Tensor::devices()` 与 `Tensor::device_info(dev?)` 查询硬件遥测。
5. **动态显存调控**（`src/runtime/kv_cache.rs`、`src/runtime/memory_pool.rs`）：
   - `kv_allocator(total_pages, page_tokens, num_heads, head_dim, dtype?, device?, max_bytes?)`：PagedAttention 页表、CoW `fork_seq`、硬字节配额、序列优先级抢占驱逐与无死锁水位线回调。
   - `memory_pool(max_bytes, device?)`：64 字节对齐 2 次幂分桶 Slab 张量缓存池。

---

## 4. 执行

### 4.1 执行模型（v0.110.0: 单遍 Control SSA IR + 堆帧状态机 VM）

Mora 采用 **单遍编译 + Control SSA IR + 堆帧状态机 VM** 架构（v0.110.0 Wave 2 & Stage 3A）：

**唯一生产管线**：
```
source → Lexer::scan_tokens → ParserV3::compile (parse_witnesses)
  → MirWitness[] + mir::control::lower_program (ControlProgram SSA IR)
  → mir::control::verify_program (SSA 支配关系与槽位校验)
  → typeck::check_mir::check_program_witnesses (单一 Span 追踪 HM 约束求解器 + import 模块检查)
  → mir::control::optimize_program (OptLevel 0/1/2: CopyProp / GVN-CSE / LICM / DCE)
  → runtime::vm::execute_program + run_main_task
```

**执行与调度内核**：
- **堆帧状态机 VM**（`src/runtime/vm/`）：基于显式 `Vec<CallFrame>` 的单一执行引擎，支持 `O(1)` 尾调用帧复用与一等定界续延多发重放（`Value::Continuation`）
- **自研多线程调度器与 Reactor**（`src/runtime/{core,scheduler,reactor,fs}.rs`，**历史**：原为 Tokio 实现）：`CoreRuntime` 持有进程级共享 `EngineShared { scheduler: TaskScheduler, reactor: AsyncIoReactor }`，驱动 Go 式 `spawn`、`MoraChannel`、`select`、`TaskContext` 协作式取消、`AsyncTcpListener` 以及 `FsPolicy` / `CheckedPath` 文件系统沙箱隔离

**编译优化控制**：
- CLI `--opt=0|1|2` 或环境变量 `MORA_OPT=0|1|2`（`0` = 关闭、`1` = Basic、`>=2` = Aggressive）
- **零源码 `MORABC02` 预编译字节码与单二进制交付**（`src/mir/control/codec.rs`、`src/cli/bundle.rs`）：`mora build <file.mora> [-o <app>]` 与 `mora build <file.mora> --emit-bc [-o <out.morac>]` 在编译期完成主模块与传递 `import` 闭包的解析、HM 静态检查与 SSA 优化，序列化为带 64 位 FNV-1a 校验和的 LEB128 二进制 `ControlProgram` 字节码，运行期零源码、零词法/语法/类型检查开销直接执行。

### 4.2 入口点

```mora
task main()
  print("Hello, Mora!")
end
```

`main` 是可选入口点。如果不存在，脚本从第一条语句开始执行。

### 4.3 求值顺序

- **从左到右**：函数参数、表达式操作数
- **严格求值**：所有参数在调用前求值

#### 4.3.1 逻辑运算符的短路求值 (v0.104.6 D477)

`or` / `and` 是**短路**（short-circuit）运算符。这是语言的目标语义，
不是实现细节：

| 表达式 | lhs | rhs | 结果 |
|--------|-----|-----|------|
| `a or b` | 总是求值 | **仅当 a 为假**才求值 | a 为真 ⇒ `true`；否则 ⇒ b |
| `a and b` | 总是求值 | **仅当 a 为真**才求值 | a 为假 ⇒ `false`；否则 ⇒ b |

两侧都必须是 `bool`（类型检查强制，见 §3）。

⚠ 短路是**求值级**的，不只是「取值级」：短路成立时 rhs **根本不会被求值**，
因而它的**副作用也不发生**：

```mora
task bump() : bool
  print("BUMP")
  return true
end

let x = true or bump()    -- bump() 不执行（输出里没有 BUMP）
let y = false and bump()  -- bump() 不执行
```

**实现约束**：任何编译路径（9 层管线 / 直连 lowering）都必须遵守本节 ——
同一个程序在两条路径下的**副作用**必须一致。此前生产路径只短路取值、
不短路求值，而直连路径两者都短路 ⇒ 两条路径分叉。
回归护栏：`tests/short_circuit_evaluation.rs`。

### 4.4 尾调用（v0.110.0 W2.3: `O(1)` 尾调用帧复用）

堆帧 VM（`src/runtime/vm/engine.rs`）对尾位置调用实行 **`O(1)` 帧就地复用**（Tail-Call Optimization），100,000+ 次深度尾递归调用在恒定栈帧空间内完成，不会触发 OS 线程栈或 VM 调用帧溢出。

---

## 5. 内存

### 5.1 内存模型

Mora 使用 **引用计数 + 持久化结构 / 词法父链共享**：

- `Value::List(List)` — 分块持久化向量（`src/value/list.rs`），值语义与结构共享
- `Value::Dict(HashMap<String, Value>)` — 值类型，深拷贝
- `Value::Closure { env: Arc<Environment>, .. }` — 词法环境由扁平 `HashMap<String, Value>` + `Arc<Environment>` 父链构成（v0.110.0 Wave 2 移除了早期 HAMT `value/persistent.rs` 与 `VectorClock` 耦合）

### 5.2 所有权语义

| 类型 | 赋值语义 |
|------|----------|
| `number`, `bool`, `char`, `nil` | 值拷贝 |
| `string` | 引用（不可变） |
| `list`, `dict` | **深拷贝**（值语义） |
| `closure`, `task` | 引用（closure 捕获冻结的环境快照） |
| `atom` | 引用（`Arc<Mutex<Value>>`） |

```mora
let a = [1, 2, 3]
let b = a           -- 深拷贝
assign b = [4, 5]   -- 不影响 a
```

**注意**: `list`/`dict` 的深拷贝为递归深拷贝；循环引用当前未检测，可能导致栈溢出。

### 5.3 垃圾回收

依赖 Rust 的 `Arc` 引用计数。无显式 GC。

---

## 6. 作用域

### 6.1 词法作用域

Mora 使用 **词法作用域**（静态作用域）：

```mora
let x = 10

let make = fn(y)
  return fn()
    print(x + y)   -- 可以访问外层 x 和 y
  end
end

let inner = make(20)
inner()              -- 30.0
```

> 具名定义（`task name() ... end`）只能在顶层出现，不能嵌套在另一个定义体内；
> 内层作用域请用闭包 `fn(x) ... end` 表达（见 6.2 表）。

### 6.2 作用域规则

| 构造 | 作用域 |
|------|--------|
| `let x = ...` | 当前块 |
| `task name() ... end` | 新作用域 |
| `fn(x) ... end` | 新作用域（捕获外层变量） |
| `for x in ...` | 新作用域 |
| `if ... then ... end` | 不创建新作用域（与外层共享） |

### 6.3 闭包捕获

闭包捕获 **引用**（通过 `Arc<Mutex<Environment>>`）：

```mora
let counter = 0

let inc = fn()
  assign counter = counter + 1
  return counter
end

print(inc())  -- 1
print(inc())  -- 2
```

### 6.4 变量遮蔽

`let` 可以遮蔽外层变量（创建新绑定）：

```mora
let x = 10
task test()
  let x = 20     -- 遮蔽外层 x
  print(x)       -- 20
end
test()
print(x)         -- 10（外层不受影响）
```

---

## 7. 控制流

### 7.1 条件

```mora
if condition then
  -- ...
else if other then
  -- ...
else
  -- ...
end
```

### 7.2 循环

```mora
-- for-in 循环
for item in list
  print(item)
end

-- range 循环
for i in range(0, 10, 1)
  print(i)
end

-- break / continue
for item in list
  if item == 3 then continue end
  if item == 7 then break end
  print(item)
end
```

### 7.3 模式匹配

```mora
let result = match value with
  0 -> "zero"
  1 -> "one"
  n: number -> "number"
  s: string -> s
  _ -> "unknown"
end
```

模式支持：
- 字面量：`0`, `"hello"`, `true`
- 变量绑定：`n: number`, `s: string`
- 通配符：`_`
- 列表解构：`[a, b, c]`
- 字典解构：`{name: n, age: a}`

### 7.4 守卫条件 (v0.16, Prolog 启发)

```mora
let result = match n with
  x when x > 0 -> "positive"
  x when x < 0 -> "negative"
  _ -> "zero"
end

-- 嵌套守卫
let result = match data with
  {age: age, name: name} when age >= 18 -> "adult"
  {age: age, name: name} when age < 18 -> "minor"
  _ -> "unknown"
end
```

### 7.5 列表 rest 模式 (v0.16, Prolog 启发)

```mora
-- match 中使用 Rest 模式解构首元素与剩余列表
let data = [1, 2, 3, 4]
let summary = match data with
  [head, ...tail] -> "non-empty"
  _ -> "empty"
end
```

### 7.6 管道

```mora
let result = "hello world"
  |> upper()
  |> split(" ")
  |> map(fn(w) w.trim() end)
  |> filter(fn(w) len(w) > 3 end)

-- v0.17: 管道支持闭包 (StreamIt 启发)
let double = fn(x) return x * 2 end
let result = 5 |> double    -- 10
```

### 7.7 代数效果 (v0.80, Algebraic Effects)

Mora 支持代数效果（Algebraic Effects），通过 `perform` 产生效果、`handle` 捕获并处理效果：

```mora
effect Ask(string): string

let result = handle Ask {
  perform Ask("What is your name?")
} {
  resume("Alice")
}
```

**类型基础**：EffectRow（`EffectRow::Empty` / `EffectRow::Var` / `EffectRow::Cons`），支持行多态合一（row-polymorphic HM unification）。

**语义**：
- `perform EffectName(arg)`：产生一个效果，捕获当前定界续延并跳转至最近匹配的 `handle`
- `handle EffectName { body } { handler }`：注册效果处理器，捕获 `perform` 并执行 `handler`
- 多发定界续延（multi-shot delimited continuations，v0.110.0 Wave 2）：`handler` 内可通过 `resume(v)` 恢复一次或多次续延（`Value::Continuation`）
- 嵌套 handler：内层 handler 优先匹配

**编译期强制（v0.96）**：效果行在类型检查的**程序边界**断言 —— 顶层表达式的
残差行不允许含具名效果标签，否则报 `EffectRowMismatch`（定位到发起
`perform` 的行，或跨函数时定位到调用点并注明来源函数）：

- `perform X` 必须处于匹配的 `handle X` 作用域内（直接或经调用链）；
- `handle X` 的 body 残差行去掉 X 后继续向上流动 —— 定义了未触发的
  handler（纯 body）是合法程序；
- fn/task **定义**是纯的（效果行捕获进函数类型）；效果在**调用时**发生，
  由调用点的 handle 上下文负责 —— 因此「定义时 perform、调用处 handle」
  的合法程序不会被拒绝；
- 跨函数传播：调用已登记效果行的函数，其行并入调用方残差行；
- **mutual recursion / 前向引用（v0.97）**：定义行经不动点预计算（行单调
  增长至稳定），先定义的函数调用后定义的函数、相互递归的定义组，效果
  均可静态传播；
- 残差行仅含行变量（多态未知行）时宽松放行，不做错误拒绝。
- 运行时的 "unhandled effect" 兜底自此只防御动态生成代码（如 `eval`）。

**perform ↔ handler 静态连接（v0.97）**：

- handler 的 `__arg0..__argN`（N = 该 handle 作用域内 perform 位点的最大
  实参数）按位点实参的推断类型静态注册；位点异质时退化为 Any（不误拒）；
- `perform` 的结果类型与 handler 返回类型统一（runtime 契约「handler
  返回值 = resume 值」的静态化）；
- 连接只在 handle body 内**直接**出现的位点生效；fn/task 体内的 perform
  （经调用触发的位点）结果类型仍是多态的 —— 精确连接需要跨调用点的
  effect signature 声明，是后续扩展点。

**显式 effect 签名（v0.98）**：

```mora
effect Ask(string): string   -- 载荷契约 + 结果契约
effect Log(string)           -- 仅载荷契约（结果 = any）
```

- **perform 位点校验**：实参数与类型必须符合签名，违反在位点报错；
- **结果静态化**：perform 的值取签名结果类型 —— fn/task 体内的高阶
  位点由此精确（不再依赖调用点的 handle 在场）；
- **handler 契约化**：`__arg0..N` 按签名参数类型注册（优先于位点推导）；
  handler 返回值必须兼容签名结果，违反在 handle 处报错；
- **导入传播**：签名随 `import` 跨文件生效（模块声明的效果契约对其
  消费方可见）；
- **重复声明**：签名一致幂等放行；不一致报错指向第二处声明；
- 签名不改变效果行（handled-ness 仍由 `handle` 的动态作用域决定），
  也不产生运行时指令 —— 它是纯类型层契约。

**Ambient effect（v0.99）**：

隐式全局副作用（当前集：`random_*` 六个标签，标签 = `random_` + 方法名）
不是用户声明的效果，而是**运行时内置的 ambient 契约**：

- **签名预置**：ambient 标签的签名在推断入口预置进 effect 签名表，
  先于文件级 `effect` 声明 —— 用户重声明同标签走重复声明规则；
- **行传播**：`random.<method>(...)` 调用把对应 ambient 标签并入
  效果行（v0.96/0.97 机制接手：跨函数传播、mutual recursion、
  handle 吸收一视同仁）—— 副作用自此对类型系统可见；
- **根边界豁免**：程序根的残差行允许含 ambient 标签（运行时根状态
  即其 handler）；**非** ambient 的未处理效果照常拒绝；
- **运行时查找顺序**：用户 `handle` 注册表 → ambient 根状态兜底；
  用户 handler 按签名获得 `__arg0..N` 契约与结果校验；
- **状态模型**：ambient 状态是运行时单属主纯值（无锁），克隆按值
  复制 —— 并行 worker 各自独立推进，并发不需要防御性同步。

-- 管道链
let result = 5 |> double |> add_one
```

### 7.7 Return

```mora
task early_return(x: number): string
  if x < 0 then
    return "negative"
  end
  return "positive"
end
```

---

## 8. 错误

### 8.1 Result 类型

Mora 使用 `Ok(v)` / `Err(e)` 构造协变 `result<T, E>` 类型进行显式错误处理（由 HM 类型推断器自动推导 `result<T, E>`）：

```mora
task divide(a: number, b: number)
  if b == 0 then
    return Err("division by zero")
  end
  return Ok(a / b)
end
```

### 8.2 错误传播与解构

通过 `match ... with` 对 `Ok(v)` 与 `Err(e)` 分支进行显式解构与传播；后缀 `?` 产生式自 v0.112.0 G2 起**已启用**，用于 `result<T, E>` 的错误传播 —— `Ok(v)?` 取 `v` 继续执行，`Err(e)?` 把 `Err` 原样向上传播（详见 `tests/spec_ebnf_surface.rs`）：

```mora
task safe_divide(a: number, b: number)
  let res = divide(a, b)
  return match res with
    Ok(v) -> Ok(v)
    Err(e) -> Err(e)
  end
end
```

### 8.3 运行时错误

以下情况会触发结构化 `MoraError` 诊断（非静默）：
- 静态或运行期类型/维度/设备不匹配
- 整数精确 `i64` 或低位宽标量算术溢出
- 索引或切片越界
- 未定义变量或跨设备未显式搬运
- I/O、文件系统沙箱越界（`FsPolicy`）或外部命令执行失败（`file.*` / `exec.*`）

### 8.4 错误处理最佳实践

```mora
task parse_port(s: string)
  if s == "" then
    return Err("empty")
  end
  return Ok(8080)
end

-- 使用 match 处理 Result
let res = parse_port("8080")
match res with
  Ok(port) -> print(port)
  Err(e) -> print(e)
end
```

---

## 9. 并发

### 9.1 Parallel 块

```mora
parallel
  let a = math.sqrt(16.0)
  let b = math.sqrt(25.0)
  let c = math.sqrt(36.0)
end
```

### 9.2 Worker 并发 (v0.19, Ballerina 启发)

```mora
parallel
  worker w1
    print("worker 1 done")
  end

  worker w2
    print("worker 2 done")
  end
end
```

### 9.3 事务支持 (v0.19, Ballerina 启发)

```mora
-- 事务块
transaction
  print("in transaction")
  commit
end

-- 带补偿的事务
transaction
  print("in transaction")
  rollback
compensation
  print("compensating")
end
```

### 9.4 Atom 可变引用 (v0.19, Clojure 启发)

```mora
-- 原子引用（线程安全的可变状态）
let counter = atom(0)
swap(counter, fn(n) return n + 1 end)
swap(counter, fn(n) return n + 1 end)
let val = deref(counter)    -- 2
```

### 9.5 Go 式云原生并发原语 (v0.110.0 Wave 2)

Mora 提供一等 Go 式并发原语，由进程级共享 `EngineShared { scheduler: TaskScheduler, reactor: AsyncIoReactor }` 驱动：

- `channel(cap)`：创建有界 FIFO 通道（`cap > 0`）或无缓冲同步会合通道（`cap == 0`），静态类型 `channel<T>` 在 HM 合一中**严格不变**；支持 `.send(v)`、`.recv()`、`.try_send(v)`、`.try_recv()`、`.close()`、`.len()`、`.is_closed()`。
- `spawn expr`：在并发任务中派生闭包或调用，返回协变 `task_handle<T>`（支持 `.join()`、`.cancel()`、`.is_finished()`）。
- `select ... end`：支持 `case v = ch.recv() => ...`、`case ch.send(v) => ...`、`timeout(ms) => ...` 与 `default => ...`，多路就绪分支按全局通道 ID 保序加锁并伪随机公平选择。
- `context()` 与 `sleep(ms)`：分层协作取消上下文（`.child()`、`.with_timeout(ms)`、`.cancel()`、`.is_cancelled()`）与可取消休眠。
- `tcp_listen(addr)` 与 `tcp_connect(addr, timeout_ms?, ctx?)`：基于 `AsyncIoReactor` 的非阻塞异步 TCP 监听器（`tcp_listener`：`.accept(ctx?)` / `.local_addr()` / `.close()` / `.is_closed()`）与流（`tcp_stream`：`.read()` / `.read_bytes()` / `.read_line()` / `.write()` / `.write_bytes()` / `.shutdown()` / `.close()` / `.is_closed()` / `.local_addr()` / `.peer_addr()`），原生响应 `TaskContext` 取消。

```mora
let ch = channel(1)
let ctx = context()

let h = spawn fn()
  ch.send(42)
  return 100
end

let got = select
  case v = ch.recv() => v + 1
  timeout(500) => -1
end

print(got, h.join(), ctx.is_cancelled())
```

---

## 10. 模块

### 10.1 Import

```mora
import "path/to/module.mora"
```

导入的模块在当前作用域中可用。

### 10.2 Export

支持导出 `let`、`task`、`struct`、`enum`、`type`、`trait`、`impl` 与 `macro` 声明，跨模块 `import` 支持菱形依赖缓存与编译期名义类型/标签元数据传播：

```mora
export struct Endpoint
  host: string = "127.0.0.1"
  port: int = 8080
end

export task helper(): int
  return 42
end

export let CONFIG = {debug: true}
```

### 10.3 模块路径与内嵌标准库

- 相对路径：`import "./utils.mora"`
- 绝对路径：`import "/home/user/libs/utils.mora"`
- 内嵌纯 Mora 标准库：`import "std/net/http"`（HTTP/1.1 Router / Chunked / Keep-Alive / 413 / SSE）与 `import "std/mcp"`（MCP JSON-RPC 2.0 / Tools / Resources / Prompts / `Struct::schema()` / Stdio NDJSON）
- 独立纯 Mora 标准库模块：`std/math/bigint`（`std/math/bigint.mora`）

### 10.4 内置模块

核心运行时（`value::MODULE_OBJECTS`）内置以下 7 个基础模块：

| 模块 | 说明 |
|------|------|
| `json` | JSON 编解码 (`parse` / `stringify`) |
| `file` | 文件读写与目录/路径检查 |
| `exec` | 外部命令与并行进程执行 (`exec.parallel`) |
| `math` | 基础数学与超越函数 |
| `stats` | 描述统计、直方图与分位数计算 |
| `linalg` | 向量与矩阵线性代数运算 |
| `random` | 伪随机数生成与采样 |

---

## 11. 元编程

### 11.1 编译期结构体标签与 Schema 反射 (v0.110.0 Stage 3C)

```mora
struct ToolInput
  query: string @tag(json="query", desc="Search keyword")
  limit: int = 10 @tag(json="limit", optional="true")
end

let schema = ToolInput::schema()
let tags = ToolInput::tags()
```

### 11.2 用户自定义宏 (v0.20, Common Lisp 启发)

```mora
macro when(condition, body)
  if condition then
    body
  end
end

-- 使用
let x = 10
when(x > 5, print("big"))
```

**注意**: 当前宏为简单模板替换，无 hygiene；宏参数在调用者作用域中展开。

### 11.6 同像性 — Homoiconicity (v0.86, Lisp 启发)

Mora 实现了完整的 Lisp 同像性（代码即数据），通过 `Value::Code` 类型表示代码为数据：

```mora
-- eval: 运行时编译并执行 Mora 代码
let code = "let x = 10 + 20"
eval(code)           -- x = 30

-- quote: 表达式 → Code 数据（不求值）
let expr = quote(1 + 2)    -- Value::Code

-- read: 字符串 → Code
let ast = read("let x = 42")

-- gensym: 生成唯一符号
let sym = gensym()          -- "g0", "g1", ...

-- macroexpand: 展开宏调用
macroexpand("my_macro", [arg1, arg2])

-- apply: 将函数应用到参数列表
apply(fn(x, y) x + y end, [1, 2])    -- 3

-- car/cdr/cons: 列表操作
let list = cons(1, cons(2, nil))
let head = car(list)           -- 1
let tail = cdr(list)           -- (2 . nil)
```

### 11.7 Quasiquote (v0.88)

Quasiquote（反引用）允许在代码模板中嵌入表达式求值：

```mora
-- 基本 quasiquote
let x = 10
let code = `(let y = ,x)     -- `,` 是 unquote，x 被求值

-- unquote-splice
let items = [1, 2, 3]
let code = `(sum ,,items)    -- `,,` 是 unquote-splice，items 被展开
```

| 语法 | 含义 |
|------|------|
| `` `(...) `` | Quasiquote — 代码模板，内部不求值 |
| `,expr` | Unquote — 求值 expr 并嵌入 |
| `,,expr` | Unquote-splice — 求值 expr 并展开（用于列表） |

---

## 12. stdlib

### 12.1 内置函数

| 函数 | 签名 | 说明 |
|------|------|------|
| `print(x)` | `any -> nil` | 输出到 stdout |
| `range(start, end, step)` | `number, number, number -> list<number>` | 生成数值列表 |
| `len(x)` | `string \| list \| dict -> number` | 长度 |
| `compose(f1, f2, ...)` | `...closure -> compose` | 组合函数 (v0.18) |
| `partial(fn, args...)` | `closure, ...any -> partial` | 部分应用 (v0.18) |
| `atom(value)` | `any -> atom` | 创建可变引用 (v0.19) |
| `swap(atom, fn)` | `atom, closure -> any` | 原子更新 (v0.19) |
| `deref(atom)` | `atom -> any` | 读取引用值 (v0.19) |
| `type_of(value)` | `any -> string` | 返回类型名 (v0.20) |
| `is_instance(value, type)` | `any, string -> bool` | 类型检查 (v0.20) |
| `methods_of(value)` | `any -> list<string>` | 返回方法列表 (v0.20) |
| `eval(code)` | `string -> any` | 运行时编译并执行代码 (v0.86) |
| `quote(expr)` | `any -> code` | 表达式 → Code（不求值） (v0.86) |
| `read(str)` | `string -> code` | 字符串 → Code (v0.86) |
| `gensym()` | `-> string` | 生成唯一符号 (v0.86) |
| `macroexpand(name, args)` | `string, list -> code` | 展开宏调用 (v0.86) |
| `apply(fn, args)` | `closure, list -> any` | 将函数应用到参数列表 (v0.86) |
| `car(list)` | `list -> any` | 取列表首元素 (v0.86) |
| `cdr(list)` | `list -> list` | 取列表尾部 (v0.86) |
| `cons(head, tail)` | `any, list -> list` | 构造列表 (v0.86) |

### 12.1.0 math.* 模块 (v0.91)

标量数学函数（APL/StreamIt 启发）。所有 Int 输入自动转 Float，结果 Float。

| 函数 | 签名 | 说明 |
|------|------|------|
| `math.sin/cos/tan/asin/acos/atan` | `number -> float` | 三角函数 |
| `math.sinh/cosh/tanh` | `number -> float` | 双曲函数 |
| `math.exp/log/log2/log10/log1p` | `number -> float` | 指数与对数 |
| `math.sqrt/cbrt` | `number -> float` | 根号 |
| `math.pow(x, y)` | `number, number -> float` | 幂 |
| `math.hypot(a, b)` | `number, number -> float` | 欧氏距离 |
| `math.abs/sign/signum` | `number -> number` | 绝对值/符号（保留 Int 类型） |
| `math.floor/ceil/round/trunc/fract` | `number -> number` | 取整（保留 Int 类型） |
| `math.PI / math.E / math.TAU` | `-> float` | 常量 |
| `math.INF / math.NAN` | `-> float` | 特殊值 |
| `math.is_nan/is_inf/is_finite(x)` | `number -> bool` | 类型谓词 |

### 12.1.0 stats.* 模块 (v0.91)

描述性统计（作用于 list<number>，结果 Float）。

| 函数 | 签名 | 说明 |
|------|------|------|
| `stats.sum/mean/median/min/max` | `list -> float` | 中心趋势 |
| `stats.var/stddev` | `list -> float` | 离散度 |
| `stats.quantile(list, q)` | `list, float -> float` | 分位数（q ∈ [0,1]） |
| `stats.histogram(list, bins)` | `list, int -> list<dict>` | 直方图 |
| `stats.corr/cov(a, b)` | `list, list -> float` | 相关性 |

### 12.1.0 linalg.* 模块 (v0.91)

基础线性代数（向量=list，矩阵=list<list>）。

| 函数 | 签名 | 说明 |
|------|------|------|
| `linalg.dot(a, b)` | `list, list -> float` | 点积 |
| `linalg.cross(a, b)` | `list, list -> list` | 3D 叉积 |
| `linalg.norm(v, [p])` | `list, float -> float` | Lp 范数 |
| `linalg.matmul(A, B)` | `list<list>, list<list> -> list<list>` | 矩阵乘 |
| `linalg.transpose(A)` | `list<list> -> list<list>` | 矩阵转置 |

### 12.1.0 random.* 模块 (v0.91, v0.99 ambient effect 化)

PRNG（手写 xoshiro256** + splitmix64，零外部依赖，确定性可重放）。

**v0.99 起 random 是 ambient effect**：状态不再驻留进程级全局 Mutex，
而是运行时单属主纯值（每个运行时实例独立种子，worker 克隆按值隔离，
并发无锁无交错）；每次调用携带 ambient 效果行（§7.7），可用
`handle random_* { ... } { ... }` 按动态作用域覆写（如测试中固定随机值）。

| 函数 | 签名 | 说明 |
|------|------|------|
| `random.random()` | `-> float` | [0, 1) |
| `random.rand_int(min, max)` | `float, float -> float` | [min, max) 整数值（实参按语言字面量规则收 float） |
| `random.rand_float(min, max)` | `float, float -> float` | [min, max) 浮点 |
| `random.rand_choice(list)` | `list -> any` | 随机选一 |
| `random.seed(n)` | `float -> nil` | 重置本运行时的种子 |
| `random.shuffle(list)` | `list -> list` | Fisher-Yates 打乱 |

### 12.2 String 方法

| 方法 | 签名 | 说明 |
|------|------|------|
| `.len()` | `-> number` | 长度 |
| `.upper()` | `-> string` | 转大写 |
| `.lower()` | `-> string` | 转小写 |
| `.trim()` | `-> string` | 去空白 |
| `.starts_with(s)` | `string -> bool` | 前缀检查 |
| `.ends_with(s)` | `string -> bool` | 后缀检查 |
| `.contains(s)` | `string -> bool` | 包含检查 |
| `.split(sep)` | `string -> list<string>` | 分割 |
| `.replace(from, to)` | `string, string -> string` | 替换 |
| `.json()` | `-> result<dict, string>` | 解析 JSON |

### 12.3 List 方法

| 方法 | 签名 | 说明 |
|------|------|------|
| `.push(item)` | `any -> list` | 追加（返回新列表） |
| `.pop()` | `-> list` | 弹出末尾（返回新列表） |
| `.get(i)` | `number -> any` | 按索引获取 |
| `.len()` | `-> number` | 长度 |
| `.map(fn)` | `closure -> list` | 映射 |
| `.filter(fn)` | `closure -> list` | 过滤 |
| `.reduce(fn, init)` | `closure, any -> any` | 归约 |
| `.take(n)` | `number -> list` | 取前 n 个元素 (v0.18) |
| `.drop(n)` | `number -> list` | 跳过前 n 个元素 (v0.18) |
| `.window(size)` | `number -> list` | 滑动窗口 (v0.17) |
| `.batch(size)` | `number -> list` | 批次处理 (v0.17) |
| `.shape()` | `-> list` | 返回维度 (v0.17) |
| `.flatten()` | `-> list` | 展平嵌套 (v0.17) |
| `.transpose()` | `-> list` | 转置二维列表 (v0.17) |
| `.reshape(rows, cols)` | `number, number -> list` | 重塑列表 (v0.17) |

### 12.3.1 广播算术 (v0.17, APL 启发)

Mora 支持列表的广播算术操作：

```mora
-- list * scalar: 每个元素乘以标量
[1, 2, 3] * 2       -- [2, 4, 6]

-- scalar + list: 标量加到每个元素
1 + [10, 20, 30]     -- [11, 21, 31]

-- list + list: 逐元素相加（等长）
[1, 2, 3] + [10, 20, 30]  -- [11, 22, 33]

-- list - list: 逐元素相减
[10, 20, 30] - [1, 2, 3]  -- [9, 18, 27]
```

支持的操作符：`+`, `-`, `*`, `/`, `%`

### 12.4 Dict 方法

| 方法 | 签名 | 说明 |
|------|------|------|
| `.get(key)` | `string -> any` | 按键获取 |
| `.set(key, val)` | `string, any -> dict` | 设置键值（返回新字典） |
| `.keys()` | `-> list<string>` | 所有键 |
| `.values()` | `-> list` | 所有值 |
| `.len()` | `-> number` | 大小 |
| `.json()` | `-> result<dict, string>` | 解析 JSON |

### 12.5 JSON 模块

| 函数 | 签名 | 说明 |
|------|------|------|
| `json.parse(text)` | `string -> value` | 解析 JSON |
| `json.stringify(val)` | `any -> string` | 序列化 JSON |

> ✅ **v0.110.0（基于 D197 演进）**：JSON 整数超出 `i64` 范围时**不再**静默降级为 f64 —— 在 `BigInt` 模块化移出核心运行时后，超界整数直接返回**显式溢出错误**，确保绝不静默丢精度。
>
> | JSON 数字 | 解析结果 | `type_of` |
> |---|---|---|
> | `5` | `5` | `int` |
> | `9223372036854775807`（`i64::MAX`） | 原值，**精确** | `int` |
> | `9223372036854775808`（超出 `i64`） | **显式错误** | — |
> | `1.5` | `1.5` | `float` |
>
> 解析边界：空 dict/list、顶层数组、深层嵌套、转义序列（`\"` `\\` `\n`）、`null` → `nil`、重复键后者覆盖前者；非法 JSON 与尾逗号均**明确报错**（非静默）。

### 12.6 File 模块

| 函数 | 签名 | 说明 |
|------|------|------|
| `file.read_text(path)` | `string -> string` | 读取文本 |
| `file.write_text(path, content)` | `string, string -> nil` | 写入文本 |
| `file.exists(path)` | `string -> bool` | 路径存在 |
| `file.list(path)` | `string -> list<string>` | 列出目录 |
| `file.mkdir_all(path)` | `string -> nil` | 递归创建目录 |
| `file.join(parts...)` | `...string -> string` | 路径拼接 |

完整 API 见 `file.read_bytes`、`file.write_bytes`、`file.append_text`、`file.remove`、`file.rename`、`file.copy`、`file.touch`、`file.is_file`、`file.is_dir`、`file.size`、`file.cwd`、`file.chdir`、`file.home_dir`、`file.abs`、`file.basename`、`file.dirname`、`file.extname`。

### 12.7 Exec 模块

| 函数 | 签名 | 说明 |
|------|------|------|
| `exec.parallel(cmds, [max_concurrent], [timeout_ms])` | `list<string>, number?, number? -> list<dict>` | 并发执行命令列表 |

每条返回字典包含 `index`、`cmd`、`stdout`、`stderr`、`exit_code`、`elapsed_ms`、`pid`、`error` 字段；`max_concurrent` 与 `timeout_ms` 须为非负数或 `nil`。

### 12.8 Tensor、自动微分、计算图与显存池原语 (v0.110.0 Wave 3B/3C & Wave 4)

| 构造器 / 原语 | 签名 / 用法 | 说明 |
| :--- | :--- | :--- |
| `Tensor::zeros(shape, dtype?, device?)` | `list<int>, string?, string? -> tensor` | 创建全 0 张量（支持命名参数 `dtype="f16", device="cpu"`） |
| `Tensor::ones` / `Tensor::full` / `Tensor::eye` / `Tensor::arange` / `Tensor::randn` / `Tensor::rand` / `Tensor::from_list` | `... -> tensor` | 张量构造器族 |
| `Tensor::devices()` / `Tensor::device_info(dev?)` | `-> list<string>` / `string? -> dict` | 枚举可用计算设备（`cpu`/`cuda:0`/`vulkan:0`/`metal:0`）与查询硬件驱动/VRAM/SM 遥测 |
| `grad(fn, argnums?)` / `value_and_grad(fn, argnums?)` | `closure, ... -> closure` | 反向模式自动微分（VJP），支持高阶嵌套 `grad(grad(f))` |
| `jvp(fn, primals, tangents)` | `closure, T, T -> (T, T)` | 前向模式自动微分（JVP），可与 `grad` 组合计算精确 HVP |
| `trace_dag(fn, inputs)` | `closure, list<tensor> -> compute_dag` | 追踪构建 `TensorComputeDag`（支持 `.compile()`、`.run(inputs)`、`.memory_plan()`、`.cache_stats()`、`.backend_info()`） |
| `kv_allocator(pages, page_tokens, heads, head_dim, dtype?, device?, max_bytes?)` | `... -> kv_allocator` | PagedAttention KV 页分配器（支持 `alloc_seq`、`append_tokens`、`fork_seq`、`evict_lru`、`evict_preempt`、`reclaim_for`、`set_quota`、`on_watermark`） |
| `memory_pool(max_bytes, device?)` | `int, string? -> memory_pool` | 64B 对齐分桶 Slab 张量缓存池（支持 `alloc`、`zeros`、`full`、`release`、`trim`、`set_quota`、`on_watermark`、`stats`） |

### 12.9 异步 TCP 网络原语 (v0.110.0 Wave 2)

| 函数 / 方法 | 签名 | 说明 |
| :--- | :--- | :--- |
| `tcp_listen(addr)` | `string -> tcp_listener` | 绑定非阻塞 TCP 监听器（支持 `.accept(ctx?)`、`.local_addr()`、`.close()`、`.is_closed()`） |
| `tcp_connect(addr, timeout_ms?, ctx?)` | `string, int?, context? -> tcp_stream` | 建立非阻塞 TCP 连接（支持 `.read`、`.read_bytes`、`.read_line`、`.write`、`.write_bytes`、`.shutdown`、`.close`、`.local_addr`、`.peer_addr`） |

### 12.10 纯 Mora 标准库 `std/net/http` 与 `std/mcp` (v0.110.0 Wave 4)

- `import "std/net/http"`：提供声明式 `http_router`（`:param` 路径参数、`*` 通配、洋葱中间件、自动 `404`/`405+Allow`）、`http_serve_once` / `http_serve_n`、`Transfer-Encoding: chunked` 双向编解码、`max_body_bytes`（超限直接返回 `413 Payload Too Large`）、`Connection: keep-alive` 连接复用与 SSE（`http_sse_open` / `http_sse_send` / `http_read_sse_events`）。
- `import "std/mcp"`：提供 MCP JSON-RPC 2.0 服务端与客户端，支持 `initialize`、`ping`、`tools/list` & `tools/call`、`resources/list` & `resources/read`、`prompts/list` & `prompts/get`、`mcp_schema_from_struct(Struct::schema())` 自动 JSON Schema 生成与 `-32602` 参数校验、HTTP+SSE 双通道传输及 Stdio NDJSON 行协议（`mcp_serve_stdio_lines` / `mcp_serve_stdio_channels` / `mcp_serve_stdio_stream`）。

---

## 13. 形式化

### 13.1 类型系统形式化

Mora 的类型系统是 **结构化类型**（structural typing）+ **名义 trait**（nominal traits）：

```
τ ::= string | char | number | bool | nil
    | list<τ> | dict<τ, τ>
    | τ -> τ                    -- 函数类型
    | τ | τ                     -- union
    | result<τ, τ>
    | dyn T                     -- trait 对象
    | T<τ, ...>                 -- 泛型实例
```

### 13.2 类型判断规则

```
Γ ⊢ n : number                  (T-Num)
Γ ⊢ s : string                  (T-Str)
Γ ⊢ true : bool                 (T-True)

Γ ⊢ e₁ : number  Γ ⊢ e₂ : number
─────────────────────────────────────  (T-Add)
Γ ⊢ e₁ + e₂ : number

Γ ⊢ e : list<τ>  Γ ⊢ i : number
────────────────────────────────  (T-Index)
Γ ⊢ e[i] : τ

Γ ⊢ f : τ₁ -> τ₂  Γ ⊢ a : τ₁
────────────────────────────────  (T-Call)
Γ ⊢ f(a) : τ₂
```

### 13.3 Trait 检查规则

```
Γ ⊢ impl T for U    Γ ⊢ e : U
────────────────────────────────  (T-TraitDispatch)
Γ ⊢ e.method() : τ    where T defines method: ... -> τ
```

**待补充**: T-Let, T-If, T-Match, T-Pipe, T-Question 等判断规则。

---

## 14. 语法

### 14.1 词法

**关键字**：以 `src/lexer.rs` 的 `identifier_from` 表为准（v0.110.0 逐项同步）：

```
as break continue do dyn end enum false fn for if
import in let loop macro match nil quote return Self
struct task then true type with
```

> 注：`with` 仅用于 `match ... with ... end` 模式匹配头。v0.110.0 Wave 1 已移除
> `app` / `document` / `max_rounds` / `orchestrate` / `prompt` / `rel` / `solve` 等关键字。

> 说明：语言保留两类语法识别机制：
>
> 1. **词法关键字**（上表）：由 lexer 直接产出专用 token。
> 2. **标识符分派**：`transaction` / `worker` / `parallel` / `perform` /
>    `handle` / `effect` / `eval` / `aggregate` / `assign` / `while` 等
>    由 parser 按**标识符**+前瞻守卫识别（见 `parser_v3/emit.rs` 的
>    `emit_statement_w`），不是关键字——因此它们仍可作为普通变量名。

**运算符**:

```
+ - * / % = == != > < >= <= | |> -> ? ::
```

**分隔符**:

```
( ) [ ] { } . , :
```

### 14.2 EBNF 语法

```ebnf
program     = { statement } ;
statement   = let_stmt | assign_stmt | task_stmt | if_stmt | for_stmt | while_stmt
            | return_stmt | import_stmt | export_stmt | parallel_stmt | match_stmt
            | worker_stmt | transaction_stmt | macro_stmt | effect_stmt
            | type_alias_stmt | struct_stmt | enum_stmt | trait_stmt | impl_stmt
            | select_stmt | perform_stmt | handle_stmt
            | expr_stmt ;
-- v0.110.0 Wave 1–4：已移除 AI Agent 编排 / TEA / 可观测块声明，
-- 并补齐 Stage 3B/3C 与 Wave 2/4 正式语法（struct/enum/@tag、trait/impl、spawn/select、no_grad、多维切片与命名/默认参数）。

let_stmt    = "let" IDENTIFIER [ ":" type ] "=" expr ;
assign_stmt = "assign" IDENTIFIER "=" expr ;
task_stmt   = "task" IDENTIFIER [ type_params ] "(" annotated_params ")" [ ":" type ] { statement } "end" ;
if_stmt     = "if" expr "then" { statement } { "else" "if" expr "then" { statement } } [ "else" { statement } ] "end" ;
for_stmt    = "for" IDENTIFIER "in" expr { statement } "end" ;
while_stmt  = "while" expr { statement } "end" ;
return_stmt = "return" [ expr ] ;
import_stmt = "import" STRING ;
export_stmt = "export" ( let_stmt | task_stmt | struct_stmt | enum_stmt | type_alias_stmt | trait_stmt | impl_stmt | macro_stmt ) ;
match_stmt  = "match" expr "with" { pattern [ "when" expr ] ( "->" | "=>" ) expr } "end" ;
parallel_stmt = "parallel" { statement } "end" ;
worker_stmt = "worker" IDENTIFIER { statement } "end" ;
transaction_stmt = "transaction" { statement } [ "compensation" { statement } ] "end" ;
macro_stmt  = "macro" IDENTIFIER "(" params ")" { statement } "end" ;
effect_stmt = "effect" IDENTIFIER "(" [ type { "," type } ] ")" [ ":" type ] ;

-- v0.110.0 Stage 3C：名义类型、编译期 @tag 元数据与源码级 trait / impl
type_alias_stmt = "type" IDENTIFIER "=" type ;
struct_stmt = "struct" IDENTIFIER [ type_params ] { struct_field } "end" ;
struct_field = IDENTIFIER ":" type [ "=" expr ] { field_tag } ;
field_tag   = "@" "tag" "(" IDENTIFIER "=" STRING { "," IDENTIFIER "=" STRING } ")" ;
enum_stmt   = "enum" IDENTIFIER [ type_params ] { enum_variant } "end" ;
enum_variant = IDENTIFIER [ "(" type { "," type } ")" ] ;
trait_stmt  = "trait" IDENTIFIER [ type_params ] [ ":" IDENTIFIER { "," IDENTIFIER } ] { trait_method } "end" ;
trait_method = ( "task" | "fn" ) IDENTIFIER [ type_params ] "(" annotated_params ")" [ ":" type ] [ { statement } "end" ] ;
impl_stmt   = "impl" [ type_params ] [ IDENTIFIER [ type_args ] "for" ] IDENTIFIER [ type_args ] { impl_method } "end" ;
impl_method = ( "task" | "fn" ) IDENTIFIER [ type_params ] "(" annotated_params ")" [ ":" type ] { statement } "end" ;

-- v0.110.0 Wave 2：Go 式多路选择与轻量协程
select_stmt = "select" ( "{" { select_arm } "}" | { select_arm } "end" ) ;
select_arm  = [ "case" ] [ IDENTIFIER "=" ] expr ( "->" | "=>" ) select_body
            | [ "case" ] "timeout" "(" expr ")" ( "->" | "=>" ) select_body
            | [ "case" ] "default" ( "->" | "=>" ) select_body ;
select_body = "do" { statement } "end" | expr ;
spawn_expr  = "spawn" spawn_target ;
spawn_target = call | method_call | expr ;

-- v0.110.0 Stage 3C：自动微分隔离块与多维零拷贝切片
no_grad_expr = "no_grad" { statement } "end" ;
tensor_slice = expr "[" slice_axis { "," slice_axis } "]" ;
slice_axis  = "..." | "None" | [ expr ] ":" [ expr ] [ ":" [ expr ] ] | expr ".." expr | expr ;

type_params = "<" IDENTIFIER { "," IDENTIFIER } ">" ;
type_args   = "<" type { "," type } ">" ;
annotated_params = [ IDENTIFIER [ ":" type ] [ "=" expr ] { "," IDENTIFIER [ ":" type ] [ "=" expr ] } ] ;
params      = [ IDENTIFIER { "," IDENTIFIER } ] ;
call_args   = [ call_arg { "," call_arg } ] ;
call_arg    = IDENTIFIER "=" expr | expr ;
variable    = IDENTIFIER ;
call        = IDENTIFIER [ "::" IDENTIFIER ] "(" call_args ")" ;
method_call = expr "." IDENTIFIER "(" call_args ")" ;
index       = expr "[" expr "]" ;
primary     = literal | variable | closure | spawn_expr | no_grad_expr ;
postfix     = primary { "(" call_args ")"
                    | "." IDENTIFIER [ "(" call_args ")" ]
                    | "[" slice_axis { "," slice_axis } "]"
                    | "as" "dyn" IDENTIFIER [ type_args ] } ;

unary       = ( "-" | "!" | "not" ) expr ;
binary      = expr "or" expr
            | expr "and" expr
            | expr ( "==" | "!=" ) expr
            | expr "|>" expr
            | expr ( "<" | ">" | "<=" | ">=" ) expr
            | expr ( "+" | "-" ) expr
            | expr ( "*" | "/" | "%" | "@" ) expr
            | expr "**" expr ;

-- ⚠ `pattern` 的形状逐分支实测自 `parser_v3/syntax.rs::parse_pattern`（D168 / Stage 3C）。
--    字面量分支只接受 NUMBER / STRING / BOOL / NIL。
pattern     = "_"
            | ( ".." | "..." ) [ IDENTIFIER ]
            | IDENTIFIER "::" IDENTIFIER [ "(" [ pattern { "," pattern } ] ")" ]
            | IDENTIFIER "(" [ pattern { "," pattern } ] ")"
            | IDENTIFIER "{" [ IDENTIFIER [ ":" pattern ] { "," IDENTIFIER [ ":" pattern ] } ] [ ".." ] "}"
            | IDENTIFIER ":" pattern
            | IDENTIFIER
            | literal
            | "[" [ pattern { "," pattern } [ "," ( ".." | "..." ) [ IDENTIFIER ] ] ] "]"
            | "{" IDENTIFIER ":" pattern { "," IDENTIFIER ":" pattern } [ ".." ] "}" ;

-- v0.80: 代数效果
perform_stmt = "perform" IDENTIFIER expr ;
handle_stmt  = "handle" IDENTIFIER "{" { statement } "}" "{" { statement } "}" ;

expr_stmt   = expr ;

expr        = literal | variable | unary | binary | call | method_call
            | index | tensor_slice | closure | pipe | question
            | spawn_expr | select_stmt | no_grad_expr | postfix
            | quasiquote | eval_expr | quote_expr ;

literal     = NUMBER | STRING | CHAR | BOOL | NIL | list_literal | dict_literal ;
list_literal = "[" [ expr { "," expr } [ "..." IDENTIFIER ] ] "]" ;
dict_literal = "{" [ IDENTIFIER ":" expr { "," IDENTIFIER ":" expr } ] "}" ;

closure     = "fn" [ type_params ] "(" annotated_params ")" ( expr | "{" { statement } "}" | { statement } "end" ) ;
pipe        = expr "|>" expr ;
question    = expr "?" ;

-- v0.86-v0.88: 同像性
eval_expr   = "eval" "(" expr ")" ;
quote_expr  = "quote" "(" expr ")" ;
quasiquote  = "`" "(" quasiquote_body ")" ;
quasiquote_body = { QUASIWORD | "," expr | ",," expr } ;
```

### 14.3 注释

```mora
-- 单行注释
```

无多行注释。

### 14.4 标识符

- 以字母或下划线开头
- 后续字符可以是字母、数字、下划线
- 大小写敏感
- 无保留字冲突（关键字已从标识符中分离）

---

## 15. 相等性

### 15.1 值相等 (`==`)

| 类型 | 比较方式 |
|------|----------|
| `number` | 数值相等（跨塔精确，规则见 [§3.6 数值塔](#36-数值塔d468)） |
| `string` | 字符串内容相等 |
| `char` | 字符相等 |
| `bool` | 布尔值相等 |
| `nil` | 总是相等 |
| `list` | **深比较**（递归比较每个元素） |
| `dict` | **深比较**（递归比较每个键值对） |
| 其他 | 引用相等（指针比较） |

```mora
[1, 2, 3] == [1, 2, 3]     -- true
{a: 1} == {a: 1}           -- true
nil == nil                  -- true
```

### 15.2 引用相等

无显式引用相等运算符。对于闭包、路由器等引用类型，`==` 比较引用。

### 15.3 不等性 (`!=`)

`!=` 是 `==` 的逻辑取反。

---

## 16. 文本模型

### 16.1 字符串

- 使用双引号：`"hello"`
- 转义序列：`\n`, `\t`, `\\`, `\"`
- 无字符数组概念（字符串是不可变的）

### 16.2 字符串拼接与格式化

```mora
let name = "Alice"
let msg = "Hello, " + name + "!"
```

数值或结构化数据可通过 `json.stringify(val)` 转为文本：

```mora
let x = 10
let msg = "Result: " + json.stringify(x * 2)
```

### 16.3 字符

```mora
let c = 'a'
let newline = '\n'
```

### 16.4 JSON

```mora
let data = json.parse("{\"name\": \"Alice\"}")
let text = json.stringify({name: "Alice"})
```

> JSON 字面量必须用双引号：单引号在 Mora 里是**字符**字面量（见 16.3），
> 写成 `json.parse('{"name": "Alice"}')` 会被解析为超长字符字面量并报错。

---

## 17. 安全

### 17.1 文件系统沙箱隔离 (`FsPolicy` & `CheckedPath`, v0.110.0 Wave 2)

Mora 运行时内置显式文件系统边界隔离（`src/runtime/fs.rs`）：
- 每个 `CoreRuntime` 维护独立的虚拟工作目录 `cwd`（`file.chdir` 仅修改当前运行时的虚拟 `cwd`，绝不调用进程级 `std::env::set_current_dir` 污染并发线程）。
- 所有 `file.*` 与模块读取路径均经过 `FsPolicy` 与 `CheckedPath` 校验：拦截内嵌 NUL 字节、在配置 `workspace_root` 时解析规范路径（`canonicalize`）并拒绝 `..` 或符号链接越界访问，同时支持只读模式（`read_only`）拦截一切写入/删除操作。

### 17.2 边界校验与外部进程安全

- `file.*` 内建在路径参数上执行显式 NUL 字节与 `FsPolicy` 边界校验
- `exec.parallel` 基于 `std::process::Command` + 自研 `AsyncSemaphore` 异步受控执行，严格校验并发上限 `max_concurrent` 与超时参数 `timeout_ms` 为非负数（**历史**：v0.110.0 之前基于 `tokio::process::Command`）
- JSON 与 LSP 传输层强制执行 `MAX_JSON_DEPTH = 256` 迭代解析上限与 `MAX_CONTENT_LENGTH = 16 MiB` 报文头上限
- `MORABC02` 字节码加载器强制校验 64 位 FNV-1a 校验和、变长整数边界与 `verify_program` SSA 结构合法性
- 数值核心对精确 `Int` (`i64`) 与低位宽标量（`i8`..`u64`）运算溢出显式返回错误

### 17.3 效果隔离与纯函数默认

Mora 不保留隐式的全局运行期 Hook 或内置配置块，所有外部 I/O、原位张量变异（`tensor_mutate`）与非确定性行为（`random_*`）均纳入静态效果行（`EffectRow`）追踪，并在调用或求导边界强制校验。

---

## 18. 互操作与云原生服务

Mora 定位为面向大模型底层与计算图/算子表达、同时具备 Go 式云原生属性的静态类型语言；编译器与核心运行时不硬编码特定 Web 框架或 LLM 客户端，而是通过语言核心原语与纯 Mora 标准库 `std/net/http`、`std/mcp` 提供一等服务支持：

- **结构化 JSON 与编译期 `@tag` 反射**：原生 `json.parse` / `json.stringify` 配合名义 `struct` 的 `@tag(json=..., desc=..., optional=...)` 与 `Struct::schema()`，零外部依赖自动生成 JSON Schema 与序列化载荷。
- **Go 式高并发与非阻塞网络 I/O**：`spawn`、`channel<T>`、`select`、`context` 与 `tcp_listen` / `tcp_connect` 直接支撑高并发 HTTP/1.1 Keep-Alive、Chunked 流、SSE 推送与 MCP 服务。

### 18.1 纯 Mora `std/net/http` 与 `std/mcp` 服务示例

```mora
import "std/net/http"
import "std/mcp"

struct EchoInput
  text: string @tag(json="text", desc="Input text")
end

let router = http_router()
let router = http_router_get(router, "/echo/:msg", fn(req)
  return http_json_response(200, {echo: http_param(req, "msg")})
end)

let mcp = mcp_server("demo-mcp", "1.0.0")
let mcp = mcp_register_typed_tool(mcp, "echo", "Echo tool", EchoInput::schema(), fn(args) return {content: args.get("text")} end)
```

### 18.2 外部工具与系统进程协同

可通过标准库 `exec.parallel` 与 `file.*` 对接外部命令行工具、传输适配器或批处理算子：

```mora
-- 通过 JSON 载荷与结构化结果互操作
let req = json.stringify({id: 1, method: "ping"})
let parsed = json.parse(req)
```

### 18.3 HTTP / MCP 协议友好特性总结

| 需求 | Mora 核心语言原语与标准库 |
|------|--------------------------|
| JSON-RPC / REST 载荷与 Schema | `json.parse` / `json.stringify` + `struct` `@tag` + `Struct::schema()` |
| 异步高并发与取消传播 | `spawn` / `channel<T>` / `select` / `context` / `tcp_listen` / `tcp_connect` |
| HTTP/1.1 与流式 SSE | `std/net/http`（`Router`、`chunked`、`keep-alive`、`max_body_bytes` 413 守卫、SSE） |
| MCP 协议服务与客户端 | `std/mcp`（Tools / Resources / Prompts、HTTP+SSE 与 Stdio NDJSON 传输） |

### 18.4 JSON 互操作

```mora
-- 序列化
let json_str = json.stringify({name: "Alice", age: 30})

-- 反序列化
let data = json.parse(json_str)
```

---

## 19. 工具链

### 19.1 CLI

```bash
mora <file.mora>                          # 编译并运行 Mora 源文件
mora run <file.mora>                      # 显式子命令运行源文件
mora build <file.mora> [-o <app>]         # 编译为自包含零源码 MORABC02 单二进制可执行文件
mora build <file.mora> --emit-bc [-o <f>] # 导出预编译 MORABC02 (.morac) SSA 字节码产物
mora <file.morac>                         # 零解析/零类型检查直接执行预编译 .morac 字节码
mora --opt=N <file.mora>                  # 指定 SSA 优化级别运行（0=关闭 / 1=基础 / >=2=激进）
mora --check <file.mora>                  # 仅执行语法与统一 HM 静态类型检查
mora --repl                               # 启动交互式 REPL
mora install <url>                        # 从 URL 安装 .mora 脚本至 vendor/
mora --version                            # 打印版本号（或 -v）
mora --help                               # 打印 CLI 帮助（或 -h）
```

### 19.2 LSP (`mora-lsp`)

Mora 提供基于 stdio JSON-RPC 2.0 的 Language Server Protocol 实现（`mora-lsp` 二进制），接入统一 HM 推断引擎 `infer_program_types`：

- 实时语法与带精确 `Span` 的 HM 静态类型诊断（含 `Tensor<DType, Shape, Device>` 维度/设备/效果错误）
- 悬停类型签名展示（Hover）
- 关键字、内建函数、模块、`Tensor::*` 与方法补全（Completion）
- 定义跳转（Go to Definition）、引用查找（References）、符号重命名（Rename）、文档符号（Document Symbols）、格式化（Formatting）、语义高亮（Semantic Tokens）与代码折叠（Folding Ranges）

### 19.3 基于代数效果的确定性测试

通过标准代数效果 `handle ... { ... } { ... }` 直接拦截 `perform` 调用或 `random_*` ambient 效果，即可在测试中实现纯函数式替身注入与多分支确定性重放。

---

## 20. 版本

### 20.1 版本语义

Mora 使用 **语义化版本**（Semantic Versioning）：

```
v0.x.y  -- 0.x 阶段，架构演进可包含显式记录的 breaking change
v1.0.0  -- 首个长期稳定版本
```

### 20.2 当前版本

**v0.110.0** — 全波次（Wave 1–Wave 4 + P1/P2）架构重铸定版：
- **Wave 1**：移除越位应用层 DSL 与重依赖，`BigInt` 独立至 `std/math/bigint`（**纯 Mora** 标准库模块 `std/math/bigint.mora`），核心收敛为 12 个顶层模块（**v0.110.0 起零第三方生产依赖** —— 原 `libc`/`parking_lot`/`tokio` 已全部清零）。
- **Wave 2**：统一至单遍 `ControlProgram` SSA IR + 堆帧状态机 VM（`O(1)` TCO + 多发定界续延 `Value::Continuation`）、**自研** `TaskScheduler`/`AsyncIoReactor`（**历史**：原为 Tokio 实现）、Go 式 `spawn`/`channel<T>`/`select`/`context`/`sleep`、非阻塞异步 TCP（`tcp_listen`/`tcp_connect`）与 `FsPolicy`/`CheckedPath` 文件系统隔离。
- **Wave 3（Stage 3A / 3B / 3C）**：确立严格 `Int`/`Float` 字面量精度与硬件低位宽后缀（`f16`/`bf16`/`fp8`/`i4`..`i64`）、`**`/`@`/`!`、单一 `Span` 追踪 HM 约束求解器、一等 64B 对齐 `Tensor<DType, Shape, Device>` 与 5 项静态检查（DType/Shape/Device/Resource/Effect）、名义泛型 `struct`/`enum`/`@tag`、源码级 `trait`/`impl`/`dyn Trait`、具名/默认参数、多维零拷贝切片与反向自动微分 `grad`/`value_and_grad`/`no_grad`。
- **Wave 4（Track 4-1 / 4-2 / 4-3 + P1/P2）**：落地 `TensorComputeDag`（`trace_dag` 4 遍算子融合 `FusedFlashAttention`/`FusedRmsNormLinear`/`FusedLinearAct`/`FusedElementwise`、`DagMemoryPlan` 静态草稿复用、`DagPlanCache`）、前向 `jvp` 与多磁带任意阶微分 `grad^n` & HVP、零依赖动态加载 **CUDA PTX / Vulkan SPIR-V / Metal MSL / CPU AVX2+NEON** 硬件计算后端、PagedAttention `KVPageAllocator`（`kv_allocator`）与 64B Slab `MemoryPool`（`memory_pool`）、纯 Mora `std/net/http`（Router/Chunked/Keep-Alive/413/SSE）与 `std/mcp`（Tools/Resources/Prompts/`Struct::schema()`/Stdio）标准库，以及零源码 `MORABC02` 预编译字节码单二进制 `mora build` 与 `.morac`。

### 20.3 版本演进里程碑

| 阶段 | 核心里程碑 |
|------|------------|
| Wave 1 (`v0.110.0`) | 编译器瘦身解耦、剥离旧解释器与重依赖、`std/math/bigint` 独立化 |
| Wave 2 (`v0.110.0`) | 单一 `ControlProgram` SSA IR、堆帧状态机 VM、Go 式并发与异步 TCP |
| Wave 3 (`v0.110.0`) | 精确数值塔、一等 `Tensor`、5 项 AI-Native 静态检查、名义 `struct`/`enum`/`trait`/`impl` 与反向 AD |
| Wave 4 (`v0.110.0`) | `TensorComputeDag` 深度融合、高阶 AD、零依赖 CUDA/Vulkan/Metal/SIMD 后端、`KVPageAllocator`/`MemoryPool`、纯 Mora `std/net/http` & `std/mcp`、`MORABC02` 单二进制构建 |

---

## 附录

### A. 关键字完整列表

以 `src/lexer.rs::identifier_from` 为唯一事实源（v0.110.0 更新，见 §14.1）：

```
Self as break continue do dyn end enum false fn for if import
in let loop macro match nil quote return struct task then true
type with
```

说明：`with` 保留为词法关键字，专用于 `match expr with ... end` 模式匹配分支语法。

由 parser 按标识符+前瞻守卫分派的语句/表达式构造（§14.1 第 2 类）：`assign` `while` `export` `trait` `impl` `transaction` `worker` `parallel` `perform` `handle` `effect` `eval` `aggregate` `spawn` `select` `no_grad`。

### B. 内置类型完整列表

```
string char int float number bool nil
f16 bf16 fp8 fp8_e4m3 fp8_e5m2 f32 f64 i4 i8 i16 i32 i64 u8 u16 u32 u64
list dict tensor compute_dag kv_allocator memory_pool
task closure result any
channel task_handle context tcp_listener tcp_stream continuation external
struct enum trait concrete union compose partial atom macro arrow code effect handler
```

### C. 运算符优先级

| 优先级 | 运算符 |
|--------|--------|
| 1 (高) | `()` `[]` `.` `::` `as dyn` |
| 2 | `**` (右结合幂运算) |
| 3 | `!` `not` `-` (一元) |
| 4 | `*` `/` `%` `@` (矩阵乘/张量收缩) |
| 5 | `+` `-` |
| 6 | `<` `>` `<=` `>=` |
| 7 | `\|>` (管道) |
| 8 | `==` `!=` |
| 9 | `and` |
| 10 | `or` |
| 11 | `?` |
| 12 (低) | `=` |

### D. 文件扩展名

- `.mora` — Mora 源文件
- `.morac` — `mora build --emit-bc` 生成的预编译 `MORABC02` 二进制 `ControlProgram` 字节码文件

---
