//! v0.52 ADR-001: 6 Domain Facade 容器模块
//!
//! 每个 facade 是一个 BC 的状态 + 行为封装：
//! - AiRuntime       (BC3)
//! - OrchRuntime     (BC4)
//! - PersistRuntime  (BC5)
//! - SandboxRuntime  (BC7)
//! - RegistryRuntime (BC8)
//! - InfraRuntime    (BC9)
//!
//! 跨 facade 协作通过显式依赖注入（参数传 &mut facade），避免 borrow 摩擦。

pub mod ai;
pub mod core;
pub mod infra;
pub mod orch;
pub mod persist;
pub mod registry;
pub mod sandbox;
