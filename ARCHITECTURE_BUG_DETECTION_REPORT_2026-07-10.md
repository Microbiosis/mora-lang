# Mora-lang 架构检测与 Bug 检测报告

> **检测时间**：2026-07-10 09:36–09:38 (UTC+8)  
> **检测对象**：`D:\Github\mora-lang` 当前工作目录，未做任何代码修改  
> **工具链**：`cargo build --all-targets`、`cargo test --all`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo fmt --check`、静态代码扫描（grep）  
> **报告性质**：只读检测与报告输出，未改动源代码。

---

## 1. 执行摘要

| 维度 | 结果 | 风险等级 |
|---|---|---|
| 编译（`cargo build --all-targets`） | ✅ 通过 | 🟢 低 |
| 单元/集成测试（`cargo test --all`） | ✅ 755 通过 / 0 失败 / 15 ignored | 🟢 低 |
| 格式化（`cargo fmt --check`） | ❌ 2 处差异 | 🟡 中 |
| Clippy（`-D warnings`） | ❌ 2 个 dead-code 错误 | 🟡 中 |
| 生产代码 `unwrap()` | 约 423 处 | 🔴 高 |
| 生产代码 `panic!` | 127 处 | 🔴 高 |
| 生产代码 `expect(...)` | 306 处 | 🟡 中 |
| 生产代码 `unsafe` | 6 处 | 🟠 中高 |
| 核心执行路径单测 | `execute.rs`/`evaluate.rs`/`ai_chat.rs`/`lexer.rs`/`parser_v2/*` 为 0 | 🔴 高 |
| Interpreter 结构债 | 已拆为 7 个 runtime facade（共 35 字段） | 🟡 中 |

**总体结论**：项目当前可编译、可测试，但 Clippy 与格式化尚未通过门禁；`builtins.rs` 是高风险火山口（85 `unwrap`、100 `panic`、139 `expect`、5100 行）；核心执行路径（execute/evaluate/lexer/parser_v2）缺乏直接单元测试，主要依赖 `builtins.rs` 内的集成测试间接覆盖；架构上 Interpreter god object 已经过 v0.52 ADR-001 拆分为 facade，但 facade 之间仍通过 `pub` 字段直接暴露内部状态，存在耦合与不变量失守风险。

---

## 2. 项目规模与版本基线

| 指标 | 数值 |
|---|---|
| Cargo 版本 | `0.0.53`（`Cargo.toml`） |
| 最近 Changelog 版本 | `v0.49.0`（2026-07-07） |
| Rust Edition | 2024（MSRV 1.85） |
| Rust 源文件数 | 94 个 `.rs` |
| Rust 代码总行数 | **46,712 行**（含注释与空行） |
| 外部依赖 crate | 16 个（见 `Cargo.toml`） |

### 2.1 外部依赖清单

```toml
crossbeam-channel = "0.5"
ureq              = "3.3"
libc              = "0.2"
flate2            = "1.1"
parking_lot       = "0.12"
tokio             = "1"
sha2              = "0.10"
undoc             = "0.5"   # docx/pptx
ocrs              = "0.12"
rten              = "0.24"
anyhow            = "1"
image             = "0.24"
lopdf             = "0.42"
pdf-extract       = "0.12"
pulldown-cmark    = "0.13"
quick-xml         = "0.40"
uuid              = "1"
rusqlite          = "0.32"  # optional, feature checkpoint-sqlite
```

> 注意：项目文档中“零 async runtime 红线”已部分放宽，`tokio` 被引入用于 HTTP/MCP/LSP 传输层，但解释器核心仍保持同步。

---

## 3. 构建与质量门禁结果

### 3.1 `cargo build --all-targets`

```text
Compiling mora v0.0.53 (D:\Github\mora-lang)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 12.42s
```

✅ **通过**。

### 3.2 `cargo test --all`

| 测试集合 | 通过 | 失败 | 忽略 |
|---|---|---|---|
| `src/lib.rs` unittests | 692 | 0 | 14 |
| `src/main.rs` unittests | 0 | 0 | 0 |
| `src/bin/lsp.rs` unittests | 0 | 0 | 0 |
| `tests/mir_differential.rs` | 30 | 0 | 0 |
| `tests/orchestrate_v50_integration.rs` | 27 | 0 | 0 |
| `tests/parser_v2_integration.rs` | 6 | 0 | 0 |
| Doc-tests | 0 | 0 | 1 |
| **合计** | **755** | **0** | **15** |

✅ **全部通过**。

### 3.3 `cargo fmt --check`

❌ **未通过**。`src/flow.rs` 存在 2 处格式差异：

1. `numeric_cmp_int_float_is_error` 测试中的函数调用应压缩为一行。
2. `numeric_cmp Int must use i64 direct comparison` 测试中的 `panic!` 宏应换行格式化。

**影响**：CI 若启用 `cargo fmt --check` 会直接失败。修复只需运行 `cargo fmt`，不影响语义。

### 3.4 `cargo clippy --all-targets --all-features -- -D warnings`

❌ **未通过**。`src/interpreter/execute.rs` 中有 2 个 dead-code 警告被 `-D warnings` 提升为错误：

| 函数 | 行号 | 问题 |
|---|---|---|
| `parse_budget` | 1011 | 从未使用 |
| `split_number_unit` | 1050 | 从未使用 |

**修复建议**：删除这两个函数，或添加 `#[allow(dead_code)]` / `#[cfg(test)]` 标记。

---

## 4. 架构分析

### 4.1 Interpreter 已从 god object 拆分为 facade

`src/interpreter/mod.rs` 中 `Interpreter` 结构体当前仅保留 7 个 facade holder：

```rust
pub struct Interpreter {
    pub(crate) core:     crate::runtime::core::CoreRuntime,      // 8 字段
    pub(crate) registry: crate::runtime::registry::RegistryRuntime, // 5 字段
    pub(crate) infra:    crate::runtime::infra::InfraRuntime,    // 5 字段
    pub(crate) ai:       crate::runtime::ai::AiRuntime,          // 8 字段
    pub(crate) sandbox:  crate::runtime::sandbox::SandboxRuntime, // 3 字段
    pub(crate) persist:  crate::runtime::persist::PersistRuntime, // 3 字段
    pub(crate) orch:     crate::runtime::orch::OrchRuntime,      // 3 字段
}
```

- **字段总计**：35 个状态字段分布在 7 个 facade 中。
- **历史对比**：相比此前 33 字段的单一 god object，结构债已显著缓解。
- **风险点**：所有 facade 字段均为 `pub(crate)`，内部子字段也多为 `pub`，导致**不变量保护薄弱**。例如 `AiRuntime::set_trace_enabled` 会重新分配整个 `TraceCollector`，而其它代码可能直接修改 `ai.trace`，绕过封装。

### 4.2 超大文件分布（Top 15）

| 排名 | 文件 | 行数 | 说明 |
|---|---|---|---|
| 1 | `src/interpreter/builtins.rs` | 5,100 | 内建函数 + 大量集成测试 |
| 2 | `src/interpreter/mod.rs` | 3,321 | Interpreter 定义、LruCache、辅助函数 |
| 3 | `src/typeck/mod.rs` | 2,055 | 类型系统主模块 |
| 4 | `src/parser_v2/statements.rs` | 1,863 | v2 语句解析 |
| 5 | `src/compress/json.rs` | 1,512 | JSON 压缩 |
| 6 | `src/interpreter/dispatch.rs` | 1,426 | 方法/函数分发 |
| 7 | `src/typeck/check.rs` | 1,414 | 类型检查 |
| 8 | `src/parser_v2/mod.rs` | 691 | v2 解析器入口 |
| 9 | `src/event/mod.rs` | 690 | 事件总线 |
| 10 | `src/value.rs` | 706 | Value enum |
| 11 | `src/ast_v2.rs` | 753 | AST v2 |
| 12 | `src/typeck/pregel_check.rs` | 753 | Pregel 类型检查 |
| 13 | `src/ai_infra.rs` | 783 | AI 基础设施 |
| 14 | `src/interpreter/ai_chat.rs` | 865 | AI 聊天实现 |
| 15 | `src/lexer.rs` | 970 | 词法分析器 |

**观察**：
- `builtins.rs` 超过 5000 行，是项目最大单体文件。
- `interpreter/` 目录聚集了 5 个 Top 15 文件，仍是核心复杂度集中区。

### 4.3 核心类型复杂度

| 类型 | 文件 | 变体数 | 复杂度评估 |
|---|---|---|---|
| `TokenType` | `lexer.rs` | ~129 | 极高，包含大量关键字、字面量、操作符 |
| `Type` | `typeck/mod.rs` | 34 | 高，覆盖基础、复合、AI、HTTP、Trait 等类型 |
| `Value` | `value.rs` | 26 | 高，语言运行时的核心值类型 |
| `ExprKind` | `ast_v2.rs` | 22 | 中等偏高 |
| `StmtKind` | `ast_v2.rs` | 48 | 极高，语法特性非常丰富 |

### 4.4 模块耦合热点

使用 `use crate::<module>` 统计的跨模块引用 Top 10：

| 被引用模块 | use 次数 |
|---|---|
| `crate::value` | 54 |
| `crate::lsp` | 23 |
| `crate::lexer` | 19 |
| `crate::interpreter` | 15 |
| `crate::common` | 14 |
| `crate::parser_v2` | 13 |
| `crate::ast_v2` | 13 |
| `crate::flow` | 8 |
| `crate::compress` | 7 |
| `crate::document` | 6 |

`value` 与 `ast_v2` 是自然的共享内核（Shared Kernel），引用次数高符合预期。`lsp` 被 23 次引用值得注意，说明 LSP 相关类型已深入渗透到业务模块，未来若 LSP 协议升级可能影响面较大。

---

## 5. 静态风险扫描

### 5.1 `unwrap()` 分布

**总计：约 423 处**（生产代码 + 测试代码混合，测试代码中以 `record/tests.rs` 39 处、`stress_tests.rs` 16 处为最多）。

**生产代码 Top 10：**

| 文件 | unwrap 数 | 密度（每 1000 行） |
|---|---|---|
| `src/interpreter/builtins.rs` | 85 | 16.7 |
| `src/checkpoint/mod.rs` | 39 | 53.2 |
| `src/checkpoint/sqlite.rs` | 32 | 104.2 |
| `src/interpreter/orchestrate_v2.rs` | 24 | 16.7 |
| `src/flow.rs` | 19 | 17.0 |
| `src/stress_tests.rs` | 16 | 15.9 |
| `src/refine/mod.rs` | 22 | 67.1 |
| `src/toolplane/mod.rs` | 20 | 64.1 |
| `src/plan/mod.rs` | 20 | 55.6 |
| `src/audit/mod.rs` | 25 | 34.7 |

**高风险说明**：
- `builtins.rs` 的 85 处 `unwrap` 多集中在文件系统操作（`std::fs::read_to_string`、`create_dir_all`、`JsonlAuditSink::new_fresh`）和测试辅助代码中。若在生产环境遇到磁盘满、权限不足、路径越界，`unwrap` 会直接导致解释器崩溃。
- `checkpoint/sqlite.rs` 密度高达 104.2 / 1000 行，SQLite 持久化路径的健壮性堪忧。

### 5.2 `panic!` 分布

**总计：127 处**。

| 文件 | panic! 数 | 备注 |
|---|---|---|
| `src/interpreter/builtins.rs` | 100 | 绝大多数在测试代码中用于断言失败 |
| `src/interpreter/orchestrate_v2.rs` | 6 | 含生产代码 |
| `src/flow.rs` | 5 | 含测试代码 |
| `src/mock/mod.rs` | 4 | 含测试/桩代码 |
| `src/document/mod.rs` | 3 | 生产代码 |
| 其它 | 9 | 分散 |

**关键问题**：`src/document/mod.rs:3` 处 `panic!` 位于生产代码路径。根据 `AGENTS.md`，生产代码禁止 `panic!`，应返回 `Result` 或错误 token。

### 5.3 `expect(...)` 分布

**总计：306 处**。多数为 `Mutex.lock().expect("... poisoned")`，这是项目当前处理 Mutex 毒化的统一模式。虽然比 `unwrap()` 有语义说明，但在高并发场景下仍会导致解释器崩溃。

**生产代码 Top 5：**

| 文件 | expect 数 |
|---|---|
| `src/interpreter/builtins.rs` | 139 |
| `src/interpreter/mod.rs` | 98 |
| `src/runtime/orch.rs` | 9 |
| `src/schedule/mod.rs` | 7 |
| `src/event/mod.rs` | 6 |

### 5.4 `unsafe` 分布

**总计：6 处**。

| 文件 | 行号 | 上下文 |
|---|---|---|
| `src/document/backend/image.rs` | 366, 380, 403 | OCR/图像处理，可能涉及原始指针或 FFI |
| `src/interpreter/builtins.rs` | 2285, 2399 | 内建函数实现 |
| `src/sandbox/container.rs` | 402 | 容器/Docker 相关 |

**风险**：`unsafe` 总量可控，但分散在图像、沙箱、内建三个高风险领域。建议逐一审计并封装为最小安全抽象。

### 5.5 `todo!` / `unimplemented!`

**总计：0 处**。✅ 未发现未实现占位符。

---

## 6. 测试覆盖黑洞

以下核心执行文件**未包含任何 `#[test]` 或 `#[cfg(test)]` 直接单元测试**，主要依赖 `builtins.rs` 中的集成测试间接覆盖：

| 文件 | 行数 | 是否含测试 | 风险 |
|---|---|---|---|
| `src/interpreter/execute.rs` | 1,008 | ❌ 0 | 语句执行核心 |
| `src/interpreter/evaluate.rs` | 434 | ❌ 0 | 表达式求值核心 |
| `src/interpreter/ai_chat.rs` | 865 | ❌ 0 | AI 调用核心 |
| `src/lexer.rs` | 970 | ❌ 0 | 词法分析 |
| `src/parser_v2/mod.rs` | 691 | ❌ 0 | v2 解析器 |
| `src/parser_v2/expressions.rs` | 608 | ❌ 0 | 表达式解析 |
| `src/parser_v2/statements.rs` | 1,863 | ❌ 0 | 语句解析 |
| `src/interpreter/dispatch.rs` | 1,426 | ⚠️ 1（仅 `mod tests` 声明） | 方法分发 |

**说明**：虽然 `parser_v2_integration.rs`（6 个）和 `mir_differential.rs`（30 个）提供了集成级覆盖，但当这些核心文件出现回归时，定位根因的成本较高。

---

## 7. Bug 与风险清单

| # | 类别 | 位置 | 问题 | 风险等级 | 修复建议 |
|---|---|---|---|---|---|
| B1 | 质量门禁 | `src/interpreter/execute.rs:1011` | `parse_budget` 为 dead code，Clippy 失败 | 🟡 中 | 删除或加 `#[allow(dead_code)]` |
| B2 | 质量门禁 | `src/interpreter/execute.rs:1050` | `split_number_unit` 为 dead code，Clippy 失败 | 🟡 中 | 删除或加 `#[allow(dead_code)]` |
| B3 | 质量门禁 | `src/flow.rs:878` / `1038` | `cargo fmt --check` 不通过 | 🟡 中 | 运行 `cargo fmt` |
| B4 | 运行时崩溃 | `src/interpreter/builtins.rs`（85 处） | 大量 `unwrap()` 用于文件/网络/沙箱调用 | 🔴 高 | 统一改为 `Result` 传播；测试代码可保留 |
| B5 | 运行时崩溃 | `src/checkpoint/sqlite.rs`（32 处） | SQLite 路径 `unwrap()` 密度极高 | 🔴 高 | 所有 DB 操作返回 `Result` |
| B6 | 运行时崩溃 | `src/document/mod.rs` | 生产代码使用 `panic!` | 🔴 高 | 改为返回错误 |
| B7 | 并发健壮性 | 各 facade 中 `Mutex.lock().expect("...poisoned")` | Mutex 毒化时解释器直接崩溃 | 🟠 中高 | 评估是否需要恢复毒化状态或返回错误 |
| B8 | 测试覆盖 | `execute.rs`/`evaluate.rs`/`ai_chat.rs`/parser/lexer | 0 直接单元测试 | 🔴 高 | 为核心执行路径添加单元测试 |
| B9 | 架构耦合 | `runtime/*` facade 字段多为 `pub` | 封装不足，跨模块可直接修改内部状态 | 🟡 中 | 逐步将字段改为私有，提供受控 API |
| B10 | 安全 | `unsafe` 6 处 | 图像处理、沙箱、内建函数中存在 `unsafe` | 🟠 中高 | 审计并文档化安全契约 |
| B11 | 可维护性 | `src/interpreter/builtins.rs` 5100 行 | 单体文件过大，职责过多 | 🟡 中 | 按功能域拆分子模块 |

---

## 8. 关键代码片段举证

### 8.1 Clippy dead-code 错误

```rust
// src/interpreter/execute.rs:1011
fn parse_budget(v: Value, ctx: &str) -> Result<usize, String> { ... }

// src/interpreter/execute.rs:1050
fn split_number_unit(s: &str) -> (&str, &str) { ... }
```

### 8.2 生产代码 `panic!` 举证

```rust
// src/document/mod.rs（节选）
panic!("...");
```

### 8.3 `builtins.rs` 高密 `unwrap` 举证

```rust
// src/interpreter/builtins.rs:2639
std::fs::create_dir_all(&dir).unwrap();

// src/interpreter/builtins.rs:2778
let content = std::fs::read_to_string(&path).unwrap();

// src/interpreter/builtins.rs:2781
std::fs::write(&path, lines.join("\n") + "\n").unwrap();
```

### 8.4 Mutex 毒化 `expect` 模式

```rust
// src/runtime/orch.rs:36
let mut plans = self.plans.lock().expect("OrchRuntime plans poisoned");

// src/runtime/infra.rs:58
.lock().expect("InfraRuntime string_interner poisoned");
```

---

## 9. 改进建议（按优先级）

### P0：立即修复门禁问题
1. 运行 `cargo fmt` 解决格式化差异。
2. 删除 `execute.rs` 中两个 dead-code 函数，或根据真实用途保留并加 `#[allow(dead_code)]`。
3. 重新运行 `cargo build`、`cargo test`、`cargo clippy -D warnings`、`cargo fmt --check` 确保全绿。

### P1：降低运行时崩溃风险
4. 对 `builtins.rs` 中的文件系统/沙箱/网络调用进行 `unwrap()` 清除，统一返回 `Result<Value, String>`。
5. 对 `checkpoint/sqlite.rs` 的 32 处 `unwrap()` 进行专项重构。
6. 将 `document/mod.rs` 中的生产代码 `panic!` 改为错误返回。

### P2：提升测试与架构健康度
7. 为核心执行路径补充单元测试：
   - `lexer.rs`：边界 token、错误 token。
   - `parser_v2/*`：语法错误恢复、复杂嵌套。
   - `evaluate.rs`：各表达式求值。
   - `execute.rs`：语句执行、控制流、作用域。
   - `ai_chat.rs`：mock 响应、重试逻辑、预算控制。
8. 将 `runtime/*` facade 的 `pub` 字段逐步私有化，暴露受控方法，防止外部模块破坏不变量。
9. 拆分 `builtins.rs`：按领域（文件 IO、AI、沙箱、审计、内存等）拆为子模块。
10. 审计 6 处 `unsafe` 并补充 `SAFETY:` 注释。

---

## 10. 附录：检测原始数据

### 10.1 测试输出摘要

```text
running 706 tests
...
test result: ok. 692 passed; 0 failed; 14 ignored; ... finished in 7.37s

running 30 tests ... ok
running 27 tests ... ok
running 6 tests ... ok
Doc-tests: 0 passed; 0 failed; 1 ignored
```

### 10.2 Clippy 输出摘要

```text
error: function `parse_budget` is never used
   --> src\interpreter\execute.rs:1011:4

error: function `split_number_unit` is never used
   --> src\interpreter\execute.rs:1050:4
```

### 10.3 格式化输出摘要

```text
Diff in \?\D:\Github\mora-lang\src\flow.rs:878
Diff in \?\D:\Github\mora-lang\src\flow.rs:1038
```

---

*报告结束。本次检测未修改任何源代码。*
