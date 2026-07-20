//! Mora 标准库（v11+ 拆出）
//!
//! 暴露 lexer / parser / ast / interpreter / typeck / lsp 六个模块，
//! 让 CLI binary（src/main.rs）和 LSP server binary（src/bin/lsp.rs）共享。

/// v0.51: 版本号叙事的单一真相源。
///
/// 编译期从 build.rs 注入的 `MORAGIT_VERSION` env 读取 (来自 `Cargo.toml`
/// 的 `version` 字段)。`env!` 在编译期展开, 零运行时开销。
///
/// 改 `Cargo.toml` 的 version 后, 下次 `cargo build` 自动重 build.rs,
/// 所有引用 `mora::VERSION` 或 `env!("MORAGIT_VERSION")` 的位置同步更新。
pub const VERSION: &str = env!("MORAGIT_VERSION");

pub mod ai_infra;
pub mod ast_v2;
// v0.42.1: Audit Sink — SHA-256 hash-chained JSONL (loongclaw-inspired)
pub mod audit;
// v0.45.0: ToolPlane Core/Extension adapter (loongclaw tool.rs pattern)
pub mod ccr;
// v0.50: Checkpoint persistence layer (Memory + SQLite)
pub mod checkpoint;
// v0.46.0: MoraSkillSpec + dual registry (CLI-Anything SKILL.md pattern)
pub mod common;
// v0.47.0: DAG-as-data orchestration (OpenFugu §1.6)
pub mod orchestrate_dag;
// v0.49.0: stress tests (concurrency / correctness / resource-leak)
#[cfg(test)]
pub mod stress_tests;
// v0.47.0: Heartbeat executable checklist (mimiclaw §1.5)
pub mod compress;
pub mod document;
pub mod event;
pub mod flow;
pub mod heartbeat;
pub mod http_server;
pub mod interpreter;
pub mod lexer;
pub mod lsp;
pub mod mcp_server;
pub mod mir;
pub mod mock;
pub mod parser_v2;
pub mod plan;
pub mod record;
pub mod refine;
pub mod runtime;
pub mod sandbox;
pub mod schedule;
pub mod skill;
pub mod toolplane;
pub mod trace_collector;
pub mod typeck;
pub mod value;
