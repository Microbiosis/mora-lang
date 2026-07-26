# Mora-lang Zero-Trust Audit (v0.34)

> ****:  → 
> ****: 2026-07-03 v0.34 merged (commit `d00a95c`)
> ****: `src/**/*.rs` ( test)
> ****:  /  /  / 
> ****:  fan-out +  file:line  P0
> ****: 50  ( 4 ),  direct-read 

---

## 0.  (TL;DR)

|  | P0 | P1 | P2 |  |
|---|---|---|---|---|
|  | 5 | 5 | 2 | 12 |
|  | 6 | 5 | 4 | 15 |
|  | 5 | 8 | 2 | 15 |
|  | 4 | 6 | 5 | 15 |
| **** | **20** | **24** | **13** | **57** |

###  ()

1. **v0.34  5  module  Interpreter  Half-Integration**: 5  builtin "", **/ v0.32-v0.33  v0.34 ** —  4  **15  P0  `Clone for Interpreter` (mod.rs:230-270)**.  shallow-clones  state  skip  v0.34 field,  fresh empty  (Scheduler/CCR counter  mint id/hash).

2. **v0.31  no-panic refactor  4 **:
   - **panic :  `.expect()`  `panic!`** — Display impl  `.expect()`  poisoned mutex  panic (`value.rs:218, 245`)
   - **unwrap : `Result<_, String>`  fallback  `Value::Nil`** —  **** (P0 : Dict.get  key  Nil, CallTask_inner  arg  Nil)
   - **panic-elimination  `src/ast_v2.rs`** — `walk_expr`  13  `.unwrap()` (ast_v2.rs:625-657)
   - **panic-elimination  `src/value.rs`  Display impl** — Display  fallback boundary

3. **v0.26-0.33  feature creep  static type system  4  soundness hole**:
   - REPL bypass (mod.rs:651-689)
   - `StmtKind::Route` dead code (parse + typeck  execute )
   - `evaluate_index`  key  Nil (evaluate.rs:180)
   - `call_task_inner`/`call_value_inner` arity check  `unwrap_or(Nil)`

4. **5  v0.34 builtin  `call_*_method`  type-soundness **:
   - `bus.emit(event, payload)`  Value,  ( P1)
   - `sandbox.check_path`  Bool  `PathBuf` (TOCTOU  caller  path)
   - `schedule.add(kind)` stringly-typed ( enum JobKind)
   - `ccr.put(data)`  Value, silent lossy (Number.to_string )
   - `mock.register` stub (handler )

5. **`Clone for Interpreter`  P0** — " clone ", " clone  v0.34 ".  v0.34 ****:  Clone impl , **** singleton.  v0.35  module  fix .

---

##  1:  (High-Concurrency)

###  P0-1.1 `Clone for Interpreter` ** `Send`-unsafe fabrication**
**File**: `src/interpreter/mod.rs:230-270` ()

```rust
worker_channels: HashMap::new(),      //  channel
worker_receivers: HashMap::new(),
ai_cache: HashMap::new(),
string_interner: HashMap::new(),
method_cache: HashMap::new(),
...
bus: crate::event::EventBus::new(),        // empty
sandbox: ...::permissive(),
scheduler: ...::Scheduler::new(),          // counter reset
ccr_store: ...::InMemoryCcrStore::new(),   // counter reset
mock_registry: ...::MockRegistry::new(),
```

**5  v0.34  fresh empty**:
- `Scheduler::new()` → `next_id: Arc<Mutex<u32>> = Mutex::new(0)` → ** clone mint  `00000001`**
- `InMemoryCcrStore::new()` → `counter: AtomicU64(0)` → ** clone mint  hash 8-char hex**
- `MockRegistry::new()` → handler  empty → **original  handler **

****: `dispatch.rs:998` (`Router.listen`)  `dispatch.rs:1035` (`McpServer.serve`)  `http_server.rs:201,311`  `interpreter.clone()`  worker.  worker **** interpreter.

****:  `Clone`  `Arc<Interpreter>`;  5  `Arc<Inner>`,  `clone()` .

---

###  P0-1.2 `EventBus::emit` **re-entrant deadlock**
**File**: `src/event/mod.rs:55-64` ()

```rust
pub fn emit(&self, event: &str, payload: &Value) {
    let map = self.handlers.lock().expect("event bus mutex poisoned");  // lock A
    for (pattern, handlers) in map.iter() {
        if matches(event, pattern) {
            for h in handlers {
                h(event, payload);   // ← handler runs while lock A held
            }
        }
    }
}  // lock A drop
```

 handler  `bus.emit`  → `std::sync::Mutex::lock()` **** → . Mora :
```mora
bus.on("outer.*", fn(e,p) bus.emit("nested."+e, p) end)
bus.emit("outer.test", nil)
```
 user-visible deadlock.

****: Clone-and-drop  —  handler snapshot,  drop lock,  iterate.

---

###  P0-1.3 `ccr.put` silent overwrite (counter wrap + clone collision)
**File**: `src/ccr/mod.rs:56-71` ()

```rust
fn put(&self, data: &str) -> String {
    let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;  // u64
    let hash = format!("{:08x}", n);    // 8 hex chars = 32 bits
    ...
    self.entries.lock()...insert(hash.clone(), entry);  // 
    hash
}
```

****:
1. **n wrap at 4_294_967_296**: `n = 0x100000001` → `"10000001"` ,  n=257 (`"00000101"`)  n=4_294_967_297 (`"10000001"`) —  `{:08x}`  8 chars, n=4_294_967_296 = `0x100000000` → `"10000000"`,  n  — ** n > 2^36**.  n = 4_294_967_296 + 0x100 = 4_294_967_552 → `"10000100"`  n=256 (`"00000100"`) .
2. **Clone  (P0-1.1 )**:  clone  AtomicU64(0) , `fetch_add(1)`  0,  mint `00000001`.  put  silent overwrite .

****: key  `n` (u64)  hex string;  `Entry::or_insert` ;  hex + checksum.

---

###  P0-1.4 `MockRegistry::call` lock-hold-across-user-fn ( P0-1.2 pattern)
**File**: `src/mock/mod.rs:73-79` — `MockHandler::Native(f)` lock-held-call; `mock::Script(_) => None`  drop.

****: `builtins.rs:466-471`  v0.34 wrapper  ( `get()`  lock  invoke),  `MockRegistry::call`  API  unsafe.  API, , ****.

****:  `MockRegistry::call`  clone-and-drop (mirror builtins.rs  path).

---

###  P0-1.5 `v2_arena: Option<AstArena>`  closure/task  deep clone
**File**: `src/interpreter/dispatch.rs:1067, 1082` — `self.v2_arena.clone()`  v2 closure/task .

`AstArena`  100KB+ .  `(map)`/`(filter)`/`(reduce)` element  deep-clone  arena. **HTTP server worker (P0-1.1 )  arena **.

****: wrap `v2_arena`  `Arc<AstArena>` .

---

### 🟡 P1-1.6 Lock-hold-across-IO (http_server.rs:175-185, dispatch.rs:982-996)
 lock + `eprintln!` stdout flush  lock .  route → .  priority-inversion.

---

### 🟡 P1-1.7 `Scheduler::tick()` race window
**File**: `src/schedule/mod.rs:173-205` — lock  `save()`  lock.  lock  panic → .

---

### 🟡 P1-1.8 `Scheduler.next_job_id: Mutex<u32>` overflow
**File**: `src/schedule/mod.rs:78-82` — 4B  wrap;  P0-1.1 Clone  mint  id.

---

### 🟡 P1-1.9 `MockRegistry::call`  wrapper  ()

---

### 🟡 P1-1.10 `v2_arena.clone()` deep clone per call ()

---

### 🟢 P2-1.11 `sandbox.check_path` TOCTOU by construction
**File**: `src/sandbox/mod.rs:81-114` — `canonicalize()` + `starts_with()`  syscall,  symlink race.  caller  path ,  API .

---

### 🟢 P2-1.12 `InMemoryCcrStore`  `RwLock`/`DashMap`
 Mutex.

---

##  2:  / 

###  P0-2.1 `parse_json_list` / `parse_json_dict` O(n²)
**File**: `src/flow.rs:413, 441, 461` — `&s[i..].trim_start()`  loop .

1000-item JSON list +  → 500K char-scans; 10K → 50M.  `dict.json()`, `web.fetch`, `crush_json_string`, AI response parse .

****: `while i < s.len() && matches!(s.as_bytes()[i], b' ' | b'\t' | b'\n' | b'\r') { i += 1; }` —  alloc, O(1) per step.

---

###  P0-2.2 7  `#[allow(dead_code)]` Interpreter 
**File**: `src/interpreter/mod.rs:163-191, 245-260` — `method_cache`/`ai_batch_queue`/`cache_warm_queue`/`ai_priority_queue`/`adaptive_temp`/`load_balancer`/`retry_policy`.

****, `grep`  0 .  Interpreter  +480B  state. `Router::listen`/`McpServer::serve`  `clone()`  480B .

****:  7 ,  `Self{}` × 4  + `Clone`.

---

###  P0-2.3 `evaluate_call`  call alloc Vec +  environment Mutex
**File**: `src/interpreter/evaluate.rs:55-87` — `Vec::new()` heap alloc + `Arc<Mutex<Environment>>::lock()` per call.

100K-token context , allocator pressure . `evaluate_pipe` (104), `evaluate_method_call` (150) .

****:  `SmallVec<[Value; 8]>`  thread a reusable `&mut Vec`; environment  RwLock.

---

###  P0-2.4 `v.to_string()`  18+ ,  `Value::String` arg
**File**: `src/interpreter/dispatch.rs` 18+ sites + `builtins.rs` lines 240-548  builtin arg . `Dict.get(key)`:  .get()  `to_string()`,  key  String.

****:  `arg_str(idx) -> Option<&str>` helper,  String  `Cow::Borrowed`.

---

###  P0-2.5 `SpeculativeVerifier`  cache +  verification queue
**File**: `src/interpreter/ai_chat.rs:355-369`, `src/ai_infra.rs:186-195` — cache key  `{draft.len()}:{verification.len()}` ( →  →  cached ).  correctness bug + perf bug.

---

###  P0-2.6 `SmartCrusher` `format!("{:?}", v)` per value
**File**: `src/compress/json.rs:340, 530, 635` — `compute_uniqueness`/`ClusterSampleStrategy`/`KeepErrorsConstraint`  value `format!`. 100K items × 5 fields = 50K+ Strings allocated.

`KeepErrorsConstraint` 14 × 2 `to_lowercase()` × N items × M fields ≈ 300K allocs.

****:  `Value` discriminant tag  hash;  lowercase keyword table.

---

### 🟡 P1-2.7 `Value::List` Display build `Vec<String>` + join (value.rs:183-190)
N intermediate String allocs per Display.

---

### 🟡 P1-2.8 `call_value_inner`  closure call  `Arc<Mutex<Environment>>` + per-arg clone (dispatch.rs:1175-1191)

---

### 🟡 P1-2.9 `starts_with(&v.to_string())`  v  String (dispatch.rs:684+)

---

### 🟡 P1-2.10 `Clone` deep clone `trait_registry`, `impl_table`  (mod.rs:243-244)

---

### 🟡 P1-2.11 `_cache_key = format!(...)`  (dispatch.rs:449-450)

---

### 🟡 P1-2.12 `estimate_bytes`  `value_to_json().len()` (compress/json.rs:950)

---

### 🟢 P2-2.13 `string_interner`  eviction,  (mod.rs:580-587)

---

### 🟢 P2-2.14 `ai_cache` key `format!("{}:{:?}", model, messages)`  100 messages per call (ai_chat.rs:404)

---

### 🟢 P2-2.15 `parse_json_string` silent UTF-8  (flow.rs:399)

---

##  3:  (Strong-Typing)

> : v0.31  no-panic refactor (commits `b374975`, `d891326`)  panic  OK, ** 4 **.

###  P0-3.1 `src/ast_v2.rs:625-657` `walk_expr` 13× `arena.get_expr(*child).unwrap()`
**File**  — 13  .unwrap()  visitor traversal.

`walk_expr`  type-checker / lints / codegen / lsp visitor  utility. `get_expr`  `Option<&TypedExpr>` (ast_v2.rs:582). `None` :
-  parse  stale NodeId
-  dangling ref
-  arena

Visitor pass  panic interpreter. ** v0.31  lexer/parser no-panic invariant** —  refactor  lexer/parser,  ast_v2.

****:  `Result<T, String>`;  `walk_expr -> Option<T>`  caller  None.

---

###  P0-3.2 `src/value.rs:218, 245` `Display::fmt`  `.expect()` — **panic in Display**
**File**  `expect("...mutex poisoned")`  `Value::Router`  `Value::Atom` Display.

`Display::fmt`  REPL, , Value  fallback boundary.  poisoned mutex →  to_string  ( List/Dict)  crash.

****:  panic in Display —  `write!(f, "<router (poisoned)>")`.

---

###  P0-3.3 `src/interpreter/mod.rs:384`  bare `.unwrap()`
**File**  `globals.lock().unwrap().define("len", ...)` —  bare unwrap. 1 .

---

###  P0-3.4 Lexer  NUL /  in strings
**File**: `src/lexer.rs:546-583, 643-692` — `string_from`/`prompt_string_from`  `\0`.  POSIX  / HTTP body  crash downstream.

****: `c < 0x20 && c != '\n' && c != '\t' && c != '\r'` → emit error token.

---

###  P0-3.5  P0 ()
- `src/interpreter/builtins.rs:244, 297-333, 391-397` (bus.emit/schedule.add/ccr.put  Value +  lossy)
- `src/ccr/mod.rs:96` `extract_hash`  malformed marker  `Some("")`
- `src/event/mod.rs:55-64` re-entrant emit ( P0-1.2)
- `src/value.rs:43` NaN 

( P1-3.6 ~ 3.13)

---

### 🟡 P1-3.6 `Value::Builtin(String)` 30+  dispatch (value.rs:60)
 `web.fecth` , . ** typing win** —  enum.

---

### 🟡 P1-3.7 `bus.emit`  Value  event name (builtins.rs:244)
`Value::Number(1.5)` → event `"1.5"`.

---

### 🟡 P1-3.8 `ccr.put` silent lossy (builtins.rs:391-397)
`Value::Number(0.1+0.2)` → `"0.30000000000000004"`, LLM  marker  reverse. CCR  contract .

---

### 🟡 P1-3.9 `schedule.add(kind)` stringly-typed (builtins.rs:297-333)
 enum JobKind, .

---

### 🟡 P1-3.10 `sandbox.allow: Vec<String>`  (sandbox/mod.rs:20, 22)
O(N) per check;  allow pattern .

---

### 🟡 P1-3.11 `event::matches`  `pa_segments.len() <= ev_segments.len() + 1`  (event/mod.rs:92-110)
,  +1 .

---

### 🟡 P1-3.12 `MockRegistry::call` Script handler  None (mock/mod.rs:73-79)
v0.34 wrapper ,  module  API  footgun.

---

### 🟡 P1-3.13 `Value::Number(f64)` NaN/Infinity  (value.rs:43)
`0.0/0.0`  equal to itself → dict lookup .

---

### 🟢 P2-3.14 `Value::List` Display  cycle guard (value.rs:184-185)
 `List` by-value, cycle ,  `Atom(Arc<Mutex<Value>>)` . Display  block on user data.

---

### 🟢 P2-3.15 `file.*`  `sandbox.check_path` (builtins.rs:32-110)
**File**  `call_file_method`  call `self.sandbox.check_path(&path)`.  file builtin . ,  bug.

---

##  4:  (Static-Typing)

###  P0-4.1 `Value::Dict`  key  Value::Nil (evaluate.rs:180)
**File**  `(Value::Dict(map), Value::String(key)) => Ok(map.get(key).cloned().unwrap_or(Value::Nil))`.

typeck `check.rs:914-925`  `Dict.get(key) -> V` (value type).  `let x: number = {"a": 1}["b"]` pass typeck. **Runtime  Value::Nil,  number binding**.  soundness hole — static checker , runtime .

****: Dict.get  narrow to `V | Nil` (Union); call site  Nil ( silent coerc).

---

###  P0-4.2 `Value::Task`/`Value::Closure` arity  `unwrap_or(Value::Nil)` (dispatch.rs:1115, 1182)
**File**  `args.get(i).cloned().unwrap_or(Value::Nil)`.

typeck `check.rs:846-861`  `arg count mismatch` ** `!sig.params.is_empty()`**:
- zero-param task 
- , runtime  nil-fill 

Static `task add(a: number, b: number) ...; add(1)` → typeck  → runtime  `add(1, nil)` → `1 + nil`  operator error.

****: `call_task_inner`  `Err("missing arg 'b'")`  arg  param  default.

---

###  P0-4.3 REPL bypasses typeck  (interpreter/mod.rs:651-689, main.rs:952-955)
**File**  `run_repl_with`  `parse_code()`  `interp.execute()` ** `typeck::check_program`**.

`run_file`, `run_record`, `run_replay`, `run_snapshot`  typeck; ** REPL skip**.  REPL  `let x: number = "hello"` , .

****: `run_repl_with` line 673  `let _ = typeck::check_program(&node_ids, &arena);` 2 .

---

###  P0-4.4 `StmtKind::Route` typecheck  execute  (execute.rs:144)
**File**  — `grep -n Route src/interpreter/execute.rs` .

`StmtKind::Route`  parse → typeck  → `execute_stmt`  `_ => Err(format!("Unsupported v2 statement: ..."))` (execute.rs:144). `route_registry: HashMap<String, String>`  → Clone → Default → ****. Worse, typeck  `routes: HashSet<String>` (mod.rs:527) ** dead state**.

`ExprKind::RouteCall` (ast_v2.rs:139)  parser path ,  orphaned.

****:  `execute_route` ****  4  dead code (Stmt + Expr + parser arm + typeck arm + dead field).

---

### 🟡 P1-4.5 `Type` enum  `Document`  (typeck/mod.rs:38-89)
21 variants  `Union(Vec<Type>)`. `DocumentBackend` (document/mod.rs:14) 5  (markdown/text/pages/metadata/blocks)  typeck  → method_return_type_fallback  `Union([])` → `any`. `let blocks: list<dict> = d.blocks()` pass typeck.

****:  `Type::Document` + 5 method arms.

---

### 🟡 P1-4.6 `current_ai_config: Option<AiConfigValue>`  typecheck
**File**: `interpreter/mod.rs:149`, **8 **: model/temperature/max_tokens/budget/per_call/system/mock_responses/speculative/draft_model.

`Type::AiConfig`  (mod.rs:55) ** singleton value,  structural record**. `with`  drop `system`/`budget`/`max_tokens` (orchestrate.rs:161-180).

---

### 🟡 P1-4.7 `load`/`read_file` typeck  (check.rs:111-115)
`load` → `Union([])` (= any); `read_file` → `String`. `let n: number = load("f")` pass, runtime  Nil → 0.

`read_bytes_file`  arm,  fall through.

---

### 🟡 P1-4.8 Typed numeric literals  (`1i64`/`1u32`/`1.0f32`)
`Literal::Number(f64, Span)`  numer . Parser  `1i64`, typeck .

---

### 🟡 P1-4.9 `document.reading_order`  module  builtin
 interpreter. typeck  `Type::Document` ( P1-4.5).

---

### 🟡 P1-4.10 `ImplDef` orphan `for_type`  (check.rs:1081-1103)
`for_type: "MisspellWidget"` ( type) + `Display` 5  impl 1 → typeck accept. Runtime dispatch ** missing method **.

****: `check_impl_def_stmt`  `for_type`  + trait methods  + method signatures .

---

### 🟢 P2-4.11 typeck errors  `line: 0, column: 0` (check.rs:204-1006 )
manual `TypeError { ... }`  Span. .

---

### 🟢 P2-4.12 `print` Union hand-maintained 6-element list (mod.rs:636-655)
.  list/dict .

---

### 🟢 P2-4.13 `let x: Never = ...` / `let x: Unknown = ...`  `Type::Trait { name }`  (mod.rs:213-218)
 `Never`  trait_registry → ;  stub trait → silent accept.

---

### 🟢 P2-4.14 `let x = expr`  hint  `init_ty = Union(vec![])`  any (check.rs:229-231)
`let x = unknown_call()` → x  any .

---

### 🟢 P2-4.15 `with`  typeck  binding key  in target type (check.rs:75-77, 467-483)
`with foo = 42 do ... end` ( `foo` ) silent accept.

---

##  → v0.34  → v0.35 

|  |  |  |  |
|---|---|---|---|
| v0.04 | `Clone for Interpreter`  13  | mod.rs:230-270 | v0.34  5  → P0-1.1 |
| v0.04 | `method_cache`  |  | 7 dead_code  (P0-2.2) |
| v0.26 | `StmtKind::Route` + `route_registry`  | parse + typeck  execute | P0-4.4 dead code |
| v0.30 | `SmartCrusher` content-aware  stringification | compress/json.rs format!("{:?}", v) per value | P0-2.6 |
| v0.31 | `panic-elimination` refactor |  lexer/parser,  ast_v2 + value.rs Display | P0-3.1, P0-3.2 |
| v0.32 | `EventBus`  | mutex-held user handler | P0-1.2 |
| v0.32 | `MockRegistry`  | mutex-held user fn + Script handler  None | P0-1.4, P1-3.12 |
| v0.32 | `RecursiveWalker` |  module (orphaned?) |  5 builtin  |
| v0.33 | `Scheduler` `next_id: Mutex<u32>` | counter overflow + Clone collision | P0-1.1, P1-1.8 |
| v0.33 | `SandboxPolicy` Vec allow/deny + Bool check_path | P1-3.10 + P2-1.11 | |
| v0.33 | `CcrStore` hex=u32 + silent overwrite | P0-1.3 | |
| v0.33 | `ReadingOrder`  module,  typeck  | P1-4.9 | |
| v0.34 | 5 builtin  Interpreter **** | 5  call_*_method  unsafe API  façade |  v0.34 builtin  P0 |

### v0.35 P0  ( ROI)

|  |  |  |  |
|---|---|---|---|
|  P0-4.3 | REPL  typeck | main.rs:952-955 + interpreter/mod.rs:651-689 | 2  |
|  P0-4.4 |  `execute_route`  4  | execute.rs:144 + parser/typeck/ast_v2 |  |
|  P0-2.2 |  7 dead  | interpreter/mod.rs:163-260 | ~80  |
|  P0-3.3 | `.unwrap()` → `.expect()` | interpreter/mod.rs:384 | 1  |
|  P0-1.2 | EventBus clone-and-drop | event/mod.rs:55-64 | 5  |
|  P0-1.4 | MockRegistry  + Script  | mock/mod.rs:73-79 | 8  |
|  P0-1.5 | v2_arena wrap in Arc | interpreter/mod.rs + dispatch.rs | 4  |
|  P0-3.1 | walk_expr  unwrap | ast_v2.rs:625-657 | 13  |
|  P0-3.2 | Display  panic | value.rs:218, 245 | 4  |
|  P0-4.1 | Dict.get narrow to V \| Nil | evaluate.rs:180 + check.rs:914-925 | 3  |
|  P0-4.2 | call_task_inner arity Err | dispatch.rs:1115, 1182 | 4  |
|  P0-2.1 | parse_json O(n²) → O(n) | flow.rs:413, 441, 461 | 10  |
|  P0-2.4 | arg_str helper | dispatch.rs 18+ sites + builtins.rs 30+ sites | large refactor |
|  P0-2.5 |  SpeculativeVerifier length cache | ai_chat.rs:355-369 + ai_infra.rs:186-195 | 20  |
|  P0-1.3 | ccr hash key  u64 | ccr/mod.rs:56-71 | 4  |
|  P0-1.1 | Clone impl  Arc  OR  Clone | interpreter/mod.rs:230-270 | 50+  |

### v0.35+ P1  (10 )

- `Type::Document` +  16  Value  Type variants
- `Value::Builtin` enum  (30+  dispatch)
- sandbox Vec → HashSet, schedule enum JobKind 
- parse_json O(n²) + SmartCrusher format!("{:?}", v) per value
- Display cycle-safe (Atom )
- Worker_channels  (`!Send`)

###  debt  (v1.0 )

- `mpsc::Receiver in worker_receivers: HashMap<…>` —  `!Send`,  Interpreter  `!Send` (`!Send for Interpreter` , +threading model )
- `Value::Number(f64)`  numer  —  v0.35 ,  v1.0  Numeric 
- Document/Trait/Schedule/Sandbox/Event/Mock/AiConfig  Type variants — , 16 

---

## 

> **v0.34 ""release,  release.**
> :  5  module  Interpreter  5 .
> :  audit module  (EventBus/Mock registry  lock pattern, Scheduler next_id , CCR hex hash  collision).
> ****: v0.34  "Clone for Interpreter"  (mod.rs:230-270)  v0.32-0.33 module ** state **  **per-clone fresh state**.  v0.35 .
>
>  5 step, ** 0  module **. v0.35  **`module-readiness-checklist`**:

```
 Module API thread-safe (Send/Sync) under documented sharing
 All Mutex guards NOT held across user-supplied callbacks
 All counter-based identity 64-bit + monotone + Clone-aware
 All file/path operations TOCTOU-safe by API shape (return enum, not Bool)
 All Value-accepting APIs validate at boundary or document lossy behavior
 Typeck has matching Type variant before builtin is added
```
