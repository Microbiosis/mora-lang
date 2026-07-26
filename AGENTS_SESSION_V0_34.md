# Mora v0.34  — 

> ****:  2026-07-02~03  mora-lang .
> : v0.33  main. : v0.34  5  v0.30-0.33  module  builtin, merge  main.
>
> ****: v0.30-0.33  5  module (`event`/`sandbox`/`schedule`/`ccr`/`mock`) ** Interpreter struct** —  `bus.emit()` / `sandbox.run()` / `schedule.add()` / `ccr.put()` / `mock.register()`. "". v0.34 .

---

## 

1. [](#1-)
2. [v0.32  + 7  deep-dive](#2-v032---7--deep-dive)
3. [v0.33  ()](#3-v033--)
4. [v0.34  ()](#4-v034--)
5. [: Mora  Interpreter struct ](#5--mora--interpreter-struct-)
6. [:  → builtin  5 ](#6----builtin--5-)
7. [8  commit ](#7-8--commit-)
8. [ ()](#8--)
9. [Demo  + ](#9-demo---)
10. [v0.35 ](#10-v035-)
11. [: ](#11--)

---

## 1. 

```bash
$ git log --oneline -3
f50fb74 merge(v0.33): 4 P1 primitives from 7-project deep-dive
6fc2a03 chore(v0.30): cleanup, format, AGENTS.md spec, lex no-panic
a43f981 feat(v0.30): SmartCrusher — content-aware JSON compression
```

**Mora **:
- v0.30 SmartCrusher (1260  compress/json.rs) 
- v0.31 no-panic refactor (21 panic → 0) 
- v0.32 3  (recursive walker + event bus + mock registry) 
- v0.33 4  P1  (schedule + sandbox + reading_order + ccr) 
- 320 lib tests + 5 integration = 325 passed
-  origin 22  commit

** (v2 PRIMITIVES )**:
```
v0.32  5  module (event/sandbox/schedule/ccr/mock)
0  — 
```

---

## 2. v0.32  + 7  deep-dive

### 2.1 : deep-dive 9  AI 

 deep-dive  7  (`AGENTS_PRIMITIVES.md`, 581 ) + 2  (`AGENTS_PRIMITIVES_v2.md`, 759 ):

|  |  |
|---|---|
| AIOS |  FIFO/RR + Tool Manager hashmap  + Context snapshot (text/logits) |
| MimiClaw | ReAct agent loop + cron (9  job) + heartbeat + tool/skill  + path `..`  |
| OpenFugu | Policy-over-models (19K router) + per-turn role (Worker/Thinker/Verifier) + DAG-as-data + sep-CMA-ES |
| OpenInfer | Stitch-together  ( vLLM frontend) + feature-gated kernels + KV  + OpenAI  |
| MinerU | Group-based layout (fig-caption ) + 3 reading order (XY-cut/gap-tree/group) + multimodal specialist |
| Headroom | ContentRouter + SmartCrusher statistical detection + CCR (Compress-Cache-Retrieve) + DocumentCompactor recursive walker + CcrStore trait + 12-char hex hash + `<<ccr:HASH,SIZE>>` marker |
| Puter | 5  DI  + EventClient wildcard + Service Extension + IFC sandbox + Token compression |
| **mini-swe-agent** | exceptions-as-flow + 3-mode  (human/confirm/yolo) + abort_exceptions  + COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT sentinel + `os.killpg`  + TTL cache + JWT compression |
| **CLI-Anything** |  registry merge + 3  cache fallback + `_find_repo_root` git + parent walk + HARNESS_PREFIX  + KIND_LABELS  + `_format_requires`  requires + 4  source chain (checkout/bundled/published/stub) |

### 2.2 v1 + v2 

**v1 (7 AI , 21 )**:
- : `react` / `plan` / `document.grouped_layout` 

**v2 (2 AI , 14 )**:
- : `interrupt` / `limits` / `sandbox.run(3-mode)` / `registry cache` / `abort_exceptions` / `path validation` / `process group kill` 

**v1 + v2  ()**:

|  | v1 (7 AI ) | v2 (2 AI ) |
|---|---|---|
|  | **** module/builtin | **** |
|  | `react` / `plan` / `document.grouped_layout` | `interrupt` / `limits` / `sandbox.run(3-mode)` |
|  | 21  | 14  |

** P0**: ,  ** 5  v0.30-0.33 module  Interpreter** (, ).

### 2.3 v0.32-0.33  ()

|  |  commit |  |
|---|---|---|
| v0.30 | SmartCrusher  (compress/json.rs 1260 ) | +1370 -303 |
| v0.31 | no-panic refactor (lexer/parser 21 panic → 0) | +59 -3 |
| v0.32 | recursive walker (Headroom) + event bus (Puter) + mock registry (OpenFugu) | +862 -1 |
| v0.33 | schedule (MimiClaw) + sandbox (MimiClaw/AIOS) + reading_order (MinerU) + ccr (Headroom) | +1381 -2 |

---

## 3. v0.33  ()

 main  v0.33 merge commit:

```bash
$ git show f50fb74 --stat | head -10
merge(v0.33): 4 P1 primitives from 7-project deep-dive
 9 files changed, 1381 insertions(+), 2 deletions(-)
 create mode 100644 src/sandbox/mod.rs        ← 
 create mode 100644 src/event/mod.rs          ← 
 ...
```

**v0.33  4  module**:
- `src/sandbox/mod.rs` (209 ): `SandboxPolicy { allow, deny, fs_root }` + `check_builtin` / `check_path`
- `src/event/mod.rs` (110 ): `EventBus` (Arc<Mutex<HashMap<Pattern, Vec<Handler>>>) + `matches` wildcard
- `src/schedule/mod.rs` (370 ): `Scheduler` + `Job` + `JobKind` + `add` / `list` / `remove` / `tick`
- `src/ccr/mod.rs` (165 ): `CcrStore` trait + `InMemoryCcrStore` + `make_marker` / `extract_hash`

**0 **: v0.33 merge  4  module ****, . Interpreter struct ****.

---

## 4. v0.34  ()

### 4.1 

```
""
```

→ : 5  v0.30-0.33 module 0 .

### 4.2  git log (8 commits + 1 merge)

```bash
$ git log --oneline main (v0.34 end state)
d00a95c merge(v0.34): integrate 5 v0.30-0.33 orphaned modules as builtins
8d50a78 docs(v0.34): CHANGELOG entry + integration demo
92355d8 Revert "feat(v0.34): ai.tokens builtin..."   ←  commit revert
374570e feat(v0.34): ai.tokens builtin...              ← revert  (deferred to v0.35)
65eea4b feat(v0.34): mock builtin (integrate mock::MockRegistry)
5066356 feat(v0.34): ccr builtin (integrate ccr::CcrStore)
c712d0f feat(v0.34): schedule builtin (integrate schedule::Scheduler)
494d073 chore(v0.34): .gitignore tmp research artifacts (cross-session leftovers)
dba1c9d feat(v0.34): sandbox builtin (integrate sandbox::SandboxPolicy)
32b1dc0 feat(v0.34): bus.emit/off/count builtin (integrate event module)
60fdd75 chore(v0.34): bump version 0.0.33 -> 0.0.34
```

### 4.3 

```
[T0]  v0.33  main
[T1]  git checkout -b v0.34-integrate       ← ""
[T2]  sed version 0.0.33 -> 0.0.34         ← Cargo.toml
[T3]  commit: chore(v0.34): bump version    (60fdd75)

[T4]  commit 1: bus builtin integration    (32b1dc0)
        - field: bus: EventBus
        - 4 init blocks: bus: EventBus::new()
        - globals: define "bus" -> Value::Builtin
        - call_event_method: emit/off/count
        - dispatch: ("bus", m) => call_event_method
        - 4 tests in bus_tests mod

[T5]  commit 2: sandbox builtin             (dba1c9d)
        - field: sandbox: SandboxPolicy
        - register: define "sandbox"
        - call_sandbox_method: check_builtin/check_path/allow/deny/mode
        - 1 test

[T6]  commit: .gitignore cleanup            (494d073)
        - /openinfer_source_analysis.md
        - /mini-swe-agent/ /cli-anything/ /openinfer/
        ( deep-dive  git clone ,  working tree)

[T7]  commit 3: schedule builtin           (c712d0f)
[T8]  commit 4: ccr builtin                (5066356)
[T9]  commit 5: mock builtin               (65eea4b)

[T10] commit (failed): ai.tokens builtin    (374570e)
        -  duplicate test fn 
        - revert: 92355d8
        - ,  v0.35

[T11] commit 6: CHANGELOG + demo            (8d50a78)
        - CHANGELOG.md: 5 builtin  + roadmap
        - examples/integration_v0_34.mora:  demo

[T12] git checkout main
[T13] git merge --no-ff v0.34-integrate     (d00a95c)
[T14] git branch -d v0.34-integrate
```

---

## 5. : Mora  Interpreter struct 

### 5.1 v0.30 (SmartCrusher): Interpreter **** compress

```rust
pub struct Interpreter {
    globals: Arc<Mutex<Environment>>,
    environment: Arc<Mutex<Environment>>,
    tool_registry: HashMap<String, ToolDef>,
    model_routes: HashMap<String, RouteConfig>,
    token_budget: Option<TokenBudget>,
    token_usage: TokenUsage,
    pub trace: TraceCollector,
    route_registry: HashMap<String, String>,
    current_ai_config: Option<AiConfigValue>,
    pub trait_registry: HashMap<String, TraitInfo>,
    pub impl_table: HashMap<String, Vec<String>>,
    pub recorder: Recorder,
    worker_channels: HashMap<String, mpsc::Sender<Value>>,
    worker_receivers: HashMap<String, mpsc::Receiver<Value>>,
    ai_cache: HashMap<String, String>,
    string_interner: HashMap<String, Value>,
    method_cache: HashMap<String, usize>,
    ai_batch_queue: Vec<(String, Vec<(String, String)>)>,
    draft_model_stats: HashMap<String, (usize, usize)>,
    cache_warm_queue: Vec<String>,
    ai_priority_queue: Vec<AiPriorityEntry>,
    adaptive_temp: AdaptiveTemperature,
    context_window: ContextWindow,
    load_balancer: LoadBalancer,
    speculative_verifier: SpeculativeVerifier,
    cache_warmer: CacheWarmer,
    retry_policy: RetryPolicy,
    v2_arena: Option<AstArena>,
    memory_store: HashMap<String, Value>,
    // v0.32: event::EventBus    ← 0 
    // v0.33: sandbox::SandboxPolicy  ← 0 
    // v0.33: schedule::Scheduler    ← 0 
    // v0.33: ccr::InMemoryCcrStore  ← 0 
    // v0.32: mock::MockRegistry     ← 0 
}
```

### 5.2 v0.34 (): Interpreter ** 5 **

```rust
pub struct Interpreter {
    // ... v0.30  ...
    memory_store: HashMap<String, Value>,
    // v0.32: 
    bus: crate::event::EventBus,                    ← 
    // v0.33: 
    sandbox: crate::sandbox::SandboxPolicy,          ← 
    // v0.33: 
    scheduler: crate::schedule::Scheduler,           ← 
    // v0.33: Compress-Cache-Retrieve
    ccr_store: crate::ccr::InMemoryCcrStore,        ← 
    // v0.32: mock registry
    mock_registry: crate::mock::MockRegistry,       ← 
}
```

****:  module  `Arc<Mutex<...>>` , `Clone` .  **** .

### 5.3 Interpreter  4  Self {} 

v0.34 patch  ** 4  `Self {}` **  init:

|  |  (v0.34) |  |
|---|---|---|
| `Interpreter::new()` | ~388 |  interpreter +  globals builtin |
| `Interpreter::new_empty()` | ~440 |  builtin,  globals |
| `Interpreter::new_with_globals()` | ~478 |  globals |
| `Clone for Interpreter` | ~220 |  clone, channel  clone |

 `bus: EventBus::new()` + `sandbox: SandboxPolicy::permissive()` + `scheduler: Scheduler::new()` + `ccr_store: InMemoryCcrStore::new()` + `mock_registry: MockRegistry::new()` — **5  init × 4  = 20  init **.

### 5.4 globals builtin 

```rust
// Interpreter::new() :
globals.lock().unwrap().define("bus".to_string(), Value::Builtin("bus".to_string()), false);
globals.lock().unwrap().define("sandbox".to_string(), Value::Builtin("sandbox".to_string()), false);
globals.lock().unwrap().define("schedule".to_string(), Value::Builtin("schedule".to_string()), false);
globals.lock().unwrap().define("ccr".to_string(), Value::Builtin("ccr".to_string()), false);
globals.lock().unwrap().define("mock".to_string(), Value::Builtin("mock".to_string()), false);
```

---

## 6. :  → builtin  5 

 v0.30-0.33 module  v0.34 builtin **** 5 :

|  |  |  |
|---|---|---|
| 1 |  `Interpreter` struct  | `src/interpreter/mod.rs` |
| 2 |  4  `Self {}`  init | `src/interpreter/mod.rs` |
| 3 |  `Interpreter::new()`  `globals` builtin | `src/interpreter/mod.rs` |
| 4 |  `dispatch.rs`  module method dispatch  | `src/interpreter/dispatch.rs` |
| 5 |  `builtins.rs`  `call_*_method`  | `src/interpreter/builtins.rs` |

### 6.1 dispatch.rs  pattern

```rust
// dispatch.rs  (line 753+)
("file", method) => self.call_file_method(method, &args),       // v0.25
("memory", method) => self.call_memory_method(method, &args),   // v0.25
// v0.34  5 :
("bus", method) => self.call_event_method(method, &args),        // bus.emit/off/count
("sandbox", method) => self.call_sandbox_method(method, &args),  // sandbox.check_*/allow/deny/mode
("schedule", method) => self.call_schedule_method(method, &args),// schedule.add/list/remove/tick/count
("ccr", method) => self.call_ccr_method(method, &args),          // ccr.put/get/marker/extract/len
("mock", method) => self.call_mock_method(method, &args),        // mock.register/unregister/count/names
("document", method) => ...
```

### 6.2 builtins.rs call_*_method 

```rust
/// v0.34: bus.* —  (Puter EventClient  wildcard)
pub fn call_event_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
    match method {
        "emit" => {
            let event = args.first().map(|v| v.to_string())
                .ok_or("bus.emit: requires event name as first arg")?;
            let payload = args.get(1).cloned().unwrap_or(Value::Nil);
            self.bus.emit(&event, &payload);
            Ok(Value::Nil)
        }
        "off" => { /* ... */ }
        "count" => Ok(Value::Number(self.bus.pattern_count() as f64)),
        _ => Err(format!("bus.{}: unknown method", method)),
    }
}
```

****:  `call_*_method`  `match method { ... }`  match,  `Result<Value, String>`,  method  `Err("X.Y: unknown method")`.

### 6.3 builtin call 

```
User script:  bus.emit("test.event", "hello")
              ↓
Parser:  bus  Value::Builtin ( globals)
              ↓
evaluate.rs: bus.emit(args) → call_event_method("emit", args)
              ↓
builtins.rs: call_event_method match "emit" → self.bus.emit(&event, &payload)
              ↓
event::EventBus::emit:  pattern  handler
              ↓
Value::Nil
```

---

## 7. 8  commit 

### Commit 1: `chore(v0.34): bump version` (60fdd75)
- ****: `Cargo.toml` `version = "0.0.33"` → `"0.0.34"`
- ****: v0.34 ,  bump

### Commit 2: `feat(v0.34): bus.emit/off/count builtin` (32b1dc0)
- **3 files changed, 862 insertions, 1 deletion**
- ****:
  - `src/interpreter/builtins.rs`: `call_event_method` 35 
  - `src/interpreter/dispatch.rs`: +1 
  - `src/interpreter/mod.rs`: field + 4 init + 1 register + 4 tests
  - **`AGENTS_PRIMITIVES_v2.md`**: 759  ( deep-dive ,  `git add -A` )
- **bug **: fmt  (heredoc-induced whitespace)
- **tests**: 4 new (`test_bus_emit_and_count` / `test_bus_off` / `test_bus_emit_missing_arg` / `test_bus_unknown_method`)

### Commit 3: `chore(v0.34): .gitignore tmp research artifacts` (494d073)
- ****:  `git add -A`  `/tmp`  git clone  mini-swe-agent / cli-anything / openinfer  index
- ****: `.gitignore`  4 :
  ```
  /openinfer_source_analysis.md
  /mini-swe-agent/
  /cli-anything/
  /openinfer/
  ```
- ****:  `git add -A`  `git status`  untracked

### Commit 4: `feat(v0.34): sandbox builtin` (dba1c9d)
- **3 files, 58 insertions**
- **call_sandbox_method 5  method**:
  - `check_builtin(name) -> bool`
  - `check_path(path) -> bool` (MimiClaw `..` )
  - `allow(pattern)` / `deny(pattern)` (mutate policy)
  - `mode() -> "strict" | "permissive"` (heuristic: empty allow)
- **test**: 1 (`test_sandbox_builtin_basic`)

### Commit 5: `feat(v0.34): schedule builtin` (c712d0f)
- **3 files, 106 insertions**
- **call_schedule_method 5 method**:
  - `add(name, kind, message, interval_s?, at_epoch?) -> id`
  - `list() -> [Job dict, ...]`
  - `remove(id) -> bool`
  - `tick() -> [triggered, ...]`
  - `count() -> n`
- ****: `add`  kind  ("every" | "at"),  Err
- **test**: 1 (`test_schedule_builtin_basic`)

### Commit 6: `feat(v0.34): ccr builtin` (5066356)
- **3 files, 72 insertions**
- **call_ccr_method 5 method**:
  - `put(data) -> hash` (8-char hex)
  - `get(hash) -> data` ( Nil)
  - `marker(hash, size) -> "<<ccr:hash,size>>"`
  - `extract(marker) -> hash` (parse marker)
  - `len() -> n`
- ****: `use crate::ccr::CcrStore;` (trait import, builtins.rs)
- **test**: 1 (`test_ccr_builtin_basic`)

### Commit 7: `feat(v0.34): mock builtin` (65eea4b)
- **3 files, 56 insertions, 5 deletions**
- **call_mock_method 4 method**:
  - `register(name)` / `unregister(name)` (stub,  handler  v0.35)
  - `count()` / `names()`
- ****: `mock.register`  handler (closure )
- **test**: 1 (`test_mock_builtin_basic`)

### Commit 8: `docs(v0.34): CHANGELOG entry + integration demo` (8d50a78)
- **2 files, 138 insertions**
- **`CHANGELOG.md`**:  v0.34  (~140 )
- **`examples/integration_v0_34.mora`**:  demo (33 , 5  builtin )

### Bonus:  commit + revert

**`feat(v0.34): ai.tokens builtin` (374570e)** — :
-  `ai.tokens` builtin (v2 mini-swe-agent cost tracking )
- ** 1**: `TokenUsage` struct  `n_calls`  (E0609)
- ****:  `token_usage.input` proxy
- ** 2**:  mock patch  `test_ai_tokens_builtin` (E0428 duplicate)
- ****: sed  3 
- ** 3**: sed ,  3  orphan 
- ****: `git revert HEAD` (92355d8) —  ai.tokens commit 
- ****: v0.35  (TokenUsage  `n_calls` , )

---

## 8.  ()

### 8.1  sed  brace mismatch (commit 2)

****: `sed -i`  bus builtin ** sed field**,  dispatch  `self.bus` .

****: `git checkout -- src/interpreter/mod.rs`  revert, ****.

****: sed ,  `grep`  context,  `grep` .

### 8.2  git add  (commit 3)

****:  deep-dive 9  AI  `git clone`  `/tmp`,  `cd /tmp`  mora  `/tmp/msa``/tmp/cli-anything``/tmp/openinfer`  mora . `git add -A` .

****: `.gitignore`  4  (`/openinfer_source_analysis.md` + 3  clone ).

****: `/tmp` .  mora repo ****  `git clone`.

### 8.3  anchor  patch 

****:  patch  `old = "..."`  `// v0.34:  sandbox  builtin` ,  fmt  `old` , `assert old in content` .

****:  `fgrep` , ****, ****.

****:  anchor ****, "".

### 8.4 duplicate test function  (commit 6 )

****:  `python mock.py`  `test_ai_tokens_builtin` ****,  commit 6 patch  fn → E0428 duplicate.

****: `git revert HEAD` +  ai.tokens.

****:  patch  half state,  patch . **git revert ** debug.

### 8.5  `\\n`  (commit 2)

****: patch  `\\n`  `\n`,  Python f-string  `\\n`  literal `\n` .

****:  patch,  `\\n`  `\n` ,  Python .

---

## 9. Demo  + 

### 9.1 `examples/integration_v0_34.mora` 

```bash
$ cargo build --bin mora
$ ./target/debug/mora.exe run examples/integration_v0_34.mora
bus patterns: 0                              ← bus.emit  0 handler ( on)
sandbox builtin_ok: true                    ← sandbox.check_builtin("ai.chat")
sandbox path_safe: true                     ← sandbox.check_path("ok.txt")
sandbox path_unsafe: false                  ← sandbox.check_path("../escape.txt") 
schedule job_id: 00000001                  ← schedule.add("demo", "every", "tick", 60)
schedule job_count: 1                      ← schedule.count()
ccr hash: 00000001                          ← ccr.put("hello from v0.34")
ccr restored: hello from v0.34              ← ccr.get(hash) !
mock patterns: 0                           ← mock.register  stub

v0.34 integration: 5 modules / 5 builtins / 8 new tests
```

****: **CCR `put → get` ** — v0.33  Headroom CCR  Mora .

### 9.2 5  demos 

```
OK   compact_demo.mora
OK   compress_demo.mora
OK   compress_smart_demo.mora
OK   mcp_server_demo.mora
OK   integration_v0_34.mora   ← v0.34 
```

### 9.3  Git  (main HEAD)

```
d00a95c merge(v0.34): integrate 5 v0.30-0.33 orphaned modules as builtins
8d50a78 docs(v0.34): CHANGELOG entry + integration demo
92355d8 Revert "feat(v0.34): ai.tokens builtin (mini-swe-agent cost tracking)"
374570e feat(v0.34): ai.tokens builtin (mini-swe-agent cost tracking)     ← 
65eea4b feat(v0.34): mock builtin (integrate mock::MockRegistry)
5066356 feat(v0.34): ccr builtin (integrate ccr::CcrStore)
c712d0f feat(v0.34): schedule builtin (integrate schedule::Scheduler)
494d073 chore(v0.34): .gitignore tmp research artifacts (cross-session leftovers)
dba1c9d feat(v0.34): sandbox builtin (integrate sandbox::SandboxPolicy)
32b1dc0 feat(v0.34): bus.emit/off/count builtin (integrate event module)
60fdd75 chore(v0.34): bump version 0.0.33 -> 0.0.34
```

### 9.4  (v0.34 main)

```
build:        clean
test:         328 + 5 = 333 passed, 0 failed
clippy:       clean
fmt:          0 diff
doc:          0 warning
5 demos:      5/5 pass
```

---

## 10. v0.35 

|  |  |  |  |
|---|---|---|---|
| `bus.on(pattern, handler)` closure  | mini-swe-agent exception-as-flow |  |  interpreter-level handler  ( Rust closure ) |
| `mock.register(name)`  handler  | v2 P2 |  |  |
| `ai.limits { step, cost, wall_time }` block | mini-swe-agent AgentConfig |  interpreter |  `TokenUsage.n_calls` ,  `ai_retry` |
| `shell.run` with `os.killpg` | mini-swe-agent local.py |  | POSIX only (`create_new_session=True`) |
| `sandbox.run(script, {mode: "human"|"confirm"|"yolo"})` | mini-swe-agent 3-mode |  |  user interaction  |
| `COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT` sentinel | mini-swe-agent local.py:48 |  | mcp tool  |

---

## 11. : 

### 11.1 v0.30-0.33  5  module 0 

****: Mora  builtin ****:
1. Interpreter struct 
2. 4  Self {}  init
3. globals  Value::Builtin
4. dispatch.rs module method 
5. builtins.rs call_*_method 

**5 ** builtin . v0.32-0.33 ** step 0 ( module )**,  1-4.  **** — " = " ,  builtin "".

### 11.2  builtin  5  ()

```
 module  checklist:
 [ ] Step 1: Interpreter struct  (Arc<Mutex<...>>)
 [ ] Step 2: 4  Self {}  (new / new_empty / new_with_globals / Clone impl)  init
 [ ] Step 3: globals.lock().unwrap().define("name", Value::Builtin("name"), false)
 [ ] Step 4: dispatch.rs module  ("module", method) => self.call_*_method(method, &args)
 [ ] Step 5: builtins.rs pub fn call_*_method(&self, method: &str, args: &[Value]) -> Result<Value, String>
 [ ] tests: 1+  builtin e2e test (script  builtin)
 [ ] CHANGELOG entry
```

**v0.30-0.33  0  commit  checklist** —  5 .

### 11.3  + revert + 

 6  `git checkout` + 1  `git revert` +  `git commit --amend`:
- **v0.34 **: `git checkout -b v0.34-integrate` → 8 commits → `git merge --no-ff` → `git branch -d`
- **8  commit  revert** ( 1  revert  ai.tokens)
- ** commit ** (cargo test / clippy / fmt / doc  green)

### 11.4  ( Mora )

1. **" = builtin "** — 
2. **`/tmp` ** — clone  in-tree
3. ** anchor ** — 
4. **patch  half state** — `git revert`  debug
5. **`git add -A` ** —  `git status`  untracked

### 11.5 

> 5  v0.30-0.33 module **""** — , , 0 . v0.34 **""** —  builtin  5 , 8 commits, 333 tests pass, 5 demos OK. **Mora ** `bus.emit("ai.chat.completed", payload)` **** `EventBus::new().on(...)` ** handler**.

---

##  A:  git 

```bash
# 0.  ("")
$ git checkout -b v0.34-integrate

# 1. bump version
$ sed -i 's/version = "0.0.33"/version = "0.0.34"/' Cargo.toml
$ git add Cargo.toml && git commit -m "chore(v0.34): bump version 0.0.33 -> 0.0.34"
# → 60fdd75

# 2. bus builtin ( patch, 5 )
$ # (patch mod.rs / dispatch.rs / builtins.rs / tests)
$ git add -A && git commit -m "feat(v0.34): bus.emit/off/count builtin (integrate event module)"
# → 32b1dc0 (3 files, 862 insertions, 1 deletion)

# 3. .gitignore cleanup
$ git add .gitignore && git commit -m "chore(v0.34): .gitignore tmp research artifacts"
# → 494d073

# 4-6. sandbox / schedule / ccr ( patch)
$ # ... ( commit)
# → dba1c9d / c712d0f / 5066356

# 7. mock builtin
$ git add -A && git commit -m "feat(v0.34): mock builtin (integrate mock::MockRegistry)"
# → 65eea4b

# 8. failed ai.tokens attempt
$ git add -A && git commit -m "feat(v0.34): ai.tokens builtin (mini-swe-agent cost tracking)"
# → 374570e (build fail: duplicate test fn)
$ git revert HEAD --no-edit
# → 92355d8 (revert OK, build clean)

# 9. CHANGELOG + demo
$ # (edit CHANGELOG.md, create examples/integration_v0_34.mora)
$ git add -A && git commit -m "docs(v0.34): CHANGELOG entry + integration demo"
# → 8d50a78 (2 files, 138 insertions)

# 10. Merge to main
$ git checkout main
$ git merge --no-ff v0.34-integrate -m "merge(v0.34): integrate 5 v0.30-0.33 orphaned modules as builtins"
# → d00a95c (merge commit)

# 11. Delete branch
$ git branch -d v0.34-integrate
```

** 11  git , 8  commit, 1  commit + revert (ai.tokens), 1  merge commit**.

---

##  B: v0.34 

|  | v0.33 → v0.34  |
|---|---|
| `Cargo.toml` | version 0.0.33 → 0.0.34 |
| `src/interpreter/mod.rs` | +167  (5 new fields, 20 init, 5 register, 5 new tests, 1 revert cleanup) |
| `src/interpreter/dispatch.rs` | +7  (5 module method routing arms) |
| `src/interpreter/builtins.rs` | + 130  (5 new call_*_method ) |
| `CHANGELOG.md` | +138  (v0.34 ) |
| `examples/integration_v0_34.mora` | +33  () |
| `.gitignore` | +6  (tmp research) |
| **** | **9 files changed, ~480 ** |

**API surface **: 5  builtin module (bus/sandbox/schedule/ccr/mock),  3-5  method. ** ~22  builtin method**  Mora .

---

##  C: Mora v0.34 builtin 

| builtin | method |  |  |
|---|---|---|---|
| `bus.emit` | (event, payload?) |  pattern  handlers | Puter EventClient |
| `bus.off` | (pattern) |  pattern | Puter |
| `bus.count` | () |  pattern  | Puter |
| `sandbox.check_builtin` | (name) | bool, builtin name  allow/deny  | AIOS + MimiClaw |
| `sandbox.check_path` | (path) | bool,  `..`  | MimiClaw path validation |
| `sandbox.allow` | (pattern) |  pattern  allow  | Puter whitelist |
| `sandbox.deny` | (pattern) |  pattern  deny  | Puter whitelist |
| `sandbox.mode` | () | "strict" / "permissive" (heuristic) | AIOS |
| `schedule.add` | (name, kind, message, interval_s?, at_epoch?) |  cron job,  id | MimiClaw cron |
| `schedule.list` | () | List of Job dicts | MimiClaw |
| `schedule.remove` | (id) | bool,  job | MimiClaw |
| `schedule.tick` | () |  due job,  [triggered, ...] | MimiClaw |
| `schedule.count` | () |  job  | MimiClaw |
| `ccr.put` | (data) | hash (8-char hex),  | Headroom CCR |
| `ccr.get` | (hash) | data  Nil | Headroom CCR |
| `ccr.marker` | (hash, size) | `<<ccr:hash,size>>`  | Headroom |
| `ccr.extract` | (marker) | hash, parse marker | Headroom |
| `ccr.len` | () | entry  | Headroom |
| `mock.register` | (name) | stub,  handler  v0.35 | OpenFugu MockWorld |
| `mock.unregister` | (name) | stub | OpenFugu |
| `mock.count` | () | pattern  | OpenFugu |
| `mock.names` | () | List of String | OpenFugu |

**v0.34 builtin : 22  method, 5  module, 5 **.

---

##  D: Mora v0.30 → v0.34 

|  |  |  |  |  builtin |
|---|---|---|---|---|
| v0.30 (SmartCrusher) | +1067 | +5 (277 total) | 0 () | 1 (`compress.json`) |
| v0.31 (no-panic) | +56 | (277) | 0 | 0 |
| v0.32 (3 modules) | +862 | +9 (286) | 3 (recursive walker, event, mock) | 0  () |
| v0.33 (4 modules) | +1381 | +34 (320) | 4 (schedule, sandbox, reading_order, ccr) | 0  () |
| **v0.34 ()** | **+480** | **+8 (328)** | **0** | **5 **  |
| **** | **+3846 ** | **+56 test (320 → 328)** | **7 ** | **5 builtin** |

****: v0.32-0.33  7  (), v0.34  0 ** 5 ** ().

**v0.34 **, . AGENTS.md  "",  ""  "" — **, **.

---

##  E: Mora  builtin  (v0.34)

| builtin |  |  |
|---|---|---|
| `print` |  | v0.0.1 |
| `range` |  | v0.0.1 |
| `len` |  | v0.0.1 |
| `ai` |  | v0.06 |
| `web` |  | v0.10 |
| `json` |  | v0.10 |
| `file` |  | v0.04 |
| `memory` |  | v0.04 |
| `agent` |  | v0.06 |
| `document` |  | v0.27 |
| `compress` |  | v0.29 |
| `crush_json` |  | v0.29 |
| `compose_prompt` |  | v0.26 |
| `tail` |  | v0.26 |
| `route` |  | v0.04 |
| `mcp_server` |  | v0.06.6 |
| `http_server` |  | v0.06.3 |
| `skill` |  | v0.16 (v0.33  0 , ) |
| **`bus`** | **v0.34 ** |  v0.32 ,  |
| **`sandbox`** | **v0.34 ** |  v0.33 ,  |
| **`schedule`** | **v0.34 ** |  v0.33 ,  |
| **`ccr`** | **v0.34 ** |  v0.33 ,  |
| **`mock`** | **v0.34 ** |  v0.32 ,  |

**Mora v0.34 builtin : 23  (18  + 5 v0.34 )**.

---

****:  **"v0.x "**  — " API", ", ". Mora v0.30-0.33  5  module 0 ****, v0.34 .  (v0.35+) ** 5 **,  commit  module.  8  commit  Mora ****.

: `D:\Github\mora-lang\AGENTS_SESSION_V0_34.md`
