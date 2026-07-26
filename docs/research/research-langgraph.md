# langchain-ai/langgraph 

> 2026-07-07  
>   
> GitHub state.pytypes.pypregel/main.pypregel/_algo.pychannels/base.py

---

## 1. 

|  |  |
|------|-----|
| **GitHub Stars** | ~35,930 |
| **** | PythonJavaScript/TypeScript  |
| **** | MIT |
| **** | ** Agent ** — "" +  +  LLM // |

LangGraph  LangChain 
- **LangChain** " LLM" — prompt → model → parser UNIX pipe
- **LangGraph** " LLM" — StateGraph  +  +  + 
-  `langchain-core` 

---

## 2. 

### 2.1 Pregel + Actor-Channel 

LangGraph  **Pregel**  Google Pregel Apache Beam  **Actor + Channel**

```

              Pregel                    

  Step 1: PLAN  →  Actors     
  Step 2: EXEC  →  Actors  
  Step 3: UPDATE →  Actors     
                     Channels   
  Repeat until no actors left / max steps     

```

**Bulk Synchronous Parallel** — 

### 2.2 

```
langgraph/
 graph/
    state.py          ← StateGraph 
    _branch.py        ← BranchSpec
    _node.py          ← StateNode 
 pregel/
    main.py           ← Pregel invoke/stream/astream
    _algo.py          ← apply_writes, prepare_next_tasks, _proc_input
    _loop.py          ← /SyncPregelLoop / AsyncPregelLoop
    _runner.py        ← PregelRunner
    _io.py            ← 
    _checkpoint.py    ← /
    _write.py         ← 
 channels/
    base.py           ← BaseChannelValueType, UpdateType, checkpoint
    last_value.py     ← LastValue
    topic.py          ← TopicPubSub/
    ephemeral_value.py← EphemeralValue
    binop.py          ← BinaryOperatorAggregatereducer  operator.add
    named_barrier_value.py ← NamedBarrierValue
 checkpoint/
    base.py           ← BaseCheckpointSaver
    memory.py         ← InMemorySaver
 types.py              ← Command, Interrupt, Send, RetryPolicy, TimeoutPolicy
 func/                 ← Functional APIentrypoint 
 supervisor/           ← Supervisor  Agent 
 store/                ← BaseStore
 cache/                ← BaseCache
 runtime.py            ← Runtime / RunControl
```

### 2.3 StateGraph 

`StateGraph` ****builder pattern `compile()`  `CompiledStateGraph` `Pregel` 

```python
class StateGraph(Generic[StateT, ContextT, InputT, OutputT]):
    """
    : State -> Partial
     state key  Annotated[type, reducer] 
    """
    def __init__(self, state_schema, context_schema=None, input_schema=None, output_schema=None):
        ...
    
    def add_node(self, node: str|Callable, action=None, *, 
                 retry_policy=None, cache_policy=None, 
                 destinations=None, defer=False) -> Self:
        ...
    
    def add_edge(self, start_key, end_key) -> Self:
        ...
    
    def add_conditional_edges(self, source, path, path_map=None) -> Self:
        ...
    
    def add_sequence(self, nodes) -> Self:   # 
        ...
    
    def compile(self, checkpointer=None, 
                interrupt_before=None, interrupt_after=None,
                store=None, cache=None, debug=False) -> CompiledStateGraph:
        ...
```


- **State  TypedDict/Pydantic** `Annotated[T, reducer]` 
- **Context ** `context_schema`  `user_id`, `db_conn` 
- ****`compile()` 

---

## 3. 

### 3.1 Channel  — 

```python
class BaseChannel(Generic[Value, Update, Checkpoint], ABC):
    """"""
    
    @abstractmethod
    def get(self) -> Value:          # 
        ...
    
    @abstractmethod
    def update(self, values: Sequence[Update]) -> bool:  # 
        ...
    
    @abstractmethod
    def checkpoint(self) -> Checkpoint:  # 
        ...
    
    @abstractmethod
    def from_checkpoint(self, checkpoint: Checkpoint) -> Self:  # 
        ...
```

 Channel 

| Channel |  |  |
|---------|------|----------|
| `LastValue` |  |  |
| `EphemeralValue` |  | / |
| `Topic` | / |  |
| `BinaryOperatorAggregate` |  `operator.add` |  |
| `NamedBarrierValue` |  | -join |
| `Context` |  | HTTP client |

> **Mora **Mora  `record/replay`  `sandbox` ****——LangGraph  `BinaryOperatorAggregate`  reducerMora 

### 3.2 Pregel BSP  — 

```python
#  pregel/_algo.py apply_writes
def apply_writes(checkpoint, channels, tasks, get_next_version, trigger_to_nodes):
    """ checkpoint  channels channel """
    # 1.  path 
    tasks = sorted(tasks, key=lambda t: task_path_str(t.path[:3]))
    
    # 2.  seen versions
    for task in tasks:
        checkpoint["versions_seen"][task.name].update(...)
    
    # 3.  channel  apply
    pending_writes_by_channel = defaultdict(list)
    for task in tasks:
        for chan, val in task.writes:
            if chan in channels:
                pending_writes_by_channel[chan].append(val)
    
    # 4.  channel  update channel 
    updated_channels = set()
    for chan, vals in pending_writes_by_channel.items():
        if channels[chan].update(vals):  # ←  reducer 
            updated_channels.add(chan)
    
    # 5.  channel 
    for chan in channels:
        if chan not in updated_channels:
            channels[chan].update(EMPTY_SEQ)
    
    return updated_channels
```

****`apply_writes` ****
- 
-  `reducer`  `operator.add`
- ****dataflow parallelism

### 3.3 Checkpoint +  — 

```python
# Checkpoint 
{
    "v": 4,                          # 
    "id": "uuid",                     #  ID
    "ts": "2026-07-07T12:00:00Z",    # 
    "channel_values": {               #  channel 
        "messages": [...],
        "foo": 42,
    },
    "channel_versions": {             #  channel 
        "messages": 5,
        "foo": 3,
    },
    "versions_seen": {                # 
        "node_a": {"messages": 4, "foo": 2},
    },
    "pending_sends": [...],           # Send
    "pending_writes": [...],          # 
}
```

Checkpoint 
- **Durability** checkpoint Agent 
- **Human-in-the-loop**`interrupt_before` / `interrupt_after` / checkpoint `Command(resume=...)` 
- **Time travel debugging**`get_state()` `update_state()` 
- **Subgraph ** subgraph  checkpoint namespace `checkpoint_ns` 

> **Mora **Mora  `record/replay`** checkpoint** ****LangGraph  checkpoint 

### 3.4 Command  — 

```python
@dataclass
class Command(Generic[N]):
    """"""
    graph: str | None = None          # None=PARENT=
    update: Any | None = None         #  return dict
    resume: dict[str, Any] | Any = None  #  interrupt()
    goto: Send | Sequence[Send | N] | N = ()  # 

    PARENT: ClassVar = "__parent__"   # 
```

`Command` 
1. **** `dict` `Command(goto="other_node")`
2. **** `Comma
