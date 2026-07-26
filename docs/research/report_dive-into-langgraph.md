# luochang212/dive-into-langgraph — 

> 2026-07-07 | LangGraph 1.0  + 

---

## 1. 

- **** 500+ starsdive-into-langgraph-en
- ****PythonJupyter Notebook 
- **** Agent  LangGraph 1.0  ReAct AgentStateGraphMiddlewareHITLMemoryMCPSupervisorParallelizationRAG  14 
- ****
  - 14  `.ipynb` 
  -  `app/` Gradio  MCP  Agent 
  - `mcp_server/`  MCP MCP  fastmcp
  - `SKILL.md` Claude Code  Skill 
  -  `supervisord`  MCP 

> **** LangGraph v1.0 v0.6 

---

## 2. 

### 2.1 

** LangGraph 1.0 + LangChain **

```

   (app/)                                 
  - Gradio Web UI                              
  - AgentService LLM +  Agent +  Agent
  -  (LLMConfig / MCPConfig / AppConfig) 

   (LangGraph 1.0)                        
  - StateGraph                          
  - ReAct Agentcreate_agent                  
  - Supervisor                       
  - Middleware                       

   (Tools + MCP)                          
  - @tool                        
  - MultiServerMCPClientMCP               
  - ToolRuntime[Context]      

   (Memory / Checkpointer)                 
  - InMemorySaver / SqliteSaver / RedisSaver     
  - InMemoryStore / SqliteStore        
  - LangMem                        

   (LangChain)                            
  - ChatOpenAI DashScope / Ark / Ollama  
  - init_chat_model                              
  - structured_outputPydantic BaseModel      

```

### 2.2 

|  |  |  API |
|------|------|----------|
| **StateGraph** | NodeEdge | `StateGraph(MessagesState)`, `add_node()`, `add_edge()`, `add_conditional_edges()` |
| **CompiledStateGraph** |  `.invoke()`  `.stream()` | `builder.compile()` |
| **MessagesState** |  `messages: list` | `from langgraph.graph import MessagesState` |
| **ToolNode** |  tool_calls  tool_results | `ToolNode(tools)` |
| **Checkpointer** | HITL | `InMemorySaver()`, `SqliteSaver()` |
| **Store** | Embedding | `InMemoryStore(index={"embed": fn, "dims": N})` |
| **Middleware** |  Agent/Model/Tool  | `@before_model`, `@wrap_model_call`, `@dynamic_prompt` |
| **ToolRuntime** | StoreRuntime  | `ToolRuntime[Context]` |

### 2.3 

**StateGraph ** ReAct 

```
 → [START] → assistant() → should_continue()
                              ↓                    ↓
                        tool_calls?           no → [END]
                              ↓ yes
                        tool() → [ assistant]
```

**DCG**assistant  tool  LLM 

****
- `stream_mode="updates"`
- `stream_mode="messages"` token 
- `stream_mode="values"` State  tool_calls 

---

## 3. 

###  1Middleware LangGraph 1.0 

 LangGraph 1.0  0.6  Agent

****

|  |  |  |
|--------|----------|------|
| `@before_agent` | Agent  | PII  |
| `@after_agent` | Agent  |  |
| `@before_model` |  | trim_messages |
| `@after_model` |  |  |
| `@wrap_model_call` |  |  |
| `@wrap_tool_call` |  |  |
| `@dynamic_prompt` |  |  State/Store/Runtime  prompt |
| `@hook_config` |  |  |

** — **
```python
from langchain.agents.middleware import wrap_model_call, ModelRequest, ModelResponse

@wrap_model_call
def dynamic_model_selection(request: ModelRequest, handler) -> ModelResponse:
    message_count = len(request.state["messages"])
    model = basic_model if message_count > 5 else advanced_model
    return handler(request.override(model=model))

agent = create_agent(model=advanced_model, middleware=[dynamic_model_selection])
```

** — **
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

** — PII  + Guardrails**
```python
@before_agent(can_jump_to=["end"])
def content_filter(state: AgentState, runtime: Runtime) -> dict | None:
    last_message = state["messages"][-1]
    content = last_message.content.lower()
    if contains_pii(content):
        return {
            "messages": [{
                "role": "assistant",
                "content": "..."
            }],
            "jump_to": "end"  # 
        }
    return None
```

###  2Human-in-the-LoopHITL Checkpoint 

HITL  `HumanInTheLoopMiddleware`  **checkpoint **

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
                "get_weather": False,        # 
                "add_numbers": True,          #  approve/edit/reject
                "calculate_bmi": {"allowed_decisions": ["approve", "reject"]},
            },
            description_prefix="Tool execution pending approval",
        ),
    ],
    checkpointer=InMemorySaver(),
)

#  Command 
result = tool_agent.invoke(
    Command(resume={"decisions": [{"type": "approve"}]}),
    config=config,
)
```

****
-  `State`  Checkpointer `thread_id` 
-  Checkpointer 
-  `approve` / `edit` / `reject` 
-  `SqliteSaver` / `PostgresSaver` / `RedisSaver` / `MongoDBSaver`

###  3Context Engineering

LangGraph 

|  |  |  |  |  |
|----------|--------|----------|----------|----------|
| **Runtime** |  |  |  |  |
| **State** |  |  | messages |  |
| **Store** |  Workflow/Agent |  | Embedding |  |

** — Runtime **
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

** — Store **
```python
store = InMemoryStore(index={"embed": embed_fn, "dims": 1024})

# 
store.put(("users",), "user_1", {
    "rules": ["User likes short, direct language"],
    "rule_id": "3",
})

# 
items = store.search(
    ("users",),
    query="language preferences",
    filter={"rule_id": "3"},
)
```

###  4Map-Reduce Send 

 LangGraph "-" Mora  `orchestrate` 

```python
from langgraph.types import Send
from typing import Annotated
import operator

class Overall(TypedDict):
    situation: str
    roles: list[str]
    responses: Annotated[list, operator.add]  # 
    best_response: str

#
