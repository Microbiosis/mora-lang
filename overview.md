# Mora-lang  Bug 

>  Bug   
> 2026-07-10  
> 

## 

- **`ARCHITECTURE_BUG_DETECTION_REPORT_2026-07-10.md`** — 

## 

|  |  |
|---|---|
| `cargo build --all-targets` |   |
| `cargo test --all` |  755  / 0  / 15 ignored |
| `cargo fmt --check` |  `src/flow.rs` 2  |
| `cargo clippy --all-targets --all-features -- -D warnings` |  `src/interpreter/execute.rs` 2  dead-code  |

## 

1. **Interpreter **`src/interpreter/mod.rs`  33  god object  7  runtime facade 35 
2. **`builtins.rs` **5100  85 `unwrap`100 `panic`139 `expect`
3. **`unwrap`  423 **checkpoint/sqlite.rs 104 / 1000 
4. ****`execute.rs``evaluate.rs``ai_chat.rs``lexer.rs``parser_v2/*`
5. ** `panic!`**`src/document/mod.rs`  `AGENTS.md` 
6. **6  `unsafe`** 

## 

1.  `cargo fmt`  `cargo fmt --check`
2. / `execute.rs`  dead-code  Clippy
3.  `builtins.rs`  `checkpoint/sqlite.rs`  `unwrap`
4. 
5.  6  `unsafe`
