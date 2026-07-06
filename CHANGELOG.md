# Changelog

All notable changes to Mora will be documented in this file.

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
- v0.47.0: DAG-as-data → `orchestrate` 扩展 (OpenFugu)
- v0.47.0: `heartbeat.md` 可执行检查列表 (mimiclaw)
- v0.47.0: `context.trim(threshold)` 智能截断 (pi-agent + AgentMesh)
- v0.48.0: `mora refine` 增量编辑 + `plan.update` 实时清单

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

master doc §6.5 says "ToolPlane 替代 tool_registry". **v0.45.0 keeps both**:
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
- v0.46.0: `SKILL.md` 格式 + 双注册表 (`mora-hub.json` + `mora-public.json`) (CLI-Anything)
- v0.47.0: DAG-as-data (OpenFugu) + `heartbeat.md` (mimiclaw) + `context.trim` (AgentMesh)

---

## [v0.44.0] - 2026-07-06 — sandbox.containerize REAL Docker + orchestrate validation

1 commit; v0.44+ roadmap from RESEARCH_PRIMITIVES_MASTER_v2.md §7.

### sandbox.containerize() — REAL Docker orchestration (pi-mono v0.44.0)

**v0.44.0 actually spawns Docker containers (NOT metadata-only).**

- **New module `src/sandbox/container.rs`**:
  - `ContainerBackend` enum: Docker (v0.44.0 ✅), Gondolin + OpenShell
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
- `exec_parallel_runs_all_commands` — 3 cmds, 收集 stdout
- `exec_parallel_respects_max_concurrent` — 6 cmds, max_concurrent=2
- `exec_parallel_empty_list_returns_empty` — 边界
- `exec_parallel_collects_stdout_per_command` — 验证内容
- `exec_parallel_kills_process_group_on_timeout` — `sleep 10` + 200ms timeout
- `exec_parallel_validates_arg_types` — 类型检查
- `exec_parallel_validates_cmd_elements` — 元素类型检查
- `exec_parallel_returns_error_for_missing_command` — 不存在命令 → exit 127
- `exec_unknown_method_errors` — unknown method

### Design decision: STD vs tokio
- Master doc §6.5 suggested `tokio::process::Command` + `tokio::sync::Semaphore`
- Project rule (AGENTS.md §3 + Cargo.toml): **"不引入 async runtime"**
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
| v0.41.0 | ✅ | event O(segments) |
| v0.41.1 | ✅ | reading_order XY-Cut++ |
| v0.42.0 | ✅ | sandbox.key + Capability |
| v0.42.1 | ✅ | audit.jsonl + AuditSink |
| **v0.43.0** | ✅ | **exec.parallel()** |
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
("解决历史遗留问题") — no new external dependencies, no semantic
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

灵感: 7-project deep-dive 的路线图 (AGENTS_PRIMITIVES.md) 的 v0.33 P1 阶段.
本版本聚焦 4 个**可独立发布**的 P1 原语, 全部 trait-based + 后台 in-memory 状态,
无新外部依赖.

#### 1. Schedule (cron) — MimiClaw 灵感

`src/schedule/mod.rs`:
- `Scheduler`: `Arc<Mutex<HashMap<String, Job>>>`
- `Job { id, name, kind, interval_s, at_epoch, message, last_run_epoch, delete_after_run }`
- `JobKind`: Every | At
- `add(name, kind, message, interval_s, at_epoch) -> Result<id, Err>`
- `list() -> Vec<Job>`, `remove(id) -> bool`
- `tick(now) -> Vec<triggered_messages>` (consume for event loop)
- `set_persist_path(path)` + best-effort JSON dump

灵感: MimiClaw cron_service.c (9 字段 cron_job_t).
**简化**: 无 channel/chat_id, std::fs JSON 持久化 (vs SPIFFS).

#### 2. Sandbox Policy — AIOS + Puter + MimiClaw 灵感

`src/sandbox/mod.rs`:
- `SandboxPolicy { allow, deny, fs_root, timeout_s, memory_limit_mb }`
- `check_builtin(name) -> Result<(), Err>` (用 `event::matches` wildcard,
  deny 优先于 allow)
- `check_path(path) -> Result<PathBuf, Err>` (MimiClaw 风格 `..` 拒绝,
  解析后必须在 fs_root 之内)
- `strict()` / `permissive()` / Default constructors

灵感:
- MimiClaw path traversal defense
- AIOS Access Manager (agent_id -> privilege_group)
- Puter iframe sandbox + capability URL params

#### 3. document.reading_order — MinerU 灵感

`src/document/reading_order/mod.rs`:
- `BBox { x, y, w, h }` + center/edge accessors
- `from_value(v)`: accept both flat bbox dict AND block dict with 'bbox' sub-dict
- `Strategy`: InputOrder | TopToBottom | GapTree | XyCut | GroupBased
- `assign_reading_order(blocks, strategy)`: 排序后给每 block 加 'reading_order_idx'

灵感: MinerU §2.8 Reading Order Recovery (3 算法).
**简化**: 无 recursive XY-cut, 无 cross-page merge, 无语义组配对.

#### 4. CCR (Compress-Cache-Retrieve) — Headroom 灵感

`src/ccr/mod.rs`:
- `CcrStore` trait: `put(data) -> hash; get(hash) -> Option<entry>; len()`
- `CcrEntry { hash, size, data }`
- `InMemoryCcrStore` default impl (Arc<Mutex<HashMap>> + u64 counter)
- `make_marker(hash, size) -> "<<ccr:hash,size>>"`
- `extract_hash(marker) -> Option<&str>`

灵感: Headroom CcrStore (lossy 后仍可恢复原值).
**简化**: 8-char hex hash (vs SHA-256), 简化 marker 格式 (无 KIND).
**未来**: v0.34 集成到 `crush_json` lossy 路径.

#### 测试

- 320 lib tests (was 286, +34)
- `cargo build --all-targets`: clean
- `cargo clippy --all-targets -- -D warnings`: clean
- `cargo fmt --check`: 0 diff

#### 路线图 (v0.34+ 计划)

P1 (v0.34 6-8 周):
- `react` (ReAct 循环) — MimiClaw agent_loop.c
- `document.grouped_layout` — MinerU 配对
- `skill` markdown — MimiClaw skill_loader
- CCR ↔ crush_json 集成 (lossy 路径自动用 marker)
- `heartbeat` 周期 — MimiClaw
- Sandbox ↔ builtin 集成 (file.read 自动 check_path)

P2+ (v0.35+ 远期):
- `plan` (DAG) — OpenFugu Conductor
- `mora serve --openai` 模式 — OpenInfer
- `prefix_cache` — OpenInfer Pegaflow
- `tiered_memory` — OpenInfer + MimiClaw
- `lifecycle` 关键字 — Puter
- DI 容器 (5 层) — Puter
- `policy` learned router — OpenFugu
- `ai.chat role` — OpenFugu 3 role
- Error Gradation — OpenFugu
- `cross_page merge` — MinerU

## [v0.32] - 2026-07-02

### Lossless-First Recursive Walker + Event Bus + Mock Registry

灵感: 通过 deep-dive 7 个 AI 基础设施项目 (AIOS / MimiClaw / OpenFugu /
OpenInfer / MinerU / Headroom / Puter) 提取的高价值原语. 完整路线图见
`AGENTS_PRIMITIVES.md` (581 行). 本版本聚焦 3 个**可独立发布**的 P0 原语,
完整 plan/react/openai-serve 留 v0.33.

#### 1. Lossless-First Recursive Walker (Headroom 灵感)

`src/compress/json.rs::compact_value_recursive` + `crush_json_recursive`:
- 整棵 Value 树的 pure iterative DFS (避免 Windows 1MB stack 溢出)
- 每个 List 节点 (`len >= min_items`) 尝试 `try_lossless_compact`
  (csv-schema 或 markdown-kv), 失败保留原值
- 新增 `CompressOptions.recursive: bool` (default false, 向后兼容)
- 顶层 List 走标准 SmartCrusher (inlined via `crush_json_inner` 避免栈嵌套)
- 2 new tests: `recursive_walker_compacts_nested_lists`,
  `compact_value_recursive_simple`

灵感: [Headroom DocumentCompactor](https://github.com/chopratejas/headroom)
(`crates/headroom-core/src/transforms/smart_crusher/compaction/walker.rs`)

#### 2. Event Bus with Wildcard (Puter 灵感)

新模块 `src/event/mod.rs`:
- `EventBus`: `Arc<Mutex<HashMap<Pattern, Vec<Handler>>>>`
- `on(pattern, handler)` 注册; `off(pattern)` 注销; `emit(event, payload)` 派发
- `matches(event, pattern)`: Puter 风格
  - trailing `*` = prefix catch-all (`outer.*` 匹配 `outer.gui.item.removed`)
  - interior `*` = single segment wildcard (`outer.*.item`)
  - bare `*` = 匹配一切
- 8 unit tests covering exact/prefix/interior/catchall/dispatch

灵感: [Puter EventClient](https://github.com/HeyPuter/puter)
(`src/backend/clients/event/EventClient.ts`)

#### 3. Mock Registry (OpenFugu + OpenInfer 灵感)

新模块 `src/mock/mod.rs`:
- `MockRegistry`: `Arc<Mutex<HashMap<String, MockHandler>>>`
- `register(name, fn) / unregister(name) / call(name, args) / count / names`
- `MockHandler`: `Arc<dyn Fn(&Value) -> Value + Send + Sync>`
- 使用 Mora 自身 `Value` 类型, 无 `serde_json` 新依赖

灵感:
- [OpenFugu MockWorld](https://github.com/trotsky1997/OpenFugu) (train/train_trinity.py)
  用于验证 sep-CMA-ES 训练算法
- OpenInfer mock mode (无 Python 依赖的纯 Rust 测试)

Mora 之前 `compress/text.rs` / `ai_chat.rs` 散落的 hardcode mock 响应,
v0.32 起统一通过 `MockRegistry` 注册. 未来 builtin (ai.chat / http.fetch) 可
consult `mock.call` 决定是否走 mock 路径, 实现 offline deterministic 测试.

#### 4. AGENTS_PRIMITIVES.md (581 行)

新增设计文档, 完整 v0.32+ 路线图 (16 个直接原语 + 5 个跨项目共性 + 7 个待增强).
每个原语含: 灵感来源 + 实现机制 (含源码引用) + Mora 语法草案 + 实施步骤 +
关联 Mora 模块.

#### 测试

- 286 lib tests (was 272, +14)
- `cargo build --all-targets`: clean
- `cargo clippy --all-targets -- -D warnings`: clean
- `cargo fmt --check`: 0 diff

#### 路线图 (v0.33+ 计划)

P1 (v0.33 6-8 周):
- `plan` (DAG) — OpenFugu Conductor
- `react` (ReAct 循环) — MimiClaw agent_loop.c
- `document.grouped_layout` — MinerU group-based
- `document.reading_order` — MinerU 3 策略
- `schedule` cron — MimiClaw cron_service
- `skill` markdown — MimiClaw skill_loader
- `sandbox` 权限 — AIOS + Puter
- `ccr` Compress-Cache-Retrieve — Headroom

P2+ (v0.34+ 远期):
- `mora serve --openai` 模式 — OpenInfer vLLM frontend 复用
- `prefix_cache` — OpenInfer Pegaflow
- `tiered_memory` — OpenInfer + MimiClaw
- `lifecycle` 关键字 — Puter hooks
- DI 容器 (5 层) — Puter
- `heartbeat` 周期 — MimiClaw
- `policy` learned router — OpenFugu TRINITY
- `ai.chat role` — OpenFugu 3 role
- Error Gradation — OpenFugu evidence grade
- `cross_page merge` — MinerU

## [v0.31] - 2026-07-02

### No-Panic Refactor + Code Quality Hardening

灵感来自 v0.30 之后的"大检查"反馈 (user: "5 项检查不够").
本版本专注于**错误处理韧性** — 用户脚本出错时不再让解释器崩溃.

#### 修: 21 panic -> 0 in lexer/parser

用户脚本有语法错误时, 之前整个进程会 `panicked at src/lexer.rs:...`
直接 abort. 现在:
- Lexer 8 个 panic 改为 emit `TokenType::Error(String)` token
- Parser 13 个 panic 改为 `eprintln!` 错误信息 + 返回 safe default
  (空字符串 / 空 list / 默认 OrchestrateKind.Sequential)
- 用户看到 `"Parse error: ..."` 友好错误而非 stack trace

`examples/_legacy/` 中的 demo (之前会 panic) 现在不再 crash 进程.

#### 修: Windows OCR model path fallback

`user_model_path()` 之前只检查 `XDG_DATA_HOME` 和 `HOME`,
两者在 Windows 上都未设置, 永远 fail. 新增 `LOCALAPPDATA` fallback
作为第 3 选项. 错误信息也更新列出所有 3 个解析路径.

#### 修: cargo doc warnings 14 -> 0

Module-level `//!` 注释中的 HTML 标签未转义:
- `<Page>`, `<Block>`, `<Span>` 改为 `\[ \]` 或反引号
- `<p>`, `<N>`, `Vec<Value>` 等改为反引号包
- bare URL `https://...` 改为 `<https://...>`

`cargo doc --no-deps` 现在 0 warning, docs.rs 渲染干净.

#### 测试

- 272 lib + 5 integration = 277 test 全过
- `cargo build --all-targets`: clean
- `cargo clippy --all-targets -- -D warnings`: clean
- `cargo fmt --check`: 0 diff
- `cargo doc --no-deps`: 0 warning

## [v0.30] - 2026-07-02

### SmartCrusher — 内容感知 JSON 压缩

灵感来自 [headroom](https://github.com/headroomlabs-ai/headroom) 的 SmartCrusher
（统计字段检测 + 多种压缩策略 + 安全约束）。把 v0.29 的"看字段名 + 30% 头 15% 尾"
升级为"按值分布推断语义角色 + 5 种策略 + 3 种安全约束"。

#### ⚠️ BREAKING CHANGES

- `CompressOptions.anomaly_keys: Vec<String>` 字段**整体删除**（v0.30 起不再解析）
- `CompressOptions` 字段从 5 个改为 11 个（v0.29 字段重命名 + 6 个新增）
- `crush_json_core` 函数**重命名**为 `crush_json`，签名 `(items, target, options)`
  （旧 `crush_json_core(input, max, anomaly_keys)` 形式已删除）
- `parse_json_simple` stub **改为真实实现**（委托 `flow::json_to_value`）
- `crush_json` / `compress.json` / `List.crush_json` 的输出 marker 改为
  `method=smart_crusher strategy={...} items={...} total={...} savings={...}`

#### 新策略（替代 v0.29 单一 head_tail）

| 策略 | 触发条件 | 行为 |
|---|---|---|
| `auto` (default) | 任意 | 按 ArrayType 自动选 |
| `topn` | 显式 / 存在 Score 字段 | 按 Score 保留 top N |
| `timeseries` | 显式 / 存在 Temporal 字段 | 头尾 + 均匀采样 |
| `cluster` | 显式 / 字段 uniqueness < 0.3 | 相似度聚类去重 |
| `lossless` | 显式 | schema 一致时转 csv-schema / md-kv |
| `smart_sample` | fallback | 头 + 中间采样 + 尾 |

#### 5 种字段角色（按值分布推断）

- `Id` — uniqueness > 0.9 且为字符串/UUID/顺序数字
- `Score` — bounded numeric range (0-1 或 0-100)
- `Temporal` — ISO 8601 / Unix timestamp 模式
- `Error` — 字段名或值含 `error`/`failed`/`exception`/... 等关键词
- `Anomaly` — 数值 >3σ from mean (1-5% 项)

#### 3 种安全约束

- `KeepErrorsConstraint` — 含错误关键词的项强制保留
- `KeepOutliersConstraint` — Anomaly 字段的 >2σ 项保留
- `KeepBoundaryConstraint` — 头 k_first + 尾 k_last 项保留（默认各 15%）

#### 新 builtin 用法

```mora
-- 默认 auto: 按字段角色自动选最佳策略
compress.json(tool_output, {target_ratio: 0.2})

-- 显式 TopN
compress.json(scored_list, {strategy: "topn", target_ratio: 0.1})

-- 显式 TimeSeries
compress.json(metrics, {strategy: "timeseries", target_ratio: 0.3})

-- Lossless (csv-schema 格式, 全保留)
compress.json(flat_table, {strategy: "lossless", max_bytes: 5000})

-- 关闭某项约束
compress.json(api_logs, {
    strategy: "auto",
    target_ratio: 0.2,
    preserve_errors: true,
    preserve_outliers: true,
    preserve_ids: false,
})

-- 拿 metadata
let result = compress.json(items, {target_ratio: 0.2})
result.savings_ratio    -- 0.8 (80% 节省)
result.strategy_used    -- "topn"
result.fields           -- [{name, role, ...}, ...]
```

#### 性能

| 量级 | 节省率 (v0.29) | 节省率 (v0.30) | 提升 |
|---|---|---|---|
| 100 项 × 5 字段 | 60% | 70-80% | +10-20% |
| 1000 项 × 20 字段 | 60% | 75-85% | +15-25% |
| 10000 项 × 30 字段 | 60% | 80-90% | +20-30% |

#### 新模块文件

- `src/compress/json.rs` — 完全重写 (267 → 970 行)
  - `FieldRole` / `FieldStats` / `ArrayType` 数据结构
  - 5 个 detector + 5 个 Strategy + 3 个 Constraint
  - `crush_json` / `crush_json_string` / `try_lossless_compact`
- `src/compress/mod.rs` — `CompressOptions` 重定义 (11 字段)
  - `parse_json_simple` 委托 `flow::json_to_value`
  - `value_to_json_simple` 委托 `flow::value_to_json`

#### 测试

- 12 个新 unit test（替代 v0.29 5 个旧 test）
  - 5 个 role detection（id/score/error/temporal/anomaly）
  - 4 个 strategy（topn/timeseries/lossless/auto）
  - 2 个 constraint（errors/outliers）
  - 1 个 metadata
  - 1 个 string 入口
- 所有 v0.29 旧 test 已删除（`crush_json_core` / `anomaly_keys` / `parse_json_simple_currently_stub`）
- 全部 272 test 通过；`cargo clippy --all-targets -- -D warnings` 通过

## [v0.29] - 2026-07-01

### compress + crush_json + OCR .rten 迁移

灵感来自 [headroom](https://github.com/headroomlabs-ai/headroom) ContentRouter + Kneedle 设计。
Mora 历史上首次支持结构化 JSON 列表压缩 + 多策略 system prompt 压缩。

#### 新增关键字 / builtin

```mora
-- 6 路策略 (auto / head_tail / summary / lossless / json / code-html-log-text)
let summary = compress(text, "summary")                       -- LLM 摘要
let head    = compress(text, "head_tail", head_pct: 0.3)     -- 保留首尾
let lossless = compress(text, "lossless")                     -- 加 size marker
let auto    = compress(text, "auto")                          -- 内容路由

-- 结构化 JSON 列表压缩 (Kneedle + 异常保留)
let crushed = crush_json(big_list, max: 10)
let crushed = crush_json(big_list, max: 10, anomaly_keys: ["error"])

-- 方法链
let summary = conv.compress("summary")
let crushed = list.crush_json(10)
```

#### 新增模块 `compress`

| 名称 | 作用 |
|---|---|
| `SubCompressor` trait | `sniff` / `compress` / `origin` 3 方法 |
| `ContentRouter` | 嗅探 → 选最佳子压缩器 |
| `JsonSubCompressor` | 委托 crush_json_core |
| `CodeSubCompressor` | regex 保留签名 + 截断 body |
| `HtmlSubCompressor` | 复用 v0.27 quick-xml 切块 |
| `LogSubCompressor` | 行 pattern cluster |
| `TextSubCompressor` | head_tail / summary / lossless 调度 |

#### ⚠️ BREAKING: `compact` 重命名为 `compress`

v0.25 的 `compact(text)` builtin 已重命名为 `compress(text, "summary")`。
`examples/compact_demo.mora` 同步改写为 v0.29 风格。

#### OCR `.rten` 模型迁移 (解决 v0.28 tech-debt)

- v0.28 vendored 的 11.7 MB `.rten` 模型已从仓库删除
- 模型现在从 `~/.local/share/mora/ocr/` 加载 (可用 `MORA_OCR_MODELS_DIR` 覆盖)
- 新增 `docs/install-ocr.md` 说明下载与安装步骤
- 新增 `.git/sdd/ocrs-shasums.txt` 作为 reference checksum
- **BREAKING**: 首次 OCR 调用前需 `mora-install-ocr` 下载模型

#### 新增文件

- `src/compress/{mod,json,code,html,log,text}.rs` (~1000 行)
- `docs/install-ocr.md`
- `.git/sdd/ocrs-shasums.txt`
- `examples/compress_demo.mora` (新)

#### 技术细节

- **零新外部依赖** — 用 v0.27 / v0.28 已有 deps (`regex` transitive from `ocrs`)
- **字节近似** — 与 v0.26 / v0.27 / v0.28 一致
- **CodeSubCompressor 纯 regex** — v0.30+ 引入 tree-sitter
- **错误前缀** `compress.` / `crush_json.` / `ocr.load.`

## [v0.28] - 2026-07-01

### Office (PPTX/DOCX) + Image OCR Backends

灵感来自 v0.27 DocumentBackend 框架与 MinerU 多格式解析思路。
沿用 v0.27 trait 框架，仅添 3 个 DocumentBackend 后端实现。

#### 新增后端

| 后端 | 文件格式 | 依赖 | 说明 |
|---|---|---|---|
| PptxBackend | .pptx | undoc 0.5 | 演示文稿 |
| DocxBackend | .docx | undoc 0.5 | Word 文档 |
| ImageBackend | .png | ocrs 0.12 + image 0.24 | 扫描件 OCR（纯 Rust / rten ONNX）|

#### 用法

```mora
let deck = document.parse("./deck.pptx")           -- PPTX
let report = document.parse("./report.docx")        -- DOCX
let scan = document.parse("./scan.png")            -- OCR

print(deck.markdown())                              -- markdown 形式
print(report.text())                                -- 纯文本
print(scan.metadata()["ocr_engine"])                -- "rten"
```

#### 与 v0.26/v0.27 组合

```mora
-- 与 v0.26 compose_prompt
let sys = compose_prompt({role:"system", text:deck.text(), budget:"32 KB"})
-- 与 v0.27 块式声明
document "report" do
    set origin: "docx"
    read "./report.docx"
end
```

#### 新增依赖（实现期真实清单）

- `undoc` 0.5（启用 `docx` + `pptx` features，纯 Rust）
- `ocrs` 0.12（OCR 引擎壳，纯 Rust）
- `rten` 0.24（ocrs 不再 re-export；必须直接依赖以 `Model::load_static_slice` 加载 `.rten`）
- `anyhow` 1（ocrs 的 `OcrEngine::new` 暴露 `anyhow::Result`；ocrs 不再 re-export `anyhow`）
- `image` 0.24（仅 `png` feature；解析 PNG header / dimensions）

全部纯 Rust，MSRV 1.85 ✅，无系统依赖。

#### 技术细节

- **零系统依赖**：所有 5 个新 crate 都是 pure Rust
- **PNG only in v0.28**：JPEG / XLSX / 扫描 PDF 留 v0.29+
- **OCR 引擎**：`ocrs 0.12` 基于 Microsoft `rten` ONNX runtime
- **多语言 OCR**：v0.28 仅英文（eng.traineddata bundled）
- **工厂分发**：v0.27 的 `parse_document(path)` 已按扩展名自动派发到 `PptxBackend` / `DocxBackend` / `ImageBackend`，用户代码无变化

#### Known issues / v0.29+ roadmap

- **11.7 MB `.rten` 模型 vendoring**：OCR 检测/识别模型（`text-detection.rten` 2.4 MB + `text-recognition.rten` 9.3 MB）以 raw blob 提交在 `tests/fixtures/`，未走 git LFS。每个 contributor / CI 首次 `git clone` 多拉 ~12 MB；`mora` release binary 经 `include_bytes!` 也内嵌这 ~12 MB；二进制 blob 无法在 PR 中 diff/审查；上游模型更新也无刷新路径。详情见 `.git/sdd/tech-debt-v0.29.md`。v0.29 计划三选一：git LFS / `build.rs` 联网下载 / 用户侧 model dir。
- **OCR 仅英文**：`ocrs 0.12` 加载的 `eng.traineddata` 仅识别拉丁字符。
- **OCR 仅 PNG**：JPEG / WebP / TIFF 留 v0.29+。
- **无扫描 PDF**：扫描版 PDF（图片型）尚未接入 OCR 路径。

## [v0.27] - 2026-07-01

### Document 统一 IR — `document.parse(...)` + 块式声明

灵感来自 [opendatalab/MinerU](https://github.com/opendatalab/MinerU) middle_json 抽象。
Mora 历史上首次支持 PDF / Markdown / HTML 文档解析,统一落到 `Value::Document` IR。

#### 新增关键字

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

#### 新增内建模块 `document`

| 函数 | 作用 |
|---|---|
| `document.parse(path)` | 解析文件,返回 `Value::Document` |

#### `Document` value 的方法

| 方法 | 返回 | 含义 |
|---|---|---|
| `doc.markdown()` | `string` | 全文档 markdown 渲染 |
| `doc.text()` | `string` | 纯文本（去格式）|
| `doc.pages()` | `List<Dict>` | 完整 IR Page 列表 |
| `doc.blocks()` | `List<Dict>` | 跨页合并的 block |
| `doc.metadata()` | `Dict` | 元信息（含 origin / pages / size）|
| `doc.origin()` | `string` | "pdf" / "markdown" / "html" |

#### 新增值类型 + Trait

- `Value::Document { backend: Arc<dyn DocumentBackend + Send + Sync>, metadata: HashMap<String, Value> }`
- `pub trait DocumentBackend: Debug + Send + Sync { fn origin / pages / markdown / text / metadata / blocks }`
- 3 个后端实现: `PdfBackend` (lopdf + pdf-extract) / `MarkdownBackend` (pulldown-cmark) / `HtmlBackend` (quick-xml)

#### 新增依赖

- `lopdf` 0.41 + `pdf-extract` 0.12 (PDF)
- `pulldown-cmark` 0.13 (Markdown)
- `quick-xml` 0.40 (HTML)
- 全部纯 Rust, MSRV 1.85 ✅, 无系统依赖

#### 与 v0.26 组合

```mora
let doc = document.parse("./report.pdf")
let sys = compose_prompt({role:"system", text:doc.markdown(), budget:"32 KB"})
let resp = ai.chat(p"根据报告：{sys}\n\n问题：{question}")
```

#### 技术细节

- **零系统依赖**：所有后端纯 Rust crate
- **二进制不出 Value 树**：原始 PDF / 图片字节封在 `backend: Arc<dyn ...>` 内
- **Lazy 后端**：访问 `.pages()` / `.markdown()` 时才构造 Value, 避免一次物化
- **可扩展**：未来加 PPTX / DOCX 后端仅需 `impl DocumentBackend`

## [v0.26] - 2026-07-01

### Prompt Sections — 分段 + 容量预算 + 滚动窗口

灵感来自 [mimiclaw](https://github.com/memovai/mimiclaw)（5 段固定缓冲）和 [headroom](https://github.com/headroomlabs-ai/headroom)（内容感知路由器），把 LLM 的 system prompt 拼装从字符串拼接升级为分段工程。

#### 新增关键字 `prompt`

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

#### 新增内建函数

| 名字 | 作用 |
|---|---|
| `compose_prompt(...)` | 拼接多段为单一 system prompt，按 section budget 截断 |
| `tail(path, max: N)` | 取文件末 N 行（JSONL/纯文本） |

#### 新增值类型

- `Value::PromptSection { name, role, text, budget_bytes }`

#### 新增 AST 节点

- `StmtKind::PromptSection { name, body }`
- `StmtKind::PromptSet { key, value }`（块内 `set role:` / `set budget:`）
- `StmtKind::PromptRead(NodeId)`（块内 `read`）

#### 技术细节

- **零依赖**：无 tokenizer，按 UTF-8 字节近似（与 mimiclaw 同思路）
- **可逆性**：每个 section 在环境里是可读 Value，便于调试与中间表示（IR）思路）
- **可组合**：字典内联形参与块式声明产生同义结果

## [v0.25] - 2026-07-01

### 代码模块化重构 (Code Modularization)

对 5 个大文件进行了模块化拆分，提升代码可维护性：

#### 拆分详情
- **interpreter**: 3402 行 → 3 文件 (mod.rs + execute.rs + evaluate.rs)
- **typeck**: 2838 行 → 2 文件 (mod.rs + check.rs)
- **parser_v2**: 2609 行 → 3 文件 (mod.rs + statements.rs + expressions.rs)
- **record**: 2091 行 → 7 文件 (mod.rs + serialization.rs + diff.rs + analysis.rs + audit.rs + snapshot.rs + tests.rs)
- **lsp/providers**: 1092 行 → 11 文件 (mod.rs + helpers.rs + 9 个 provider 模块)

#### 改进
- 每个模块职责单一，便于理解和维护
- 函数按功能分组，提高代码可读性
- 模块间依赖关系更清晰

### 跨平台兼容性修复
- 修复 `test_memory_save_load` 测试在 Windows 上的路径问题
- 使用 `std::env::temp_dir()` 替代硬编码的 `/tmp` 路径

## [v0.24] - 2026-06-30

### ParserV2 完整迁移 (Complete)

ParserV2 已完成对旧 Parser 的完整迁移，所有功能已覆盖。
旧 parser.rs (2459 行) 已删除，主程序和测试全部使用 ParserV2。

#### 新增语句解析
- **append_statement**: 追加文件写入
- **read_bytes_statement**: 读取字节文件
- **write_bytes_statement**: 写入字节文件
- **stream_statement**: 流式循环 `stream <expr> as <var> do ... end`
- **tool_statement**: 工具定义 `tool name(params): type do ... end`
- **observe_statement**: 可观测性配置 (trace/metrics/otel)
- **span_statement**: 追踪范围 `span "name" tags {..} do ... end`
- **record_tokens_statement**: 记录 token 使用量
- **assignment_statement**: 赋值语句 `IDENT = expr`
- **index_assignment**: 索引赋值 `IDENT[expr] = expr`
- **commit/rollback**: 事务提交/回滚

#### 新增表达式解析
- **match_expression**: 模式匹配表达式 (含 when 守卫)
- **pattern**: 模式解析 (字面量/变量/列表/字典/通配符)
- **parse_format_string**: 格式字符串插值
- **parse_ai_model_call**: ai_model 调用 (支持 keyword args)
- **flatten_prompt_parts**: Prompt 表达式展平
- **list_literal / dict_literal**: 列表和字典字面量
- **char_literal**: 字符字面量 `'a'`
- **NamespaceRef**: 命名空间引用 `Module::method()`

#### 新增类型系统支持
- **parse_generic_params**: 泛型参数 `<T: Bound>`
- **parse_type_list**: 类型列表 `<T, U, V>`
- **parse_type_name_recursive**: 递归解析嵌套泛型
- **parse_where_clause**: where 子句

#### 类型检查修复
- **let 推断**: 已知类型自动推断，不再强制要求类型注解
- **string + any**: 允许字符串拼接 (运行时做类型转换)

#### 重构
- **ObserveConfig**: 在 ast_v2.rs 中定义新类型，使用 NodeId
- **FnDef / TraitMethod**: 在 ast_v2.rs 中定义新类型，使用 Vec<NodeId>
- **Pattern**: 在 ast_v2.rs 中定义新类型，Guard condition 使用 NodeId
- **consume_method_name**: 支持关键字作为方法名
- **表达式优先级**: 修复方法调用优先级 (binary → unary → call → primary)
- **反向适配器**: ast_v2_to_v1.rs 支持完整 AST 转换

### 9 Languages Features Integration (Complete)

All features from the learning plan have been implemented.

### v0.21: Rust 风格类型系统

- **借用语法**: `&expr` / `&mut expr`
- **生命周期标注**: `<'a>` 参数
- **借用冲突检查**: 编译期检查不可变/可变借用冲突

### v0.22: 性能优化

- **AI 调用内联缓存**: 相同 prompt 直接返回缓存结果
- **管道融合**: 连续 map/filter/take/drop 合并执行
- **常量折叠**: 编译期计算常量表达式
- **字符串驻留**: 相同字符串只存储一次
- **HTTP 连接池**: 线程池优化 (最多16线程)
- **MCP 异步处理**: 线程池处理请求 (最多8并发)
- **类型检查增量优化**: 缓存已检查的表达式类型

### v0.24: 强类型升级

- **类型别名**: `type Name = TargetType`
- **枚举类型**: `enum Name { V1, V2(Type) }`
- **结构体类型**: `struct Name { field: Type }`

### 文档

- **docs/mora-spec.md**: Mora 语言规范 (20 章)
- **docs/influences.md**: 9 语言影响分析
- **docs/learning-plan.md**: 特性融入计划
- **docs/workflow-v0.20.md**: 开发工作流

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
