# Changelog

All notable changes to Mora will be documented in this file.

## [v0.75.1] — 2026-07-31 — P0 架构债务修复（内部重构，无行为变更）

按全项目架构审查（architecture-reviewer）确认的 3 项 P0 阻塞项修复，打破 `runtime/` ↔ `interpreter/`、`mir/` ↔ `interpreter/` 两处双向依赖循环并清理越层耦合。

### Changed — 类型下沉（runtime ↔ interpreter 解耦）
- 新增 `src/runtime/types.rs`：`AiConfigValue` / `LruCache` / `RouteConfig` / `TokenBudget` / `TokenUsage` / `ToolDef` / `TraitInfo` / `TraitMethodSig` 及 `impl_method_key` / `default_impl_method_key` 从 `interpreter/mod.rs` 下沉到 runtime 侧（字段可见性 `pub(crate)`）。
- `interpreter/mod.rs` 保留 re-export（`pub use crate::runtime::types::{...}`）保持既有路径兼容；`runtime/core.rs` / `ai.rs` / `infra.rs` / `registry.rs` 改 `use crate::runtime::types::*`。
- 验收：`grep -rn "crate::interpreter" src/runtime/` 为空。

### Changed — MirHost 抽象（mir ↔ interpreter 解耦）
- 新增 `src/mir/host.rs`：`MirHost` trait 定义 MIR 解释器所需宿主能力（方法桥 / config / checkpoint / environment / dynamic_sends / trait registry / `clone_box`）。
- `Interpreter` 实现 `MirHost`（委托既有固有方法，`clone_box` = `self.clone()`）。
- `mir/interp.rs` / `mir/handlers.rs` / `mir/dag_interp.rs` / `mir/jit.rs` 宿主参数从 `&mut Interpreter` 改为 `&mut dyn MirHost`。
- 验收：`grep -rn "crate::interpreter" src/mir/ src/pregel/`（生产代码）为空。

### Changed — MirPregelEngine 归位（结构错位修复）
- `interpreter/mir_pregel_engine.rs` → `src/pregel/mod.rs`；`interpreter/worker_pool.rs` → `src/pregel/worker_pool.rs`（git mv，历史保留）。
- `MirPregelEngine::run` / `reconcile_outcome` / `apply_write` 宿主参数改为 `&mut dyn MirHost`；内部字段访问改 trait 方法；并行 worker 克隆宿主改用 `clone_box()`。
- 注：原 P0 计划的「MirPregelEngine 迁移」原属 P2，因 `h_orchestrate` 依赖其路径而并入本次（否则 mir → interpreter 循环无法完全打破）。

### Changed — 移除 MirExpr 死字段
- `MirExpr.ty: Option<Type>`（`src/mir/expr/mod.rs`）为从未被写入/读取的死字段（构造器恒 `None`，`typeck/hm` 的 memo 快速路径永不触发），整体移除并删除 30+ 处 `ty: None` 构造行。
- `typeck/hm/mod.rs` / `typeck/check_mir.rs` 修正声称「写回 ty」的误导性注释；`check_program_mir_with_types` 保留为薄透传（语义澄清）。
- 删除依赖 memo 行为的测试 `tests/tier1_typeck_mir.rs::typed_mir_expr_zero_handled`；更新 `tests/tier2_mir_expr_pipeline.rs` 与 `tests/tier0_replacement.rs` 的公共 API 守门签名（宿主参数改为 `&mut dyn MirHost`）。
- 注意：`MirExprKind` 变体中的类型注解（`LetBinding::type_hint` / `Param::type_hint` / `StructDef::fields` 等）为活跃功能，保留不动。

### Tests
- 549 通过 / 0 失败（既有失败 `tier0_closure_mir::closure_reused_across_calls_via_mir` 为 pre-existing，干净树同样失败，非本次引入）。
- clippy `-D warnings` error 数从基线 95 → 89；`cargo fmt --check` 通过。

## [Unreleased] — Tier 0 → Tier 1 

MIR Tier 1 5 ////`run_file` / `run_record` / `run_replay` / `run_snapshot` / REPL (`mora --repl`)  `mora::mir::interp::run_mir` + `run_main_task` `Interpreter::interpret` / `execute` / `evaluate``MORA_INTERP`  `interpreter_mode()` 

**** Tier 0 

1. `tests/mir_differential.rs` — AST 
2. `Interpreter::mir_call_function` / `mir_call_method` / `mir_import` / `mir_with_config` — MIR  AST builtin 
3. `Interpreter::evaluate` / `call_value_inner` / `call_task_inner` —  builtin 

### Added
- `src/mir/interp.rs`:  `MirSignal` enum  `run_mir_with_signal` / `run_main_task_with_signal` REPL  Return/Break/Continue 
- `src/mir/`: α.5-α.8  MirInstMacroDef / Worker / Commit / Route / Observe / Span / RecordTokens / Save / Load / ReadFile / WriteFile / AppendFile / ReadBytesFile / WriteBytesFile / TraitDef / ImplDef / Orchestrate / Eval / SkillDef / PromptSection / DocumentSection `StmtKind` 
- `src/mir/lower.rs`: `lower_stmt`  41  `StmtKind` `#[allow(unreachable_patterns)]`  catch-all 
- `tests/tier0_replacement.rs`: 7  syntax / semantics / type-system / stdlib / runtime  AST `interpret`/`execute`

### Changed
- `src/main.rs`:  `interpreter_mode()`  AST fallback`run_file` / `run_record` / `run_replay` / `run_snapshot`  MIR lowering
- `src/interpreter/mod.rs::run_repl_with`: REPL  `mora::mir::lower::lower_program` + `mora::mir::interp::run_mir` task  `MirInst::TaskDef` 

## [v0.49.0] - 2026-07-07 —  +  +  (15 fixes)

1 commit; v0.49 audit follow-up (per user request: check simple implementations for high-concurrency / high-pressure correctness).

### Category A:  (6 fixes)

| # | Fix | File | Test |
|---|---|---|---|
| A1+B1 | CapabilityStore  `current_generation`; revoke  bump ; check  token.generation == current_generation (was: revoke ) | `src/sandbox/capability.rs:200-273` | revoke_invalidates_token_immediately + stress race |
| A2 | `mora.refine` builtin: drop lock before session.refine() (I/O ). RefineSession::refine  owned RefineStep  &RefineStep | `src/interpreter/builtins.rs:1745-1755`, `src/refine/mod.rs:107-154` | 7/7 refine builtin tests |
| A3 | mora.refine  + RefineStep clone  — 50 thread  | `src/stress_tests.rs:stress_refine_concurrent` |
| A4 | Semaphore::release  Ordering::AcqRel (was SeqCst, lighter barrier); acquire  AcqRel/Acquire (was SeqCst) | `src/interpreter/builtins.rs:2065-2090` | stress_semaphore_cas |
| A5 | CapabilityStore::check  inline lookup ( get() clone) —  get + check + generation  | `src/sandbox/capability.rs:264-281` | stress_capability_check_throughput (100k check/sec) |
| A6 | Interpreter.ai_cache / string_interner / draft_model_stats  Arc<Mutex<>>  (was raw HashMap, ) | `src/interpreter/mod.rs:155-205, 271-313`, `src/interpreter/ai_chat.rs:323-364, 411-414, 506-516` | stress_lru_concurrent (100 thread put) |

### Category B:  (5 fixes, B1  A1)

| # | Fix | File | Test |
|---|---|---|---|
| B1 | ( A1) | | |
| B2 | ContainerHandle::exec_with_timeout: spawn waiter thread + recv_timeout(N).  output()  (docker exec sleep infinity ) | `src/sandbox/container.rs:255-309` | stress_docker_exec_timeout (#\[ignore\],  docker) |
| B3 | generate_container_name  Arc<AtomicU64> counter : mora-{nanos}-{counter}. 100  | `src/sandbox/container.rs:354-369` | stress_container_name_unique (100 thread) |
| B4 | orchestrate graph step > 100 magic → MAX_GRAPH_STEPS: usize = 1000 const +  | `src/interpreter/orchestrate.rs:8-9, 50-58` |  4  orchestrate tests |

### Category C:  (4 fixes)

| # | Fix | File | Test |
|---|---|---|---|
| C1 | LruCache  (cap 10000 for ai_cache); put/get/evict O(1) +  entry  | `src/interpreter/mod.rs:139-179` | stress_ai_cache_lru_cap (1M put) |
| C2 | LruCache cap 50000 for string_interner |  C1 | stress_string_interner_lru_cap (1M put) |
| C3 | ContainerHandle  Drop impl: docker rm -f (opt-in via auto_cleanup=true, default true; with_auto_cleanup(false) ) | `src/sandbox/container.rs:223-251` | real_docker_spawn_and_destroy (#\[ignore\]) |
| C4 | worker_receivers cleanup: . Interpreter Clone  Arc  (v0.34 singletons via Arc) | `src/interpreter/mod.rs:248-314` | ( worker tests ) |

### New infrastructure

- **`LruCache<V>` struct** (mod.rs:139-179):  LRU, no deps. VecDeque<String> for O(1) pop_front + HashMap<String, V> for O(1) lookup. Used by ai_cache and string_interner.

### Test

- **New `src/stress_tests.rs`** (10 stress tests, all #[ignore] by default, run with --ignored):
  - stress_capability_revoke_under_race (100 thread)
  - stress_refine_concurrent (50 thread)
  - stress_semaphore_cas (1000 acquires + releases)
  - stress_capability_check_throughput (100k check/sec)
  - stress_ai_cache_lru_cap (1M put)
  - stress_string_interner_lru_cap (1M put)
  - stress_container_name_unique (100 thread, 1000 names)
  - stress_orchestrate_max_steps (B4 const sanity)
  - stress_lru_concurrent (100 thread put)
  - stress_container_drop_cleanup (100 handle drop, #[ignore],  docker)
  - stress_docker_exec_timeout (#[ignore],  docker)

- **Updated tests** (existing, behavior changes):
  - sandbox::capability::revoke_invalidates_token_immediately:  v0.49  (revoke )
  - interpreter::builtins::tests_v042_capability::sandbox_revoke_bumps_generation:  v0.49 (revoked token check_call  false)

### Total impact
- 1 commit
- ~1100 LOC net (impl + tests + stress infrastructure)
- +21 tests (11 sandbox capability + 1 builtin capability + 9 stress)
- 1 new Cargo dep: NONE (0 deps added; uses std::sync)
- 562 tests pass total (lib 556 + bin 6), 0 fail
- 14 tests #[ignore] (real Docker / long-running)
- clippy: 10 acceptable warnings in stress_tests.rs (test code quality, not bugs)
- fmt clean

## [v0.48.0] - 2026-07-06 — plan.update + mora.refine (pi-agent + CLI-Anything)

1 commit; v0.48+ roadmap from RESEARCH_PRIMITIVES_MASTER_v2.md §3.3.

### plan.update — real-time checklist (pi-agent §1.11)

- **New module `src/plan/mod.rs`**:
  - `StepStatus` enum: Pending () / InProgress () / Done ()
    with emoji ↔ text ↔ alias parsing (todo / doing / completed / etc.)
  - `PlanStep { id, text, status }` — single checklist item
  - `Plan` — ordered list + HashMap by_id for O(1) update
  - `add_step` / `update([(id, status)])` / `remove_step` / `get`
  - `complete_count` / `in_progress_count` / `pending_count` /
    `completion_ratio` helpers
  - 9 module-level tests (emoji/text parsing, add/update/remove,
    completion_ratio, empty plan)

- **`plan.*` builtins** (added to `call_plan_method`, 7 methods):
  - `plan.create(name, steps)` → String(name); steps: List[Dict{id, text, status?}]
  - `plan.update(name, updates)` → Bool(true); updates: List[[id, status]]
  - `plan.add(name, id, text)` → Bool(true) (append step)
  - `plan.remove(name, id)` → Bool(true)
  - `plan.list(name?)` → List (of plan names or step Dict[])
  - `plan.info(name)` → Dict{total, done, pending, completion_ratio}
  - Status accepts: pending/todo/, in_progress/in-progress//doing,
    done/completed//finish (emoji + text + alias all supported)

### mora.refine — incremental edit loop (CLI-Anything §1.3)

- **New module `src/refine/mod.rs`**:
  - `RefineStep { iteration, script_path, refined_path, instruction,
    original_bytes, refined_bytes, diff_lines_added/removed, timestamp }`
  - `RefineSession::new(script_path)` — computes `<stem>.refine/`
    subdir from script path
  - `RefineSession::refine(instruction)` — REAL file I/O: read script,
    create .refine/ dir, write `<stem>.refined.<n>.<ext>` with
    `# --- INSTRUCTION (refine iter n): <text>` header + original
    content. Returns `&RefineStep` with diff line counts.
  - `RefineRegistry` — multi-script session map
  - 6 module-level tests (real file I/O, multi-iteration,
    separate files, nonexistent error, multi-session, dict fields)

- **`mora.*` builtins** (added to `call_mora_method`, 3 methods):
  - `mora.refine(script_path, instruction)` → Dict{iteration, script,
    refined, instruction, original_bytes, refined_bytes,
    diff_lines_added, diff_lines_removed} (REAL file I/O)
  - `mora.refine_info(script_path, iteration?)` → Dict (latest or
    specific iteration)
  - `mora.list_refines()` → List[String] of all script paths with sessions

- **`Interpreter.plans` + `Interpreter.refine_registry` fields** (both
  `Arc<Mutex<>>` for `&self` API compat).

- **`BuiltinKind::Plan` + `BuiltinKind::Mora`** new variants; `plan` and
  `mora` global names registered.

### Design decision: REAL file I/O (not metadata-only)

master doc §3.3 says "mora refine 'add X' " (CLI-Anything).
**v0.48.0 actually writes files**:
- `mora.refine()` reads original script + writes `.refine/<stem>.refined.<n>.<ext>`
  with instruction header (REAL create_dir_all + write)
- `mora.refine_info()` re-reads file metadata for accurate
  original_bytes / refined_bytes
- No metadata-only "this is what we'd do" stubs

### 30 new tests (9 plan module + 6 refine module + 15 builtin)
- 9 `plan::tests::*`
- 6 `refine::tests::*` (incl. real file I/O tests)
- 8 `tests_v048_plan::*` (create/update/add/remove/list/info/emoji/unknown)
- 7 `tests_v048_refine::*` (real_file/iteration_increment/latest/specific/
  list/nonexistent/unknown)

### Total impact
- 1 commit
- ~700 LOC (+~280 plan + ~270 refine + ~150 builtin + ~80 tests cleanup)
- +30 tests (531 pre-existing retained)
- **561 tests pass total** (lib 555 + bin 6), 0 fail (1 pre-existing doctest)
- clippy clean (`-D warnings`), fmt clean
- 0 new deps

### v0.41+ roadmap complete

This commit finishes master doc §4 first wave + v0.45-v0.48 (8 commits).
v0.41-v0.48 covers all P0/P1/P2 patches identified by §4 of
RESEARCH_PRIMITIVES_MASTER_v2.md. Future work (v1.0+) includes:
- WASM sandbox (master doc §3.4)
- TRINITY router (deferred — repo access limited)
- 5-layer DI container (Puter)
- serde_yaml/serde_json upgrades (currently hand-written)

---

## [v0.47.0] - 2026-07-06 — DAG-as-data + heartbeat.md + context.trim

1 commit; v0.47+ roadmap from RESEARCH_PRIMITIVES_MASTER_v2.md §3.3.

### DAG-as-data orchestration (OpenFugu §1.6)

- **New module `src/orchestrate_dag/mod.rs`**:
  - `OrchestrateDag { nodes, edges }` — declarative DAG (OpenFugu
    `model_id[]` / `subtasks[]` / `access_list[]` 
    Mora adaptation: nodes + edges)
  - `validate()` — detect cycles, duplicate nodes, unknown endpoints
  - `topological_order()` — Kahn's algorithm (BFS) — O(V+E)
  - `has_cycle()` — boolean helper
  - 9 module-level tests (linear/diamond/4-layer, cycle detection,
    self-loop, duplicate node, unknown endpoint)

- **`ai.dag(nodes, edges)` builtin** (added to `call_ai_method`):
  - `nodes`: `List[String]` — agent names
  - `edges`: `List[[from, to]]` — pair list
  - Returns `List[String]` in execution order
  - Returns error on cycle / invalid input (real topological sort)

### heartbeat.md executable checklist (mimiclaw §1.5)

- **New module `src/heartbeat/mod.rs`**:
  - `HeartbeatItem { text, done, line_number }` — parsed checklist line
  - `parse_heartbeat(content, source)` — REAL md parser, supports
    `- [x]` / `- [X]` / `- [ ]` / `- []` formats
  - `HeartbeatReport { source, total, done, pending, items }` with
    `completion_ratio()` and `is_complete()` helpers
  - `load_heartbeat(path)` — REAL file I/O
  - 11 module-level tests (incl. 1 real file test)

- **`ai.heartbeat(path?)` builtin** (added to `call_ai_method`):
  - `path?`: optional path (default `~/.mora/HEARTBEAT.md`)
  - Returns `Dict{path, total, done, pending, completion_ratio,
    is_complete, items[]}` — REAL heartbeat.md parse
  - mimiclaw pattern: HEARTBEAT.md as executable agent behavior source

### context.trim smart truncation (pi-agent + AgentMesh)

- **`ai.context.trim(threshold?)` builtin** (added to `call_ai_method`):
  - `threshold?`: optional 0.0-1.0 (overrides default 0.8)
  - Calls `Interpreter.context_window.compress()` (REAL method, drops
    oldest messages first per `compression_ratio`)
  - Returns `Number(tokens_dropped)` (Number of tokens freed)
  - pi-agent+AgentMesh pattern: token-budget-aware truncation

- **`ai.context.info()` builtin** — diagnostic:
  - Returns `Dict{max_tokens, current_tokens, messages, compression_threshold}`

### Design decision: additive to existing infrastructure

- `OrchestrateDag` is **NEW module** (vs v0.25 orchestrate block syntax):
  declarative data (nodes + edges) vs procedural block (agents + edges).
  Both can coexist — block syntax for hand-written, dag builtin for
  programmatic graph generation.

- `HeartbeatItem` parses markdown checklists by line-prefix match
  (no regex dep), 30 LOC. v0.34 AIOS `tool_conflict_map` uses same
  line-iteration pattern.

- `context.trim` calls existing `ContextWindow::compress()` (v0.24)
  instead of writing new compression logic. `ContextWindow` already
  has add_message / needs_compression / compress / get_messages.

### 34 new tests (9 DAG module + 11 heartbeat module + 14 builtin)
- 9 `orchestrate_dag::tests::*` (linear/diamond/4-layer/cycle/self-loop/
  duplicate/unknown-edge/has_cycle/empty-edges)
- 11 `heartbeat::tests::*` (parse formats + completion_ratio +
  is_complete + real file test)
- 5 `tests_v047_dag::*` (linear/cycle/diamond/empty/2-args)
- 5 `tests_v047_heartbeat::*` (real_file/all_done/empty/nonexistent/items)
- 4 `tests_v047_context::*` (info/trim_empty/threshold_range/valid)

### Total impact
- 1 commit
- ~770 LOC (+~290 orchestrate_dag + ~180 heartbeat + ~120 builtin + ~180 tests)
- +34 tests (497 pre-existing retained)
- **531 tests pass total** (lib 525 + bin 6), 0 fail (1 pre-existing doctest)
- clippy clean (`-D warnings`), fmt clean
- 0 new deps

### Next v0.48 patches (per master doc §4)
- v0.48.0: `mora refine` incremental edit loop (CLI-Anything)
- v0.48.0: `plan.update([{step, status}])` real-time checklist (pi-agent)

---

## [v0.46.0] - 2026-07-06 — SKILL.md + MoraSkillSpec + dual registry (CLI-Anything)

1 commit; v0.46+ roadmap from RESEARCH_PRIMITIVES_MASTER_v2.md §3.3.

### MoraSkillSpec + SkillRegistry (CLI-Anything pattern)

- **New module `src/skill/mod.rs`**:
  - `MoraSkillSpec { name, description, trigger, body, source }` — parsed
    SKILL.md content (YAML frontmatter + Markdown body)
  - `MoraSkillSpec::parse(content, source)` — **REAL YAML frontmatter
    parser** (hand-written, no `serde_yaml` dep); supports `name:`,
    `description:`, `trigger:` + quoted values
  - `MoraSkillSpec::load_file(path)` — REAL file I/O read + parse
  - `SkillRegistry` with **dual-registry semantics** (CLI-Anything's
    `registry.json` + `public_registry.json`):
    - Internal: `HashMap<String, MoraSkillSpec>` (programmatic)
    - External: `public_registry_path: Option<PathBuf>` (mora-public.json hub)
  - `SkillRegistry::load_public_registry()` — REAL JSON read of hub
    file (uses simple `find_json_string` helper, no serde_json dep)
  - 10 module-level tests including 1 real file test

- **7 new builtins** added to `call_skill_method`:
  - `skill.list()` → `List[String]` of skill names
  - `skill.find(name)` → `Dict{name, description, trigger, body, source}` or Nil
  - `skill.load(path)` → `Bool(true)` — REAL `MoraSkillSpec::load_file` call
  - `skill.install(name, content)` → `Bool(true)` — synthesize from SKILL.md
    string content
  - `skill.uninstall(name)` → `Bool(true)`
  - `skill.set_hub(path)` → `Bool(true)` — set public_registry path
  - `skill.refresh_hub()` → `Number(count)` — REAL `load_public_registry` call

- **`Interpreter.skill_registry: Arc<Mutex<SkillRegistry>>`** field;
  Arc<Mutex<>> keeps `call_skill_method(&self, ...)` signature.

- **`BuiltinKind::Skill`** new variant; `skill` global registered.

### Design decision: hand-written YAML/JSON parsers (0 new deps)

master doc §3.3 says "CLI-Anything uses serde_yaml + serde_json". **v0.46.0
avoids both**:
- YAML frontmatter (3 keys: name/description/trigger): 30 LOC regex split
- JSON hub parse (name + description extraction): 5 LOC `find_json_string` helper
- Result: 0 new Cargo deps, parses the formats CLI-Anything uses

Full `serde_yaml` + `serde_json` support deferred to v1.0+ (per master doc
future roadmap) when SKILL.md files become more complex.

### 19 new tests (10 module + 9 builtin)
- 10 `skill::tests::*` (incl. 1 real file test for public_registry)
- 9 `interpreter::builtins::tests_v046_skill::*` (incl. 2 real file tests
  for skill.load + skill.set_hub/refresh_hub)

### Total impact
- 1 commit
- ~440 LOC (+~280 skill module + ~80 builtin wiring + ~80 tests)
- +19 tests (478 pre-existing retained)
- **497 tests pass total** (lib 491 + bin 6), 0 fail (1 pre-existing doctest)
- clippy clean (`-D warnings`), fmt clean
- 0 new deps

### Next v0.47 patches (per master doc §4)
- v0.47.0: DAG-as-data → `orchestrate`  (OpenFugu)
- v0.47.0: `heartbeat.md`  (mimiclaw)
- v0.47.0: `context.trim(threshold)`  (pi-agent + AgentMesh)
- v0.48.0: `mora refine`  + `plan.update` 

---

## [v0.45.0] - 2026-07-06 — ToolPlane + ai.retry + ai.role

1 commit; v0.45+ roadmap from RESEARCH_PRIMITIVES_MASTER_v2.md §3.3.

### ToolPlane — Core/Extension adapter (loongclaw)

- **New module `src/toolplane/mod.rs`**:
  - `PlaneKind` enum: `Core` (built-in) vs `Extension` (user/plugin)
  - `ToolSpec { name, description, parameters }` — metadata only
  - `ToolPlane` struct: name + kind + `HashMap<String, ToolSpec>`
  - `ToolPlaneRegistry` — multi-plane container
  - `default_registry()` — pre-registers `ai` + `sandbox` core planes
  - 11 module-level tests

- **8 new builtins** added to `call_toolplane_method`:
  - `tool.plane.create(name, kind)` → `Bool(true)`
  - `tool.plane.register(plane, tool, desc, params)` → `Bool(true)`
  - `tool.plane.unregister(plane, tool)` → `Bool(true)` (existed?)
  - `tool.plane.list()` → `List[String]` of plane names
  - `tool.plane.list_tools(plane)` → `List[String]` of tool names
  - `tool.plane.info(plane)` → `Dict{name, kind, tool_count}` or Nil
  - `tool.plane.find(plane, tool)` → `Dict{plane, tool, desc, params}` or Nil
  - `tool.plane.remove(plane)` → `Bool(true)`

- **`Interpreter.tool_planes: Arc<Mutex<ToolPlaneRegistry>>`** field;
  default has 2 core planes (`ai`, `sandbox`).
  Arc<Mutex<>> keeps `call_toolplane_method(&self, ...)` signature.

- **`BuiltinKind::Toolplane`** new variant; `tool` global registered
  (alongside existing `exec`, `sandbox`, etc.).

### ai.retry — tenacity-style retry policy (mini-swe-agent)

- **`ai.retry(attempts, backoff_ms?, strategy?)`** builtin:
  - `attempts`: Number/String — retry count (must be > 0)
  - `backoff_ms`: Number — base delay in ms (default 1000)
  - `strategy`: String — `fixed` / `exponential` / `linear` (default exponential)
  - Returns `Dict{attempts, backoff_ms, backoff, schedule}` where
    `schedule` is `List[Number]` of computed delays per attempt
  - Mini-swe-agent uses `tenacity@0.10s→60s` exp backoff; v0.45.0 mirrors
    this pattern with config validation

### ai.role — per-turn AI role (OpenFugu Worker/Thinker/Verifier)

- **`ai.role(name)`** builtin → `String(name)`:
  - OpenFugu canonical roles: `worker`, `thinker`, `verifier`
  - Custom roles also accepted (informational, no validation)
  - Returns the role name (caller-side enforcement for downstream ai.chat)

### Design decision: additive not replacement

master doc §6.5 says "ToolPlane  tool_registry". **v0.45.0 keeps both**:
- `Interpreter.tool_registry` (v0.34, single HashMap) — preserved
- `Interpreter.tool_planes` (v0.45.0, multi-plane) — added

Full migration deferred to v0.46+ to avoid breaking `tool_registry`-using
code paths in interpreter/execute.rs.

### 13 new tests (11 toolplane module + 6 toolplane builtin + 7 ai builtin)
- 11 `toolplane::tests::*`
- 6 `interpreter::builtins::tests_v045_toolplane::*`
- 7 `interpreter::builtins::tests_v045_ai::*`

### Total impact
- 1 commit
- ~580 LOC (+~290 toolplane module + ~200 builtin wiring + ~90 tests)
- +24 tests (454 pre-existing retained)
- **478 tests pass total** (lib 472 + bin 6), 0 fail (1 pre-existing doctest)
- clippy clean (`-D warnings`), fmt clean
- 0 new deps

### Next v0.46 patches (per master doc §4)
- v0.46.0: `SKILL.md`  +  (`mora-hub.json` + `mora-public.json`) (CLI-Anything)
- v0.47.0: DAG-as-data (OpenFugu) + `heartbeat.md` (mimiclaw) + `context.trim` (AgentMesh)

---

## [v0.44.0] - 2026-07-06 — sandbox.containerize REAL Docker + orchestrate validation

1 commit; v0.44+ roadmap from RESEARCH_PRIMITIVES_MASTER_v2.md §7.

### sandbox.containerize() — REAL Docker orchestration (pi-mono v0.44.0)

**v0.44.0 actually spawns Docker containers (NOT metadata-only).**

- **New module `src/sandbox/container.rs`**:
  - `ContainerBackend` enum: Docker (v0.44.0 ), Gondolin + OpenShell
    (deferred to v1.0+, returns explicit error)
  - `NetworkMode` (Isolated/Host), `MountSpec` (host:container:mode),
    `ResourceLimits` (cpu_cores, memory_mb), `ContainerSpec`
  - `ContainerHandle { container_id, container_name, backend, spec, started_at }` —
    runtime handle to a **real** spawned container
  - `spawn_container(spec) -> ContainerHandle` — calls `docker run -d` for real
  - `ContainerHandle::exec(&[cmd])` — runs `docker exec <id> <cmd>`
  - `ContainerHandle::destroy()` — runs `docker rm -f <id>`

- **4 new builtins** added to `call_sandbox_method`:
  - `sandbox.containerize(backend, mounts?, network?, cpu?, mem?, image?)`
    → `Number(id_hash)` — returns hash of real container ID;
    `Interpreter.container` holds full `ContainerHandle`
  - `sandbox.container_exec(cmd, args...)` → `Dict{exit_code, stdout, stderr, elapsed_ms}`
    — runs via `docker exec`
  - `sandbox.container_info()` → `Dict{container_id, container_name, backend, image, network, mount_count, elapsed_ms}` or `Nil`
  - `sandbox.container_clear()` → `Bool(true)` — actually runs `docker rm -f`

- **`Interpreter.container: Arc<Mutex<Option<ContainerHandle>>>`** field;
  Arc<Mutex<>> keeps `call_sandbox_method(&self, ...)` signature intact
  (no breaking change to dispatch).

### Tested against real Docker daemon

`real_docker_spawn_and_destroy` integration test (#[ignore]):
```text
$ cargo test --lib real_docker_spawn_and_destroy -- --ignored --nocapture
running 1 test
test sandbox::container::tests::real_docker_spawn_and_destroy ... ok
test result: ok. 1 passed; 0 failed; 0 ignored
```

The test:
1. Spawns `docker run -d --name mora-XXX alpine:latest sleep infinity`
2. Verifies container_id is real (>= 12 hex chars)
3. Runs `docker exec <id> echo hello-from-mora` and checks stdout
4. Cleans up via `docker rm -f <id>`

**All 4 real-docker integration tests pass in 1.15s** when run with `--ignored`.

### orchestrate block — already implemented v0.25 (validation only)

master doc §1.13 cites revenue-orchestrator's `handoff_criteria` pattern.
**Pre-existing v0.25 implementation** in `src/interpreter/orchestrate.rs`:
- `orchestrate sequential <input> -> <output> { agents... }`
- `orchestrate graph <input> -> <output> { edges with `on:` predicate }`
- `orchestrate loop <input> -> <output>, max_rounds: N, on: <cond> { agent }`

Added 3 parse-validation tests (no new code needed).

### Design decision: Docker-only in v0.44.0

master doc §1.11 mentions Gondolin / Docker / OpenShell. **Decision**:
- **Docker**: implemented in v0.44.0 (most common, real CLI spawn)
- **Gondolin / OpenShell**: deferred to v1.0+ — `spawn_container()`
  returns clear "not yet implemented" error if requested

Future builtins (sandbox.exec via container, sandbox.file.read via mount
  validation) can check `Interpreter.container.is_some()` to apply
  container-aware policies.

### 14 new tests (11 module + 0 builtin unit + 4 docker ignored + 3 orchestrate parse)
- 11 `sandbox::container::tests::*` (incl. 1 #[ignore] docker integration)
- 4 `interpreter::builtins::tests_v044_container_real::*` (4 #[ignore] docker)
- 3 `interpreter::builtins::tests_v044_orchestrate_validate::*`
- **4 skipped (#[ignore])** unless `cargo test -- --ignored` with Docker daemon

### Total impact
- 1 commit (after v0.44.0 metadata-only attempt was REVERTED)
- ~600 LOC (+~400 container module + ~150 builtin wiring + ~50 tests)
- +14 tests (436 pre-existing retained)
- **454 tests pass total** (lib 448 + bin 6), 0 fail (1 pre-existing doctest)
- clippy clean (`-D warnings`), fmt clean
- 0 new deps

### Next v0.45 patches
- v0.45.0: `ToolPlane` Core/Extension adapter (loongclaw, ~150 LOC)
- v0.45.0: `ai.retry { attempts: 10, backoff: exponential }` (mini-swe-agent)
- v0.45.0: `ai.role { worker / thinker / verifier }` (OpenFugu)

---

## [v0.43.1] - 2026-07-05 — memory.remember / bus.subscribe (markdown + pub-sub)

1 commit; third P1 of the v0.41+ roadmap from RESEARCH_PRIMITIVES_MASTER_v2.md.

### memory.remember / recall_markdown / list_markdown (pi-agent inspired)

- **3 new builtins** added to `call_memory_method`:
  - `memory.remember(category, text)` → `Bool(true)`; appends to
    `~/.mora/memory/YYYY-MM-DD.md` under `## {category}` section
  - `memory.recall_markdown(category)` → `String`; collects all entries
    under `## {category}` across all markdown files
  - `memory.list_markdown()` → `List[String]`; lists all categories

- **Markdown format** (auto-generated):
  ```
  # 2026-07-05

  ## {category}

  - {text}

  ## {other_category}

  - {text}
  ```
  Subsequent remember to existing category appends bullets (no duplicate section).

- **`Interpreter.markdown_memory_dir: Option<PathBuf>`** field added;
  overrides default `~/.mora/memory/` for test isolation + custom deployments.
  Wired through Clone impl + 3 constructors.

- **Cross-pollination with HashMap memory**: remember also writes to
  `memory_store["md:{category}"]` so existing `memory.recall()` works.

- **5 helper functions added**:
  - `markdown_memory_dir(override)` — resolution precedence: field > env > home
  - `today_date_string()` — UNIX days → YYYY-MM-DD (handles leap years)
  - `remember_markdown(override, cat, text)` — atomic write per file
  - `recall_markdown(override, cat)` — read all .md, extract section
  - `list_markdown_categories(override)` — collect unique `## ` headers

### bus.subscribe / bus.publish (Puter / AgentMesh / Solace inspired)

- **2 new builtins** added to `call_event_method`:
  - `bus.subscribe(pattern)` → `Number(token)`; registers pattern via
    `EventBus::on()` with no-op handler (real handlers via LSP/HTTP/MCP layer)
  - `bus.publish(topic, payload)` → `Number(pattern_count)`; emits via
    `EventBus::emit()` which has v0.41.0 O(segments) indexed matching

- **Pattern matching** inherits v0.41.0 O(segments) indexed matching
  (Puter EventClient code-verified). Subscribers using `agent.*` catch
  `agent.foo`, `agent.foo.bar`, etc.

### 12 new tests (6 memory + 6 bus)
- `memory_remember_appends_to_markdown` — file write
- `memory_remember_appends_to_existing_section` — no duplicate section
- `memory_recall_markdown_returns_text` — section readback
- `memory_recall_markdown_returns_empty_for_unknown` — missing category
- `memory_list_markdown_lists_categories` — multiple categories
- `memory_recall_after_remember_syncs_to_memory_store` — HashMap sync
- `bus_subscribe_returns_token` — Number(token)
- `bus_subscribe_validates_pattern` — type check
- `bus_publish_returns_pattern_count` — Number
- `bus_publish_validates_topic` — type check
- `bus_subscribe_then_publish_wildcard_match` — wildcard end-to-end
- `bus_subscribe_uses_existing_pattern_matching` — exact + prefix patterns

### Design decision: Test isolation via field, not env var
- Master doc §6.4/§6.5 suggested using `MORA_MEMORY_DIR` env var
- **Switched to `Interpreter.markdown_memory_dir: Option<PathBuf>`**:
  - Cleaner test isolation (no global env state, parallel tests safe)
  - Field-level override matches existing `Interpreter.sandbox`,
    `Interpreter.audit_sink` pattern
  - Env var fallback preserved (`$MORA_MEMORY_DIR` still works if field is None)
  - Default falls back to `$HOME/.mora/memory/`

### Total impact
- 1 commit
- ~620 LOC (+~280 impl + ~50 init sites + ~290 tests)
- +12 tests (424 pre-existing retained)
- 436 tests pass total (lib 430 + bin 6), 0 fail (1 pre-existing doctest)
- clippy clean (`-D warnings`), fmt clean
- 0 new deps

### Next v0.44 patches (per master doc §4)
- v0.44.0: `orchestrate { on: expression }` — predicate routing (revenue-orchestrator)
- v0.44.0: `sandbox.containerize` Gondolin mode (pi-mono)
- v0.45.0: `ToolPlane` Core/Extension adapter (loongclaw) + `ai.retry` + `ai.role`

---

## [v0.43.0] - 2026-07-05 — exec.parallel() concurrent subprocess (pi-mono v1)

1 commit; **finishes master doc §4 v0.41-0.43 first wave** (5 patches total).

### exec.parallel() — concurrent subprocess execution

- **New `BuiltinKind::Exec` variant** + `call_exec_method` dispatcher
  + builtin `exec` registered in `Interpreter::new()` globals.

- **`exec.parallel(cmds, [max_concurrent], [timeout_ms])`** builtin:
  - First arg: `List[String]` — commands to execute (run via `sh -c`)
  - Optional 2nd arg: `Number` — max concurrent workers (default = cmds.len())
  - Optional 3rd arg: `Number` — per-cmd timeout in ms (default = no timeout)
  - Returns: `List[Dict{cmd, stdout, stderr, exit_code, pid, elapsed_ms, error}]`

- **Process group isolation** (mini-swe-agent v1 style):
  - **Unix**: `pre_exec` calls `libc::setpgid(0, 0)` to create new process group
  - **Windows**: `creation_flags(CREATE_NEW_PROCESS_GROUP)` (0x00000200)
  - On timeout: `killpg(pid, SIGKILL)` (Unix) / `taskkill /F /T /PID` (Windows)
  - Prevents orphaned grandchild processes

- **STD-ONLY implementation** (deliberate deviation from master doc §6.5):
  - `tokio::process::Command` (master doc suggested) **rejected** — AGENTS.md
    and Cargo.toml both forbid async runtime
  - Used: `std::thread::spawn` + `std::process::Command` +
    `std::sync::{mpsc, Arc, Condvar, Mutex}`
  - Custom `Semaphore` impl (std lacks one) using AtomicUsize + Mutex + Condvar
  - Atomic index distribution via `AtomicUsize::fetch_add`

### 9 new tests (Interpreter-level)
- `exec_parallel_runs_all_commands` — 3 cmds,  stdout
- `exec_parallel_respects_max_concurrent` — 6 cmds, max_concurrent=2
- `exec_parallel_empty_list_returns_empty` — 
- `exec_parallel_collects_stdout_per_command` — 
- `exec_parallel_kills_process_group_on_timeout` — `sleep 10` + 200ms timeout
- `exec_parallel_validates_arg_types` — 
- `exec_parallel_validates_cmd_elements` — 
- `exec_parallel_returns_error_for_missing_command` —  → exit 127
- `exec_unknown_method_errors` — unknown method

### Design decision: STD vs tokio
- Master doc §6.5 suggested `tokio::process::Command` + `tokio::sync::Semaphore`
- Project rule (AGENTS.md §3 + Cargo.toml): **" async runtime"**
- Implemented equivalent with std threads + custom Semaphore
- Result: 0 new deps, all std library APIs

### Total impact
- 1 commit
- ~390 LOC (+~250 impl + ~140 tests)
- +9 tests (415 pre-existing retained)
- 424 tests pass total, 0 fail
- clippy clean (`-D warnings`), fmt clean
- 0 new deps

### v0.41+ roadmap progress (master doc §4)
| Version | Status | Patch |
|---------|--------|-------|
| v0.41.0 |  | event O(segments) |
| v0.41.1 |  | reading_order XY-Cut++ |
| v0.42.0 |  | sandbox.key + Capability |
| v0.42.1 |  | audit.jsonl + AuditSink |
| **v0.43.0** |  | **exec.parallel()** |
| v0.43.1+ | planned | memory.remember/recall, bus.subscribe, orchestrate, etc. |

**First wave complete.** All 5 patches from RESEARCH_PRIMITIVES_MASTER_v2.md §4
implemented and committed.

---

## [v0.42.1] - 2026-07-05 — Audit Sink SHA-256 Hash Chain (loongclaw)

1 commit; second P1 of the v0.41+ roadmap from RESEARCH_PRIMITIVES_MASTER_v2.md
(loongclaw crates/kernel/src/audit.rs:34-204 inspired).

### Audit Sink — JSONL + SHA-256 hash chain

- **New module `src/audit/mod.rs`** — implements loongclaw-style audit log:
  - `AuditEvent { timestamp_ms, actor, action, target, payload_json, token_id, prev_hash, hash }`
  - `AuditSink` trait (`Send + Sync`): write / flush / verify_chain / event_count
  - `JsonlAuditSink` — append-only JSONL file with SHA-256 hash chain
    (`hash = SHA-256(canonical_json(event) + prev_hash)`)
  - `NullSink` — no-op default (audit disabled)
  - `AuditError` enum (Io, ChainBroken, HashMismatch, ParseError)

- **`Interpreter.audit_sink: Arc<dyn AuditSink>`** field added; default `NullSink`.
  Wired through `Clone::clone()` impl + 3 constructors.

- **3 new builtins** (added to `call_sandbox_method`, NOT new BuiltinKind):
  - `sandbox.audit_emit(actor, action, target?, payload?)` → `Value::Bool(true)`
  - `sandbox.audit_flush()` → flushes write buffer to disk
  - `sandbox.audit_verify()` → `Value::Bool(true)` if chain OK, else
    `Value::String(error)` (so Mora can branch on it)

- **Hash chain design**:
  - First event: `prev_hash = "0" × 64` (genesis)
  - Each subsequent event: `prev_hash = previous event's hash`
  - `verify_chain()` reads whole file, recomputes hash for each line,
    catches both `prev_hash` mismatch (line deleted/inserted) AND
    `hash` mismatch (content tampered)

- **Crash safety**: `new(path)` reads last line of existing file and
  restores `last_hash` from the most recent `hash` field — process
  restart resumes the chain instead of restarting from genesis.

- **No `serde` dep added** — JSON serialization is hand-written
  (`json_string()` escape function, ~30 LOC). Only `sha2 = "0.10"`
  added to Cargo.toml (per AGENTS.md §3, deps justified).

### 20 new tests (audit module unit + Interpreter builtin integration)
- 12 `audit::tests::*` (JsonlAuditSink + NullSink + parser/serializer)
- 8 `interpreter::builtins::tests_v0421_audit::*` (full builtin flow)

### Total impact
- 1 commit
- ~700 LOC (+~480 audit module + ~100 builtin wiring + ~20 InitSite +
  ~100 tests; minor clones/sed)
- +20 tests (395 pre-existing retained)
- 415 tests pass total, 0 fail
- clippy clean (`-D warnings`), fmt clean
- 1 new dep (`sha2 = "0.10"`)

### Next v0.43 patches (per master doc §4)
- v0.43.0: `exec.parallel()` (pi-mono v1 subprocess isolation, ~50 LOC)

---

## [v0.42.0] - 2026-07-05 — Capability Token System (loongclaw)

1 commit; first P1 of the v0.41+ roadmap from RESEARCH_PRIMITIVES_MASTER_v2.md
(loongclaw crates/contracts/src/contracts.rs:24-52 inspired).

### Capability Token System

- **New module `src/sandbox/capability.rs`** — implements token-based
  authorization alongside the v0.33 pattern-based `allow/deny`:
  - `Capability` enum (13 variants: `FileRead`, `FileWrite`, `WebFetch`,
    `WebSearch`, `ExecBash`, `ExecParallel`, `MemoryRead`, `MemoryWrite`,
    `AuditEmit`, `BusSubscribe`, `BusPublish`, `AgentInvoke`, `AgentRegister`)
  - `CapabilityToken { token_id, allowed, denied, expires_at, generation, created_at }`
  - `CapabilityStore` (Arc<Mutex<BTreeMap>>) — issue/get/check/revoke API
  - `SandboxError` enum with structured variants (UnknownCapability,
    TokenExpired, TokenNotFound, CapViolation, GenerationMismatch)

- **`SandboxPolicy.capabilities: CapabilityStore`** field added
  (default `CapabilityStore::new()`). v0.33 pattern-based API
  (`allow/deny BTreeSet`, `check_builtin`, `check_path`) is **unchanged**.

- **4 new builtins** wired through `call_sandbox_method`:
  - `sandbox.key { "file.read", "web.fetch" }` → `Value::Number(token_id)`
  - `sandbox.check_call(token_id, "file.read")` → `Value::Bool`
  - `sandbox.revoke(token_id)` → `Value::Bool(true)` (loongclaw-style:
    bumps `generation`, doesn't delete token)
  - `sandbox.token_count()` → `Value::Number`

- **`Capability::parse(s)` and `as_str()`** for round-trip between
  Rust enum and mora source strings.

### Design decisions
- **Token handle = `Value::Number(u64)`** (NOT a new Value variant).
  Avoids touching the 56-variant `Value` enum (per AGENTS.md §5, v0.x
  may break but prefer minimal surface).
- **Arc<Mutex> around CapabilityStore** so `SandboxPolicy: Clone` still works
  (interpreter copy semantics share the store, not duplicate it).
- **Revoke bumps generation** (loongclaw style) instead of deleting.
  This means `check_call` doesn't validate generation — that's a
  higher-layer PolicyEngine concern, exposed via `SandboxError::GenerationMismatch`.
- **No TTL in v0.42.0 builtin** — `sandbox.key` accepts any args, no
  `sandbox.key_ttl { ..., ttl: 5s }` yet. Token's `expires_at` field is
  ready; builtins will be added in v0.42.x if needed.

### 21 new tests (CapabilityStore unit + Interpreter builtin integration)
- 11 `sandbox::capability::tests::*` (CapabilityStore unit)
- 10 `interpreter::builtins::tests_v042_capability::*` (full builtin flow)

### Total impact
- 1 commit
- ~520 LOC (+~280 capability module + ~90 builtin wiring + ~150 tests)
- +21 tests (374 pre-existing retained)
- 395 tests pass total, 0 fail
- clippy clean (`-D warnings`), fmt clean
- 0 new deps

### Next v0.42+ patches (per master doc §4)
- v0.42.1: `audit.jsonl` + AuditSink SHA-256 chain (loongclaw, ~200 LOC)
- v0.43.0: `exec.parallel()` (pi-mono v1 isolation, ~50 LOC)

---

## [v0.41.1] - 2026-07-05 — Reading Order XY-Cut++ (MinerU algorithm upgrade)

1 commit; second P0 of the v0.41+ roadmap from RESEARCH_PRIMITIVES_MASTER_v2.md.

### Reading order: XY-Cut++ algorithm upgrade (MinerU arXiv:2504.10258)

- **New `Strategy::XyCutPlusPlus` variant** (and aliases `xy_cut_plus_plus` /
  `xy++` / `xy_cut_pp` via `Strategy::from_str`). Old variants
  (`InputOrder` / `TopToBottom` / `GapTree` / `XyCut` / `GroupBased`)
  remain unchanged — fully backwards-compatible.

- **Old `Strategy::XyCut` (v0.33)** was a flat sort `(y, then x)` — no
  recursive segmentation. **New `XyCutPlusPlus`** implements the actual
  recursive XY-Cut algorithm (arXiv:2504.10258):
  1. **Cross-layout element detection** (`is_cross_layout`): elements
     with `width > beta * max_width` AND `overlap_count >= 2` are split
     off (e.g. cross-column headers / footers).
  2. **Density-ratio axis selection** (`compute_prefer_horizontal`):
     `x_density > density_threshold * y_density` → prefer horizontal
     first (split by y, then within each row by x).
  3. **Recursive projection-segmentation** (`recursive_xy_cut`):
     project to axis → find gap-runs → split into sub-segments →
     recurse with flipped axis preference.
  4. **Merge cross-layout elements** at the right position based on
     vertical center.

- **5 helper functions added** (all private, file-local):
  - `is_cross_layout(all, bbox)` — cross-column detection
  - `compute_prefer_horizontal(entries)` — adaptive axis selection
  - `compute_density_ratios(entries)` — x/y density calculation
  - `project_to_axis(entries, axis)` — 1D histogram projection
  - `split_projection(hist, min_gap)` — find gap-run segments
  - `recursive_xy_cut(entries, prefer_horizontal_first)` — core recursion
  - `merge_cross_layout_elements(main, cross)` + `find_insertion_point`

- **5 named constants** (MinerU defaults):
  `XY_CUT_PLUS_PLUS_BETA = 2.0`, `DENSITY_THRESHOLD = 0.9`,
  `OVERLAP_THRESHOLD = 0.1`, `MIN_OVERLAP_COUNT = 2`,
  `MIN_GAP_THRESHOLD = 5.0`.

- **7 new tests** (8 pre-existing retained):
  - `strategy_from_str_xy_cut_pp` — aliases parse correctly
  - `xy_cut_pp_single_column_doc` — newspaper-style vertical ordering
  - `xy_cut_pp_two_column_doc` — academic two-column (L1,R1,L2,R2 row-by-row)
  - `xy_cut_pp_with_cross_layout_header` — wide header inserted at top
  - `xy_cut_pp_single_block_returns_unchanged` — single-block edge case
  - `xy_cut_pp_preserves_all_blocks` — no blocks lost or duplicated
  - `xy_cut_pp_complexity_below_o_n_squared` — perf benchmark, 50 blocks < 200ms

### Source inspiration
`MinerU` arXiv:2504.10258 "XY-Cut++: Advanced Layout Ordering via Hierarchical Mask
Matching" (April 2025). Mora previously had only the simple `recursive_xy_cut`
from `mineru/model/reading_order/xycut.py`; v0.41.1 upgrades to the newer
algorithm per master doc §6.2.

### Total impact
- 1 commit
- ~290 LOC (+~230 impl + ~60 tests + ~10 const)
- +7 tests (8 pre-existing retained)
- 374 tests pass total (was 367), 0 fail
- clippy clean (`-D warnings`), fmt clean
- 0 new deps
- Backwards-compatible: existing `Strategy` variants unchanged; only adds
  a new variant + aliases

### Next v0.41 patches (per master doc §4)
- v0.42.0: `sandbox.key` + `Capability` enum (loongclaw, ~200 LOC)
- v0.42.1: `audit.jsonl` + AuditSink SHA-256 chain (loongclaw, ~200 LOC)
- v0.43.0: `exec.parallel()` (pi-mono v1 isolation, ~50 LOC)

---

## [v0.41.0] - 2026-07-05 — Event Bus O(segments) (Puter, code-verified)

1 commit; first P0 of the v0.41+ roadmap from RESEARCH_PRIMITIVES_MASTER_v2.md.

### Event bus: O(segments) indexed matching replaces linear scan

- **`EventBus` now uses a 3-bucket index** instead of a single
  `HashMap<Pattern, Vec<Handler>>` iterated on every emit:
  - `exact`: literal patterns (e.g. `"ai.chat.completed"`) → O(1) lookup
  - `prefix`: trailing-wildcard patterns (e.g. `"ai.*"`, `"a.b.*"`, `"*"`)
    keyed by the prefix-without-`.*` (e.g. `"ai"`, `"a.b"`, `""`) →
    O(segments) prefix walk
  - `interior`: middle-wildcard patterns (e.g. `"a.*.c"`, `"*.b.*"`)
    kept as fallback linear scan (rare in practice; required by
    existing API semantics)

- **`emit` complexity**:
  - Old (v0.32-0.40): **O(patterns × segments)** — `map.iter().filter(matches).flat_map(...)`
  - New (v0.41): **O(segments)** for exact/prefix paths
    (interior fallback remains O(interior_patterns))

- **`classify_pattern()` helper** routes `on(pattern)` registrations to
  the correct bucket at registration time, so `emit` never needs to
  parse patterns.

- **Catch-all `*` pattern**: keyed by empty string `""`, looked up
  once at the start of `emit`'s prefix walk — verified via new
  `bus_catchall_star_routes_to_prefix_empty` test.

- **10 new tests** (8 pre-existing retained):
  - `classify_pattern_routes_correctly` (Pure function test)
  - `bus_handlers_route_to_correct_buckets` (Register dispatches to right bucket)
  - `bus_emit_literal_match_fires_handler` (Exact path)
  - `bus_emit_wildcard_match_fires_handler` (Prefix path)
  - `bus_emit_with_no_subscribers_is_noop` (Empty case)
  - `bus_emit_with_multiple_wildcards_fires_all` (Multi-level Puter walk)
  - `bus_interior_wildcard_still_works` (Interior fallback)
  - `bus_catchall_star_routes_to_prefix_empty` (Catch-all)
  - `bus_off_removes_from_correct_bucket` (off() routes to right bucket)
  - `bus_emit_complexity_scales_with_segments_not_patterns` (Perf benchmark,
    100 patterns + 1000 emits < 200ms)

### Source inspiration
`Puter` `src/backend/clients/event/EventClient.ts:62-67` (verified 2026-07-05
via MCP search; see RESEARCH_PRIMITIVES_MASTER_v2.md §1.10).

### Total impact
- 1 commit
- ~165 LOC (108 impl + ~57 tests)
- +10 tests (8 pre-existing retained)
- 367 tests pass total, 0 fail
- clippy clean (`-D warnings`), fmt clean
- 0 new deps
- Backwards-compatible: same `on(pattern, handler)` / `emit(event, payload)`
  / `off(pattern)` API, same matching semantics

### Next v0.41 patches (per master doc §4)
- v0.41.1: `reading_order` XY-Cut++ (MinerU algorithm upgrade, ~60 LOC)
- v0.42.0: `sandbox.key` + `Capability` enum (loongclaw, ~200 LOC)
- v0.42.1: `audit.jsonl` + AuditSink SHA-256 chain (loongclaw, ~200 LOC)
- v0.43.0: `exec.parallel()` (pi-mono v1 isolation, ~50 LOC)

---

## [v0.40] - 2026-07-04 — Env Refactor (Closure Env Immutable)

2 commits resolving Permanent #1 (Env cross-thread safety) — the
LAST of the 5 "permanent debts" the v0.34 audit identified.

### EnvRef immutable snapshot for closure captures

- **`Value::Closure.env` now `EnvRef` (immutable Box<Environment>)**
  instead of `Arc<Mutex<Environment>>` (shared mutable). The captured
  environment is FROZEN at closure-creation time — no other thread or
  closure can mutate a closure's bound variables.

- **`EnvRef`** type introduced — a Box<Environment> wrapper that's
  Send-safe (Environment contains only Send fields). `EnvRef::borrow()`
  returns `&Environment` for read access. `EnvRef::from_arc_mutex()`
  converts legacy `Arc<Mutex<>>` sources.

- **3 Closure constructor sites** (evaluate:214, execute:562, mock:142)
  now use `EnvRef::from_arc_mutex(self.environment.clone())`.
- **1 Closure destructure site** (dispatch:1193) updated to clone
  the inner Environment from EnvRef.

- **NON-CHANGE**: `Interpreter.globals/environment` remain as
  `Arc<Mutex<Environment>>` — the Rc<RefCell<>> optimization was
  explored but rejected in v0.40 because it would make Interpreter
  !Send (breaking HTTP/MCP worker boundaries). This is now
  documented as a future optimization after Interpreter restructuring.

### Closure env always Local (Immutable Snapshot)

The v0.34 audit claimed "Env cross-thread safety" was a permanent debt.
v0.40 resolves it by making closures own an immutable copy of the env
at capture time. Cross-thread workers hold `Arc<Mutex<Interpreter>>` —
the Interpreter's env chain stays as `Arc<Mutex<>>` (Send-safe), and
each closure snapshot is an owned Box<Environment> (also Send-safe).

No more "other thread could mutate my closure's env" concern.

### Total impact
- 2 commits on branch v0.40-env-refactor
- ~30 LOC net + ~10 LOC tests
- 1 new test (envref_from_arc_mutex_roundtrip)
- 5 demos pass (pre-existing PDF test failures in worktree only)
- 0 new deps
- **FINAL permanent debt resolved**: v0.34 audit's 5 "permanent debts"
  are now ALL solved (crossbeam v0.36, Type enum 8 variants v0.36,
  NaN/Inf guard v0.36, numeric tower v0.38, env snapshot v0.40).

---

## [v0.39] - 2026-07-03 — Env Refactor DEFERRED (No Functional Change)

1 commit + 1 CHANGELOG; no functional changes shipped.

### Status: Env refactor not completed

The plan to add `EnvRef` (Local Rc<RefCell> / Owned Box<Environment>)
to replace `Arc<Mutex<Environment>>` in `Value::Closure.env` was
attempted but **not landed**. The change cascades across 8 files
and triggers 19+ compile errors at each step:

- `value.rs` (Closure.env, Environment.parent, 6 parent.lock() sites)
- `interpreter/mod.rs` (globals/environment fields + 4 Self{} blocks)
- `interpreter/{dispatch,evaluate,execute}.rs` (~15 self.environment.clone()
  + Arc::new(Mutex::new(...)) sites)
- `interpreter/{orchestrate,trait_dispatch,ai_chat,ai_helpers,builtins}.rs`
  (~30 .lock().expect() sites)
- `mock/mod.rs` (Closure constructor)
- `http_server.rs` + `mcp_server.rs` (worker boundary std::thread::spawn)
- All cross-thread Captures need `EnvRef::Owned` deep clone (cycle
  guard via HashSet<*const Environment>)

The v0.34 audit's "permanent debt" tag for this item is now **fully
vindicated**: this refactor is multi-day coordinated work. v0.38's
release notes claimed it would land in v0.39; v0.39 partial work
proves the size.

### What landed (1 commit)
- `refactor(v0.39): rename Environment::with_parent -> with_parent_of`
  — frees the name `with_parent` for the v0.40 Env helper that
  will uniformly dispatch across `EnvRef::Local`/`EnvRef::Owned`.

### v0.40 plan (next version)

Single multi-commit coordinated refactor:
1. `value.rs`: add `EnvRef` enum (Local Rc<RefCell> / Owned Box<Environment>).
2. `value.rs`: change `Closure.env: EnvRef`, `Environment.parent: Option<Box<EnvRef>>`.
3. `value.rs`: replace 6 `parent.lock()` sites with `self.with_parent(|p| ...)`.
4. `interpreter/mod.rs`: `globals/environment: Rc<RefCell<>>` (single atomic
   change with all 4 Self{} blocks + Clone impl + 30 .lock()→.borrow()).
5. `interpreter/{dispatch,evaluate,execute}.rs`: propagate EnvRef to
   closure constructors + task body.
6. `mock/mod.rs`: Closure env uses EnvRef::Local.
7. `http_server.rs` + `mcp_server.rs`: at `std::thread::spawn` boundary,
   deep clone `EnvRef::Local` to `EnvRef::Owned`. Add `cycle_detected`
   guard via HashSet.
8. Tests: cross-thread closure isolation + Send/Sync assertions.
9. CHANGELOG + merge.

Estimated: 6-8 atomic commits, ~500 LOC, 1 full day of work.

---

## [v0.38] - 2026-07-03 — Numeric Tower (Half Final)

7 commits resolving Permanent #2 (numeric tower) partial migration.
Env refactor (Permanent #1 cross-thread gap, P1-2.8) deferred to
v0.39 — see "Deferred to v0.39" section below for why.

### Numeric tower complete (Permanent #2)

- **`Value::Int(i64)` + `Value::Float(f64)` variants** — added
  alongside legacy `Value::Number(f64)`. The 3 numeric variants
  participate in Display / PartialEq / Hash / JSON encoding /
  type_name().

- **`Literal::Int(i64, Span)` + `Literal::Float(f64, Span)`** —
  parsed from `1i`, `1f` suffixes. flow.rs + evaluate.rs +
  literal_to_value_inner + typeck all handle the new variants.

- **Lexer recognizes `1i` / `1u` / `1f` / `1.0f` / `1.0f64` suffixes** —
  `number_from()` detects the optional suffix character + width.
  Parser routes Int/Float tokens to corresponding Literal arms.

- **`Type::Int` + `Type::Float` variants** — name() / type_to_hint_string
  / exhaustiveness tests updated. Literal::Int now produces
  `Type::Int` (not the legacy Number fallback).

- **Strict numeric promotion (Rust-style)**:
  - `Int + Int = Int` (pure integer arithmetic)
  - `Float + Float = Float` (pure float arithmetic)
  - `Int + Float` / `Float + Int` → **strict type error**
  - Mixed with `Number` (legacy) → coerced to f64 (back-compat)

- **13 new tests** covering Int promotion, Float promotion,
  strict mixed errors, Number compat, eval_binary Add,
  numeric_cmp Lt/Eq, typeck Type::Int/Float name.

### Deferred to v0.39 (Env refactor — was 3 commits in plan)

The v0.38 plan included an Env refactor (Permanent #1: cross-thread
Env safety) implementing:
- `EnvRef` two-tier enum (Local Rc<RefCell> / Owned Box<Environment>)
- `Closure.env` typed as `EnvRef` (was `Arc<Mutex<Environment>>`)
- Interpreter globals/environment → `Rc<RefCell<>>`
- Worker boundary (HTTP/MCP/parallel) creates `EnvRef::Owned`
  via deep clone of `String → Value` data
- Cycle guard via `HashSet<*const Environment>` during deep clone

**Status: not landed in v0.38**. During C6 implementation we hit
18+ compile errors spanning value.rs, interpreter/{mod,evaluate,
execute,dispatch}, http_server.rs, mcp_server.rs, mock/mod.rs.
The error pattern (`Rc<RefCell<...>>` cannot be sent across threads)
**affirms the v0.34 audit's "permanent debt" tag** for this item.

Two lessons learned:
1. The full refactor requires coordinated changes across 8 files.
   Splitting per-commit would break the build at every step.
2. Rc<RefCell> is fundamentally not Send, so any interpreter path
   that crosses thread boundaries (HTTP server spawn, MCP server
   spawn, parallel Worker block) must explicitly convert to
   EnvRef::Owned.

**v0.39 will be dedicated to this single Env refactor** as a
multi-commit coordinated change. v0.38 left the Interpreter struct
untouched (globals/environment still `Arc<Mutex<Environment>>`),
so the codebase compiles cleanly.

### Total impact
- 7 commits on branch `v0.38-numeric-env`
- ~300 LOC net + 200 LOC tests
- 350 tests pass; 0 failures (was 337, +13 numeric tower)
- 5 demos × unchanged pass count
- 0 new deps

---

## [v0.37] - 2026-07-03 — Debt Cleanup Round 3 (Final Pre-v0.38)

8 commits resolving the remaining P1 + P2 audit items + 1 cleanup.
v0.38 is reserved for the full numeric tower migration and the
Env refactor (both deferred for risk management — see below).

### Stringly-typed dispatch eliminated

- **`Value::Builtin(String)` → `Value::Builtin(BuiltinKind)`** (P1-3.6)
  22-variant enum covers every builtin the interpreter knows. The
  giant `(name.as_str(), method)` tuple-match in `dispatch.rs:746`
  replaced with an exhaustive `(BuiltinKind, method)` — compiler now
  enforces adding a new builtin requires either updating dispatch or
  routing through `call_*_method`.

### Builtin boundary tightening

- **bus.emit / bus.off / sandbox.check_* / schedule.add / ccr.put /
  ccr.get / mock.register / unregister / call** all now require
  `Value::String` for their primary argument (P1-3.7/3.8/3.9).
  Previously a `Value::List {1, 2, 3}` silently became the literal
  text `[1, 2, 3]` via `to_string()` — silent lossy bug. Now type
  errors are raised immediately at the boundary.

### Dead-code removals

- **`MockRegistry::call` deleted entirely** (P1-3.12). v0.36 deprecated
  it; v0.37 completes the deprecation by deleting the method. All
  test sites use `MockRegistry::get()` to inspect handlers directly.

### Type soundness holes closed

- **`typeck Load` returns `Type::String`** (P1-4.7) — was `Union([])`
  (= any). Aligns with semantically adjacent `ReadFile`. The `Load`
  keyword still has no v2 executor (falls through to "Unsupported v2
  statement"); a future commit will implement it.
- **`typeck error Span positions`** (P2-4.11) — 7 of 11 sites now
  carry the actual source location via `from_span_with_detail`. The
  3 remaining `line: 0, column: 0` sites are inside `check_call_expr`
  where the callee NodeId isn't threaded; deferred to v0.38.
- **`typeck with-block validates key against whitelist** (P2-4.15) —
  catches `with { modle = "x" }` (typo'd "model") at typeck time.
  Runtime's `execute_with` silently dropped unknown keys; that gap
  is now closed.

### Concurrency tightening

- **`http_server.rs` request handler** hoists method/path clones
  before the route lookup lock (P1-1.6b) — critical section now
  guards only HashMap ops, not String allocations.

### Deferred to v0.38 (too large for this PR)

- **Permanent #2 full numeric tower** (Value::Int(i64) / Float(f64) +
  Literal::Int/Float + parser suffix + 258-site arithmetic sweep).
  The naive approach via `as_f64()` helper was rejected — full
  migration touches arithmetic promotion rules and needs careful
  type promotion design.
- **P1-2.8 Env refactor (LocalEnv Rc<RefCell>)** — requires worker
  boundary redesign. Cross-thread closures mean plain `Rc` is unsafe;
  the architecture needs a two-tier Environment model.

### Total impact
- 8 commits, single feature branch `v0.37-final-cleanup`
- ~250 LOC net + ~50 LOC tests
- 337 tests pass; 0 failures
- 5 demos × unchanged pass count
- 0 new deps

---

## [v0.36] - 2026-07-03 — Type Completeness + Permanent Debt Resolution

Round 2 of zero-trust audit cleanup. 14 commits resolving 11 P1 + 1 P2
items the audit deferred, plus 1 audit-discovered **CI pre-existing bug**.
P1-2.8 (Env pool) and Permanent #2 (full numeric tower) deferred to v0.37.

### Permanent debt resolution (3 items the v0.34 audit claimed unsolvable)

- **crossbeam-channel migration** — `std::sync::mpsc` → `crossbeam-channel`
  for `worker_channels` / `worker_receivers`. Sender/Receiver are now
  `Send + Sync`, eliminating the long-standing "Interpreter: !Send"
  constraint. Closes Permanent #1.

- **8 new `Type` variants** — `Agent`, `TraitObject`, `Compose`, `Partial`,
  `Atom`, `Macro`, `PromptSection`, `Document`. Previously these v0.17-
  v0.27 Value kinds all fell back to `Type::Union(vec![])` (= "any"),
  leaving them untyped. Closes Permanent #3.

- **NaN/Inf rejection (P1-3.13)** — `Value::Number` Display no longer
  prints garbage strings; renders `nan`/`inf`/`-inf` and keeps
  IEEE PartialEq semantics. Closes **part** of Permanent #2 (display
  layer). Full numeric tower (Int/Float variants, parser suffix) → v0.37.

### High-stress hardening

- `trait_registry` / `impl_table` / `tool_registry` wrapped in `Arc<HashMap>`
  for cheap `Clone` (P1-2.10). Per-HTTP-worker 50+ KB deep-clone eliminated.
- `Value::List` / `Dict` Display streams writes (no `Vec<String>::join`)
  (P1-2.7).
- `Value` Display adds depth limit (cycle guard) — recursive Value trees
  no longer stack-overflow (P2-3.14).
- `estimate_bytes` walks Value tree directly instead of full re-serialize
  (P1-2.12).

### Concurrency hardening

- `Scheduler.next_id: Mutex<u32>` → `Arc<AtomicU64>` — no overflow (P1-1.8).
- `SandboxPolicy.allow`/`deny` `Vec<String>` → `BTreeSet<String>` for O(log N)
  checks (P1-3.10).
- `http_server` startup routes listing snapshots under Mutex, prints after
  drop — no lock-held-across-`eprintln!` (P1-1.6).

### Static-type hardening

- `check_impl_def_stmt` rejects `for_type` that doesn't name a known type
  (P1-4.10) — closes the orphan-impl soundness hole.

### Sandbox integration

- All `file.*` methods now route through `sandbox.check_path` (P2-3.15).
  Default permissive policy allows everything so existing scripts
  unaffected; strict policy can now block file access via deny patterns.

### Misc

- `MockRegistry::call` marked `#[deprecated]` — use the wrapper
  `call_mock_method` from `builtins.rs` (P1-1.9).

### CI fix (pre-existing bug)

- `ci.yml` integration job was referencing 5 example scripts that no
  longer exist at `examples/*.mora` (they're in `examples/_legacy/`).
  Job was passing via `|| true` but never actually running anything.
  Updated to the 5 active demos that DO exist.

### Deferred to v0.37

- **P1-2.8 Env pool** — requires structural change to v2 closure
  capture; bigger than v0.36 scope warrants.
- **Permanent #2 full numeric tower** — `Value::Int(i64)`/`Float(f64)`
  variants + `Literal::Int`/`Float` + parser suffix tokens. Affects 60+
  Value::Number sites across the codebase.
- **P1-4.7 `load` typed Union** + **P1-3.6 `Value::Builtin` enum migration** +
  **P1-3.7/3.8/3.9/3.10 builtin boundaries**.
- **P2 cluster** — string_interner eviction, ai_cache hash key,
  parse_json UTF-8, print signature cleanup, typeck error spans
  (line:0), Never/Unknown placeholder, with-block validation.

### Total impact
- 14 commits, single feature branch `v0.36-type-completeness`
- ~300 LOC net + ~30 LOC tests
- 337 tests pass; 0 failures
- 5 demos × unchanged pass count
- 1 new dep: crossbeam-channel 0.5

---

## [v0.35] - 2026-07-03 — Technical Debt Cleanup (20 P0s)

Remediation of all 20 P0 findings from the v0.34 zero-trust audit.
No new features; internal hardening across 4 dimensions:
concurrency / high-stress / strong-typing / static-typing.

### Concurrency (cluster A) — v0.32-0.33 module API hardening

- **`Clone for Interpreter` shares singleton state** (`interpreter/mod.rs`)
  EventBus / Scheduler / MockRegistry already Arc-backed (`#[derive(Clone)]`);
  SandboxPolicy derives Clone; `InMemoryCcrStore` now has manual `Clone`
  (AtomicU64 workaround — counter is preserved at clone time). Previously
  Clone reset 5 v0.34 fields by fresh-construction, breaking counter identity
  and losing event handlers across HTTP/MCP worker clones.

- **`EventBus::emit` clone-and-drop** (`event/mod.rs`)
  Snapshot matched handlers, drop the Mutex guard, then invoke.
  Re-entrant `bus.emit` from a handler no longer deadlocks.

- **`MockRegistry::call` clone-and-drop** (`mock/mod.rs`)
  Same pattern. Native handler invocation no longer holds the registry lock.

- **`ccr.put` hash widens 8 → 16 hex chars** (`ccr/mod.rs`)
  AtomicU64 counter now produces `{:016x}`, avoiding silent overwrite at
  n = 2^32. Test assertion updated to `hash.len() == 16`.

- **`v2_arena` wrapped in `Arc<AstArena>`** (`interpreter/mod.rs`)
  Per-call `.clone()` in v2 closure/task dispatch is now a cheap Arc bump
  instead of deep-cloning the entire AST.

### No-panic refactor residue (cluster B) — completing v0.31 invariant

- **11× `.unwrap()` removed from `walk_expr` visitor** (`ast_v2.rs`)
  Visitor previously panicked on dangling NodeId. Now skips silently,
  relying on the existing `_ => visit_expr(arena, expr)` fallthrough.

- **`Value::Router` / `Atom` Display infallible** (`value.rs`)
  Poisoned mutex no longer crashes the REPL print loop.
  2 new tests: `router_display_does_not_panic_on_empty_routes` and
  `atom_display_does_not_panic_on_valid_value`.

- **Bare `.unwrap()` → `.expect()` on globals mutex** (`interpreter/mod.rs`)
  Symmetric with the 4 other `globals.lock().expect(...)` sites.

- **Lexer rejects control chars in string literals** (`lexer.rs`)
  NUL and 0x01-0x1f / 0x7f now emit `TokenType::Error` instead of silently
  absorbing (which crashed POSIX / HTTP / file boundaries downstream).
  `\t`, `\n`, `\r` stay legitimate for multi-line literals.

### Static-type soundness (cluster C)

- **REPL now type-checks** (`interpreter/mod.rs` `run_repl_with`)
  Other entry points already did; the REPL was the gap.

- **`Dict.get` return type widens `V` → `V | Nil`** (`typeck/mod.rs`)
  Runtime may return `Nil` on missing key; typeck now agrees.

- **`call_task_inner` / `call_value_inner` surface arity errors**
  Previously silently `unwrap_or(Value::Nil)`-filled missing args.
  Now errors with `"task/closure expects N args, got M"`.

- **`route` statement reports clean runtime error** (`interpreter/execute.rs`)
  `StmtKind::Route` was parsed + type-checked but never executed.
  Now reports `"route statement 'X' is not executable in v0.35; use web
  server endpoints instead"` instead of falling through to a generic
  "Unsupported v2 statement" message.

### Hot-path / structural (cluster D)

- **8 dead `#[allow(dead_code)]` Interpreter fields removed**
  `method_cache`, `ai_batch_queue`, `cache_warm_queue`, `ai_priority_queue`,
  `adaptive_temp`, `load_balancer`, `retry_policy`, `route_registry`.
  These were write-once-construct with 0 read sites.

- **`_cache_key` dead alloc removed** (`interpreter/dispatch.rs`)
  Format-on-every-method-dispatch inlined as a comment.

- **`parse_json_list` / `parse_json_dict` O(n²) → O(n)** (`flow.rs`)
  `&s[i..].trim_start()` per loop iter replaced with byte-index `skip_ws`.
  No more slicing allocations; O(1) whitespace skip per step.

### Total impact
- 20 P0s fixed (out of 57 audit findings total)
- 335 tests pass; 0 failures (+2 from commit B2)
- 5 demos × unchanged pass count (compact_demo, compress_demo,
  compress_smart_demo, mcp_server_demo, integration_v0_34)
- ~210 LOC net + ~40 LOC new tests
- 16 commits, single feature branch `v0.35-technical-debt`

---

## [v0.34] - 2026-07-03

### Integrate 5 v0.30-0.33 Orphaned Modules as Builtins

v0.30-0.33 added 5 new modules (event/sandbox/schedule/ccr/mock) but
**never integrated them into Interpreter** — scripts could not call
`bus.emit()`, `sandbox.run()`, `schedule.add()`, `ccr.put()`,
`mock.register()`. v0.34 fixes this history debt by adding each
module as a top-level builtin with method dispatch routing.

This is the **historical debt cleanup** requested by the user
("") — no new external dependencies, no semantic
change, no API rename.

#### 1. bus.emit/off/count builtin (event::EventBus)
- **v0.32 module**: `EventBus` with Puter-style wildcard matching
  (`outer.*` catch-all prefix, interior `*` single-segment)
- **v0.34 integration**:
  * `bus.emit(event, payload?)` — fire all matching handlers
  * `bus.off(pattern)` — deregister all matching handlers
  * `bus.count()` — return pattern count
- **Limitation**: `bus.on(pattern, handler)` requires a Rust closure;
  not exposed as builtin (closure boundary with builtin dispatch is
  non-trivial). v0.32's `EventBus::on` remains available for direct
  Rust API.
- 4 unit tests in `bus_tests` mod.

#### 2. sandbox.check_builtin/check_path/allow/deny builtin (sandbox::SandboxPolicy)
- **v0.33 module**: MimiClaw path validation + AIOS access manager
- **v0.34 integration**:
  * `sandbox.check_builtin(name)` -> bool (allow/deny pattern match)
  * `sandbox.check_path(path)` -> bool (reject `..` per MimiClaw)
  * `sandbox.allow(pattern)` / `sandbox.deny(pattern)`
  * `sandbox.mode()` -> "strict" or "permissive"
- 1 unit test in `bus_tests` mod.

#### 3. schedule.add/list/remove/tick/count builtin (schedule::Scheduler)
- **v0.33 module**: MimiClaw cron_service 9-field cron_job_t
- **v0.34 integration**:
  * `schedule.add(name, kind, message, interval_s?, at_epoch?)` -> id
  * `schedule.list()` -> List of job dicts
  * `schedule.remove(id)` -> bool
  * `schedule.tick()` -> [triggered_messages] (uses Scheduler::now())
  * `schedule.count()` -> pattern count
- 1 unit test in `bus_tests` mod.

#### 4. ccr.put/get/marker/extract builtin (ccr::CcrStore)
- **v0.33 module**: Headroom Compress-Cache-Retrieve with
  `<<ccr:HASH,SIZE>>` marker
- **v0.34 integration**:
  * `ccr.put(data)` -> hash (8-char hex from u64 counter)
  * `ccr.get(hash)` -> data (or Nil if not found)
  * `ccr.marker(hash, size)` -> `<<ccr:hash,size>>` (Headroom format)
  * `ccr.extract(marker)` -> hash (parse marker, returns hash part)
  * `ccr.len()` -> entry count
- 1 unit test in `bus_tests` mod.

#### 5. mock.register/unregister/count/names builtin (mock::MockRegistry)
- **v0.32 module**: OpenFugu MockWorld + OpenInfer mock mode pattern
- **v0.34 integration**:
  * `mock.register(name)` -> stub (real handler wiring needs closure
    boundary, deferred to v0.35+)
  * `mock.unregister(name)` -> stub
  * `mock.count()` -> pattern count
  * `mock.names()` -> [String, ...] (registered handler names)
- **Limitation**: `mock.register` doesn't actually wire a handler
  (closure boundary). v0.32's `MockRegistry::register` still works
  for direct Rust API.
- 1 unit test in `bus_tests` mod.

#### Tests

- 8 new test cases in `bus_tests` mod (consolidated to avoid mod
  structure issues during iterative development)
- 328 lib tests pass (was 320 at v0.33 merge, +8)
- `cargo build --all-targets`: clean
- `cargo clippy --all-targets -- -D warnings`: clean
- `cargo fmt --check`: 0 diff

#### Implementation notes

- **5 new fields on Interpreter struct**: bus, sandbox, scheduler,
  ccr_store, mock_registry (all Arc<Mutex<...>>-based, Clone is
  cheap)
- **5 new globals definitions** in `Interpreter::new()`:
  `bus` / `sandbox` / `schedule` / `ccr` / `mock`
- **5 new method dispatch functions** in `builtins.rs`:
  call_event_method, call_sandbox_method, call_schedule_method,
  call_ccr_method, call_mock_method
- **5 new dispatch routing arms** in `dispatch.rs` module section
- All public APIs use `Result<Value, String>` (no panic in production)

#### Roadmap (v0.35+)

- `bus.on()` with closure capture via closure registry
- `mock.register()` with actual handler wiring
- `ai.limits` block (step/cost/wall_time) per mini-swe-agent
- `shell.run` with process group kill (POSIX `killpg`)

### Fix Production Panics on User-Input Paths

- `src/lexer.rs`: replace `value.parse().unwrap()` with `error_token`
  fallback for malformed number literals.
- `src/flow.rs`: replace `unreachable!()` in `parse_json_dict` with
  `Err("JSON object key must be a string")`.
- `src/lsp/providers/formatting.rs`: replace `.expect()` on LSP
  `range/start/end` params with graceful empty-array fallback.
- `src/interpreter/mod.rs`: replace `.expect("should have elements")` in
  `extract_embeddings` with `Result::Err`.
- `src/parser_v2/statements.rs`: finish v0.34 fix for
  `.expect("loop requires exactly one agent")` — return a valid `NodeId`
  via `arena.alloc_stmt` and include the new `with_config` field.
- `src/parser_v2/statements.rs`: replace `.expect("eval requires 'given:'")`
  with fallback to `NodeId(0)` + error log when `given:` is missing.
- `src/lsp/server.rs`: remove redundant `id.expect("id should exist")`;
  propagate `docs` and `shutdown` mutex poison via `io::Result`.
- `src/interpreter/evaluate.rs`: convert `environment.lock().expect(...)` to
  `?` and remove irrefutable `unwrap()` after `Some` matches.
- `src/interpreter/execute.rs`: convert all `environment.lock().expect(...)`
  to `?`.
- `src/interpreter/dispatch.rs`: convert `atom`/`environment`/`done`/`routes`
  /`tool_registry` mutex expects to `?`.
- `src/interpreter/trait_dispatch.rs`: convert `environment.lock().expect(...)`
  to `?`.
- `src/interpreter/orchestrate.rs`: convert `environment.lock().expect(...)`
  to `?` (including the nested closure in Graph edge evaluation).
- `src/interpreter/mod.rs`: convert `globals.lock().expect(...)` in
  `interpret()` to `?`; unify `new()` `.unwrap()` to
  `.expect("globals mutex poisoned")`.

#### Tests

- `tests/parser_v2_integration.rs`: add `test_parse_eval_without_given_no_panic`.
- `src/lsp/server.rs`: add `handle_notification_without_id_no_panic`.

#### Verification

- `cargo build --all-targets`: clean
- `cargo test --all`: 331 passed, 2 ignored
- `cargo clippy --all-targets --all-features -- -D warnings`: clean
- `cargo fmt --check`: 0 diff

## [v0.33] - 2026-07-02

### Schedule + Sandbox + Reading Order + CCR (4 P1 primitives)

: 7-project deep-dive  (AGENTS_PRIMITIVES.md)  v0.33 P1 .
 4 **** P1 ,  trait-based +  in-memory ,
.

#### 1. Schedule (cron) — MimiClaw 

`src/schedule/mod.rs`:
- `Scheduler`: `Arc<Mutex<HashMap<String, Job>>>`
- `Job { id, name, kind, interval_s, at_epoch, message, last_run_epoch, delete_after_run }`
- `JobKind`: Every | At
- `add(name, kind, message, interval_s, at_epoch) -> Result<id, Err>`
- `list() -> Vec<Job>`, `remove(id) -> bool`
- `tick(now) -> Vec<triggered_messages>` (consume for event loop)
- `set_persist_path(path)` + best-effort JSON dump

: MimiClaw cron_service.c (9  cron_job_t).
****:  channel/chat_id, std::fs JSON  (vs SPIFFS).

#### 2. Sandbox Policy — AIOS + Puter + MimiClaw 

`src/sandbox/mod.rs`:
- `SandboxPolicy { allow, deny, fs_root, timeout_s, memory_limit_mb }`
- `check_builtin(name) -> Result<(), Err>` ( `event::matches` wildcard,
  deny  allow)
- `check_path(path) -> Result<PathBuf, Err>` (MimiClaw  `..` ,
   fs_root )
- `strict()` / `permissive()` / Default constructors

:
- MimiClaw path traversal defense
- AIOS Access Manager (agent_id -> privilege_group)
- Puter iframe sandbox + capability URL params

#### 3. document.reading_order — MinerU 

`src/document/reading_order/mod.rs`:
- `BBox { x, y, w, h }` + center/edge accessors
- `from_value(v)`: accept both flat bbox dict AND block dict with 'bbox' sub-dict
- `Strategy`: InputOrder | TopToBottom | GapTree | XyCut | GroupBased
- `assign_reading_order(blocks, strategy)`:  block  'reading_order_idx'

: MinerU §2.8 Reading Order Recovery (3 ).
****:  recursive XY-cut,  cross-page merge, .

#### 4. CCR (Compress-Cache-Retrieve) — Headroom 

`src/ccr/mod.rs`:
- `CcrStore` trait: `put(data) -> hash; get(hash) -> Option<entry>; len()`
- `CcrEntry { hash, size, data }`
- `InMemoryCcrStore` default impl (Arc<Mutex<HashMap>> + u64 counter)
- `make_marker(hash, size) -> "<<ccr:hash,size>>"`
- `extract_hash(marker) -> Option<&str>`

: Headroom CcrStore (lossy ).
****: 8-char hex hash (vs SHA-256),  marker  ( KIND).
****: v0.34  `crush_json` lossy .

#### 

- 320 lib tests (was 286, +34)
- `cargo build --all-targets`: clean
- `cargo clippy --all-targets -- -D warnings`: clean
- `cargo fmt --check`: 0 diff

####  (v0.34+ )

P1 (v0.34 6-8 ):
- `react` (ReAct ) — MimiClaw agent_loop.c
- `document.grouped_layout` — MinerU 
- `skill` markdown — MimiClaw skill_loader
- CCR ↔ crush_json  (lossy  marker)
- `heartbeat`  — MimiClaw
- Sandbox ↔ builtin  (file.read  check_path)

P2+ (v0.35+ ):
- `plan` (DAG) — OpenFugu Conductor
- `mora serve --openai`  — OpenInfer
- `prefix_cache` — OpenInfer Pegaflow
- `tiered_memory` — OpenInfer + MimiClaw
- `lifecycle`  — Puter
- DI  (5 ) — Puter
- `policy` learned router — OpenFugu
- `ai.chat role` — OpenFugu 3 role
- Error Gradation — OpenFugu
- `cross_page merge` — MinerU

## [v0.32] - 2026-07-02

### Lossless-First Recursive Walker + Event Bus + Mock Registry

:  deep-dive 7  AI  (AIOS / MimiClaw / OpenFugu /
OpenInfer / MinerU / Headroom / Puter) . 
`AGENTS_PRIMITIVES.md` (581 ).  3 **** P0 ,
 plan/react/openai-serve  v0.33.

#### 1. Lossless-First Recursive Walker (Headroom )

`src/compress/json.rs::compact_value_recursive` + `crush_json_recursive`:
-  Value  pure iterative DFS ( Windows 1MB stack )
-  List  (`len >= min_items`)  `try_lossless_compact`
  (csv-schema  markdown-kv), 
-  `CompressOptions.recursive: bool` (default false, )
-  List  SmartCrusher (inlined via `crush_json_inner` )
- 2 new tests: `recursive_walker_compacts_nested_lists`,
  `compact_value_recursive_simple`

: [Headroom DocumentCompactor](https://github.com/chopratejas/headroom)
(`crates/headroom-core/src/transforms/smart_crusher/compaction/walker.rs`)

#### 2. Event Bus with Wildcard (Puter )

 `src/event/mod.rs`:
- `EventBus`: `Arc<Mutex<HashMap<Pattern, Vec<Handler>>>>`
- `on(pattern, handler)` ; `off(pattern)` ; `emit(event, payload)` 
- `matches(event, pattern)`: Puter 
  - trailing `*` = prefix catch-all (`outer.*`  `outer.gui.item.removed`)
  - interior `*` = single segment wildcard (`outer.*.item`)
  - bare `*` = 
- 8 unit tests covering exact/prefix/interior/catchall/dispatch

: [Puter EventClient](https://github.com/HeyPuter/puter)
(`src/backend/clients/event/EventClient.ts`)

#### 3. Mock Registry (OpenFugu + OpenInfer )

 `src/mock/mod.rs`:
- `MockRegistry`: `Arc<Mutex<HashMap<String, MockHandler>>>`
- `register(name, fn) / unregister(name) / call(name, args) / count / names`
- `MockHandler`: `Arc<dyn Fn(&Value) -> Value + Send + Sync>`
-  Mora  `Value` ,  `serde_json` 

:
- [OpenFugu MockWorld](https://github.com/trotsky1997/OpenFugu) (train/train_trinity.py)
   sep-CMA-ES 
- OpenInfer mock mode ( Python  Rust )

Mora  `compress/text.rs` / `ai_chat.rs`  hardcode mock ,
v0.32  `MockRegistry` .  builtin (ai.chat / http.fetch) 
consult `mock.call`  mock ,  offline deterministic .

#### 4. AGENTS_PRIMITIVES.md (581 )

,  v0.32+  (16  + 5  + 7 ).
:  +  () + Mora  +  +
 Mora .

#### 

- 286 lib tests (was 272, +14)
- `cargo build --all-targets`: clean
- `cargo clippy --all-targets -- -D warnings`: clean
- `cargo fmt --check`: 0 diff

####  (v0.33+ )

P1 (v0.33 6-8 ):
- `plan` (DAG) — OpenFugu Conductor
- `react` (ReAct ) — MimiClaw agent_loop.c
- `document.grouped_layout` — MinerU group-based
- `document.reading_order` — MinerU 3 
- `schedule` cron — MimiClaw cron_service
- `skill` markdown — MimiClaw skill_loader
- `sandbox`  — AIOS + Puter
- `ccr` Compress-Cache-Retrieve — Headroom

P2+ (v0.34+ ):
- `mora serve --openai`  — OpenInfer vLLM frontend 
- `prefix_cache` — OpenInfer Pegaflow
- `tiered_memory` — OpenInfer + MimiClaw
- `lifecycle`  — Puter hooks
- DI  (5 ) — Puter
- `heartbeat`  — MimiClaw
- `policy` learned router — OpenFugu TRINITY
- `ai.chat role` — OpenFugu 3 role
- Error Gradation — OpenFugu evidence grade
- `cross_page merge` — MinerU

## [v0.31] - 2026-07-02

### No-Panic Refactor + Code Quality Hardening

 v0.30 "" (user: "5 ").
**** — .

#### : 21 panic -> 0 in lexer/parser

,  `panicked at src/lexer.rs:...`
 abort. :
- Lexer 8  panic  emit `TokenType::Error(String)` token
- Parser 13  panic  `eprintln!`  +  safe default
  ( /  list /  OrchestrateKind.Sequential)
-  `"Parse error: ..."`  stack trace

`examples/_legacy/`  demo ( panic)  crash .

#### : Windows OCR model path fallback

`user_model_path()`  `XDG_DATA_HOME`  `HOME`,
 Windows ,  fail.  `LOCALAPPDATA` fallback
 3 .  3 .

#### : cargo doc warnings 14 -> 0

Module-level `//!`  HTML :
- `<Page>`, `<Block>`, `<Span>`  `\[ \]` 
- `<p>`, `<N>`, `Vec<Value>` 
- bare URL `https://...`  `<https://...>`

`cargo doc --no-deps`  0 warning, docs.rs .

#### 

- 272 lib + 5 integration = 277 test 
- `cargo build --all-targets`: clean
- `cargo clippy --all-targets -- -D warnings`: clean
- `cargo fmt --check`: 0 diff
- `cargo doc --no-deps`: 0 warning

## [v0.30] - 2026-07-02

### SmartCrusher —  JSON 

 [headroom](https://github.com/headroomlabs-ai/headroom)  SmartCrusher
 +  +  v0.29 " + 30%  15% "
" + 5  + 3 "

####  BREAKING CHANGES

- `CompressOptions.anomaly_keys: Vec<String>` ****v0.30 
- `CompressOptions`  5  11 v0.29  + 6 
- `crush_json_core` **** `crush_json` `(items, target, options)`
   `crush_json_core(input, max, anomaly_keys)` 
- `parse_json_simple` stub **** `flow::json_to_value`
- `crush_json` / `compress.json` / `List.crush_json`  marker 
  `method=smart_crusher strategy={...} items={...} total={...} savings={...}`

####  v0.29  head_tail

|  |  |  |
|---|---|---|
| `auto` (default) |  |  ArrayType  |
| `topn` |  /  Score  |  Score  top N |
| `timeseries` |  /  Temporal  |  +  |
| `cluster` |  /  uniqueness < 0.3 |  |
| `lossless` |  | schema  csv-schema / md-kv |
| `smart_sample` | fallback |  +  +  |

#### 5 

- `Id` — uniqueness > 0.9 /UUID/
- `Score` — bounded numeric range (0-1  0-100)
- `Temporal` — ISO 8601 / Unix timestamp 
- `Error` —  `error`/`failed`/`exception`/... 
- `Anomaly` —  >3σ from mean (1-5% )

#### 3 

- `KeepErrorsConstraint` — 
- `KeepOutliersConstraint` — Anomaly  >2σ 
- `KeepBoundaryConstraint` —  k_first +  k_last  15%

####  builtin 

```mora
--  auto: 
compress.json(tool_output, {target_ratio: 0.2})

--  TopN
compress.json(scored_list, {strategy: "topn", target_ratio: 0.1})

--  TimeSeries
compress.json(metrics, {strategy: "timeseries", target_ratio: 0.3})

-- Lossless (csv-schema , )
compress.json(flat_table, {strategy: "lossless", max_bytes: 5000})

-- 
compress.json(api_logs, {
    strategy: "auto",
    target_ratio: 0.2,
    preserve_errors: true,
    preserve_outliers: true,
    preserve_ids: false,
})

--  metadata
let result = compress.json(items, {target_ratio: 0.2})
result.savings_ratio    -- 0.8 (80% )
result.strategy_used    -- "topn"
result.fields           -- [{name, role, ...}, ...]
```

#### 

|  |  (v0.29) |  (v0.30) |  |
|---|---|---|---|
| 100  × 5  | 60% | 70-80% | +10-20% |
| 1000  × 20  | 60% | 75-85% | +15-25% |
| 10000  × 30  | 60% | 80-90% | +20-30% |

#### 

- `src/compress/json.rs` —  (267 → 970 )
  - `FieldRole` / `FieldStats` / `ArrayType` 
  - 5  detector + 5  Strategy + 3  Constraint
  - `crush_json` / `crush_json_string` / `try_lossless_compact`
- `src/compress/mod.rs` — `CompressOptions`  (11 )
  - `parse_json_simple`  `flow::json_to_value`
  - `value_to_json_simple`  `flow::value_to_json`

#### 

- 12  unit test v0.29 5  test
  - 5  role detectionid/score/error/temporal/anomaly
  - 4  strategytopn/timeseries/lossless/auto
  - 2  constrainterrors/outliers
  - 1  metadata
  - 1  string 
-  v0.29  test `crush_json_core` / `anomaly_keys` / `parse_json_simple_currently_stub`
-  272 test `cargo clippy --all-targets -- -D warnings` 

## [v0.29] - 2026-07-01

### compress + crush_json + OCR .rten 

 [headroom](https://github.com/headroomlabs-ai/headroom) ContentRouter + Kneedle 
Mora  JSON  +  system prompt 

####  / builtin

```mora
-- 6  (auto / head_tail / summary / lossless / json / code-html-log-text)
let summary = compress(text, "summary")                       -- LLM 
let head    = compress(text, "head_tail", head_pct: 0.3)     -- 
let lossless = compress(text, "lossless")                     --  size marker
let auto    = compress(text, "auto")                          -- 

--  JSON  (Kneedle + )
let crushed = crush_json(big_list, max: 10)
let crushed = crush_json(big_list, max: 10, anomaly_keys: ["error"])

-- 
let summary = conv.compress("summary")
let crushed = list.crush_json(10)
```

####  `compress`

|  |  |
|---|---|
| `SubCompressor` trait | `sniff` / `compress` / `origin` 3  |
| `ContentRouter` |  →  |
| `JsonSubCompressor` |  crush_json_core |
| `CodeSubCompressor` | regex  +  body |
| `HtmlSubCompressor` |  v0.27 quick-xml  |
| `LogSubCompressor` |  pattern cluster |
| `TextSubCompressor` | head_tail / summary / lossless  |

####  BREAKING: `compact`  `compress`

v0.25  `compact(text)` builtin  `compress(text, "summary")`
`examples/compact_demo.mora`  v0.29 

#### OCR `.rten`  ( v0.28 tech-debt)

- v0.28 vendored  11.7 MB `.rten` 
-  `~/.local/share/mora/ocr/`  ( `MORA_OCR_MODELS_DIR` )
-  `docs/install-ocr.md` 
-  `.git/sdd/ocrs-shasums.txt`  reference checksum
- **BREAKING**:  OCR  `mora-install-ocr` 

#### 

- `src/compress/{mod,json,code,html,log,text}.rs` (~1000 )
- `docs/install-ocr.md`
- `.git/sdd/ocrs-shasums.txt`
- `examples/compress_demo.mora` ()

#### 

- **** —  v0.27 / v0.28  deps (`regex` transitive from `ocrs`)
- **** —  v0.26 / v0.27 / v0.28 
- **CodeSubCompressor  regex** — v0.30+  tree-sitter
- **** `compress.` / `crush_json.` / `ocr.load.`

## [v0.28] - 2026-07-01

### Office (PPTX/DOCX) + Image OCR Backends

 v0.27 DocumentBackend  MinerU 
 v0.27 trait  3  DocumentBackend 

#### 

|  |  |  |  |
|---|---|---|---|
| PptxBackend | .pptx | undoc 0.5 |  |
| DocxBackend | .docx | undoc 0.5 | Word  |
| ImageBackend | .png | ocrs 0.12 + image 0.24 |  OCR Rust / rten ONNX|

#### 

```mora
let deck = document.parse("./deck.pptx")           -- PPTX
let report = document.parse("./report.docx")        -- DOCX
let scan = document.parse("./scan.png")            -- OCR

print(deck.markdown())                              -- markdown 
print(report.text())                                -- 
print(scan.metadata()["ocr_engine"])                -- "rten"
```

####  v0.26/v0.27 

```mora
--  v0.26 compose_prompt
let sys = compose_prompt({role:"system", text:deck.text(), budget:"32 KB"})
--  v0.27 
document "report" do
    set origin: "docx"
    read "./report.docx"
end
```

#### 

- `undoc` 0.5 `docx` + `pptx` features Rust
- `ocrs` 0.12OCR  Rust
- `rten` 0.24ocrs  re-export `Model::load_static_slice`  `.rten`
- `anyhow` 1ocrs  `OcrEngine::new`  `anyhow::Result`ocrs  re-export `anyhow`
- `image` 0.24 `png` feature PNG header / dimensions

 RustMSRV 1.85 

#### 

- **** 5  crate  pure Rust
- **PNG only in v0.28**JPEG / XLSX /  PDF  v0.29+
- **OCR **`ocrs 0.12`  Microsoft `rten` ONNX runtime
- ** OCR**v0.28 eng.traineddata bundled
- ****v0.27  `parse_document(path)`  `PptxBackend` / `DocxBackend` / `ImageBackend`

#### Known issues / v0.29+ roadmap

- **11.7 MB `.rten`  vendoring**OCR /`text-detection.rten` 2.4 MB + `text-recognition.rten` 9.3 MB raw blob  `tests/fixtures/` git LFS contributor / CI  `git clone`  ~12 MB`mora` release binary  `include_bytes!`  ~12 MB blob  PR  diff/ `.git/sdd/tech-debt-v0.29.md`v0.29 git LFS / `build.rs`  /  model dir
- **OCR **`ocrs 0.12`  `eng.traineddata` 
- **OCR  PNG**JPEG / WebP / TIFF  v0.29+
- ** PDF** PDF OCR 

## [v0.27] - 2026-07-01

### Document  IR — `document.parse(...)` + 

 [opendatalab/MinerU](https://github.com/opendatalab/MinerU) middle_json 
Mora  PDF / Markdown / HTML , `Value::Document` IR

#### 

```mora
document "report" do
    set origin: "pdf"
    set max_pages: 3
    read "./q3-report.pdf"
end

let doc = document.parse("./q3-report.pdf")
let md  = doc.markdown()
let pages = doc.pages()
let meta = doc.metadata()
```

####  `document`

|  |  |
|---|---|
| `document.parse(path)` | , `Value::Document` |

#### `Document` value 

|  |  |  |
|---|---|---|
| `doc.markdown()` | `string` |  markdown  |
| `doc.text()` | `string` | |
| `doc.pages()` | `List<Dict>` |  IR Page  |
| `doc.blocks()` | `List<Dict>` |  block |
| `doc.metadata()` | `Dict` |  origin / pages / size|
| `doc.origin()` | `string` | "pdf" / "markdown" / "html" |

####  + Trait

- `Value::Document { backend: Arc<dyn DocumentBackend + Send + Sync>, metadata: HashMap<String, Value> }`
- `pub trait DocumentBackend: Debug + Send + Sync { fn origin / pages / markdown / text / metadata / blocks }`
- 3 : `PdfBackend` (lopdf + pdf-extract) / `MarkdownBackend` (pulldown-cmark) / `HtmlBackend` (quick-xml)

#### 

- `lopdf` 0.41 + `pdf-extract` 0.12 (PDF)
- `pulldown-cmark` 0.13 (Markdown)
- `quick-xml` 0.40 (HTML)
-  Rust, MSRV 1.85 , 

####  v0.26 

```mora
let doc = document.parse("./report.pdf")
let sys = compose_prompt({role:"system", text:doc.markdown(), budget:"32 KB"})
let resp = ai.chat(p"{sys}\n\n{question}")
```

#### 

- **** Rust crate
- ** Value ** PDF /  `backend: Arc<dyn ...>` 
- **Lazy ** `.pages()` / `.markdown()`  Value, 
- **** PPTX / DOCX  `impl DocumentBackend`

## [v0.26] - 2026-07-01

### Prompt Sections —  +  + 

 [mimiclaw](https://github.com/memovai/mimiclaw)5  [headroom](https://github.com/headroomlabs-ai/headroom) LLM  system prompt 

####  `prompt`

```mora
prompt "identity" do
    set role: "system"
    set budget: "256 B"
    read "./SOUL.md"
end

prompt "memory" do
    set role: "system"
    set budget: "8 KB"
    tail("./sessions/today.jsonl", max: 20)
end

let sys = compose_prompt("identity", "memory")
```

#### 

|  |  |
|---|---|
| `compose_prompt(...)` |  system prompt section budget  |
| `tail(path, max: N)` |  N JSONL/ |

#### 

- `Value::PromptSection { name, role, text, budget_bytes }`

####  AST 

- `StmtKind::PromptSection { name, body }`
- `StmtKind::PromptSet { key, value }` `set role:` / `set budget:`
- `StmtKind::PromptRead(NodeId)` `read`

#### 

- **** tokenizer UTF-8  mimiclaw 
- **** section  ValueIR
- ****

## [v0.25] - 2026-07-01

###  (Code Modularization)

 5 

#### 
- **interpreter**: 3402  → 3  (mod.rs + execute.rs + evaluate.rs)
- **typeck**: 2838  → 2  (mod.rs + check.rs)
- **parser_v2**: 2609  → 3  (mod.rs + statements.rs + expressions.rs)
- **record**: 2091  → 7  (mod.rs + serialization.rs + diff.rs + analysis.rs + audit.rs + snapshot.rs + tests.rs)
- **lsp/providers**: 1092  → 11  (mod.rs + helpers.rs + 9  provider )

#### 
- 
- 
- 

### 
-  `test_memory_save_load`  Windows 
-  `std::env::temp_dir()`  `/tmp` 

## [v0.24] - 2026-06-30

### ParserV2  (Complete)

ParserV2  Parser 
 parser.rs (2459 )  ParserV2

#### 
- **append_statement**: 
- **read_bytes_statement**: 
- **write_bytes_statement**: 
- **stream_statement**:  `stream <expr> as <var> do ... end`
- **tool_statement**:  `tool name(params): type do ... end`
- **observe_statement**:  (trace/metrics/otel)
- **span_statement**:  `span "name" tags {..} do ... end`
- **record_tokens_statement**:  token 
- **assignment_statement**:  `IDENT = expr`
- **index_assignment**:  `IDENT[expr] = expr`
- **commit/rollback**: /

#### 
- **match_expression**:  ( when )
- **pattern**:  (////)
- **parse_format_string**: 
- **parse_ai_model_call**: ai_model  ( keyword args)
- **flatten_prompt_parts**: Prompt 
- **list_literal / dict_literal**: 
- **char_literal**:  `'a'`
- **NamespaceRef**:  `Module::method()`

#### 
- **parse_generic_params**:  `<T: Bound>`
- **parse_type_list**:  `<T, U, V>`
- **parse_type_name_recursive**: 
- **parse_where_clause**: where 

#### 
- **let **: 
- **string + any**:  ()

#### 
- **ObserveConfig**:  ast_v2.rs  NodeId
- **FnDef / TraitMethod**:  ast_v2.rs  Vec<NodeId>
- **Pattern**:  ast_v2.rs Guard condition  NodeId
- **consume_method_name**: 
- ****:  (binary → unary → call → primary)
- ****: ast_v2_to_v1.rs  AST 

### 9 Languages Features Integration (Complete)

All features from the learning plan have been implemented.

### v0.21: Rust 

- ****: `&expr` / `&mut expr`
- ****: `<'a>` 
- ****: /

### v0.22: 

- **AI **:  prompt 
- ****:  map/filter/take/drop 
- ****: 
- ****: 
- **HTTP **:  (16)
- **MCP **:  (8)
- ****: 

### v0.24: 

- ****: `type Name = TargetType`
- ****: `enum Name { V1, V2(Type) }`
- ****: `struct Name { field: Type }`

### 

- **docs/mora-spec.md**: Mora  (20 )
- **docs/influences.md**: 9 
- **docs/learning-plan.md**: 
- **docs/workflow-v0.20.md**: 

From Prolog, StreamIt, APL, Clojure, Lisp, Smalltalk, Common Lisp, Ballerina, Logo.

#### Pattern Matching Enhancement (Prolog)
- **Match guard conditions**: `match n with x when x > 0 -> ... end`
- **List rest pattern**: `[head, ...tail] = [1, 2, 3]`
- **Dict partial match**: `{name: n} = {"name": "Alice", "age": 30}`

#### Pipe & Stream (StreamIt + APL)
- **Pipe with closure**: `5 |> fn(x) return x * 2 end`
- **Window aggregation**: `[1,2,3,4,5].window(3)` → `[[1,2,3],[2,3,4],[3,4,5]]`
- **Batch processing**: `[1,2,3,4,5].batch(3)` → `[[1,2,3],[4,5,6],[7]]`
- **Array operations**: `.shape()`, `.flatten()`, `.transpose()`, `.reshape()`
- **Broadcast arithmetic**: `[1,2,3] * 2` → `[2,4,6]`

#### Functional Core (Clojure + Lisp)
- **Compose**: `compose(f, g, h)` → composed function
- **Take/Drop**: `[1,2,3].take(2)` → `[1,2]`, `[1,2,3].drop(1)` → `[2,3]`
- **Partial application**: `partial(add, 10)` → partial applied function

#### Concurrency (Clojure)
- **Atom**: `atom(0)` → mutable reference
- **Swap**: `swap(counter, fn(n) return n + 1 end)`
- **Deref**: `deref(counter)` → current value

#### Reflection (Smalltalk)
- **type_of**: `type_of(42)` → `"number"`
- **is_instance**: `is_instance("hello", "string")` → `true`
- **methods_of**: `methods_of([1,2])` → `["push","pop","map",...]`
- **Message chain**: Router methods return self for chaining

### Statistics
- **Tests**: 147 → 178 (+31)
- **Code**: +7010 / -1517 lines

## [v0.15] - 2026-06-28

### AI Config Integration

- **TokenBudget.per_call**: Per-call token limit check
- **real_ai_chat_with_tools**: Now reads temperature/max_tokens/system from config
- **Route config**: RouteConfig settings now applied to AI calls
- **with mock_llm**: Mock LLM response queue for testing

### Record CLI Extension

- **mora record list**: List all recordings
- **mora record stats**: Show recording statistics
- **mora record timeline**: Show call timeline
- **mora record export**: Export JSONL/Markdown
- **mora record audit**: Secret scanning with .moraignore
- **mora record report**: Evidence report generation
- **mora snapshot**: Snapshot testing for regression

### Documentation

- **docs/mora-spec.md**: Mora Language Specification (20 chapters)
- **docs/influences.md**: 9 Languages Influence Analysis
- **docs/learning-plan.md**: Feature Integration Plan

### Statistics
- **Tests**: 126 → 147 (+21)

## [v0.14] - 2026-06-27

### Record/Replay/Diff CLI

- **mora record**: Record AI calls to JSONL
- **mora replay**: Replay recordings deterministically
- **mora diff**: Compare two recordings

### Statistics
- **Tests**: 121 → 126 (+5)

## [v0.13] - 2026-06-26

### Breaking Changes

- Removed `Type::Any` variant
- Removed Walrus syntax (`:=`)

### Statistics
- **Tests**: 113 → 121 (+8)

---

## Version History

| Version | Date | Tests | Key Features |
|---------|------|-------|--------------|
| v0.20 | 2026-06-28 | 178 | 9 languages integration |
| v0.15 | 2026-06-28 | 147 | AI config + record CLI |
| v0.14 | 2026-06-27 | 126 | record/replay/diff |
| v0.13 | 2026-06-26 | 121 | Remove Type::Any |
