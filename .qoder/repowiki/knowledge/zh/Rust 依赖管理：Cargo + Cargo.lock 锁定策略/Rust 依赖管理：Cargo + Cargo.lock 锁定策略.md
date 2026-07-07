---
kind: dependency_management
name: Rust Cargo + Cargo.lock 
category: dependency_management
scope:
    - '**'
source_files:
    - Cargo.toml
    - Cargo.lock
    - .github/workflows/ci.yml
    - editors/vscode/package.json
---

## 1. 

- **** Rust/Cargo `Cargo.toml` `Cargo.lock` 
- **** crates.io`registry+https://github.com/rust-lang/crates.io-index` registry`[source]`  `CARGO_REGISTRY_*` 
- **vendoring** `vendor/` `cargo vendor` crates.io 
- **** `build.rs`  `MORAGIT_VERSION` `env!()` “”

## 2. 

|  |  |
|---|---|
| `Cargo.toml` | `[dependencies]``[dev-dependencies]``[features]``[dependencies.inkwell]`  |
| `Cargo.lock` | CI  key |
| `.github/workflows/ci.yml` | CI  `hashFiles('Cargo.lock')`  cache key lockfile  |
| `editors/vscode/package.json` | VS Code  npm  devDependencies |
| `vendor/` |  vendoring  |

## 3. 

### 3.1 
 Cargo crate `mora` `[[bin]]` 
- `mora` — CLI 
- `mora-lsp` — LSP 

 crate 

### 3.2 
`Cargo.toml`  inline comment 
- ****HTTP  `ureq`  `reqwest` tokio async runtime v0.50  HTTP/MCP server  tokio
- ****`SO_REUSEADDR`  `libc`  `socket2` libc 
- **** `undoc``image``rusqlite`  `default-features = false` +  features
- ****JIT  `[features]` + `dep:inkwell` 

### 3.3 
-  `ureq = "3.3"``tokio = { version = "1", ... }` patch/minor 
-  pin  `rten = "0.24"``anyhow = "1"`ocrs  public API  `anyhow::Result`  re-export anyhow
- MSRV v0.x  ureq 3.3 → edition 2024 + MSRV 1.85

### 3.4 
- `proptest = "1.7"`  `[dev-dependencies]`  property-based testing
- PDF/Office/OCR

## 4. 

1. ** `Cargo.lock`**CI  `Cargo.lock`  hash  keylockfile 
2. ** inline comment** `Cargo.toml`  breaking change 
3. ** `default-features = false` +  features** image  png
4. ** `[features]` ** `jit = ["dep:inkwell"]``checkpoint-sqlite = ["rusqlite"]`
5. ** `vendor/` ** vendoring `cargo vendor`  CI 
6. **VS Code **`editors/vscode/package.json`  TypeScript  `Cargo.lock`

## 5. 

- ** registry** crates.io `CARGO_NET_OFFLINE`  `cargo vendor`
- **edition 2024 + MSRV 1.85** Rust 
- **JIT  inkwell (LLVM)** `--features jit`  LLVM 17