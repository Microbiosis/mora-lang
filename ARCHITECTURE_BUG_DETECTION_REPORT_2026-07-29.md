# Mora-lang 架构检测与 Bug 检测报告

> **检测日期**: 2026-07-29 08:50 (GMT+8)
> **版本**: Cargo.toml v0.0.53 | 最新 commit `a9770e5`
> **检测范围**: 122 源文件, ~34,920 LOC
> **检测方式**: cargo build / fmt --check / grep 静态分析（未修改任何代码）

---

## 执行摘要

| 维度 | 状态 | 关键发现 |
|------|------|----------|
| **编译** | 🔴 完全失败 | 76 个编译错误 (lib) + 84 个 (test)，项目当前无法编译 |
| **Clippy** | ⚫ 无法运行 | 编译失败，clippy 无法执行 |
| **测试** | ⚫ 无法运行 | 编译失败，测试无法执行 |
| **格式化** | 🟡 116 处违规 | 主要集中在 `mir/expr.rs` 和 `mir_pregel_engine.rs` |
| **架构** | 🟡 部分改善 | Interpreter 已拆为 7 facade，但 MIR Pregel 引擎引入破坏性变更 |
| **代码质量** | 🟡 需关注 | 835 处 unwrap/panic/expect，8.3% 死代码率 (ai_infra.rs) |

### 总体评级: 🔴 P0 — 项目当前不可编译

最新 commit `a9770e5 feat(orchestrate): connect MIR-native Pregel engine` 引入了未完成的 `mir_pregel_engine.rs`，引用了 9 个不存在的类型，导致整个项目编译失败。同时 `mir/expr.rs` 的 `MirExpr` 枚举存在递归类型无限大小错误。

---

## 一、编译错误分析 (P0 — 阻断性)

### 1.1 错误统计

| 错误码 | 数量 | 说明 |
|--------|------|------|
| E0433 | 62 | 找不到类型 (`MirExprKind` 41, `MirReducerKind` 12, `MirCallee` 5, `MirInterruptWhen` 4) |
| E0574 | 15 | 期望 struct/variant，实际是 enum `MirExpr` |
| E0422 | 5 | 找不到 struct (`MirPregelConfig` 4, `Param` 1) |
| E0432 | 1 | 未解析的 import (`MirAgentDef`, `MirEdgeDef`, `MirStateChannel`) |
| E0072 | 1 | 递归类型 `MirExpr` 无限大小 |
| **合计** | **84** | (含 test 编译错误) |

### 1.2 根因分析

**根因 #1: `mir_pregel_engine.rs` 引用 9 个未定义类型**

文件 `src/interpreter/mir_pregel_engine.rs`（375 行）作为 Batch D2 骨架被提交，但引用了以下在 `mir/expr.rs` 中**根本不存在**的类型：

```
MirExprKind       — 41 处引用，期望是 MirExpr 的 kind 字段类型
MirReducerKind    — 12 处引用，期望是 Pregel reducer 枚举
MirCallee         — 5 处引用，期望是函数调用目标类型
MirInterruptWhen  — 4 处引用，期望是中断时机枚举
MirPregelConfig   — 4 处引用，期望是 Pregel 配置结构体
MirAgentDef       — import 失败，期望是 Agent 定义结构体
MirEdgeDef        — import 失败，期望是边定义结构体
MirStateChannel   — import 失败，期望是状态通道结构体
Param             — 1 处引用，期望是参数结构体
```

文件中大量 TODO 注释表明这些类型"将在 Pregel 迁移期间定义"，但迁移从未完成。这是典型的 **未完成功能被提交** 问题。

**根因 #2: `MirExpr` 枚举递归类型无限大小 (E0072)**

`src/mir/expr.rs:40` 的 `MirExpr` 枚举存在 3 个未装箱的递归变体：

```rust
// 第 143-147 行 — Command 变体中 MirExpr 未用 Box
Command {
    update: Option<(String, MirExpr)>,  // ← 缺少 Box
    ...
},

// 第 150-153 行 — Send 变体中 MirExpr 未用 Box
Send {
    input: MirExpr,  // ← 缺少 Box
},

// 第 163-167 行 — EvalTest 变体中 MirExpr 未用 Box
EvalTest {
    given: MirExpr,  // ← 缺少 Box
    expects: Vec<MirExpr>,  // Vec 可以，但 given 需要 Box
    ...
},
```

编译器建议: `insert some indirection (e.g., a Box, Rc, or &) to break the cycle`

**根因 #3: `mir_pregel_engine.rs` 将 `MirExpr` 当 struct 使用 (E0574)**

引擎代码假设 `MirExpr` 是一个带 `kind: MirExprKind` 字段的 struct，但实际定义是 enum。15 处 `MirExpr { kind: ... }` 构造全部失败。

### 1.3 API 不一致 Bug

| 位置 | 问题 |
|------|------|
| `mir_pregel_engine.rs:47` | `new(_config: String)` — 构造函数接受 `String` 占位符 |
| `mir_pregel_engine.rs:381` | 测试中 `MirPregelEngine::new(config)` 传入 `MirPregelConfig` — 类型不匹配 |
| `mir_pregel_engine.rs:92` | `run()` 中 `self.config.edges` — `config` 是 `String`，没有 `.edges` 字段 |
| `mir_pregel_engine.rs:114` | `collect_interrupts(node_name, MirInterruptWhen::Before)` — 函数签名是 `&str`，传入枚举值 |
| `mir_pregel_engine.rs:139` | `self.config.edges` / `self.config.agents` — `String` 类型无此字段 |
| `mir_pregel_engine.rs:152` | `self.config.agents[agent_idx]` — 同上 |
| `mir_pregel_engine.rs:246` | `state_reducers` 是 `HashMap<String, String>` 但 `.unwrap_or(MirReducerKind::Last)` — 类型不匹配 |

### 1.4 编译警告 (6 个)

```
warning: unused import: `crate::mir::MirFunction`
warning: unused imports: `EnumVariant` and `StructField`
warning: unused import: `crate::interpreter::mir_pregel_engine::MirPregelEngine`
warning: unused import: `std::collections::HashMap`
warning: unused import: `crate::common::Literal`
warning: unused import: `std::collections::HashMap`
```

---

## 二、格式化检测

- **违规文件数**: 116 处 diff
- **主要影响文件**:
  - `examples/debug_parse.rs` — 7 处
  - `examples/debug_parser.rs` — 2 处
  - `src/interpreter/mir_pregel_engine.rs` — 9 处
  - `src/mir/expr.rs` — 大量（几乎每行都需格式化）
- **评估**: `mir/expr.rs` 和 `mir_pregel_engine.rs` 是最新提交的文件，未运行 `cargo fmt`

---

## 三、架构检测

### 3.1 Interpreter Facade 重构状态 (ADR-001)

✅ **已完成**: Interpreter 从 god object（30+ 字段）重构为 7 个 facade holder：

```rust
pub struct Interpreter {
    pub(crate) core: CoreRuntime,        // BC1: 8 个核心执行字段
    pub(crate) registry: RegistryRuntime, // BC8: trait/impl/mock/ccr/memory
    pub(crate) infra: InfraRuntime,       // BC9: recorder/interner/cache/bus/scheduler
    pub(crate) ai: AiRuntime,             // BC3: model_routes/budget/trace/cache
    pub(crate) sandbox: SandboxRuntime,   // BC7: sandbox/container/tool_planes
    pub(crate) persist: PersistRuntime,   // BC5: audit/checkpoint/memory
    pub(crate) orch: OrchRuntime,         // BC4: plans/refine/skill
}
```

⚠️ **遗留问题**:
- 所有字段标记为 `pub(crate)` 而非 `pub`，但 binary crate（main.rs/lsp.rs）需要访问
- `Clone` 仍需手动实现（7 个 facade 全部需要 Clone）
- facade 内部逻辑仍在 `interpreter/` 模块中（builtins.rs 85 unwrap, dispatch.rs, ai_chat.rs 874 行）

### 3.2 MIR 层架构问题

🔴 **MIR Pregel 引擎是半成品**:
- `mir_pregel_engine.rs` 注释明确写着"当前为骨架（Batch D2）：定义结构 + 接口，内部逻辑先用 TODO 标记"
- 但 commit `a9770e5` 将其 `pub(crate)` 并在 `interpreter/mod.rs:5` 注册了模块
- 旧版 `orchestrate_v2::PregelEngine` 仍然存在（注释说 "Batch D4 删除旧 PregelEngine"）
- 两个 Pregel 引擎并存，但新版无法编译

🟡 **MIR expr.rs 与 parser_v3 的类型不匹配**:
- `parser_v3/mod.rs` 有 6 个 TODO 标记 "Migrate to MirExpr during full migration"
- `mir/interp.rs` 有 TODO "Pregel orchestration types will be defined during migration"
- `mir/lower.rs` 有 TODO "Implement proper callee extraction during migration"
- `typeck/mod.rs` 有 TODO "implement HM type inference on MirExpr tree"

### 3.3 文件大小分布 (Top 10)

| 排名 | 文件 | 行数 | 风险评估 |
|------|------|------|----------|
| 1 | `parser_v2/statements.rs` | 2224 | 🟡 超大文件，应拆分 |
| 2 | `typeck/mod.rs` | 2063 | 🟡 超大文件 |
| 3 | `parser_v3/mod.rs` | 1713 | 🟡 超大文件，6 个 TODO |
| 4 | `mir/lower.rs` | 1519 | 🟡 复杂 lowering 逻辑 |
| 5 | `compress/json.rs` | 1512 | 🟡 JSON 压缩逻辑集中 |
| 6 | `mir/ssa.rs` | 1356 | 🟡 SSA 构造 |
| 7 | `typeck/check.rs` | 1302 | 🟡 类型检查 |
| 8 | `interpreter/orchestrate_v2.rs` | 1273 | 🟡 旧版 Pregel 引擎 |
| 9 | `interpreter/dispatch.rs` | 1215 | 🟡 方法分派 |
| 10 | `main.rs` | 1106 | 🟡 CLI 入口 |

### 3.4 依赖架构

```
Cargo.toml 依赖 (14 个):
  ureq 3.3          — 同步 HTTP 客户端
  tokio 1           — async runtime (仅 HTTP/MCP/LSP server)
  crossbeam-channel — Send/Sync channel
  parking_lot       — 高性能锁
  sha2              — 审计哈希链
  flate2            — 录制文件压缩
  libc              — SO_REUSEADDR
  undoc             — DOCX/PPTX 解析
  ocrs + rten       — OCR 引擎
  anyhow            — 错误处理
  image             — 图像处理
  lopdf             — PDF 解析
  pdf-extract       — PDF 文本提取
  pulldown-cmark    — Markdown 解析
  quick-xml         — HTML/XML 解析
  uuid              — Checkpoint ID
  rusqlite (opt)    — SQLite checkpoint
  inkwell (opt)     — LLVM JIT
  proptest (dev)    — 属性测试
```

⚠️ **关注点**: `ocrs` + `rten` + `image` + `lopdf` + `pdf-extract` 引入了较重的 ML/PDF 依赖，增加了编译时间和二进制体积。

---

## 四、Bug 检测

### 4.1 P0 — 阻断性 Bug

| # | Bug | 文件:行 | 影响 |
|---|-----|---------|------|
| P0-1 | 项目无法编译 — 9 个未定义类型 | `mir_pregel_engine.rs` 全文 | 所有功能不可用 |
| P0-2 | MirExpr 递归类型无限大小 | `mir/expr.rs:145,151,163` | 编译失败 |
| P0-3 | MirExpr 被当作 struct 使用 | `mir_pregel_engine.rs` (15 处) | 编译失败 |

### 4.2 P1 — 严重 Bug

| # | Bug | 文件:行 | 影响 |
|---|-----|---------|------|
| P1-1 | MirPregelEngine::new() 签名与测试不匹配 | `:47` vs `:381` | 类型系统矛盾 |
| P1-2 | run() 访问 String 类型的 .edges/.agents | `:139,152,181` | 逻辑错误 |
| P1-3 | collect_interrupts 签名 &str 但传枚举值 | `:114,195` vs `:304` | 类型不匹配 |
| P1-4 | state_reducers HashMap<String,String> vs MirReducerKind | `:31` vs `:246` | 类型不匹配 |
| P1-5 | restore_checkpoint 为空实现 | `:324` | 检查点恢复不工作 |

### 4.3 P2 — 代码质量 Bug

| # | Bug | 统计 | 影响 |
|---|-----|------|------|
| P2-1 | unwrap() 滥用 | 464 处 (src/) | 运行时 panic 风险 |
| P2-2 | panic! 滥用 | 146 处 (src/) | 运行时崩溃 |
| P2-3 | expect() 滥用 | 225 处 (src/) | 运行时 panic 风险 |
| P2-4 | 死代码 | ai_infra.rs 65/783 = 8.3% | 维护负担 |
| P2-5 | TODO/FIXME 标记 | 29 处 / 9 文件 | 未完成功能 |
| P2-6 | #[allow(dead_code)] 滥用 | 100+ 处 | 隐藏真实死代码 |

### 4.4 unwrap/panic 热点文件 (Top 10)

| 文件 | unwrap | panic | expect | 合计 |
|------|--------|-------|--------|------|
| `interpreter/builtins/mod.rs` | 85 | 100 | — | 185 |
| `checkpoint/mod.rs` | 37 | — | — | 37 |
| `checkpoint/sqlite.rs` | 32 | — | — | 32 |
| `audit/mod.rs` | 25 | 1 | — | 26 |
| `interpreter/orchestrate_v2.rs` | 19 | 6 | — | 25 |
| `flow.rs` | 11 | 4 | — | 15 |
| `checkpoint/memory.rs` | 7 | — | — | 7 |
| `heartbeat/mod.rs` | 10 | — | — | 10 |
| `lsp/json.rs` | 11 | 2 | — | 13 |
| `compress/json.rs` | 3 | — | — | 3 |

### 4.5 TODO/FIXME 分布

| 文件 | 数量 | 内容摘要 |
|------|------|----------|
| `mir_pregel_engine.rs` | 13 | Pregel 迁移类型定义、构造函数、中断点、检查点恢复 |
| `parser_v3/mod.rs` | 6 | 迁移到 MirExpr、Agent 类型、边条件、循环编排 |
| `mir/jit.rs` | 3 | LLVM 绑定、SSA→IR→JIT、类型映射 |
| `mir/interp.rs` | 2 | Pregel 编排类型、编排实现 |
| `mir/lower.rs` | 1 | Callee 提取 |
| `mir/expr.rs` | 1 | Pattern 扩展 (Tuple/List/Dict) |
| `mir/optimize/mod.rs` | 1 | RegMatcher 注释 |
| `typeck/mod.rs` | 1 | HM 类型推断 on MirExpr |
| `typeck/hm/unify.rs` | 1 | 数值类型子类型规则 |

---

## 五、修复优先级建议

### 🔴 立即修复 (P0 — 恢复编译)

1. **在 `mir/expr.rs` 中为 Command/Send/EvalTest 的 MirExpr 字段添加 `Box`**
   - `update: Option<(String, Box<MirExpr>)>`
   - `input: Box<MirExpr>`
   - `given: Box<MirExpr>`

2. **选择以下方案之一处理 `mir_pregel_engine.rs`**:
   - **方案 A (推荐)**: 将模块标记为 `#[cfg(not(test))]` 或注释掉 `mod mir_pregel_engine` 声明，直到类型定义完成
   - **方案 B**: 补全所有 9 个缺失类型定义 + 修复 API 不一致
   - **方案 C**: 回退 commit `a9770e5` 中 mir_pregel_engine.rs 的引入部分

3. **运行 `cargo fmt`** 格式化 `mir/expr.rs` 和 `mir_pregel_engine.rs`

### 🟡 短期修复 (P1 — 1-2 周)

4. 清理 6 个 unused import 警告
5. 统一 MirPregelEngine 的 new() 签名与测试
6. 修复 collect_interrupts 的类型签名
7. 实现或移除 restore_checkpoint 空实现

### 🟢 中期改进 (P2 — 持续)

8. 将 builtins/mod.rs (185 unwrap/panic) 逐步替换为 `?` + Result
9. 清理 ai_infra.rs 的 65 个 dead_code 标注（移除或实现）
10. 拆分 parser_v2/statements.rs (2224 行) 和 typeck/mod.rs (2063 行)
11. 将 `cargo fmt --check` 纳入 CI 流程

---

## 六、与上次检测对比 (2026-07-11)

| 指标 | 2026-07-11 | 2026-07-29 | 变化 |
|------|-----------|-----------|------|
| 编译状态 | ✅ 通过 (863 测试) | 🔴 失败 (76 错误) | ⬇️ 严重恶化 |
| unwrap | 473 | 464 | ⬇️ -9 (微降) |
| panic! | 173 | 146 | ⬇️ -27 (改善) |
| expect | 328 | 225 | ⬇️ -103 (改善) |
| 合计 | 974 | 835 | ⬇️ -139 (改善) |
| ai_infra dead_code | 65 | 65 | → 不变 |
| fmt 违规 | 未检测 | 116 | 🆕 新指标 |
| TODO/FIXME | 未检测 | 29 | 🆕 新指标 |

**关键变化**: 上次检测时项目可编译（863 测试通过），本次检测项目完全无法编译。根因是 commit `a9770e5` 引入了未完成的 MIR Pregel 引擎。unwrap/panic/expect 总量有所改善（-139），但被编译失败掩盖。

---

## 七、结论

Mora-lang 项目正处于 **MIR 迁移的关键过渡期**。Interpreter facade 重构（ADR-001）已取得实质性进展，unwrap/panic 数量持续下降。但最新 commit 引入了半成品的 `mir_pregel_engine.rs`，导致项目完全无法编译。

**首要行动**: 恢复编译能力（P0-1/2/3），然后再继续 MIR Pregel 迁移。在编译恢复前，所有其他改进都无法验证。

---

*报告生成于 2026-07-29 | 自动化架构检测任务*
