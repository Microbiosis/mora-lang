# Sub-project A: interpreter.rs Architectural Refactor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor `src/interpreter.rs` (4413 LOC) into 5–6 focused sub-modules with type-safe AI error handling, eliminating 120+ lines of duplicated AI client code, while preserving the public API surface byte-for-byte.

**Architecture:** Bottom-up migration. First extract leaf modules (`value`, `flow`, `json_compat`) that have no internal dependencies. Then introduce `AiError` (thiserror) and a shared `AiClient` wrapper. Replace the 3 duplicated ureq call sites. Finally, split the 2700-line `impl Interpreter` block into `eval/*` (4 files) and `builtin/ai/*` (6 files) sub-modules.

**Tech Stack:**
- Rust 1.96, edition 2024
- `ureq 3.3` (HTTP), `libc 0.2` (FFI)
- New dep: `thiserror 2.x` (compile-time error enum macros)
- Tools: `cargo-public-api` (API snapshot), `cargo-udeps` (dep audit), `cargo-outdated` (dep audit)

---

## Global Constraints

These apply to every task. Copied verbatim from `docs/superpowers/specs/2026-06-23-subproject-a-interpreter-architecture-design.md` §2 and §6.

1. **No behavior change**: 84/84 tests must pass at every step.
2. **Public API snapshot** must be byte-identical to `docs/superpowers/specs/api-baseline.txt` at every step. (Auto-derived `impl Freeze/Send/Sync/Unpin/UnsafeUnpin/RefUnwindSafe/UnwindSafe` may fluctuate — diff those out before comparing.)
3. **No new clippy warnings** (`cargo clippy --all-targets` must not add warnings; 35 pre-existing `clippy::collapsible_if` are accepted).
4. **No new build warnings** (debug + release).
5. **No new audit findings** (`cargo audit` must remain 0 vulnerabilities).
6. **No new unused deps** (`cargo +nightly udeps --all-targets` must remain `All deps seem to have been used`).
7. **Re-exports preserved**: `mora::Value`, `mora::Interpreter`, `mora::json_to_value`, `mora::FlowSignal`, `mora::Environment` must keep working from the same paths.
8. **Each task ends with a commit**. Use the commit message format: `<type>(<scope>): <subject>` (Conventional Commits).

**Commit verification after every task:**
```bash
cargo build --all-targets 2>&1 | grep -E "^(error|warning)" | head -5
cargo test 2>&1 | tail -5
cargo public-api --simplified 2>/dev/null > /tmp/api_after.txt
diff <(grep -vE "impl core::marker::(Freeze|Send|Sync|Unpin|UnsafeUnpin)|impl core::panic::unwind_safe::(RefUnwindSafe|UnwindSafe)" docs/superpowers/specs/api-baseline.txt) <(grep -vE "impl core::marker::(Freeze|Send|Sync|Unpin|UnsafeUnpin)|impl core::panic::unwind_safe::(RefUnwindSafe|UnwindSafe)" /tmp/api_after.txt)
```
All three commands must exit cleanly (build 0 warnings; test "0 failed"; diff empty).

---

## File Structure (post-refactor)

| Path | Action | Purpose | LOC est |
|---|---|---|---|
| `src/lib.rs` | Modify | Add `pub mod value; pub mod flow; pub mod json_compat; pub mod ai_error; mod eval; mod builtin;` | +8 |
| `src/value.rs` | Create | `Value`, `Environment`, `FlowSignal`, `StreamReader` | 350 |
| `src/flow.rs` | Create | `is_truthy`, `eval_binary`, `numeric_op`, `values_equal`, `literal_to_value_static`, `check_type`, `type_name`, `value_to_json`, `expect_string`, `is_builtin_object`, `is_pipe_method` | 200 |
| `src/json_compat.rs` | Create | `json_to_value` + 6 `parse_json_*` | 300 |
| `src/ai_error.rs` | Create | `AiError` enum (thiserror) | 90 |
| `src/interpreter.rs` | Rewrite | `Interpreter` struct + `impl Default` + top-level `Interpreter::run` / `run_repl` / `run_repl_with` + `interpret` dispatch | 1200 |
| `src/eval/mod.rs` | Create | `eval_expr`, `eval_stmt`, `register_tool`, `eval_dyn_trait`, `dispatch_trait_method` | 100 |
| `src/eval/call.rs` | Create | `call_function`, `call_task`, `call_closure`, `call_method`, `call_value` | 500 |
| `src/eval/methods.rs` | Create | `call_file_method` (file I/O builtin dispatch) | 400 |
| `src/eval/prompt.rs` | Create | `eval_prompt_parts`, `eval_route_arg`, `eval_prompt_parts_from_stmt` | 150 |
| `src/builtin/mod.rs` | Create | Builtin function registration + dispatch | 200 |
| `src/builtin/io.rs` | Create | `read`, `write` builtin functions | 100 |
| `src/builtin/http.rs` | Create | `web.fetch` builtin (now using `AiClient`) | 80 |
| `src/builtin/ai/mod.rs` | Create | `ai.*` namespace | 50 |
| `src/builtin/ai/client.rs` | Create | `AiClient` (shared ureq abstraction) | 250 |
| `src/builtin/ai/chat.rs` | Create | `ai.chat`, `chat_with_tools` | 200 |
| `src/builtin/ai/agent.rs` | Create | `run_agent` | 250 |
| `src/builtin/ai/critic.rs` | Create | `run_critic` | 100 |
| `src/builtin/ai/embedding.rs` | Create | `cosine_similarity`, `dot_product`, `euclidean_distance`, `l2_norm`, `mock_bow_embedding` | 200 |

**Total estimated post-refactor LOC**: 4720 (vs 4413). The +307 LOC comes from module headers, doc comments, and `pub(crate)` accessors that the monolith used implicit access for. Net *cyclomatic* complexity drops ~40%.

---

## Task 1: Extract `value.rs` (Value/Environment/FlowSignal/StreamReader)

**Files:**
- Create: `src/value.rs`
- Modify: `src/lib.rs:6-14` (add `pub mod value;`)
- Modify: `src/interpreter.rs:1-225` (delete moved code + `use crate::value::*;` insert)

**Interfaces:**
- Consumes: nothing (leaf module)
- Produces: `pub enum Value`, `pub struct Environment`, `pub enum FlowSignal`, `pub struct StreamReader` + their `impl` blocks

- [ ] **Step 1: Snapshot current public API for comparison**

Run: `cargo public-api --simplified 2>/dev/null | grep -E "^pub.*mora::(Value|Environment|FlowSignal|StreamReader)" > /tmp/api_value_before.txt && wc -l /tmp/api_value_before.txt`
Expected: ~25 lines (Value enum + Environment + FlowSignal + StreamReader signatures)

- [ ] **Step 2: Create `src/value.rs`**

Write the file with this content (extracted from interpreter.rs:39–225):

```rust
//! v0.x: 从 interpreter.rs 抽离的运行时值/环境/控制流核心类型。

use std::collections::HashMap;
use std::fmt;
use std::io::{BufReader, Read};
use std::sync::{Arc, Mutex};
use crate::ast::Stmt;

// ─── StreamReader ──────────────────────────────────────────────────────

pub struct StreamReader(pub Arc<Mutex<BufReader<Box<dyn Read + Send + Sync>>>>);

impl StreamReader {
    pub fn new(reader: Box<dyn Read + Send + Sync>) -> Self {
        StreamReader(Arc::new(Mutex::new(BufReader::new(reader))))
    }
    pub fn read_line(&self) -> Result<Option<String>, String> {
        let mut guard = self.0.lock().map_err(|e| format!("StreamReader lock: {}", e))?;
        let mut buf = String::new();
        match guard.read_line(&mut buf) {
            Ok(0) => Ok(None),
            Ok(_) => {
                if buf.ends_with('\n') { buf.pop(); }
                if buf.ends_with('\r') { buf.pop(); }
                Ok(Some(buf))
            }
            Err(e) => Err(format!("StreamReader: {}", e)),
        }
    }
}

impl fmt::Debug for StreamReader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamReader").finish()
    }
}

// ─── Value ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub enum Value {
    String(String),
    Char(char),
    Number(f64),
    Bool(bool),
    Nil,
    List(Vec<Value>),
    Dict(HashMap<String, Value>),
    Task { name: String, params: Vec<String>, body: Vec<Stmt> },
    Closure { params: Vec<String>, body: Vec<Stmt>, env: Arc<Mutex<Environment>> },
    Builtin(String),
    Conversation { messages: Vec<(String, String)>, model: String, base_url: String, api_key: String },
    Stream { reader: StreamReader, done: Arc<Mutex<bool>> },
    Agent { name: String, tool_names: Vec<String>, model_route: String, max_steps: usize, system: String },
    AiConfig { model: Option<String>, temperature: Option<f64>, max_tokens: Option<usize>, system: Option<String>, budget: Option<usize> },
    Router { routes: Arc<Mutex<Vec<(String, String, Value)>>> },
    HttpRequest { method: String, path: String, query: String, body: Box<Value>, params: HashMap<String, String> },
    McpServer { tools: Vec<(String, Value)> },
    TraitObject { for_generics: Vec<String>, trait_generics: Vec<String>, for_type: String, trait_name: String, data: Box<Value> },
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        crate::flow::values_equal(self, other)  // defined in Task 2
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", crate::flow::value_to_string(self))
    }
}

// ─── Environment ───────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Environment {
    pub bindings: HashMap<String, Value>,
    pub parent: Option<Arc<Mutex<Environment>>>,
}

impl Environment {
    pub fn new() -> Self {
        Environment { bindings: HashMap::new(), parent: None }
    }
    pub fn with_parent(parent: Arc<Mutex<Environment>>) -> Self {
        Environment { bindings: HashMap::new(), parent: Some(parent) }
    }
    pub fn define(&mut self, name: String, value: Value) { self.bindings.insert(name, value); }
    pub fn get(&self, name: &str) -> Option<Value> {
        if let Some(v) = self.bindings.get(name) { return Some(v.clone()); }
        if let Some(parent) = &self.parent { return parent.lock().ok()?.get(name); }
        None
    }
    pub fn assign(&mut self, name: &str, value: Value) -> Result<(), String> {
        if self.bindings.contains_key(name) { self.bindings.insert(name.to_string(), value); return Ok(()); }
        if let Some(parent) = &self.parent { return parent.lock().map_err(|e| format!("env lock: {}", e))?.assign(name, value); }
        Err(format!("undefined variable: {}", name))
    }
}

// ─── FlowSignal ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum FlowSignal {
    Return(Value),
    Break,
    Continue,
}

impl FlowSignal {
    pub fn is_return(&self) -> bool { matches!(self, FlowSignal::Return(_)) }
}
```

**IMPORTANT**: `Value::PartialEq` and `Value::Display` reference `crate::flow::values_equal` / `crate::flow::value_to_string` — these will be defined in Task 2. For now, `src/flow.rs` (created in Step 3) will contain stubs.

- [ ] **Step 3: Create stub `src/flow.rs` so this task compiles**

Create `src/flow.rs` with stubs:

```rust
//! v0.x: 控制流与值比较工具 (从 interpreter.rs 抽离)。

pub fn values_equal(_a: &crate::value::Value, _b: &crate::value::Value) -> bool {
    unimplemented!("filled in Task 2")
}

pub fn value_to_string(_v: &crate::value::Value) -> String {
    unimplemented!("filled in Task 2")
}
```

This is a temporary stub. Real implementations land in Task 2.

- [ ] **Step 4: Add `pub mod value; pub mod flow;` to `src/lib.rs`**

Edit `src/lib.rs` to add (after the existing `pub mod interpreter;`):

```rust
pub mod value;
pub mod flow;
```

- [ ] **Step 5: Delete moved code from `src/interpreter.rs`**

Remove lines 39–225 (the `Value`, `Environment`, `FlowSignal`, `StreamReader` definitions + their impls). Also remove the `use std::io::{BufReader, Read}` and `use std::sync::{Arc, Mutex}` if they become unused.

Add at the top of `src/interpreter.rs`:

```rust
use crate::value::{Value, Environment, FlowSignal, StreamReader};
```

(Note: re-export via `pub use crate::value::*;` in interpreter.rs is NOT needed — `Value` etc. remain accessible via `mora::Value` through lib.rs.)

- [ ] **Step 6: Build and verify**

Run: `cargo build --all-targets 2>&1 | tail -20`
Expected: 0 errors. May show warnings about `unimplemented!()` in `flow.rs` stubs — those are expected at this point.

- [ ] **Step 7: Verify public API**

Run: `cargo public-api --simplified 2>/dev/null > /tmp/api_after.txt && diff <(grep -vE "impl core::marker::(Freeze|Send|Sync|Unpin|UnsafeUnpin)|impl core::panic::unwind_safe::(RefUnwindSafe|UnwindSafe)" docs/superpowers/specs/api-baseline.txt) <(grep -vE "impl core::marker::(Freeze|Send|Sync|Unpin|UnsafeUnpin)|impl core::panic::unwind_safe::(RefUnwindSafe|UnwindSafe)" /tmp/api_after.txt)`
Expected: empty diff. (`Value`, `Environment`, `FlowSignal`, `StreamReader` should still be accessible from `mora::*` paths because we re-export via `pub mod value;`.)

- [ ] **Step 8: Commit**

```bash
git add src/lib.rs src/value.rs src/flow.rs src/interpreter.rs
git commit -m "refactor(interpreter): extract value.rs (Value/Environment/FlowSignal/StreamReader)

Step 1 of 8 in interpreter.rs architectural refactor.
- New module: src/value.rs (350 LOC)
- Stubs created: src/flow.rs (filled in next task)
- interpreter.rs: 4413 → ~4188 LOC

Public API unchanged: mora::Value, mora::Environment, mora::FlowSignal
all re-exported via 'pub mod value;'."
```

---

## Task 2: Extract `flow.rs` (control-flow + comparison + JSON stringify)

**Files:**
- Create: `src/flow.rs` (replace stubs with real impls)
- Modify: `src/interpreter.rs:3360-3553` (delete `is_truthy`, `eval_binary`, `numeric_op`, `numeric_cmp`, `values_equal`, `literal_to_value_static`, `check_type`, `type_name`, `value_to_json`)

**Interfaces:**
- Consumes: `crate::value::Value` (from Task 1)
- Produces: `pub fn is_truthy`, `pub fn eval_binary`, `pub fn numeric_op`, `pub fn values_equal`, `pub fn literal_to_value_static`, `pub fn check_type`, `pub fn type_name`, `pub fn value_to_json`, `pub fn expect_string`, `pub fn is_builtin_object`, `pub fn is_pipe_method`

- [ ] **Step 1: Capture pre-move line counts**

Run: `wc -l src/interpreter.rs`
Expected: ~4188

- [ ] **Step 2: Read the functions to move from interpreter.rs**

Read `src/interpreter.rs:3360-3553` to extract:
- `is_truthy(value: &Value) -> bool`
- `eval_binary(left: Value, op: &BinaryOp, right: Value) -> Result<Value, String>`
- `numeric_op<F>(...)`, `numeric_cmp<F>(...)`
- `values_equal(a: &Value, b: &Value) -> bool`
- `literal_to_value_static(lit: &Literal) -> Value`
- `check_type(value: &Value, hint: &str) -> bool`
- `type_name(value: &Value) -> &'static str`
- `value_to_json(value: &Value) -> String`
- (also `expect_string`, `is_builtin_object`, `is_pipe_method` from around 3373-3426)

- [ ] **Step 3: Rewrite `src/flow.rs` with real implementations**

Replace the stub content with the actual functions, adjusting visibility:
- `pub fn` for those that were free functions in interpreter.rs
- Replace `Value` references with `crate::value::Value` (or `use crate::value::Value` at top)
- Replace `BinaryOp` with `crate::ast::BinaryOp`
- Replace `Literal` with `crate::ast::Literal`

Example skeleton:

```rust
//! v0.x: 控制流与值比较工具 (从 interpreter.rs 抽离)。

use crate::ast::{BinaryOp, Literal};
use crate::value::Value;
use std::collections::HashMap;

pub fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Nil => false,
        Value::Bool(b) => *b,
        _ => true,
    }
}

pub fn eval_binary(left: Value, op: &BinaryOp, right: Value) -> Result<Value, String> {
    // ... actual logic from interpreter.rs:3434 ...
}

pub fn numeric_op<F>(left: Value, right: Value, op: F) -> Result<Value, String>
where F: Fn(f64, f64) -> f64 { /* ... */ }

pub fn numeric_cmp<F>(left: Value, right: Value, op: F) -> Result<Value, String>
where F: Fn(f64, f64) -> bool { /* ... */ }

pub fn values_equal(a: &Value, b: &Value) -> bool {
    // ... actual logic from interpreter.rs:3478 ...
}

pub fn literal_to_value_static(lit: &Literal) -> Value {
    // ... actual logic from interpreter.rs:3491 ...
}

pub fn check_type(value: &Value, hint: &str) -> bool {
    // ... actual logic from interpreter.rs:3503 ...
}

pub fn type_name(value: &Value) -> &'static str {
    // ... actual logic from interpreter.rs:3523 ...
}

pub fn value_to_json(value: &Value) -> String {
    // ... actual logic from interpreter.rs:3548 ...
}

pub fn expect_string(value: Value, context: &str) -> Result<String, String> {
    // ... actual logic from interpreter.rs:3373 ...
}

pub fn is_builtin_object(name: &str) -> bool {
    // ... actual logic from interpreter.rs:3368 ...
}

pub fn is_pipe_method(name: &str) -> bool {
    // ... actual logic from interpreter.rs:3426 ...
}

pub fn value_to_string(v: &Value) -> String {
    // matches the old Display::fmt body, used by Value::fmt
    // (move the body of impl Display for Value from interpreter.rs:133 here)
    value_to_json(v)  // simplified; actual body uses Display trait-style formatting
}
```

(If `value_to_string` semantics differ from `value_to_json` in the original code, copy the exact original body.)

- [ ] **Step 4: Delete moved code from `src/interpreter.rs`**

Remove lines 3360–3553 (the 11 free functions now in flow.rs). Also delete any now-unused `use` statements.

Add at the top of `src/interpreter.rs`:

```rust
use crate::flow::{is_truthy, eval_binary, values_equal, literal_to_value_static, check_type, type_name, value_to_json, expect_string, is_builtin_object, is_pipe_method};
```

(Or `use crate::flow::*;` if preferred.)

- [ ] **Step 5: Update `src/value.rs` to reference `flow` correctly**

In `src/value.rs`, the `impl PartialEq for Value` and `impl Display for Value` reference `crate::flow::values_equal` / `crate::flow::value_to_string` — these now exist with real implementations. Build should succeed.

- [ ] **Step 6: Build, test, and verify API**

```bash
cargo build --all-targets 2>&1 | tail -5
cargo test 2>&1 | tail -5
cargo public-api --simplified 2>/dev/null > /tmp/api_after.txt
diff <(grep -vE "impl core::marker::(Freeze|Send|Sync|Unpin|UnsafeUnpin)|impl core::panic::unwind_safe::(RefUnwindSafe|UnwindSafe)" docs/superpowers/specs/api-baseline.txt) <(grep -vE "impl core::marker::(Freeze|Send|Sync|Unpin|UnsafeUnpin)|impl core::panic::unwind_safe::(RefUnwindSafe|UnwindSafe)" /tmp/api_after.txt)
```

All must exit cleanly. 84/84 tests still pass.

- [ ] **Step 7: Commit**

```bash
git add src/flow.rs src/interpreter.rs src/value.rs
git commit -m "refactor(interpreter): extract flow.rs (is_truthy/eval_binary/values_equal)

Step 2 of 8 in interpreter.rs architectural refactor.
- New module: src/flow.rs (200 LOC)
- interpreter.rs: 4188 → ~4000 LOC
- 11 free functions relocated; all call sites use 'use crate::flow::*'

Public API unchanged."
```

---

## Task 3: Extract `json_compat.rs` (hand-written JSON parser)

**Files:**
- Create: `src/json_compat.rs`
- Modify: `src/interpreter.rs:3586-3825` (delete json_to_value + 6 parse_json_*)
- Modify: `src/interpreter.rs:727, 1960, 1985, 2106, 2786, 2852, 2895, 2932, 2979, 3236` (update import to `use crate::json_compat::json_to_value;`)

**Interfaces:**
- Consumes: nothing
- Produces: `pub fn json_to_value(json: &str) -> Result<Value, String>` (signature unchanged)

- [ ] **Step 1: Read the JSON parser to move**

Read `src/interpreter.rs:3586-3825` (240 LOC). Capture all 7 functions: `json_to_value`, `parse_json_value`, `parse_json_string`, `parse_json_number`, `parse_json_bool`, `parse_json_null`, `parse_json_list`, `parse_json_dict`.

- [ ] **Step 2: Create `src/json_compat.rs`**

```rust
//! v0.x: 手写 JSON 解析器(从 interpreter.rs 抽离)。
//! 零外部依赖,纯 std 实现。
//! 仅在 AI 响应解析路径使用,不是公共 API 的一部分。

use crate::value::Value;
use std::collections::HashMap;

pub fn json_to_value(json: &str) -> Result<Value, String> {
    let s = json.trim();
    let (v, consumed) = parse_json_value(s)?;
    if consumed != s.len() {
        return Err(format!("json_to_value: trailing content after value (consumed {} of {} bytes)", consumed, s.len()));
    }
    Ok(v)
}

// ... parse_json_value, parse_json_string, parse_json_number, parse_json_bool,
//     parse_json_null, parse_json_list, parse_json_dict — bodies copied verbatim
//     from interpreter.rs:3594-3825 with only `Value` qualified as `crate::value::Value`
```

- [ ] **Step 3: Add `pub mod json_compat;` to `src/lib.rs`**

After `pub mod flow;`:

```rust
pub mod json_compat;
```

- [ ] **Step 4: Delete moved code from `src/interpreter.rs`**

Remove lines 3586–3825. Add at top:

```rust
use crate::json_compat::json_to_value;
```

- [ ] **Step 5: Build, test, verify API**

```bash
cargo build --all-targets 2>&1 | tail -5
cargo test 2>&1 | tail -5
cargo public-api --simplified 2>/dev/null > /tmp/api_after.txt
diff <(grep -vE "impl core::marker::(Freeze|Send|Sync|Unpin|UnsafeUnpin)|impl core::panic::unwind_safe::(RefUnwindSafe|UnwindSafe)" docs/superpowers/specs/api-baseline.txt) <(grep -vE "impl core::marker::(Freeze|Send|Sync|Unpin|UnsafeUnpin)|impl core::panic::unwind_safe::(RefUnwindSafe|UnwindSafe)" /tmp/api_after.txt)
```

Must pass. 84/84 tests, no API diff.

- [ ] **Step 6: Commit**

```bash
git add src/lib.rs src/json_compat.rs src/interpreter.rs
git commit -m "refactor(interpreter): extract json_compat.rs (hand-written JSON parser)

Step 3 of 8 in interpreter.rs architectural refactor.
- New module: src/json_compat.rs (300 LOC)
- interpreter.rs: 4000 → ~3760 LOC
- json_to_value public API preserved (signature unchanged)

Public API unchanged: mora::json_to_value still accessible."
```

---

## Task 4: Add `thiserror` + `ai_error.rs` (AiError enum)

**Files:**
- Modify: `Cargo.toml` (add `thiserror = "2"`)
- Modify: `src/lib.rs` (add `pub mod ai_error;`)
- Create: `src/ai_error.rs`

**Interfaces:**
- Consumes: nothing
- Produces: `pub enum AiError`, `pub fn AiError::is_retryable(&self) -> bool`, `impl From<AiError> for String`

- [ ] **Step 1: Add thiserror dependency**

Edit `Cargo.toml` `[dependencies]` section:

```toml
# v0.x: type-safe AI error handling (thiserror introduced in sub-project A)
thiserror = "2"
```

- [ ] **Step 2: Refresh lockfile**

Run: `cargo update -p thiserror`
Expected: lockfile gains thiserror + thiserror-impl.

- [ ] **Step 3: Create `src/ai_error.rs`**

```rust
//! v0.x: 类型化 AI 错误。
//!
//! 替代原 stringly-typed 错误字符串(原 is_retryable_error(&str) 模式)。
//! 在 builtin/ai/* 与外部 mora::Result<Value, String> 之间提供结构化诊断。
//!
//! 通过 `impl From<AiError> for String` 自动适配到 `?` 操作符,保持现有调用面不变。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AiError {
    #[error("HTTP {0} from {1}")]
    HttpStatus(u16, String),

    #[error("network error connecting to {url}: {source}")]
    Network {
        url: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("failed to read response body: {0}")]
    BodyRead(String),

    #[error("failed to parse AI response: {0}")]
    Parse(String),

    #[error("retry exhausted after {attempts} attempts; last error: {last}")]
    RetryExhausted {
        attempts: u32,
        #[source]
        last: Box<AiError>,
    },
}

impl AiError {
    /// 是否可重试 — 替代原 stringly-typed is_retryable_error。
    pub fn is_retryable(&self) -> bool {
        match self {
            AiError::Network { .. } => true,
            AiError::HttpStatus(429, _) => true,        // rate limit
            AiError::HttpStatus(500..=599, _) => true,  // server errors
            AiError::BodyRead(_) => false,
            AiError::Parse(_) => false,
            AiError::RetryExhausted { last, .. } => last.is_retryable(),
        }
    }
}

/// AiError → String 自动转换,保持 builtin/ai/* 返回 Result<_, String>。
impl From<AiError> for String {
    fn from(e: AiError) -> String { e.to_string() }
}
```

- [ ] **Step 4: Add `pub mod ai_error;` to `src/lib.rs`**

After `pub mod json_compat;`:

```rust
pub mod ai_error;
```

- [ ] **Step 5: Build, test, audit, verify API**

```bash
cargo build --all-targets 2>&1 | tail -5
cargo test 2>&1 | tail -5
cargo audit 2>&1 | tail -5
cargo public-api --simplified 2>/dev/null > /tmp/api_after.txt
diff <(grep -vE "impl core::marker::(Freeze|Send|Sync|Unpin|UnsafeUnpin)|impl core::panic::unwind_safe::(RefUnwindSafe|UnwindSafe)" docs/superpowers/specs/api-baseline.txt) <(grep -vE "impl core::marker::(Freeze|Send|Sync|Unpin|UnsafeUnpin)|impl core::panic::unwind_safe::(RefUnwindSafe|UnwindSafe)" /tmp/api_after.txt)
```

**New API expected**: `mora::ai_error::AiError` and its variants. The diff should ONLY show additions related to `mora::ai_error::*`. If the diff shows other changes, fix before committing.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/ai_error.rs
git commit -m "feat(ai): introduce AiError typed error (thiserror)

Step 4 of 8 in interpreter.rs architectural refactor.
- New dep: thiserror 2.x
- New module: src/ai_error.rs (90 LOC)
- Public API addition: mora::ai_error::AiError

Existing stringly-typed error paths unchanged (no call sites updated yet).
That happens in Task 5/6 when AiClient is introduced."
```

---

## Task 5: Create `builtin/ai/client.rs` (AiClient shared abstraction)

**Files:**
- Create: `src/builtin/mod.rs` (placeholder)
- Create: `src/builtin/ai/mod.rs` (placeholder)
- Create: `src/builtin/ai/client.rs`
- Modify: `src/lib.rs` (add `mod builtin;`)

**Interfaces:**
- Consumes: `crate::ai_error::AiError`
- Produces: `pub struct AiClient` + `pub fn AiClient::new() -> Result<Self, AiError>` (AiError has `From<AiError> for String` so callers using `?` get String) + `pub fn AiClient::get(&self, url: &str) -> Result<String, AiError>` + `pub fn AiClient::post_json(&self, url: &str, auth_header: Option<(&str, &str)>, body: &str) -> Result<String, AiError>`

**NOTE**: This task CREATES `AiClient` but does NOT replace any existing call sites. Replacement happens in Task 6.

**Convention used throughout this plan**: Function bodies in new modules are marked `// body copied verbatim from interpreter.rs:NNNN` (or similar). The engineer should:
1. Open `src/interpreter.rs` at the indicated line range
2. Copy the function body verbatim
3. Adjust: `self.foo(...)` → `foo(interp, ...)` (or `self` → explicit receiver)
4. Adjust: `Value` (the unqualified name) → `crate::value::Value` (or use `use crate::value::Value` at top)
5. Adjust: any now-cross-module call sites

**For particularly long functions (>50 LOC)**: copy the full body; do NOT rewrite. The plan's purpose is *reorganization*, not *redesign*.

- [ ] **Step 1: Create `src/builtin/mod.rs` (placeholder)**

```rust
//! v0.x: 内置函数注册表与路由。

// 子模块将由 Task 5/6/7 逐步添加。
```

- [ ] **Step 2: Create `src/builtin/ai/mod.rs` (placeholder)**

```rust
//! v0.x: ai.* 内置命名空间。
pub mod client;
// chat / agent / critic / embedding 由后续任务添加。
```

- [ ] **Step 3: Add `mod builtin;` to `src/lib.rs`**

After `pub mod ai_error;`:

```rust
mod builtin;
```

- [ ] **Step 4: Create `src/builtin/ai/client.rs`**

```rust
//! v0.x: 共享 HTTP client + 重试逻辑。
//!
//! 替代原 interpreter.rs 中 3 处重复的 ureq::AgentBuilder + 重试循环 + 错误处理代码。
//! 内部使用 ureq 3.3 API(已在前面 commit 升级)。
//! 错误类型为 crate::ai_error::AiError;自动通过 From 适配到 String。

use crate::ai_error::AiError;
use std::time::Duration;

const HTTP_READ_TIMEOUT_SECS: u64 = 30;
const HTTP_WRITE_TIMEOUT_SECS: u64 = 30;
const AI_READ_TIMEOUT_SECS: u64 = 120;
const DEFAULT_RETRY_MAX: u32 = 3;
const DEFAULT_RETRY_BASE_MS: u64 = 500;

pub struct AiClient {
    agent: ureq::Agent,
    retry_max: u32,
    retry_base_ms: u64,
}

impl AiClient {
    pub fn new() -> Result<Self, AiError> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(AI_READ_TIMEOUT_SECS)))
            .timeout_send_body(Some(Duration::from_secs(HTTP_WRITE_TIMEOUT_SECS)))
            .http_status_as_error(false)
            .build()
            .into();
        Ok(Self {
            agent,
            retry_max: DEFAULT_RETRY_MAX,
            retry_base_ms: DEFAULT_RETRY_BASE_MS,
        })
    }

    /// GET 请求;4xx/5xx 自动转 AiError::HttpStatus。
    pub fn get(&self, url: &str) -> Result<String, AiError> {
        match self.agent.get(url).call() {
            Ok(mut response) => {
                let status = response.status().as_u16();
                let text = response.body_mut().read_to_string()
                    .map_err(|e| AiError::BodyRead(e.to_string()))?;
                if (400..600).contains(&status) {
                    let excerpt: String = text.chars().take(200).collect();
                    Err(AiError::HttpStatus(status, format!("{} (body excerpt: {})", url, excerpt)))
                } else {
                    Ok(text)
                }
            }
            Err(e) => Err(AiError::Network { url: url.to_string(), source: Box::new(e) }),
        }
    }

    /// POST JSON 请求;支持可选 Authorization 头;带重试。
    pub fn post_json(
        &self,
        url: &str,
        auth_header: Option<(&str, &str)>,
        body: &str,
    ) -> Result<String, AiError> {
        self.run_with_retry(|| {
            let mut req = self.agent.post(url);
            if let Some((name, value)) = auth_header {
                req = req.header(name, value);
            }
            match req.header("Content-Type", "application/json").send(body) {
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    let text = response.body_mut().read_to_string()
                        .map_err(|e| AiError::BodyRead(e.to_string()))?;
                    if status >= 400 {
                        let excerpt: String = text.chars().take(300).collect();
                        Err(AiError::HttpStatus(status, format!("{} (body: {})", url, excerpt)))
                    } else {
                        Ok(text)
                    }
                }
                Err(e) => Err(AiError::Network { url: url.to_string(), source: Box::new(e) }),
            }
        })
    }

    fn run_with_retry<F, T>(&self, op: F) -> Result<T, AiError>
    where
        F: Fn() -> Result<T, AiError>,
    {
        let mut last_err: Option<AiError> = None;
        for attempt in 0..=self.retry_max {
            if attempt > 0 {
                let sleep_ms = self.retry_base_ms * (1 << (attempt - 1)).min(8);
                std::thread::sleep(Duration::from_millis(sleep_ms));
            }
            match op() {
                Ok(v) => return Ok(v),
                Err(e) if e.is_retryable() && attempt < self.retry_max => {
                    last_err = Some(e);
                }
                Err(e) => return Err(e),
            }
        }
        Err(AiError::RetryExhausted {
            attempts: self.retry_max,
            last: Box::new(last_err.expect("at least one error after loop")),
        })
    }
}
```

- [ ] **Step 5: Build, test**

```bash
cargo build --all-targets 2>&1 | tail -5
cargo test 2>&1 | tail -5
```

- [ ] **Step 6: Commit**

```bash
git add src/lib.rs src/builtin/
git commit -m "refactor(ai): introduce AiClient shared HTTP/retry abstraction

Step 5 of 8 in interpreter.rs architectural refactor.
- New module: src/builtin/ai/client.rs (AiClient, 250 LOC)
- Replaces duplicated ureq::AgentBuilder + retry logic
  (to be applied at call sites in Task 6)
- Errors typed as crate::ai_error::AiError
- No call sites updated yet — existing 3 AI call sites unchanged

Public API unchanged (AiClient is pub(crate))."
```

---

## Task 6: Replace 3 AI client call sites with AiClient

**Files:**
- Create: `src/builtin/http.rs` (moves `web.fetch` from interpreter.rs:2517-2574)
- Create: `src/builtin/ai/chat.rs` (moves `ai.chat` from interpreter.rs:2576-2706)
- Modify: `src/builtin/ai/mod.rs` (add `pub mod chat;`)
- Modify: `src/interpreter.rs:2517-2706` (delete moved code; replace with call to builtin::*)

**Interfaces:**
- Consumes: `crate::builtin::ai::client::AiClient`
- Produces: `pub fn web_fetch(url: &str) -> Result<String, String>`, `pub fn ai_chat(...) -> Result<Value, String>`

- [ ] **Step 1: Create `src/builtin/http.rs` with the web.fetch impl**

```rust
//! v0.x: web.fetch 内置函数。
//!
//! 替代原 interpreter.rs:2517-2574 重复的 ureq 客户端代码。

use crate::builtin::ai::client::AiClient;

pub fn web_fetch(url: &str) -> Result<String, String> {
    let client = AiClient::new()?;
    client.get(url).map_err(|e| e.to_string())
}
```

- [ ] **Step 2: Create `src/builtin/ai/chat.rs` with the ai.chat impl**

Read `src/interpreter.rs:2576-2706` first to extract the full `ai_chat` body (it has retry loop, response body extraction, etc.). Then:

```rust
//! v0.x: ai.chat 内置函数。
//!
//! 替代原 interpreter.rs:2576-2706 重复的 ureq 客户端代码。

use crate::builtin::ai::client::AiClient;
use crate::value::Value;
use std::time::Duration;

pub fn ai_chat(base_url: &str, api_key: &str, body: &str, timeout_secs: u64) -> Result<Value, String> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let client = AiClient::new()?;
    let text = client.post_json(
        &url,
        Some(("Authorization", &format!("Bearer {}", api_key))),
        body,
    ).map_err(|e| e.to_string())?;
    // 提取 choices[0].message.content(原 extract_ai_content)
    // body copied verbatim from interpreter.rs:2576-2706
    // (extract_ai_content stays in interpreter.rs initially; only the HTTP/retry/parse dispatch moves)
    Ok(Value::String(text))  // simplified
}
```

- [ ] **Step 3: Update `src/builtin/ai/mod.rs` to declare `chat`**

```rust
//! v0.x: ai.* 内置命名空间。
pub mod chat;
pub mod client;
// agent / critic / embedding 由后续任务添加。
```

- [ ] **Step 4: Delete moved code from `src/interpreter.rs`**

Remove lines 2517–2706 (`web_fetch` and `ai_chat` standalone functions). Replace each call site with a call to `crate::builtin::http::web_fetch(...)` / `crate::builtin::ai::chat::ai_chat(...)`.

- [ ] **Step 5: Build, test, verify API**

```bash
cargo build --all-targets 2>&1 | tail -5
cargo test 2>&1 | tail -5
cargo public-api --simplified 2>/dev/null > /tmp/api_after.txt
diff <(grep -vE "impl core::marker::(Freeze|Send|Sync|Unpin|UnsafeUnpin)|impl core::panic::unwind_safe::(RefUnwindSafe|UnwindSafe)" docs/superpowers/specs/api-baseline.txt) <(grep -vE "impl core::marker::(Freeze|Send|Sync|Unpin|UnsafeUnpin)|impl core::panic::unwind_safe::(RefUnwindSafe|UnwindSafe)" /tmp/api_after.txt)
```

- [ ] **Step 6: Commit**

```bash
git add src/builtin/ src/interpreter.rs
git commit -m "refactor(builtin): replace 2/3 AI call sites with AiClient

Step 6 of 8 in interpreter.rs architectural refactor.
- New module: src/builtin/http.rs (web.fetch, 30 LOC)
- New module: src/builtin/ai/chat.rs (ai.chat, 130 LOC)
- interpreter.rs: 3760 → ~3500 LOC
- 60+ lines of duplicated ureq code eliminated
- real_ai_chat_with_tools (3rd call site) deferred to Task 7

Public API unchanged."
```

---

## Task 7: Split `impl Interpreter` into `eval/*` + `builtin/ai/*`

**Files:**
- Create: `src/eval/mod.rs`, `src/eval/call.rs`, `src/eval/methods.rs`, `src/eval/prompt.rs`
- Create: `src/builtin/ai/agent.rs`, `src/builtin/ai/critic.rs`, `src/builtin/ai/embedding.rs`, `src/builtin/io.rs`
- Modify: `src/lib.rs` (add `mod eval;`)
- Modify: `src/builtin/ai/mod.rs` (add new sub-module declarations)
- Modify: `src/builtin/mod.rs` (add `pub mod io;` and routing logic)
- Modify: `src/interpreter.rs:505-3235` (massively slim down — move methods out)

**This is the largest task** (~120 minutes estimated). It is broken into sub-tasks below to keep individual commits reviewable.

### Task 7a: Extract `eval/call.rs` (call_function/call_task/call_closure/call_method/call_value)

- [ ] **Step 1: Create `src/eval/mod.rs` (placeholder)**

```rust
//! v0.x: 表达式/语句求值核心。
pub mod call;
pub mod methods;
pub mod prompt;
```

- [ ] **Step 2: Add `mod eval;` to `src/lib.rs`**

After `mod builtin;`:

```rust
mod eval;
```

- [ ] **Step 3: Read methods to move**

Read `src/interpreter.rs:1726-2263` to extract:
- `fn call_function(&mut self, name: &str, args: Vec<Value>, call_site: Span) -> Result<Value, String>`
- `fn call_task(&mut self, params: &[String], body: &[Stmt], args: Vec<Value>) -> Result<Value, String>`
- `fn call_closure(&mut self, params: &[String], body: &[Stmt], env: Arc<Mutex<Environment>>, args: Vec<Value>) -> Result<Value, String>`
- `fn call_method(&mut self, mut object: Value, method: &str, args: Vec<Value>, call_site: Span) -> Result<Value, String>`
- `pub fn call_value(&mut self, value: &Value, args: Vec<Value>) -> Result<Value, String>`

- [ ] **Step 4: Create `src/eval/call.rs`**

Each `call_*` method becomes a free function taking `&mut Interpreter` explicitly (they already take `&mut self`):

```rust
//! v0.x: 函数/任务/闭包/方法调用分发。

use crate::ast::{Span, Stmt};
use crate::interpreter::Interpreter;
use crate::value::{Environment, Value};
use std::sync::{Arc, Mutex};

pub fn call_function(
    interp: &mut Interpreter,
    name: &str,
    args: Vec<Value>,
    call_site: Span,
) -> Result<Value, String> {
    // body copied verbatim from interpreter.rs:1726
    // signature change: self → interp (no longer a method)
}

pub fn call_task(
    interp: &mut Interpreter,
    params: &[String],
    body: &[Stmt],
    args: Vec<Value>,
) -> Result<Value, String> {
    // body copied verbatim from interpreter.rs:1804
}

pub fn call_closure(
    interp: &mut Interpreter,
    params: &[String],
    body: &[Stmt],
    env: Arc<Mutex<Environment>>,
    args: Vec<Value>,
) -> Result<Value, String> {
    // body copied verbatim from interpreter.rs:1825
}

pub fn call_method(
    interp: &mut Interpreter,
    object: Value,
    method: &str,
    args: Vec<Value>,
    call_site: Span,
) -> Result<Value, String> {
    // body copied verbatim from interpreter.rs:1870
}

pub fn call_value(
    interp: &mut Interpreter,
    value: &Value,
    args: Vec<Value>,
) -> Result<Value, String> {
    // body copied verbatim from interpreter.rs:2236
}
```

- [ ] **Step 5: Delete moved methods from `src/interpreter.rs`**

Remove lines 1726–2263. Replace internal call sites with `crate::eval::call::call_function(self, ...)` etc.

- [ ] **Step 6: Build, test**

```bash
cargo build --all-targets 2>&1 | tail -5
cargo test 2>&1 | tail -5
```

- [ ] **Step 7: Commit (intermediate)**

```bash
git add src/eval/ src/interpreter.rs
git commit -m "refactor(eval): extract eval/call.rs (5 call_* functions)

Step 7a of 8 in interpreter.rs architectural refactor.
- New module: src/eval/call.rs (500 LOC)
- interpreter.rs: 3500 → ~3050 LOC
- 5 methods relocated; all call sites use 'crate::eval::call::*'

Public API unchanged."
```

### Task 7b: Extract `eval/methods.rs` (call_file_method)

- [ ] **Step 1: Read call_file_method**

Read `src/interpreter.rs:2263-2500` to extract `fn call_file_method(&self, method: &str, args: &[Value]) -> Result<Value, String>`.

- [ ] **Step 2: Create `src/eval/methods.rs`**

```rust
//! v0.x: 内置方法分发(file/stream/object 等)。

use crate::interpreter::Interpreter;
use crate::value::Value;

pub fn call_file_method(
    interp: &Interpreter,
    method: &str,
    args: &[Value],
) -> Result<Value, String> {
    // body copied verbatim from interpreter.rs:2263
}
```

- [ ] **Step 3: Delete from `src/interpreter.rs` and verify**

```bash
cargo build --all-targets 2>&1 | tail -5
cargo test 2>&1 | tail -5
```

- [ ] **Step 4: Commit**

```bash
git add src/eval/methods.rs src/interpreter.rs
git commit -m "refactor(eval): extract eval/methods.rs (call_file_method)

Step 7b of 8 in interpreter.rs architectural refactor.
- New module: src/eval/methods.rs (400 LOC)
- interpreter.rs: 3050 → ~2700 LOC

Public API unchanged."
```

### Task 7c: Extract `eval/prompt.rs` (eval_prompt_parts, eval_route_arg, etc.)

- [ ] **Step 1: Read prompt/arg evaluators**

Read `src/interpreter.rs:1615-1726` for:
- `fn eval_route_arg(arg: &Expr, interp: &mut Interpreter) -> Result<String, String>`
- `fn eval_prompt_parts(parts: &[Expr], interp: &mut Interpreter) -> Result<String, String>`
- `fn eval_prompt_parts_from_stmt(prompt_expr: &Expr, interp: &mut Interpreter) -> Result<String, String>`

- [ ] **Step 2: Create `src/eval/prompt.rs`**

```rust
//! v0.x: prompt 与 route 参数求值。

use crate::ast::Expr;
use crate::interpreter::Interpreter;

pub fn eval_route_arg(arg: &Expr, interp: &mut Interpreter) -> Result<String, String> { /* ... */ }
pub fn eval_prompt_parts(parts: &[Expr], interp: &mut Interpreter) -> Result<String, String> { /* ... */ }
pub fn eval_prompt_parts_from_stmt(prompt_expr: &Expr, interp: &mut Interpreter) -> Result<String, String> { /* ... */ }
```

- [ ] **Step 3: Delete and verify**

```bash
cargo build --all-targets 2>&1 | tail -5
cargo test 2>&1 | tail -5
```

- [ ] **Step 4: Commit**

```bash
git add src/eval/prompt.rs src/interpreter.rs
git commit -m "refactor(eval): extract eval/prompt.rs (eval_prompt_parts/eval_route_arg)

Step 7c of 8 in interpreter.rs architectural refactor.
- New module: src/eval/prompt.rs (150 LOC)
- interpreter.rs: 2700 → ~2570 LOC

Public API unchanged."
```

### Task 7d: Extract `builtin/ai/embedding.rs` (cosine/dot/euclidean/l2/mock_bow)

- [ ] **Step 1: Read embedding functions**

Read `src/interpreter.rs:3826-3870` (cosine_similarity, dot_product, euclidean_distance, l2_norm) and `3294-3335` (mock_bow_embedding).

- [ ] **Step 2: Create `src/builtin/ai/embedding.rs`**

```rust
//! v0.x: 向量相似度/距离工具 + bag-of-words 嵌入。
//!
//! v0.04 补:ai.cosine/dot/euclidean/norm 推迟到 v1.0。
//! 保留为 "v1.0 复活点" + 内部测试用。

use crate::value::Value;

#[allow(dead_code)]
pub fn cosine_similarity(a: &[f64], b: &[f64]) -> Result<f64, String> {
    // body copied verbatim from interpreter.rs:3826
}

#[allow(dead_code)]
pub fn dot_product(a: &[f64], b: &[f64]) -> Result<f64, String> {
    // body copied verbatim from interpreter.rs:3841
}

#[allow(dead_code)]
pub fn euclidean_distance(a: &[f64], b: &[f64]) -> Result<f64, String> {
    // body copied verbatim from interpreter.rs:3850
}

#[allow(dead_code)]
pub fn l2_norm(a: &[f64]) -> f64 {
    // body copied verbatim from interpreter.rs:3859
}

#[allow(dead_code)]
pub fn mock_bow_embedding(s: &str) -> Vec<f64> {
    // body copied verbatim from interpreter.rs:3294
}
```

- [ ] **Step 3: Add to `src/builtin/ai/mod.rs`**

```rust
pub mod chat;
pub mod client;
pub mod embedding;
// agent / critic 由后续子任务添加。
```

- [ ] **Step 4: Delete and verify**

```bash
cargo build --all-targets 2>&1 | tail -5
cargo test 2>&1 | tail -5
```

- [ ] **Step 5: Commit**

```bash
git add src/builtin/ai/ src/interpreter.rs
git commit -m "refactor(builtin): extract builtin/ai/embedding.rs (vector math)

Step 7d of 8 in interpreter.rs architectural refactor.
- New module: src/builtin/ai/embedding.rs (200 LOC)
- interpreter.rs: 2570 → ~2400 LOC
- 5 functions relocated (all #[allow(dead_code)] v1.0 revival anchors)

Public API unchanged."
```

### Task 7e: Extract `builtin/ai/agent.rs` and `builtin/ai/critic.rs`

- [ ] **Step 1: Read run_agent and run_critic**

Read `src/interpreter.rs:3133-3235` (run_agent) and `3006-3132` (run_critic).

- [ ] **Step 2: Create `src/builtin/ai/agent.rs`**

```rust
//! v0.x: run_agent 内部方法(原 interpreter.rs:3133)。

use crate::interpreter::Interpreter;
use crate::value::Value;

pub fn run_agent(
    interp: &mut Interpreter,
    agent_name: &str,
    tool_names: &[String],
    model_route: &str,
    max_steps: usize,
    system: &str,
    task: &str,
) -> Result<Value, String> {
    // body copied verbatim from interpreter.rs:3133
}
```

- [ ] **Step 3: Create `src/builtin/ai/critic.rs`**

```rust
//! v0.x: run_critic 内部方法(原 interpreter.rs:3006)。

use crate::interpreter::Interpreter;
use crate::value::Value;

pub fn run_critic(
    interp: &mut Interpreter,
    answer: &str,
    context: Option<&str>,
) -> Result<Value, String> {
    // body copied verbatim from interpreter.rs:3006
}
```

- [ ] **Step 4: Add to `src/builtin/ai/mod.rs`**

```rust
pub mod agent;
pub mod chat;
pub mod client;
pub mod critic;
pub mod embedding;
```

- [ ] **Step 5: Delete from interpreter.rs and verify**

```bash
cargo build --all-targets 2>&1 | tail -5
cargo test 2>&1 | tail -5
```

- [ ] **Step 6: Commit**

```bash
git add src/builtin/ai/ src/interpreter.rs
git commit -m "refactor(builtin): extract builtin/ai/agent.rs and critic.rs

Step 7e of 8 in interpreter.rs architectural refactor.
- New module: src/builtin/ai/agent.rs (250 LOC)
- New module: src/builtin/ai/critic.rs (100 LOC)
- interpreter.rs: 2400 → ~2050 LOC
- 2 methods relocated

Public API unchanged."
```

### Task 7f: Final interpreter.rs cleanup + finalize `eval/mod.rs`

- [ ] **Step 1: Inventory remaining impl Interpreter methods**

Run: `grep -nE "    pub fn |    fn " src/interpreter.rs | wc -l`
Expected: < 30 (was 104)

- [ ] **Step 2: Finalize `src/eval/mod.rs`**

The placeholder should now declare all submodules and may need helper `use` statements:

```rust
//! v0.x: 表达式/语句求值核心。
pub mod call;
pub mod methods;
pub mod prompt;

use crate::ast::Stmt;
use crate::interpreter::Interpreter;
use crate::value::Value;

/// 求值入口 — 原 impl Interpreter::eval_expr / eval_stmt。
pub fn eval_expr(interp: &mut Interpreter, expr: &crate::ast::Expr) -> Result<Value, String> { /* ... */ }
pub fn eval_stmt(interp: &mut Interpreter, stmt: &crate::ast::Stmt) -> Result<Value, String> { /* ... */ }
```

If the original `eval_expr`/`eval_stmt` are very long (>500 LOC), keep them in interpreter.rs as `impl Interpreter` methods and reference them from `eval::mod`. Adjust strategy based on actual size.

- [ ] **Step 3: Run all global verification**

```bash
cargo build --all-targets 2>&1 | tail -5
cargo build --release --all-targets 2>&1 | tail -5
cargo test 2>&1 | tail -5
cargo clippy --all-targets 2>&1 | grep -cE "^warning: "
cargo +nightly udeps --all-targets 2>&1 | tail -3
cargo audit 2>&1 | tail -3
cargo public-api --simplified 2>/dev/null > /tmp/api_after.txt
diff <(grep -vE "impl core::marker::(Freeze|Send|Sync|Unpin|UnsafeUnpin)|impl core::panic::unwind_safe::(RefUnwindSafe|UnwindSafe)" docs/superpowers/specs/api-baseline.txt) <(grep -vE "impl core::marker::(Freeze|Send|Sync|Unpin|UnsafeUnpin)|impl core::panic::unwind_safe::(RefUnwindSafe|UnwindSafe)" /tmp/api_after.txt)
wc -l src/interpreter.rs
```

Expected:
- All builds 0 warnings
- Test 84/84 passed, 0 failed
- clippy warning count = 35 (unchanged from pre-refactor)
- udeps: "All deps seem to have been used"
- audit: 0 vulnerabilities
- API diff: empty (modulo impl marker fluctuation)
- interpreter.rs LOC: < 1500

- [ ] **Step 4: Commit (final of task 7)**

```bash
git add src/eval/ src/interpreter.rs
git commit -m "refactor(eval): finalize eval/mod.rs entry points

Step 7f of 8 in interpreter.rs architectural refactor.
- eval/mod.rs now exports eval_expr/eval_stmt entry points
- interpreter.rs: 2050 → < 1500 LOC (target met)
- 84/84 tests pass; 0 clippy warnings added; 0 API diff
- This commit is the final structural split before Task 8

Public API unchanged."
```

---

## Task 8: Migrate test fixtures + final spec verification

**Files:**
- Modify: `src/interpreter.rs:4070-4413` (move trait test fixtures)
- Create: `src/eval/call.rs` `#[cfg(test)] mod tests` (or `src/eval/call_tests.rs`)

**Decision** (per spec §4.3): tests stay in production files. They are migrated from `src/interpreter.rs` end-of-file `#[cfg(test)] mod tests` to `src/eval/call.rs` end-of-file `#[cfg(test)] mod tests` because `test_trait_basic_dispatch` / `test_trait_inherit_construction_checks_parents` exercise `call_function` / `call_task` / `call_closure` / `call_method`.

- [ ] **Step 1: Read existing test fixtures**

Read `src/interpreter.rs:4070-4413` (the entire `#[cfg(test)] mod tests { ... }` block).

- [ ] **Step 2: Identify which tests belong in `eval/call.rs`**

Tests that exercise:
- `call_function` / `call_task` / `call_closure` / `call_method` → move to `src/eval/call.rs`

Tests that exercise other functionality stay in `src/interpreter.rs` (or move to wherever the production code lives).

- [ ] **Step 3: Move the trait dispatch tests**

Append to `src/eval/call.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    // ... moved test bodies from interpreter.rs:4070-4413 ...
    // Adjust helper invocations to use the new function-style API:
    //   self.call_function(...) → call_function(self, ...)
    //   self.call_task(...) → call_task(self, ...)
    // etc.
}
```

- [ ] **Step 4: Delete moved tests from `src/interpreter.rs`**

Remove the `#[cfg(test)] mod tests { ... }` block (or just the moved tests, keeping others).

- [ ] **Step 5: Run full verification**

```bash
cargo build --all-targets 2>&1 | tail -5
cargo test 2>&1 | tail -5
cargo clippy --all-targets 2>&1 | grep -cE "^warning: "
cargo public-api --simplified 2>/dev/null > /tmp/api_after.txt
diff <(grep -vE "impl core::marker::(Freeze|Send|Sync|Unpin|UnsafeUnpin)|impl core::panic::unwind_safe::(RefUnwindSafe|UnwindSafe)" docs/superpowers/specs/api-baseline.txt) <(grep -vE "impl core::marker::(Freeze|Send|Sync|Unpin|UnsafeUnpin)|impl core::panic::unwind_safe::(RefUnwindSafe|UnwindSafe)" /tmp/api_after.txt)
wc -l src/interpreter.rs src/value.rs src/flow.rs src/json_compat.rs src/ai_error.rs src/eval/*.rs src/builtin/*.rs src/builtin/ai/*.rs
```

Expected:
- 84/84 tests pass
- 0 clippy warnings added (35 pre-existing)
- API diff empty
- `interpreter.rs` < 1500 LOC
- New modules exist with their target LOC

- [ ] **Step 6: Commit**

```bash
git add src/interpreter.rs src/eval/call.rs
git commit -m "test(eval): migrate trait dispatch tests to eval/call.rs

Step 8 of 8 in interpreter.rs architectural refactor.
- 230 lines of trait dispatch tests moved from interpreter.rs
  to src/eval/call.rs (where the production code they exercise now lives)
- interpreter.rs final LOC: < 1500 (target met)
- 84/84 tests pass; 0 clippy warnings added; 0 API diff
- Sub-project A complete

Public API unchanged."
```

- [ ] **Step 7: Final spec acceptance — all DoD items checked**

Review §9 of `docs/superpowers/specs/2026-06-23-subproject-a-interpreter-architecture-design.md`:
- [ ] 8 migration steps complete (8 commits in git log: `git log --oneline | head -8`)
- [ ] `interpreter.rs` LOC < 1500
- [ ] `impl Interpreter` methods < 30 (was 104)
- [ ] AI client code duplication = 0 (single AiClient)
- [ ] Public API surface byte-identical to baseline
- [ ] `cargo test` 84/84 pass
- [ ] `cargo build --all-targets` 0 warnings
- [ ] `cargo clippy` no new warnings
- [ ] `cargo audit` 0 vulnerabilities
- [ ] `cargo +nightly udeps` all used
- [ ] Spec doc committed to `docs/superpowers/specs/...` (already done in brainstorm step)

**Sub-project A complete. Ready for sub-project B (unsafe/panic centralization).**
