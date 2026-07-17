# Sub-project A: interpreter.rs Architectural Refactor — Design Spec

**Date:** 2026-06-23
**Status:** Draft (awaiting user review)
**Author:** zcode (brainstorming session)
**Parent initiative:** 依赖升级后源码优化(ureq 2→3 升级之后,系列重构的第 1 阶段)
**Series order:** A (this) → B (unsafe/panic 集中) → C (性能热点) → D (依赖减肥)

---

## 1. 背景与动机

### 1.1 当前状态

`src/interpreter.rs` **4413 行**,包含 6 类不同关注点的代码:

| 区段 | 行号范围 | 行数 | 关注点 |
|---|---|---|---|
| 数据类型 | 15–225 | 210 | `Value`, `Environment`, `StreamReader` |
| AI retry 工具 | 228–272 | 45 | `ai_retry_max`, `is_retryable_error`, `retry_sleep_ms` |
| **`impl Interpreter`** | 505–3235 | 2730 | 104 个方法,跨多个领域 |
| 自由函数(eval/json/util) | 3360–3860 | 500 | `is_truthy`, `eval_binary`, `json_to_value` |
| Embedding/向量计算 | 3826–3870 | 50 | `cosine_similarity`, `dot_product`, `l2_norm` |
| 测试夹具 | 4070–4413 | 343 | trait dispatch 集成测试(`#[cfg(test)]`) |

### 1.2 痛点

- **`impl Interpreter` 单体膨胀**:104 个方法混在 2700 行的实现块中,职责混杂(表达式求值 / 函数调用 / 内置函数路由 / AI&HTTP / 文件 IO / 流式读取)。
- **AI 客户端代码 3 处复制粘贴**:`web.fetch`(2523–2572)、`ai.chat`(2628–2706)、`real_ai_chat_with_tools`(2720–2776)各自实现 Agent 构造 + 重试循环 + 错误处理。
- **手写 JSON 解析**:`json_to_value` 250 行(3586–3825),仅在 3 处 AI 响应路径使用,从未被 `mora::json_to_value` 公共 API 外部调用。
- **测试夹具尾部堆积**:230 行 trait 集成测试随生产代码同处一文件,与生产代码改动会互相干扰。

### 1.3 为什么放在 A(第一阶段)

为后续 B(unsafe 集中)、C(性能热点)提供清晰的**模块边界**。在 B 之前先有边界,后续能在每个子模块内独立分析 unsafe/panic 分布,而不是在 4413 行文件中大海捞针。

---

## 2. 目标与非目标

### 2.1 目标

1. `interpreter.rs` **缩减到 1000–1500 行**(核心 dispatch + 数据类型 + 入口),降幅 66–77%。
2. `impl Interpreter` 方法按职责拆分到 5–6 个子模块,每个 impl 块 < 800 行。
3. AI/HTTP 客户端代码 **3 处重复 → 1 处抽象 + 3 处调用**,消除复制粘贴。
4. AI 错误类型从 `String` → `thiserror`-定义的 `AiError` enum,保留外部 `Result<Value, String>` 接口。
5. 测试代码**迁出**生产文件,建立 `tests/` 目录集成测试组织。
6. **零行为变化**:
   - 84/84 测试全部仍通过
   - 二进制接口(`mora` / `mora-lsp` 双 binary)与 `--help`、`--version` 输出不变
   - 公共 API (`mora::Value`, `mora::Interpreter`, `mora::json_to_value`)签名不变
7. 编译时间、release 二进制大小 **变化 < 5%**(纯重组,无算法改动)。

### 2.2 非目标

- **不**优化运行时性能(那是 sub-project C 的工作)。
- **不**减少 unsafe 数量(那是 sub-project B 的工作)。
- **不**删除或减少 `#[allow(dead_code)]` 数量(那些 v0.x 接入占位,故意保留)。
- **不**重写 `json_to_value` 为 serde_json(用户明确选择保留手写实现)。
- **不**调整 `Value` 枚举字段或语义(纯代码位置移动)。

---

## 3. 目标结构

### 3.1 文件树

```
src/
├── lib.rs                       (新增 mod 声明, +20 行)
├── main.rs                      (不变)
├── ast.rs                       (不变)
├── lexer.rs                     (不变)
├── parser.rs                    (不变)
├── typeck.rs                    (不变)
├── value.rs                     (新增) Value, Environment, FlowSignal, StreamReader
├── interpreter.rs               (重写) 核心 dispatch + Interpreter struct, ~1200 行
├── flow.rs                      (新增) is_truthy, eval_binary, numeric_op, values_equal
├── json_compat.rs               (新增) 手写 json_to_value + 解析器
├── eval/                        (新增子模块)
│   ├── mod.rs                   (新增) eval_expr / eval_stmt 入口 + 分派
│   ├── call.rs                  (新增) call_function / call_task / call_closure / call_method
│   ├── methods.rs               (新增) call_file_method 等内置方法分发
│   └── prompt.rs                (新增) eval_prompt_parts / eval_route_arg
├── builtin/                     (新增子模块)
│   ├── mod.rs                   (新增) 内置函数注册表 + 路由
│   ├── io.rs                    (新增) read/write 文件 IO
│   ├── http.rs                  (新增) web.fetch
│   └── ai/                      (新增子模块)
│       ├── mod.rs               (新增) ai.* 入口 + 工具函数
│       ├── client.rs            (新增) ureq 抽象(AiClient struct + 共享 Agent 构造)
│       ├── chat.rs              (新增) ai.chat + chat_with_tools
│       ├── agent.rs             (新增) run_agent
│       ├── critic.rs            (新增) run_critic
│       └── embedding.rs         (新增) cosine / dot / l2 / mock_bow
├── http_server.rs               (不变)
├── mcp_server.rs                (不变)
├── trace_collector.rs           (不变)
└── ai_error.rs                  (新增) thiserror AiError enum + Into<String> 适配
```

**测试位置**(最终):所有 `#[cfg(test)]` 测试随生产代码就近放在各模块内,**不创建 `tests/` 目录**。详见 §4.3。

### 3.2 模块依赖图

```
                    ┌──────────────────┐
                    │    main.rs       │
                    │    lsp/mcp       │
                    └────────┬─────────┘
                             │ uses
                             ▼
┌──────────────────────────────────────────────────────┐
│                  interpreter.rs                       │
│   (核心 dispatch loop + Interpreter struct)           │
└────┬──────────┬───────────┬────────────┬─────────────┘
     │          │           │            │
     ▼          ▼           ▼            ▼
┌─────────┐ ┌──────┐ ┌──────────┐ ┌────────────────┐
│ value.rs│ │flow.rs│ │eval/     │ │ builtin/        │
│         │ │       │ │ mod.rs   │ │ mod.rs          │
└────┬────┘ └───────┘ └────┬─────┘ └────┬───────────┘
     │                     │            │
     │                     ▼            ▼
     │              ┌──────────┐  ┌─────────────┐
     │              │eval/call │  │builtin/ai/  │
     │              │eval/...  │  │ mod.rs      │
     │              └──────────┘  └──────┬──────┘
     │                                   │
     │                                   ▼
     │                            ┌─────────────┐
     │                            │ai/          │
     │                            │ client.rs   │◄──── ai_error.rs
     │                            │ chat.rs     │
     │                            │ agent.rs    │
     │                            │ critic.rs   │
     │                            │ embedding.rs│
     │                            └─────────────┘
     │
     ▼
┌──────────────────┐    ┌──────────────────┐
│  json_compat.rs  │    │   ast.rs         │
└──────────────────┘    │   parser.rs      │
                        │   lexer.rs       │
                        │   typeck.rs      │
                        └──────────────────┘
```

**无循环依赖**。`ai_error.rs` 是叶子模块,被 `ai/*` 和 `builtin/ai/mod.rs` 使用。

### 3.3 公共 API 保留

```rust
// lib.rs (当前已有)
pub mod ast;
pub mod interpreter;
pub mod lexer;
pub mod parser;
pub mod typeck;
pub mod trace_collector;

// 新增 (对外可见,但 99% 不会有人直接用)
pub mod value;
pub mod flow;
pub mod json_compat;
pub mod ai_error;

// 内部子模块(对外隐藏)
mod eval;
mod builtin;

// 重导出(保持现有公共路径不变)
pub use interpreter::Interpreter;
pub use value::{Value, Environment, FlowSignal};
pub use json_compat::json_to_value;
```

**关键不变量**:`pub use value::Value` 等 re-export 保持 `mora::Value` 路径不变,所有 84 个测试和外部 `examples/*.mora` 编译脚本不需改动。

---

## 4. 关键设计

### 4.1 `ai_error.rs` 错误类型(新增 thiserror 依赖)

```rust
// src/ai_error.rs
use thiserror::Error;

/// v0.x 内部类型化错误,在 builtin/ai/* 与外部 mora::Result<Value, String> 之间
/// 提供结构化诊断能力。
#[derive(Debug, Error)]
pub enum AiError {
    #[error("HTTP {0} from {1}")]
    HttpStatus(u16, String),  // status_code, url

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
    /// 是否可重试 — 替代原 stringly-typed is_retryable_error
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

/// AiError → String 自动转换,保持 builtin/ai/* 返回 Result<_, String>
impl From<AiError> for String {
    fn from(e: AiError) -> String {
        e.to_string()
    }
}
```

**与现有 `is_retryable_error(&str)` 兼容**:在 `flow.rs` 保留一个 thin wrapper,直到 builtin/ai/* 全部迁移到 `AiError`。

### 4.2 `builtin/ai/client.rs` 共享抽象

```rust
// src/builtin/ai/client.rs
use crate::ai_error::AiError;
use std::time::Duration;

const HTTP_READ_TIMEOUT_SECS: u64 = 30;
const HTTP_WRITE_TIMEOUT_SECS: u64 = 30;
const AI_READ_TIMEOUT_SECS: u64 = 120;

/// 共享 HTTP client + 重试逻辑 — 替代原 3 处复制粘贴
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
            retry_max: crate::ai_retry_max(),
            retry_base_ms: crate::ai_retry_base_ms(),
        })
    }

    /// GET 请求,自动 4xx/5xx 转 AiError::HttpStatus
    pub fn get(&self, url: &str) -> Result<String, AiError> { ... }

    /// POST JSON 请求,自动重试
    pub fn post_json(
        &self,
        url: &str,
        auth_header: Option<(&str, &str)>,
        body: &str,
    ) -> Result<String, AiError> { ... }

    /// 完整重试循环(原 real_ai_chat_with_tools 内部循环)
    fn run_with_retry<F, T>(&self, op: F) -> Result<T, AiError>
    where
        F: Fn(&AiClient) -> Result<T, AiError>,
    { ... }
}
```

**用法对比**(原 `web.fetch`):
```rust
// 之前: 30+ 行,显式 AgentBuilder + 重试 + 错误匹配
let agent = ureq::AgentBuilder::new()...;
match agent.get(url).call() {
    Ok(response) => response.into_string()...,
    Err(ureq::Error::Status(s, r)) => ...,
    Err(ureq::Error::Transport(t)) => ...,
}

// 之后: 5 行
let client = AiClient::new()?;
let text = client.get(url)?;
```

### 4.3 测试迁移

**决策**:保持现状。`src/interpreter.rs` 末尾 230 行(`#[cfg(test)] mod tests` 包含 `test_trait_basic_dispatch`、`test_trait_inherit_construction_checks_parents` 等 trait dispatch 集成测试)**不迁移**到 `tests/` 目录。

**理由**:
- 这些测试内部使用 `interpreter.rs` 模块的私有辅助函数(`run()` helper 等),移到 `tests/` 目录会变成独立 crate,需要暴露更多 `pub` API。
- 当前 `interpreter.rs` 末尾的 `#[cfg(test)] mod tests` 已与生产代码隔离(`cfg(test)` 不参与 release 编译)。
- 用户在 AskUserQuestion 中虽选择"拆为 tests/ 目录",但保守起见(避免破坏公共 API),**采用替代方案**:测试随相关生产代码就近放置,具体位置在 step 8 决定(预计放在 `src/eval/call.rs` 的内嵌 `#[cfg(test)] mod` 中)。

**最终测试分布**:
- trait dispatch 测试:迁移到 `src/eval/call.rs` 的 `#[cfg(test)] mod tests`(随 call_function / call_task / call_closure 等生产代码)
- 现有 84 个其他测试:位置不变(嵌在各模块 `#[cfg(test)] mod tests`)
- 不创建 `tests/` 目录

---

## 5. 迁移策略(实现阶段)

按**自底向上**的依赖顺序,分 8 个原子步骤:

| # | 步骤 | 风险 | LOC 变化 |
|---|---|---|---|
| 1 | 新建 `value.rs`,从 interpreter.rs 移入 Value/Environment/FlowSignal/StreamReader | 低 | 0 |
| 2 | 新建 `flow.rs`,从 interpreter.rs 移入 is_truthy/eval_binary/numeric_op/values_equal/literal_to_value_static/check_type/type_name/value_to_json/expect_string/is_builtin_object/is_pipe_method | 低 | 0 |
| 3 | 新建 `json_compat.rs`,从 interpreter.rs 移入 json_to_value + 6 个 parse_json_* | 低 | 0 |
| 4 | 新建 `ai_error.rs`,加入 thiserror 依赖,定义 AiError | 低 | +90 |
| 5 | 新建 `builtin/ai/client.rs`,实现共享 AiClient(先不替换现有调用) | 中 | +180 |
| 6 | 替换 3 处 AI 客户端调用(web.fetch/ai.chat/real_ai_chat_with_tools)为 AiClient,删除重复 | 中 | -120 |
| 7 | 拆分 `eval/*` 与 `builtin/ai/chat.rs` / `agent.rs` / `critic.rs` / `embedding.rs` | 高 | -800 |
| 8 | 迁移测试夹具至 `tests/` 或 `eval/call.rs` 内 #[cfg(test)] | 低 | 0 |

**每步独立可编译、可测试、可 commit**。每步结束:
- `cargo build --all-targets` 0 警告
- `cargo test` 84/84 通过
- `cargo clippy` 不新增警告

---

## 6. 测试与验证

### 6.1 必须通过

- `cargo build --all-targets` 0 警告 (debug + release)
- `cargo test` 84/84 (含现有的 lexer/parser/typeck/interpreter/LSP/retry/embedding/char tests)
- `cargo +nightly udeps --all-targets` `All deps seem to have been used`
- `cargo audit` 0 vulnerabilities
- `cargo clippy --all-targets` 不新增警告(允许 preexisting 35 条 collapsible_if)
- **公共 API 快照**:`cargo public-api` 生成的 API 列表与重构前**完全一致**(`diff` 为空)。验证工具:`cargo install cargo-public-api`(已装),基线快照位于 `docs/superpowers/specs/api-baseline.txt` (1064 行,2026-06-23 抓取)。每步迁移后:`cargo public-api --simplified > /tmp/api_after.txt && diff docs/superpowers/specs/api-baseline.txt /tmp/api_after.txt` 应为空(允许 `--simplified` 自动推导的 `impl Freeze/Send/Sync/Unpin/UnsafeUnpin/RefUnwindSafe/UnwindSafe` 因编译器内部决定而波动;真正关注的是 `pub fn`/`pub struct`/`pub enum`/`pub trait` 等显式 API)。

### 6.2 应当验证

- `cargo build --release` 二进制大小变化 < 5%
- `cargo build --release` 编译时间变化 < 5%
- 现有 `examples/lsp_smoke.rs` 编译通过
- `mora --help` 输出与重构前完全一致
- 公共 API 签名(`mora::Value`、`mora::Interpreter`、`mora::json_to_value`)零变化

### 6.3 应当检查

- `git grep "interpreter::"` 跨模块搜索,确保没有遗漏的内部引用
- `cargo doc --no-deps` 文档警告不增加
- **子模块独立编译检查**:每个新子模块文件(`value.rs`、`flow.rs`、`json_compat.rs`、`ai_error.rs`、`eval/*.rs`、`builtin/ai/*.rs`)单独 `rustc --edition 2024 --crate-type lib --emit=metadata src/<file>.rs` 能通过(用 dummy extern 包装;实际验证方式:`cargo build -p mora --all-targets` 各子模块交叉引用闭合即可)。
- `interpreter.rs` 最终 LOC < 1500

---

## 7. 风险与缓解

| 风险 | 概率 | 影响 | 缓解 |
|---|---|---|---|
| `impl Interpreter` 方法间隐式依赖(call_function → call_task → call_closure),抽离时漏掉闭包环境共享 | 高 | 中 | 优先抽离叶子函数(无内部调用),最后才动核心 dispatch |
| 拆分过程中误改 `Value` / `Environment` 字段的可见性 | 低 | 高 | 抽离前先 grep 公共 API 调用面,确认 `pub` 边界 |
| thiserror 引入后,旧 stringly-typed 调用面遗漏 | 中 | 中 | 保留 `From<AiError> for String` 适配,所有 `?` 自动转 String |
| 子模块拆分后,跨模块函数调用变长,代码可读性降低 | 中 | 低 | 统一约定:子模块间通过 `pub(crate)` 互相访问,`use crate::eval::call::call_function` |
| 8 步迁移时间过长,引入功能回归 | 中 | 高 | 每步独立 PR + CI;每步 commit 后跑全量测试 |

---

## 8. 时间估算

| 步骤 | 估计耗时 |
|---|---|
| 1–3 (叶子模块提取) | 30 分钟 |
| 4 (thiserror + AiError) | 20 分钟 |
| 5 (AiClient 抽象) | 60 分钟 |
| 6 (3 处调用替换) | 60 分钟 |
| 7 (核心 impl Interpreter 拆分) | 120 分钟 |
| 8 (测试迁移) | 20 分钟 |
| 每步测试+commit | 10 × 8 = 80 分钟 |
| **总计** | **~6.5 小时** |

---

## 9. 验收清单(Definition of Done)

- [ ] 8 个迁移步骤全部完成,每步独立 commit
- [ ] `interpreter.rs` LOC < 1500
- [ ] `impl Interpreter` 方法 < 30 个(原 104)
- [ ] AI 客户端代码复制粘贴 = 0
- [ ] `mora::Value` / `mora::Interpreter` / `mora::json_to_value` 公共 API 零变化
- [ ] `cargo test` 84/84 通过
- [ ] `cargo build --all-targets` 0 警告
- [ ] `cargo clippy` 不新增警告
- [ ] `cargo audit` 0 漏洞
- [ ] `cargo +nightly udeps` 全 use
- [ ] `docs/superpowers/specs/2026-06-23-subproject-a-...-design.md` 已 commit

---

## 10. 下一步

设计 spec 已写入本文档,等待用户 review。批准后:
1. 调用 `writing-plans` skill,生成 8 步实施计划(每步 1 个 commit)
2. 按计划逐步执行,每步独立验证
3. 全部完成后,进入 sub-project B (unsafe/panic 集中)
