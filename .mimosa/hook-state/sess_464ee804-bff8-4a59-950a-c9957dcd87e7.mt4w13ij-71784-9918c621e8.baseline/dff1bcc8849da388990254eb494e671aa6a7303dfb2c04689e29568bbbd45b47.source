//! Mora LSP server 实现
//!
//! 入口：`pub fn run()`
//!
//! 内部模块：
//! - `transport` — JSON-RPC 帧解析（Content-Length 协议）
//! - `json` — 手写 JSON Value + parser + serializer（零依赖）
//! - `server` — 主循环 + 路由 + diagnostics
//! - `providers` — 各 LSP method 的实现

pub mod json;
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
