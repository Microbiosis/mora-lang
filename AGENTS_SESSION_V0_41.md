# Mora-lang v0.34-v0.41 

> ****:  2026-07-02~04  mora-lang .
> ****: v0.34  (d00a95c  merge). ****: v0.40  + 17  + .
>
> ****: " →  →  → ".
> : 57  → 43  P0/P1/P2  → 17  → 1 .
>
> ****: ~18,000 . : ~45 .

---

## 

1. [](#1-)
2. [ (v0.34) — ](#2--v034--)
3. [v0.35 —  P0  (20 )](#3-v035---p0-)
4. [v0.36 —  + ](#4-v036----)
5. [v0.37 — ](#5-v037--)
6. [v0.38 —  ( #2)](#6-v038--)
7. [v0.39-v0.40 — Env ](#7-v039-v040--env-)
8. [ ( 1-5 ) — ](#8---)
9. [17 ](#9-17-)
10. [ — mora-lang  Interpreter ](#10---mora-lang--interpreter-)
11. [v0.34  "" — ](#11-v034----)
12. [ — ](#12---)
13. [:  commit ](#13---commit-)
14. [: ](#14--)

---

## 1. 

### 1.1 

```
 1: 
    
      (///)
     4  Explore  fan-out
     57  (20 P0 + 24 P1 + 13 P2)
     : AUDIT_ZEROTRUST_V0_34.md

 2:  (5 )
    
     v0.35: 20 P0  (16 commit + 1 merge)
     v0.36: 12 P1 + 2 P2 + 2  (14 commit)
     v0.37: 7 P1 + 2 P2 (8 commit)
     v0.38:  #2 (9 commit)
     v0.39:  rename (2 commit)
     v0.40: Env  #5 (3 commit)

 3:  (5 )
    
      1 : loongclaw (1 )
      2 : mini-swe-agent + CLI-Anything (2 )
      3 : AIOS + mimiclaw + OpenFugu + OpenInfer + MinerU + Headroom + Puter (7 )
      4 : pi-agent + AgentMesh + revenue-orchestrator + ai-coder-symphony (4 )
      5 : vesh-agents + AgentMesh Go + Solace Agent Mesh (3 )
     : RESEARCH_PRIMITIVES_MASTER.md (579 )
```

### 1.2 

|  |  |
|---|---|
|  (P0/P1/P2) | 57 |
| P0  | 20 (v0.35) |
| P1  | 19 ( v0.36-v0.37) |
| P2  | 4 ( v0.36-v0.37) |
|  | 5/5 (v0.36: 2, v0.38: 1, v0.40: 1) |
| CI  | 1 (v0.36) |
|  | 46 |
|  | 6 (v0.35-v0.40) |
|  commit | ~72 ( 6 ) |
|  | ~3,000 LOC |
|  | ~60 |
|  | 17 |
|  | ~580  () + ~750  () |

### 1.3  Git  (v0.34 → v0.40)

```bash
# v0.40 (main HEAD)
215336d docs: RESEARCH_PRIMITIVES_MASTER.md update (17 projects)
625c712 docs: RESEARCH_PRIMITIVES_MASTER.md — initial (15 projects, 458 lines)
76d5a5b merge(v0.40): resolve Cargo.lock conflict
a979617 merge(v0.40): Env refactor merged to main
2cb2cd0 docs(v0.40): CHANGELOG + clippy fixes
c78e2ec feat(v0.40): Closure.env -> EnvRef immutable snapshot
69b1cd2 feat(v0.40): EnvRef + derive Clone on Environment

# v0.39-v0.38 (main history)
aab7e95 merge(v0.39): env deferred (rename only)
d15d0b3 docs(v0.39): CHANGELOG with v0.40 plan
5f71bb2 refactor(v0.39): rename with_parent -> with_parent_of
4b814a5 merge(v0.38): numeric tower partial
465f890 style(v0.38): clippy + fmt
ce2c198 merge(v0.38): CHANGELOG
bb5b658 test(v0.38): 13 numeric tower tests
7ff8236 feat(v0.38): strict promotion Int+Int=Int, Float+Float=Float
2b74f3d feat(v0.38): Type::Int/Float variants
62b6d17 feat(v0.38): lexer 1i/1u/1f suffix
4e77074 feat(v0.38): Literal::Int/Float
9ebc7b5 feat(v0.38): Value::Int/Float
fc75c60 chore(v0.38): bump version

# v0.37-v0.36-v0.35 ()
# ... ( 40+  commit)
```

---

## 2.  (v0.34) — 

### 2.1 

"": **, .**  "" () .

**** :

|  |  |  |
|---|---|---|
| **** |  | " bus.emit ?" |
| **** |  | " estimate_bytes ?" |
| **** | panic  | " ccr.put(data)  List ?" |
| **** |  | "REPL ?" |

### 2.2 

 **Explore ** ( fan-out):

```

   Agent 1:  (src/value.rs, src/lexer.rs, src/parser_v2/, ...)
   Agent 2:  (src/flow.rs, src/compress/, src/interpreter/ai_chat.rs, ...)
   Agent 3:  (src/typeck/, src/interpreter/evaluate.rs, ...)
   Agent 4:  (src/event/mod.rs, src/ccr/mod.rs, src/schedule/mod.rs, ...)
```

 Agent  (file:line , , ). : ** file:line **  **3 ** — .

Agent ,  ****  P0  (:  `Read` src/  ast_v2.rs:625-657  11  `.unwrap()`).

### 2.3 

** P0 ( 5)**:

| # |  |  |  |
|---|---|---|---|
| 1 | `Clone for Interpreter`  5  v0.34  (bus/sandbox/scheduler/ccr/mock) —  clone  id | `interpreter/mod.rs:230-270` |  HTTP/MCP worker  |
| 2 | `EventBus::emit` **** — Mutex  | `event/mod.rs:55-64` |  `bus.emit`  |
| 3 | 11× `.unwrap()`  `walk_expr`  —  NodeId  panic | `ast_v2.rs:625-657` |  |
| 4 | `Display::fmt`  `.expect()` — poisoned mutex  panic | `value.rs:218, 245` | REPL  poisoned mutex  |
| 5 | REPL  — `check_program`  REPL  | `interpreter/mod.rs:651-689` |  `let x: number = "hello"`  REPL  |

****:

|  | P0 | P1 | P2 |  |
|---|---|---|---|---|
|  | 5 | 5 | 2 | Clone , ,  |
|  | 6 | 5 | 4 | O(n²) , ,  |
|  | 5 | 8 | 2 | , , NaN  |
|  | 4 | 6 | 5 | , ,  |
| **** | **20** | **24** | **13** | — |

### 2.4 

** 1: Fan-out + **. 4  Agent ,  Read  P0 . Agent  "compress/json.rs:357  hash.len() == 8" —  UUID  (8-4-4-4-12), **** CCR . Agent .

** 2:  file:line > **. . " unwrap" ; "ast_v2.rs:625-657  11  `.unwrap()` " .

** 3: "" , **.  3  "" —  2  (crossbeam , ) .  (Env ) **** —  8 ,  19+ , . ,  ( Rc<RefCell>) .

---

## 3. v0.35 —  P0 

### 3.1 

**16  commit + 1  bump + 1  merge = 18  commit**. : 1  →  → Clone .

### 3.2 Commit 

| # | Commit |  |  |  |
|---|---|---|---|---|
| 1 | B3: `.unwrap()` → `.expect()` | no-panic residue |  |  1  |
| 2 | B2: Display infallible | no-panic residue |  |  value.rs |
| 3 | C3: Dict.get  V\|Nil |  |  | 1  typeck  |
| 4 | B4: lexer  | no-panic residue |  |  lexer.rs |
| 5 | C4: arity  |  | - | 2  dispatch  |
| 6 | C1: REPL  |  | - | 2  |
| 7 | C2+D2+D4:  | + |  | 120 LOC  |
| 8 | B1: walk_expr infallible | no-panic residue |  | 11  unwrap → pattern |
| 9 | D3:  _cache_key |  |  | -2  |
| 10 | D1: parse_json O(n²)→O(n) |  |  | ~20 LOC  |
| 11 | A2: EventBus  |  |  | 6 LOC  |
| 12 | A3: MockRegistry  |  |  | 6 LOC mock |
| 13 | A4: ccr  |  | - | hash  |
| 14 | A5: v2_arena  Arc  |  |  |  |
| 15 | A1: **Clone ** |  |  |  — 5  |
| 16 | CHANGELOG + merge | — | — |  |

**"?"** :  commit  ( commit ). A1 (Clone )  HTTP/MCP worker ;  14  commit .

### 3.3 

**B3 — 1 **:  `.unwrap()`  `interpreter/mod.rs:384`.  `.expect("globals mutex poisoned")`  4 .

**C2+D2 — **:  9  `#[allow(dead_code)]` ,  write-once-construct, .  `speculative_verifier` **** — `ai_chat.rs:359`  `.verify()`. . .  v0.38 .

**D1 — JSON  O(n²)**: `parse_json_list`  `&s[i..].trim_start()`.  10K  JSON ,  5 . : `skip_ws()` , , .

### 3.4 

 commit :
```bash
cargo build --all-targets          # 
cargo test --all                   # 335  ( 337)
cargo clippy --all-targets -- -D warnings  # 
cargo fmt --check                  # 
```

Merge , 5 :
```bash
cargo run --bin mora -- run examples/integration_v0_34.mora
cargo run --bin mora -- run examples/compact_demo.mora
cargo run --bin mora -- run examples/compress_demo.mora
cargo run --bin mora -- run examples/compress_smart_demo.mora
cargo run --bin mora -- run examples/mcp_server_demo.mora
```

---

## 4. v0.36 —  + 

### 4.1 

 24  P1 + 13  P2 + 3  "" . v0.36  **12  P1 + 2  P2 + 2 ** — .

### 4.2  "" 

** #1: `mpsc::Receiver`  Interpreter  Send.**  "",  `std::sync::mpsc::Receiver`  `!Send`. :  `crossbeam-channel::Receiver` ( `Send + Sync`).  30 LOC. `crossbeam-channel`  `crossbeam-utils` .

** #3: 16  Value  Type .**  "" . :  `Type`  8  (Agent, TraitObject, Compose, Partial, Atom, Macro, PromptSection, Document). ~120 LOC.  `match Type { ... }` .

### 4.3 

**Arc-wrap **: `trait_registry`, `impl_table`  `tool_registry`  `Arc<HashMap<...>>` . Clone  refcount bump.  `Arc::make_mut` ().  HTTP/MCP worker  ~50KB .

** NaN/Inf **: `Value::Number`  `Display`  panic .  `nan`/`inf`/`-inf`  IEEE PartialEq . 4  4 .

****:  `file.*`  `fs::*`  `sandbox.check_path` . ; .

---

## 5. v0.37 — 

### 5.1 

 P1 : 6  builtin  + 3  + 2  P2 .

### 5.2 

**Value::Builtin(String) → BuiltinKind **:  "30+ "  22 .  builtin  dispatch  `call_*_method`  — .

**12  builtin **: `bus.emit`, `bus.off`, `sandbox.check_*`, `schedule.add`, `ccr.put/get`, `mock.register/unregister/call`  `Value::String` .  `v.to_string()`  `Value::List {1,2,3}`  `[1, 2, 3]`.

** Span **:  ( `line:0, column:0` )  11  7 . 3  `check_call_expr` ,  NodeId .

**with-block **:  `with` : `model`, `temperature`, `max_tokens`, `system`, `mock_llm`, `compact_at`.  (,  `modle`)  `TypeError`.

---

## 6. v0.38 — 

### 6.1 

** #2: .**  `Value::Number(f64)`  "",  `Int`/`Float`  258 .

### 6.2 

 ** Rust **:

```
Int + Int = Int        ()
Float + Float = Float  ()
Int + Float =  (Rust )
Float + Int =  (Rust )
Number ( f64) +  → f64 (, )
```

7  commit:  →  →  →  →  → 13  → CHANGELOG.

### 6.3 

**Env  ( #1)**  v0.39  8  commit,  v0.39 ( v0.40 ).

---

## 7. v0.39-v0.40 — Env 

****. .

### 7.1 v0.39 — "" 

****:  C6  `EnvRef` (Local Rc<RefCell> / Owned Box<Environment>)  **19+ **  8 . : `Rc<RefCell<Environment>>`  `!Send`,  `Value`  ( `Interpreter`)  HTTP/MCP worker  `Arc<Mutex<Interpreter>>` .

****:
- 1  commit:  `Environment::with_parent` → `with_parent_of` ( v0.40 )
- 1  commit: CHANGELOG  v0.39 

****:  "" **100% **. , . .

### 7.2 v0.40 — 

****:  Rc<RefCell<>> ,  `EnvRef`  `Box<Environment>`  — , .

 Send  (Box<Environment>  Send),  `Arc<Mutex<Environment>>` : .

**3  commit **:
1.  value.rs  `EnvRef`  + Environment  derive Clone
2.  `Value::Closure.env`  `EnvRef` (),  3  + 1 
3. CHANGELOG + clippy 

****:  5  "" , 4  (crossbeam, Type , NaN guard, ).  5  (Env) :  Rc<RefCell<>> .

---

## 8.  — 

### 8.1 

 ** fan-out **:

```
 N :
   URL
      README/
      Explore Agent ( 2-3 ) 
     Agent : file:line  + 3  + 
     ,  mora-lang 
```

 Agent :
- **** ()
- ****  file:line 
- ****, 
- **** ( |  | mora-lang )

### 8.2  1 : loongclaw (loong)

****:  13-crate Rust ,  DAG.  L0-L9. :
1. `CapabilityToken` —  `allowed_capabilities: BTreeSet<Capability>` 
2. `PolicyEngine` trait — :  + `PolicyExtensionChain`
3. `AuditSink` — SHA-256  JSONL , 

** mora-lang **:  ROI :
- `sandbox.key { ... }` →  (~200 LOC)
- `audit.jsonl` →  +  (~200 LOC)
- `Fault`  →  String (~80 LOC)

### 8.3  2 : mini-swe-agent + CLI-Anything

**mini-swe-agent**: Python , 100 . :
- **Exception-as-flow**: `InterruptAgentFlow` ; 
- ****: `start_new_session=True` + `os.killpg` 

**CLI-Anything**: 44.7k . :
```
matrix_registry.json  → →→ (9 !)
registry.json         →  harness CLI
public_registry.json  →  CLI
```

### 8.4  3 : 7 

 mora-lang v0.32-0.34 . : **mora-lang  API , **.

|  | mora-lang  |  |  |
|---|---|---|---|
| `schedule` | 370 | AIOS + mimiclaw | mimiclaw  cron  12 ,  9 . AIOS  4 . |
| `event` | 110 | Puter | Puter  O(segments) . mora  O(patterns) . |
| `sandbox` | 209 | Puter + AIOS | Puter  iframe . mora +. `thread_local!` . |
| `ccr` | 165 | Headroom | Headroom  SHA-256 . mora  u64  — . |
| `mock` | 56 | OpenFugu | OpenFugu  per-domain . mora /. |
| `reading_order` | 113 | MinerU | MinerU  XY-cut + ML layoutreader. mora  center_y→center_x . |
| `compress` | ~1260 | Headroom | Headroom  5  + 11 . mora  JSON/. |

### 8.5  4 :  + 

**pi-agent / pi-mono** (, ):
-  (steering + follow-up) 
-  (`Promise.all`)
- `without("delegate")`  ( = 1)
- 
-  markdown (`.pi/memory.md`)
- 

**AgentMesh (MinimalFuture)**: , LLM . . :  WebSocket ,  pub-sub, +.

### 8.6  5 : vesh-agents + Pregel BSP

**vesh-agents**:  LLM  ( LLM ).  Stripe/Postgres/CSV .

**AgentMesh Go (hupe1980)**: **Pregel ** . : , , .  CoW : 10k+  GC .

---

## 9. 17 

### 9.1  ( 3+ )

|  |  | mora-lang  |
|---|---|---|
|  +  | 2 (loongclaw, AIOS) | `sandbox.key { ... }` |
|  +  | 2 (loongclaw, CLI-Anything) | `audit.jsonl` |
|  /  | 4 (loongclaw, CLI-Anything, mimiclaw, vesh-agents) | `mora-hub.json` |
|  / ToolKind  | 3 (CLI-Anything, mimiclaw, vesh-agents) | `ToolKind` enum |
|  +  | 2 (mini-swe-agent, pi-agent) | `exec(cmd, timeout)` |
|  | 2 (mini-swe-agent, pi-agent) | `FlowSignal`  |
|  (markdown) | 3 (pi-agent, AgentMesh, mimiclaw) | `memory.remember()` |
|  /  | 3 (revenue-orchestrator, AgentMesh, vesh-agents) | `context.outputs` |
|  ( LLM ) | 2 (vesh-agents, revenue-orchestrator) | `orchestrate` |

### 9.2  ( 1 )

|  |  |  |
|---|---|---|
| Pregel BSP  | AgentMesh Go | ,  |
|  CoW  | AgentMesh Go | 10k+  GC  |
| WASM  | AgentMesh Go, loongclaw |  WASM  |
| TRINITY 19.5K  | OpenFugu |  Transformer  |
|  | pi-mono |  |
|  LLM  | vesh-agents |  LLM  |
|  XY-cut | MinerU |  |
| SHA-256  | Headroom |  |

---

## 10.  — mora-lang  Interpreter 

### 10.1 v0.34  ()

```rust
pub struct Interpreter {
    // v0.34  ~25  (AI )
    globals: Arc<Mutex<Environment>>,
    environment: Arc<Mutex<Environment>>,
    // ... , , , ,  ...

    // v0.32-0.33:  5 **** Interpreter
    // bus: EventBus                  ← 
    // sandbox: SandboxPolicy         ← 
    // scheduler: Scheduler           ← 
    // ccr_store: InMemoryCcrStore    ← 
    // mock_registry: MockRegistry    ← 
}
```

: " = ." v0.32-0.33 ** 5 ** ( → 4  Self  →  → dispatch → builtins.rs).  "" — .

### 10.2 5  Builtin 

 v0.34 , :

```
 1: Interpreter struct  (Arc<Mutex<...>>)
 2: 4  Self {}  (new / new_empty / new_with_globals / Clone impl)  init
 3: globals.lock().unwrap().define("name", Value::Builtin("name"), false)
 4: dispatch.rs  ("module", method) => self.call_*_method(method, &args)
 5: builtins.rs  pub fn call_*_method(&self, method: &str, args: &[Value]) -> Result<Value, String>
```

 5 **** — , .  builtin .

### 10.3 v0.35-v0.40  Interpreter

```rust
pub struct Interpreter {
    //  (v0.40 )
    globals: Arc<Mutex<Environment>>,
    environment: Arc<Mutex<Environment>>,

    //  Arc<HashMap<>>  (v0.36)
    tool_registry: Arc<HashMap<String, ToolDef>>,
    trait_registry: Arc<HashMap<String, TraitInfo>>,
    impl_table: Arc<HashMap<String, Vec<String>>>,

    // v0.34 5  builtin ( Arc , v0.35)
    bus: EventBus,                    // Arc<Mutex<HashMap<...>>> 
    sandbox: SandboxPolicy,           // BTreeSet  (v0.36)
    scheduler: Scheduler,             // AtomicU64 next_id (v0.36)
    ccr_store: InMemoryCcrStore,      // 16-char hex  (v0.35)
    mock_registry: MockRegistry,      // call()  (v0.37)

    // Worker  (crossbeam-channel, v0.36)
    worker_channels: HashMap<String, crossbeam_channel::Sender<Value>>,

    //  (v0.35-v0.37): method_cache, ai_batch_queue,
    // cache_warm_queue, ai_priority_queue, adaptive_temp,
    // load_balancer, retry_policy, route_registry

    // v0.38 numeric tower: Value  Int(i64) + Float(f64)
    // v0.40 env refactor: Value::Closure.env  EnvRef ()
}
```

---

## 11. v0.34  "" — 

 5  " (v1.0 )." :

| # |  |  |  |  |
|---|---|---|---|---|
| 1 | `mpsc::Receiver`  Interpreter  Send |   | v0.36 | 30 LOC, crossbeam-channel |
| 2 | `Value::Number(f64)`  |   | v0.38 | ~400 LOC, 7  commit |
| 3 | 16  Value  Type  |   | v0.36 | 8  Type , 120 LOC |
| 4 | NaN/Inf  |   | v0.36 | 20 LOC, 4  |
| 5 | Env  |   () | v0.40 | 3  commit, EnvRef  |

**: 5/5 .**

 #5 :  ("mpsc::Receiver +  +  Type ") .  ""  **Env ** —  Rc<RefCell<>>  Interpreter  `!Send`. v0.40  `EnvRef = Box<Environment>` :  (),  ().  Rc<RefCell<>>  Interpreter  — " Send  + Send  harness "  —  v1.0 .

---

## 12.  — 

### 12.1 

1. **.**  "" — .  4  (, , , ) .
2. ** Agent.**  Agent  file:line , ,  (file:line + 3 ).
3. ** Agent .** Agent  (~15%  direct-read ).  P0.
4. **.** P0 (panic//) → P1 (, ) → P2 (). .

### 12.2 

1. ** commit.**  1 , .  commit .
2. ** commit  4 ** (build, test, clippy, fmt). .
3. ** commit  worktree .**  Env  8 ,  `git worktree`  main .
4. ** commit , .** Env  3  2 . ; .

### 12.3 

1. ** Explore Agent.**  Agent  (), , .
2. **.** : "mora-lang ? ? ?"
3. **.**  3+  (, , , ),  mora-lang .
4. **.**  — ,  mora-lang .
5. **.** .

### 12.4 

1. **.** `v0.35-technical-debt`, `v0.36-type-completeness` . Merge  `--no-ff` .
2. ** churn  worktree.** `.worktrees/v0.40-env`  Env ,  main.
3. ** revert.** v0.39  Env  revert . v0.40 . .
4. **.** . CHANGELOG, ,  — .

---

## 13. :  commit 

### v0.35 (: v0.35-technical-debt, 18  commit)

```
f8bf8bf  chore(v0.35): bump version 0.0.34 -> 0.0.35
ca00d03  fix(v0.35): .unwrap() -> .expect() on globals mutex (1 token)
e1b529f  fix(v0.35): Value::Router/Atom Display infallible (+2 tests)
1a7af23  fix(v0.35): typeck Dict.get returns V | Nil (1 line)
578c555  fix(v0.35): lexer rejects control chars in strings (\t\n\r stay)
480c764  fix(v0.35): call_task_inner / call_value_inner arity errors
08ee13b  fix(v0.35): REPL run_repl_with now type-checks (2 lines)
3a2f3ed  fix(v0.35): 8 dead fields removed + StmtKind::Route cleanup
293984c  fix(v0.35): walk_expr 11× .unwrap() -> pattern (11 sites)
97fe2ba  fix(v0.35): remove dead _cache_key format! (-2 lines)
884cc08  fix(v0.35): parse_json O(n²) -> O(n) via byte-index skip_ws
2e81ced  fix(v0.35): EventBus::emit clone-and-drop (no re-entrant deadlock)
9789e5a  fix(v0.35): MockRegistry::call clone-and-drop
f8f60ef  fix(v0.35): ccr hash 8 -> 16 hex (silent overwrite at 2^32 fixed)
9def32f  fix(v0.35): v2_arena wrapped in Arc<AstArena> (cheap clone)
5a0cf6e  fix(v0.35): Clone for Interpreter shares 5 v0.34 singletons via Arc
2ba55a3  docs(v0.35): CHANGELOG entry (20 P0 fixes)
9fc78c7  merge(v0.35): 20 P0s remediated, merge to main
8e9e6bb  style(v0.35): post-merge rustfmt
```

### v0.36 (: v0.36-type-completeness, 14  commit)

```
3908642  chore(v0.36): bump version 0.0.35 -> 0.0.36
22290a0  perf(v0.36): Arc-wrap trait_registry/impl_table/tool_registry
3862e48  feat(v0.36): swap std mpsc to crossbeam-channel (Permanent #1 DONE)
e150a64  fix(v0.36): Value::Number Display handles NaN/Inf (+4 tests)
601a615  fix(v0.36): List/Dict Display streaming + depth limit
6a05a1c  fix(v0.36): Scheduler AtomicU64 + SandboxPolicy BTreeSet
a38151d  fix(v0.36): MockRegistry::call deprecated
18f6265  fix(v0.36): file.* routes through sandbox.check_path
8a56c46  fix(v0.36): http_server routes listing lock-hold-across-IO
22f202d  fix(v0.36): typeck check_impl_def_stmt rejects orphan for_type
54b4347  feat(v0.36): Type enum adds 8 variants (Permanent #3 DONE)
ddcef92  fix(v0.36): estimate_bytes streams (no re-serialize)
bee19ad  ci(v0.36): fix integration job example paths (_legacy/)
5e2281b  merge(v0.36): CHANGELOG + fmt cleanup
1dae17a  merge(v0.36): merged to main
```

### v0.37 (: v0.37-final-cleanup, 8  commit)

```
315262a  chore(v0.37): bump version 0.0.36 -> 0.0.37
992329f  fix(v0.37): tighten 12 builtin boundaries (Value::String required)
b66b4de  fix(v0.37): delete MockRegistry::call entirely
18dcb88  feat(v0.37): Value::Builtin -> typed BuiltinKind enum (22 variants)
f966c43  fix(v0.37): http_server request handler lock hoist
933084c  fix(v0.37): typeck Load narrows to Type::String
9e17906  fix(v0.37): typeck errors carry real Span positions (7/11 sites)
473212d  fix(v0.37): typeck with-block validates key against whitelist
82fcbb8  merge(v0.37): CHANGELOG + fmt cleanup
f8305b2  merge(v0.37): merged to main
```

### v0.38 (: v0.38-numeric-env, 9  commit)

```
fc75c60  chore(v0.38): bump version 0.0.37 -> 0.0.38
9ebc7b5  feat(v0.38): Value::Int(i64) + Value::Float(f64) (numeric tower p1)
4e77074  feat(v0.38): Literal::Int/Float (numeric tower p2)
62b6d17  feat(v0.38): lexer 1i/1u/1f suffix (numeric tower p3)
2b74f3d  feat(v0.38): Type::Int/Float (numeric tower p4)
7ff8236  feat(v0.38): strict promotion Int+Int=Int, Float+Float=Float, mix Err
bb5b658  test(v0.38): 13 numeric tower tests (350 total, +13)
ce2c198  merge(v0.38): CHANGELOG
465f890  style(v0.38): clippy + fmt cleanup
4b814a5  merge(v0.38): merged to main
```

### v0.39 (: v0.39-env-refactor, 2  commit)

```
5f71bb2  refactor(v0.39): rename with_parent -> with_parent_of (name freed)
d15d0b3  docs(v0.39): CHANGELOG for Env-refactor-deferred release
aab7e95  merge(v0.39): merged to main
```

### v0.40 (: v0.40-env-refactor [worktree], 3  commit)

```
900a8db  chore(v0.40): bump version 0.0.39 -> 0.0.40
69b1cd2  feat(v0.40): EnvRef + derive Clone on Environment
c78e2ec  feat(v0.40): Value::Closure.env -> EnvRef (immutable snapshot)
2cb2cd0  docs(v0.40): CHANGELOG + fix clippy warnings
a979617  merge(v0.40): merged to main (with Cargo.lock conflict fix)
76d5a5b  merge(v0.40): resolve Cargo.lock conflict
```

###  (main , 2  commit)

```
625c712  docs: RESEARCH_PRIMITIVES_MASTER.md (15 projects, 458 lines)
215336d  docs: +vesh-agents + AgentMesh Go + Solace Agent Mesh (17 projects, 579 lines)
```

---

## 14. : 

|  |  |  | ? |  |
|---|---|---|---|---|
| **loongclaw** | Rust | 644 |  | , ,  |
| **mini-swe-agent** | Python | 5.6k |  | , , sentinel |
| **CLI-Anything** | Python/MDX | 44.7k |  | , HARNESS.md, SKILL.md |
| **AIOS** | Python | — |  | FIFO/RR ,  |
| **mimiclaw** | C (ESP32) | — |  | 12  cron, ,  vs  |
| **OpenFugu** | Python | — |  | TRINITY 19.5K , DAG-as-data |
| **OpenInfer** | Rust/CUDA | — |  |  vLLM,  KV  |
| **MinerU** | Python | — |  |  XY-cut, 30+ BlockType |
| **Headroom** | Rust/Python | — |  | SHA-256 , 5  |
| **Puter** | TypeScript | — |  | O(segments) , 5  DI |
| **pi-agent/pi-mono** | TS/Python | — |  | , ,  |
| **AgentMesh (MinimalFuture)** | Python | 294 |  | ,  |
| **revenue-orchestrator** | — | — |  ( README) | , ,  |
| **ai-coder-symphony** | — | — |  ( README) | ,  |
| **vesh-agents** | Python | — |  (PyPI) |  LLM ,  |
| **AgentMesh Go (hupe1980)** | Go | 6 |  | Pregel BSP,  CoW, WASM |
| **Solace Agent Mesh** | — | — |  | ,  |

---

****: ——→→→—— 3 , 6 , 17  72  commit.  "";  "→→→."  57 .  0.  17  mora-lang . .

****: "" , .  5  "" —  5 . —— Env  5 ,  2 . ; , , .
