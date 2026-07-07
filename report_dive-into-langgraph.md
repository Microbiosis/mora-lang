# 项目：luochang212/dive-into-langgraph — 深度源码分析报告

> 分析日期：2026-07-07 | 项目定位：LangGraph 1.0 完全指南（开源电子书 + 实战教程）

---

## 1. 项目概览

- **星数**：中文仓库约 500+ stars（活跃增长中），英文版本（dive-into-langgraph-en）同步维护
- **主要语言**：Python（Jupyter Notebook 为主）
- **核心定位**：帮助 Agent 开发者快速掌握 LangGraph 1.0 框架的完整教程，涵盖 ReAct Agent、StateGraph、Middleware、HITL、Memory、MCP、Supervisor、Parallelization、RAG 等 14 个章节
- **工程产出**：
  - 14 个章节 `.ipynb` 教程文件
  - 一个完整的 `app/` Gradio 聊天应用（含 MCP 集成、多 Agent 配置、流式输出）
  - `mcp_server/` 目录：天气 MCP、算数 MCP 的完整实现（基于 fastmcp）
  - `SKILL.md`：可作为 Claude Code 的 Skill 直接使用
  - 基于 `supervisord` 的 MCP 服务进程管理方案

> **关键承诺**：所有代码完全基于 LangGraph v1.0，不含任何 v0.6 历史残留。

---

## 2. 核心架构解析

### 2.1 架构总览

本项目并非框架本身，而是**基于 LangGraph 1.0 + LangChain 的顶层应用与教程**。其架构分层如下：

```
┌──────────────────────────────────────────────┐
│  应用层 (app/)                                 │
│  - Gradio Web UI                              │
│  - AgentService（懒加载 LLM + 子 Agent + 主 Agent）│
│  - 配置层 (LLMConfig / MCPConfig / AppConfig) │
├──────────────────────────────────────────────┤
│  编排层 (LangGraph 1.0)                        │
│  - StateGraph（状态图）                          │
│  - ReAct Agent（create_agent）                  │
│  - Supervisor（监督者模式）                       │
│  - Middleware（中间件管道）                       │
├──────────────────────────────────────────────┤
│  工具层 (Tools + MCP)                          │
│  - @tool 装饰器（本地工具）                       │
│  - MultiServerMCPClient（MCP 适配）              │
│  - ToolRuntime[Context]（运行时权限与上下文）      │
├──────────────────────────────────────────────┤
│  持久层 (Memory / Checkpointer)                 │
│  - InMemorySaver / SqliteSaver / RedisSaver     │
│  - InMemoryStore / SqliteStore（长期记忆）        │
│  - LangMem（第三方记忆库）                        │
├──────────────────────────────────────────────┤
│  模型层 (LangChain)                            │
│  - ChatOpenAI（兼容 DashScope / Ark / Ollama）  │
│  - init_chat_model                              │
│  - structured_output（Pydantic BaseModel）      │
└──────────────────────────────────────────────┘
```

### 2.2 关键抽象

| 抽象 | 说明 | 核心 API |
|------|------|----------|
| **StateGraph** | 状态图是流程编排的核心，节点（Node）通过边（Edge）连接，支持条件边和循环 | `StateGraph(MessagesState)`, `add_node()`, `add_edge()`, `add_conditional_edges()` |
| **CompiledStateGraph** | 编译后的图，可调用 `.invoke()` 或 `.stream()` | `builder.compile()` |
| **MessagesState** | 内置状态类型，核心字段 `messages: list` | `from langgraph.graph import MessagesState` |
| **ToolNode** | 预构建的工具调用节点，自动处理 tool_calls 和 tool_results | `ToolNode(tools)` |
| **Checkpointer** | 状态持久化检查点，支持失败恢复、HITL、时间旅行 | `InMemorySaver()`, `SqliteSaver()` |
| **Store** | 长期记忆存储，支持向量检索（Embedding）和过滤查询 | `InMemoryStore(index={"embed": fn, "dims": N})` |
| **Middleware** | 中间件管道，可拦截 Agent/Model/Tool 的生命周期 | `@before_model`, `@wrap_model_call`, `@dynamic_prompt` |
| **ToolRuntime** | 工具运行时上下文，注入权限、Store、Runtime 信息 | `ToolRuntime[Context]` |

### 2.3 数据流与控制流

**StateGraph 数据流**（以 ReAct 模式为例）：

```
用户输入 → [START] → assistant(节点) → should_continue(条件边)
                              ↓                    ↓
                        tool_calls?           no → [END]
                              ↓ yes
                        tool(工具节点) → [返回 assistant]
```

这是一个**有向循环图（DCG）**，assistant 和 tool 之间可循环多次，直到 LLM 决定不再调用工具。

**流式输出模式**（三种粒度）：
- `stream_mode="updates"`：每个节点完成后输出一次更新
- `stream_mode="messages"`：每个 token 生成后输出
- `stream_mode="values"`：输出 State 的完整快照，可查看 tool_calls 信息

---

## 3. 关键机制与模式

### 机制 1：Middleware 中间件管道（LangGraph 1.0 最大亮点）

这是 LangGraph 1.0 相对 0.6 的根本性改进。中间件通过装饰器注册到 Agent，形成拦截管道。

**装饰器类型**（全表）：

| 装饰器 | 执行位置 | 用途 |
|--------|----------|------|
| `@before_agent` | Agent 执行前 | 敏感词过滤、PII 检测、权限检查 |
| `@after_agent` | Agent 执行后 | 后处理、日志记录 |
| `@before_model` | 每次模型调用前 | 消息截断（trim_messages）、上下文压缩 |
| `@after_model` | 模型收到响应后 | 响应后处理、格式化 |
| `@wrap_model_call` | 包裹模型调用全程 | 动态模型切换（预算控制）、文件注入 |
| `@wrap_tool_call` | 包裹工具调用全程 | 工具权限控制、工具调用日志 |
| `@dynamic_prompt` | 动态生成系统提示词 | 基于 State/Store/Runtime 动态修改 prompt |
| `@hook_config` | 配置钩子行为 | 全局配置拦截 |

**代码示例 — 预算控制（动态模型切换）**：
```python
from langchain.agents.middleware import wrap_model_call, ModelRequest, ModelResponse

@wrap_model_call
def dynamic_model_selection(request: ModelRequest, handler) -> ModelResponse:
    message_count = len(request.state["messages"])
    model = basic_model if message_count > 5 else advanced_model
    return handler(request.override(model=model))

agent = create_agent(model=advanced_model, middleware=[dynamic_model_selection])
```

**代码示例 — 消息截断（上下文压缩）**：
```python
from langchain.agents.middleware import before_model
from langgraph.graph.message import REMOVE_ALL_MESSAGES

@before_model
def trim_messages(state: AgentState, runtime: Runtime) -> dict | None:
    messages = state["messages"]
    if len(messages) <= 3:
        return None
    first_msg = messages[0]
    recent_messages = messages[-3:]
    return {
        "messages": [
            RemoveMessage(id=REMOVE_ALL_MESSAGES),
            first_msg,
            *recent_messages
        ]
    }
```

**代码示例 — PII 检测 + 内容屏蔽（Guardrails）**：
```python
@before_agent(can_jump_to=["end"])
def content_filter(state: AgentState, runtime: Runtime) -> dict | None:
    last_message = state["messages"][-1]
    content = last_message.content.lower()
    if contains_pii(content):
        return {
            "messages": [{
                "role": "assistant",
                "content": "检测到敏感信息，已屏蔽处理..."
            }],
            "jump_to": "end"  # 直接跳转到结束
        }
    return None
```

### 机制 2：Human-in-the-Loop（HITL）与 Checkpoint 状态恢复

HITL 通过 `HumanInTheLoopMiddleware` 实现，核心依赖 **checkpoint 持久化**。

```python
from langchain.agents.middleware import HumanInTheLoopMiddleware
from langgraph.checkpoint.memory import InMemorySaver
from langgraph.types import Command

tool_agent = create_agent(
    model=llm,
    tools=[get_weather, add_numbers, calculate_bmi],
    middleware=[
        HumanInTheLoopMiddleware(
            interrupt_on={
                "get_weather": False,        # 无需审批
                "add_numbers": True,          # 需审批，允许 approve/edit/reject
                "calculate_bmi": {"allowed_decisions": ["approve", "reject"]},
            },
            description_prefix="Tool execution pending approval",
        ),
    ],
    checkpointer=InMemorySaver(),
)

# 触发中断后，用户通过 Command 恢复
result = tool_agent.invoke(
    Command(resume={"decisions": [{"type": "approve"}]}),
    config=config,
)
```

**关键设计**：
- 中断时，当前完整 `State` 被写入 Checkpointer（由 `thread_id` 标识）
- 恢复时，从 Checkpointer 读取状态，继续执行后续节点
- 支持 `approve` / `edit` / `reject` 三种决策类型
- 生产环境推荐 `SqliteSaver` / `PostgresSaver` / `RedisSaver` / `MongoDBSaver`

### 机制 3：三层上下文体系（Context Engineering）

LangGraph 将上下文明确分为三层，每层有不同的生命周期和作用域：

| 上下文层 | 作用域 | 生命周期 | 存储内容 | 典型用途 |
|----------|--------|----------|----------|----------|
| **Runtime** | 所有节点共享 | 单次请求 | 用户身份、环境变量、部署信息 | 权限判断、环境感知 |
| **State** | 节点间顺序传递 | 单次对话线程 | messages、工具调用结果、临时变量 | 对话历史、流程状态 |
| **Store** | 跨 Workflow/Agent | 持久化 | 用户偏好、Embedding、长期记忆 | 用户画像、知识库 |

**代码示例 — Runtime 控制工具权限**：
```python
from pydantic import BaseModel
from langchain.tools import tool, ToolRuntime

class Context(BaseModel):
    authority: Literal["admin", "user"]

@tool
def math_add(runtime: ToolRuntime[Context, Any], a: int, b: int) -> int:
    if runtime.context.authority != "admin":
        raise PermissionError("User does not have permission to add numbers")
    return a + b
```

**代码示例 — Store 读写长期记忆**：
```python
store = InMemoryStore(index={"embed": embed_fn, "dims": 1024})

# 写入
store.put(("users",), "user_1", {
    "rules": ["User likes short, direct language"],
    "rule_id": "3",
})

# 向量检索
items = store.search(
    ("users",),
    query="language preferences",
    filter={"rule_id": "3"},
)
```

### 机制 4：Map-Reduce 并发模式（Send 函数）

这是 LangGraph 中实现"发散-归约"的核心机制，与 Mora 的 `orchestrate` 可能缺少的原语直接相关。

```python
from langgraph.types import Send
from typing import Annotated
import operator

class Overall(TypedDict):
    situation: str
    roles: list[str]
    responses: Annotated[list, operator.add]  # 归约：列表合并
    best_response: str

#
