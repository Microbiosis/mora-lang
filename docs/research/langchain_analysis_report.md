# langchain-ai/langchain 

> 2026-07-07
> AI Agent
> Mora

---

## 1. 

|  |  |
|------|------|
| **** | `langchain-ai/langchain`Python/ `langchain-ai/langchainjs`JS/TS |
| **** | Python+ TypeScriptJS |
| **** | **"The platform for reliable agents"** — AI Agent |
| **** | LangChain-> LangChain CoreRunnable-> LangGraph-> LangSmith |
| **** | 90k+GitHubLLM |
| **** | 1000+ LCELPregel// |

> ****LangChain****—— `Runnable`  `Chain`  `LangGraph` LLMAgent

---

## 2. 

### 2.1 

```
Layer 4: LangSmith (//)
  - Tracing, evaluation, testing, deployment

Layer 3: LangGraph ()
  - StateGraph, Pregel, checkpoint, interrupt

Layer 2: LangChain ()
  - Chains, Agents, RAG templates, 1000+

Layer 1: LangChain Core ()
  - Runnable, BaseMessage, BaseTool, Prompts
```

### 2.2 Runnable 

 `Runnable[Input, Output]` `langchain_core/runnables/base.py`LangChain**""**

```python
class Runnable(ABC, Generic[Input, Output]):
    def invoke(self, input: Input, config: RunnableConfig | None = None) -> Output
    def batch(self, inputs: list[Input], ...) -> list[Output]
    def stream(self, input: Input, ...) -> Iterator[Output]
    async def ainvoke(self, input: Input, ...) -> Output
    
    def __or__(self, other) -> RunnableSequence  # : A | B | C
    def assign(self, **kwargs) -> Runnable       # 
    def pick(self, keys) -> Runnable             # 
```

****
-  `Runnable`  **sync/async/batch/stream** 
-  `invoke`/asyncio
-  `|`  `RunnableSequence`/`RunnableParallel` shell

### 2.3 Message-Centric 

`BaseMessage``langchain_core/messages/base.py`

```python
class BaseMessage(Serializable):
    content: str | list[str | dict[Any, Any]]  # 
    additional_kwargs: dict  # tool_calls
    response_metadata: dict    # token
    type: str                  # 
    id: str | None             # ID
    
    @property
    def content_blocks(self) -> list[ContentBlock]
    # ContentBlock = {"type": "text" | "image" | "tool_use" | ...}
```

****
- `content`  `str` `list[dict]`/
- `content_blocks`  provider-specific 
- `additional_kwargs`  ****——

### 2.4 LangGraph Pregel + StateGraph

LangGraph`langgraph/graph/state.py` **Google Pregel** ""

```python
class StateGraph(Generic[StateT, ContextT, InputT, OutputT]):
    def add_node(self, name, action, input_schema=..., retry_policy=..., cache_policy=...)
    def add_edge(self, start, end)
    def add_conditional_edges(self, source, path, path_map)
    def add_sequence(self, nodes)
    
    def compile(self, checkpointer=..., interrupt_before=..., interrupt_after=...) -> CompiledStateGraph

class CompiledStateGraph(Pregel):
    def invoke(self, input, config) -> OutputT
    def stream(self, input, config, stream_mode="values") -> Iterator[StreamPart]
    def get_state(self, config) -> StateSnapshot
    def update_state(self, config, values) -> None
```

**Pregel ** = 
1. ****channels
2. ****channels
3. ****channelschannelreducer
4. ****checkpointer
5. ****edgesbranches

### 2.5 

|  |  |  |
|------|----------|------|
| **Runnable** | `langchain_core/runnables/base.py` | schema |
| **** | `langchain_core/messages/base.py` |  |
| **** | `langchain_core/tools/base.py` | ToolSchema |
| **Agent Schema** | `langchain_core/agents.py` | Action/Observation/Finish |
| **** | `langgraph/graph/state.py` | StateGraphBuilder |
| **** | `langgraph/types.py` | CommandSendInterruptRetryPolicyStreamMode |
| **Pregel** | `langgraph/pregel/` | channel |
| **** | `langgraph/checkpoint/` |  |

---

## 3. 

### 1Channel + Reducer 

****`langgraph/graph/state.py` + `langgraph/channels/`

```python
class State(TypedDict):
    messages: Annotated[list, operator.add]  # reducer = append
    count: int                               # reducer = replace (LastValue)
    temp: Annotated[str, EphemeralValue]     # 
```

****
-  `BaseChannel``LastValue`, `EphemeralValue`, `BinaryOperatorAggregate`
- `Annotated[type, reducer]` 
- channelreducer `operator.add` 
- `EphemeralValue` 

**Mora******Agent

---

### 2Tool Schema  + 

****`langchain_core/tools/base.py`

```python
@tool
def get_weather(
    city: str,                     # LLMschema
    runtime: ToolRuntime,          # schema
    tool_call_id: Annotated[str, InjectedToolCallId]  # 
) -> str:
    ...

# schema: {"city": {"type": "string"}}
```

****
- `create_schema_from_function()`  `inspect.signature` + `typing.get_type_hints` Pydantic schema
- `InjectedToolArg` **LLMschema**
- `_filter_injected_args()` 
- `handle_tool_error`  `handle_validation_error`  `bool`/`str`/`callable`

**Mora**Mora `with` **""**

---

### 3Checkpoint + Time Travel 

****`langgraph/checkpoint/`, `langgraph/types.py`

```python
# 
checkpointer = SqliteSaver.from_conn_string("./state.db")
graph = builder.compile(checkpointer=checkpointer)

#  thread
config = {"configurable": {"thread_id": "user-123"}}
graph.invoke({"query": "..."}, config)

# 
snapshot = graph.get_state(config)
# fork
graph.update_state(config, {"messages": [...]}, as_node="human")
for chunk in graph.stream(None, config):  # fork
    ...
```

****
-  `Checkpoint`channel
- `thread_id` `checkpoint_id` 
- **** `fork` 
-  SQLitePostgresRedis 

**Mora**Mora `record/replay` LangGraph****Agent+

---

### 4Interrupt + Command(resume) 

****`langgraph/types.py`

```python
from langgraph.types import interrupt, Command

def human_approval_node(state: State):
    #  GraphInterruptvalue
    #  resume 
    decision = interrupt({
        "message": "Approve sending this email?",
        "draft": state["draft"]
    })
    return {"approved": decision == "approve"}

# 
graph.stream(Command(resume="approve"), config)
```

****
- `interrupt(value)`  `GraphInterrupt`Pregel loop
- checkpoint
-  `Command(resume=...)`  `interrupt()` 
-  `interrupt_before`/`interrupt_after` 
-  `interrupt` resume

**Mora**Mora `interrupt` 
1. ****
2. ****
3. **Command**

---

### 5Send + Map-Reduce

****`langgraph/types.py`

```python
from langgraph.types import Send

def dispatch_node(state: OverallState):
    # 3subject
    return [
        Send("generate_joke", {"subject": s})
        for s in state["subjects"]  # ["cats", "dogs", "birds"]
    ]

# 3 generate_joke  reducer 
```

****
- `Send(node, arg)` ""
-  `Send` Pregel****
-  `Annotated[list, operator.add]` reducer
- **** `RunnableParallel`

**Mora**Mora `orchestrate` **spawn/dispatch**

---

### 6Command 

****`langgraph/types.py`

```python
@dataclass
class Command(Generic[N]):
    graph: str | None = None          # None=, "__parent__"=
    update: Any | None = None         # 
    resume: dict[str, Any] | Any = None  # 
    goto: Send | Sequence[Send | N] | N = ()  # 

# Command
def router_node(state):
    if state["needs_human"]:
        return Command(goto="human_review", update={"status": "pending"})
    return Command(goto="auto_process")
```

****
- `Command` ****
- `goto` Send+
- `graph=Command.PARENT` 
- ********

**Mora**`Command` LangGraph——****Mora `return Command(...)`  `yield control(...)` 

---

### 7RetryPolicy + TimeoutPolicy + CachePolicy 

****`langgraph/types.py`

```python
class RetryPolicy(NamedTuple):
    initial_interval: float = 0.5      # 
    backoff_factor: float = 2.0       # 
    max_interval: float = 128.0       # 
    max_attempts: int = 3             # 
    jitter: bool = True                # 
    retry_on: type[Exception] | Callable[[Exception], bool] = default_retry_on

class TimeoutPolicy:
    run_timeout: float | None = None    # 
    idle_timeout: float | None = None   # 
    refresh_on: Literal["auto", "heartbeat"] = "auto"  # 

class CachePolicy:
    key_func: Callable = default_cache_key  # 
    ttl: int | None = None                  # 

# 
builder.add_node("slow_node", slow_func, 
                   retry_policy=RetryPolicy(max_attempts=5),
                   timeout_policy=TimeoutPolicy(run_timeout=30.0),
                   cache_policy=CachePolicy(ttl=3600))
```

****
- 
- `TimeoutPolicy` ****asyncio `heartbeat` 
- 

**Mora**Mora`ai.chat`, `exec.bash`****

---

### 8StreamMode 

****`langgraph/types.py`

```python
StreamMode = Literal[
    "values",       # 
    "updates",      # 
    "checkpoints",  # 
    "tasks",        # /
    "debug",        # checkpoints + tasks
    "messages",     # LLMtoken
    "custom"        #  StreamWriter 
]

# 
for chunk in graph.stream(input, config, stream_mode="updates"):
    if chunk["type"] == "updates":
        print(chunk["data"])  # {node_name: output}
```

****
- ""****
-  `stream_mode` 
- `StreamWriter` mode `"custom"` mode 

**Mora**`observe` ****token

---

## 4. Mora

### 1Channel + Reducer  -> state 

**LangChain**`Annotated[list, operator.add]` 

**Mora**`orchestrate` 

**Mora**

```mora
orchestrate travel_planner {
    state {
        //  = LastValue
        destination: string
        
        // reducer
        messages: list<Message> with reducer = append
        costs: list<float> with reducer = sum
        
        // 
        _temp_signal: string with ephemeral
    }
    
    node search_flights -> state { costs: [100.0] }  // 
    node search_hotels -> state { costs: [200.0] }  // 
    // costs = [100.0, 200.0]sum reducer = 300.0
}
```

****LangGraph"Agent"Mora

---

### 2InjectedArg  -> with 

**LangChain**`InjectedToolArg` LLMschema

**Mora**`with` LLM

**Mora**

```mora
fn get_weather(city: string, db: Database with injected) -> string {
    // `db` ai.chattool schema
    //  `city` LLM
    return db.query(city)
}

// tool call
tool get_weather with db = main_db
```

****"LLM"

---

### 3Checkpoint +  -> record 

**LangChain** `get_state`/`update_state`/`fork`

**Mora**`record` 

**Mora**

```mora
orchestrate long_task with checkpoint = auto {
    // 
    // 
    // - 
    // - fork
}

// CLI
// mora replay --checkpoint-id=abc --fork --modify-state='{"count": 5}'
```

****Agent

---

### 4Interrupt + Command(resume) -> interrupt 

**LangChain**`interrupt(value)` `Command(resume=...)` 

**Mora**`interrupt`  `src/interpreter/execute.rs`  `interrupt` 

**Mora**

```mora
fn human_approval_node(state: State) -> State {
    // interrupt  Future-like 
    // 
    let decision = interrupt {
        type: "approval",
        title: "Approve email?",
        data: state.draft
    }
    
    //  decision = 
    return state { approved: decision == "approve" }
}

//  CLI/API  resume 
// mora resume --task-id=xxx --value='{"decision": "approve"}'
```

****MoraAI-native scripts

---

### 5Send +  -> spawn  dispatch 

**LangChain**`Send(node, arg)` 

**Mora**`orchestrate` 

**Mora**

```mora
fn map_task(state: State) -> Command {
    let items = state.subjects  // ["cats", "dogs", "birds"]
    
    // 
    return dispatch [
        spawn generate_joke { subject: item }
        for item in items
    ]
    //  reducer 
}
```

****

---

### 6Command  -> return control(...) 

**LangChain** `Command(update=..., goto=...)` 

**Mora**

**Mora**

```mora
fn router(state: State) -> State {
    if state.confidence < 0.5 {
        // 
        return control {
            state: state { status: "needs_review" }
            goto: "human_review"
        }
    }
    return control {
        state: state { status: "auto_processed" }
        goto: "execute"
    }
}
```

****

---

### 7RetryPolicy + TimeoutPolicy -> 

**LangChain**

**Mora**`ai.chat`  `exec` 

**Mora**

```mora
fn risky_task() -> string
    with retry = { max_attempts: 3, backoff: exponential }
    with timeout = { run: 30s, idle: 10s }
    with cache = { ttl: 1h }
{
    let result = ai.chat("Analyze this large file...")
    return result
}
```

****

---

### 8Content Blocks  -> ai.chat 

**LangChain**`BaseMessage.content_blocks`  `text`, `image`, `tool_use`, `reasoning` 

**Mora**`ai.chat` 

**Mora**

```mora
let response = ai.chat("Describe this image", with image = file("photo.png"))

// response 
match response.blocks {
    [TextBlock { text }, ImageBlock { url, mime }] -> { ... }
    [ToolUseBlock { id, name, arguments }] -> { ... }
    [ReasoningBlock { reasoning }] -> { ... }  // DeepSeek/R1 style
}
```

****LLMMora

---

### 9Tool Schema  -> Mora

**LangChain**PythonPydantic schema

**Mora**Mora****tool schema

**Mora**

```mora
// Moraschema
fn search(query: string, limit: int = 10) -> list<Result> {
    ...
}

// 
// {
//   "name": "search",
//   "parameters": {
//     "type": "object",
//     "properties": {
//       "query": { "type": "string" },
//       "limit": { "type": "integer", "default": 10 }
//     },
//     "required": ["query"]
//   }
// }
```

****MoraPython****

---

### 10Graph -> Mora

**LangChain**`compile()` 

**Mora******
- `goto: "nonexistent_node"`
- 
- 
- /

****

---

###  / 

|  |  |
|--------|------|
| **** | LangChain""Mora |
| **Pydantic** | LangChainPydanticschemaMoraPydantic |
| **Python** | `get_type_hints`, `inspect.signature`, `issubclass` Mora |
| **≠** | LangChain1000+Mora |
| **** | LangChain"infamously steep"Mora |
| **** | LangChain `|` batch/stream""Mora |

---

## 5. 17

### 5.1 LangChain vs LoongClaw

|  | LangChain | LoongClaw |
|------|-----------|-----------|
| **** | LLM | GPT/Claude |
| **** | 1000+ | OpenAI/Anthropic API |
| **** | Core->Chain->Graph | API |
| **** | StateGraph + Checkpoint |  |
| **Mora** | **LangChain** | LoongClaw |

****LangChain **LangGraph Pregel**  **** LoongClawMora

### 5.2 LangChain vs AIOS

|  | LangChain | AIOS |
|------|-----------|------|
| **** |  | LLM |
| **** |  |  |
| **Agent** | Graph |  |
| **** | Tool |  |
| **Mora** | **LangChain** | AIOSMora sandbox |

****LangChain****AIOS****Mora `sandbox`  `capability` AIOS `orchestrate` LangGraph

### 5.3 LangChain vs mini-swe-agent

|  | LangChain | mini-swe-agent |
|------|-----------|----------------|
| **** |  | Agent |
| **** | / | shell, git, edit |
| **** |  |  |
| **Mora** | **LangChain** | mini-swe-agent |

****LangChain **Tool Schema**  **** mini-swe-agentAgentMora

### 5.4 LangChain vs AutoGen / CrewAI

|  | LangChain (LangGraph) | AutoGen | CrewAI |
|------|----------------------|---------|--------|
| **** | + | Conversational | + |
| **** |  |  |  |
| **** | Pregel |  |  |
| **** | `interrupt()`  | UserProxyAgent |  |
| **Mora** | **LangGraph** | AutoGen |

****LangGraph ** + Pregel** ——AutoGenCrewAI

### 5.5 LangChain vs Agents-CLI / ChatDev

|  | LangChain | Agents-CLI | ChatDev |
|------|-----------|------------|---------|
| **** |  | -> |  |
| **** |  | NL-> |  |
| **Mora** | **LangChain** | Agents-CLINLMoraintent -> plan |

****LangChain **** MoraAgents-CLI****

---

## 6. LangChainMora

### 6.1 

1. ** + Reducer**Agent`append`/`sum`/`replace`
2. ****`with injected` LLM
3. **Checkpoint**
4. **Interrupt/Resume**
5. **Tool Schema**Mora
6. **Content Blocks**`ai.chat` 

### 6.2 

7. ****`spawn`/`dispatch` 
8. **Command**+
9. ****`retry`/`timeout`/`cache` /
10. **Graph**Mora

### 6.3 

- ****
- ****
- **Python**MoraPython
- ****

---

> ****LangChainLangGraph**Agent** **Pregel +  +  + ** AgentMora**LangChain**schemaAgent
