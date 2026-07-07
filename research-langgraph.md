# 项目：langchain-ai/langgraph 深度源码分析

> 分析日期：2026-07-07  
> 分析范围：核心架构、执行引擎、编排机制、差异化特性  
> 数据来源：GitHub 源码（state.py、types.py、pregel/main.py、pregel/_algo.py、channels/base.py）、官方文档、第三方技术评测

---

## 1. 项目概览

| 属性 | 值 |
|------|-----|
| **GitHub Stars** | ~35,930 |
| **主要语言** | Python（JavaScript/TypeScript 有独立实现） |
| **许可证** | MIT |
| **核心定位** | **低层级、有状态 Agent 编排框架** — 用"图"结构（状态 + 节点 + 边）控制 LLM 的循环/分支/持久化执行，而非简单链式调用 |

LangGraph 与 LangChain 的关系：
- **LangChain** 解决"调用 LLM" — prompt → model → parser（线性管道，类似 UNIX pipe）
- **LangGraph** 解决"控制 LLM" — StateGraph 有向图（循环 + 条件分支 + 持久化 + 人机介入）
- 两者共享 `langchain-core` 基础设施，是从简单到复杂的连续谱

---

## 2. 核心架构解析

### 2.1 架构哲学：Pregel + Actor-Channel 模型

LangGraph 的执行引擎 **Pregel** 直接受 Google Pregel（图计算框架）和 Apache Beam 启发，核心抽象是 **Actor + Channel**（而非传统的消息队列或回调）：

```
┌─────────────────────────────────────────────┐
│              Pregel 执行循环                   │
├─────────────────────────────────────────────┤
│  Step 1: PLAN  ──→ 决定本轮激活哪些 Actors     │
│  Step 2: EXEC  ──→ 并行执行所有激活的 Actors  │
│  Step 3: UPDATE ──→ 将 Actors 的写入同步到    │
│                     Channels（本步内不可见）   │
│  Repeat until no actors left / max steps     │
└─────────────────────────────────────────────┘
```

关键设计：**单步内写入不可见（Bulk Synchronous Parallel）** — 每个节点只能读到上一步结束后的状态，不能读到同一步其他节点正在写入的值。这消除了竞争条件，使并行节点天然安全。

### 2.2 核心模块划分

```
langgraph/
├── graph/
│   ├── state.py          ← StateGraph 构建器（节点、边、条件边、编译）
│   ├── _branch.py        ← 条件分支（BranchSpec）
│   └── _node.py          ← StateNode 节点封装
├── pregel/
│   ├── main.py           ← Pregel 执行引擎（invoke/stream/astream）
│   ├── _algo.py          ← 核心算法：apply_writes, prepare_next_tasks, _proc_input
│   ├── _loop.py          ← 同步/异步执行循环（SyncPregelLoop / AsyncPregelLoop）
│   ├── _runner.py        ← PregelRunner（任务调度执行）
│   ├── _io.py            ← 输入映射、通道读取
│   ├── _checkpoint.py    ← 检查点序列化/反序列化
│   └── _write.py         ← 节点写入通道的抽象
├── channels/
│   ├── base.py           ← BaseChannel（通道抽象：ValueType, UpdateType, checkpoint）
│   ├── last_value.py     ← LastValue（默认通道，只保留最新值）
│   ├── topic.py          ← Topic（PubSub，可配置去重/累积）
│   ├── ephemeral_value.py← EphemeralValue（单步后即消失）
│   ├── binop.py          ← BinaryOperatorAggregate（reducer 聚合，如 operator.add）
│   └── named_barrier_value.py ← NamedBarrierValue（等待特定节点集合全部完成）
├── checkpoint/
│   ├── base.py           ← BaseCheckpointSaver（持久化接口）
│   └── memory.py         ← InMemorySaver（内存检查点）
├── types.py              ← 核心类型：Command, Interrupt, Send, RetryPolicy, TimeoutPolicy
├── func/                 ← Functional API（entrypoint 装饰器，更高层抽象）
├── supervisor/           ← Supervisor 多 Agent 模式（预构建）
├── store/                ← BaseStore（长期记忆存储）
├── cache/                ← BaseCache（节点级缓存）
└── runtime.py            ← Runtime / RunControl（执行上下文）
```

### 2.3 关键抽象：StateGraph 构建器

`StateGraph` 是一个**编译式图构建器**（builder pattern），不能直接使用，必须先 `compile()` 为 `CompiledStateGraph`（即 `Pregel` 子类）：

```python
class StateGraph(Generic[StateT, ContextT, InputT, OutputT]):
    """
    节点签名: State -> Partial
    每个 state key 可用 Annotated[type, reducer] 标记聚合函数
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
    
    def add_sequence(self, nodes) -> Self:   # 快捷顺序编排
        ...
    
    def compile(self, checkpointer=None, 
                interrupt_before=None, interrupt_after=None,
                store=None, cache=None, debug=False) -> CompiledStateGraph:
        ...
```

核心设计点：
- **State 是共享的 TypedDict/Pydantic**：所有节点读写同一个状态对象，但通过 `Annotated[T, reducer]` 控制冲突时的合并策略
- **Context 是运行时不变量**：通过 `context_schema` 暴露 `user_id`, `db_conn` 等不随状态变化的数据
- **编译时静态验证**：`compile()` 会检查所有边是否指向已知节点、图是否可达入口点

---

## 3. 关键机制与模式

### 3.1 机制一：Channel 系统 — 状态传播的原语

```python
class BaseChannel(Generic[Value, Update, Checkpoint], ABC):
    """所有通道的基类，定义了状态如何被读取、更新、持久化。"""
    
    @abstractmethod
    def get(self) -> Value:          # 读取当前值
        ...
    
    @abstractmethod
    def update(self, values: Sequence[Update]) -> bool:  # 批量更新（来自同一步多个节点）
        ...
    
    @abstractmethod
    def checkpoint(self) -> Checkpoint:  # 序列化到检查点
        ...
    
    @abstractmethod
    def from_checkpoint(self, checkpoint: Checkpoint) -> Self:  # 从检查点恢复
        ...
```

内置 Channel 类型及用途：

| Channel | 语义 | 典型用途 |
|---------|------|----------|
| `LastValue` | 只保留最新值 | 状态字段（默认） |
| `EphemeralValue` | 单步后即消失 | 临时输入/输出 |
| `Topic` | 可累积的去重/追加列表 | 消息流、事件流 |
| `BinaryOperatorAggregate` | 二元聚合器（如 `operator.add`） | 列表累加、计数器 |
| `NamedBarrierValue` | 等待特定节点全部完成 | 并行-汇聚模式（join） |
| `Context` | 上下文管理器生命周期 | 数据库连接、HTTP client |

> **Mora 借鉴点**：Mora 的 `record/replay` 与 `sandbox` 是跨时间维度的，但缺少**同一步内多节点对共享状态的聚合语义**——LangGraph 的 `BinaryOperatorAggregate` 是声明式的 reducer，Mora 目前需要手动合并。

### 3.2 机制二：Pregel BSP 执行引擎 — 步骤隔离与并行安全

```python
# 摘自 pregel/_algo.py 核心：apply_writes
def apply_writes(checkpoint, channels, tasks, get_next_version, trigger_to_nodes):
    """将一组任务的写入应用到 checkpoint 和 channels，返回被更新的 channel 集合。"""
    # 1. 按 path 排序，确保确定性顺序
    tasks = sorted(tasks, key=lambda t: task_path_str(t.path[:3]))
    
    # 2. 更新 seen versions（每个节点记录了它上次读取时的版本）
    for task in tasks:
        checkpoint["versions_seen"][task.name].update(...)
    
    # 3. 按 channel 分组写入，然后统一 apply
    pending_writes_by_channel = defaultdict(list)
    for task in tasks:
        for chan, val in task.writes:
            if chan in channels:
                pending_writes_by_channel[chan].append(val)
    
    # 4. 对每个 channel 调用 update（所有同 channel 的写入被合并）
    updated_channels = set()
    for chan, vals in pending_writes_by_channel.items():
        if channels[chan].update(vals):  # ← 这是 reducer 被调用的地方
            updated_channels.add(chan)
    
    # 5. 未被更新的 channel 也通知有新步骤（驱动循环继续）
    for chan in channels:
        if chan not in updated_channels:
            channels[chan].update(EMPTY_SEQ)
    
    return updated_channels
```

**关键洞察**：`apply_writes` 在一步结束后一次性执行，所有节点的写入是**同时可见**的。这意味着：
- 并行节点不会互相读到对方写入的中间值
- 通过 `reducer` 函数（如 `operator.add`）可以定义并发写入的合并规则
- 这本质上是一种**数据流并行**（dataflow parallelism），而非共享内存并发

### 3.3 机制三：Checkpoint + 检查点持久化 — 状态的时间机器

```python
# Checkpoint 结构（推测自源码中的版本迁移逻辑）
{
    "v": 4,                          # 版本号（向后兼容迁移）
    "id": "uuid",                     # 检查点唯一 ID
    "ts": "2026-07-07T12:00:00Z",    # 时间戳
    "channel_values": {               # 各 channel 当前值
        "messages": [...],
        "foo": 42,
    },
    "channel_versions": {             # 各 channel 版本号（用于触发检测）
        "messages": 5,
        "foo": 3,
    },
    "versions_seen": {                # 每个节点最后读取到的版本
        "node_a": {"messages": 4, "foo": 2},
    },
    "pending_sends": [...],           # 已发送但尚未执行的任务（Send）
    "pending_writes": [...],          # 已写入但尚未提交的写入
}
```

Checkpoint 提供的能力：
- **Durability（持久执行）**：服务器重启后从 checkpoint 恢复，Agent 从中断处继续
- **Human-in-the-loop**：`interrupt_before` / `interrupt_after` 节点会在执行前/后暂停，状态保存到 checkpoint，等待外部 `Command(resume=...)` 恢复
- **Time travel debugging**：`get_state()` 可获取任意历史步骤的状态快照，`update_state()` 可回溯并修改状态后重新执行
- **Subgraph 嵌套**：每个 subgraph 有自己的 checkpoint namespace，通过 `checkpoint_ns` 隔离

> **Mora 借鉴点**：Mora 已有 `record/replay`，但缺少**细粒度的步骤级 checkpoint** 和**状态版本化**。LangGraph 的 checkpoint 是图执行的核心基础设施，不是外围日志。

### 3.4 机制四：Command 原语 — 节点控制流的第一公民

```python
@dataclass
class Command(Generic[N]):
    """从节点内部发出的控制流命令，可以更新状态、跳转节点、恢复中断。"""
    graph: str | None = None          # 目标图（None=当前，PARENT=父图）
    update: Any | None = None         # 状态更新（替代 return dict）
    resume: dict[str, Any] | Any = None  # 恢复中断（配合 interrupt()）
    goto: Send | Sequence[Send | N] | N = ()  # 跳转到哪个节点

    PARENT: ClassVar = "__parent__"   # 跳到父图的特殊标记
```

`Command` 的作用：
1. **动态路由**：节点不返回 `dict`，而是返回 `Command(goto="other_node")`，覆盖静态边
2. **父图通信**：子图通过 `Comma
