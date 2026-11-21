# Mora 语言设计影响源（v0.110.0）

> 9 个学习对象加上系统级原语借鉴，共同塑造了 **Mora v0.110.0**——一门面向大模型底层、计算图与算子、大模型推理/训练与动态资源调控，同时具备 Go 式云原生并发与纯 Mora HTTP/MCP 标准库的 **AI-native 静态类型语言**。

---

## 1. Clojure — 默认不可变、持久化向量与软件事务内存（STM）

**基因**：值语义 + 分块持久化向量（`src/value/list.rs`）+ 原子引用（`atom`/`swap`/`deref`）+ 事务回滚（`transaction`）

```clojure
;; Clojure: 所有数据结构默认不可变，状态变更通过 atom / dosync 显式进行
(def a [1 2 3])
(def b (conj a 4))
(def counter (atom 0))
(swap! counter inc)
```

**Mora v0.110.0 体现**：
```mora
let a = [1, 2, 3]
let b = a |> push(4)   -- a 保持 [1, 2, 3]，b 为 [1, 2, 3, 4]
assign a = [10]        -- 显式重新绑定

let counter = atom(0)
swap(counter, fn(n) n + 1 end)
print(deref(counter))  -- 1
```

| Clojure 原则 | Mora v0.110.0 实现 |
| :--- | :--- |
| 默认不可变绑定 | `let` 默认不可变，仅 `assign` 允许显式局部重绑定 |
| 持久化序列 | `Value::List` 采用分块持久化向量（`src/value/list.rs`），支持结构共享 |
| 受控共享可变状态 | `atom(v)` / `swap(a, f)` / `deref(a)` 线程安全原子更新 |
| 事务与补偿 | `transaction ... compensation ... end` 失败自动回滚环境与补偿执行 |
| REPL 驱动开发 | `mora --repl`（`src/cli/repl.rs`） |

---

## 2. Common Lisp — 编译期宏、代数效果与多范式融合

**基因**：卫生/模板宏系统、条件恢复（在 Mora 中升级为带静态效果行的多发定界续延代数效果 `effect`/`perform`/`handle`）

```lisp
;; Common Lisp: 宏变换与 handler-case / restart 条件恢复
(defmacro when (condition &body body)
  `(if ,condition (progn ,@body)))
```

**Mora v0.110.0 体现**：
```mora
macro unless(cond, body)
  if !cond then
    body
  end
end

effect Ask(string): int

let total = handle Ask {
  let a = perform Ask("first")
  let b = perform Ask("second")
  a + b
} {
  resume(10) + resume(20)
}
print(total)
```

| Common Lisp 原则 | Mora v0.110.0 实现 |
| :--- | :--- |
| 多范式共存 | 函数式 + 命令式 + 名义 `struct`/`enum`/`trait`/`impl` + 声明式计算图 |
| 可恢复条件系统 | 升级为静态类型化代数效果 `effect`/`perform`/`handle` + 多发定界续延 `resume`（`Value::Continuation`） |
| 显式错误代数类型 | 协变 `result<T, E>`（`Ok(v)` / `Err(e)`）配合 `match` 解构 |
| 宏与元编程 | `macro` + `macroexpand` + `quote` / `quasiquote` |

---

## 3. Prolog — 结构化模式匹配、守卫与合一思想

**基因**：声明式模式解构、守卫条件（`when`）、编译期 HM 约束与维度符号合一（Unification）

```prolog
% Prolog: 合一与守卫
classify(0, zero).
classify(N, positive) :- N > 0.
```

**Mora v0.110.0 体现**：
```mora
enum Shape
  Circle(float)
  Rect(float, float)
end

struct Point
  x: int = 0
  y: int = 0
end

let describe = fn(s: Shape)
  match s with
    Shape::Circle(r) when r > 10.0 -> "large circle"
    Shape::Circle(_) -> "small circle"
    Shape::Rect(w, h) -> "rect"
  end
end

print(describe(Shape::Circle(15.0)))
```

| Prolog 原则 | Mora v0.110.0 实现 |
| :--- | :--- |
| 模式匹配与守卫 | `match expr with ... when cond -> ... end`（支持字面量、列表 `[h, ...t]`、字典 `{k: v, ..}`、名义 `Struct { .. }` 与 `Enum::Variant(...)`） |
| 约束合一（Unification） | `src/typeck/hm/` 单一 `Span` 追踪 HM 约束求解器 + 张量符号维度（`[M, K] @ [K, N] -> [M, N]`）合一 |

---

## 4. Lisp — 代码即数据（Homoiconicity）

**基因**：同像性（`Value::Code`）、准引用与展开拼接（`` `(...) ``、`,expr`、`,,expr`）、运行时编译求值 `eval`

```lisp
(+ 1 2)
'(+ 1 2)
(eval '(+ 1 2))
```

**Mora v0.110.0 体现**：
```mora
let expr = quote(1 + 2)
let ast = read("let x = 42")
let sym = gensym()

let items = [1, 2, 3]
let spliced = `(sum ,,items)

let lst = cons(1, cons(2, nil))
print(car(lst), cdr(lst), sym)
```

| Lisp 原则 | Mora v0.110.0 实现 |
| :--- | :--- |
| 代码即数据 | `quote` / `read` / `eval` / `macroexpand` + 一等 `Value::Code` |
| 准引用与列表拼接 | `` `( ... ,x ,,xs ) ``（`TokenType::CommaComma`） |
| 序对与列表原语 | `cons` / `car` / `cdr` / `apply` |

---

## 5. Ballerina & Go — 云原生并发原语与服务友好标准库

**基因**：语言级轻量协程（`spawn`）、类型化不变通道（`channel<T>`）、公平多路选择（`select`）、分层取消上下文（`context`）、非阻塞异步 TCP（`tcp_listen`/`tcp_connect`）与单二进制交付（`mora build`）

> **v0.110.0 架构演进**：Mora 不再将 HTTP Router 或 MCP Server 硬编码在编译器与 `Value` 中，而是提供坚实的 Go 式并发 + 异步网络原语 + `@tag` 编译期结构体反射，在标准库 `std/net/http` 与 `std/mcp` 中**用纯 Mora 语言自身实现**。

**Mora v0.110.0 体现**：
```mora
import "std/net/http"
import "std/mcp"

struct WeatherArgs
  city: string @tag(json="city", desc="Target city name")
  days: int = 3 @tag(json="days", optional="true")
end

let schema = mcp_schema_from_struct(WeatherArgs::schema(), "Weather query")
let router = http_router()
let router = http_router_get(router, "/health/:node", fn(req)
  return http_json_response(200, {ok: true, node: http_param(req, "node")})
end)
```

| 云原生原则 | Mora v0.110.0 实现 |
| :--- | :--- |
| 轻量协程与通道 | `spawn`（`task_handle<T>`）、`channel(cap)`（`channel<T>` 不变类型）、`select`（`case`/`timeout`/`default`）、`context()` |
| 非阻塞网络 I/O | `tcp_listen` / `tcp_connect` / `tcp_listener` / `tcp_stream`（基于**自研** `AsyncIoReactor`） |
| 纯 Mora HTTP/SSE 标准库 | `std/net/http.mora`（声明式 `Router`、`:param`/`*`、中间件、`chunked`、`keep-alive`、`max_body_bytes` 413 守卫、SSE 流） |
| 纯 Mora MCP 标准库 | `std/mcp.mora`（JSON-RPC 2.0、Tools/Resources/Prompts、`Struct::schema()` 自动派生、HTTP+SSE 与 Stdio NDJSON） |
| 零源码单二进制交付 | `mora build <file.mora> -o <app>`（预编译 `MORABC02` 二进制 `ControlProgram` 字节码，零源码嵌入） |

---

## 6. StreamIt — 数据流管道与计算图算子融合

**基因**：声明式管道组合（`|>`）与静态数据流子图优化（在 Mora Wave 4 中演进为 `TensorComputeDag` 算子融合流水线）

**Mora v0.110.0 体现**：
```mora
-- 标量/集合流式管道
let words = "hello cloud native mora"
  |> upper()
  |> split(" ")
  |> filter(fn(w) len(w) > 4 end)

-- 神经网络计算图追踪与 4 遍自动算子融合（FlashAttention / RmsNormLinear / LinearAct / Elementwise）
let x = Tensor::ones([2, 4])
let w = Tensor::ones([4, 3])
let b = Tensor::zeros([3])
let dag = trace_dag(fn(inp, weight, bias)
  return (inp @ weight + bias).gelu()
end, [x, w, b]).compile()
let out = dag.run([x, w, b])
```

| StreamIt 原则 | Mora v0.110.0 实现 |
| :--- | :--- |
| 管道组合 | `\|>` 管道运算符 + `compose` / `partial` / `curry` |
| 计算图融合与 Buffer 复用 | `trace_dag` → `TensorComputeDag`（4 遍算子融合 + `DagMemoryPlan` 静态草稿区复用 + `DagPlanCache`） |

---

## 7. APL — 数组/张量原语、矩阵收缩与高表达密度

**基因**：一等硬件位宽 `Tensor<DType, Shape, Device>`、矩阵/批量张量收缩 `@`、幂 `**`、多维广播切片（`[:, ..., None, ::-1]`）与高阶自动微分

**Mora v0.110.0 体现**：
```mora
-- 一等 64 字节对齐紧凑张量与硬件低位宽后缀（f16/bf16/fp8/i4..i64）
let a = Tensor::from_list([[1.0, 2.0], [3.0, 4.0]])
let b = Tensor::eye(2)
let c = (a @ b)[0..2, ::-1]

-- 一阶/高阶自动微分（反向 grad、前向 jvp、精确 HVP）
let f = fn(x: float) return x ** 3.0 end
let d1 = grad(f)
let d2 = grad(d1)
print(d1(2.0), d2(2.0))   -- 12.0, 12.0
```

| APL / 数组语言原则 | Mora v0.110.0 实现 |
| :--- | :--- |
| 一等张量与硬件位宽 | `Tensor<DType, Shape, Device>`（`f64`/`f32`/`f16`/`bf16`/`fp8_e4m3`/`fp8_e5m2`/`i4`..`i64`/`u8`..`u64`/`bool`） |
| 矩阵乘与多维切片 | 原生 `@` 算子、`**` 幂运算、零拷贝步幅切片 `t[0..4, :, None, ::-1]` |
| 原生自动微分 | `grad` / `value_and_grad` / `jvp` / `no_grad` / 任意阶 `grad^n` & HVP |
| 数学/统计/线性代数 | `math.*`、`stats.*`、`linalg.*`、`random.*` 原生互通 `Tensor` 与 `List`，大整数独立为 `std/math/bigint` |

---

## 8. Logo — 渐进复杂度与高可读性

**基因**：无分号、清晰的 `task ... end` / `struct ... end` 块结构、可选类型注解（局部双向推断 + 全局 HM 求解）

```mora
-- Level 1: 极简脚本直接运行
print("Hello, Mora!")

-- Level 2: 带默认参数与命名参数的任务
task scale_vec(v: list<float>, factor: float = 2.0): list<float>
  return v.map(fn(x) x * factor end)
end

print(scale_vec([1.0, 2.0], factor=3.0))
```

---

## 9. Smalltalk — 统一方法分派与源码级 `trait` / `impl` / `dyn`

**基因**：所有核心值（包括 `Tensor`、`Channel`、`TcpStream`、`KVPageAllocator`、`MemoryPool`、用户自定义 `struct`/`enum`）均支持一致的 `.method(...)` 调用与源码级 `trait` / `impl` 扩展。

```mora
trait Describable
  task summary(self): string
    return "object"
  end
end

struct ModelConfig
  hidden_size: int = 768
  layers: int = 12
end

impl Describable for ModelConfig
  task summary(self): string
    return "transformer"
  end
end

let cfg = ModelConfig { hidden_size: 1024 }
let d: dyn Describable = cfg as dyn Describable
print(d.summary())
```

---

## 10. 底层系统与运行时原语对照（v0.110.0 核实）

| Mora 核心子系统 | 借鉴来源 | 核心机制 | 实现模块（`src/` & `std/`） |
| :--- | :--- | :--- | :--- |
| 单遍 Control SSA IR 与优化器 | MLIR / Cranelift SSA | 显式基本块 `BasicBlock`、`ControlInst`、`ControlTerminator`、CopyProp/GVN-CSE/LICM/DCE 与 `MORABC02` 二进制编解码 | `src/mir/control/{ir,lower,verify,opt,codec}.rs` |
| 堆帧状态机 VM 与定界续延 | OCaml 5 / Chez Scheme | 显式 `Vec<CallFrame>`、`O(1)` 尾调用帧复用、`CapturedFrames` 多发定界续延切片重放 | `src/runtime/vm/engine.rs`、`src/runtime/vm/continuation.rs` |
| 5 项 AI-Native 静态检查 | Dex / Jaxtyping / Koka | 单一 `Span` 追踪 HM 求解器：静态校验 `DType`、`Shape`（符号维度）、`Device`、`Resource`（原位别名）、`Effect` | `src/typeck/hm/{mod,infer,stage3c}.rs` |
| `TensorComputeDag` 与融合后端 | XLA / PyTorch 2 Inductor / FlashAttention-2 | 4 遍融合（`FusedFlashAttention`/`FusedRmsNormLinear`/`FusedLinearAct`/`FusedElementwise`）、`DagMemoryPlan`、动态 CUDA PTX / Vulkan SPIR-V / Metal MSL / CPU AVX2+NEON 后端 | `src/runtime/{compute_dag,simd_kernels}.rs`、`src/runtime/gpu/{mod,cuda,vulkan,metal}.rs` |
| `KVPageAllocator` 与 `MemoryPool` | vLLM PagedAttention / CUDA Caching Allocator | 64B 对齐分页 KV 物理池、CoW `fork_seq`、硬字节配额、优先级抢占驱逐、上升沿回差水位线回调与 2 次幂 Slab 复用池 | `src/runtime/{kv_cache,memory_pool}.rs` |
| 协程调度器与异步 Reactor | Go Runtime / Tokio | `TaskScheduler`、`MoraChannel`（全局 ID 保序加锁公平 `select`）、`TaskContext` 级联取消、`AsyncIoReactor` 非阻塞 TCP | `src/runtime/{scheduler,reactor,fs}.rs` |
