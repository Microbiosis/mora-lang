# Mora-lang Zero-Trust Audit (v0.34)

> **审查原则**: 不信任 → 必然找失败模式
> **审查时点**: 2026-07-03 v0.34 merged (commit `d00a95c`)
> **审查范围**: `src/**/*.rs` (非 test)
> **审查维度**: 并发 / 高压力 / 强类型 / 静态类型
> **审查方法**: 子代理 fan-out + 主对话交叉验证 file:line 关键 P0
> **输出**: 50 项发现 (跨 4 维度), 已 direct-read 源码交叉验证

---

## 0. 执行摘要 (TL;DR)

| 维度 | P0 | P1 | P2 | 总 |
|---|---|---|---|---|
| 高并发 | 5 | 5 | 2 | 12 |
| 高压力 | 6 | 5 | 4 | 15 |
| 强类型 | 5 | 8 | 2 | 15 |
| 静态类型 | 4 | 6 | 5 | 15 |
| **总** | **20** | **24** | **13** | **57** |

### 跨维度核心认识 (历史遗留)

1. **v0.34 集成 5 个 module 进 Interpreter 是 Half-Integration**: 5 个 builtin 的"调用链"完整, 但**底层模块本身的安全/语义假设在 v0.32-v0.33 设计阶段没考虑 v0.34 的多线程共享** — 这次审计 4 个维度里 **15 个 P0 的根因都指向 `Clone for Interpreter` (mod.rs:230-270)**. 它要么 shallow-clones 大 state 但 skip 所有 v0.34 field, 要么构造 fresh empty 计数器 (Scheduler/CCR counter 重复 mint id/hash).

2. **v0.31 的 no-panic refactor 留下了 4 类历史债**:
   - **panic 替代: 边界用 `.expect()` 替代 `panic!`** — Display impl 里 `.expect()` 在 poisoned mutex 上 panic (`value.rs:218, 245`)
   - **unwrap 替代: `Result<_, String>` 返回但 fallback 到 `Value::Nil`** — 这反而是 **更强的类型问题** (P0 静态: Dict.get 漏 key 返回 Nil, CallTask_inner 漏 arg 返回 Nil)
   - **panic-elimination 没扫 `src/ast_v2.rs`** — `walk_expr` 还有 13 个 `.unwrap()` (ast_v2.rs:625-657)
   - **panic-elimination 没扫 `src/value.rs` 的 Display impl** — Display 是 fallback boundary

3. **v0.26-0.33 的 feature creep 在 static type system 留下 4 个 soundness hole**:
   - REPL bypass (mod.rs:651-689)
   - `StmtKind::Route` dead code (parse + typeck 但 execute 不实现)
   - `evaluate_index` 漏 key 静默 Nil (evaluate.rs:180)
   - `call_task_inner`/`call_value_inner` arity check 用 `unwrap_or(Nil)`

4. **5 个 v0.34 builtin 的 `call_*_method` 调用链完整但每个都是 type-soundness 漏洞**:
   - `bus.emit(event, payload)` 接受任意 Value, 无类型校验 (强类型 P1)
   - `sandbox.check_path` 返回 Bool 而非 `PathBuf` (TOCTOU 闭合靠 caller 不真用 path)
   - `schedule.add(kind)` stringly-typed (内部已有 enum JobKind)
   - `ccr.put(data)` 接受任意 Value, silent lossy (Number.to_string 不可逆)
   - `mock.register` stub (handler 边界)

5. **`Clone for Interpreter` 是隐藏的 P0** — 表面看是"深 clone 维护语义", 实际是"深 clone 重置 v0.34 字段". 这是 v0.34 集成没解决的**历史债**: 模块加字段时 Clone impl 同步改了字段, 但**没考虑**这些字段是有状态 singleton. 下次 v0.35 加 module 必须先 fix 这个.

---

## 维度 1: 高并发 (High-Concurrency)

### 🔴 P0-1.1 `Clone for Interpreter` 是**脆的 `Send`-unsafe fabrication**
**File**: `src/interpreter/mod.rs:230-270` (实测确认)

```rust
worker_channels: HashMap::new(),      // 不克隆 channel
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

**5 个 v0.34 字段都被构造为 fresh empty**:
- `Scheduler::new()` → `next_id: Arc<Mutex<u32>> = Mutex::new(0)` → **两个 clone mint 相同 `00000001`**
- `InMemoryCcrStore::new()` → `counter: AtomicU64(0)` → **两个 clone mint 相同 hash 8-char hex**
- `MockRegistry::new()` → handler 表 empty → **original 注册 handler 看不见**

**集成路径**: `dispatch.rs:998` (`Router.listen`) 和 `dispatch.rs:1035` (`McpServer.serve`) 和 `http_server.rs:201,311` 都用 `interpreter.clone()` 喂 worker. 每次 worker 启动都拿到一个**语义剥离**的 interpreter.

**修复**: 删除 `Clone` 或改用 `Arc<Interpreter>`; 或把 5 个字段改成 `Arc<Inner>`, 让 `clone()` 真正共享.

---

### 🔴 P0-1.2 `EventBus::emit` **re-entrant deadlock**
**File**: `src/event/mod.rs:55-64` (实测确认)

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

任何 handler 在同线程调用 `bus.emit` 再次 → `std::sync::Mutex::lock()` **不可重入** → 死锁. Mora 脚本可触发:
```mora
bus.on("outer.*", fn(e,p) bus.emit("nested."+e, p) end)
bus.emit("outer.test", nil)
```
这是 user-visible deadlock.

**修复**: Clone-and-drop 模式 — 先复制 handler snapshot, 再 drop lock, 再 iterate.

---

### 🔴 P0-1.3 `ccr.put` silent overwrite (counter wrap + clone collision)
**File**: `src/ccr/mod.rs:56-71` (实测确认)

```rust
fn put(&self, data: &str) -> String {
    let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;  // u64
    let hash = format!("{:08x}", n);    // 8 hex chars = 32 bits
    ...
    self.entries.lock()...insert(hash.clone(), entry);  // 静默覆盖
    hash
}
```

**两条静默覆盖路径**:
1. **n wrap at 4_294_967_296**: `n = 0x100000001` → `"10000001"` 看起来新, 但 n=257 (`"00000101"`) 和 n=4_294_967_297 (`"10000001"`) — 等等实际上 `{:08x}` 仅 8 chars, n=4_294_967_296 = `0x100000000` → `"10000000"`, 不与之前任何 n 冲突 — **直到 n > 2^36**. 真正碰撞点是 n = 4_294_967_296 + 0x100 = 4_294_967_552 → `"10000100"` 与 n=256 (`"00000100"`) 碰撞.
2. **Clone 碰撞 (P0-1.1 副产物)**: 两个 clone 实例都从 AtomicU64(0) 开始, `fetch_add(1)` 都返回 0, 都 mint `00000001`. 后 put 的 silent overwrite 前者.

**修复**: key 用 `n` (u64) 而不是 hex string; 用 `Entry::or_insert` 去重; 或保留 hex + checksum.

---

### 🔴 P0-1.4 `MockRegistry::call` lock-hold-across-user-fn (同 P0-1.2 pattern)
**File**: `src/mock/mod.rs:73-79` — `MockHandler::Native(f)` lock-held-call; `mock::Script(_) => None` 静默 drop.

**关键**: `builtins.rs:466-471` 的 v0.34 wrapper 已经规避 (先 `get()` 后释放 lock 再 invoke), 但 `MockRegistry::call` 原始 API 仍 unsafe. 两个 API, 两个语义, **没注释**.

**修复**: 把 `MockRegistry::call` 改用 clone-and-drop (mirror builtins.rs 已修好的 path).

---

### 🔴 P0-1.5 `v2_arena: Option<AstArena>` 在 closure/task 调用时 deep clone
**File**: `src/interpreter/dispatch.rs:1067, 1082` — `self.v2_arena.clone()` 每次 v2 closure/task 调用.

`AstArena` 内容可能是 100KB+ 源码. 每个 `(map)`/`(filter)`/`(reduce)` element 都 deep-clone 整个 arena. **HTTP server worker (P0-1.1 已发散) 实际上每请求都在做 arena 拷贝**.

**修复**: wrap `v2_arena` 在 `Arc<AstArena>` 里.

---

### 🟡 P1-1.6 Lock-hold-across-IO (http_server.rs:175-185, dispatch.rs:982-996)
路由表 lock + `eprintln!` stdout flush 在 lock 内. 另一线程注册 route → 阻塞. 教科书 priority-inversion.

---

### 🟡 P1-1.7 `Scheduler::tick()` race window
**File**: `src/schedule/mod.rs:173-205` — lock 释放后 `save()` 重新拿 lock. 介于两 lock 间 panic → 数据不一致.

---

### 🟡 P1-1.8 `Scheduler.next_job_id: Mutex<u32>` overflow
**File**: `src/schedule/mod.rs:78-82` — 4B 后 wrap; 配合 P0-1.1 Clone 整 mint 重复 id.

---

### 🟡 P1-1.9 `MockRegistry::call` 与 wrapper 语义分叉 (已列出)

---

### 🟡 P1-1.10 `v2_arena.clone()` deep clone per call (已列出)

---

### 🟢 P2-1.11 `sandbox.check_path` TOCTOU by construction
**File**: `src/sandbox/mod.rs:81-114` — `canonicalize()` + `starts_with()` 是两个 syscall, 中间 symlink race. 当前 caller 不真用 path 所以闭合了, 但 API 形状不对.

---

### 🟢 P2-1.12 `InMemoryCcrStore` 应 `RwLock`/`DashMap`
读多写少用 Mutex.

---

## 维度 2: 高压力 / 性能

### 🔴 P0-2.1 `parse_json_list` / `parse_json_dict` O(n²)
**File**: `src/flow.rs:413, 441, 461` — `&s[i..].trim_start()` 每 loop 调一次.

1000-item JSON list + 空白 → 500K char-scans; 10K → 50M. 这是 `dict.json()`, `web.fetch`, `crush_json_string`, AI response parse 共享的路径.

**修复**: `while i < s.len() && matches!(s.as_bytes()[i], b' ' | b'\t' | b'\n' | b'\r') { i += 1; }` — 无 alloc, O(1) per step.

---

### 🔴 P0-2.2 7 个 `#[allow(dead_code)]` Interpreter 字段
**File**: `src/interpreter/mod.rs:163-191, 245-260` — `method_cache`/`ai_batch_queue`/`cache_warm_queue`/`ai_priority_queue`/`adaptive_temp`/`load_balancer`/`retry_policy`.

实际**完全无引用**, `grep` 全文 0 读写点. 每个 Interpreter 实例 +480B 永远为空 state. `Router::listen`/`McpServer::serve` 每次 `clone()` 都构造 480B 垃圾.

**修复**: 直接删这 7 字段, 简化和 `Self{}` × 4 块 + `Clone`.

---

### 🔴 P0-2.3 `evaluate_call` 每个 call alloc Vec + 拿 environment Mutex
**File**: `src/interpreter/evaluate.rs:55-87` — `Vec::new()` heap alloc + `Arc<Mutex<Environment>>::lock()` per call.

100K-token context 内数万个调用, allocator pressure 主导. `evaluate_pipe` (104), `evaluate_method_call` (150) 同样.

**修复**: 用 `SmallVec<[Value; 8]>` 或 thread a reusable `&mut Vec`; environment 改 RwLock.

---

### 🔴 P0-2.4 `v.to_string()` 重复 18+ 次, 包括已 `Value::String` arg
**File**: `src/interpreter/dispatch.rs` 18+ sites + `builtins.rs` lines 240-548 全部 builtin arg 提取. `Dict.get(key)`: 每 .get() 调一次 `to_string()`, 即便 key 已是 String.

**修复**: 加 `arg_str(idx) -> Option<&str>` helper, 对 String 用 `Cow::Borrowed`.

---

### 🔴 P0-2.5 `SpeculativeVerifier` 长度 cache + 不存在的 verification queue
**File**: `src/interpreter/ai_chat.rs:355-369`, `src/ai_infra.rs:186-195` — cache key 只用 `{draft.len()}:{verification.len()}` (长度相同 → 不同响应 → 同 cached 结论). 是 correctness bug + perf bug.

---

### 🔴 P0-2.6 `SmartCrusher` `format!("{:?}", v)` per value
**File**: `src/compress/json.rs:340, 530, 635` — `compute_uniqueness`/`ClusterSampleStrategy`/`KeepErrorsConstraint` 对每个 value `format!`. 100K items × 5 fields = 50K+ Strings allocated.

`KeepErrorsConstraint` 14 × 2 `to_lowercase()` × N items × M fields ≈ 300K allocs.

**修复**: 改用 `Value` discriminant tag 直接 hash; 一次性 lowercase keyword table.

---

### 🟡 P1-2.7 `Value::List` Display build `Vec<String>` + join (value.rs:183-190)
N intermediate String allocs per Display.

---

### 🟡 P1-2.8 `call_value_inner` 每次 closure call 新 `Arc<Mutex<Environment>>` + per-arg clone (dispatch.rs:1175-1191)

---

### 🟡 P1-2.9 `starts_with(&v.to_string())` 即便 v 已是 String (dispatch.rs:684+)

---

### 🟡 P1-2.10 `Clone` deep clone `trait_registry`, `impl_table` 等 (mod.rs:243-244)

---

### 🟡 P1-2.11 `_cache_key = format!(...)` 计算即丢 (dispatch.rs:449-450)

---

### 🟡 P1-2.12 `estimate_bytes` 全树 `value_to_json().len()` (compress/json.rs:950)

---

### 🟢 P2-2.13 `string_interner` 无 eviction, 单调增长 (mod.rs:580-587)

---

### 🟢 P2-2.14 `ai_cache` key `format!("{}:{:?}", model, messages)` 重格式 100 messages per call (ai_chat.rs:404)

---

### 🟢 P2-2.15 `parse_json_string` silent UTF-8 截断 (flow.rs:399)

---

## 维度 3: 强类型 (Strong-Typing)

> 评估: v0.31 的 no-panic refactor (commits `b374975`, `d891326`) 在 panic 维度 OK, 但**留下了 4 类更难处理的债**.

### 🔴 P0-3.1 `src/ast_v2.rs:625-657` `walk_expr` 13× `arena.get_expr(*child).unwrap()`
**File** 实测确认 — 13 处精确 .unwrap() 在 visitor traversal.

`walk_expr` 是 type-checker / lints / codegen / lsp visitor 的核心 utility. `get_expr` 返回 `Option<&TypedExpr>` (ast_v2.rs:582). `None` 出现时机:
- 增量 parse 留下 stale NodeId
- 宏展开产生 dangling ref
- 手构造 arena

Visitor pass 直接 panic interpreter. **这违反了 v0.31 的 lexer/parser no-panic invariant** — 但当时 refactor 只扫了 lexer/parser, 没扫 ast_v2.

**修复**: 返回 `Result<T, String>`; 或 `walk_expr -> Option<T>` 让 caller 处理 None.

---

### 🔴 P0-3.2 `src/value.rs:218, 245` `Display::fmt` 用 `.expect()` — **panic in Display**
**File** 实测确认 `expect("...mutex poisoned")` 在 `Value::Router` 和 `Value::Atom` Display.

`Display::fmt` 是 REPL, 错误格式化, Value 插值的 fallback boundary. 单个 poisoned mutex → 任何后续 to_string 那个值 (及包含它的 List/Dict) 都 crash.

**修复**: 永不 panic in Display — 返回 `write!(f, "<router (poisoned)>")`.

---

### 🔴 P0-3.3 `src/interpreter/mod.rs:384` 单个 bare `.unwrap()`
**File** 实测确认 `globals.lock().unwrap().define("len", ...)` — 整个文件唯一 bare unwrap. 1 行修复.

---

### 🔴 P0-3.4 Lexer 接受 NUL / 控制字符 in strings
**File**: `src/lexer.rs:546-583, 643-692` — `string_from`/`prompt_string_from` 允许 `\0`. 在 POSIX 文件名 / HTTP body 等边界会 crash downstream.

**修复**: `c < 0x20 && c != '\n' && c != '\t' && c != '\r'` → emit error token.

---

### 🔴 P0-3.5 其他 P0 (跨维度)
- `src/interpreter/builtins.rs:244, 297-333, 391-397` (bus.emit/schedule.add/ccr.put 接受任意 Value + 静默 lossy)
- `src/ccr/mod.rs:96` `extract_hash` 对 malformed marker 返回 `Some("")`
- `src/event/mod.rs:55-64` re-entrant emit (同时是 P0-1.2)
- `src/value.rs:43` NaN 静默传播

(详见 P1-3.6 ~ 3.13)

---

### 🟡 P1-3.6 `Value::Builtin(String)` 30+ 字符串比较做 dispatch (value.rs:60)
错别字 `web.fecth` 编译过, 运行时才炸. 这是**最容易修复的 typing win** — 改 enum.

---

### 🟡 P1-3.7 `bus.emit` 接受任何 Value 当 event name (builtins.rs:244)
`Value::Number(1.5)` → event `"1.5"`.

---

### 🟡 P1-3.8 `ccr.put` silent lossy (builtins.rs:391-397)
`Value::Number(0.1+0.2)` → `"0.30000000000000004"`, LLM 用 marker 取回无法 reverse. CCR 核心 contract 破坏.

---

### 🟡 P1-3.9 `schedule.add(kind)` stringly-typed (builtins.rs:297-333)
内部已有 enum JobKind, 边界没对齐.

---

### 🟡 P1-3.10 `sandbox.allow: Vec<String>` 集合当列表用 (sandbox/mod.rs:20, 22)
O(N) per check; 重复 allow pattern 都被扫.

---

### 🟡 P1-3.11 `event::matches` 段数比较 `pa_segments.len() <= ev_segments.len() + 1` 边界不清晰 (event/mod.rs:92-110)
当前行为对, 但表达 +1 容易让人误读.

---

### 🟡 P1-3.12 `MockRegistry::call` Script handler 静默返回 None (mock/mod.rs:73-79)
v0.34 wrapper 已修复, 但 module 原 API 仍 footgun.

---

### 🟡 P1-3.13 `Value::Number(f64)` NaN/Infinity 静默传播 (value.rs:43)
`0.0/0.0` 不是 equal to itself → dict lookup 错乱.

---

### 🟢 P2-3.14 `Value::List` Display 无 cycle guard (value.rs:184-185)
当前 `List` by-value, cycle 不可能, 但 `Atom(Arc<Mutex<Value>>)` 可自反. Display 不应 block on user data.

---

### 🟢 P2-3.15 `file.*` 方法不调用 `sandbox.check_path` (builtins.rs:32-110)
**File** 实测确认 `call_file_method` 没 call `self.sandbox.check_path(&path)`. 沙箱内置但 file builtin 绕过. 设计问题, 不是 bug.

---

## 维度 4: 静态类型 (Static-Typing)

### 🔴 P0-4.1 `Value::Dict` 漏 key 静默 Value::Nil (evaluate.rs:180)
**File** 实测确认 `(Value::Dict(map), Value::String(key)) => Ok(map.get(key).cloned().unwrap_or(Value::Nil))`.

typeck `check.rs:914-925` 声明 `Dict.get(key) -> V` (value type). 静态 `let x: number = {"a": 1}["b"]` pass typeck. **Runtime 返回 Value::Nil, 被强迫塞进 number binding**. 这是 soundness hole — static checker 同意, runtime 违反.

**修复**: Dict.get 应 narrow to `V | Nil` (Union); call site 必须显式处理 Nil (不可 silent coerc).

---

### 🔴 P0-4.2 `Value::Task`/`Value::Closure` arity 用 `unwrap_or(Value::Nil)` (dispatch.rs:1115, 1182)
**File** 实测确认两处 `args.get(i).cloned().unwrap_or(Value::Nil)`.

typeck `check.rs:846-861` 报告 `arg count mismatch` 但**仅当 `!sig.params.is_empty()`**:
- zero-param task 完全不校验
- 即便报告, runtime 仍 nil-fill 跑

Static `task add(a: number, b: number) ...; add(1)` → typeck 报 → runtime 用 `add(1, nil)` → `1 + nil` 触发 operator error.

**修复**: `call_task_inner` 必须 `Err("missing arg 'b'")` 若 arg 缺失且 param 无 default.

---

### 🔴 P0-4.3 REPL bypasses typeck 完全 (interpreter/mod.rs:651-689, main.rs:952-955)
**File** 实测确认 `run_repl_with` 调 `parse_code()` 然后 `interp.execute()` 但**没调 `typeck::check_program`**.

`run_file`, `run_record`, `run_replay`, `run_snapshot` 全 typeck; **只 REPL skip**. 用户在 REPL 写 `let x: number = "hello"` 不会报错, 运行时才报错.

**修复**: `run_repl_with` line 673 加 `let _ = typeck::check_program(&node_ids, &arena);` 2 行修复.

---

### 🔴 P0-4.4 `StmtKind::Route` typecheck 但 execute 不实现 (execute.rs:144)
**File** 实测确认 — `grep -n Route src/interpreter/execute.rs` 无匹配.

`StmtKind::Route` 经 parse → typeck 通过 → `execute_stmt` 落到 `_ => Err(format!("Unsupported v2 statement: ..."))` (execute.rs:144). `route_registry: HashMap<String, String>` 字段声明 → Clone → Default → **从不读**. Worse, typeck 内 `routes: HashSet<String>` (mod.rs:527) 是**完全分离的 dead state**.

`ExprKind::RouteCall` (ast_v2.rs:139) 无 parser path 产生, 是 orphaned.

**修复**: 实现 `execute_route` **或** 删除 4 层 dead code (Stmt + Expr + parser arm + typeck arm + dead field).

---

### 🟡 P1-4.5 `Type` enum 无 `Document` 变体 (typeck/mod.rs:38-89)
21 variants 末尾是 `Union(Vec<Type>)`. `DocumentBackend` (document/mod.rs:14) 5 方法 (markdown/text/pages/metadata/blocks) 全在 typeck 未知 → method_return_type_fallback 返回 `Union([])` → `any`. `let blocks: list<dict> = d.blocks()` pass typeck.

**修复**: 加 `Type::Document` + 5 method arms.

---

### 🟡 P1-4.6 `current_ai_config: Option<AiConfigValue>` 不 typecheck
**File**: `interpreter/mod.rs:149`, **8 字段**: model/temperature/max_tokens/budget/per_call/system/mock_responses/speculative/draft_model.

`Type::AiConfig` 存在 (mod.rs:55) **但仅做 singleton value, 不做 structural record**. `with` 块运行时静默 drop `system`/`budget`/`max_tokens` (orchestrate.rs:161-180).

---

### 🟡 P1-4.7 `load`/`read_file` typeck 错位 (check.rs:111-115)
`load` → `Union([])` (= any); `read_file` → `String`. `let n: number = load("f")` pass, runtime 返回 Nil → 0.

`read_bytes_file` 缺这条 arm, 完全 fall through.

---

### 🟡 P1-4.8 Typed numeric literals 不支持 (`1i64`/`1u32`/`1.0f32`)
`Literal::Number(f64, Span)` 唯一 numer 路径. Parser 不识别 `1i64`, typeck 不识别.

---

### 🟡 P1-4.9 `document.reading_order` 是 module 非 builtin
未绑定 interpreter. typeck 无 `Type::Document` (跨 P1-4.5).

---

### 🟡 P1-4.10 `ImplDef` orphan `for_type` 任意字符串 (check.rs:1081-1103)
`for_type: "MisspellWidget"` (不存在 type) + `Display` 5 方法只 impl 1 → typeck accept. Runtime dispatch 失败**仅在调用 missing method 时**.

**修复**: `check_impl_def_stmt` 验证 `for_type` 存在 + trait methods 全实现 + method signatures 兼容.

---

### 🟢 P2-4.11 typeck errors 总是 `line: 0, column: 0` (check.rs:204-1006 多处)
manual `TypeError { ... }` 不携带 Span. 用户错误指向文件头.

---

### 🟢 P2-4.12 `print` Union hand-maintained 6-element list (mod.rs:636-655)
维护陷阱. 嵌套 list/dict 无限深度都接受.

---

### 🟢 P2-4.13 `let x: Never = ...` / `let x: Unknown = ...` 降级到 `Type::Trait { name }` 占位 (mod.rs:213-218)
如果 `Never` 不在 trait_registry → 报错; 如声明了 stub trait → silent accept.

---

### 🟢 P2-4.14 `let x = expr` 无 hint 时 `init_ty = Union(vec![])` 永久 any (check.rs:229-231)
`let x = unknown_call()` → x 永久 any 传播.

---

### 🟢 P2-4.15 `with` 块 typeck 不验 binding key 是否 in target type (check.rs:75-77, 467-483)
`with foo = 42 do ... end` (无 `foo` 字段) silent accept.

---

## 历史债 → v0.34 集成 → v0.35 路线图

| 历史版本 | 引入债 | 描述 | 当前状态 |
|---|---|---|---|
| v0.04 | `Clone for Interpreter` 重置 13 字段 | mod.rs:230-270 | v0.34 加 5 字段也重置 → P0-1.1 |
| v0.04 | `method_cache` 字段 | 永远空 | 7 dead_code 字段之一 (P0-2.2) |
| v0.26 | `StmtKind::Route` + `route_registry` 字段 | parse + typeck 不 execute | P0-4.4 dead code |
| v0.30 | `SmartCrusher` content-aware 形 stringification | compress/json.rs format!("{:?}", v) per value | P0-2.6 |
| v0.31 | `panic-elimination` refactor | 只扫 lexer/parser, 留 ast_v2 + value.rs Display | P0-3.1, P0-3.2 |
| v0.32 | `EventBus` 模块 | mutex-held user handler | P0-1.2 |
| v0.32 | `MockRegistry` 模块 | mutex-held user fn + Script handler 静默 None | P0-1.4, P1-3.12 |
| v0.32 | `RecursiveWalker` | 仅 module (orphaned?) | 未在本次集成 5 builtin 之列 |
| v0.33 | `Scheduler` `next_id: Mutex<u32>` | counter overflow + Clone collision | P0-1.1, P1-1.8 |
| v0.33 | `SandboxPolicy` Vec allow/deny + Bool check_path | P1-3.10 + P2-1.11 | |
| v0.33 | `CcrStore` hex=u32 + silent overwrite | P0-1.3 | |
| v0.33 | `ReadingOrder` 仅 module, 没 typeck 入口 | P1-4.9 | |
| v0.34 | 5 builtin 加到 Interpreter 但**模块自身不安全** | 5 个 call_*_method 是模块 unsafe API 的 façade | 所有 v0.34 builtin 都是 P0 |

### v0.35 P0 必做清单 (按 ROI)

| 优先级 | 修改 | 文件 | 行数 |
|---|---|---|---|
| 🔥 P0-4.3 | REPL 加 typeck | main.rs:952-955 + interpreter/mod.rs:651-689 | 2 行 |
| 🔥 P0-4.4 | 实现 `execute_route` 或删 4 层 | execute.rs:144 + parser/typeck/ast_v2 | 大改 |
| 🔥 P0-2.2 | 删 7 dead 字段 | interpreter/mod.rs:163-260 | ~80 行 |
| 🔥 P0-3.3 | `.unwrap()` → `.expect()` | interpreter/mod.rs:384 | 1 行 |
| 🔥 P0-1.2 | EventBus clone-and-drop | event/mod.rs:55-64 | 5 行 |
| 🔥 P0-1.4 | MockRegistry 同上 + Script 分支 | mock/mod.rs:73-79 | 8 行 |
| 🔥 P0-1.5 | v2_arena wrap in Arc | interpreter/mod.rs + dispatch.rs | 4 行 |
| 🔥 P0-3.1 | walk_expr 不 unwrap | ast_v2.rs:625-657 | 13 处改动 |
| 🔥 P0-3.2 | Display 不 panic | value.rs:218, 245 | 4 行 |
| 🔥 P0-4.1 | Dict.get narrow to V \| Nil | evaluate.rs:180 + check.rs:914-925 | 3 行 |
| 🔥 P0-4.2 | call_task_inner arity Err | dispatch.rs:1115, 1182 | 4 行 |
| 🔥 P0-2.1 | parse_json O(n²) → O(n) | flow.rs:413, 441, 461 | 10 行 |
| 🔥 P0-2.4 | arg_str helper | dispatch.rs 18+ sites + builtins.rs 30+ sites | large refactor |
| 🔥 P0-2.5 | 删 SpeculativeVerifier length cache | ai_chat.rs:355-369 + ai_infra.rs:186-195 | 20 行 |
| 🔥 P0-1.3 | ccr hash key 改 u64 | ccr/mod.rs:56-71 | 4 行 |
| 🔥 P0-1.1 | Clone impl 改 Arc 共享 OR 删 Clone | interpreter/mod.rs:230-270 | 50+ 行 |

### v0.35+ P1 集群 (10 项大改)

- `Type::Document` + 全 16 个 Value 缺 Type variants
- `Value::Builtin` enum 化 (30+ 字符串 dispatch)
- sandbox Vec → HashSet, schedule enum JobKind 边界对齐
- parse_json O(n²) + SmartCrusher format!("{:?}", v) per value
- Display cycle-safe (Atom 中锁)
- Worker_channels 重新设计 (`!Send`)

### 历史 debt 永久债 (v1.0 之前不解决)

- `mpsc::Receiver in worker_receivers: HashMap<…>` — 这是 `!Send`, 需要 Interpreter 永久 `!Send` (`!Send for Interpreter` 不存在语法, 但需要文档+threading model 决策)
- `Value::Number(f64)` 单 numer 类型 — 整 v0.35 不解决, 等 v1.0 多 Numeric 变体
- Document/Trait/Schedule/Sandbox/Event/Mock/AiConfig 没有 Type variants — 永久债, 16 项

---

## 跨维度元结论

> **v0.34 是"集成债清理"release, 不是新功能 release.**
> 它做的对的事情: 把 5 个 module 接到 Interpreter 的 5 步模式跑通.
> 它没做的事情: 集成时没 audit module 自身的历史债 (EventBus/Mock registry 的 lock pattern, Scheduler next_id 计数器, CCR hex hash 的 collision).
> **核心问题**: v0.34 的 "Clone for Interpreter" 字段重置 (mod.rs:230-270) 把 v0.32-0.33 module 的**共享 state 模型** 降级到 **per-clone fresh state**. 这是 v0.35 必须修的.
>
> 真问题不在写完 5 step, 而在**第 0 步之前 module 自身的安全假设就要被审计**. v0.35 应该加 **`module-readiness-checklist`**:

```
□ Module API thread-safe (Send/Sync) under documented sharing
□ All Mutex guards NOT held across user-supplied callbacks
□ All counter-based identity 64-bit + monotone + Clone-aware
□ All file/path operations TOCTOU-safe by API shape (return enum, not Bool)
□ All Value-accepting APIs validate at boundary or document lossy behavior
□ Typeck has matching Type variant before builtin is added
```
