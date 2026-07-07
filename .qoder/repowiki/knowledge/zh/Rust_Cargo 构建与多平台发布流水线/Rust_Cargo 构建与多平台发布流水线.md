---
kind: build_system
name: Rust/Cargo 
category: build_system
scope:
    - '**'
source_files:
    - Cargo.toml
    - build.rs
    - Dockerfile
    - .github/workflows/ci.yml
    - .github/workflows/release.yml
    - .github/workflows/reusable-rust-ci.yml
    - docker-compose.yml
---

## 1. 

- ****: Cargo ( `Cargo.toml` `0.0.53`)
- **/**: Rust edition 2024MSRV 1.85 ureq 3.3 
- ****:  crate  `[[bin]]` 
  - `mora` — REPL / CLI / HTTP-MCP  (`src/main.rs`)
  - `mora-lsp` — LSP  (`src/bin/lsp.rs`)
- ****:
  - `jit` →  inkwell + LLVM17 SSA 
  - `checkpoint-sqlite` →  rusqlite 
- **** `build.rs` `CARGO_PKG_VERSION`  `MORAGIT_VERSION`  `env!()` 

## 2. 

- `Cargo.toml`: features
- `build.rs`:  `MORAGIT_VERSION` 
- `Dockerfile`:  musl strip  ~10MB
- `.github/workflows/ci.yml`: PR/push CIcheck/test/fmt/clippy/integration/LSP smoke/record snapshot
- `.github/workflows/release.yml`:  tag `v*`  5  + Docker  + GitHub Release 
- `.github/workflows/reusable-rust-ci.yml`: 
- `docker-compose.yml`:  REPL/HTTP/MCP/observe 

## 3. 

### 3.1 
 `src/`MIR/SSALSP  crate `[[bin]]`  crate VS CodeNeovimHelixSublimeVimEmacs `mora-lsp`  LSP 

### 3.2 
 `Cargo.toml`  `version``build.rs`  `MORAGIT_VERSION`CLI bannerMCP serverInfoLSP serverInfo

### 3.3 
 async runtime v0.50  tokio  HTTP/MCP server  feature OCR/PDF/Office  optional features 

### 3.4 
Dockerfile  `rust:alpine` + `musl-dev`  `alpine:3.21` root CMD  `--repl``docker-compose.yml`  service  `examples/` 

## 4. CI/CD 

### CIPR/push main
- check: `cargo check --all-targets`
- test:  ubuntu/windows/macos + stable/nightly `--lib`  `--all-targets`
- fmt: `cargo fmt --all -- --check`
- clippy: `--all-targets --all-features -- -D warnings`
- integration:  release  `mora` `examples/*.mora`
- lsp:  `mora-lsp`  `examples/lsp_smoke` LSP 
- record:  `mora record list`  `mora snapshot` 

### Releasetag `v*`
 5 x86_64-unknown-linux-gnux86_64-unknown-linux-muslaarch64-apple-darwinx86_64-apple-darwinx86_64-pc-windows-msvc `mora`  `mora-lsp` tar.gz/zip  artifact linux/amd64 Docker  `mora-docker-amd64.tar.gz` GitHub Release SHA-256 checksums  release notes

## 5. 

1.  `Cargo.toml`  `[[bin]]`  `release.yml` matrix  target 
2.  `Cargo.toml`  `version` `env!("MORAGIT_VERSION")` 
3.  optional feature  `ci.yml`  clippy  `--all-features` 
4.  `release.yml`  matrix  Dockerfile  `--target`
5.  LSP `cargo build --release --bin mora-lsp`  `examples/lsp_smoke.py`  LSP  `./target/release/mora-lsp`
6. `docker compose up <service>`  Rust 