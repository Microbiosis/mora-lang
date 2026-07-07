# Mora-lang 综合审查报告

> **审查范围**: 代码安全审计 + 架构优化分析
> **代码库**: `D:\Github\mora-lang`
> **版本**: v0.0.52 (edition 2024)
> **总代码量**: ~40,000 LOC / 77 Rust 源文件

---

## 一、代码安全审计

### 1.1 执行摘要

| 扫描模块 | 状态 | 说明 |
|---------|------|------|
| 密钥泄露检测 | ✅ 通过 | 未发现真实密钥泄露（排除 14 项熵值误报） |
| OWASP 模式检测 | ⚠️ 2 项发现 | CORS 通配符 + 默认绑定 0.0.0.0 |
| 依赖漏洞扫描 | ⚠️ 受限 | `cargo audit` 不可用，需手动复查 |
| `unwrap()` 合规 | ❌ 违规 | 生产代码中 334 处 `unwrap()`，违反 AGENTS.md |

---

### 1.2 真实安全问题

#### 🔴 A05: 安全配置错误 — CORS 通配符

- **文件**: `src/http_server.rs:468`
- **代码**:
  ```rust
  "Access-Control-Allow-Origin: *\r\n"
  ```
- **风险**: HTTP 响应头设置 `Access-Control-Allow-Origin: *`，允许任何来源的跨域请求。若服务端点涉及敏感操作（文件读写、AI 调用、容器执行），可被恶意网页利用。
- **建议**: 将通配符改为脚本可配置的白名单，或默认关闭 CORS（仅允许 `localhost`）。

#### 🟡 A05: 安全配置错误 — 默认绑定 0.0.0.0

- **文件**: `src/interpreter/dispatch.rs:1009-1010`
- **代码**:
  ```rust
  let addr = args.first().map(|v| v.to_string())
      .unwrap_or_else(|| "0.0.0.0:3000".to_string());
  let (host, port) = addr.split_once(':').unwrap_or(("0.0.0.0", "3000"));
  ```
- **风险**: `Router.listen()` 默认监听所有网络接口（`0.0.0.0`）。在公共网络环境中，这会将开发中的 HTTP 服务暴露给外部。
- **建议**: 默认值改为 `"127.0.0.1:3000"`，由用户显式选择 `"0.0.0.0"` 时给出警告。

---

### 1.3 代码质量违规

#### ❌ `unwrap()` 泛滥（334 处生产代码）

- **严重程度**: HIGH
- **违反规则**: AGENTS.md §3 — "生产代码**禁止** `unwrap()` / `panic!`"
- **分布**:
  - `src/interpreter/dispatch.rs`: ~80 处
  - `src/interpreter/builtins.rs`: ~60 处
  - `src/interpreter/mod.rs`: ~40 处
  - `src/typeck/mod.rs`: ~30 处
  - 其他模块: ~124 处
- **典型模式**:
  ```rust
  // dispatch.rs:496
  .unwrap_or(0)
  
  // interpreter/mod.rs:599
  .lock().map_err(|_| "globals mutex poisoned".to_string())?.get("main").clone()
  ```
- **风险**: 用户输入（如通过 `file.read_text` 传入的路径、通过 HTTP 请求传入的参数）触发 panic，导致整个解释器崩溃。
- **建议**: 系统性替换为 `?` 传播错误，或在边界处使用 `expect("有意义")` 并确保前置验证。

---

### 1.4 误报排除说明

| 原扫描发现 | 判定 | 理由 |
|-----------|------|------|
| `builtins.rs:642` — Command Injection | 误报 | `ContainerHandle::exec()` 是 Docker 容器方法，非 `std::process::Command::exec` |
| `container.rs:251,538` — Command Injection | 误报 | 251 行是方法定义，538 行是测试中的硬编码安全命令 |
| `main.rs:347` — Path Traversal | 误报 | 字符串字面量 `p"..."` 中的省略号 |
| `compress/*.rs` — Path Traversal | 误报 | 格式化字符串中的 `... [elided]` 省略号文本 |
| `sandbox/mod.rs:205-206` — Path Traversal | 误报 | 测试代码故意测试路径遍历防御 |
| 14 项 High Entropy String | 误报 | 均为格式字符串、错误消息、代码片段 |

---

### 1.5 依赖漏洞评估（受限）

> `cargo audit` 未安装，以下基于 Cargo.lock 的手动评估。

| 依赖 | 版本 | 评估 |
|------|------|------|
| `ureq` | 3.3 | 较新，无已知严重漏洞 |
| `crossbeam-channel` | 0.5.15 | 稳定，无已知严重漏洞 |
| `lopdf` | 0.42 | PDF 解析库历史上漏洞较多，建议关注 RustSec |
| `libc` | 0.2.x | 极老的 minor 版本，建议升级到最新 patch |
| `chrono` | 0.4.45 | 已修复旧版 CVE，当前版本安全 |
| `flate2` | 1.1 | 较新，无已知问题 |
| `sha2` | 0.10 | 稳定，无已知问题 |

**建议**: 安装 `cargo-audit` 后执行 `cargo audit` 获取完整依赖漏洞报告。

---

## 二、架构优化分析

### 2.1 整体架构概览

```
Lexer (925 LOC)
  → ParserV2 (2,900 LOC: mod.rs + expressions.rs + statements.rs)
    → ASTv2 (686 LOC)
      → TypeCK (3,200 LOC: mod.rs + check.rs)
        → Interpreter (12,700 LOC: 9 files)
          → builtins.rs (5,014 LOC) ← 最大文件
          → dispatch.rs (1,337 LOC)
          → execute.rs (1,044 LOC)
          → mod.rs (3,251 LOC)
```

卫星系统：LSP (1,400 LOC)、Record (1,900 LOC)、Document (2,800 LOC)、Sandbox/Schedule/Skill/Plan 等 (10,000 LOC)

---

### 2.2 五大架构摩擦点（按严重性排序）

#### 🔴 #1: `Interpreter` 上帝对象 — 30+ 字段

- **文件**: `src/interpreter/mod.rs:137-205`
- **问题**: `Interpreter` 结构体持有来自 15+ 子系统的状态：
  ```rust
  pub struct Interpreter {
      globals, environment, tool_registry, model_routes, token_budget,
      token_usage, trace, current_ai_config, trait_registry, impl_table,
      recorder, worker_channels, worker_receivers, ai_cache, string_interner,
      draft_model_stats, context_window, speculative_verifier, cache_warmer,
      v2_arena, memory_store, bus, sandbox, scheduler, ccr_store,
      mock_registry, audit_sink, markdown_memory_dir, container,
      tool_planes, skill_registry, plans, refine_registry,
  }
  ```
- **影响**:
  - 每新增一个功能（skill/plan/refine/container），必须修改此结构体
  - `Clone` 实现 43 行手动字段复制
  - 3 个构造函数（`new()` / `new_empty()` / `new_with_globals()`）各 30+ 字段初始化，极易遗漏
- **可深化性**: ⭐⭐⭐⭐⭐ — 提取领域 Facade 是最高优先级重构

**建议方案**: 按领域拆分为 facade：
```rust
struct Interpreter {
    core: CoreRuntime,          // globals, environment, v2_arena
    ai: AiRuntime,              // model_routes, context_window, speculative_verifier
    sandbox: SandboxRuntime,    // sandbox, container, audit_sink
    registry: RegistryRuntime,  // tool_planes, skill_registry, plans, refine_registry
    infra: InfraRuntime,        // bus, scheduler, ccr_store, mock_registry
}
```

---

#### 🔴 #2: `builtins.rs` — 5,014 行的方法大杂烩

- **文件**: `src/interpreter/builtins.rs`
- **问题**: 单一 `impl Interpreter` 块包含：
  - `call_file_method` (255 行) — 20+ 文件操作
  - `call_sandbox_method` (380 行) — 容器生命周期、能力令牌、审计
  - `call_ai_method` (170 行) — 重试策略、角色、上下文窗口
  - `call_schedule_method` (90 行)
  - `call_event_method` (70 行)
  - `call_memory_method`、`call_ccr_method`、`call_mock_method` 等
- **影响**:
  - **零测试覆盖**（5,014 行代码，0 个单元测试）
  - 修改任一内置方法需要重新编译整个文件
  - 不同领域的逻辑（文件 I/O vs Docker 容器 vs AI 调用）相互污染
- **可深化性**: ⭐⭐⭐⭐⭐ — 接口已自然分化为 `call_xxx_method`，只需物理拆分

**建议方案**: `src/interpreter/builtins/` 目录：
```
builtins/
  mod.rs          — 统一入口 `call_builtin(kind, method, args)`
  file.rs         — file.* 方法 (~260 LOC)
  sandbox.rs      — sandbox.* + container.* 方法 (~400 LOC)
  ai.rs           — ai.* 方法 (~200 LOC)
  schedule.rs     — schedule.* 方法 (~100 LOC)
  event.rs        — bus.* 方法 (~80 LOC)
  memory.rs       — memory.* 方法
  ccr.rs          — ccr.* 方法
  mock.rs         — mock.* 方法
```

---

#### 🔴 #3: `dispatch.rs` — 1,337 行的 `match` 爆炸

- **文件**: `src/interpreter/dispatch.rs`
- **问题**: `call_method` 对 `Value` 变体（List/Dict/Builtin/String/Stream/Agent/Router/Conversation/TraitObject）做外层 `match`，再在每个 arm 内对 `method` 字符串做内层 `match`。形成 **类型 × 方法** 的笛卡尔积。
- **影响**:
  - 新增一个类型需要编辑此文件
  - 新增一个方法需要编辑此文件
  - `List.map` 的实现与 `List.filter` 被无关的 `Dict` 方法隔开，认知负荷高
- **可深化性**: ⭐⭐⭐⭐ — 引入 trait 分发即可解决

**建议方案**: `MethodDispatch` trait：
```rust
trait MethodDispatch {
    fn dispatch(&self, method: &str, args: Vec<Value>, interp: &mut Interpreter)
        -> Result<Value, String>;
}

// 每个 Value 类别独立实现文件
impl MethodDispatch for ListValue { ... }   // dispatch/list.rs
impl MethodDispatch for DictValue { ... }   // dispatch/dict.rs
impl MethodDispatch for BuiltinValue { ... } // dispatch/builtin.rs
```

---

#### 🟡 #4: `main.rs` — 厚 CLI 混合控制器逻辑

- **文件**: `src/main.rs` (1,042 LOC)
- **问题**: 包含：
  - 260 行的 `match args[1]` 子命令分发
  - `run_record` / `run_replay` / `run_diff` / `run_snapshot` 业务逻辑
  - `format_size` / `format_ts` / `format_duration` / `truncate` 工具函数
  - `install_package` 包管理逻辑
  - MCP CLI 工具
- **影响**: 新增子命令必须修改此文件；无法单元测试 CLI 行为。
- **可深化性**: ⭐⭐⭐⭐

**建议方案**: `src/cli/` 目录：
```
cli/
  mod.rs          — 命令解析入口
  commands/
    run.rs        — mora run <file>
    check.rs      — mora --check
    record.rs     — mora record / replay / diff / snapshot
    mcp.rs        — mora mcp tool-list / tool-search
    install.rs    — mora install <url>
  format.rs       — format_size, format_ts 等工具函数
```

---

#### 🟡 #5: `parser_v2/statements.rs` — 1,696 行的语句解析器

- **文件**: `src/parser_v2/statements.rs`
- **问题**: 25+ 种语句的解析逻辑（`let`、`task`、`if`、`for`、`match`、`with`、`transaction`、`trait`、`impl`、`orchestrate`、`skill`、`prompt`、`document` 等）全部在一个 `impl ParserV2` 块中。
- **影响**: `ParserV2::declaration()`（`mod.rs:65-163`）是 100 行的 `if/else` 链检查 40+ token 类型；新增语句类型需同时改 `mod.rs` 和 `statements.rs`。
- **可深化性**: ⭐⭐⭐

**建议方案**: 按领域分组：
```
parser_v2/stmts/
  mod.rs          — 声明路由
  control.rs      — if, for, match, with, parallel
  definitions.rs  — task, trait, impl, type, enum, struct
  orchestrate.rs  — orchestrate, skill, eval
  resources.rs    — prompt, document, transaction
```

---

### 2.3 测试覆盖率缺口

| 模块 | LOC | 测试数 | 风险 |
|------|-----|--------|------|
| `interpreter/builtins.rs` | 5,014 | **0** | 🔴 最高 — 所有内置副作用无测试 |
| `interpreter/dispatch.rs` | 1,337 | **0** | 🔴 核心分发逻辑无测试 |
| `interpreter/execute.rs` | 1,044 | **0** | 🟡 语句执行 |
| `interpreter/ai_chat.rs` | 834 | **0** | 🟡 网络依赖，但 mock 路径可测 |
| `parser_v2/statements.rs` | 1,696 | **0** | 🔴 所有语句解析无测试 |
| `typeck/check.rs` | 1,174 | **0** | 🔴 类型检查核心 |
| `interpreter/mod.rs` | 3,251 | ~137 | 🟢 数量尚可，但集中在相似度函数 |

**核心问题**: 从 Parser → TypeCK → Interpreter builtins → dispatch 的执行主路径几乎没有任何单元测试。这是 ~12,000 LOC 的零测试盲区。

---

### 2.4 浅模块识别

| 模块 | 接口 | 问题 |
|------|------|------|
| `lsp/mod.rs` | 22 行，仅 re-export | 无行为，纯命名空间管道 |
| `document/backend/mod.rs` | 9 行 | 空 re-export 壳 |
| `lsp/providers/mod.rs` | 22 行 | 仅 re-export 8 个 provider |
| `common.rs` | 76 行 | `Span`/`BinaryOp`/`Literal` 被到处使用但未抽象化 |
| `trace_collector.rs` | 268 行 | `Vec` + `HashMap` 的薄包装 |

这些浅模块不是紧急问题，但增加了目录层级而无实际抽象收益。

---

## 三、优先修复建议

### 立即执行（本周内）

1. **修复 CORS 通配符** (`http_server.rs:468`)
   - 将 `*` 改为可配置来源，或默认仅 `localhost`

2. **修复默认绑定地址** (`dispatch.rs:1009`)
   - 默认值 `"0.0.0.0:3000"` → `"127.0.0.1:3000"`

3. **安装 cargo-audit 并运行依赖扫描**
   ```bash
   cargo install cargo-audit
   cargo audit
   ```

### 短期执行（本月内）

4. **制定 unwrap() 清除计划**
   - 按模块分批替换为 `?` 或 `expect("有语义")`
   - 优先处理 interpreter/ 和 typeck/ 中的 unwrap

5. **拆分 `builtins.rs`**
   - 这是最大文件（5,014 LOC），拆分后单文件降至 200-400 LOC
   - 接口不变：`Interpreter::call_builtin(kind, method, args)`

6. **提取 `Interpreter` 领域 Facade**
   - 将 30+ 字段聚合为 5-6 个领域 facade
   - 每个 facade 可独立实例化和测试

### 中期执行（下季度）

7. **重构 `dispatch.rs` 为 trait 分发**
8. **拆分 `main.rs` 为 `cli/` 模块**
9. **为核心执行路径补充单元测试**
   - 目标: parser → typeck → interpreter builtins → dispatch 主路径有基本覆盖

---

## 四、附录

### A. 扫描工具信息

- **安全扫描**: 自定义 Python 扫描器（正则 + Shannon 熵值 + OWASP 模式）
- **架构分析**: 静态代码分析 + 子代理深度探索
- **依赖审计**: 受限（`cargo audit` 未安装）

### B. 参考文件

- `AGENTS.md` — 项目编码规范（unwrap/panic 禁令来源）
- `src/interpreter/mod.rs:137-205` — Interpreter 结构体定义
- `src/interpreter/builtins.rs` — 5,014 行内置方法
- `src/interpreter/dispatch.rs:1009-1010` — 0.0.0.0 默认绑定
- `src/http_server.rs:468` — CORS 通配符
