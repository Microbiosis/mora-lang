//! v0.51: 注入 MORAGIT_VERSION 给编译期使用
//!
//! 策略: cargo 自动把 CARGO_PKG_VERSION 注入 build script 进程环境,
//! 直接 emit 它即可。`mora::VERSION` 常量在 lib.rs 用 `env!("MORAGIT_VERSION")`
//! 编译期展开, 零运行时开销。
//!
//! 这是版本号叙事的单一真相源:
//!   - main.rs banner  → `format!("Mora v{}", mora::VERSION)`
//!   - mcp_server.rs  → serverInfo.version
//!   - lsp/server.rs  → serverInfo.version
//!   - bin/lsp.rs     → println!(...)
//!   - document/mod.rs 错误信息 → 引用时取
//!
//! 历史: v0.50 之前这些位置硬编码 "0.04" / "0.1" / "0.1.0" / "v0.25" / "v0.28" 等,
//! 与 Cargo.toml 0.0.53 不同步, 在 release 时容易漂移。

fn main() {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rustc-env=MORAGIT_VERSION={}", version);
}
