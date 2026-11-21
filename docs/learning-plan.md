# Mora 语言特性与架构演进蓝图（v0.110.0 全波次落定版）

> 本文档记录 Mora 从早期多范式特性融入（P1–P5）到 `v0.110.0` 四波次（Wave 1–Wave 4 + P1/P2）底层重铸的完整特性映射与实现位置。

---

## 1. 全景阶段总览

| 阶段 / 波次 | 核心主题 | 关键交付 | 状态 | 核心实现模块 |
| :--- | :--- | :--- | :---: | :--- |
| **早期 P1–P5** | 多范式语言内核 | 模式匹配与守卫、管道 `\|>` 与窗口/重塑、闭包与组合子、`parallel`/`worker`/`transaction`/`atom`、Lisp 同像性（`macro`/`quote`/`quasiquote`/`eval`） | ✅ 完成 | `src/parser_v3/`、`src/runtime/builtins/` |
| **Wave 1** | 架构解耦与编译器瘦身 | 移除 20+ 越位 Agent/办公/TEA/Rel 模块与重依赖；核心收敛为 12 个顶层模块（**v0.110.0 起零第三方生产依赖**，原 `libc`/`parking_lot`/`tokio` 已全部清零）；`BigInt` 独立至 `std/math/bigint` | ✅ 完成 | `src/lib.rs`、`std/math/bigint/` |
| **Wave 2** | 单一 Control SSA IR、堆帧 VM 与云原生并发 | 单遍 `MirWitness[] → ControlProgram` SSA IR + 优化器；堆帧 VM（`O(1)` 尾调用 + 多发定界续延 `Value::Continuation`）；Go 式 `spawn`/`channel<T>`/`select`/`context`/`sleep`、异步 `tcp_listen`/`tcp_connect` 与 `FsPolicy`/`CheckedPath` | ✅ 完成 | `src/mir/control/`、`src/runtime/{vm,scheduler,reactor,fs}.rs` |
| **Wave 3 — Stage 3A** | 精确数值与统一 HM 求解器 | 严格分离 `Int` (`i64`) / `Float` (`f64`) 字面量、硬件低位宽后缀（`f16`/`bf16`/`fp8`/`i4`..`i64`）、`**`/`@`/`!`；单一 `Span` 追踪 HM 约束求解器（`check_program_witnesses`） | ✅ 完成 | `src/lexer.rs`、`src/typeck/hm/` |
| **Wave 3 — Stage 3B** | 一等 `Tensor` 与 5 项 AI-Native 静态检查 | 64 字节对齐紧凑 `AlignedByteBuffer`（含 `i4` 半字节打包）、零拷贝视图；编译期 `DType` / `Shape` / `Device` / `Resource` / `Effect` 5 项静态检查 | ✅ 完成 | `src/common/dtype.rs`、`src/value/tensor.rs`、`src/typeck/hm/infer.rs` |
| **Wave 3 — Stage 3C** | 名义类型、源码 `trait`/`impl`、多维切片与反向 AD | 名义泛型 `struct`（默认值 + `@tag` + `Struct::schema()`）/ `enum`、源码级 `trait`/`impl`/`dyn Trait`、具名与默认参数、零拷贝多维切片 `t[0..2, :, None, ::-1]`、反向自动微分 `grad`/`value_and_grad`/`no_grad` | ✅ 完成 | `src/typeck/hm/stage3c.rs`、`src/runtime/autograd.rs` |
| **Wave 4 — Track 4-1** | `TensorComputeDag`、深度融合、高阶 AD 与硬件 GPU 后端 | `trace_dag` 4 遍融合（`FusedFlashAttention`/`FusedRmsNormLinear`/`FusedLinearAct`/`FusedElementwise`）、`DagMemoryPlan` & `DagPlanCache`；前向 `jvp`、任意阶 `grad^n` & HVP；`AVX2+FMA`/`NEON` 多线程核 + 零依赖动态加载 **CUDA PTX / Vulkan SPIR-V / Metal MSL** 硬件后端 | ✅ 完成 | `src/runtime/{compute_dag,simd_kernels,autograd_higher_order}.rs`、`src/runtime/gpu/` |
| **Wave 4 — Track 4-2** | 动态显存与资源调控（`KVPageAllocator` & `MemoryPool`） | 64B 对齐分页 KV 物理池、CoW `fork_seq`、硬字节配额、序列优先级抢占驱逐、无死锁水位线回调；64B 对齐 2 次幂 Slab `memory_pool` | ✅ 完成 | `src/runtime/{kv_cache,memory_pool}.rs` |
| **Wave 4 — Track 4-3** | 纯 Mora `std/net/http` & `std/mcp` 与 `MORABC02` 单二进制 | 纯 Mora `std/net/http.mora`（Router/Chunked/Keep-Alive/413/SSE）与 `std/mcp.mora`（Tools/Resources/Prompts/`Struct::schema()`/Stdio）；`mora build` 零源码 `MORABC02` 字节码单二进制与 `.morac` | ✅ 完成 | `std/net/http.mora`、`std/mcp.mora`、`src/mir/control/codec.rs`、`src/cli/bundle.rs` |

---

## 2. 核心特性设计与实现细节

### 2.1 模式匹配、解构与名义代数数据类型（P1 + Stage 3C）

```mora
enum Response<T>
  Success(T)
  Failure(int, string)
end

struct User
  name: string @tag(json="name")
  age: int = 18 @tag(json="age", optional="true")
end

let u = User { name: "Alice", age: 24 }
let tag = match u with
  User { name: n, age: a } when a >= 18 -> n
  _ -> "minor"
end
```

- **实现要点**：
  - `src/parser_v3/syntax.rs` & `emit.rs`：解析字面量、列表 `[head, ...tail]`、字典 `{k: v, ..}`、名义结构体 `Struct { field: pat, .. }` 与枚举变体 `Enum::Variant(p1, p2)` 模式及 `when` 守卫。
  - `src/typeck/hm/stage3c.rs`：静态校验结构体字段存在性、类型兼容性与枚举变体载荷数。

### 2.2 管道、列表/张量重塑与广播算术（P2 + Stage 3B/3C）

```mora
-- 列表管道与窗口/重塑
-- 注：list.reshape(rows, cols) 对列表元素按 ravel 顺序复制，不足则循环重复，但多出元素绝不静默丢弃（报错拦截）。
let grid = [1, 2, 3, 4, 5, 6].reshape(2, 3)

-- 张量零拷贝步幅视图与多维广播切片
let t = Tensor::arange(0.0, 12.0, 1.0).reshape([3, 4])
let sub = t[0..2, :, None]
```

- **实现要点**：
  - `src/runtime/builtins/method.rs`：`list.reshape(rows, cols)` 元素按 ravel 顺序复制，不足则循环重复，当源长度大于目标 `rows * cols` 时显式报错拒绝静默丢数据（见 `tests/reshape_no_silent_drop.rs`）。
  - `src/value/tensor.rs`：`Tensor` 支持零拷贝 `transpose(d0, d1)`（含负轴 `-1, -2`）、`reshape` 与多维步幅切片 `slice_axes`（`Index`/`Range`/`Ellipsis`/`NewAxis`）。

### 2.3 一等 `Tensor`、5 项 AI-Native 静态检查与自动微分（Stage 3B/3C + Track 4-1）

```mora
-- 编译期静态检查 DType / Shape / Device / Resource / Effect
let x: Tensor<f64, [2, 3], cpu> = Tensor::ones([2, 3], dtype="f64")
let w: Tensor<f64, [3, 4], cpu> = Tensor::ones([3, 4], dtype="f64")
let y: Tensor<f64, [2, 4], cpu> = x @ w

-- 反向 grad、前向 jvp 与高阶 HVP
let loss = fn(v)
  return (v * v) * v
end
let vg = value_and_grad(loss)(Tensor::from_list([1.0, 2.0, 3.0]))
let fwd = jvp(loss, Tensor::from_list([1.0, 2.0, 3.0]), Tensor::ones([3]))
```

- **实现要点**：
  - `src/common/dtype.rs` & `src/value/tensor.rs`：64 字节对齐 `AlignedByteBuffer`，支持 `f64`/`f32`/`f16`/`bf16`/`fp8_e4m3`/`fp8_e5m2`/`i64`..`i4`/`u64`..`u8`/`bool`。
  - `src/runtime/autograd.rs` & `src/runtime/autograd_higher_order.rs`：多磁带 `TapeNodeId` 追踪，支持任意阶导数 `grad(grad(f))`、三模式精确 Hessian-Vector Product 与 `trace_dag(grad(f))` 反向图融合。

### 2.4 计算图算子融合、硬件 GPU 后端与显存池调控（Track 4-1 & Track 4-2）

```mora
-- 查询可用硬件计算设备（CPU SIMD / CUDA / Vulkan / Metal）
let devs = Tensor::devices()
let info = Tensor::device_info("cpu")

-- 动态 KV-Cache 页表与 64B 对齐 Slab MemoryPool
let kv = kv_allocator(64, 16, 8, 64, "f16", "cpu", 1048576)
kv.alloc_seq("req-1", 10)
kv.append_tokens("req-1", 24)
kv.fork_seq("req-1", "req-2")

let pool = memory_pool(4194304, "cpu")
let buf = pool.zeros([128, 128], "f32")
pool.release(buf)
```

- **实现要点**：
  - `src/runtime/compute_dag.rs`：4 遍融合（`FusedFlashAttention` `Br=32, Bc=32` 在线 Softmax、`FusedRmsNormLinear`、`FusedLinearAct`、`FusedElementwise`）+ `DagMemoryPlan` 静态生命周期复用 + `DagPlanCache`。
  - `src/runtime/gpu/{mod,cuda,vulkan,metal}.rs`：通过 `DynLib` 零依赖动态加载系统 GPU 驱动（NVIDIA CUDA Driver API + PTX ISA 7.0 5 核 + `CudaVramArena`、Vulkan 1.0 + 纯 Rust SPIR-V 1.0 计算管线、macOS Metal MSL）。
  - `src/runtime/kv_cache.rs` & `src/runtime/memory_pool.rs`：支持硬字节配额、序列优先级抢占与上升沿水位线回调（经堆帧 VM `CombinatorFrame::WatermarkCallbacks` 无死锁触发）。

### 2.5 云原生并发、纯 Mora HTTP/MCP 标准库与 `MORABC02` 单二进制构建（Wave 2 & Track 4-3）

```bash
# 编译为自包含零源码单二进制可执行文件
mora build app.mora -o app

# 或导出预编译 MORABC02 字节码产物并直接执行
mora build app.mora --emit-bc -o app.morac
mora app.morac
```

- **实现要点**：
  - `src/mir/control/codec.rs` & `src/cli/bundle.rs`：确定性 LEB128 二进制编解码器（`MORABC02` + FNV-1a 64 位校验 + 加载期 `verify_program`），单二进制内不含任何 `.mora` 明文源码。
  - `std/net/http.mora` & `std/mcp.mora`：完全使用 Mora 原生并发、异步 TCP 与 `@tag` 结构体反射编写的声明式 HTTP/SSE 与 MCP 标准库。
