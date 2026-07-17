# Interpreter Facade 拆分（ADR-001）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 完成 ADR-001 完整实施，将 `Interpreter` god object（33 字段 / 43 行 Clone）拆为薄核心 + 6 个 Domain Facade，推动 **Plateau A 结构债清偿** 退出门。

**Architecture:** 按 ADR-001 §1 BC 边界拆为 6 facade（`InfraRuntime` / `AiRuntime` / `OrchRuntime` / `PersistRuntime` / `SandboxRuntime` / `RegistryRuntime`） + 1 薄核心 `CoreRuntime`。`Interpreter` 只保留 7 个 facade 字段（`pub(crate)`），所有业务字段变 private。跨 facade 协作通过显式依赖注入（`&mut AiRuntime` 等），返回 owned 数据避免 borrow 摩擦。8 个独立 commit，每 commit 抽 1 facade（最后 1 commit 拆 `builtins.rs`），每 commit 含 3-5 个 facade 单元测试。

**Tech Stack:** Rust 2024 edition / 现有 `Interpreter` / `Arc<Mutex<>>` 模式 / 现有 613 tests 基础 / `#[cfg(test)]` 单元测试 / `cargo test --all` + `cargo clippy --all-targets --all-features -- -D warnings`

**Spec:** [`../specs/2026-07-09-interpreter-facade.md`](../specs/2026-07-09-interpreter-facade.md)

---

## 文件结构（前置：commit 1 后落地）

新增/修改：

```
src/
├── runtime/                          ← 新建（commit 1 起逐步填充）
│   ├── mod.rs                        ← 新建（commit 1）
│   ├── core.rs                       ← 新建（commit 7）
│   ├── ai.rs                         ← 新建（commit 2）
│   ├── orch.rs                       ← 新建（commit 3）
│   ├── persist.rs                    ← 新建（commit 4）
│   ├── sandbox.rs                    ← 新建（commit 5）
│   ├── registry.rs                   ← 新建（commit 6）
│   └── infra.rs                      ← 新建（commit 1）
├── interpreter/
│   ├── mod.rs                        ← 修改（8 commits 累积）
│   ├── builtins/                     ← 新建（commit 8）
│   │   ├── mod.rs
│   │   ├── file.rs
│   │   ├── sandbox.rs
│   │   ├── ai.rs
│   │   ├── memory.rs
│   │   ├── schedule.rs
│   │   ├── json.rs
│   │   ├── document.rs
│   │   ├── toolplane.rs
│   │   └── mora.rs
│   ├── builtins.rs                   ← commit 8 删除
│   └── dispatch.rs                   ← commit 8 拆 MethodDispatch trait
```

---

## Task 1: 抽 `InfraRuntime` facade

**Files:**
- Create: `src/runtime/mod.rs`
- Create: `src/runtime/infra.rs`
- Modify: `src/interpreter/mod.rs:203-326`（移除 InfraRuntime 字段 + 调整 new/Clone）
- Modify: 所有访问 `interp.recorder` / `interp.string_interner` / `interp.ai_cache` / `interp.bus` / `interp.scheduler` 的点（约 30+ 处）

**注意**：本计划给出**关键 sub-task** 与代码骨架（避免数千行 plan）。执行时按需展开。

### Task 1.1: 创建 `src/runtime/mod.rs`

- [ ] **Step 1: 写空 mod 声明**

```rust
//! v0.52 ADR-001: 6 Domain Facade 容器模块
//!
//! 每个 facade 是一个 BC 的状态 + 行为封装：
//! - AiRuntime       (BC3)
//! - OrchRuntime     (BC4)
//! - PersistRuntime  (BC5)
//! - SandboxRuntime  (BC7)
//! - RegistryRuntime (BC8)
//! - InfraRuntime    (BC9)
//!
//! 跨 facade 协作通过显式依赖注入（参数传 &mut facade），避免 borrow 摩擦。

pub mod ai;
pub mod core;
pub mod infra;
pub mod orch;
pub mod persist;
pub mod registry;
pub mod sandbox;
```

- [ ] **Step 2: 编译验证**

Run: `cd "D:/Github/mora-lang" && cargo build --all-targets 2>&1 | tail -5`
Expected: 失败（ai/core/orch/persist/registry/sandbox 还没建）— 这是预期的，下一步建。

### Task 1.2: 创建 `src/runtime/infra.rs`（最小可用版本）

- [ ] **Step 1: 写 InfraRuntime struct + Default + 单元测试骨架**

```rust
//! v0.52 ADR-001: InfraRuntime — BC9 (scheduling + 字符串驻留 + recorder)

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
    /// 构造带指定 thread_id 的 recorder
    pub fn with_recorder_thread(mut self, thread_id: String) -> Self {
        self.recorder = Recorder::new_off().with_thread(thread_id);
        self
    }

    /// 字符串驻留（去重）
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

> **注意**：具体字段类型（`RecordState` / `LruCache::new` / `Recorder::new_off()`）需要从 `src/interpreter/mod.rs:139-179` 和 `src/record/mod.rs` 实际定义确认。执行时按需调整。

- [ ] **Step 2: 在 mod.rs 加 `pub mod infra;`**

（已在 1.1 Step 1 中加入）

- [ ] **Step 3: 编译验证 infra.rs 单独**

Run: `cd "D:/Github/mora-lang" && cargo build -p mora --lib 2>&1 | tail -20`
Expected: 可能因其他 facade 缺而失败 — 临时方案：建空 stub（见 1.3）。

### Task 1.3: 建 5 个 facade 空 stub（让编译过）

- [ ] **Step 1: 在 `src/runtime/` 建 ai.rs / orch.rs / persist.rs / sandbox.rs / registry.rs / core.rs 空文件**

每个文件最小内容：

```rust
//! Stub — 待对应 commit 填充
```

- [ ] **Step 2: 编译**

Run: `cd "D:/Github/mora-lang" && cargo build --all-targets 2>&1 | tail -5`
Expected: PASS（可能有些 unused warning）

### Task 1.4: 把 Interpreter 的 InfraRuntime 字段迁出

- [ ] **Step 1: 修改 `Interpreter` struct**

在 `src/interpreter/mod.rs:203-275` 移除：
- `recorder: crate::record::Recorder`
- `string_interner: std::sync::Arc<Mutex<LruCache<Value>>>`
- `ai_cache: std::sync::Arc<Mutex<LruCache<String>>>`
- `bus: crate::event::EventBus`
- `scheduler: crate::schedule::Scheduler`

替换为：
```rust
pub(crate) infra: crate::runtime::infra::InfraRuntime,
```

- [ ] **Step 2: 修改 `Interpreter::new()`**

把字段初始化改成：
```rust
infra: InfraRuntime::default(),
```

- [ ] **Step 3: 修改 `Clone for Interpreter`**

`recorder: crate::record::Recorder::new_off()` → `infra: InfraRuntime::default()`（注：Clone 改成全用 default，因为这些字段共享 Arc）
> **更优**：保留 `infra` 的 Clone（内部 Arc clone 仍是 O(1)）

实际用：
```rust
infra: self.infra.clone(),
```

- [ ] **Step 4: 改所有访问点（grep 后批量）**

Run: `cd "D:/Github/mora-lang" && grep -rn "self.recorder\|self.string_interner\|self.ai_cache\|self.bus\|self.scheduler" src/ --include="*.rs" 2>&1 | head -30`

对每个匹配点改为 `self.infra.recorder` / `self.infra.string_interner` / `self.infra.ai_cache` / `self.infra.bus` / `self.infra.scheduler`。

> **若某个 facade 方法需要其他 facade**：通过 `&Interpreter` 拿（按 spec §2.3 模式）。

- [ ] **Step 5: 跑 4 门禁**

```bash
cd "D:/Github/mora-lang"
cargo build --all-targets 2>&1 | tail -3
cargo test --all 2>&1 | grep "test result:" | head -3
cargo fmt --check 2>&1 | head -3
cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -3
```

Expected: 全部 PASS；测试数 ≥ 618（613 现有 + 5 新增 InfraRuntime unit）

- [ ] **Step 6: Commit**

```bash
cd "D:/Github/mora-lang"
git add src/runtime/ src/interpreter/mod.rs
git commit -m "refactor(ADR-001): 抽 InfraRuntime facade

- 新增 src/runtime/{mod,infra}.rs
- Interpreter 移除 recorder/string_interner/ai_cache/bus/scheduler 5 字段
- 替换为 pub(crate) infra: InfraRuntime
- 30+ 访问点改为 self.infra.xxx
- 5 个 InfraRuntime 单元测试

验证：
  cargo build --all-targets  PASS
  cargo test --all          618+ passed / 0 failed / 14 ignored
  cargo fmt --check         PASS
  cargo clippy -D warnings  PASS"
```

---

## Task 2-6: 抽 AiRuntime / OrchRuntime / PersistRuntime / SandboxRuntime / RegistryRuntime

**每个 facade 1 个独立 commit**，模式与 Task 1 完全相同（建 facade 文件 → 改 Interpreter 字段 → 改访问点 → 跑 4 门禁 → commit）。

### Task 2: 抽 AiRuntime

**Files:** `src/runtime/ai.rs` 新建 / `src/interpreter/mod.rs` 改 9 字段 / ~50 访问点

字段（按 spec §2.2）：
- `model_routes: HashMap<String, RouteConfig>`
- `token_budget: Option<TokenBudget>`
- `token_usage: TokenUsage`
- `trace: TraceCollector`
- `context_window: ContextWindow`
- `speculative_verifier: SpeculativeVerifier`
- `cache_warmer: CacheWarmer`（**`#[allow(dead_code)]` 保留**）
- `draft_model_stats: Arc<Mutex<HashMap<String, (usize, usize)>>>`

> **重要**：`trace: TraceCollector` 是 `pub` 字段 — 抽到 facade 后变 `pub(crate)`。外部访问点改为 `interp.ai.trace`。

AiRuntime 方法示例：
- `record_token(input, output)`
- `get_cached(prompt_hash) -> Option<String>`
- `cache_response(prompt_hash, response)`

测试（5 个）：
- `default_ai_routes_empty`
- `token_budget_set_and_get`
- `token_usage_increments`
- `trace_records_event`
- `ai_cache_put_and_get`

### Task 3: 抽 OrchRuntime

**Files:** `src/runtime/orch.rs` 新建 / Interpreter 改 3 字段 / ~20 访问点

字段：
- `plans: Arc<Mutex<HashMap<String, Plan>>>`
- `refine_registry: Arc<Mutex<RefineRegistry>>`
- `skill_registry: Arc<Mutex<SkillRegistry>>`

OrchRuntime 方法示例：
- `plan_create(name, steps)`
- `plan_update(name, updates)`
- `skill_load(path)` / `skill_install(path)`

测试（5 个）：
- `plans_default_empty`
- `refine_registry_default`
- `skill_registry_default`
- `plan_create_returns_name`
- `refine_registry_registers_session`

### Task 4: 抽 PersistRuntime

**Files:** `src/runtime/persist.rs` 新建 / Interpreter 改 3 字段 / ~10 访问点

字段：
- `audit_sink: Arc<dyn AuditSink>`
- `markdown_memory_dir: Option<PathBuf>`
- `checkpoint_saver: Option<Arc<dyn CheckpointSaver>>`

PersistRuntime 方法示例：
- `save_checkpoint(thread_id, checkpoint)`
- `load_checkpoint(thread_id) -> Option<Checkpoint>`
- `audit_event(event)`

测试（3 个）：
- `default_persist_null_sink`
- `markdown_memory_dir_default_none`
- `checkpoint_saver_default_none`

### Task 5: 抽 SandboxRuntime

**Files:** `src/runtime/sandbox.rs` 新建 / Interpreter 改 4 字段 / ~20 访问点

字段：
- `sandbox: SandboxPolicy`
- `container: Arc<Mutex<Option<ContainerHandle>>>`
- `tool_planes: Arc<Mutex<ToolPlaneRegistry>>`
- 注：capability 字段在 `src/sandbox/capability.rs` 是 module-level state，需要查实际归属

> **风险**：`ContainerHandle` 含 Drop impl（v0.49 C3 fix），拆出时不能破坏 Drop 语义。

测试（5 个）：
- `default_sandbox_policy`
- `container_default_none`
- `tool_planes_default_has_core`
- `sandbox_validate_path` (safe path)
- `sandbox_validate_path_rejected` (escape attempt)

### Task 6: 抽 RegistryRuntime

**Files:** `src/runtime/registry.rs` 新建 / Interpreter 改 5 字段 / ~30 访问点

字段：
- `trait_registry: Arc<HashMap<String, TraitInfo>>`
- `impl_table: Arc<HashMap<String, Vec<String>>>`
- `mock_registry: MockRegistry`
- `ccr_store: InMemoryCcrStore`
- `memory_store: HashMap<String, Value>`

测试（5 个）：
- `default_registry_empty_traits`
- `mock_registry_default`
- `ccr_store_default_empty`
- `memory_store_default_empty`
- `registry_trait_lookup` (after insert)

---

## Task 7: 抽 CoreRuntime + Interpreter 薄化

**Files:** `src/runtime/core.rs` 新建 / `src/interpreter/mod.rs` 大改 / 12 个 pub 字段全 private / Clone 改写

**这是最重的一步** — 6 字段（globals/environment/tool_registry/v2_arena/current_ai_config/config_stack） + Interpreter 改成纯 facade holder。

### Task 7.1: 建 CoreRuntime

- [ ] **Step 1: 写 `src/runtime/core.rs`**

```rust
//! v0.52 ADR-001: CoreRuntime — 语言执行必需的薄核心

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

### Task 7.2: 改 Interpreter 字段

- [ ] **Step 1: 替换 6 字段为 `core: CoreRuntime`**

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

- [ ] **Step 2: 简化 Clone**

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

**应 ≤ 10 行**（vs 当前 43 行）。

### Task 7.3: 改所有 12 个原 pub 字段访问

- [ ] **Step 1: grep 找所有访问点**

```bash
cd "D:/Github/mora-lang"
grep -rn "interp\.recorder\|interp\.trace\|interp\.trait_registry\|interp\.impl_table\|interp\.audit_sink\|interp\.markdown_memory_dir\|interp\.container\|interp\.tool_planes\|interp\.skill_registry\|interp\.plans\|interp\.refine_registry\|interp\.checkpoint_saver" src/ --include="*.rs" 2>&1 | head -40
```

- [ ] **Step 2: 批量改**

| 旧 | 新 |
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

- [ ] **Step 3: 跑 4 门禁 + commit**

（与 Task 1 相同模式）

### Task 7.4: 加 CoreRuntime 单元测试（5-10 个）

- [ ] **Step 1: 写测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_default_globals_and_env_share() {
        let core = CoreRuntime::default();
        // globals 和 environment 初始指向同一个 Arc
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

    // ... 更多
}
```

---

## Task 8: 拆 `builtins.rs` 多文件 + MethodDispatch trait

**Files:** 删除 `src/interpreter/builtins.rs` / 新建 `src/interpreter/builtins/{mod,file,sandbox,ai,memory,schedule,json,document,toolplane,mora}.rs` / 改 `src/interpreter/dispatch.rs` 引入 `MethodDispatch` trait

### Task 8.1: 分析当前 `builtins.rs` 的方法分类

- [ ] **Step 1: grep `call_xxx_method` 找所有 builtin dispatch 函数**

```bash
cd "D:/Github/mora-lang"
grep -n "pub fn call_.*_method" src/interpreter/builtins.rs | head -50
```

> **预期发现**：`call_file_method` / `call_sandbox_method` / `call_ai_method` / `call_memory_method` / `call_schedule_method` / `call_json_method` / `call_document_method` / `call_toolplane_method` / `call_mora_method` 等 20-30 个 dispatch 函数。

### Task 8.2: 定义 `MethodDispatch` trait

- [ ] **Step 1: 在 `src/interpreter/dispatch.rs` 加 trait**

```rust
//! v0.52 ADR-001: MethodDispatch trait — 统一 builtin dispatch 入口

use crate::interpreter::Interpreter;
use crate::value::Value;

pub trait MethodDispatch {
    fn method_name(&self) -> &'static str;
    fn call(&self, interp: &mut Interpreter, args: Vec<Value>) -> Result<Value, String>;
}
```

> **实现细节**：每个 `call_xxx_method` 函数改成实现 `MethodDispatch` 的 struct 的 method。然后 dispatch 入口（`src/interpreter/mod.rs` 现有 dispatch 逻辑）改成遍历 trait impls。

### Task 8.3: 拆文件

- [ ] **Step 1: 按 builtin 类型拆 8-10 个子文件**

每个 `call_xxx_method` 函数（200-500 行）迁到对应子文件。`builtins.rs` 删除。

例如 `src/interpreter/builtins/file.rs`:
```rust
//! v0.52 ADR-001: file.* builtin dispatch

use super::MethodDispatch;
use crate::interpreter::Interpreter;
use crate::value::Value;

pub struct FileDispatch;

impl MethodDispatch for FileDispatch {
    fn method_name(&self) -> &'static str { "file" }
    fn call(&self, interp: &mut Interpreter, args: Vec<Value>) -> Result<Value, String> {
        // ... 现有 call_file_method 逻辑
    }
}
```

- [ ] **Step 2: 跑 4 门禁 + commit**

> **预期问题**：本 commit 改动面大（5000+ LOC 拆 8 文件），需要多次 cargo build 调整。

### Task 8.4: 为每个 builtin 子文件加单元测试

- [ ] **Step 1: 写 10-20 个 builtin 单元测试**

```rust
// src/interpreter/builtins/file.rs 末尾
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

    // ... 更多（每个 builtin 文件 2-3 个测试）
}
```

---

## 验收检查表

- [ ] Task 1：InfraRuntime 抽出 + 5 unit + 4 门禁全绿 + commit
- [ ] Task 2：AiRuntime 抽出 + 5 unit + 4 门禁全绿 + commit
- [ ] Task 3：OrchRuntime 抽出 + 5 unit + 4 门禁全绿 + commit
- [ ] Task 4：PersistRuntime 抽出 + 3 unit + 4 门禁全绿 + commit
- [ ] Task 5：SandboxRuntime 抽出 + 5 unit + 4 门禁全绿 + commit
- [ ] Task 6：RegistryRuntime 抽出 + 5 unit + 4 门禁全绿 + commit
- [ ] Task 7：CoreRuntime 抽出 + Interpreter 薄化（≤ 7 字段 / Clone ≤ 10 行 / 12 pub 全 private）+ 5-10 unit + commit
- [ ] Task 8：builtins.rs 拆为 ≤ 400 LOC 子文件 + MethodDispatch trait + 10-20 unit + commit
- [ ] 总测试：650+ passed / 0 failed
- [ ] ADR-001 状态更新：Proposed → Accepted
- [ ] CHANGELOG.md v0.52 章节记录破坏性变更

---

## 回退策略

每个 commit 独立可回退：
```bash
git reset --hard HEAD~1   # 撤回最近 1 个 facade
git reset --hard HEAD~n   # 撤回最近 n 个 facade
```

如发现某 facade 拆错，整段 revert 该 commit 即可，不影响后续 facade 抽取。

---

**Plan 完成。** 两种执行方式：
1. **Subagent-Driven (推荐)** — 我每 task 派 1 个 subagent，task 之间 review
2. **Inline Execution** — 在当前 session 直接 batch 执行

请选择。
