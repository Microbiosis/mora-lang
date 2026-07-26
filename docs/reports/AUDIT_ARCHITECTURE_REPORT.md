# Mora-lang 

> ****:  + 
> ****: `D:\Github\mora-lang`
> ****: v0.0.52 (edition 2024)
> ****: ~40,000 LOC / 77 Rust 

---

## 

### 1.1 

|  |  |  |
|---------|------|------|
|  |   |  14  |
| OWASP  |  2  | CORS  +  0.0.0.0 |
|  |   | `cargo audit`  |
| `unwrap()`  |   |  334  `unwrap()` AGENTS.md |

---

### 1.2 

####  A05:  — CORS 

- ****: `src/http_server.rs:468`
- ****:
  ```rust
  "Access-Control-Allow-Origin: *\r\n"
  ```
- ****: HTTP  `Access-Control-Allow-Origin: *`AI 
- ****:  CORS `localhost`

#### 🟡 A05:  —  0.0.0.0

- ****: `src/interpreter/dispatch.rs:1009-1010`
- ****:
  ```rust
  let addr = args.first().map(|v| v.to_string())
      .unwrap_or_else(|| "0.0.0.0:3000".to_string());
  let (host, port) = addr.split_once(':').unwrap_or(("0.0.0.0", "3000"));
  ```
- ****: `Router.listen()` `0.0.0.0` HTTP 
- ****:  `"127.0.0.1:3000"` `"0.0.0.0"` 

---

### 1.3 

####  `unwrap()` 334 

- ****: HIGH
- ****: AGENTS.md §3 — "**** `unwrap()` / `panic!`"
- ****:
  - `src/interpreter/dispatch.rs`: ~80 
  - `src/interpreter/builtins.rs`: ~60 
  - `src/interpreter/mod.rs`: ~40 
  - `src/typeck/mod.rs`: ~30 
  - : ~124 
- ****:
  ```rust
  // dispatch.rs:496
  .unwrap_or(0)
  
  // interpreter/mod.rs:599
  .lock().map_err(|_| "globals mutex poisoned".to_string())?.get("main").clone()
  ```
- ****:  `file.read_text`  HTTP  panic
- ****:  `?`  `expect("")` 

---

### 1.4 

|  |  |  |
|-----------|------|------|
| `builtins.rs:642` — Command Injection |  | `ContainerHandle::exec()`  Docker  `std::process::Command::exec` |
| `container.rs:251,538` — Command Injection |  | 251 538  |
| `main.rs:347` — Path Traversal |  |  `p"..."`  |
| `compress/*.rs` — Path Traversal |  |  `... [elided]`  |
| `sandbox/mod.rs:205-206` — Path Traversal |  |  |
| 14  High Entropy String |  |  |

---

### 1.5 

> `cargo audit`  Cargo.lock 

|  |  |  |
|------|------|------|
| `ureq` | 3.3 |  |
| `crossbeam-channel` | 0.5.15 |  |
| `lopdf` | 0.42 | PDF  RustSec |
| `libc` | 0.2.x |  minor  patch |
| `chrono` | 0.4.45 |  CVE |
| `flate2` | 1.1 |  |
| `sha2` | 0.10 |  |

****:  `cargo-audit`  `cargo audit` 

---

## 

### 2.1 

```
Lexer (925 LOC)
  → ParserV2 (2,900 LOC: mod.rs + expressions.rs + statements.rs)
    → ASTv2 (686 LOC)
      → TypeCK (3,200 LOC: mod.rs + check.rs)
        → Interpreter (12,700 LOC: 9 files)
          → builtins.rs (5,014 LOC) ← 
          → dispatch.rs (1,337 LOC)
          → execute.rs (1,044 LOC)
          → mod.rs (3,251 LOC)
```

LSP (1,400 LOC)Record (1,900 LOC)Document (2,800 LOC)Sandbox/Schedule/Skill/Plan  (10,000 LOC)

---

### 2.2 

####  #1: `Interpreter`  — 30+ 

- ****: `src/interpreter/mod.rs:137-205`
- ****: `Interpreter`  15+ 
  ```rust
  pub struct Interpreter {
      globals, environment, tool_registry, model_routes, token_budget,
      token_usage, trace, current_ai_config, trait_registry, impl_table,
      recorder, worker_channels, worker_receivers, ai_cache, string_interner,
      draft_model_stats, context_window, speculative_verifier, cache_warmer,
      v2_arena, memory_store, bus, sandbox, scheduler, ccr_store,
      mock_registry, audit_sink, markdown_memory_dir, container,
      tool_planes, skill_registry, plans, refine_registry,
  }
  ```
- ****:
  - skill/plan/refine/container
  - `Clone`  43 
  - 3 `new()` / `new_empty()` / `new_with_globals()` 30+ 
- ****:  —  Facade 

****:  facade
```rust
struct Interpreter {
    core: CoreRuntime,          // globals, environment, v2_arena
    ai: AiRuntime,              // model_routes, context_window, speculative_verifier
    sandbox: SandboxRuntime,    // sandbox, container, audit_sink
    registry: RegistryRuntime,  // tool_planes, skill_registry, plans, refine_registry
    infra: InfraRuntime,        // bus, scheduler, ccr_store, mock_registry
}
```

---

####  #2: `builtins.rs` — 5,014 

- ****: `src/interpreter/builtins.rs`
- ****:  `impl Interpreter` 
  - `call_file_method` (255 ) — 20+ 
  - `call_sandbox_method` (380 ) — 
  - `call_ai_method` (170 ) — 
  - `call_schedule_method` (90 )
  - `call_event_method` (70 )
  - `call_memory_method``call_ccr_method``call_mock_method` 
- ****:
  - ****5,014 0 
  - 
  -  I/O vs Docker  vs AI 
- ****:  —  `call_xxx_method`

****: `src/interpreter/builtins/` 
```
builtins/
  mod.rs          —  `call_builtin(kind, method, args)`
  file.rs         — file.*  (~260 LOC)
  sandbox.rs      — sandbox.* + container.*  (~400 LOC)
  ai.rs           — ai.*  (~200 LOC)
  schedule.rs     — schedule.*  (~100 LOC)
  event.rs        — bus.*  (~80 LOC)
  memory.rs       — memory.* 
  ccr.rs          — ccr.* 
  mock.rs         — mock.* 
```

---

####  #3: `dispatch.rs` — 1,337  `match` 

- ****: `src/interpreter/dispatch.rs`
- ****: `call_method`  `Value` List/Dict/Builtin/String/Stream/Agent/Router/Conversation/TraitObject `match` arm  `method`  `match` ** × ** 
- ****:
  - 
  - 
  - `List.map`  `List.filter`  `Dict` 
- ****:  —  trait 

****: `MethodDispatch` trait
```rust
trait MethodDispatch {
    fn dispatch(&self, method: &str, args: Vec<Value>, interp: &mut Interpreter)
        -> Result<Value, String>;
}

//  Value 
impl MethodDispatch for ListValue { ... }   // dispatch/list.rs
impl MethodDispatch for DictValue { ... }   // dispatch/dict.rs
impl MethodDispatch for BuiltinValue { ... } // dispatch/builtin.rs
```

---

#### 🟡 #4: `main.rs` —  CLI 

- ****: `src/main.rs` (1,042 LOC)
- ****: 
  - 260  `match args[1]` 
  - `run_record` / `run_replay` / `run_diff` / `run_snapshot` 
  - `format_size` / `format_ts` / `format_duration` / `truncate` 
  - `install_package` 
  - MCP CLI 
- ****:  CLI 
- ****: 

****: `src/cli/` 
```
cli/
  mod.rs          — 
  commands/
    run.rs        — mora run <file>
    check.rs      — mora --check
    record.rs     — mora record / replay / diff / snapshot
    mcp.rs        — mora mcp tool-list / tool-search
    install.rs    — mora install <url>
  format.rs       — format_size, format_ts 
```

---

#### 🟡 #5: `parser_v2/statements.rs` — 1,696 

- ****: `src/parser_v2/statements.rs`
- ****: 25+ `let``task``if``for``match``with``transaction``trait``impl``orchestrate``skill``prompt``document`  `impl ParserV2` 
- ****: `ParserV2::declaration()``mod.rs:65-163` 100  `if/else`  40+ token  `mod.rs`  `statements.rs`
- ****: 

****: 
```
parser_v2/stmts/
  mod.rs          — 
  control.rs      — if, for, match, with, parallel
  definitions.rs  — task, trait, impl, type, enum, struct
  orchestrate.rs  — orchestrate, skill, eval
  resources.rs    — prompt, document, transaction
```

---

### 2.3 

|  | LOC |  |  |
|------|-----|--------|------|
| `interpreter/builtins.rs` | 5,014 | **0** |   —  |
| `interpreter/dispatch.rs` | 1,337 | **0** |   |
| `interpreter/execute.rs` | 1,044 | **0** | 🟡  |
| `interpreter/ai_chat.rs` | 834 | **0** | 🟡  mock  |
| `parser_v2/statements.rs` | 1,696 | **0** |   |
| `typeck/check.rs` | 1,174 | **0** |   |
| `interpreter/mod.rs` | 3,251 | ~137 | 🟢  |

****:  Parser → TypeCK → Interpreter builtins → dispatch  ~12,000 LOC 

---

### 2.4 

|  |  |  |
|------|------|------|
| `lsp/mod.rs` | 22  re-export |  |
| `document/backend/mod.rs` | 9  |  re-export  |
| `lsp/providers/mod.rs` | 22  |  re-export 8  provider |
| `common.rs` | 76  | `Span`/`BinaryOp`/`Literal`  |
| `trace_collector.rs` | 268  | `Vec` + `HashMap`  |



---

## 

### 

1. ** CORS ** (`http_server.rs:468`)
   -  `*`  `localhost`

2. **** (`dispatch.rs:1009`)
   -  `"0.0.0.0:3000"` → `"127.0.0.1:3000"`

3. ** cargo-audit **
   ```bash
   cargo install cargo-audit
   cargo audit
   ```

### 

4. ** unwrap() **
   -  `?`  `expect("")`
   -  interpreter/  typeck/  unwrap

5. ** `builtins.rs`**
   - 5,014 LOC 200-400 LOC
   - `Interpreter::call_builtin(kind, method, args)`

6. ** `Interpreter`  Facade**
   -  30+  5-6  facade
   -  facade 

### 

7. ** `dispatch.rs`  trait **
8. ** `main.rs`  `cli/` **
9. ****
   - : parser → typeck → interpreter builtins → dispatch 

---

## 

### A. 

- ****:  Python  + Shannon  + OWASP 
- ****:  + 
- ****: `cargo audit` 

### B. 

- `AGENTS.md` — unwrap/panic 
- `src/interpreter/mod.rs:137-205` — Interpreter 
- `src/interpreter/builtins.rs` — 5,014 
- `src/interpreter/dispatch.rs:1009-1010` — 0.0.0.0 
- `src/http_server.rs:468` — CORS 
