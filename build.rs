//! 编译期版本号同步脚本（`MORAGIT_VERSION`）
//!
//! 读取 `Cargo.toml` 的 `CARGO_PKG_VERSION`，通过 `cargo:rustc-env` 注入
//! `MORAGIT_VERSION` 环境变量。`src/lib.rs` 通过 `env!("MORAGIT_VERSION")`
//! 读取并暴露为 `mora::VERSION`（版本号的唯一事实源）。
//!
//! 主要消费者：
//!   - `src/main.rs`     → CLI `--version` 与启动横幅 `format!("Mora v{}", mora::VERSION)`
//!   - `src/lsp/server.rs` → LSP `initialize` 响应 `serverInfo.version`
//!   - `src/bin/lsp.rs`  → `mora-lsp --version`

fn main() {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rustc-env=MORAGIT_VERSION={}", version);
}
