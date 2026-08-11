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

// v0.52 ADR-001: facade 模块 — Interpreter 字段 pub 让 binary crate 访问
// 后续 Task 7 阶段会考虑加 accessor
pub mod ai;
// v0.75.25: 活 AI 基础设施（ContextWindow/SpeculativeVerifier/CacheWarmer）
// 自 src/ai_infra.rs 迁入；12 个出生即死的规划类型随旧文件删除
pub mod ai_infra;
pub mod core;
// v0.80: EffectHandler trait + EffectRegistry（Stage 2/4 algebraic effects 落地）
pub mod effect;
pub mod infra;
pub mod orch;
pub mod persist;
pub mod registry;
pub mod sandbox;
pub mod types;
