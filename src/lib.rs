//! Mora 标准库（v11+ 拆出）
//!
//! 暴露 lexer / parser / ast / interpreter / typeck / lsp 六个模块，
//! 让 CLI binary（src/main.rs）和 LSP server binary（src/bin/lsp.rs）共享。

pub mod ast;
pub mod interpreter;
pub mod lexer;
pub mod parser;
pub mod typeck;
pub mod lsp;
pub mod trace_collector;
pub mod http_server;
pub mod mcp_server;
