//! v0.51:  MORAGIT_VERSION
//!
//! : cargo  CARGO_PKG_VERSION  build script ,
//!  emit `mora::VERSION`  lib.rs  `env!("MORAGIT_VERSION")`
//! ,
//!
//! :
//!   - main.rs banner  → `format!("Mora v{}", mora::VERSION)`
//!   - mcp_server.rs  → serverInfo.version
//!   - lsp/server.rs  → serverInfo.version
//!   - bin/lsp.rs     → println!(...)
//!   - document/mod.rs  →
//!
//! : v0.50  "0.04" / "0.1" / "0.1.0" / "v0.25" / "v0.28" ,
//!  Cargo.toml 0.0.53 ,  release

fn main() {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rustc-env=MORAGIT_VERSION={}", version);
}
