# Mora 语言 Agent 原语补充研究 — 6 个新项目综合报告

> **研究日期**: 2026-07-07  
> **覆盖项目**: 6 个（google/agents-cli, langchain-ai/langchain, langchain-ai/langgraph, langgraph4j/langgraph4j, luochang212/dive-into-langgraph, OpenBMB/ChatDev）  
> **对比基准**: 已有 17 项目研究（RESEARCH_PRIMITIVES_MASTER_v2.md, v0.41-v0.48 已实施）  
> **目标**: 识别 mora 语言尚缺少的**功能性**与**互补性**特性/原语

---

## 0. 执行摘要

本次研究覆盖 6 个重量级 Agent 项目（累计 170k+ GitHub Stars）。与已有 17 项目研究交叉比对后，**提取出 12 项对 mora 语言有补充价值的原语建议**，其中 **P0（高优先级）5 项，P1（中优先级）4 项，P2（研究性）3 项**。

核心发现：
- **LangChain/LangGraph 生态**（90k+35k stars）是当前生产级 Agent 编排的事实标准，其 **Pregel BSP 执行模型、Checkpoint 持久化、Command 动态控制流、Channel+Reducer 状态聚合** 是 mora `orchestrate` 原语目前最缺失的底层机制。
- **agents-cli**（4.2k stars，Google 官方）代表 **Agent 的 DevOps 化**趋势——Eval-first 质量门控、8 阶段生命周期、Skill-as-Code 知识包。这是 mora 工具链方向的关键参考。
- **ChatDev**（33.7k stars）展示了**角色扮演 + 零代码编排**的演进路径——动态边展开（Map/Tree）、阶段感知记忆挂载、SKILL.md 行为知识加载。这是 mora 应用层语义的重要补充。
- **dive-into-langgraph**（教程）提炼了**中间件管道、三层上下文分离、动态提示词**等模式化最佳实践。

---

## 1. 六项目核心发现速览

### 1.1 google/agents-cli — Agent 的 DevOps 工具链

| 维度 | 核心机制 | 对 Mora 的价值 |
|------|----------|---------------|
| **生命周期** | 8 阶段闭环（Spec → Scaffold → Build → Orchestrate → Eval → Deploy → Publish → Observe） | mora 工具链应内置 `mora init` / `mora eval` / `mora deploy` 命令 |
| **编排** | ADK Workflow 显式图 + 条件路由 + JoinNode + 并行 Worker | `workflow` 原语可补充 fan-out/fan-in 语义 |
| **评估** | Eval-first 质量门控（LLM-as-judge + 自定义指标 + 对比优化） | `eval` 作为一等原语，与 `test` 区分 |
| **状态** | State Prefix Namespace（session/user/app/temp 四级） | `state` 作用域前缀语义 |
| **安全** | Tool Confirmation Gates（条件确认 + 运行时 IAM 强制） | `capability` 门控增强 |
| **恢复** | Session Rewind（精确回滚到指定 invocation） | `rewind` / `replay` 增强 |
| **后台** | Ambient Agent（Pub/Sub + Cron 触发 + 指数退避） | `ambient` 事件驱动原语 |
| **跨 Agent** | A2A Protocol + Agent Card 发现机制 | `a2a` 跨框架通信原语（实验性） |

### 1.2 langchain-ai/langchain — 分层编排平台

| 维度 | 核心机制 | 对 Mora 的价值 |
|------|----------|---------------|
| **统一接口** | `Runnable` 四模式（invoke/batch/stream/ainvoke）+ `\|` 管道 | 编译时类型推导，比 Python 运行时更安全 |
| **数据流** | Message-Centric，`content` 支持 `str` + `list[dict]` 内容块 | `ai.chat` 返回多模态内容块的原生支持 |
| **Schema 推导** | `InjectedToolArg` 参数对 LLM 不可见 | `injected` 关键字：工具参数对 LLM schema 隐藏 |
| **执行策略** | Retry/Timeout/Cache 策略每节点可配置 | `@retry` / `@timeout` / `@cache` 注解 |

### 1.3 langchain-ai/langgraph — 有状态图编排框架

| 维度 | 核心机制 | 对 Mora 的价值 |
|------|----------|---------------|
| **执行模型** | Pregel BSP（Bulk Synchronous Parallel）：步骤内写入不可见，天然并行安全 | `orchestrate` 的并发语义基石 |
| **状态通道** | `Channel` + `Reducer`（LastValue/Topic/BinaryOperatorAggregate） | `@reduce(append\|add\|last\|custom)` 注解 |
| **检查点** | 步骤级状态持久化，版本化状态（`channel_values` + `versions_seen`） | `orchestrate @checkpoint(saver: ...)` |
| **控制流** | `Command(goto=..., update=..., resume=...)` 节点内嵌路由 | `command` 返回类型，打破静态图限制 |
| **中断恢复** | `interrupt()` + `Command(resume=...)`，多中断顺序匹配 | `interrupt` 原语增强（v0.34 已有基础） |
| **动态派发** | `Send` 实现 Map-Reduce 式动态并行 | `spawn` / `send` 动态任务生成 |
| **时间旅行** | `get_state()` / `update_state()` 回溯任意历史快照 | `mora replay --fork` 语义 |
| **子图** | 子图独立 `checkpoint_ns` 命名空间，编译期扁平化 | `subgraph` 嵌套编排 |

### 1.4 langgraph4j/langgraph4j — Java 移植验证

| 维度 | 核心机制 | 对 Mora 的价值 |
|------|----------|---------------|
| **Schema 注解** | `Channel` 定义状态键的更新语义（覆盖/累加/自定义） | 验证 LangGraph 的 Channel+Reducer 是**跨语言共识** |
| **持久化后端** | Memory/MySQL/PostgreSQL/Redis/DynamoDB 等 8 种 | mora 检查点应有**存储后端抽象** |
| **Interrupt** | `CompileConfig.interruptsBefore()` + `InterruptionMetadata` | 中断是**编译期配置**而非运行时异常 |
| **Subgraph 扁平化** | `ProcessedNodesEdgesAndConfig.process()` 编译期展开 | 运行时零开销嵌套 |

### 1.5 luochang212/dive-into-langgraph — 模式化最佳实践

| 维度 | 核心机制 | 对 Mora 的价值 |
|------|----------|---------------|
| **中间件管道** | `@before_model`, `@wrap_model_call`, `@dynamic_prompt` 等 8 种装饰器 | `middleware` 声明式拦截管道 |
| **三层上下文** | Runtime（请求共享）/ State（节点传递）/ Store（持久化） | 显式上下文分层类型系统 |
| **动态提示词** | `@dynamic_prompt` 基于 State/Store 动态生成 system prompt | `dynamic_prompt` 模板机制 |
| **上下文压缩** | `SummarizationMiddleware` 超限时自动摘要 | `context_policy { on_overflow: summarize }` |
| **MCP 工程** | `supervisord` 守护 MCP 进程（自动重启） | 服务进程管理原语 |

### 1.6 OpenBMB/ChatDev — 角色扮演与零代码编排

| 维度 | 核心机制 | 对 Mora 的价值 |
|------|----------|---------------|
| **动态边展开** | `edge` 声明 `dynamic_config: {type: map\|tree, split: ...}` | `edge ... { dynamic: map, split: by_line }` |
| **阶段感知记忆** | `retrieve_stage: [pre_gen, gen, post_gen, finished]` | `memory: store { retrieve_at: [...], write_at: ... }` |
| **复合编排** | `phase` + `break_cycle` 用户可覆盖的循环终止 | `phase ... { break_when: ..., max_iterations: 5 }` |
| **Skill 加载** | `activate_skill` 从 `.agents/skills/<name>/SKILL.md` 动态加载 | `skills: ["deep-research"]` 自动发现 |
| **Human 节点** | `human` 节点一等公民，资源信号量限制并发 | `node Review = human { ... }` |
| **重试策略 DSL** | status_code / exception_type / error_substring / non_retryable 多层匹配 | `retry_policy { retry_on_status: [429, 503] }` |
| **自循环** | `pseudo_edge` + `context_window` 上下文窗口管理 | `context_window: 5, self_loop: true` |
| **千级 Agent** | MacNet 拓扑生成 + 分层聚合 | 未来扩展点 |
| **RL 编排** | Puppeteer 中央编排器学习激活顺序 | 研究性质 |

---

## 2. 与已有 17 项目的交叉比对（去重与增强）

### 2.1 已实施的原语（v0.41-v0.48，不复述）

| 原语 | 来源 | 状态 |
|------|------|------|
| `capability` / `policy` / `audit` | loongclaw | ✅ 已实施 |
| `exec.bash` + 进程组隔离 | mini-swe-agent | ✅ 已实施 |
| `ai.retry` | mini-swe-agent | ✅ 已实施 |
| `interrupt` (5 种结构化类型) | mini-swe-agent | ✅ 已实施 |
| `3-mode` (human/confirm/yolo) | mini-swe-agent | ✅ 已实施 |
| `SKILL.md` 双注册表 | CLI-Anything | ✅ 已实施 |
| `tool_conflict_map` | AIOS | ✅ 已实施 |
| `mimiclaw` Cron + ReAct | mimiclaw | ✅ 已实施 |
| `sandbox` 容器化 | 多项目 | ✅ 已实施 |
| `observe` / `span` / `record_tokens` | 多项目 | ✅ 已实施 |
| `record` / `replay` / `diff` | 多项目 | ✅ 已实施 |
| `orchestrate` 基础图编排 | 多项目 | ✅ 已实施 |
| `refine` 对话优化 | 多项目 | ✅ 已实施 |
| `semaphore` | 多项目 | ✅ 已实施 |

### 2.2 本次新发现的补充项（去重后）

通过 6 新项目 × 多维度分析，与已有 17 项目研究对比，**以下机制是此前研究未覆盖或覆盖不足的**：

#### 全新发现（6 项目独有）

| # | 机制 | 来源 | 已有研究覆盖度 |
|---|------|------|-------------|
| 1 | **Pregel BSP 执行模型** | LangGraph | ❌ 未覆盖（AIOS 是调度器，非图执行引擎） |
| 2 | **Channel + Reducer 状态聚合** | LangGraph / LangGraph4j | ❌ 未覆盖（loongclaw 有状态但无聚合语义） |
| 3 | **Command 动态控制流** | LangGraph | ❌ 未覆盖（mini-swe-agent 有 interrupt 但无内嵌路由） |
| 4 | **Send 动态 Map-Reduce** | LangGraph | ❌ 未覆盖 |
| 5 | **Checkpoint 步骤级版本化** | LangGraph / LangGraph4j | ⚠️ 部分覆盖（`record` 是录制，`checkpoint` 是版本化持久化） |
| 6 | **Eval-first 质量门控** | agents-cli | ❌ 未覆盖（已有测试，但无 Agent 行为评估） |
| 7 | **A2A 跨 Agent 协议** | agents-cli | ❌ 未覆盖 |
| 8 | **State Prefix Namespace** | agents-cli | ❌ 未覆盖 |
| 9 | **Session Rewind 精确回滚** | agents-cli | ⚠️ 部分覆盖（`replay` 是全量重放，`rewind` 是精确回滚） |
| 10 | **Ambient Agent 事件驱动** | agents-cli | ❌ 未覆盖 |
| 11 | **动态边展开（Map/Tree）** | ChatDev | ❌ 未覆盖 |
| 12 | **阶段感知记忆挂载** | ChatDev | ❌ 未覆盖 |
| 13 | **Middleware 拦截管道** | dive-into-langgraph | ❌ 未覆盖 |
| 14 | **三层上下文分离** | dive-into-langgraph | ⚠️ 部分覆盖（有环境/作用域，但无显式分层） |
| 15 | **动态提示词生成** | dive-into-langgraph | ❌ 未覆盖 |
| 16 | **上下文压缩策略** | dive-into-langgraph | ❌ 未覆盖 |
| 17 | **InjectedToolArg 隐藏参数** | LangChain | ❌ 未覆盖 |
| 18 | **human 节点一等公民** | ChatDev | ⚠️ 部分覆盖（3-mode 有 human 模式，但非图节点） |
| 19 | **Skill 动态加载** | ChatDev | ⚠️ 部分覆盖（有 SKILL.md 格式，但无运行时 `activate_skill`） |
| 20 | **Retry/Timeout/Cache 策略注解** | LangGraph / LangChain | ⚠️ 部分覆盖（有 `ai.retry`，但无节点级策略） |

### 2.3 合并后的统一原语建议

将 20 个新发现按**语义相关性**聚类为 12 项原语建议：

---

## 3. 12 项原语建议（按优先级排序）

### P0-1: 状态通道 + Reducer（State Channel + Reducer）

**来源**: LangGraph, LangGraph4j, ChatDev  
**问题**: 当前 `orchestrate` 的节点写入共享状态是**全量覆盖**，并行节点竞争写入同一键时无定义行为。  
**建议**: 引入状态 Schema 声明，为每个键指定更新语义。

```mora
// 当前（问题）：并行节点都写 messages，结果不可预测
orchestrate my_flow {
  state: { messages: [Message] }
  node A -> messages = [...]   // 覆盖
  node B -> messages = [...]   // 覆盖（竞争！）
}

// 建议：声明式 Reducer
orchestrate my_flow {
  state: {
    messages: [Message] @append,      // 并行写入 = 追加合并
    total_cost: number @add,          // 并行写入 = 数值相加
    last_decision: string @last,      // 并行写入 = 取最后一个（默认）
    context: Context @merge(fn(old, new) -> ...)
  }
  node A -> messages = [...]   // 追加到列表
  node B -> messages = [...]   // 追加到列表（安全！）
}
```

**为什么 P0**: 这是并行安全的基础机制。没有它，任何并行编排都不可信。LangGraph 的 Pregel BSP 模型依赖此机制实现步骤内写入隔离。

---

### P0-2: 检查点持久化（Checkpoint Persistence）

**来源**: LangGraph, LangGraph4j, agents-cli  
**问题**: 当前 `record` 是**录制**（用于回归测试），`replay` 是**重放**。但缺少**步骤级状态版本化**和**故障恢复**。  
**建议**: 将 `checkpoint` 作为 `orchestrate` 的核心基础设施，默认自动保存，支持持久化后端抽象。

```mora
// 声明式检查点
orchestrate booking_flow @checkpoint(saver: "sqlite", thread: "user_123") {
  state: { ... }
  node check_availability
  node confirm_booking
  interrupt before confirm_booking  // 暂停，等待用户确认
}

// 恢复执行
let result = booking_flow.resume(thread: "user_123", as_of: "confirm_booking")

// 时间旅行：回溯到任意步骤
booking_flow.update_state(thread: "user_123", as_of: "check_availability", {
  dates: ["2026-08-01"]
})

// 精确回滚（agents-cli 的 rewind）
booking_flow.rewind(before_invocation: "confirm_booking")
```

**为什么 P0**: 长流程（审批、多轮对话、复杂任务）的故障恢复和 Human-in-the-Loop 是刚需。LangGraph 的 checkpoint 是其最核心的差异化特性。

---

### P0-3: Command 动态控制流（Command Dynamic Control Flow）

**来源**: LangGraph, ChatDev  
**问题**: 当前 `orchestrate` 的边是**静态**的，编译后不可变。节点无法动态决定下一跳。  
**建议**: 允许节点返回 `Command` 类型，同时携带状态更新和路由决策。

```mora
// 静态边（当前）
orchestrate my_flow {
  node classifier -> node A when output == "urgent"
  node classifier -> node B when output == "normal"
  node classifier -> node C when output == "spam"
}

// 动态控制流（建议）
node classifier {
  let result = ai.chat p"Classify: {input}".tool("classify")
  
  // 节点内决定路由 + 状态更新
  return command {
    goto: result.category,
    update: { priority_score: result.confidence }
  }
}

// 等价于：return { goto: "A", update: { priority_score: 0.9 } }
```

**为什么 P0**: 静态边无法处理 Supervisor/Swarm 等高级模式（LangGraph 的 `Command(goto=...)` 是其子图跳转和 Agent handoff 的基础）。

---

### P0-4: 动态派发（Dynamic Dispatch / Send）

**来源**: LangGraph, ChatDev  
**问题**: 当前 `orchestrate` 的并行需要**静态**声明所有节点。无法从运行时数据生成并行任务。  
**建议**: 引入 `send` / `spawn` 原语，支持运行时动态 Map-Reduce。

```mora
// Map-Reduce 动态并行
node split_tasks {
  let tasks = ai.chat p"Split into subtasks: {goal}".tool("split")
  
  // 运行时生成 N 个并行任务
  return tasks.map(t => send("process_task", { task: t }))
}

node process_task {
  input: { task: Task }
  let result = ai.chat p"Process: {task}"
  return { partial_result: result }
}

node join_results {
  // 所有 process_task 完成后聚合
  input: { partial_results: [Result] @append }  // Reducer 自动聚合
  let summary = ai.chat p"Summarize: {partial_results}"
  return { final: summary }
}

// 边声明：split_tasks -> process_task 是动态展开
edge split_tasks -> process_task { dynamic: map }
edge process_task -> join_results { dynamic: reduce }
```

**为什么 P0**: 批处理、代码审查、数据处理等场景必须从运行时数据决定并行度。ChatDev 的 `dynamic_edge_executor.py` 和 LangGraph 的 `Send` 都验证了这是高频需求。

---

### P0-5: Eval 评估门控（Eval-first Quality Gate）

**来源**: agents-cli  
**问题**: 当前 `test` 是代码正确性测试，但缺少 **Agent 行为质量评估**（提示词质量、工具选择正确性、输出准确性）。  
**建议**: 引入 `eval` 作为一等原语，与 `test` 区分。

```mora
// 定义评估数据集
eval dataset BookingEval {
  case {
    input: "Book a flight from NYC to London on Aug 1"
    expected: { destination: "London", date: "2026-08-01" }
    metric: exact_match
  }
  case {
    input: "I need a hotel in Tokyo"
    expected: { contains: "hotel", location: "Tokyo" }
    metric: llm_as_judge(threshold: 0.85)
  }
}

// 评估 Agent
eval run BookingEval on booking_agent {
  threshold: 0.85
  iterations: 5
}

// 部署门控
deploy booking_agent {
  gate: eval BookingEval >= 0.85
  target: cloud_run
}
```

**为什么 P0**: agents-cli 的核心理念是 **"Eval-first, not test-first"**——Agent 的行为质量无法被传统单元测试覆盖。这是 mora 从"脚本语言"升级为"Agent 工程语言"的关键一步。

---

### P1-6: 状态前缀命名空间（State Prefix Namespace）

**来源**: agents-cli  
**问题**: 当前状态是全局的，无作用域分层。  
**建议**: 引入四级前缀命名空间。

```mora
// 当前：所有状态在同一个作用域
let state = { step: 2, language: "zh", total_queries: 1000 }

// 建议：前缀命名空间（自动路由到不同持久化后端）
let state.step = 2                    // session 级（默认， ephemeral）
let state.user:preferred_language = "zh"  // user 级（跨会话持久化）
let state.app:total_queries += 1      // app 级（全局计数器）
let state.temp:intermediate = data   // temp 级（当前调用，结束后清理）

// 在 orchestrate 中声明
orchestrate my_flow {
  state: {
    "user:profile": UserProfile @persistent,   // 自动持久化到用户存储
    "app:metrics": Metrics @persistent,        // 自动持久化到全局存储
    "temp:draft": Draft @ephemeral             // 调用结束后清理
  }
}
```

**为什么 P1**: 不是阻塞性需求，但会显著简化多会话 Agent 的状态管理。agents-cli 的 ADK 框架通过此前缀实现 session/user/app 三级隔离。

---

### P1-7: Middleware 拦截管道（Middleware Pipeline）

**来源**: dive-into-langgraph  
**问题**: 当前 `ai.chat` 是裸调用，缺少系统化的拦截机制（预算控制、PII 过滤、上下文压缩、日志记录）。  
**建议**: 声明式中间件管道。

```mora
// 全局中间件（对所有 ai.chat 生效）
middleware global {
  before_model: budget_guard { max_tokens: 10000, max_cost_usd: 0.50 }
  before_model: pii_filter { mask: ["credit_card", "ssn"] }
  wrap_model_call: latency_logger { metric_name: "llm_latency" }
  after_model: context_compressor { on_overflow: summarize, max_messages: 20 }
}

// 局部中间件（对特定 orchestrate 生效）
orchestrate customer_support {
  middleware: [
    dynamic_prompt {                       // 基于上下文动态生成 system prompt
      template: p"You are a {tone} support agent. User tier: {tier}."
      bind: { tone: state.user:tone, tier: state.user:tier }
    }
  ]
  node handle_request
}
```

**为什么 P1**: 中间件是生产级 Agent 的必需品（ LangGraph 的 `@before_model` 等 8 种装饰器）。当前 mora 只能在每个 `ai.chat` 调用处手写这些逻辑，重复且易遗漏。

---

### P1-8: 工具参数注入隐藏（Injected Tool Arguments）

**来源**: LangChain  
**问题**: 当前工具的所有参数都暴露给 LLM schema，但有些参数（如数据库连接、用户身份）是**运行时注入**的，不应被 LLM 生成。  
**建议**: `injected` 关键字。

```mora
// 当前：所有参数都在 LLM schema 中（风险：LLM 可能伪造 user_id）
fn query_db(sql: string, user_id: string) -> Result { ... }

// 建议：injected 参数对 LLM 不可见
fn query_db(sql: string, user_id: string with injected) -> Result {
  // user_id 由运行时注入，不在 LLM 看到的 tool schema 中
  // LLM 只能生成 sql 参数
  ...
}

// 调用时注入
let result = ai.chat p"Query active users" with tools=[query_db]
  where query_db.user_id = current_user.id  // 运行时注入
```

**为什么 P1**: 安全关键。LangChain 的 `InjectedToolArg` 是防范 LLM 伪造身份/权限的核心机制。

---

### P1-9: A2A 跨 Agent 通信（A2A Protocol）

**来源**: agents-cli  
**问题**: 当前 mora 的 Agent 是单机的，无跨框架/跨进程通信标准。  
**建议**: 实验性支持 Google A2A 协议。

```mora
// 声明远程 Agent（实验性）
a2a remote_scanner {
  card_url: "https://scanner.internal/.well-known/agent.json"
  capabilities: ["scan", "report"]
  auth: oauth2 { scope: "scanner:read" }
}

// 在 orchestrate 中使用
orchestrate security_audit {
  node local_analysis -> remote_scanner.a2a {  // 调用远程 Agent
    input: { target: state.target }
  }
  node report
}
```

**为什么 P1**: A2A 协议尚在 v0.9.1 演进中，但作为实验性原语可以为 mora 未来生态互操作预留接口。agents-cli 将其作为核心特性。

---

### P2-10: Ambient Agent 事件驱动（Ambient / Event-Driven Agent）

**来源**: agents-cli  
**问题**: 当前 mora 的 Agent 是**请求-响应**式的，无后台/事件驱动模式。  
**建议**: 声明式后台 Agent 配置。

```mora
// 声明后台 Agent（生成配置，不实现运行时）
ambient nightly_report {
  trigger: cron("0 20 * * *")       // 每晚 8 点
  agent: report_generator
  max_concurrent: 4
  retry: exponential { max: 5, base: 1min }
  on_failure: notify("ops@company.com")
}

// 等价生成：K8s CronJob / Cloud Scheduler / Eventarc 配置
```

**为什么 P2**: 需要 cron/事件基础设施，初期可仅生成配置（如 K8s CronJob），不实现运行时。属于"互补性"而非"功能性"特性。

---

### P2-11: 阶段感知记忆挂载（Stage-Aware Memory Attachment）

**来源**: ChatDev  
**问题**: 当前 `memory` 是全局的，无精细化检索时机控制。  
**建议**: 记忆挂载到节点+阶段。

```mora
// 在 orchestrate 中声明记忆挂载
orchestrate code_review {
  node reviewer {
    memory: store {
      retrieve_at: [pre_gen]          // 生成前检索历史审查记录
      write_at: [finished]            // 完成后写入本次审查结论
      scoring: { time_decay: 0.9, length_factor: 0.1 }  // 评分权重
    }
    let review = ai.chat p"Review this code: {code}"
    return { review }
  }
}
```

**为什么 P2**: 生产价值高但实现复杂（需要向量存储 + 评分算法）。可作为 v0.50+ 的扩展。

---

### P2-12: 上下文压缩策略（Context Compression Policy）

**来源**: dive-into-langgraph  
**问题**: 当前 `ai.chat` 无上下文窗口管理，长会话可能溢出 token 限制。  
**建议**: 声明式上下文策略。

```mora
// 全局上下文策略
context_policy global {
  max_messages: 20
  max_tokens: 10000
  on_overflow: summarize          // 溢出时自动摘要旧消息
  // 或：on_overflow: truncate_oldest
  // 或：on_overflow: raise_error
}

// 局部策略（覆盖全局）
node long_conversation {
  context_policy: { max_messages: 50, on_overflow: summarize }
  let response = ai.chat p"{state.long_context}"
}
```

**为什么 P2**:  LangGraph 的 `SummarizationMiddleware` 是可选组件。当前可通过应用层实现，但作为语言原语会更优雅。

---

## 4. 实施路线图建议

### v0.50 — 状态与执行模型（P0 项）

| 原语 | 实施方式 | 预估工作量 | 依赖 |
|------|----------|----------|------|
| Channel + Reducer | 扩展 `orchestrate` state 语法 + 运行时合并器 | 中 | 现有 orchestrate |
| Checkpoint | 新 `CheckpointSaver` trait + SQLite/内存实现 | 中 | Channel + Reducer |
| Command | 扩展节点返回值类型 + 编译器支持 | 中 | 现有 orchestrate |
| Dynamic Dispatch | 运行时 `Send` 队列 + 编译期类型推导 | 高 | Command + Reducer |

### v0.51 — 质量与评估（P0-5 + P1-6）

| 原语 | 实施方式 | 预估工作量 | 依赖 |
|------|----------|----------|------|
| Eval | 新 `eval` 语法 + LLM-as-judge 内置 | 中 | 现有 test 基础设施 |
| State Namespace | 扩展 `state` 键解析 + 路由到不同存储 | 低 | 现有 state |

### v0.52 — 生产级管道（P1-7 + P1-8）

| 原语 | 实施方式 | 预估工作量 | 依赖 |
|------|----------|----------|------|
| Middleware | 拦截器注册表 + 生命周期钩子 | 高 | 现有 ai.chat |
| Injected Args | 编译期 schema 过滤 + 运行时注入 | 低 | 现有 tool 系统 |

### v0.53+ — 生态与扩展（P1-9 + P2-10/11/12）

| 原语 | 实施方式 | 预估工作量 | 依赖 |
|------|----------|----------|------|
| A2A | 实验性模块 + Agent Card 解析 | 中 | 外部协议标准 |
| Ambient | 配置生成器（K8s CronJob/Cloud Scheduler） | 低 | 无运行时依赖 |
| Stage Memory | 向量存储集成 + 评分算法 | 高 | 外部存储 |
| Context Compression | 中间件实现 + 摘要策略 | 中 | Middleware |

---

## 5. 与已有 17 项目的统一模式表

将 23 个项目（17 + 6）的所有发现按**模式**聚类，形成 mora 语言设计的统一参考：

| 模式 | 代表项目 | mora 原语 | 状态 |
|------|----------|----------|------|
| **Capability 安全** | loongclaw | `capability`, `policy`, `audit` | ✅ v0.34 |
| **Sandbox 执行** | mini-swe-agent, headroom | `exec.bash`, `sandbox.spawn` | ✅ v0.34 |
| **Interrupt 结构化** | mini-swe-agent | `interrupt FormatError { ... }` | ✅ v0.34 |
| **Human-in-the-Loop** | mini-swe-agent, ChatDev | `3-mode`, `human` 节点 | ✅ v0.34 |
| **Skill 知识包** | CLI-Anything, ChatDev | `SKILL.md` 双注册表 | ✅ v0.41 |
| **编排基础** | 多项目 | `orchestrate` | ✅ v0.41 |
| **工具冲突** | AIOS | `tool_conflict_map` | ✅ v0.41 |
| **Cron 触发** | mimiclaw | `cron` 表达式 | ✅ v0.41 |
| **可观测性** | 多项目 | `observe`, `span`, `record_tokens` | ✅ v0.41 |
| **录制重放** | 多项目 | `record`, `replay`, `diff` | ✅ v0.41 |
| **并发控制** | 多项目 | `semaphore` | ✅ v0.49 |
| **Channel + Reducer** | LangGraph, LangGraph4j | **P0-1** | ⏳ v0.50 |
| **Checkpoint 持久化** | LangGraph, LangGraph4j | **P0-2** | ⏳ v0.50 |
| **Command 动态路由** | LangGraph | **P0-3** | ⏳ v0.50 |
| **动态派发** | LangGraph, ChatDev | **P0-4** | ⏳ v0.50 |
| **Eval 质量门控** | agents-cli | **P0-5** | ⏳ v0.51 |
| **State Namespace** | agents-cli | **P1-6** | ⏳ v0.51 |
| **Middleware 管道** | dive-into-langgraph | **P1-7** | ⏳ v0.52 |
| **Injected Args** | LangChain | **P1-8** | ⏳ v0.52 |
| **A2A 协议** | agents-cli | **P1-9** | ⏳ v0.53 |
| **Ambient Agent** | agents-cli | **P2-10** | ⏳ v0.53+ |
| **Stage Memory** | ChatDev | **P2-11** | ⏳ v0.53+ |
| **Context Compression** | dive-into-langgraph | **P2-12** | ⏳ v0.53+ |

---

## 6. 风险与免责声明

1. **A2A 协议**: Google 提出，尚在 v0.9.1 演进中，建议作为实验性原语，不承诺向后兼容。
2. **Context Compression**: 强绑定 LLM 的摘要能力，不同模型效果差异大。建议作为可选中间件而非强制语言特性。
3. **Ambient Agent**: 需要外部基础设施（K8s/Cloud Scheduler），mora 作为语言应仅生成配置，不实现运行时。
4. **LangGraph 依赖**: LangGraph 生态变化极快（v0.6 → v1.0 有 breaking change），借鉴其语义而非 API 形态。
5. **ChatDev 研究性质**: MacNet / Puppeteer 分支是研究代码，不建议直接迁移到语言原语。
6. **agents-cli 绑定 GCP**: Eval-first、Agent Identity、Context Caching 等特性强绑定 Google Cloud，建议提取通用语义而非具体实现。

---

## 7. 附录：项目快速参考卡

| 项目 | Stars | 语言 | 核心定位 | 最关键的原语灵感 |
|------|-------|------|----------|----------------|
| google/agents-cli | 4.2k | Python | Agent 的 DevOps 工具链 | Eval-first, State Namespace, A2A, Ambient |
| langchain-ai/langchain | 90k+ | Python | 分层编排平台 | InjectedToolArg, Runnable 四模式 |
| langchain-ai/langgraph | 35.9k | Python | 有状态图编排 | Pregel BSP, Channel+Reducer, Checkpoint, Command, Send |
| langgraph4j/langgraph4j | - | Java | Java 图编排 | 验证 Channel+Reducer 跨语言共识，8 种持久化后端 |
| luochang212/dive-into-langgraph | 500+ | Python | LangGraph 教程 | Middleware 管道, 三层上下文, 动态提示词, 上下文压缩 |
| OpenBMB/ChatDev | 33.7k | Python | 零代码多 Agent 编排 | 动态边展开, 阶段记忆, 角色扮演, Skill 加载 |

---

*本报告为 mora-lang v0.49+ 原语演进参考。所有建议均需经过语言设计评审（形式化语法、类型系统兼容性、实现复杂度评估）后进入实施阶段。*
