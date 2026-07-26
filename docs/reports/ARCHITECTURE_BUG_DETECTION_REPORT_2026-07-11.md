# Mora-lang  Bug  v0.0.55

> ****2026-07-11  
> ****Cargo 0.0.55v0.55 baseline  
> ****94  .rs   
> ** LOC**36,874  tests/examples/  
> **2026-07-10** 46,712 → 36,874 LOC src 

---

## 

|  |  |  |
|------|------|------|
| `cargo build --all-targets` |   | 0.81s |
| `cargo test --all` |  863  / 0  / 15 ignored |  755 → 863+108  |
| `cargo clippy -D warnings` |   |  2 dead-code |
| `cargo fmt --check` |   |  2  |

**** clippy / fmt 

---

## 

### 2.1 unwrap / panic / expect 

|  |  |  |  |
|------|------|------------|------|
| `unwrap()` | 473 | ↑89 423 → 473 |  **** |
| `panic!` | 173 |  |   |
| `.expect()` | 328 |  |   |
| **** | **974** | — |   |

****unwrap 

|  | unwrap | panic | expect |  | unwrap/ |
|------|--------|-------|--------|------|-----------------|
| `interpreter/builtins.rs` | 85 | 100 | 139 | 5,098 | 16.7 |
| `checkpoint/mod.rs` | 39 | 0 | — | 723 | 54.0 |
| `record/tests.rs` | 39 | 0 | — | 607 | 64.4 |
| `checkpoint/sqlite.rs` | 32 | 0 | — | 307 | 104.0  |
| `audit/mod.rs` | 25 | 0 | — | 720 | 34.7 |
| `interpreter/orchestrate_v2.rs` | 24 | 6 | — | 1,435 | 16.7 |
| `refine/mod.rs` | 22 | 0 | — | 328 | 67.1 |
| `toolplane/mod.rs` | 20 | 0 | — | 312 | 64.5 |
| `plan/mod.rs` | 20 | 0 | — | 276 | 72.5 |
| `flow.rs` | 19 | 5 | — | 1,168 | 16.3 |

** **
- **`checkpoint/sqlite.rs`**104 unwrap/
- **`refine/mod.rs`**67.1 /
- **`plan/mod.rs`**72.5 /

**builtins.rs **85 unwrap + 100 panic + 139 expect = **324 ** 5,098  **6.4%** 15.7 

### 2.2 

|  | LOC |  |  |
|------|-----|----------|------|
| `interpreter/builtins.rs` | 5,098 |   | 196 match + 324  |
| `interpreter/mod.rs` | 3,336 |   | Interpreter + |
| `parser_v2/statements.rs` | 2,166 | 🟡  |  |
| `typeck/mod.rs` | 2,055 | 🟡  | + |
| `compress/json.rs` | 1,512 | 🟡  | JSON  |
| `interpreter/orchestrate_v2.rs` | 1,435 | 🟡  |  |
| `interpreter/dispatch.rs` | 1,417 | 🟡  |  |
| `interpreter/execute.rs` | 1,162 | 🟡  |  |
| `flow.rs` | 1,168 | 🟡  |  |
| `main.rs` | 1,043 | 🟡  | CLI  |
| `lexer.rs` | 1,246 | 🟡  |  |
| `value.rs` | 706 | 🟠  |  |
| `ai_infra.rs` | 783 | 🟡  | AI 65 dead_code  |

### 2.3 

|  |  |  |
|------|--------|------|
| `TokenType` | ~120+ | 🟡 match  |
| `Value` | 33 | 🟠  |
| `Type` | 30 | 🟠  |
| `ExprKind` | — |  |
| `StmtKind` | — |  |

### 2.4 pub 

|  |  |  |
|------|------------|------|
| `runtime/*` 7  facade | 34  `pub`  |  #1facade  pub  |
| `interpreter/mod.rs` | 8  `pub(crate)` +  `pub` |  |

**runtime facade ** `pub` `pub(crate)`
- `ai.rs`7  pub token_budget, token_usage, trace, draft_model_stats, context_window, speculative_verifier, cache_warmer
- `core.rs`7  pub globals, environment, tool_registry, v2_arena, current_ai_config, config_stack, worker_channels/receivers
- `infra.rs`6  pub recorder, string_interner, ai_cache, bus, scheduler,  1
- `persist.rs``registry.rs``sandbox.rs``orch.rs` 4-5  pub 

### 2.5 dead_code 

|  | 108 |
|------|-----|
| `#[allow(dead_code)]` | 101 |
| `#[allow(unused)]` | 1 |

****
- **`ai_infra.rs`**65  dead_code 783  8.3%
- `typeck/mod.rs`10
- `interpreter/mod.rs`9

> `ai_infra.rs`  dead_code ——65  dead_code 

---

## Bug 

### 3.1  ai_chat.rs 

|  |  | LOC |  |
|------|--------|-----|------|
| `interpreter/ai_chat.rs` | **0** | 865 |  AI |

****AI  mora AI-native 865 

### 3.2 🟡 dispatch.rs 

|  |  | LOC |  |
|------|--------|-----|----------|
| `interpreter/dispatch.rs` | 5 | 1,417 | 3.5/ |
| `interpreter/execute.rs` | 9 | 1,162 | 7.7/ |

dispatch 5  1,417 

### 3.3 🟡 unwrap  checkpoint/sqlite.rs 

`checkpoint/sqlite.rs`32 unwrap / 307  = **104/**SQLite  IO unwrap  panic

### 3.4 🟡 builtins.rs 100  panic!

`builtins.rs`  100  `panic!`  panic  panic  AGENTS.md " unwrap()/panic!"

### 3.5 🟠 Arc<Mutex> 

|  |  |
|------|------|
| `Arc<Mutex<_>>` | 45 |
| `Arc<RwLock<_>>` | 6 |

****
- `value.rs`18  Arc<Mutex>Value 
- `runtime/sandbox.rs`3
- `runtime/orch.rs`3
- `runtime/infra.rs`3

> mora  C1 async runtime `Arc<Mutex>` 45  Arc<Mutex>  `value.rs`  18 ——Value 

### 3.6 🟠 697  clone()

`clone()`  697  clone Value/AST  clone 

### 3.7 🟢  unsafe

 6  `unsafe`document/backend 3 + builtins.rs 2 + sandbox/container 1

---

## 

### 4.1 Interpreter → Runtime 


```
interpreter/mod.rs (3,336) ←  7  runtime facade holder
   runtime/core.rs    (7 pub , 106)
   runtime/ai.rs      (7 pub , 120)
   runtime/infra.rs   (6 pub , 126)
   runtime/persist.rs (4 pub , 95)
   runtime/registry.rs(4 pub , 119)
   runtime/sandbox.rs (4 pub , 134)
   runtime/orch.rs     (4 pub , 119)
```

****
1. runtime facade  `pub` `pub(crate)`
2. ****runtime  import interpreter  5 `RouteConfig`, `TokenBudget`, `TokenUsage`, `AiConfigValue`, `ToolDef`, `TraitInfo`, `TraitMethodSig`, `LruCache`
3. Interpreter mod.rs  3,336  +  + 138 

### 4.2 

```
runtime/* → interpreter/* (5  import)
   interpreter → runtime
```

 runtime  interpreter 
- `RouteConfig`, `TokenBudget`, `TokenUsage` → runtime/ai.rs
- `AiConfigValue`, `ToolDef` → runtime/core.rs
- `LruCache` → runtime/infra.rs
- `TraitInfo`, `TraitMethodSig` → runtime/registry.rs

**** shared kernelBC1 Language core `types.rs` interpreter ↔ runtime 

### 4.3 

|  |  |  |
|------|--------|----------|
| interpreter/mod.rs | 138 | Interpreter  |
| interpreter/builtins.rs | 101 |  |
| typeck/* | 67 + 22 |  |
| flow.rs | 38 |  |
| record/tests.rs | 27 |  |
| lexer.rs | 27 |  |
| checkpoint/mod.rs | 24 |  |
| parser_v2/* | 22 + 19 + 7 |  |

****
|  |  |  |
|------|--------|--------|
| `ai_chat.rs` | **0** |  AI |
| `dispatch.rs` | 5 | 🟡  |
| `execute.rs` | 9 | 🟡  |

### 4.4 

```
src/
   interpreter/   (10, 13,495) ← 
   parser_v2/     (3, 3,867)
   typeck/        (3, 4,222)
   checkpoint/    (3, 1,179)
   runtime/       (8, ~1,200)
   compress/      (5, 2,472)
   document/      ()
   lsp/           ()
   sandbox/       (3, 1,281)
   ...
```

****
- `interpreter/`  13,495 36.7%
- `builtins.rs`  5,098  interpreter  37.8%
- `runtime/`  7  facade  35  `pub`

---

## 2026-07-10

|  | 2026-07-10 | 2026-07-11 |  |
|------|------------|------------|------|
| Cargo  | 0.0.53 | 0.0.55 | ↑2 minor |
|  | 755 | 863 | +108  |
| unwrap  | 334 → 423 | 473 | ↑50  |
| clippy | 2 dead-code  | 0  |   |
| fmt | 2 diff  | 0  |   |
| ai_infra.rs dead_code |  | 65 |   |
| ai_chat.rs  |  | 0 |  |

****
-  clippy / fmt 
-   755  863+108

****
-  unwrap  423 → 473+50
-  panic!/expect  974
-  ai_infra.rs 65 dead_code ——

---

## 

### P0

1. ** ai_chat.rs **865  Bug 
2. ** ai_infra.rs  65 dead_code **

### P1

3. ** builtins.rs**5,098  + 324  5-8 
4. ** checkpoint/sqlite.rs  unwrap **104/ →  <10/
5. ** runtime  34  pub  pub(crate)**

### P2

6. ** runtime ↔ interpreter ** shared types  BC1
7. ** interpreter/mod.rs**3,336  →  +  + 
8. ** value.rs  18  Arc<Mutex>**

### P3Plateau A 

9. **unwrap **473 →  <50
10. **clone() **697  →  clone

---

## 

- `cargo build --all-targets`
- `cargo test --all`
- `cargo clippy --all-targets --all-features -- -D warnings`lint 
- `cargo fmt --check`
- `grep -rn "unwrap()" src/`
- `grep -rn "panic!" src/` + `grep -rn ".expect(" src/`
- `grep -rn "#\[test\]" src/`
- `grep -rn "pub " src/runtime/`
- `grep -rn "use crate::interpreter" src/runtime/`
- `grep -rn "#\[allow(dead_code" src/`
- `grep -rn "Arc<Mutex" src/`
- `wc -l src/**/*.rs`

---

* Makers *
