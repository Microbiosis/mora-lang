# 项目：langgraph4j/langgraph4j — 深度源码分析报告

> 分析时间：2026-07-07  
> 分析版本：v1.8.20 (最新稳定版)  
> 分析目标：提取核心架构、编排机制与差异化设计，为 Mora 语言提供原语灵感

---

## 1. 项目概览

| 属性 | 内容 |
|------|------|
| **仓库** | [langgraph4j/langgraph4j](https://github.com/langgraph4j/langgraph4j) |
| **语言** | Java 17+ |
| **核心定位** | Java 生态中构建**有状态、多 Agent LLM 应用**的图编排框架，是 Python LangGraph 的 Java 移植版 |
| **最新版本** | v1.8.20 (2026-06-27) |
| **生态集成** | LangChain4j、Spring AI、OpenTelemetry |
| **持久化后端** | Memory / MySQL / PostgreSQL / Redis / DynamoDB / Hazelcast / CockroachDB / Oracle |
| **License** | MIT |

**一句话概括**：LangGraph4j 将 Agent 工作流建模为**状态图（StateGraph）**，通过显式的节点、边、共享状态和检查点机制，实现可循环、可中断、可持久化、可回溯的 Agent 编排。

---

## 2. 核心架构解析

### 2.1 顶层架构：模块划分

```
langgraph4j/
├── langgraph4j-core/           # 核心：StateGraph、CompiledGraph、AgentState、Channel、Checkpoint
├── langgraph4j-{mysql|postgres|redis|...}-saver  # 持久化适配器
├── langchain4j/                # LangChain4j 集成 + ReACT AgentExecutor
├── spring-ai/                    # Spring AI 集成 + AgentExecutor
├── studio/                       # 嵌入式 Web UI（Spring Boot/Jetty/Quarkus）
├── how-tos/                      # Jupyter 教程（persistence、time-travel、subgraph等）
└── javelit/                      # 可视化组件（Spinner、MultiSelect、PlantUML）
```

### 2.2 关键抽象：四元组模型

LangGraph4j 的核心可抽象为 **四元组** `(State, Schema, Node, Edge)`：

| 抽象 | 类型 | 职责 |
|------|------|------|
| **AgentState** | `Map<String, Object>` | 全局共享状态，在节点间传递 |
| **Schema (Channels)** | `Map<String, Channel<?>>` | 定义每个状态键的**更新语义**（覆盖/累加/默认） |
| **Node** | `NodeAction<S>` / `AsyncNodeAction<S>` | 执行逻辑，返回 `Map<String, Object>` 作为状态更新 |
| **Edge** | `EdgeAction<S>` / `AsyncEdgeAction<S>` | 控制流转移，条件边决定下一跳节点 |

### 2.3 数据流：编译→运行→流式产出

```
StateGraph (定义期)
    │ addNode / addEdge / addConditionalEdges
    ▼
compile( CompileConfig )  →  CompiledGraph (运行期)
    │ checkpointSaver?  recursionLimit?  interruptsBefore/After?
    ▼
stream( inputs, RunnableConfig )  →  AsyncGenerator<NodeOutput>
    │ 每步产出：NodeOutput(nodeId, state)
    ▼
CheckpointSaver.put()  ← 持久化 ← 状态快照
```

### 2.4 状态更新机制：Channel + Reducer

这是 LangGraph4j 最有设计价值的部分。状态不是简单的 `Map` 覆盖，而是通过 **Schema** 声明每个键的合并策略：

```java
// 定义 Schema：messages 使用累加器，其他键默认覆盖
Map<String, Channel<?>> SCHEMA = Map.of(
    "messages", Channels.appender(ArrayList::new),  // 新值追加到列表
    "counter",  Channels.reducer( (old, val) -> val )  // 覆盖
);
```

`Channel` 接口定义了三种更新原语：
- **Reducer**: `(_old, _new) -> merged` — 自定义合并逻辑
- **Default**: `Supplier<T>` — 键缺失时的默认值
- **特殊标记**: `MARK_FOR_RESET`（重置为默认值）、`MARK_FOR_REMOVAL`（删除键）

> **架构洞察**：这本质上是 Actor Model 中**状态机**与**事件溯源（Event Sourcing）**的轻量结合。每个节点产出的是“增量事件”，Schema 定义如何折叠（fold）这些事件到状态上。

---

## 3. 关键机制与模式

### 机制1：Checkpoint / 持久化 + 时间旅行

**代码位置**: `CompiledGraph.getStateHistory()`, `updateState()`, `CheckpointSaver` 接口

```java
// 获取某线程的全部历史状态
Collection<StateSnapshot> history = compiledGraph.getStateHistory(config);

// 直接更新状态（人工介入或外部修正）
RunnableConfig newConfig = compiledGraph.updateState(config, Map.of("messages", newMessages), "asNode");

// 恢复执行
compiledGraph.stream(GraphInput.resume(), newConfig);
```

**设计要点**：
- 每个 `RunnableConfig` 携带 `threadId` + `checkpointId` + `nextNode`，形成**执行上下文**
- `CheckpointSaver` 是插件化接口，已支持 7+ 种后端
- `StateSnapshot` 包含：当前状态、下一节点、config、元数据
- 社区 ISSUE 提到缺少 `parentConfig`（父子检查点关系），这是相比 Python/JS 版的缺失

### 机制2：Interrupt / Human-in-the-Loop

**代码位置**: `CompileConfig.interruptsBefore()`, `CompiledGraph.shouldInterruptBefore()`

```java
var compiledGraph = stateGraph.compile(
    CompileConfig.builder()
        .interruptsBefore("execute_tool")   // 在执行工具前暂停
        .interruptsAfter("call_llm")         // 在LLM调用后暂停
        .build()
);

// 运行到中断点后产出 InterruptionMetadata
for (var item : compiledGraph.stream(inputs)) {
    if (item instanceof InterruptionMetadata im) {
        // 人工审批、修改状态、然后 resume
        var newConfig = compiledGraph.updateState(im.config(), approvedState);
        compiledGraph.stream(GraphInput.resume(), newConfig);
    }
}
```

**设计要点**：
- 中断不是异常，而是**受控的流暂停**
- 中断后可以通过 `updateState()` 修改状态，然后 `GraphInput.resume()` 继续
- 与 `CheckpointSaver` 结合，实现**审批工作流**（OpenHuskyAgent 的核心用法）

### 机制3：Command 模式 — 节点决定下一跳 + 状态更新

**代码位置**: `Command.java`, `StateGraph.addNode(id, AsyncCommandAction, mappings)`

```java
// 节点同时返回：1) 下一跳节点  2) 状态更新
public interface AsyncCommandAction<S extends AgentState> {
    CompletableFuture<Command> apply(S state, RunnableConfig config);
}

// Command 结构
record Command(String gotoNode, Map<String, Object> update) {}
```

**设计要点**：
- 传统节点只返回状态更新，边负责路由；Command 模式让**节点内聚路由决策**
- 这对应于 LangGraph 的 `Command` 原语（LangGraph 1.0 引入），用于**动态图修改**和**子图跳回**
- Mora 的 `orchestrate` 目前由外部控制流驱动，缺少这种**内嵌路由**能力

### 机制4：Subgraph / 图组合

**代码位置**: `SubStateGraphNode`, `SubCompiledGraphNode`, `ProcessedNodesEdgesAndConfig.process()`

```java
// 三种子图集成方式
stateGraph.addNode("sub", subGraph);              // 1. 作为 StateGraph 嵌套
stateGraph.addNode("sub", compiledSubGraph);       // 2. 作为 CompiledGraph 嵌套
// 3. 作为 NodeAction 内部调用（代码级）
```

**设计要点**：
- 子图与父图**共享同一个 AgentState**，通过 ID 前缀隔离（`subgraph::nodeId`）
- 编译期将子图扁平化到父图（`process()` 方法），运行时无额外开销
- 支持子图从父图 resume（`SUBGRAPH_RESUME_UPDATE_DATA` 元数据）

### 机制5：ParallelNode / 并行分支

**代码位置**: `ParallelNode`, `CompiledGraph` 构造器中对多 target 的处理

```java
// 一个源节点指向多个目标节点时，自动合成 ParallelNode
var parallelNode = new ParallelNode<>(sourceId, actions, channels);
// 并行节点执行后，结果通过 Channel.Reducer 合并回共享状态
```

**设计要点**：
- 并行分支有约束：不能是条件边（`unsupportedConditionalEdgeOnParallelNode`）
- 并行节点必须汇聚到单一目标（`illegalMultipleTargetsOnParallelNode`）
- 结果合并依赖 Schema 中的 Reducer，例如 `Channels.appender()` 将多分支的列表追加

### 机制6：Hooks / 可观测性拦截

**代码位置**: `NodeHooks`, `EdgeHooks`, `LG4JLoggable`

```java
stateGraph
    .addBeforeCallNodeHook("call_llm", (nodeId, state, config) -> { log(); return state; })
    .addAfterCallNodeHook((nodeId, state, config, result) -> { metrics(); return result; })
    .addWrapCallNodeHook((nodeId, state, config, action) -> {
        // OpenTelemetry span 包裹
        return action.apply(state, config);
    });
```

**设计要点**：
- 支持节点级和全局级 Hook
- 支持 Before/After/WrapCall 三种时机
- 与 OpenTelemetry 模块打通，实现分布式追踪

---

## 4. 对 Mora 语言的借鉴建议

### 建议1：引入 Schema/Channel 状态更新语义（高优先级）

**现状**：Mora 的 `with` 块和 `orchestrate` 中，状态传递是隐式的，更新语义不明确（全量覆盖还是增量合并）。

**LangGraph4j 模式**：
```java
Map<String, Channel<?>> SCHEMA = Map.of(
    "messages", Channels.appender(ArrayList::new),
    "context",  Channels.reducer( (old, val) -> merge(old, val) )
);
```

**Mora 原语建议**：
```mora
// 定义状态模式：声明每个字段的合并策略
state_schema MyState {
    messages: [Message] @append,      // 自动追加
    context: Context @merge,          // 自定义合并
    counter: int @replace,             // 覆盖（默认）
    flags: Set<String> @union,        // 并集
}

// 在 orchestrate 中隐式使用
orchestrate MyFlow(state: MyState) {
    node fetcher -> state {          // 返回的 Map 按 Schema 折叠
        return { messages: [newMsg] } // 追加到 messages
    }
}
```

### 建议2：引入 Checkpoint / 持久化抽象（高优先级）

**现状**：Mora 有 `record`/`replay`，但缺少**结构化检查点**和**多后端持久化**。

**Mora 原语建议**：
```mora
// 1. 声明检查点策略
orchestrate MyFlow(state: MyState) @checkpoint(saver: "postgres", thread: "session_id") {
    ...
}

// 2. 运行时 API
val history = flow.state_history(thread_id);  // 时间旅行
flow.update_state(thread_id, checkpoint_id, { messages: [user_edit] });
flow.resume(thread_id, checkpoint_id);

// 3. 中断等待人工输入
node human_approval @interrupt_after {
    // 自动暂停，等待外部 update_state
}
```

### 建议3：引入 Command 内嵌路由原语（中优先级）

**现状**：Mora 的 `orchestrate` 控制流由语法结构决定（顺序、条件），节点不能动态决定下一跳。

**Mora 原语建议**：
```mora
node router -> state, next: string {
    let result = ai.chat("决定下一步", state);
    return state, next: result.choice;  // 同时返回状态更新和下一跳
}

// 等价于
node router -> Command {
    return Command {
        update: { messages: [...] },
        goto: result.choice
    };
}
```

### 建议4：线程隔离（Thread）原语（中优先级）

**现状**：Mora 的 `orchestrate` 没有显式的多会话/多线程模型。

**LangGraph4j 模式**：`RunnableConfig.threadId` 隔离不同会话的检查点历史。

**Mora 原语建议**：
```mora
// 每个 thread_id 有独立的检查点历史
val session = flow.spawn_thread("user_123");
val result = session.run({ query: "hello" });
val history = session.checkpoints();  // 仅该会话的历史
```

### 建议5：编译期图验证（低优先级，但提升语言可靠性）

**LangGraph4j 模式**：`compile()` 验证 orphaned nodes、missing entry points、duplicate edges 等。

**Mora 建议**：在 `orchestrate` 定义时（类似 Rust 宏或编译期检查），静态验证：
- 所有节点可达
- 条件边映射完整
- 无循环依赖超限（`recursionLimit`）

### 风险/不适用项

| 项目 | 原因 |
|------|------|
| **Java CompletableFuture 模型** | Mora 是 Rust 异步运行时，直接移植不兼容，但概念（`async/await` + `stream`）可借鉴 |
| **PlantUML/Mermaid 可视化** | Mora 可编译为可视化格式，但非语言核心 |
| **Spring Boot/Jetty 嵌入** | 属于 Java 生态，Mora 的运行时 Web 界面需另设计（如 TUI 或 WebAssembly） |
| **完整的 ReACT AgentExecutor** | LangGraph4j 提供了开箱即用的 ReACT 循环，Mora 已有 `ai.chat` + tool 调用，可在标准库层面提供类似模板 |

---

## 5. 与已有17项目的差异化

### 5.1 与 LangGraph (Python 原版) 的差异

| 维度 | LangGraph (Python) | LangGraph4j (Java) |
|------|---------------------|----------------------|
| 状态模型 | 原生 Python dict + TypedDict | 显式 `AgentState` + `Channel` Schema + Reducer |
| 异步 | `asyncio` / `async` | `CompletableFuture` + `AsyncGenerator`（java-async-generator 库） |
| 检查点 | 内置 `MemorySaver` / `PostgresSaver` | 8 种后端适配器，更企业级 |
| 子图 | 函数式嵌套 | 编译期扁平化，性能更优 |
| 生态 | 紧密集 LangChain | 同时集成 LangChain4j **和** Spring AI |
| 可视化 | 内置 Mermaid | PlantUML + Mermaid + 嵌入式 Studio |

### 5.2 与 AIOS / mini-swe-agent / loongclaw 的差异化

| 项目 | 核心定位 | 与 LangGraph4j 的关键差异 |
|------|----------|--------------------------|
| **AIOS** | LLM 作为操作系统内核 | AIOS 是 Agent 的**调度器**（CPU time sharing），LangGraph4j 是 Agent 的**工作流编排器**（control flow graph）。Mora 可借鉴 AIOS 的调度优先级 + LangGraph4j 的图持久化 |
| **mini-swe-agent** | 软件工程 Agent | 端到端自动化（edit/search/shell），LangGraph4j 是底层框架。mini-swe-agent 可以**用** LangGraph4j 构建 |
| **loongclaw** | 多 Agent 自主规划 | 基于图神经网络/强化学习的动态规划，LangGraph4j 是**显式**人工定义的图。LangGraph4j 更适合可控的企业流程 |
| **ChatDev** | 多角色协作开发 | 基于角色通信协议（CEO/CTO/Programmer），LangGraph4j 缺少**角色内省**和**群体决策**机制。这是 Mora 可扩展的方向 |
| **agents-cli** | 自然语言到命令映射 | 以 LLM 为中心的 CLI 工具，LangGraph4j 是库而非 CLI。Mora 的 `exec.bash` 接近 agents-cli，但缺少 LangGraph4j 的状态持久化 |

### 5.3 LangGraph4j 的独特设计总结

1. **编译期图扁平化**：子图在 `compile()` 时展开为父图的节点和边，运行时零开销
2. **Channel 状态代数**：通过 `Reducer`/`Default`/`Appender` 声明式定义状态合并，而非命令式手动合并
3. **中断作为一等公民**：`interruptsBefore`/`interruptsAfter` 是编译配置的一部分，与 Checkpoint 深度集成
4. **嵌入式 Studio**：可直接嵌入到 Spring Boot/Jetty/Quarkus 应用中，成为开发调试工具
5. **Java 异步生成器**：通过 `java-async-generator` 库实现类似 Python `yield` 的流式输出，在 JVM 生态中罕见

---

## 6. 结论：Mora 的优先级采纳清单

| 优先级 | 机制 | Mora 形态建议 | 理由 |
|--------|------|---------------|------|
| 🔴 P0 | Channel/Schema + Reducer | 状态字段注解 `@append`, `@merge`, `@replace` | 解决当前状态更新语义模糊问题 |
| 🔴 P0 | Checkpoint + 持久化抽象 | `orchestrate @checkpoint(saver: ...)` + `thread` 概念 | 实现长时间运行、可恢复、可审批的 Agent 流程 |
| 🟡 P1 | Command 内嵌路由 | 节点返回 `Command { update, goto }` | 让节点内聚决策，减少控制流与数据流分离的复杂度 |
| 🟡 P1 | Thread 隔离 | `flow.spawn_thread()` / `session.checkpoints()` | 多用户/多会话场景的刚需 |
| 🟢 P2 | 编译期图验证 | `orchestrate` 编译时检查节点可达性、边完整性 | 提升语言可靠性，降低运行时错误 |
| 🟢 P2 | Hooks 系统 | `@before_node`, `@after_node`, `@wrap` 注解 | 观测性、AOP 拦截、安全审计 |

---

> **分析人**：Orchestrator Agent  
> **数据来源**：GitHub 源码（langgraph4j/langgraph4j main 分支）、README、CHANGELOG、how-tos notebooks、社区讨论  
> **分析深度**：核心源码文件（StateGraph.java、CompiledGraph.java、AgentState.java、Channel.java、AgentExecutor.java） + 架构文档
