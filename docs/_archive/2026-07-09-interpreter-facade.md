# Interpreter Facade ADR-001

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:**  ADR-001  `Interpreter` god object33  / 43  Clone + 6  Domain Facade **Plateau A ** 

**Architecture:**  ADR-001 §1 BC  6 facade`InfraRuntime` / `AiRuntime` / `OrchRuntime` / `PersistRuntime` / `SandboxRuntime` / `RegistryRuntime` + 1  `CoreRuntime``Interpreter`  7  facade `pub(crate)` private facade `&mut AiRuntime`  owned  borrow 8  commit commit  1 facade 1 commit  `builtins.rs` commit  3-5  facade 

**Tech Stack:** Rust 2024 edition /  `Interpreter` / `Arc<Mutex<>>`  /  613 tests  / `#[cfg(test)]`  / `cargo test --all` + `cargo clippy --all-targets --all-features -- -D warnings`

**Spec:** [`../specs/2026-07-09-interpreter-facade.md`](../specs/2026-07-09-interpreter-facade.md)

---

## commit 1 

/

```
src/
 runtime/                          ← commit 1 
    mod.rs                        ← commit 1
    core.rs                       ← commit 7
    ai.rs                         ← commit 2
    orch.rs                       ← commit 3
    persist.rs                    ← commit 4
    sandbox.rs                    ← commit 5
    registry.rs                   ← commit 6
    infra.rs                      ← commit 1
 interpreter/
    mod.rs                        ← 8 commits 
    builtins/                     ← commit 8
       mod.rs
       file.rs
       sandbox.rs
       ai.rs
       memory.rs
       schedule.rs
       json.rs
       document.rs
       toolplane.rs
       mora.rs
    builtins.rs                   ← commit 8 
    dispatch.rs                   ← commit 8  MethodDispatch trait
```

---

## Task 1:  `InfraRuntime` facade

**Files:**
- Create: `src/runtime/mod.rs`
- Create: `src/runtime/infra.rs`
- Modify: `src/interpreter/mod.rs:203-326` InfraRuntime  +  new/Clone
- Modify:  `interp.recorder` / `interp.string_interner` / `interp.ai_cache` / `interp.bus` / `interp.scheduler`  30+ 

****** sub-task**  plan

### Task 1.1:  `src/runtime/mod.rs`

- [ ] **Step 1:  mod **

```rust
//! v0.52 ADR-001: 6 Domain Facade 
//!
//!  facade  BC  + 
//! - AiRuntime       (BC3)
//! - OrchRuntime     (BC4)
//! - PersistRuntime  (BC5)
//! - SandboxRuntime  (BC7)
//! - RegistryRuntime (BC8)
//! - InfraRuntime    (BC9)
//!
//!  facade  &mut facade borrow 

pub mod ai;
pub mod core;
pub mod infra;
pub mod orch;
pub mod persist;
pub mod registry;
pub mod sandbox;
```

- [ ] **Step 2: **

Run: `cd "D:/Github/mora-lang" && cargo build --all-targets 2>&1 | tail -5`
Expected: ai/core/orch/persist/registry/sandbox — 

### Task 1.2:  `src/runtime/infra.rs`

- [ ] **Step 1:  InfraRuntime struct + Default + **

```rust
//! v0.52 ADR-001: InfraRuntime — BC9 (scheduling +  + recorder)

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use crate::event::EventBus;
use crate::record::Recorder;
use crate::schedule::Scheduler;
use crate::interpreter::LruCache;
use crate::value::Value;

#[derive(Debug, Clone)]
pub struct InfraRuntime {
    pub recorder: Recorder,
    pub string_interner: Arc<Mutex<LruCache<Value>>>,
    pub ai_cache: Arc<Mutex<LruCache<String>>>,
    pub bus: EventBus,
    pub scheduler: Scheduler,
}

impl Default for InfraRuntime {
    fn default() -> Self {
        Self {
            recorder: Recorder::new_off(),
            string_interner: Arc::new(Mutex::new(LruCache::new(50000))),
            ai_cache: Arc::new(Mutex::new(LruCache::new(10000))),
            bus: EventBus::new(),
            scheduler: Scheduler::default(),
        }
    }
}

impl InfraRuntime {
    ///  thread_id  recorder
    pub fn with_recorder_thread(mut self, thread_id: String) -> Self {
        self.recorder = Recorder::new_off().with_thread(thread_id);
        self
    }

    /// 
    pub fn intern_string(&self, val: Value) -> u64 {
        self.string_interner.lock().put(val)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_infra_default() {
        let infra = InfraRuntime::default();
        assert!(matches!(infra.recorder.state(), RecordState::Off));
    }

    #[test]
    fn string_interner_dedups() {
        let infra = InfraRuntime::default();
        let id1 = infra.intern_string(Value::String("hello".into()));
        let id2 = infra.intern_string(Value::String("hello".into()));
        assert_eq!(id1, id2);
    }

    #[test]
    fn ai_cache_starts_empty() {
        let infra = InfraRuntime::default();
        assert!(infra.ai_cache.lock().get("anything").is_none());
    }

    #[test]
    fn bus_default_constructor() {
        let _bus = InfraRuntime::default().bus;
    }

    #[test]
    fn scheduler_default_constructor() {
        let _sched = InfraRuntime::default().scheduler;
    }
}
```

> ****`RecordState` / `LruCache::new` / `Recorder::new_off()` `src/interpreter/mod.rs:139-179`  `src/record/mod.rs` 

- [ ] **Step 2:  mod.rs  `pub mod infra;`**

 1.1 Step 1 

- [ ] **Step 3:  infra.rs **

Run: `cd "D:/Github/mora-lang" && cargo build -p mora --lib 2>&1 | tail -20`
Expected:  facade  —  stub 1.3

### Task 1.3:  5  facade  stub

- [ ] **Step 1:  `src/runtime/`  ai.rs / orch.rs / persist.rs / sandbox.rs / registry.rs / core.rs **



```rust
//! Stub —  commit 
```

- [ ] **Step 2: **

Run: `cd "D:/Github/mora-lang" && cargo build --all-targets 2>&1 | tail -5`
Expected: PASS unused warning

### Task 1.4:  Interpreter  InfraRuntime 

- [ ] **Step 1:  `Interpreter` struct**

 `src/interpreter/mod.rs:203-275` 
- `recorder: crate::record::Recorder`
- `string_interner: std::sync::Arc<Mutex<LruCache<Value>>>`
- `ai_cache: std::sync::Arc<Mutex<LruCache<String>>>`
- `bus: crate::event::EventBus`
- `scheduler: crate::schedule::Scheduler`


```rust
pub(crate) infra: crate::runtime::infra::InfraRuntime,
```

- [ ] **Step 2:  `Interpreter::new()`**


```rust
infra: InfraRuntime::default(),
```

- [ ] **Step 3:  `Clone for Interpreter`**

`recorder: crate::record::Recorder::new_off()` → `infra: InfraRuntime::default()`Clone  default Arc
> **** `infra`  Clone Arc clone  O(1)


```rust
infra: self.infra.clone(),
```

- [ ] **Step 4: grep **

Run: `cd "D:/Github/mora-lang" && grep -rn "self.recorder\|self.string_interner\|self.ai_cache\|self.bus\|self.scheduler" src/ --include="*.rs" 2>&1 | head -30`

 `self.infra.recorder` / `self.infra.string_interner` / `self.infra.ai_cache` / `self.infra.bus` / `self.infra.scheduler`

> ** facade  facade** `&Interpreter`  spec §2.3 

- [ ] **Step 5:  4 **

```bash
cd "D:/Github/mora-lang"
cargo build --all-targets 2>&1 | tail -3
cargo test --all 2>&1 | grep "test result:" | head -3
cargo fmt --check 2>&1 | head -3
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3
```

Expected:  PASS ≥ 618613  + 5  InfraRuntime unit

- [ ] **Step 6: Commit**

```bash
cd "D:/Github/mora-lang"
git add src/runtime/ src/interpreter/mod.rs
git commit -m "refactor(ADR-001):  InfraRuntime facade

-  src/runtime/{mod,infra}.rs
- Interpreter  recorder/string_interner/ai_cache/bus/scheduler 5 
-  pub(crate) infra: InfraRuntime
- 30+  self.infra.xxx
- 5  InfraRuntime 


  cargo build --all-targets  PASS
  cargo test --all          618+ passed / 0 failed / 14 ignored
  cargo fmt --check         PASS
  cargo clippy -D warnings  PASS"
```

---

## Task 2-6:  AiRuntime / OrchRuntime / PersistRuntime / SandboxRuntime / RegistryRuntime

** facade 1  commit** Task 1  facade  →  Interpreter  →  →  4  → commit

### Task 2:  AiRuntime

**Files:** `src/runtime/ai.rs`  / `src/interpreter/mod.rs`  9  / ~50 

 spec §2.2
- `model_routes: HashMap<String, RouteConfig>`
- `token_budget: Option<TokenBudget>`
- `token_usage: TokenUsage`
- `trace: TraceCollector`
- `context_window: ContextWindow`
- `speculative_verifier: SpeculativeVerifier`
- `cache_warmer: CacheWarmer`**`#[allow(dead_code)]` **
- `draft_model_stats: Arc<Mutex<HashMap<String, (usize, usize)>>>`

> ****`trace: TraceCollector`  `pub`  —  facade  `pub(crate)` `interp.ai.trace`

AiRuntime 
- `record_token(input, output)`
- `get_cached(prompt_hash) -> Option<String>`
- `cache_response(prompt_hash, response)`

5 
- `default_ai_routes_empty`
- `token_budget_set_and_get`
- `token_usage_increments`
- `trace_records_event`
- `ai_cache_put_and_get`

### Task 3:  OrchRuntime

**Files:** `src/runtime/orch.rs`  / Interpreter  3  / ~20 


- `plans: Arc<Mutex<HashMap<String, Plan>>>`
- `refine_registry: Arc<Mutex<RefineRegistry>>`
- `skill_registry: Arc<Mutex<SkillRegistry>>`

OrchRuntime 
- `plan_create(name, steps)`
- `plan_update(name, updates)`
- `skill_load(path)` / `skill_install(path)`

5 
- `plans_default_empty`
- `refine_registry_default`
- `skill_registry_default`
- `plan_create_returns_name`
- `refine_registry_registers_session`

### Task 4:  PersistRuntime

**Files:** `src/runtime/persist.rs`  / Interpreter  3  / ~10 


- `audit_sink: Arc<dyn AuditSink>`
- `markdown_memory_dir: Option<PathBuf>`
- `checkpoint_saver: Option<Arc<dyn CheckpointSaver>>`

PersistRuntime 
- `save_checkpoint(thread_id, checkpoint)`
- `load_checkpoint(thread_id) -> Option<Checkpoint>`
- `audit_event(event)`

3 
- `default_persist_null_sink`
- `markdown_memory_dir_default_none`
- `checkpoint_saver_default_none`

### Task 5:  SandboxRuntime

**Files:** `src/runtime/sandbox.rs`  / Interpreter  4  / ~20 


- `sandbox: SandboxPolicy`
- `container: Arc<Mutex<Option<ContainerHandle>>>`
- `tool_planes: Arc<Mutex<ToolPlaneRegistry>>`
- capability  `src/sandbox/capability.rs`  module-level state

> ****`ContainerHandle`  Drop implv0.49 C3 fix Drop 

5 
- `default_sandbox_policy`
- `container_default_none`
- `tool_planes_default_has_core`
- `sandbox_validate_path` (safe path)
- `sandbox_validate_path_rejected` (escape attempt)

### Task 6:  RegistryRuntime

**Files:** `src/runtime/registry.rs`  / Interpreter  5  / ~30 


- `trait_registry: Arc<HashMap<String, TraitInfo>>`
- `impl_table: Arc<HashMap<String, Vec<String>>>`
- `mock_registry: MockRegistry`
- `ccr_store: InMemoryCcrStore`
- `memory_store: HashMap<String, Value>`

5 
- `default_registry_empty_traits`
- `mock_registry_default`
- `ccr_store_default_empty`
- `memory_store_default_empty`
- `registry_trait_lookup` (after insert)

---

## Task 7:  CoreRuntime + Interpreter 

**Files:** `src/runtime/core.rs`  / `src/interpreter/mod.rs`  / 12  pub  private / Clone 

**** — 6 globals/environment/tool_registry/v2_arena/current_ai_config/config_stack + Interpreter  facade holder

### Task 7.1:  CoreRuntime

- [ ] **Step 1:  `src/runtime/core.rs`**

```rust
//! v0.52 ADR-001: CoreRuntime — 

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use crate::ast_v2::AstArena;
use crate::interpreter::{AiConfigValue, Environment, ToolDef};

#[derive(Debug, Clone)]
pub struct CoreRuntime {
    pub globals: Arc<Mutex<Environment>>,
    pub environment: Arc<Mutex<Environment>>,
    pub tool_registry: Arc<HashMap<String, ToolDef>>,
    pub v2_arena: Option<Arc<AstArena>>,
    pub current_ai_config: Option<AiConfigValue>,
    pub config_stack: Vec<Option<AiConfigValue>>,
}

impl Default for CoreRuntime {
    fn default() -> Self {
        let env = Arc::new(Mutex::new(Environment::default()));
        Self {
            globals: env.clone(),
            environment: env,
            tool_registry: Arc::new(HashMap::new()),
            v2_arena: None,
            current_ai_config: None,
            config_stack: Vec::new(),
        }
    }
}
```

### Task 7.2:  Interpreter 

- [ ] **Step 1:  6  `core: CoreRuntime`**

```rust
pub struct Interpreter {
    pub(crate) core: CoreRuntime,
    pub(crate) ai: AiRuntime,
    pub(crate) orch: OrchRuntime,
    pub(crate) persist: PersistRuntime,
    pub(crate) sandbox: SandboxRuntime,
    pub(crate) registry: RegistryRuntime,
    pub(crate) infra: InfraRuntime,
}
```

- [ ] **Step 2:  Clone**

```rust
impl Clone for Interpreter {
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
            ai: self.ai.clone(),
            orch: self.orch.clone(),
            persist: self.persist.clone(),
            sandbox: self.sandbox.clone(),
            registry: self.registry.clone(),
            infra: self.infra.clone(),
        }
    }
}
```

** ≤ 10 **vs  43 

### Task 7.3:  12  pub 

- [ ] **Step 1: grep **

```bash
cd "D:/Github/mora-lang"
grep -rn "interp\.recorder\|interp\.trace\|interp\.trait_registry\|interp\.impl_table\|interp\.audit_sink\|interp\.markdown_memory_dir\|interp\.container\|interp\.tool_planes\|interp\.skill_registry\|interp\.plans\|interp\.refine_registry\|interp\.checkpoint_saver" src/ --include="*.rs" 2>&1 | head -40
```

- [ ] **Step 2: **

|  |  |
|----|----|
| `interp.recorder` | `interp.infra.recorder` |
| `interp.trace` | `interp.ai.trace` |
| `interp.trait_registry` | `interp.registry.trait_registry` |
| `interp.impl_table` | `interp.registry.impl_table` |
| `interp.audit_sink` | `interp.persist.audit_sink` |
| `interp.markdown_memory_dir` | `interp.persist.markdown_memory_dir` |
| `interp.container` | `interp.sandbox.container` |
| `interp.tool_planes` | `interp.sandbox.tool_planes` |
| `interp.skill_registry` | `interp.orch.skill_registry` |
| `interp.plans` | `interp.orch.plans` |
| `interp.refine_registry` | `interp.orch.refine_registry` |
| `interp.checkpoint_saver` | `interp.persist.checkpoint_saver` |

- [ ] **Step 3:  4  + commit**

 Task 1 

### Task 7.4:  CoreRuntime 5-10 

- [ ] **Step 1: **

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_default_globals_and_env_share() {
        let core = CoreRuntime::default();
        // globals  environment  Arc
        assert!(Arc::ptr_eq(&core.globals, &core.environment));
    }

    #[test]
    fn core_tool_registry_empty() {
        let core = CoreRuntime::default();
        assert!(core.tool_registry.is_empty());
    }

    #[test]
    fn core_v2_arena_default_none() {
        let core = CoreRuntime::default();
        assert!(core.v2_arena.is_none());
    }

    // ... 
}
```

---

## Task 8:  `builtins.rs`  + MethodDispatch trait

**Files:**  `src/interpreter/builtins.rs` /  `src/interpreter/builtins/{mod,file,sandbox,ai,memory,schedule,json,document,toolplane,mora}.rs` /  `src/interpreter/dispatch.rs`  `MethodDispatch` trait

### Task 8.1:  `builtins.rs` 

- [ ] **Step 1: grep `call_xxx_method`  builtin dispatch **

```bash
cd "D:/Github/mora-lang"
grep -n "pub fn call_.*_method" src/interpreter/builtins.rs | head -50
```

> ****`call_file_method` / `call_sandbox_method` / `call_ai_method` / `call_memory_method` / `call_schedule_method` / `call_json_method` / `call_document_method` / `call_toolplane_method` / `call_mora_method`  20-30  dispatch 

### Task 8.2:  `MethodDispatch` trait

- [ ] **Step 1:  `src/interpreter/dispatch.rs`  trait**

```rust
//! v0.52 ADR-001: MethodDispatch trait —  builtin dispatch 

use crate::interpreter::Interpreter;
use crate::value::Value;

pub trait MethodDispatch {
    fn method_name(&self) -> &'static str;
    fn call(&self, interp: &mut Interpreter, args: Vec<Value>) -> Result<Value, String>;
}
```

> **** `call_xxx_method`  `MethodDispatch`  struct  method dispatch `src/interpreter/mod.rs`  dispatch  trait impls

### Task 8.3: 

- [ ] **Step 1:  builtin  8-10 **

 `call_xxx_method` 200-500 `builtins.rs` 

 `src/interpreter/builtins/file.rs`:
```rust
//! v0.52 ADR-001: file.* builtin dispatch

use super::MethodDispatch;
use crate::interpreter::Interpreter;
use crate::value::Value;

pub struct FileDispatch;

impl MethodDispatch for FileDispatch {
    fn method_name(&self) -> &'static str { "file" }
    fn call(&self, interp: &mut Interpreter, args: Vec<Value>) -> Result<Value, String> {
        // ...  call_file_method 
    }
}
```

- [ ] **Step 2:  4  + commit**

> **** commit 5000+ LOC  8  cargo build 

### Task 8.4:  builtin 

- [ ] **Step 1:  10-20  builtin **

```rust
// src/interpreter/builtins/file.rs 
#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::Interpreter;

    #[test]
    fn file_read_nonexistent_returns_err() {
        let mut interp = Interpreter::new();
        let result = FileDispatch.call(&mut interp, vec![Value::String("/no/such/path".into())]);
        assert!(result.is_err());
    }

    // ...  builtin  2-3 
}
```

---

## 

- [ ] Task 1InfraRuntime  + 5 unit + 4  + commit
- [ ] Task 2AiRuntime  + 5 unit + 4  + commit
- [ ] Task 3OrchRuntime  + 5 unit + 4  + commit
- [ ] Task 4PersistRuntime  + 3 unit + 4  + commit
- [ ] Task 5SandboxRuntime  + 5 unit + 4  + commit
- [ ] Task 6RegistryRuntime  + 5 unit + 4  + commit
- [ ] Task 7CoreRuntime  + Interpreter ≤ 7  / Clone ≤ 10  / 12 pub  private+ 5-10 unit + commit
- [ ] Task 8builtins.rs  ≤ 400 LOC  + MethodDispatch trait + 10-20 unit + commit
- [ ] 650+ passed / 0 failed
- [ ] ADR-001 Proposed → Accepted
- [ ] CHANGELOG.md v0.52 

---

## 

 commit 
```bash
git reset --hard HEAD~1   #  1  facade
git reset --hard HEAD~n   #  n  facade
```

 facade  revert  commit  facade 

---

**Plan ** 
1. **Subagent-Driven ()** —  task  1  subagenttask  review
2. **Inline Execution** —  session  batch 


