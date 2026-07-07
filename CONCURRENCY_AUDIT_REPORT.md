# mora-lang 高并发/高压力场景升级审计报告

> 生成时间：2026-06-25
> 审计范围：mora-lang v0.49.0 全项目 `src/` 目录
> 审计维度：线程模型、锁竞争、超时/限流、资源耗尽防护、缓存语义、数据结构并发安全

---

## 严重级别总览

| 级别 | 数量 | 说明 |
|------|------|------|
| 🔴 **P0 — Critical** | 12 | 功能缺陷、级联崩溃、吞吐量归零 |
| 🟠 **P1 — High** | 13 | 严重性能瓶颈、设计级缺陷 |
| 🟡 **P2 — Medium** | 14 | 性能退化、可靠性隐患、设计债 |

---

## 🔴 P0 — Critical（生产环境不可用或功能缺陷）

### P0-1. CapabilityStore.revoke() — 撤销一个，全体失效

| 属性 | 详情 |
|------|------|
| **文件** | `src/sandbox/capability.rs` |
| **行号** | 286–296 (`revoke`) + 260–281 (`check`) |
| **问题** | `revoke()` 将全局 `current_generation` +1；但 **所有 token 在 `issue()` 时 generation 都硬编码为 `0`**。一旦 revoke 任意 token，所有已发放 token 立即全部失效。 |
| **风险** | 沙箱策略崩溃，合法授权全部中断 |
| **修复** | per-token `revoked` 标志位 + `DashMap<u64, TokenEntry>` |

```rust
// 工业级方案
use dashmap::DashMap;
use std::sync::atomic::AtomicBool;

pub struct TokenEntry {
    pub token: CapabilityToken,
    pub revoked: AtomicBool,
}

// revoke: 只设置 AtomicBool = true
// check: 读取该 token 的 revoked 标志
```

---

### P0-2. InMemoryCcrStore::Clone — 半共享语义导致 hash 冲突

| 属性 | 详情 |
|------|------|
| **文件** | `src/ccr/mod.rs` |
| **行号** | 56–63 |
| **问题** | Clone 共享 `entries` (Arc clone) 但复制 `counter` 瞬时值到**新的独立** `AtomicU64`。两个 Clone 实例并发 `put()` 时产生完全相同的 hash，第二次 `insert` 覆盖第一次数据。 |
| **风险** | CCR 数据静默丢失 |
| **修复** | `counter` 也走 `Arc<AtomicU64>` |

```rust
pub struct InMemoryCcrStore {
    entries: Arc<Mutex<HashMap<String, CcrEntry>>>,
    counter: Arc<AtomicU64>,  // 共享，非独立
}
```

---

### P0-3. HTTP Server — Interpreter 全局锁导致请求完全串行化

| 属性 | 详情 |
|------|------|
| **文件** | `src/http_server.rs` |
| **行号** | 167 (参数声明), 316 (实际获取) |
| **问题** | `interpreter: Arc<Mutex<Interpreter>>` — 任意时刻只有一个请求能进入解释器执行。AI 调用可能耗时 2-30 秒，此期间整个 HTTP 服务器冻结。 |
| **风险** | 无论线程池多大，实际并发处理能力 ≈ **1** |
| **修复** | Interpreter 拆分为 `SharedState`(Arc) + `PerRequestState` |

```rust
pub struct SharedState {
    pub trait_registry: Arc<HashMap<String, TraitInfo>>,
    pub tool_registry: Arc<HashMap<String, ToolDef>>,
    pub model_routes: Arc<HashMap<String, RouteConfig>>,
    // ... 其他只读/共享可变数据
}

// HTTP 处理时：每个请求 Clone 一个独立 Interpreter，但共享只读配置
let interp = Interpreter::new_with_shared(shared_state.clone());
interp.call_value(&handler, args)?; // 无锁执行
```

---

### P0-4. HTTP Server — 完全缺失超时机制

| 属性 | 详情 |
|------|------|
| **文件** | `src/http_server.rs` |
| **行号** | 203–218 (工作线程), 412–457 (请求解析) |
| **问题** | `TcpStream` 无 `set_read_timeout`/`set_write_timeout`；`recv()` 和 handler 执行无限阻塞；`parse_request` 中 `read_line`/`read_exact` 永远等待慢客户端 |
| **风险** | Slowloris 攻击、半开连接耗尽线程池、LLM API 挂死饿死所有请求 |
| **修复** | 连接级 + Handler 级双重超时 |

```rust
stream.set_read_timeout(Some(Duration::from_secs(30)))?;
stream.set_write_timeout(Some(Duration::from_secs(30)))?;

// Handler 级超时
crossbeam_channel::bounded(1);
match timeout_rx.recv_timeout(Duration::from_secs(60)) {
    Ok(result) => result,
    Err(_) => send_response(stream, 504, "Gateway Timeout"),
}
```

---

### P0-5. HTTP Server — mpsc + Arc<Mutex<Receiver>> 竞争瓶颈

| 属性 | 详情 |
|------|------|
| **文件** | `src/http_server.rs` |
| **行号** | 195–219 |
| **问题** | `std::sync::mpsc::Receiver` 不是 `Sync`，强行用 `Arc<Mutex<Receiver>>` 共享。每次分发连接所有工作线程竞争同一个 Mutex。 |
| **风险** | QPS > 1000 时锁竞争显著；工作线程持有锁休眠，其他线程全部阻塞 |
| **修复** | 使用 crossbeam-channel（项目已依赖）|

```rust
use crossbeam_channel::unbounded;

let (tx, rx) = unbounded::<TcpStream>();
for _ in 0..pool_size {
    let rx = rx.clone(); // 无锁多消费者
    std::thread::spawn(move || {
        while let Ok(stream) = rx.recv() { /* ... */ }
    });
}
```

---

### P0-6. Environment parent 递归锁链

| 属性 | 详情 |
|------|------|
| **文件** | `src/value.rs` |
| **行号** | 460 (定义), 501–584 (`get`/`assign`/`get_binding`/`move_variable`/`borrow_variable*`) |
| **问题** | 变量查找逐层向上加锁，深度 N 时需获取 N 个 Mutex。使用 `std::sync::Mutex` + `.expect()`，级联 poison 崩溃。 |
| **风险** | 20 层嵌套 = 20 次 futex；一个线程 panic 导致全局解释器崩溃 |
| **修复** | 扁平化环境或 COW 不可变环境 |

```rust
// 方案 A: 扁平化（最彻底）
pub struct Environment {
    pub values: HashMap<String, Arc<RwLock<Value>>>,
    pub flattened_parent: Option<Arc<HashMap<String, Arc<RwLock<Value>>>>>, // 不可变快照
}

// 方案 B: COW 不可变（函数式风格）
use im::HashMap;
pub struct Environment {
    pub values: Arc<HashMap<String, Arc<RwLock<Value>>>>,
    pub parent: Option<Arc<Environment>>,
}
```

---

### P0-7. borrow_variable / borrow_variable_mut — 语义完全错误

| 属性 | 详情 |
|------|------|
| **文件** | `src/value.rs` |
| **行号** | 559–570 (不可变借用), 573–584 (可变借用) |
| **问题** | 函数名宣称"借用"，但行为是 **克隆包装**：`Arc::new(Mutex::new(value.clone()))`。返回的 `Arc<Mutex<Value>>` 与原始变量**无任何关联**。 |
| **风险** | 内存爆炸（O(n²) 深拷贝）；并发修改不可见（独立副本）|
| **修复** | Environment 存储值本身为 `Arc<RwLock<Value>>`，借用返回 `Arc::clone` |

```rust
use parking_lot::RwLock;

pub struct Environment {
    pub values: HashMap<String, Arc<RwLock<Value>>>,
    pub parent: Option<Arc<Environment>>,
}

pub fn borrow_variable(&self, name: &str) -> Result<Arc<RwLock<Value>>, String> {
    self.values.get(name)
        .map(Arc::clone)  // O(1) 真正共享引用
        .or_else(|| self.parent?.borrow_variable(name).ok())
        .ok_or_else(|| format!("undefined variable: {}", name))
}
```

---

### P0-8. LruCache — 伪 LRU + 粗粒度锁

| 属性 | 详情 |
|------|------|
| **文件** | `src/interpreter/mod.rs` |
| **行号** | 140–186 (定义), 209/212 (使用) |
| **问题** | **双重缺陷**：① `get()` 完全不更新访问顺序，实际为 FIFO 而非 LRU；② `Arc<Mutex<LruCache>>` 导致所有缓存操作串行。 |
| **风险** | 缓存命中率暴跌；热点数据被误驱逐；AI 请求雪崩穿透到后端 API |
| **修复** | 引入 `moka` crate 或手写真 LRU |

```rust
// 方案 A: moka（推荐，工业级）
use moka::sync::Cache;

let ai_cache: Cache<String, String> = Cache::builder()
    .max_capacity(10_000)
    .time_to_live(Duration::from_secs(3600))
    .build();

// 方案 B: 最小修复（若坚持手写）
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

### P0-9. string_interner — 检查-然后-插入竞态窗口

| 属性 | 详情 |
|------|------|
| **文件** | `src/interpreter/mod.rs` |
| **行号** | 636–653 |
| **问题** | 经典 Check-Then-Act：先 `lock()` 检查，释放锁，再 `lock()` 插入。两次 lock 之间其他线程可能插入相同字符串。 |
| **风险** | 重复驻留、错误淘汰、锁竞争加剧 |
| **修复** | 单次锁内完成检查或插入 |

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

### P0-10. EventBus — emit() 三锁串行

| 属性 | 详情 |
|------|------|
| **文件** | `src/event/mod.rs` |
| **行号** | 115–164 |
| **问题** | `emit()` 按顺序获取 `exact.lock() → prefix.lock() → interior.lock()`。emit 是只读操作但用 `Mutex`，多个 emit 线程也全局串行。 |
| **风险** | 高频事件场景下吞吐量被锁竞争严重限制 |
| **修复** | `Mutex` → `RwLock`；interior 用 `ArcSwap` 实现无锁 snapshot |

```rust
exact: Arc<RwLock<HashMap<Pattern, Vec<Handler>>>>,
prefix: Arc<RwLock<HashMap<Pattern, Vec<Handler>>>>,
interior: Arc<ArcSwap<HashMap<Pattern, Vec<Handler>>>>, // 无锁读
```

---

### P0-11. CapabilityStore — 单 Mutex 串行所有 token 操作

| 属性 | 详情 |
|------|------|
| **文件** | `src/sandbox/capability.rs` |
| **行号** | 215–314 |
| **问题** | 所有 `issue/get/check/revoke` 竞争同一把 `Mutex<CapabilityStoreInner>`。check 是只读操作但无法并发。 |
| **风险** | 高 agent 并发场景（100+ 线程）下锁竞争急剧恶化 |
| **修复** | `DashMap<u64, CapabilityToken>` 替代 `Mutex<BTreeMap>` |

```rust
use dashmap::DashMap;

pub struct CapabilityStore {
    tokens: Arc<DashMap<u64, CapabilityToken>>,
    next_id: AtomicU64,
}
// issue: tokens.insert(id, token) — 无全局锁
// check: tokens.get(&id) — shard-level 并发
// revoke: tokens.remove(&id) 或设置 revoked 标志
```

---

### P0-12. Semaphore 自旋锁（stress_tests.rs 中的信号量）

| 属性 | 详情 |
|------|------|
| **文件** | `src/stress_tests.rs` |
| **行号** | 170–225 |
| **问题** | `loop { ... compare_exchange ...; std::thread::yield_now() }`。`yield_now()` 不会释放 CPU 时间片，990 个线程处于自旋循环。 |
| **风险** | CPU 打满；优先级反转；无超时死锁 |
| **修复** | `Condvar` 或 `tokio::sync::Semaphore` |

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

## 🟠 P1 — High（高负载下显著劣化）

### P1-1. RouteTable 使用 Mutex 而非 RwLock

| 属性 | 详情 |
|------|------|
| **文件** | `src/http_server.rs` |
| **行号** | 33 (定义), 249 (获取) |
| **问题** | 路由表读多写少，但用 `Mutex` 导致每次请求都独占访问。 |
| **修复** | `Arc<RwLock<HashMap<...>>>` 或 `Arc<DashMap<...>>` |

### P1-2. HTTP 解析无资源限制（DoS 向量）

| 属性 | 详情 |
|------|------|
| **文件** | `src/http_server.rs` |
| **行号** | 412–457 |
| **问题** | `content_length` 无上限；header 行无长度限制；header 数量无限制。 |
| **风险** | 超大 Content-Length 直接触发 OOM |
| **修复** | 加资源上限 |

```rust
const MAX_BODY_SIZE: usize = 10 * 1024 * 1024;  // 10 MB
const MAX_HEADERS: usize = 100;
const MAX_HEADER_LINE: usize = 8 * 1024;         // 8 KB
```

### P1-3. Interpreter::clone 丢失关键状态

| 属性 | 详情 |
|------|------|
| **文件** | `src/interpreter/mod.rs` |
| **行号** | 281–324 |
| **问题** | `worker_channels` / `worker_receivers` / `memory_store` / `context_window` / `speculative_verifier` / `v2_arena` 被重置为默认值。 |
| **风险** | HTTP worker 间 Worker 通信断裂、会话记忆丢失、AI 策略退化为冷启动 |
| **修复** | 抽取 `SharedState` 用 `Arc<...>` 共享 |

### P1-4. execute_parallel 伪并行（顺序执行）

| 属性 | 详情 |
|------|------|
| **文件** | `src/interpreter/execute.rs` |
| **行号** | 406–423 |
| **问题** | 注释明确承认"简化实现：顺序执行"。用户写 `parallel { ... }` 期望并发，实际顺序执行。 |
| **修复** | `rayon::iter::ParallelIterator` 或线程池 |

### P1-5. call_value_inner 闭包环境深拷贝

| 属性 | 详情 |
|------|------|
| **文件** | `src/interpreter/dispatch.rs` |
| **行号** | 1205–1208 |
| **问题** | 每次闭包调用深拷贝整个捕获环境，闭包内外修改互不可见。 |
| **修复** | 直接共享 `Arc<Environment>` 引用 |

### P1-6. Arc::make_mut 写放大

| 属性 | 详情 |
|------|------|
| **文件** | `src/interpreter/execute.rs` |
| **行号** | 591, 633 |
| **问题** | 引用计数 >1 时，`Arc::make_mut` 完整克隆整个 `HashMap`。 |
| **修复** | `Arc<RwLock<HashMap>>` 替代 `Arc<HashMap>` + `make_mut` |

### P1-7. hex_encode 大量小分配

| 属性 | 详情 |
|------|------|
| **文件** | `src/flow.rs` |
| **行号** | 37–39 |
| **问题** | `bytes.iter().map(|b| format!("{:02x}", b)).collect()` — 每个字节一次堆分配。1MB 文件 = 100万次分配。 |
| **修复** | 预分配 + 直接写入 |

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

### P1-8. retry_sleep_ms — 伪随机 jitter 与"雷群问题"

| 属性 | 详情 |
|------|------|
| **文件** | `src/interpreter/mod.rs` |
| **行号** | 91–99 |
| **问题** | `subsec_nanos()` 是时间戳而非随机数。批量重试时间高度同步。小基数下 jitter 完全消失。 |
| **风险** | 100 个客户端同步重试，周期性冲击服务端 |
| **修复** | 引入 `rand` crate 的真随机 jitter |

```rust
use rand::Rng;

fn retry_sleep_ms(attempt: u32, base_ms: u64) -> u64 {
    let exp = base_ms.saturating_mul(1u64 << attempt.min(6)); // 封顶 64x
    let jitter_max = (exp / 2).max(base_ms);
    let jitter = rand::thread_rng().gen_range(0..=jitter_max);
    exp + jitter
}
```

### P1-9. ContextWindow — Vec::remove(0) 的 O(n) 性能衰减

| 属性 | 详情 |
|------|------|
| **文件** | `src/ai_infra.rs` |
| **行号** | 86–94 |
| **问题** | `Vec::remove(0)` 需将所有后续元素前移。长会话下退化为 O(n²)。 |
| **修复** | `VecDeque` 替代 `Vec` |

```rust
use std::collections::VecDeque;

pub struct ContextWindow {
    pub messages: VecDeque<(String, String)>,
    // ...
}

// add_message 中：
let removed = self.messages.pop_front(); // O(1)
```

### P1-10. Scheduler — tick() 全局大锁遍历 + 同步持久化阻塞

| 属性 | 详情 |
|------|------|
| **文件** | `src/schedule/mod.rs` |
| **行号** | 176–208 (tick), 216–248 (save) |
| **问题** | `tick()` 获取 `jobs.lock()` 后遍历全部 job；`save()` 每次全量 JSON 序列化同步写盘。 |
| **风险** | O(N) 扫描无法扩展；持久化阻塞事件循环数十毫秒 |
| **修复** | 时间轮 (Hierarchical Timing Wheel) + 批量异步持久化 |

### P1-11. InMemoryCcrStore — 全局 Mutex 串行读写

| 属性 | 详情 |
|------|------|
| **文件** | `src/ccr/mod.rs` |
| **行号** | 71–101 |
| **问题** | `Arc<Mutex<HashMap>>` 保护所有 put/get/len。HashMap 扩容时锁持有时间突增。 |
| **修复** | `RwLock<HashMap>`（读多写少）或 `DashMap`（读写均频繁）|

### P1-12. exec_with_timeout — 线程泄露

| 属性 | 详情 |
|------|------|
| **文件** | `src/sandbox/container.rs` |
| **行号** | 251–304 |
| **问题** | 每调用创建一个 OS 线程；`waiter.join()` 可能无限阻塞；PID 重用风险。 |
| **修复** | `tokio::process::Command` 或 `wait-timeout` crate |

### P1-13. SandboxPolicy check_builtin() — BTreeSet 线性扫描

| 属性 | 详情 |
|------|------|
| **文件** | `src/sandbox/mod.rs` |
| **行号** | 83–106 |
| **问题** | 遍历 `deny`/`allow` BTreeSet（O(N)），对每个 pattern 调用 `matches()`（字符串 split + 逐段比较）。 |
| **修复** | Trie/Prefix Tree 索引 或 `regex::RegexSet` |

---

## 🟡 P2 — Medium（可靠性隐患 / 性能退化）

### P2-1. 强制 `Connection: close`

| 属性 | 详情 |
|------|------|
| **文件** | `src/http_server.rs` |
| **行号** | 468 |
| **问题** | 硬编码 `Connection: close`，短连接模型下 QPS 受限于 TCP 三次握手。 |
| **修复** | 支持 HTTP/1.1 Keep-Alive |

### P2-2. 无连接数/速率限制

| 属性 | 详情 |
|------|------|
| **文件** | `src/http_server.rs` |
| **行号** | 222–231 |
| **问题** | `listener.incoming()` 无限制接受所有连接。 |
| **修复** | `Semaphore` 限制同时处理连接数 |

### P2-3. Mutex Poisoning panic（全项目 8+ 处）

| 属性 | 详情 |
|------|------|
| **文件** | `value.rs`, `interpreter/mod.rs`, `http_server.rs`, `event/mod.rs` 等 |
| **问题** | `.lock().expect("...mutex poisoned")` — 单点故障级联崩溃。 |
| **修复** | 立即替换为 `parking_lot::Mutex`（无 poison、更快、更小）|

```rust
// Cargo.toml
// parking_lot = "1.1"

use parking_lot::Mutex;
let guard = self.environment.lock(); // 直接返回 Guard，无需 unwrap
```

### P2-4. StreamReader — 串行读 + 暴露 MutexGuard

| 属性 | 详情 |
|------|------|
| **文件** | `src/value.rs` |
| **行号** | 15–29 |
| **问题** | `BufReader` 被 `std::sync::Mutex` 包裹，I/O 完全串行化。 |
| **修复** | `tokio::sync::Mutex`（异步场景）或 `parking_lot::Mutex`（同步场景）|

### P2-5. Value::List / Dict 裸数据结构

| 属性 | 详情 |
|------|------|
| **文件** | `src/value.rs` |
| **行号** | 155–156 |
| **问题** | `Vec` 和 `HashMap` 本身非线程安全。并发 resize 导致未定义行为。 |
| **修复** | `Dict` → `Arc<DashMap<String, Value>>`；`List` → `Arc<RwLock<Vec<Value>>>` |

### P2-6. McpServer / Conversation / Agent 裸 Vec

| 属性 | 详情 |
|------|------|
| **文件** | `src/value.rs` |
| **行号** | 175, 188, 215 |
| **问题** | 裸 `Vec` 无锁保护，并发 push 触发 resize 导致内存损坏。 |
| **修复** | 统一 `Arc<RwLock<Vec<...>>>` |

### P2-7. Value::clone() 深拷贝

| 属性 | 详情 |
|------|------|
| **文件** | `value.rs` 多处 |
| **问题** | `get()`/`define()`/`get_binding()` 等高频路径每次递归深拷贝。 |
| **修复** | Environment 存储 `Arc<Value>`，`get()` 返回 `Arc::clone`（O(1)）|

### P2-8. RetryPolicy 无 jitter

| 属性 | 详情 |
|------|------|
| **文件** | `src/ai_infra.rs` |
| **行号** | 749–756 |
| **问题** | 纯指数退避，无 jitter。当前 `#[allow(dead_code)]`，激活后雷群风险。 |
| **修复** | 同 P1-8，引入随机 jitter |

### P2-9. ContainerHandle Drop — 同步阻塞 docker rm

| 属性 | 详情 |
|------|------|
| **文件** | `src/sandbox/container.rs` |
| **行号** | 232–244 |
| **问题** | `Drop::drop()` 中直接调用 `Command::new("docker").status()`，同步阻塞。 |
| **修复** | 异步清理队列 |

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

### P2-10. CapabilityStore 过期 token 内存泄漏

| 属性 | 详情 |
|------|------|
| **文件** | `src/sandbox/capability.rs` |
| **问题** | `issue()` 可设置 TTL，但过期 token 永久留在 `by_id` 中。 |
| **修复** | 惰性清理（随机抽查）或后台线程定期扫描，或使用 `moka` 缓存 |

### P2-11. LruCache put — 更新已存在 key 时不刷新 order

| 属性 | 详情 |
|------|------|
| **文件** | `src/interpreter/mod.rs` |
| **行号** | 162–165 |
| **问题** | 注释明确承认"简单实现"：更新时不移动 order。高频更新的 key 反而更容易被淘汰。 |
| **修复** | `put()` 中更新时也将 key 移到 `order` 末尾 |

### P2-12. hex_encode 重复实现且不一致

| 属性 | 详情 |
|------|------|
| **文件** | `src/flow.rs:37-39` vs `src/audit/mod.rs:133-139` |
| **问题** | 两处实现功能相同，但 `flow.rs` 版本更差（无预分配）。 |
| **修复** | 统一到优化版本，`audit/mod.rs` 中 `use crate::flow::hex_encode;` |

### P2-13. EventBus Debug / pattern_count 统计不一致

| 属性 | 详情 |
|------|------|
| **文件** | `src/event/mod.rs` |
| **行号** | 52–63, 192–197 |
| **问题** | 分别独立获取三把锁，两次获取之间可能有其他线程修改。 |
| **修复** | 单次锁获取块内读取全部计数 |

### P2-14. AI 结构体无任何并发原语

| 属性 | 详情 |
|------|------|
| **文件** | `src/ai_infra.rs` 多处 |
| **问题** | `LoadBalancer`、`ModelSwitcher`、`SmartCacheEviction`、`CostOptimizer` 等均为纯数据结构，无并发设计。 |
| **修复** | `LoadBalancer` 用 `AtomicUsize` 做 round-robin；pricing 表用 `Arc<HashMap>`（只读）|

---

## 修复优先级矩阵

| 优先级 | 问题 | 文件 | 修复复杂度 | 推荐 crate |
|--------|------|------|-----------|-----------|
| **立即** | CapabilityStore revoke 全体失效 | `sandbox/capability.rs` | 低 | `dashmap` |
| **立即** | CcrStore Clone 语义 | `ccr/mod.rs` | 低 | — |
| **立即** | HTTP 全局 interpreter 锁 | `http_server.rs` | 高 | 架构重构 |
| **本周** | HTTP 超时 + 资源限制 | `http_server.rs` | 低 | — |
| **本周** | mpsc Arc<Mutex<Receiver>> | `http_server.rs` | 低 | `crossbeam-channel` |
| **本周** | Environment 递归锁链 | `value.rs` | 高 | `parking_lot`, `im` |
| **本周** | borrow_variable 语义错误 | `value.rs` | 中 | `parking_lot` |
| **本周** | LruCache 伪 LRU + 锁 | `interpreter/mod.rs` | 中 | `moka` 或手写修复 |
| **本周** | string_interner 竞态 | `interpreter/mod.rs` | 低 | — |
| **本周** | EventBus 三锁串行 | `event/mod.rs` | 中 | `parking_lot` |
| **本周** | CapabilityStore 单锁 | `sandbox/capability.rs` | 低 | `dashmap` |
| **两周** | hex_encode 优化 | `flow.rs` | 低 | — |
| **两周** | retry jitter | `interpreter/mod.rs` | 低 | `rand` |
| **两周** | ContextWindow VecDeque | `ai_infra.rs` | 低 | — |
| **两周** | execute_parallel 真并行 | `interpreter/execute.rs` | 中 | `rayon` |
| **两周** | Scheduler 时间轮 | `schedule/mod.rs` | 中 | `delay-queue` |
| **一个月** | HTTP Keep-Alive | `http_server.rs` | 低 | — |
| **一个月** | 全项目 Mutex → parking_lot | 全项目 | 低 | `parking_lot` |
| **长期** | 迁移到 tokio 异步运行时 | 全项目 | 高 | `tokio`, `axum` |

---

## 推荐引入的工业级依赖

| Crate | 用途 | 替换目标 |
|-------|------|---------|
| `parking_lot` | 无 poison、更快、更小的 Mutex/RwLock | 全项目 `std::sync::Mutex` |
| `dashmap` | 并发安全 HashMap，分片锁 | `Arc<Mutex<HashMap>>` 场景 |
| `moka` | 工业级并发缓存，Segmented LRU | `LruCache` |
| `rayon` | 数据并行迭代器 | `execute_parallel` |
| `tokio` | 异步运行时 | 长期 HTTP/LSP 架构迁移 |
| `rand` | 真随机数 | `retry_sleep_ms` jitter |
| `hex` | SIMD 优化的 hex 编解码 | `hex_encode` |
| `delay-queue` | 高效定时任务调度 | `Scheduler` |
| `crossbeam-channel` | 无锁多消费者 channel | `std::sync::mpsc` |

---

*报告结束。建议按"立即 → 本周 → 两周 → 一个月 → 长期"的优先级分批修复，每批修复后运行 `cargo test -- --ignored` 验证 stress tests。*
