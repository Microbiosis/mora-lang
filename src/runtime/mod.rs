//! v0.110.0: 核心运行时模块
//!
//! 包含语言核心执行状态（`core`）、确定性环境 PRNG（`random`）、堆帧状态机虚拟机（`vm`）、
//! 原生自动微分引擎（`autograd` / `autograd_higher_order`）、计算图算子融合与硬件 GPU/SIMD 后端
//! （`compute_dag` / `simd_kernels` / `gpu`）、显存页表与分桶内存池（`kv_cache` / `memory_pool`）、
//! 文件系统沙箱（`fs`）以及零依赖自研多线程任务调度器（`scheduler`）与非阻塞异步 I/O Reactor（`reactor`）。

// Wave 3 Stage 3C / Wave 4 P1: Reverse/forward/higher-order automatic differentiation engine
pub mod autograd;
pub(crate) mod autograd_higher_order;
pub mod builtins;
// Wave 4: Native TensorComputeDag, operator fusion & DeviceBackend abstraction
pub mod compute_dag;
pub mod core;
pub use core::{CoreRuntime, EngineShared};
// v0.110.0: 文件系统隔离策略、独立 virtual_cwd 与 CheckedPath 凭证
pub mod fs;
// Wave 4 P2-2: Native hardware GPU drivers (CUDA PTX / Vulkan SPIR-V / Metal MSL)
pub(crate) mod gpu;
// Wave 4: PagedAttention-style dynamic KV-cache resource allocator & general MemoryPool
pub mod kv_cache;
pub mod memory_pool;
// v0.99: ambient random —— PRNG 纯值 + ambient effect 操作分发
pub mod random;
// Wave 2 W2.4: Zero-dependency native async I/O reactor and structured task scheduler
pub mod reactor;
pub mod scheduler;
// Wave 4 P1: Hardware-SIMD & multi-threaded cache-blocked CPU kernels
pub(crate) mod simd_kernels;
// v0.110.1: Zero-dependency native async TLS 1.2/1.3 transport (Windows Schannel / Unix OpenSSL)
pub mod tls;
// Wave 2 W2.3: Heap-framed stackful VM & delimited continuations
pub mod vm;
