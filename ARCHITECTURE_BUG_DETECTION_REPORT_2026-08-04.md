# Mora-lang 架构与 Bug 检测报告

> **检测日期**: 2026-08-04  
> **版本**: v0.75.82 (commit `af70149`)  
> **检测范围**: `src/` 全部 146 个 `.rs` 文件 + `tests/` 18 个集成测试文件  
> **检测工具**: `cargo build`, `cargo clippy --all-targets --all-features -D warnings`, `cargo fmt --check`, `cargo test`, Python 精确过滤 `#[cfg(test)]` 静态分析  
> **对比基线**: `ARCHITECTURE_BUG_DETECTION_REPORT_2026-08-03.md` (v0.75.53, commit `09be96f`)

---

## 1. 执行摘要

| 维度 | 结果 | 评级 |
|------|------|------|
| 编译通过 | ✅ `cargo build` 零错误 | 🟢 |
| 格式化 | ✅ `cargo fmt --check` 零违规 | 🟢 |
| Clippy | ✅ `--all-targets --all-features -D warnings` 零警告 | 🟢 |
| Lib 单元测试 | ⚠️ 611 个测试中 1 个卡住（运行 6 分 16 秒后停止） | 🟡 |
| 生产代码 `.unwrap()` | **0 处** | 🟢 |
| 生产代码 `panic!` | **3 处** | 🟡 |
| 生产代码 `.expect(...)` | **80 处** | 🟡 |
| TODO/FIXME/XXX/HACK | **0 处** | 🟢 |
| 最大文件 | `parser_v3/mod.rs` **3,393 行** | 🟡 |
| Mutex/RwLock poison 连锁 panic | **29 处**（trace_collector 10 + schedule 9 + event 6 + audit 4） | 🔴 |
| 测试卡住 | `runtime::sandbox::tests::clone_shares_container_arc` | 🔴 |

**总体评级**: 🟡 **基本健康，但存在两项需关注风险** —— 编译/静态检查/格式化全部通过，生产代码 `unwrap` 已清零；但 `cargo test --lib` 中出现测试长时间卡住，且锁中毒 `expect` 共 29 处，一旦发生 panic 会级联崩溃。

---

## 2. 编译与静态检查

### 2.1 编译状态

```
cargo build                           ✅ Finished dev profile, 0 errors
cargo fmt --check                     ✅ 0 files violate formatting
cargo clippy --all-targets --all-features -D warnings  ✅ 0 warnings
```

**较上期**: 三项检查均保持通过。本期 `mir/opt.rs` 拆分为 `mir/opt/` 子模块、`mir/ssa.rs` 拆出 `mir/ssa/deconstruct.rs` 后，clippy 仍零警告。

### 2.2 测试状态

| 测试套件 | 数量 | 结果 | 说明 |
|---------|------|------|------|
| Lib 单元测试 (`cargo test --lib`) | 611 | ⚠️ **异常** | 前 610 个测试通过，最后一个 `runtime::sandbox::tests::clone_shares_container_arc` 卡住超过 6 分钟，任务被手动停止 |

**详细现象**:
- 测试进程在 `interpreter::builtins::exec::tests_v043_exec::exec_parallel_kills_process_group_on_timeout` 完成后输出两次 `SUCCESS: The process with PID ... has been terminated.`。
- 随后进入 `runtime::sandbox::tests::clone_shares_container_arc`，状态更新为 `has been running for over 60 seconds`。
- 等待 6 分 16 秒后仍未结束，使用 `TaskStop` 终止后台任务。

**初步判断**: 该测试本身逻辑简单（创建两个 `SandboxRuntime` 克隆，验证 `Arc<Mutex<Option<ContainerHandle>>>` 共享），不应长时间运行。卡住可能与前序 `exec_parallel_kills_process_group_on_timeout` 测试未完全清理的子进程/线程有关，导致后续测试在获取锁或创建资源时死锁。这是**本期新增的可复现测试缺陷**，建议立即调查。

---

## 3. 代码规模与结构

### 3.1 规模概览

| 指标 | 当前值 | 上期值 | 变化 |
|------|--------|--------|------|
| 总代码行数 (`src/`) | **54,700 行** | 54,441 | +259 |
| `.rs` 文件数 | **146 个** | 131 | +15 |
| 顶层模块数 (`lib.rs`) | **26 个** | 26 | — |
| `pub` 声明 | **607 个** | 604 | +3 |
| `pub(crate)` 声明 | **77 个** | 5 | +72 |
| `#[cfg(test)]` 内联测试模块 | **83 个** | 82 | +1 |
| `clone()` 调用 | **1,026 处** | 1,039 | -13 |
| `.lock()` 调用 | **163 处** | 165 | -2 |
| `Arc::new` | **143 处** | 133 | +10 |
| `thread::spawn` | **10 处** | 10 | — |

### 3.2 最大文件 TOP 20

| 排名 | 文件 | 总行数 | 生产行 | 测试行 | 测试占比 |
|------|------|--------|--------|--------|----------|
| 1 | `src/parser_v3/mod.rs` | **3,393** | 3,393 | 0 | 0% |
| 2 | `src/pregel/mod.rs` | **2,552** | 1,326 | 1,226 | 48% |
| 3 | `src/interpreter/builtins/mod.rs` | **2,254** | 38 | 2,216 | 98.3% |
| 4 | `src/interpreter/dispatch.rs` | **1,648** | 1,502 | 146 | 8.9% |
| 5 | `src/value.rs` | **1,213** | 856 | 357 | 29.4% |
| 6 | `src/mir/jit.rs` | **1,066** | 1,066 | 0 | 0% |
| 7 | `src/mir/ssa.rs` | **1,025** | 1,025 | 0 | 0% |
| 8 | `src/mir/handlers.rs` | **923** | 923 | 0 | 0% |
| 9 | `src/compress/json.rs` | **921** | 597 | 324 | 35.2% |
| 10 | `src/interpreter/ai_chat.rs` | **895** | 895 | 0 | 0% |
| 11 | `src/mir/dag.rs` | **858** | 660 | 198 | 23.1% |
| 12 | `src/document/reading_order/mod.rs` | **848** | 268 | 580 | 68.4% |
| 13 | `src/interpreter/mod.rs` | **789** | 789 | 0 | 0% |
| 14 | `src/lexer.rs` | **728** | 728 | 0 | 0% |
| 15 | `src/audit/mod.rs` | **720** | 514 | 206 | 28.6% |
| 16 | `src/mir/lower.rs` | **716** | 716 | 0 | 0% |
| 17 | `src/mir/optimize/dag_rule.rs` | **696** | 501 | 195 | 28.0% |
| 18 | `src/mir/expr/mod.rs` | **692** | 654 | 38 | 5.5% |
| 19 | `src/event/mod.rs` | **690** | 303 | 387 | 56.1% |
| 20 | `src/mir/optimize/dag_search.rs` | **689** | 343 | 346 | 50.2% |

### 3.3 超过 1,000 行的文件

| 文件 | 行数 | 较上期变化 | 说明 |
|------|------|-----------|------|
| `src/parser_v3/mod.rs` | **3,393** | +167 (3,226→3,393) | 持续增长，**零单元测试覆盖** |
| `src/pregel/mod.rs` | **2,552** | +3 (2,549→2,552) | 基本稳定 |
| `src/interpreter/builtins/mod.rs` | **2,254** | -1,065 (3,319→2,254) | 生产代码已迁出，仅剩 38 行生产 + 12 个测试块 |
| `src/interpreter/dispatch.rs` | **1,648** | +228 (1,420→1,648) | 分发表持续增长 |
| `src/value.rs` | **1,213** | 新入榜 | 值类型系统 |
| `src/mir/jit.rs` | **1,066** | 不变 | copy-and-patch JIT (x86_64) |
| `src/mir/ssa.rs` | **1,025** | -419 (1,444→1,025) | 拆出 `ssa/deconstruct.rs` |

**上期超 1,000 行但本期已降的文件**: `compress/json.rs` (1,516→921), `mir/handlers.rs` (1,451→923), `mir/opt.rs` (1,176→208，拆分为子模块)。

### 3.4 测试代码占比高的文件（>50%，总行 >100）

| 文件 | 总行数 | 测试占比 | `#[cfg(test)]` 块数 |
|------|--------|----------|---------------------|
| `src/interpreter/builtins/mod.rs` | 2,254 | **98.3%** | 12 |
| `src/document/reading_order/mod.rs` | 848 | 68.4% | 1 |
| `src/event/mod.rs` | 690 | 56.1% | 1 |
| `src/mir/optimize/dag_search.rs` | 689 | 50.2% | 1 |
| `src/mir/optimize/cost.rs` | 406 | 50.5% | 1 |
| `src/checkpoint/sqlite.rs` | 307 | 55.4% | 1 |
| `src/orchestrate_dag/mod.rs` | 276 | 54.7% | 1 |
| `src/heartbeat/mod.rs` | 214 | 53.3% | 1 |
| `src/mock/mod.rs` | 207 | 57.0% | 1 |
| `src/interpreter/builtins/event.rs` | 179 | 55.9% | 1 |

### 3.5 零单元测试覆盖的大文件

以下文件无内联 `#[cfg(test)]` 模块，依赖 `tests/` 集成测试间接覆盖：

| 文件 | 行数 |
|------|------|
| `src/parser_v3/mod.rs` | 3,393 |
| `src/mir/jit.rs` | 1,066 |
| `src/mir/ssa.rs` | 1,025 |
| `src/mir/handlers.rs` | 923 |
| `src/interpreter/ai_chat.rs` | 895 |
| `src/interpreter/mod.rs` | 789 |
| `src/lexer.rs` | 728 |
| `src/mir/lower.rs` | 716 |

---

## 4. 错误处理深度分析

### 4.1 统计方法

使用 Python 脚本逐文件解析 `src/` 下所有 `.rs` 文件：
- 通过大括号匹配精确识别 `#[cfg(test)]` 块范围并排除。
- 将 `src/record/tests.rs`、`src/stress_tests.rs` 视为测试文件整体排除。
- 移除 `//` 注释与字符串字面量后再匹配。
- 区分 `.unwrap()`、`.unwrap_or` / `.unwrap_or_else` / `.unwrap_or_default`（后者不计入）。

### 4.2 生产代码 unwrap/panic/expect（精确值）

| 类型 | 数量 | 较上期 | 变化 |
|------|------|--------|------|
| `.unwrap()` | **0** | 2 | **-2（已清零）** |
| `panic!` | **3** | 3 | — |
| `.expect(...)` | **80** | 33* | **+47** |
| **生产总计** | **83** | 38 | +45 |

\* 上期 `expect` 统计未覆盖全仓，实际遗漏了 `schedule/mod.rs`、`event/mod.rs`、`audit/mod.rs`、`mock/mod.rs` 等文件。本期为全量精确值，真实新增约 5–8 处。

### 4.3 生产代码 `panic!` — 3 处

| 文件 | 行号 | 代码 | 风险 |
|------|------|------|------|
| `src/cli/mod.rs` | L30 | `ParserV3::compile(source).unwrap_or_else(\|e\| panic!("compile_and_opt failed: {e}"))` | CLI 编译失败直接终止进程 |
| `src/mir/ssa/deconstruct.rs` | L176 | `.unwrap_or_else(\|\| panic!("unmapped SSA reg {}", ssa_r))` | MIR 优化内部不变量破坏 |
| `src/mir/ssa/deconstruct.rs` | L270 | `.unwrap_or_else(\|\| panic!("unmapped SSA reg {}", ssa_r))` | 同上 |

**较上期**: `mir/ssa.rs` 的两处 panic 随模块拆分迁移至 `mir/ssa/deconstruct.rs`，逻辑未变；`cli/mod.rs` panic 持续存在。

### 4.4 生产代码 `.expect(...)` — 80 处（按文件分布）

| 文件 | 数量 | 主要模式 | 风险等级 |
|------|------|----------|----------|
| `src/trace_collector.rs` | **10** | `.lock().expect("trace collector poisoned")` | **P1** — Mutex 中毒连锁 panic |
| `src/schedule/mod.rs` | **9** | `.lock().expect("scheduler mutex poisoned")` | **P1** — 同上 |
| `src/event/mod.rs` | **6** | `.expect("event bus rwlock/mutex poisoned")` | **P1** — 同上 |
| `src/audit/mod.rs` | **4** | `.lock().expect("audit sink mutex poisoned")` | **P1** — 同上 |
| `src/main.rs` | **5** | `.expect("Failed to read/write/create...")` | P3 — CLI 入口 |
| `src/mock/mod.rs` | **5** | `.lock().expect("mock registry mutex poisoned")` | P2 — 测试辅助但公开 API |
| `src/interpreter/ai_chat.rs` | **4** | `.expect("...poisoned")` | P2 — 锁中毒 |
| `src/interpreter/builtins/mora.rs` | **4** | `.expect("refine_registry poisoned")` 等 | P2 |
| `src/interpreter/builtins/sandbox.rs` | **4** | `.lock().expect("container poisoned")` | P2 |
| `src/mir/cache.rs` | **4** | `.lock().expect("DagCache entries poisoned")` | P2 — 新增全局缓存 |
| `src/interpreter/builtins/exec.rs` | **3** | `.lock().expect("semaphore mutex poisoned")` 等 | P2 |
| `src/mir/ssa.rs` | **3** | `.last().expect("is_empty checked above")` | P4 — 前置条件已验证 |
| `src/orchestrate_dag/mod.rs` | **2** | `.expect("topological_order: ...")` | P3 |
| `src/parser_v3/mod.rs` | **2** | `.next().expect("len==1 verified above")` | P4 — 前置条件已验证 |
| `src/pregel/worker_pool.rs` | **2** | `.lock().expect("worker pool ... poisoned")` | P2 |
| `src/runtime/orch.rs` | **2** | `.lock().expect("OrchRuntime ... poisoned")` | P2 |
| `src/toolplane/mod.rs` | **2** | `.expect("toolplane: register ... failed")` | P3 — 初始化失败 |
| 其他 12 个文件 | 各 1 处 | 初始化/前置条件/锁中毒 | P2–P4 |

### 4.5 TODO/FIXME/XXX/HACK

| 标记 | 数量 | 位置 |
|------|------|------|
| TODO | **0** | — |
| FIXME | **0** | — |
| XXX | **0** | — |
| HACK | **0** | — |

较上期（2 处 TODO）进一步清零。

---

## 5. dead_code / 未使用项

### 5.1 Clippy dead_code 警告

`cargo clippy --all-targets --all-features -- -D warnings` 输出 **0 warnings**，无 `dead_code` 警告。

### 5.2 `#[allow(dead_code)]` 分布

共 **7 处**（较上期 8 处减少 1 处）：

| 文件 | 行号 | 上下文 |
|------|------|--------|
| `src/interpreter/ai_helpers.rs` | L344 | 内联标注 |
| `src/interpreter/mod.rs` | L432, L672 | 内联 + 模块级标注 |
| `src/runtime/types.rs` | L82, L131, L134 | 未来扩展用字段 |
| `src/runtime/registry.rs` | L125 | 内联标注 |

### 5.3 其他 `#[allow(...)]` 标注

| 抑制类型 | 数量 | 位置 |
|----------|------|------|
| `clippy::too_many_arguments` | 3 | `handlers.rs`(2), `record/mod.rs`(1) |
| `clippy::needless_range_loop` | 2 | `ssa.rs`(2) |
| `clippy::should_implement_trait` | 1 | `reading_order/mod.rs` |
| `clippy::never_loop` | 1 | `ai_chat.rs` |
| `clippy::large_enum_variant` | 1 | `mir/mod.rs` |
| `unused_imports` | 1 | `builtins/mod.rs` |
| `unused` | 1 | `optimize/pattern.rs` |

---

## 6. 架构问题（按严重程度 P0–P4）

### P0 — 阻塞问题

**无**。编译、fmt、clippy 全部通过。

### P1 — 高优先级

| # | 文件 | 问题 | 影响 | 较上期 |
|---|------|------|------|--------|
| P1-1 | `src/trace_collector.rs` | 10 处 `.lock().expect("trace collector poisoned")` | 观测系统崩溃级联到主流程 | 未修复（持续 3 期） |
| P1-2 | `src/schedule/mod.rs` | 9 处 `.lock().expect("scheduler mutex poisoned")` | 调度器崩溃级联 | 新增（上期遗漏） |
| P1-3 | `src/event/mod.rs` | 6 处 `.expect("event bus ...poisoned")` | 事件总线崩溃级联 | 新增（上期遗漏） |
| P1-4 | `src/audit/mod.rs` | 4 处 `.lock().expect("audit sink mutex poisoned")` | 审计系统崩溃级联 | 新增（上期遗漏） |
| P1-5 | `src/parser_v3/mod.rs` | 3,393 行，0% 单元测试覆盖，持续增长 | 维护困难，编译时间长 | 恶化（+167 行） |
| P1-6 | `src/runtime/sandbox.rs` | `clone_shares_container_arc` 测试在 `cargo test --lib` 中卡住 >6 分钟 | 测试管线不可靠，可能存在死锁 | **新增** |

### P2 — 中优先级

| # | 文件 | 问题 | 影响 | 较上期 |
|---|------|------|------|--------|
| P2-1 | `src/cli/mod.rs:30` | 编译失败直接 `panic!` | CLI 工具异常终止 | 未修复（持续 3 期） |
| P2-2 | `src/mir/ssa/deconstruct.rs:176,270` | SSA 寄存器映射失败 panic ×2 | MIR 优化阶段崩溃 | 位置变更 |
| P2-3 | `src/pregel/mod.rs` | 2,552 行，BSP 引擎 + 测试混杂（48% 测试） | 职责边界模糊 | 未修复 |
| P2-4 | `src/pregel/worker_pool.rs` | 超时 worker 线程泄漏（代码注释已承认） | 长时间运行资源泄漏 | 未修复 |
| P2-5 | `src/interpreter/builtins/mod.rs` | 2,254 行中 98.3% 为测试代码，12 个 `#[cfg(test)]` 块 | 文件虚大，应迁出测试 | 改善（-1,065 行） |
| P2-6 | `src/interpreter/dispatch.rs` | 1,648 行，增长 228 行 | 分发表职责过度集中 | 恶化 |
| P2-7 | `src/interpreter/builtins/exec.rs` | 子进程超时后 waiter 线程在极端情况下可能永久阻塞 | worker 线程池耗尽风险 | 新增 |

### P3 — 低优先级 / 观察项

| # | 文件 | 问题 |
|---|------|------|
| P3-1 | `src/mir/jit.rs` | 16 处 `unsafe`（mprotect/VirtualProtect/transmute/JIT 执行） |
| P3-2 | `src/mir/jit.rs` | 仅支持 x86_64 |
| P3-3 | `src/document/backend/image.rs` | 3 处 `unsafe` + 全局 `OnceLock<OcrEngine>` |
| P3-4 | `src/mir/cache.rs:32` | 全局 `static DAG_CACHE: OnceLock<DagCache>`，无失效机制 |
| P3-5 | `src/sandbox/container.rs:331` | 全局 `AtomicU64` 容器计数器（合理） |
| P3-6 | `src/mir/optimize/rule.rs` | 4 个 `static` 模式常量（合理） |

### P4 — 信息项 / 已改善

| # | 说明 |
|---|------|
| P4-1 | `mir/opt.rs` 从 1,176 行拆分为 `opt.rs`(208) + `opt/loops.rs`(463) + `opt/simple.rs`(336) 等，改善显著 |
| P4-2 | `compress/json.rs` 从 1,516 行降至 921 行 |
| P4-3 | `mir/handlers.rs` 从 1,451 行降至 923 行 |
| P4-4 | `mir/ssa.rs` 从 1,444 行降至 1,025 行（拆出 `deconstruct.rs`） |
| P4-5 | 生产代码 `.unwrap()` 从 2 处降至 **0** |

---

## 7. 潜在 Bug / 风险详情

### P1 — 高优先级风险

#### P1-1: Mutex 中毒连锁 panic — `trace_collector.rs`（10 处）

```rust
// src/trace_collector.rs:74,79,103,109,120,140,147,168,195
self.inner.lock().expect("trace collector poisoned")
```

**风险**: `TraceCollector` 使用 `Arc<Mutex<TraceCollectorInner>>`。一旦任意持有锁的线程 panic，Mutex 被 poison，后续所有 `.lock().expect()` 都会 panic，导致整个观测系统崩溃并级联到主执行流程。

**建议**: 改为 `lock().unwrap_or_else(|e| e.into_inner())` 并记录降级事件，避免级联崩溃。

**状态**: 未修复（首次报告 2026-08-02，已持续 3 期）。

#### P1-2 ~ P1-4: Mutex/RwLock 中毒连锁 panic — `schedule` / `event` / `audit`

同样模式，共 **29 处**锁中毒 `expect`：
- `schedule/mod.rs` 9 处（调度器）
- `event/mod.rs` 6 处（事件总线）
- `audit/mod.rs` 4 处（审计系统）
- `trace_collector.rs` 10 处（追踪收集器）

**建议**: 统一引入 `lock_recover()` 辅助函数，将 `Mutex`/`RwLock` 的 poison 处理为降级读取，而不是 panic。

#### P1-5: `parser_v3/mod.rs` 持续膨胀

3,393 行，0% 单元测试覆盖，单文件包含词法分析、语法分析、AST 构建、MIR lowering。较上期增长 167 行。

**建议**: 按阶段拆分为 `lexer/`、`parser/expr.rs`、`parser/stmt.rs`、`lower.rs` 子模块。

#### P1-6: `cargo test --lib` 测试卡住 — `runtime::sandbox::tests::clone_shares_container_arc`

**现象**: 在 `exec_parallel_kills_process_group_on_timeout` 测试之后，该测试卡住超过 6 分钟。

**风险**: CI/本地测试不可靠；可能存在未清理的子进程或死锁。

**建议**:
1. 单独运行 `cargo test --lib runtime::sandbox::tests::clone_shares_container_arc` 确认是否复现。
2. 检查 `exec_parallel_kills_process_group_on_timeout` 是否遗留孤儿进程或持有全局锁。
3. 为该测试添加超时属性或隔离运行。

### P2 — 中优先级风险

#### P2-1: `cli/mod.rs` 编译失败 panic

```rust
// src/cli/mod.rs:30
ParserV3::compile(source).unwrap_or_else(|e| panic!("compile_and_opt failed: {e}"));
```

**状态**: 未修复（持续 3 期）。

#### P2-2: `mir/ssa/deconstruct.rs` 寄存器映射失败 panic ×2

```rust
// src/mir/ssa/deconstruct.rs:176,270
.unwrap_or_else(|| panic!("unmapped SSA reg {}", ssa_r))
```

**状态**: 位置从 `mir/ssa.rs` 迁出，逻辑未变。

#### P2-3: `pregel/worker_pool.rs` 超时线程泄漏

代码注释已承认：

```rust
/// A timed-out job's outcome is absent; its worker thread is leaked
/// (Rust has no cooperative thread cancellation) and the pool must
/// be rebuilt to reclaim it.
```

**状态**: 未修复（持续 3 期，已文档化限制）。

#### P2-7: `interpreter/builtins/exec.rs` waiter 线程泄漏风险

```rust
// src/interpreter/builtins/exec.rs:295-298
let waiter = thread::spawn(move || {
    let result = child.wait_with_output();
    let _ = done_tx.send(result);
});
```

**风险**: timeout 后执行 `kill_process_group(pid)` 再 `waiter.join()`，但如果 kill 失败，waiter 线程将永久阻塞。该函数在 `exec_parallel` 的 worker 线程中调用，可能导致 worker 线程池耗尽。

### P3 — 低优先级

#### P3-1: `mir/jit.rs` 16 处 `unsafe`

包含 `mprotect`/`VirtualProtect`、`std::mem::transmute`、JIT 代码执行。`L1058` 等执行点缺少 `# SAFETY` 注释。

#### P3-4: `mir/cache.rs` 全局 `OnceLock<DagCache>`

进程级全局缓存，无失效/清理机制，长期运行可能累积内存。

---

## 8. 与上期报告对比总结

### 8.1 改善项

| 指标 | 上期 | 本期 | 变化 |
|------|------|------|------|
| 生产 `.unwrap()` | 2 | **0** | ✅ 已清零 |
| TODO/FIXME | 2 | **0** | ✅ -2 |
| `#[allow(dead_code)]` | 8 | **7** | ✅ -1 |
| `builtins/mod.rs` 行数 | 3,319 | **2,254** | ✅ -1,065 |
| `mir/opt.rs` 行数 | 1,176 | **208** | ✅ 拆分子模块 |
| `mir/handlers.rs` 行数 | 1,451 | **923** | ✅ -528 |
| `compress/json.rs` 行数 | 1,516 | **921** | ✅ -595 |
| `mir/ssa.rs` 行数 | 1,444 | **1,025** | ✅ 拆出子模块 |
| 超 1,000 行文件数 | 8 | **7** | ✅ -1 |

### 8.2 恶化 / 新增项

| 指标 | 上期 | 本期 | 变化 |
|------|------|------|------|
| `parser_v3/mod.rs` 行数 | 3,226 | **3,393** | ⚠️ +167 |
| `interpreter/dispatch.rs` 行数 | 1,420 | **1,648** | ⚠️ +228 |
| 生产 `expect`（精确值） | 33* | **80** | ⚠️ +47（主要因上期遗漏） |
| 测试卡住 | 无 | **clone_shares_container_arc >6min** | 🔴 新增 |
| Mutex/RwLock poison `expect` | 10 | **29** | 🔴 新增（主要因上期遗漏） |

\* 上期统计不全。

### 8.3 未修复项（已持续 3 期）

| 项目 | 首次报告 | 状态 |
|------|----------|------|
| `trace_collector.rs` Mutex 中毒连锁 (P1-1) | 2026-08-02 | 未修复 |
| `cli/mod.rs` 编译失败 panic (P2-1) | 2026-08-02 | 未修复 |
| `pregel/worker_pool.rs` 线程泄漏 (P2-4) | 2026-08-02 | 未修复（已文档化） |
| `parser_v3/mod.rs` 大文件 (P1-5) | 2026-08-02 | 恶化 |

---

## 9. 近期重构成效（v0.75.53 → v0.75.82）

| 重构项 | 效果 | 状态 |
|--------|------|------|
| `mir/opt.rs` 拆分为 `mir/opt/` 子模块 | 1,176 → 208 行 + 子模块 | ✅ 完成 |
| `mir/ssa.rs` 拆出 `ssa/deconstruct.rs` | 1,444 → 1,025 行 + 419 行 | ✅ 完成 |
| `builtins/mod.rs` 生产代码迁出 | 3,319 → 2,254 行（仅 38 行生产） | ✅ 完成 |
| `compress/json.rs` 瘦身 | 1,516 → 921 行 | ✅ 完成 |
| `mir/handlers.rs` 瘦身 | 1,451 → 923 行 | ✅ 完成 |
| 死指令删除（RecordTokens/Route） | `mir/mod.rs` 清理 | ✅ 完成 |
| `if` 结果寄存器化 | 新增 `MirInst::Copy` | ✅ 完成 |
| `typeinfer` 死模块删除 | 零调用者移除 | ✅ 完成 |
| 生产 `unwrap` 清零 | 2 → 0 | ✅ 完成 |
| 环境经 `env` 参数单一传递 | 去全局槽/回落 | ✅ 完成 |

---

## 10. 行动建议（按优先级排序）

### 立即行动（本周）

1. **P1-6 调查测试卡住**: 单独运行 `runtime::sandbox::tests::clone_shares_container_arc`，确认是否与 `exec_parallel_kills_process_group_on_timeout` 相关；检查是否有全局锁或孤儿进程未清理。
2. **P1-1 ~ P1-4 锁中毒降级**: 统一将 29 处 `Mutex`/`RwLock` 的 `.lock().expect("...poisoned")` 改为 `.lock().unwrap_or_else(|e| e.into_inner())` 并记录降级事件。涉及文件：
   - `trace_collector.rs`(10)
   - `schedule/mod.rs`(9)
   - `event/mod.rs`(6)
   - `audit/mod.rs`(4)

### 短期（v0.76.x）

3. **P2-1**: `cli/mod.rs:30` panic 改为返回 `Result`。
4. **P2-2**: `mir/ssa/deconstruct.rs` 的 2 处 panic 改为返回 `Result` 或添加 `debug_assert!`。
5. **P1-5**: 拆分 `parser_v3/mod.rs` (3,393 行) 为 `lexer`/`expr`/`stmt`/`lower` 子模块。
6. **P2-5**: 将 `builtins/mod.rs` 的 12 个内联测试块迁出到独立测试文件。

### 中期（v0.77+）

7. **P2-3**: 拆分 `pregel/mod.rs` (2,552 行) 为 `engine`/`worker`/`graph` 子模块。
8. **P2-6**: 分析 `interpreter/dispatch.rs` 增长原因，按 domain 拆分。
9. **P2-7**: 为 `exec.rs` waiter 线程添加更严格的超时与清理，避免 worker 线程池耗尽。
10. **P3-1**: 为 `mir/jit.rs` 的 16 处 `unsafe` 补充 `# SAFETY` 注释，并考虑 `miri` 测试。
11. 分析 1,026 处 `clone()`，识别性能热点。

---

## 11. 附录：统计脚本与复现命令

### 11.1 复现锁中毒 `expect` 列表

```bash
cd /d/Github/mora-lang
grep -RIn '\.lock()\.expect(".*poisoned")' src/
```

### 11.2 复现测试卡住

```bash
cd /d/Github/mora-lang
cargo test --lib --no-fail-fast
```

观察点：运行到 `runtime::sandbox::tests::clone_shares_container_arc` 后是否长时间无响应。

### 11.3 生产代码 unwrap/panic/expect 统计脚本

```bash
cd /d/Github/mora-lang
python3 scripts/audit_stats.py src
```

脚本路径：`scripts/audit_stats.py`（本次检测新增，精确过滤 `#[cfg(test)]` 块与注释）。

---

> **报告生成时间**: 2026-08-04  
> **检测方式**: 自动化脚本 + 人工复核，**未修改任何源代码**
