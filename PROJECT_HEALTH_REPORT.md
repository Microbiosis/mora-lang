# Mora-lang 

> 2026-07-08 17:26 CST
> 
> v0.51 (Cargo.toml: 0.0.53)

---

## 1. 

|  |  |  |
|--------|------|------|
| `cargo build --all-targets` |  PASS |  |
| `cargo test --all` (unit + integration) |  PASS | **639 tests pass** (606 lib + 27 integration + 6 bin)0 fail |
| `cargo test --all` (doc tests) |  SKIP | OS  (os error 448: ) |
| `cargo clippy --all-targets --all-features -- -D warnings` |  PASS |  clippy  |
| `cargo fmt --check` |  PASS |  ( `src/interpreter/mod.rs:775` `cargo fmt`) |

### 

|  |  |  |  |  |
|----------|------|------|------|------|
|  (lib) | 606 | 0 | 14 | 14  #[ignore]  Docker  |
|  (tests/) | 27 | 0 | 0 | orchestrate_v50_integration (27) + parser_v2_integration |
|  (bin) | 6 | 0 | 0 | mora + mora-lsp |
| **** | **639** | **0** | **14** | |

> 14  #[ignore] 10  (//)4  Docker  ( Docker daemon)

---

## 2. 

|  |  |
|------|------|
| Rust  | **83 ** `.rs`  |
| / | **19 **  |
|  | `src/interpreter/builtins.rs` (191.8 KB) |
|  | `src/interpreter/mod.rs` (99.9 KB) |
| Cargo  | **17 ** ( Rust) |
|  | 2  (`mora`, `mora-lsp`) |
| Feature flag | 1  (`checkpoint-sqlite`) |
|  | 23  `.mora` (7  + 16  _legacy) |

---

## 3.  (v0.41 → v0.51)

|  |  |  |  |  |
|------|------|------|------|------|
| v0.41.0 | 07-05 | Event Bus O(segments)  | +10 |  |
| v0.41.1 | 07-05 | Reading Order XY-Cut++ (MinerU) | +7 |  |
| v0.42.0 | 07-05 | Capability Token  (loongclaw) | +21 |  |
| v0.42.1 | 07-05 | Audit Sink SHA-256  | +20 |  |
| v0.43.0 | 07-05 | exec.parallel()  | +9 |  |
| v0.43.1 | 07-05 | memory.remember + bus.subscribe | +12 |  |
| v0.44.0 | 07-06 | sandbox.containerize Docker | +14 |  |
| v0.45.0 | 07-06 | ToolPlane + ai.retry + ai.role | +24 |  |
| v0.46.0 | 07-06 | SKILL.md + MoraSkillSpec  | +19 |  |
| v0.47.0 | 07-06 | DAG-as-data + heartbeat.md + context.trim | +34 |  |
| v0.48.0 | 07-06 | plan.update + mora.refine | +30 |  |
| v0.49.0 | 07-07 |  +  +  (15 fixes) | +21 |  |
| v0.50 | 07-08 | tokio + parking_lot + checkpoint  | - |  |
| **v0.51** | **07-08** | **P0  + checkpoint trait ** | - | ** ** |

> v0.41-v0.51  +2800 LOC+200+ 

---

## 4. 

### 

|  |  |  |
|------|--------|------|
| `lexer.rs` |  90% |  1i/1f  |
| `parser_v2/` |  95% | 3  (mod + statements + expressions) |
| `ast_v2.rs` |  90% | AST v2  |
| `typeck/` |  85% |  (mod + check + pregel_check) |
| `interpreter/` |  90% | 10  |
| `flow.rs` |  90% |  (if/while/for/match/pipe) |
| `value.rs` |  90% | Value  (Int/Float/Number ) |

### AI 

|  |  |  |
|------|--------|------|
| `interpreter/ai_chat.rs` |  85% | AI  +  |
| `interpreter/ai_helpers.rs` |  80% | AI  |
| `interpreter/orchestrate_v2.rs` |  80% | Agent  v2 |
| `ai_infra.rs` |  80% | AI  |
| `http_server.rs` |  85% | HTTP  |
| `mcp_server.rs` |  80% | MCP  |

### 

|  |  |  |
|------|--------|------|
| `lsp/` |  80% | LSP  (10 providers) |
| `record/` |  85% | / (7 ) |
| `trace_collector.rs` |  80% |  |

###  (v0.41+ )

|  |  |  |
|------|--------|------|
| `event/` |  90% | Event Bus (O(segments) ) |
| `sandbox/` |  85% |  + Capability + Docker  |
| `audit/` |  90% | SHA-256  |
| `checkpoint/` |  70% |  (Memory + SQLite) |
| `compress/` |  85% |  (5 ) |
| `document/` |  85% |  IR (6 ) |
| `schedule/` |  85% | Cron  |
| `ccr/` |  85% |  |
| `mock/` |  80% | Mock  |
| `plan/` |  85% | / (v0.48) |
| `refine/` |  85% |  (v0.48) |
| `skill/` |  85% | SKILL.md  (v0.46) |
| `toolplane/` |  85% | ToolPlane  (v0.45) |
| `orchestrate_dag/` |  85% | DAG-as-data  (v0.47) |
| `heartbeat/` |  85% |  (v0.47) |

---

## 5. 

### P0 — 

| # |  |  |  |
|---|------|------|------|
| **FMT-1** | ~~`cargo fmt --check` ~~ | `src/interpreter/mod.rs:775` |   ( `cargo fmt`) |

### P1 — 

| # |  |  |
|---|------|------|
| DOC-1 | Doc tests  Windows  | os error 448:  |
| |  |  |

### P2 — 

| # |  |  |
|---|------|------|
| SIZE-1 | `builtins.rs`  191.8 KB |  |
| SIZE-2 | `interpreter/mod.rs`  99.9 KB |  |
| VERSION-1 | Cargo.toml (0.0.53)  git tag (v0.51)  |  |

---

## 6. Git 

|  |  |
|------|------|
|  | `main` |
|  origin/main | **+119 commits** |
|  | `47d5cee` v0.51: Phase 1 - 4  v0.50 P0  |
|  |   () |

>  119 

---

## 7. 

|  |  |  |
|------|------|------|
|  |  |  |
|  |  | 639 tests0 fail14 stress/docker  |
|  |  | Clippy 1  |
|  |  |  + AI + checkpoint  |
|  |  | v0.34-v0.40  |
|  |  | 83 /19 2  |
|  |  | v0.41-v0.51  1-4  |

### 

** (A)** CI build  / test (639 pass)  / clippy  / fmt 19 

****
1. 🟡  119  origin/main
2. 🟡  doc test  Windows mount point 
3. 🟢  Cargo.toml  (0.0.53 → )
4. 🟢  `builtins.rs` (191.8 KB)  `interpreter/mod.rs` (99.9 KB) 
