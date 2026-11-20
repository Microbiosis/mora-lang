# Decision Log: 个人项目约束（不引入借鉴项目硬约束）

**Date**: 2026-08-08（v0.110.0 校准）  
**Version**: v0.110.0  
**Status**: Active

## 背景

早期调研参考开源项目（Rust / TypeScript / Kotlin / Go / Mojo / Zig）时，曾出现将外部项目的发布流程或时间盒纪律当作硬约束的倾向。本决策记录明确 Mora 的自治边界。

## 决策

**所有外部项目的「时间盒纪律」与「公司化发布流程」都不作为 Mora 的硬约束；Mora 唯一遵循的硬约束是 `AGENTS.md` §0–§7 确立的工程与架构原则（架构优先、从底层原语完整实现、测试同步、禁止兼容补丁桥）。**

### 1. 不引入外部时间盒或流程枷锁

- **不预设截止日**：尊重作者主动拍板的里程碑与节奏，不假设外部窗口期压力。
- **不保留历史兼容包袱**：在 `0.x` 阶段，定位漂移或双轨冗余模块直接在大版本里程碑（如 `v0.110.0` Wave 1–4）彻底剥离并重铸底层原语，不留空壳兼容桥（`AGENTS.md §6`）。
- **反保守重构原则**：当架构收口需要跨模块迁移时，从底层原语、类型系统、IR/VM 到标准库与测试一次性完整闭环，而非打局部补丁。

## Mora v0.110.0 的核心技术护城河（已在仓内实测验证）

1. **统一 `Span` 追踪 Hindley-Milner 类型系统与 5 项 AI-Native 静态检查**：
   - 单一入口 `check_program_witnesses`（`src/typeck/hm/`），编译期强制校验 `DType`、`Shape`（符号维度与广播求解）、`Device`（静态跨设备禁令）、`Resource`（原位修改与别名安全）、`Effect`（封闭效果行包含与自动微分纯度）。
2. **单遍 `ControlProgram` SSA IR、堆帧状态机 VM 与 `MORABC02` 零源码字节码**：
   - `MirWitness[] → ControlProgram` 单遍降低、SSA 验证器与优化器（CopyProp / GVN-CSE / LICM / DCE）、显式 `Vec<CallFrame>` 堆帧 VM（`O(1)` 尾调用优化 + 多发定界续延 `Value::Continuation`），以及 `mora build` 零源码 `MORABC02` 单二进制与 `.morac` 预编译字节码直跑。
3. **原生一等 `Tensor`、`TensorComputeDag` 算子融合、高阶 AD 与零依赖 GPU 驱动后端**：
   - 64 字节对齐紧凑 `AlignedByteBuffer`（含 `f16`/`bf16`/`fp8`/`i4` 半字节打包）、零拷贝多维步幅切片、反向 `grad`/前向 `jvp`/高阶 `grad^n` & HVP、`trace_dag` 4 遍融合（`FusedFlashAttention`、`FusedRmsNormLinear`、`FusedLinearAct`、`FusedElementwise`）、`DagMemoryPlan` 静态草稿复用，以及零外部编译依赖动态加载的 **CUDA PTX / Vulkan SPIR-V / Metal MSL / CPU AVX2+NEON** 后端。
4. **大模型显存动态调控原语**：
   - PagedAttention 风格 `KVPageAllocator`（`kv_allocator`，支持 CoW `fork_seq`、硬字节配额、序列优先级抢占驱逐与无死锁回差水位线回调）与 64B 对齐分桶 Slab `MemoryPool`（`memory_pool`）。
5. **Go 式云原生并发与纯 Mora `std/net/http` & `std/mcp` 标准库**：
   - `spawn`、`channel<T>`、`select`、`context`、异步 `tcp_listen`/`tcp_connect` 配合编译期 `@tag` 结构体反射，直接用 Mora 语言自身构建完整的 HTTP/1.1（Router / Chunked / Keep-Alive / 413 / SSE）与 MCP（Tools / Resources / Prompts / `Struct::schema()` / Stdio NDJSON）标准库。
6. **HM 统一的 `mora-lsp` 语言服务器与编辑器生态**：
   - 受限 `Content-Length` 与迭代式 RFC 8259 JSON 解析，实时提供张量维度/设备/效果推导悬停、补全、跳转、重命名与格式化。
