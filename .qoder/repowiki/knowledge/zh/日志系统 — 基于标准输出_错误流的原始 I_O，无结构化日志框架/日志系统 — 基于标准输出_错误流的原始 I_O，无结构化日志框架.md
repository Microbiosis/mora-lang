---
kind: logging_system
name:  — / I/O
category: logging_system
scope:
    - '**'
source_files:
    - src/main.rs
    - src/bin/lsp.rs
    - src/trace_collector.rs
---

Cargo.toml  `log``tracing``slog``env_logger``fern``simplelog`  `use` “” Rust  `println!` / `eprintln!`  stdout/stderr


- CLI  `src/main.rs`  `println!` record/replay/snapshot/report  `eprintln!` + `process::exit(N)` 
- LSP  `src/bin/lsp.rs`  `println!`/`eprintln!` 
- interpretertypeckmirrecordhttp_servermcp_server  `eprintln!("...: {}", e)`  `Result`  logger 
- “” `src/trace_collector.rs` Trace/Metrics 

“logging_system”—— `println!` `eprintln!` /