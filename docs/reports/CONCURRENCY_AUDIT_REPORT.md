# mora-lang /

> 2026-06-25
> mora-lang v0.49.0  `src/` 
> /

---

## 

|  |  |  |
|------|------|------|
|  **P0 — Critical** | 12 |  |
| 🟠 **P1 — High** | 13 |  |
| 🟡 **P2 — Medium** | 14 |  |

---

##  P0 — Critical

### P0-1. CapabilityStore.revoke() — 

|  |  |
|------|------|
| **** | `src/sandbox/capability.rs` |
| **** | 286–296 (`revoke`) + 260–281 (`check`) |
| **** | `revoke()`  `current_generation` +1 ** token  `issue()`  generation  `0`** revoke  token token  |
| **** |  |
| **** | per-token `revoked`  + `DashMap<u64, TokenEntry>` |

```rust
// 
use dashmap::DashMap;
use std::sync::atomic::AtomicBool;

pub struct TokenEntry {
    pub token: CapabilityToken,
    pub revoked: AtomicBool,
}

// revoke:  AtomicBool = true
// check:  token  revoked 
```

---

### P0-2. InMemoryCcrStore::Clone —  hash 

|  |  |
|------|------|
| **** | `src/ccr/mod.rs` |
| **** | 56–63 |
| **** | Clone  `entries` (Arc clone)  `counter` **** `AtomicU64` Clone  `put()`  hash `insert`  |
| **** | CCR  |
| **** | `counter`  `Arc<AtomicU64>` |

```rust
pub struct InMemoryCcrStore {
    entries: Arc<Mutex<HashMap<String, CcrEntry>>>,
    counter: Arc<AtomicU64>,  // 
}
```

---

### P0-3. HTTP Server — Interpreter 

|  |  |
|------|------|
| **** | `src/http_server.rs` |
| **** | 167 (), 316 () |
| **** | `interpreter: Arc<Mutex<Interpreter>>` — AI  2-30  HTTP  |
| **** |  ≈ **1** |
| **** | Interpreter  `SharedState`(Arc) + `PerRequestState` |

```rust
pub struct SharedState {
    pub trait_registry: Arc<HashMap<String, TraitInfo>>,
    pub tool_registry: Arc<HashMap<String, ToolDef>>,
    pub model_routes: Arc<HashMap<String, RouteConfig>>,
    // ... /
}

// HTTP  Clone  Interpreter
let interp = Interpreter::new_with_shared(shared_state.clone());
interp.call_value(&handler, args)?; // 
```

---

### P0-4. HTTP Server — 

|  |  |
|------|------|
| **** | `src/http_server.rs` |
| **** | 203–218 (), 412–457 () |
| **** | `TcpStream`  `set_read_timeout`/`set_write_timeout``recv()`  handler `parse_request`  `read_line`/`read_exact`  |
| **** | Slowloris LLM API  |
| **** |  + Handler  |

```rust
stream.set_read_timeout(Some(Duration::from_secs(30)))?;
stream.set_write_timeout(Some(Duration::from_secs(30)))?;

// Handler 
crossbeam_channel::bounded(1);
match timeout_rx.recv_timeout(Duration::from_secs(60)) {
    Ok(result) => result,
    Err(_) => send_response(stream, 504, "Gateway Timeout"),
}
```

---

### P0-5. HTTP Server — mpsc + Arc<Mutex<Receiver>> 

|  |  |
|------|------|
| **** | `src/http_server.rs` |
| **** | 195–219 |
| **** | `std::sync::mpsc::Receiver`  `Sync` `Arc<Mutex<Receiver>>`  Mutex |
| **** | QPS > 1000  |
| **** |  crossbeam-channel|

```rust
use crossbeam_channel::unbounded;

let (tx, rx) = unbounded::<TcpStream>();
for _ in 0..pool_size {
    let rx = rx.clone(); // 
    std::thread::spawn(move || {
        while let Ok(stream) = rx.recv() { /* ... */ }
    });
}
```

---

### P0-6. Environment parent 

|  |  |
|------|------|
| **** | `src/value.rs` |
| **** | 460 (), 501–584 (`get`/`assign`/`get_binding`/`move_variable`/`borrow_variable*`) |
| **** |  N  N  Mutex `std::sync::Mutex` + `.expect()` poison  |
| **** | 20  = 20  futex panic  |
| **** |  COW  |

```rust
//  A: 
pub struct Environment {
    pub values: HashMap<String, Arc<RwLock<Value>>>,
    pub flattened_parent: Option<Arc<HashMap<String, Arc<RwLock<Value>>>>>, // 
}

//  B: COW 
use im::HashMap;
pub struct Environment {
    pub values: Arc<HashMap<String, Arc<RwLock<Value>>>>,
    pub parent: Option<Arc<Environment>>,
}
```

---

### P0-7. borrow_variable / borrow_variable_mut — 

|  |  |
|------|------|
| **** | `src/value.rs` |
| **** | 559–570 (), 573–584 () |
| **** | "" ****`Arc::new(Mutex::new(value.clone()))` `Arc<Mutex<Value>>` **** |
| **** | O(n²) |
| **** | Environment  `Arc<RwLock<Value>>` `Arc::clone` |

```rust
use parking_lot::RwLock;

pub struct Environment {
    pub values: HashMap<String, Arc<RwLock<Value>>>,
    pub parent: Option<Arc<Environment>>,
}

pub fn borrow_variable(&self, name: &str) -> Result<Arc<RwLock<Value>>, String> {
    self.values.get(name)
        .map(Arc::clone)  // O(1) 
        .or_else(|| self.parent?.borrow_variable(name).ok())
        .ok_or_else(|| format!("undefined variable: {}", name))
}
```

---

### P0-8. LruCache —  LRU + 

|  |  |
|------|------|
| **** | `src/interpreter/mod.rs` |
| **** | 140–186 (), 209/212 () |
| **** | ****① `get()`  FIFO  LRU② `Arc<Mutex<LruCache>>`  |
| **** | AI  API |
| **** |  `moka` crate  LRU |

```rust
//  A: moka
use moka::sync::Cache;

let ai_cache: Cache<String, String> = Cache::builder()
    .max_capacity(10_000)
    .time_to_live(Duration::from_secs(3600))
    .build();

//  B: 
pub fn get(&mut self, key: &str) -> Option<&V> {
    if self.map.contains_key(key) {
        let owned = key.to_string();
        self.order.retain(|k| k != &owned);
        self.order.push_back(owned);
    }
    self.map.get(key)
}
```

---

### P0-9. string_interner — --

|  |  |
|------|------|
| **** | `src/interpreter/mod.rs` |
| **** | 636–653 |
| **** |  Check-Then-Act `lock()`  `lock()`  lock  |
| **** |  |
| **** |  |

```rust
pub fn intern_string(&self, s: String) -> Value {
    let mut map = self.string_interner.lock();
    if let Some(interned) = map.get(&s) {
        return interned.clone();
    }
    let val = Value::String(s.clone());
    map.put(s, val.clone());
    val
}
```

---

### P0-10. EventBus — emit() 

|  |  |
|------|------|
| **** | `src/event/mod.rs` |
| **** | 115–164 |
| **** | `emit()`  `exact.lock() → prefix.lock() → interior.lock()`emit  `Mutex` emit  |
| **** |  |
| **** | `Mutex` → `RwLock`interior  `ArcSwap`  snapshot |

```rust
exact: Arc<RwLock<HashMap<Pattern, Vec<Handler>>>>,
prefix: Arc<RwLock<HashMap<Pattern, Vec<Handler>>>>,
interior: Arc<ArcSwap<HashMap<Pattern, Vec<Handler>>>>, // 
```

---

### P0-11. CapabilityStore —  Mutex  token 

|  |  |
|------|------|
| **** | `src/sandbox/capability.rs` |
| **** | 215–314 |
| **** |  `issue/get/check/revoke`  `Mutex<CapabilityStoreInner>`check  |
| **** |  agent 100+  |
| **** | `DashMap<u64, CapabilityToken>`  `Mutex<BTreeMap>` |

```rust
use dashmap::DashMap;

pub struct CapabilityStore {
    tokens: Arc<DashMap<u64, CapabilityToken>>,
    next_id: AtomicU64,
}
// issue: tokens.insert(id, token) — 
// check: tokens.get(&id) — shard-level 
// revoke: tokens.remove(&id)  revoked 
```

---

### P0-12. Semaphore stress_tests.rs 

|  |  |
|------|------|
| **** | `src/stress_tests.rs` |
| **** | 170–225 |
| **** | `loop { ... compare_exchange ...; std::thread::yield_now() }``yield_now()`  CPU 990  |
| **** | CPU  |
| **** | `Condvar`  `tokio::sync::Semaphore` |

```rust
use std::sync::{Mutex, Condvar};

struct Sem {
    inner: Mutex<usize>,
    cvar: Condvar,
}

fn acquire(&self) {
    let mut permits = self.inner.lock().unwrap();
    while *permits == 0 {
        permits = self.cvar.wait(permits).unwrap();
    }
    *permits -= 1;
}

fn release(&self) {
    let mut permits = self.inner.lock().unwrap();
    *permits += 1;
    self.cvar.notify_one();
}
```

---

## 🟠 P1 — High

### P1-1. RouteTable  Mutex  RwLock

|  |  |
|------|------|
| **** | `src/http_server.rs` |
| **** | 33 (), 249 () |
| **** |  `Mutex`  |
| **** | `Arc<RwLock<HashMap<...>>>`  `Arc<DashMap<...>>` |

### P1-2. HTTP DoS 

|  |  |
|------|------|
| **** | `src/http_server.rs` |
| **** | 412–457 |
| **** | `content_length` header header  |
| **** |  Content-Length  OOM |
| **** |  |

```rust
const MAX_BODY_SIZE: usize = 10 * 1024 * 1024;  // 10 MB
const MAX_HEADERS: usize = 100;
const MAX_HEADER_LINE: usize = 8 * 1024;         // 8 KB
```

### P1-3. Interpreter::clone 

|  |  |
|------|------|
| **** | `src/interpreter/mod.rs` |
| **** | 281–324 |
| **** | `worker_channels` / `worker_receivers` / `memory_store` / `context_window` / `speculative_verifier` / `v2_arena`  |
| **** | HTTP worker  Worker AI  |
| **** |  `SharedState`  `Arc<...>`  |

### P1-4. execute_parallel 

|  |  |
|------|------|
| **** | `src/interpreter/execute.rs` |
| **** | 406–423 |
| **** | "" `parallel { ... }`  |
| **** | `rayon::iter::ParallelIterator`  |

### P1-5. call_value_inner 

|  |  |
|------|------|
| **** | `src/interpreter/dispatch.rs` |
| **** | 1205–1208 |
| **** |  |
| **** |  `Arc<Environment>`  |

### P1-6. Arc::make_mut 

|  |  |
|------|------|
| **** | `src/interpreter/execute.rs` |
| **** | 591, 633 |
| **** |  >1 `Arc::make_mut`  `HashMap` |
| **** | `Arc<RwLock<HashMap>>`  `Arc<HashMap>` + `make_mut` |

### P1-7. hex_encode 

|  |  |
|------|------|
| **** | `src/flow.rs` |
| **** | 37–39 |
| **** | `bytes.iter().map(|b| format!("{:02x}", b)).collect()` — 1MB  = 100 |
| **** |  +  |

```rust
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}
```

### P1-8. retry_sleep_ms —  jitter ""

|  |  |
|------|------|
| **** | `src/interpreter/mod.rs` |
| **** | 91–99 |
| **** | `subsec_nanos()`  jitter  |
| **** | 100  |
| **** |  `rand` crate  jitter |

```rust
use rand::Rng;

fn retry_sleep_ms(attempt: u32, base_ms: u64) -> u64 {
    let exp = base_ms.saturating_mul(1u64 << attempt.min(6)); //  64x
    let jitter_max = (exp / 2).max(base_ms);
    let jitter = rand::thread_rng().gen_range(0..=jitter_max);
    exp + jitter
}
```

### P1-9. ContextWindow — Vec::remove(0)  O(n) 

|  |  |
|------|------|
| **** | `src/ai_infra.rs` |
| **** | 86–94 |
| **** | `Vec::remove(0)`  O(n²) |
| **** | `VecDeque`  `Vec` |

```rust
use std::collections::VecDeque;

pub struct ContextWindow {
    pub messages: VecDeque<(String, String)>,
    // ...
}

// add_message 
let removed = self.messages.pop_front(); // O(1)
```

### P1-10. Scheduler — tick()  + 

|  |  |
|------|------|
| **** | `src/schedule/mod.rs` |
| **** | 176–208 (tick), 216–248 (save) |
| **** | `tick()`  `jobs.lock()`  job`save()`  JSON  |
| **** | O(N)  |
| **** |  (Hierarchical Timing Wheel) +  |

### P1-11. InMemoryCcrStore —  Mutex 

|  |  |
|------|------|
| **** | `src/ccr/mod.rs` |
| **** | 71–101 |
| **** | `Arc<Mutex<HashMap>>`  put/get/lenHashMap  |
| **** | `RwLock<HashMap>` `DashMap`|

### P1-12. exec_with_timeout — 

|  |  |
|------|------|
| **** | `src/sandbox/container.rs` |
| **** | 251–304 |
| **** |  OS `waiter.join()` PID  |
| **** | `tokio::process::Command`  `wait-timeout` crate |

### P1-13. SandboxPolicy check_builtin() — BTreeSet 

|  |  |
|------|------|
| **** | `src/sandbox/mod.rs` |
| **** | 83–106 |
| **** |  `deny`/`allow` BTreeSetO(N) pattern  `matches()` split +  |
| **** | Trie/Prefix Tree   `regex::RegexSet` |

---

## 🟡 P2 — Medium / 

### P2-1.  `Connection: close`

|  |  |
|------|------|
| **** | `src/http_server.rs` |
| **** | 468 |
| **** |  `Connection: close` QPS  TCP  |
| **** |  HTTP/1.1 Keep-Alive |

### P2-2. /

|  |  |
|------|------|
| **** | `src/http_server.rs` |
| **** | 222–231 |
| **** | `listener.incoming()`  |
| **** | `Semaphore`  |

### P2-3. Mutex Poisoning panic 8+ 

|  |  |
|------|------|
| **** | `value.rs`, `interpreter/mod.rs`, `http_server.rs`, `event/mod.rs`  |
| **** | `.lock().expect("...mutex poisoned")` —  |
| **** |  `parking_lot::Mutex` poison|

```rust
// Cargo.toml
// parking_lot = "1.1"

use parking_lot::Mutex;
let guard = self.environment.lock(); //  Guard unwrap
```

### P2-4. StreamReader —  +  MutexGuard

|  |  |
|------|------|
| **** | `src/value.rs` |
| **** | 15–29 |
| **** | `BufReader`  `std::sync::Mutex` I/O  |
| **** | `tokio::sync::Mutex` `parking_lot::Mutex`|

### P2-5. Value::List / Dict 

|  |  |
|------|------|
| **** | `src/value.rs` |
| **** | 155–156 |
| **** | `Vec`  `HashMap`  resize  |
| **** | `Dict` → `Arc<DashMap<String, Value>>``List` → `Arc<RwLock<Vec<Value>>>` |

### P2-6. McpServer / Conversation / Agent  Vec

|  |  |
|------|------|
| **** | `src/value.rs` |
| **** | 175, 188, 215 |
| **** |  `Vec`  push  resize  |
| **** |  `Arc<RwLock<Vec<...>>>` |

### P2-7. Value::clone() 

|  |  |
|------|------|
| **** | `value.rs`  |
| **** | `get()`/`define()`/`get_binding()`  |
| **** | Environment  `Arc<Value>``get()`  `Arc::clone`O(1)|

### P2-8. RetryPolicy  jitter

|  |  |
|------|------|
| **** | `src/ai_infra.rs` |
| **** | 749–756 |
| **** |  jitter `#[allow(dead_code)]` |
| **** |  P1-8 jitter |

### P2-9. ContainerHandle Drop —  docker rm

|  |  |
|------|------|
| **** | `src/sandbox/container.rs` |
| **** | 232–244 |
| **** | `Drop::drop()`  `Command::new("docker").status()` |
| **** |  |

```rust
lazy_static! {
    static ref CLEANUP_QUEUE: Sender<String> = spawn_cleanup_thread();
}

impl Drop for ContainerHandle {
    fn drop(&mut self) {
        if self.auto_cleanup {
            let _ = CLEANUP_QUEUE.send(self.container_id.clone());
        }
    }
}
```

### P2-10. CapabilityStore  token 

|  |  |
|------|------|
| **** | `src/sandbox/capability.rs` |
| **** | `issue()`  TTL token  `by_id`  |
| **** |  `moka`  |

### P2-11. LruCache put —  key  order

|  |  |
|------|------|
| **** | `src/interpreter/mod.rs` |
| **** | 162–165 |
| **** | "" order key  |
| **** | `put()`  key  `order`  |

### P2-12. hex_encode 

|  |  |
|------|------|
| **** | `src/flow.rs:37-39` vs `src/audit/mod.rs:133-139` |
| **** |  `flow.rs`  |
| **** | `audit/mod.rs`  `use crate::flow::hex_encode;` |

### P2-13. EventBus Debug / pattern_count 

|  |  |
|------|------|
| **** | `src/event/mod.rs` |
| **** | 52–63, 192–197 |
| **** |  |
| **** |  |

### P2-14. AI 

|  |  |
|------|------|
| **** | `src/ai_infra.rs`  |
| **** | `LoadBalancer``ModelSwitcher``SmartCacheEviction``CostOptimizer`  |
| **** | `LoadBalancer`  `AtomicUsize`  round-robinpricing  `Arc<HashMap>`|

---

## 

|  |  |  |  |  crate |
|--------|------|------|-----------|-----------|
| **** | CapabilityStore revoke  | `sandbox/capability.rs` |  | `dashmap` |
| **** | CcrStore Clone  | `ccr/mod.rs` |  | — |
| **** | HTTP  interpreter  | `http_server.rs` |  |  |
| **** | HTTP  +  | `http_server.rs` |  | — |
| **** | mpsc Arc<Mutex<Receiver>> | `http_server.rs` |  | `crossbeam-channel` |
| **** | Environment  | `value.rs` |  | `parking_lot`, `im` |
| **** | borrow_variable  | `value.rs` |  | `parking_lot` |
| **** | LruCache  LRU +  | `interpreter/mod.rs` |  | `moka`  |
| **** | string_interner  | `interpreter/mod.rs` |  | — |
| **** | EventBus  | `event/mod.rs` |  | `parking_lot` |
| **** | CapabilityStore  | `sandbox/capability.rs` |  | `dashmap` |
| **** | hex_encode  | `flow.rs` |  | — |
| **** | retry jitter | `interpreter/mod.rs` |  | `rand` |
| **** | ContextWindow VecDeque | `ai_infra.rs` |  | — |
| **** | execute_parallel  | `interpreter/execute.rs` |  | `rayon` |
| **** | Scheduler  | `schedule/mod.rs` |  | `delay-queue` |
| **** | HTTP Keep-Alive | `http_server.rs` |  | — |
| **** |  Mutex → parking_lot |  |  | `parking_lot` |
| **** |  tokio  |  |  | `tokio`, `axum` |

---

## 

| Crate |  |  |
|-------|------|---------|
| `parking_lot` |  poison Mutex/RwLock |  `std::sync::Mutex` |
| `dashmap` |  HashMap | `Arc<Mutex<HashMap>>`  |
| `moka` | Segmented LRU | `LruCache` |
| `rayon` |  | `execute_parallel` |
| `tokio` |  |  HTTP/LSP  |
| `rand` |  | `retry_sleep_ms` jitter |
| `hex` | SIMD  hex  | `hex_encode` |
| `delay-queue` |  | `Scheduler` |
| `crossbeam-channel` |  channel | `std::sync::mpsc` |

---

*" →  →  →  → " `cargo test -- --ignored`  stress tests*
