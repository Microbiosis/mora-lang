# Mora v0.34  panic + actor/pressure 

> ****"v0.34  panic""v0.34  2 actor/pressure/async I/O"
>
> ****`fix/v0.34-production-panics`  `feat/v0.34-actor-pressure`

---

## 0. 

|  |  |
|------|----------|
|  Mora  | §1 → §2 → §3|
|  panic  | §4panic |
|  actor/pressure  | §5actor/pressure |
|  commit  | §6/|
| " Interpreter " | §7v0.35 |

---

## 1. v0.34 

Mora  v0.34 ""

1. ** panic**`AGENTS.md` 
   - `unwrap()` / `panic!` `expect("")` 
   - lexer / parser **** `Result`  emit error token panic
   -  `.unwrap()``.expect("mutex poisoned")``.expect("xxx is None")`

2. **v0.34  5  builtin **event / schedule / ccr / mock / sandbox `Arc<Mutex<...>>`  HTTP/MCP worker  worker  [v0.34 ]  2 

`AGENTS.md`  v0.x " →  → /"**** panic actor/pressure 

---

## 2. 

```

                                
   1.  panic                     
   2.  Arc<Mutex>  actor/      
   3.                    
   4.  async I/O                           

              
              
  
                               
                               
 A panic              Bactor/pressure
()                ( tokio/reqwest)
                               
 step1:  fix             step1: Cargo.toml  tokio/reqwest
  (lexer, flow.rs, lsp       
   formatting, interpreter     step2:  actor.rs 
   mod.rs)                    
  → fix/v0.34-production-      step3:  pressure.rs
    panics                 
  → commit d891326             step4: 5  actor 
                                 (event/schedule/ccr/mock/trace)
 step2:  plan mode        
   panic            step5:  Interpreter 
  AskUserQuestion        →  16 ****
  ExitPlanMode                → " v0.35"
  →  commit b374975   
                                step6:  CHANGELOG +  doc
                                 → feat/v0.34-actor-pressure
                               
                               
                              main (fast-forward)
                             
```

 `main`

|  |  |  |
|------|------|------|
| `d891326` | `fix/v0.34-production-panics` |  panic4 ~12 |
| `b374975` | `fix/v0.34-production-panics` |  panic10 |
| `8e975a6` | `feat/v0.34-actor-pressure` | actor + pressure + 5 actor  |
| `540f72f` | `feat/v0.34-actor-pressure` |  Interpreter  v0.35 |
| `ffa6ff6` | `feat/v0.34-actor-pressure` | CHANGELOG  |

---

## 3. 

""

### 3.1  panic  5 ""

 `grep`  `.unwrap()` / `.expect()` / `.panic!` / `.unreachable!`"""""" `.expect("mutex poisoned")` 



```bash
#  .expect() / .unwrap() / .panic! / .unreachable!()
grep -nw --include="*.rs" -e "panic!" -e "\.unwrap()" src/

#  .expect( 
grep -Rnw --include="*.rs" src/ -e "\.expect("
```

### 3.2  `.unwrap()` 

|  |  |  |
|--------|--------|----------|
| `some_lock.lock().expect("xxx mutex poisoned")` | `some_lock.lock().map_err(|_| "xxx mutex poisoned".to_string())?` |  `Result<T, String>` panic  `Err`  |
| `some_lock.lock().expect("xxx mutex poisoned")` | `let guard = some_lock.lock().expect("xxx mutex poisoned"); guard.xxx` | `new()`/`init()`  `Result`  expect |

 `.expect("xxx mutex poisoned")`

### 3.3 irrefutable `Some(x) => y.unwrap()` ""

`Some(Value::Closure { .. }) => self.call_value_inner(&func_val.unwrap(), ...)` ** panic**——`func_val`  `Some`  `Some``unwrap`  AGENTS.md  `.unwrap()` irrefutable pattern

```rust
match func_val {
    Some(ref val) => {
        if matches!(val, Value::Closure { v2_node_id: Some(_), .. }) {
            self.call_value_inner(val, arg_vals, arena)
        } else {
            self.call_function(callee, arg_vals, Span::default())
        }
    }
    None => self.call_function(callee, arg_vals, Span::default()),
}
```

 `Some` 

```rust
match val {
    Some(ref v) => self.call_value_inner(v, vec![left_val], arena),
    None => self.call_method(left_val, name, vec![], Span::default()),
}
```

### 3.4 `actor.rs` "boxed future  state" Rust async actor 

****`F: FnMut(&mut S, M) -> Fut` + `Fut: Future + 'static`  handler  future  `state``'static`  handler  `state.xxx()`

****

```rust
pub type ActorFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

pub fn spawn_actor<S, M, F>(mut state: S, mut handler: F) -> ActorHandle<M>
where
    S: Send + 'static,
    M: Send + 'static,
    F: for<'a> FnMut(&'a mut S, M) -> ActorFuture<'a> + Send + 'static,
{
    let (tx, mut rx) = mpsc::unbounded_channel::<M>();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            handler(&mut state, msg).await;
        }
    });
    ActorHandle { tx }
}
```

 `Box::pin(async move { ... })` future  `&mut state`  `'a` 

### 3.5 " sync  async" 

Actor ****`Interpreter`  `Arc<Mutex<...>>`  `ActorHandle<...>`  5  `call_*_method`  `dispatch.rs`  `actor.ask(...).await`  builtin dispatch `execute` → `call_function` → `call_method` → builtin `async fn`

****actor **** Interpreter actor  5  pilot  async migrationpilot  `feat/v0.34-actor-pressure` Interpreter " v0.35"

### 3.6  `io::Error::new(io::ErrorKind::Other, "...")`  clippy  `io::Error::other("...")`

```rust
// 
io::Error::new(io::ErrorKind::Other, "shutdown mutex poisoned")
// clippy::io_other_error
io::Error::other("shutdown mutex poisoned")
```

### 3.7  CHANGELOG 

CHANGELOG  `use cargo test --all` 

---

## 4.  A panic 

### 4.1  fixcommit `d891326`

|  |  |
|------|------|
| `src/lexer.rs` |  `value.parse().unwrap()`  `match`  `error_token`emit `TokenType::Error(msg)`|
| `src/flow.rs` | `parse_json_dict`  `unreachable!()`  `Err("JSON object key must be a string")` |
| `src/lsp/providers/formatting.rs` | `range/start/end`  `.expect(...)`  `match`  `Value::Array` |
| `src/interpreter/mod.rs` | `extract_embeddings`  `.expect("should have elements")`  `match` +  `Err` |
| `src/parser_v2/statements.rs` | `loop`  `.expect("loop requires exactly one agent")`  `arena.alloc_stmt`  `NodeId` `with_config`  |

** + ** `git show` 

- `src/lexer.rs:706`
- `src/flow.rs:458`
- `src/lsp/providers/formatting.rs:24-33`
- `src/interpreter/mod.rs:787-797`
- `src/parser_v2/statements.rs:848-870` bug 

### 4.2 commit `b374975`

 `AskUserQuestion`  panic"" `ExitPlanMode`  5 

####  1parser_v2

`eval`  `given:`  panic

```rust
let given = match given {
    Some(g) => g,
    None => {
        eprintln!("Parse error: eval block requires a 'given:' clause");
        crate::ast_v2::NodeId(0)
    }
};
```

####  2LSP `handle_message`  `handle_notification`

`handle_message`  `id.expect("id should exist")` `if id.is_none() { return; }` —— `expect`  `if let Some(id) = id { ... }` 

`handle_notification`  `()`  `io::Result<()>` `docs.lock().expect("docs mutex poisoned")`  `map_err(|e| io::Error::other("docs mutex poisoned"))?``handle_message`  `return self.handle_notification(...);` 

`handle_request`  `docs.lock().expect(...)` 9  `handle_*` hover / completion / definition / references / documentSymbol / formatting / rename / semanticTokens / foldingRange `Value` `Result<Value, String>` `map_err(...)?` 

**clippy **`io::Error::new(io::ErrorKind::Other, "...")`  clippy  `io::Error::other("...")` clippy `io_other_error` lint

####  3 evaluate.rs

 `self.environment.lock().expect("env")` / `expect("environment mutex poisoned")`  `replace_all`  `map_err(|_| "environment mutex poisoned".to_string())?`

irrefutable `Some(...) => val.unwrap()`  `Some(ref val) => ...` 

`match_guard_pattern`  `Option<Vec<(String, Value)>>` `?`  `Result` `.ok()?`

```rust
env.lock()
    .ok()
    ?
    .define(name.clone(), value.clone(), false);
```

####  4 execute.rs

`replace_all`  message

- `.expect("env mutex poisoned")` → `.map_err(|_| "env mutex poisoned".to_string())?`
- `.expect("env")` → `.map_err(|_| "env mutex poisoned".to_string())?`
- `.expect("environment mutex poisoned")` → `.map_err(|_| "environment mutex poisoned".to_string())?`

####  5dispatch / trait_dispatch / orchestrate / mod.rs

`replace_all` 4  message

- `.expect("atom mutex poisoned")` → `.map_err(|_| "atom mutex poisoned".to_string())?`
- `.expect("done mutex poisoned")` → `.map_err(|_| "done mutex poisoned".to_string())?`
- `.expect("routes mutex poisoned")` → `.map_err(|_| "routes mutex poisoned".to_string())?`
- `.expect("tool_registry mutex poisoned")` → `.map_err(|_| "tool_registry mutex poisoned".to_string())?`
- `.expect("env")` → `.map_err(|_| "env mutex poisoned".to_string())?`

`orchestrate.rs`  Graph  evaluation  `find`  `bool` `expect("env")`—— `?`  `Result`

```rust
if let Ok(mut env) = self.environment.lock() {
    env.define("result".to_string(), Value::String(current.clone()), false);
    env.define("rounds".to_string(), ...);
}
```

 false—— panic 

`interpreter/mod.rs`  `Interpreter::new()`  `globals.lock().unwrap().define(...)` `new()`  `Self`  `Result` `?` `.expect("globals mutex poisoned")`" expect"

`interpret()`  `self.globals.lock().expect("globals mutex poisoned").get("main")`  `Result<()>`  `?` 

### 4.3 

 2 

- `tests/parser_v2_integration.rs::test_parse_eval_without_given_no_panic` ——  `eval "name"\nend`  panic
- `src/lsp/server.rs::tests::handle_notification_without_id_no_panic` ——  `id`  JSON-RPC notification  panic

### 4.4 clippy 

`assert!(stmts.len() > 0)`  `clippy::len_zero` `assert!(!stmts.is_empty())`

### 4.5 line A

```
cargo build --all-targets                                  → clean
cargo test --all                                          → 331 passed
cargo clippy --all-targets --all-features -- -D warnings  → clean
cargo fmt --check                                         → 0 diff
```

---

## 5.  Bactor/pressure 

### 5.1 commit `8e975a6`

#### 5.1.1 Cargo.toml

```toml
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time", "net", "io-util", "signal"] }
reqwest = { version = "0.12", features = ["json", "stream"] }
# ureq  AI/Web 
ureq = "3.3"
```

#### 5.1.2 

`src/main.rs`  `src/bin/lsp.rs` 

```rust
#[tokio::main]
async fn main() { ... }
```

`main.rs` process::exit 

#### 5.1.3 actor.rs

 100 

- `ActorHandle<M>` `mpsc::UnboundedSender<M>`
- `tell` (`mpsc::send`)  `ask``mpsc::send` + `oneshot::channel`
- `spawn_actor`  HRTB `for<'a> FnMut(&'a mut S, M) -> Pin<Box<dyn Future + Send + 'a>>` handler  future  `&mut state`

#### 5.1.4 pressure.rs

 150 

- `CircuitBreaker`Closed / Open / HalfOpen /  / Open 
- `QuotaManager` endpoint  `concurrent` + `per_minute`
- `PressureControl::call(endpoint, max_concurrent, max_per_minute, future)`/ breaker

### 5.2 5  actor 



```rust
// 1.  use
use crate::actor::{spawn_actor, ActorHandle};
use tokio::sync::oneshot;

// 2.  enum
pub enum XxxMsg {
    Op1 { ... },
    Op2(oneshot::Sender<T>),
    ...
}

// 3. 
#[derive(Default)]
pub struct XxxState { ... }

// 4. spawn 
pub fn spawn_xxx_actor() -> ActorHandle<XxxMsg> {
    spawn_actor(XxxState::new(), |state, msg| Box::pin(async move {
        match msg { ... }
    }))
}
```

 5 

|  |  |  |  |
|------|------|----------|--------|
| event | `src/event/mod.rs` | `EventBusMsg::{On, Off, Emit, PatternCount}` | `Emit`  handler  actor  |
| schedule | `src/schedule/mod.rs` | `SchedulerMsg::{SetPersistPath, Add, List, Remove, Tick, Count}` | `Add`  `at_epoch`  |
| ccr | `src/ccr/mod.rs` | `CcrStoreMsg::{Put, Get, Len}` | 3  |
| mock | `src/mock/mod.rs` | `MockRegistryMsg::{Register, Unregister, Get, Count, Names}` |  `MockHandler`  `Clone`actor  clone  |
| trace_collector | `src/trace_collector.rs` | `TraceCollectorMsg::{SetEnabled, IsEnabled, StartSpan, EndSpan, RecordTokens, RecordCall, GetMetrics}` |  `impl Default for TraceCollectorState`clippy `new_without_default` |

 1  `#[tokio::test]` actor 

### 5.3 Interpreter ""

`Interpreter` 5  `Arc<Mutex<...>>`  `ActorHandle<...>` 16  `dispatch.rs` / `builtins.rs`  `call_*_method`  `await`

**** `new()` / `Clone`  actor CHANGELOG " v0.35"

### 5.4 line B

```
cargo build --all-targets                                  → clean
cargo test --lib                                          → 341 passed
cargo clippy --all-targets --all-features -- -D warnings  → clean
cargo fmt --check                                         → 0 diff
```

---

## 6. 

### 6.1 

```
main ()
 fix/v0.34-production-panics ( commit)
    d891326  fix(v0.34): replace production panics with Result/error tokens
    b374975  fix(v0.34): eliminate remaining interpreter panic paths and add tests
 feat/v0.34-actor-pressure ( commit)
     8e975a6  feat(v0.34): actor + pressure infrastructure and 5 domain actor pilots
     540f72f  docs(v0.34): clarify that Interpreter field swap is deferred to v0.35
     ffa6ff6  docs(v0.34):  v0.34 
```

 `git checkout main && git merge fix/v0.34-production-panics`  fast-forward

`feat/v0.34-actor-pressure` ** main**——"v0.34  2 " v0.35  async migration 

### 6.2 

 `git log --oneline | head`

- `fix(v0.34): <>`
- `feat(v0.34): <>`
- `docs(v0.34): <>`

**** panicking "v0.34 "——panic  bug fix  feature

### 6.3 CHANGELOG.md 

- `fix/v0.34-production-panics`  v0.34 "Fix Production Panics on User-Input Paths"  + 
- `feat/v0.34-actor-pressure`  v0.34 "actor/pressure 5  actor ""Interpreter  v0.35"

---

## 7.  v0.35 

**** v0.35  commit 

### 7.1  builtin dispatch  async



1. `interpreter::interpret`  `async fn`
2. `interpreter::execute_*` `execute_let` / `execute_assign` / `execute_for` / ... `async fn`
3. `interpreter::evaluate_*` `evaluate` / `evaluate_call` / `evaluate_pipe` / `evaluate_method_call` / ... `async fn`
4. `interpreter::call_function` / `call_method` / `call_value*`  `async fn`
5. 5  `call_*_method`  `actor.ask(...).await`
6. `Interpreter::new`  `Clone` actor handle  cheap clone spawn 
7. `run_file` / `run_repl`  async 
8.  330+  `#[tokio::main]`  `tokio::runtime::Runtime` 

### 7.2  3 `PressureControl`  AI/Web 

`real_ai_chat` / `real_web_fetch` / `call_ai_api` / `real_ai_chat_with_tools` / `run_agent` / `run_critic` / `batch_chat`  `PressureControl::call("ai:default", 5, 60, || async { ... })`

### 7.3  4 async

- `src/interpreter/ai_chat.rs`  `ureq::post/get`  `reqwest`
- `src/http_server.rs`  `tokio::net::TcpListener`
- `src/mcp_server.rs`  `tokio::sync::mpsc` + `tokio::io::AsyncBufRead/AsyncWrite`
- `src/lsp/server.rs`  `tokio::select!`  transport`transport.rs`  async

### 7.4 v0.35  ureq

7.3  `Cargo.toml`  `ureq` 

---

## 8. 



```bash
# 1.  v0.34 
git checkout main

# 2.  A panic 
git checkout -b fix/v0.34-production-panics
#  src/lexer.rs, src/flow.rs, src/lsp/providers/formatting.rs,
# src/interpreter/mod.rs (extract_embeddings), src/parser_v2/statements.rs
git add -A && git commit -m "fix(v0.34): replace production panics with Result/error tokens"
#  §4.2  10 
git add -A && git commit -m "fix(v0.34): eliminate remaining interpreter panic paths and add tests"
#  main
git checkout main && git merge fix/v0.34-production-panics  # fast-forward

# 3.  B actor/pressure 
git checkout -b feat/v0.34-actor-pressure
#  §5  actor.rs, pressure.rs,  5 , 
git add -A && git commit -m "feat(v0.34): actor + pressure infrastructure and 5 domain actor pilots"
# 
git add -A && git commit -m "docs(v0.34): clarify that Interpreter field swap is deferred to v0.35"
# CHANGELOG 
git add -A && git commit -m "docs(v0.34):  v0.34 "

# 4.  commit 
cargo fmt && cargo build --all-targets && \
  cargo clippy --all-targets --all-features -- -D warnings && \
  cargo test --all
```

---

## 9. 

### 9.1 

|  | line A  | line B  |
|------|------------|------------|
| `Cargo.toml` | — |  `tokio` + `reqwest` |
| `src/main.rs` | — | `#[tokio::main] async fn main` |
| `src/bin/lsp.rs` | — | `#[tokio::main] async fn main` |
| `src/lexer.rs` | `parse().unwrap()` → `match` + `error_token` | — |
| `src/flow.rs` | `unreachable!()` → `Err(...)` | — |
| `src/lsp/providers/formatting.rs` | `.expect(...)` → `match`  | — |
| `src/lsp/server.rs` |  9  `handle_*`  `Result``docs`/`shutdown` mutex  `?` | — |
| `src/interpreter/evaluate.rs` |  `?`irrefutable `unwrap`  | — |
| `src/interpreter/execute.rs` |  `?` | — |
| `src/interpreter/dispatch.rs` |  mutex  `?` | — |
| `src/interpreter/trait_dispatch.rs` |  `?` | — |
| `src/interpreter/orchestrate.rs` |  `?` `if let Ok(env)` | — |
| `src/interpreter/mod.rs` | `extract_embeddings`  `Err``new()`  `expect("globals mutex poisoned")``interpret()`  `?` | — |
| `src/parser_v2/statements.rs` | `eval`  `given:`  fallback `NodeId(0)` | — |
| `tests/parser_v2_integration.rs` |  `test_parse_eval_without_given_no_panic` | — |
| `src/event/mod.rs` | — |  actor  |
| `src/schedule/mod.rs` | — |  actor  |
| `src/ccr/mod.rs` | — |  actor  |
| `src/mock/mod.rs` | — |  actor  |
| `src/trace_collector.rs` | — |  actor  |
| `src/actor.rs` | — | **** |
| `src/pressure.rs` | — | **** |
| `src/lib.rs` | — |  `actor` / `pressure`  |
| `CHANGELOG.md` |  "Fix Production Panics on User-Input Paths" |  "" |

### 9.2 

```bash
# panic 
grep -Rnw --include="*.rs" src/ -e "panic!" -e "\.unwrap()" -e "\.expect(" -e "unreachable!"

#  expect
grep -n "\.expect(" src/interpreter/execute.rs

# 
cargo fmt && cargo build --all-targets && \
  cargo clippy --all-targets --all-features -- -D warnings && \
  cargo test --all

# 
git log --oneline main..HEAD
git diff --stat
```

### 9.3 

```
fix(v0.34): replace production panics with Result/error tokens
fix(v0.34): eliminate remaining interpreter panic paths and add tests
feat(v0.34): actor + pressure infrastructure and 5 domain actor pilots
docs(v0.34): clarify that Interpreter field swap is deferred to v0.35
docs(v0.34):  v0.34 
```

 `git log``fix(v0.34): mock.register/unregister actually wire handlers`  `tag(v0.x): <>` 

---

## 10. 

"v0.34  panic"****

1. **""** commit  panic ""/
2. **""**actor/pressure  5  pilotcargo test 341 passed Interpreter
3. **""**Interpreter  16  + 
4. **""** commit `fix/feat/docs` CHANGELOG 
5. **""** dispatch  200+  + 330+  session" + 5  pilot""Interpreter " v0.35 



1.  `cargo test --all` 341 
2.  `src/actor.rs`  `src/pressure.rs` tokio actor + 
3.  `docs/workflow-v0.24-parser-migration.md`

Mora v0.34 ""****AGENTS.md / CHANGELOG.md / docs/ 
