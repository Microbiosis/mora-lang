//! Mora 标准库（v11+ 拆出）
//!
//! 暴露 lexer / parser / ast / interpreter / typeck / lsp 六个模块，
//! 让 CLI binary（src/main.rs）和 LSP server binary（src/bin/lsp.rs）共享。

pub mod ai_infra;
pub mod ast_v2;
// v0.42.1: Audit Sink — SHA-256 hash-chained JSONL (loongclaw-inspired)
pub mod audit;
// v0.45.0: ToolPlane Core/Extension adapter (loongclaw tool.rs pattern)
pub mod ccr;
pub mod common;
pub mod compress;
pub mod document;
pub mod event;
pub mod flow;
pub mod http_server;
pub mod interpreter;
pub mod lexer;
pub mod lsp;
pub mod mcp_server;
pub mod mock;
pub mod parser_v2;
pub mod record;
pub mod sandbox;
pub mod schedule;
pub mod toolplane;
pub mod trace_collector;
pub mod typeck;
pub mod value;
