//! Mora LSP server binary
//!
//! 用法：在编辑器中配置 LSP 启动命令为 `mora-lsp`
//! 通信：stdin/stdout 上的 JSON-RPC 2.0 + LSP 协议

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        match args[1].as_str() {
            "--version" | "-v" => {
                println!("mora-lsp 0.1.0");
                return;
            }
            "--help" | "-h" => {
                println!("mora-lsp — Mora language server");
                println!("Communication: stdin/stdout (JSON-RPC 2.0 + LSP)");
                println!("Configure your editor to launch this binary as the LSP server.");
                return;
            }
            _ => {}
        }
    }
    mora::lsp::run();
}
