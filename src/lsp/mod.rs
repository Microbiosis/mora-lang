//! Mora LSP server 实现
//!
//! 入口：`pub fn run()`
//!
//! 内部模块：
//! - `transport` — JSON-RPC 帧解析（Content-Length 协议）
//! - `server` — 主循环 + 路由 + diagnostics（JSON 统一复用 `crate::flow::json`）
//! - `providers` — 各 LSP method 的实现

pub mod providers;
pub mod server;
pub mod transport;

pub fn run() {
    let mut s = server::Server::new();
    if let Err(e) = s.run() {
        eprintln!("[mora-lsp] fatal: {}", e);
        std::process::exit(1);
    }
}
