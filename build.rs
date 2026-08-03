//! v0.51: 版本号同步（MORAGIT_VERSION）
//!
//! cargo 构建脚本：读取 `CARGO_PKG_VERSION`，以 `cargo:rustc-env` 注入
//! `MORAGIT_VERSION` 环境变量。lib.rs 通过 `env!("MORAGIT_VERSION")`
//! 读取并暴露为 `mora::VERSION`（版本号叙事的单一真相源）。
//!
//! 消费者：
//!   - main.rs banner  → `format!("Mora v{}", mora::VERSION)`
//!   - mcp_server.rs   → serverInfo.version
//!   - lsp/server.rs   → serverInfo.version
//!   - bin/lsp.rs      → println!(...)
//!   - document/mod.rs → 版本号引用
//!
//! 历史：v0.50 时代曾出现过 "0.04" / "0.1" / "0.1.0" / "v0.25" / "v0.28"
//! 等不一致版本号，v0.51 起统一由 Cargo.toml 的 `0.0.53` 驱动，release
//! 构建无需手工同步。

fn main() {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rustc-env=MORAGIT_VERSION={}", version);
}
