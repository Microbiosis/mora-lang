//! v0.110.0: 核心运行时模块
//!
//! 保留语言核心状态（`core`）、确定性 PRNG 状态（`random`）、堆栈帧式连续体虚拟机（`vm`）
//! 以及基于 `tokio` 的任务调度器（`scheduler`）与异步 I/O 多路复用器（`reactor`）。

pub mod builtins;
pub mod core;
pub use core::{CoreRuntime, EngineShared};
// v0.110.0: 文件系统隔离策略、独立 virtual_cwd 与 CheckedPath 凭证
pub mod fs;
// v0.99: ambient random —— PRNG 纯值 + ambient effect 操作分发
pub mod random;
// Wave 2 W2.4: tokio-backed async I/O reactor and structured task scheduler
pub mod reactor;
pub mod scheduler;
// Wave 2 W2.3: Heap-framed stackful VM & delimited continuations
pub mod vm;
