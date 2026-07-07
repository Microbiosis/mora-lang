# 项目：OpenBMB/ChatDev 深度源码分析报告

> 分析日期：2026-07-07
> 分析版本：ChatDev 2.0 (DevAll) main 分支 + ChatDev 1.0 (Legacy) chatdev1.0 分支 + MacNet/Croto/Puppeteer 研究分支
> 分析师：AI Agent 架构与编程语言设计分析师

---

## 1. 项目概览

| 属性 | 值 |
|------|------|
| **GitHub Stars** | 33.7k |
| **主要语言** | Python (68.6%) + Vue (28.6%) |
| **核心定位** | **零代码多智能体编排平台**（ChatDev 2.0 DevAll），最初是虚拟软件公司（ChatDev 1.0） |
| **许可证** | Apache-2.0 |
| **关键论文** | ChatDev (arXiv:2307.07924), MacNet (ICLR 2025), Puppeteer (NeurIPS 2025), Croto (arXiv:2406.08979) |

ChatDev 已从单一领域（软件开发）的**多智能体角色扮演系统**演进为通用**零代码多智能体编排平台**。其演进路径清晰：
- **1.0**：链式 `Phase` 编排 + 双角色 `RolePlaying` 对话 + 软件公司模拟
- **MacNet**：DAG 拓扑扩展，支持千级 agent 协作
- **Croto**：跨团队编排（Cross-Team Orchestration）
- **Puppeteer**：RL 优化的中央动态编排器
- **2.0 (DevAll)**：YAML 驱动的通用图编排引擎，支持 `agent` / `human` / `subgraph` / `python` / `loop` 等节点类型

---

## 2. 核心架构解析

### 2.1 模块划分（2.0 DevAll）

```
ChatDev/
├── server/           # FastAPI 后端，REST API + WebSocket
├── frontend/         # Vue 3 可视化画布（Workflow / Launch / Tutorial）
├── runtime/          # 智能体抽象与执行引擎
│   ├── node/         # 节点执行器（agent, human, python, passthrough, literal, loop, subgraph）
│   │   ├── executor/       # 具体执行器（AgentNodeExecutor, HumanNodeExecutor...）
│   │   ├── agent/          # LLM 调用、Memory、Thinking、Skills
│   │   │   ├── memory/     # MemoryBase / MemoryManager（Simple/File/Blackboard/Mem0）
│   │   │   ├── thinking/   # ThinkingManager（Pre/Post Generation Reflection）
│   │   │   ├── skills/     # AgentSkillManager（SKILL.md 发现与激活）
│   │   │   └── providers/  # OpenAI / Gemini / 等模型适配器
│   │   └── registry.py     # 节点类型注册表
│   └── edge/         # 边条件与处理器
│       ├── conditions/     # FunctionEdge / KeywordEdge
│       └── processors/     # Payload 转换
├── workflow/         # 图编排引擎
│   ├── graph.py            # GraphExecutor：核心执行器（DAG/循环/多数投票）
│   ├── graph_context.py    # GraphContext：图运行时上下文
│   ├── graph_manager.py    # GraphManager：拓扑分层、循环检测
│   ├── cycle_manager.py    # CycleManager：循环执行控制
│   ├── runtime.py          # RuntimeBuilder + RuntimeContext：运行时构建
│   └── executor/           # 执行策略 + 动态边 + 资源管理
│       ├── resource_manager.py   # Semaphore 并发资源协调
│       └── dynamic_edge_executor.py  # Map/Tree 动态展开
├── entity/           # 配置定义与校验（dataclass + schema）
│   ├── configs/      # Node/Edge/Graph/Memory/Thinking/Skill 配置
│   ├── messages.py   # Message / MessageBlock / AttachmentRef / ToolCallPayload
│   └── enums.py      # 枚举定义（Role, Stage, InputMode, LogLevel...）
├── functions/        # 自定义 Python 工具（用户可扩展）
├── schema_registry/  # 动态 schema 注册（Provider/Memory/Thinking）
└── yaml_instance/    # 可运行工作流模板（ChatDev_v1.yaml, deep_research_v1.yaml...）
```

### 2.2 关键抽象与数据流

#### 图模型（Graph Model）
ChatDev 2.0 使用**有向图**（支持 DAG + 循环）作为编排核心抽象：

- **Node**：带类型的执行单元。核心类型：`agent`（LLM 智能体）、`human`（人工介入）、`subgraph`（子图嵌套）、`python`（代码执行）、`passthrough`（透传）、`literal`（固定文本）、`loop_counter` / `loop_timer`（循环控制）。
- **Edge**：带条件的单向边。支持 `condition`（`function` / `keyword` 匹配）、`carry_data`（数据传递）、`keep_message`（保留消息）、`clear_context`（清空上下文），以及**动态配置**（`dynamic_config`）。
- **GraphContext**：封装图配置、运行时目录、全局状态、变量。

#### 执行流（Execution Flow）
```
TaskInput → GraphExecutor.execute_graph()
  → GraphManager.build_graph() [拓扑分层 / 循环检测]
  → _build_memories_and_thinking() [初始化记忆和思考]
  → 策略选择：
      - DAG → DagExecutionStrategy（按拓扑层执行）
      - Cycle → CycleExecutionStrategy（循环控制）
      - MajorityVoting → MajorityVoteStrategy（多路投票）
  → _execute_node(node) 
      → 获取 dynamic_config（Map/Tree）
      → NodeExecutor.execute() [策略模式：AgentNodeExecutor / HumanNodeExecutor / ...]
      → _process_edge_output() [条件路由 + 动态展开]
  → _collect_all_outputs() + _save_memories() + ResultArchiver.export()
```

### 2.3 消息系统（Message System）

ChatDev 定义了非常精细的**统一消息抽象**（`entity/messages.py`）：

```python
@dataclass
class Message:
    role: MessageRole       # system / user / assistant / tool
    content: MessageContent  # str | List[MessageBlock] | List[Dict]
    name: Optional[str]
    tool_call_id: Optional[str]
    metadata: Dict[str, Any]
    tool_calls: List[ToolCallPayload]
    keep: bool = False           # 是否保留在上下文窗口
    preserve_role: bool = False

@dataclass
class MessageBlock:
    type: MessageBlockType  # text / image / audio / video / file / data
    text: Optional[str]
    attachment: Optional[AttachmentRef]  # 本地/远程附件引用
    data: Dict[str, Any]
```

**关键设计**：`AttachmentRef` 支持 `local_path`、`remote_file_id`、`data_uri`（base64），实现了**多模态内容**与**LLM 输出**的统一封装。工具调用结果（`FunctionCallOutputEvent`）和文件读取结果都被统一转换为 `MessageBlock` 序列。

---

## 3. 关键机制与模式

### 机制1：角色扮演通信协议（Role-Playing Communication Protocol）

**位置**：`chatdev1.0/chatdev/phase.py`（基于 CAMEL 框架）

ChatDev 1.0 的核心创新是**双角色对话模式**。每个 `Phase` 不是简单的单 agent 调用，而是定义了 `assistant_role` 和 `user_role` 之间的**角色扮演对话**：

```python
class Phase(ABC):
    def chatting(self, chat_env, task_prompt, assistant_role_name, user_role_name, ...):
        # 初始化 RolePlaying 会话
        role_play_session = RolePlaying(
            assistant_role_name=assistant_role_name,
            user_role_name=user_role_name,
            assistant_role_prompt=...,
            user_role_prompt=...,
            task_prompt=task_prompt,
            ...
        )
        # 多轮对话，直到产生 seminar_conclusion 或达到 chat_turn_limit
        for i in range(chat_turn_limit):
            assistant_response, user_response = role_play_session.step(input_user_msg, ...)
            if assistant_response.msg.info:  # 发现 <INFO> 标记，提前结束
                seminar_conclusion = assistant_response.msg.content
                break
```

**关键洞察**：`seminar_conclusion` 是 Phase 的**结构化输出协议**。双方通过对话达成共识，生成带标记（如 `<INFO> Finished`）的结论，然后由 `update_chat_env()` 将结论写入全局环境。这是**去中心化协商**而非**中心化命令**的模式。

---

### 机制2：组合阶段与循环控制（ComposedPhase + Cycle Breaking）

**位置**：`chatdev1.0/chatdev/composed_phase.py`

```python
class ComposedPhase(ABC):
    def execute(self, chat_env):
        for cycle_index in range(1, self.cycle_num + 1):
            for phase_item in self.composition:
                self.phases[phase].phase_env = self.phase_env
                self.phases[phase].update_phase_env(chat_env)
                if self.break_cycle(self.phases[phase].phase_env):
                    return chat_env
                chat_env = self.phases[phase].execute(chat_env, ...)
                if self.break_cycle(self.phases[phase].phase_env):
                    return chat_env
```

`break_cycle()` 是**用户可覆盖的终止条件**。例如 `CodeCompleteAll` 在 `unimplemented_file == ""` 时终止，`CodeReview` 在 `"Finished" in modification_conclusion` 时终止。这实现了**条件驱动的循环编排**。

---

### 机制3：动态边展开（Dynamic Edge Expansion — Map / Tree）

**位置**：`workflow/executor/dynamic_edge_executor.py`

ChatDev 2.0 在**边级别**支持动态执行模式，这是极其独特的设计：

```python
class DynamicEdgeConfig(BaseConfig):
    type: str              # "map" or "tree"
    split: SplitConfig     # 如何拆分输入（如按行、按 JSON 路径）
    config: BaseConfig       # MapDynamicConfig / TreeDynamicConfig
```

- **Map 模式**：将输入拆分为 N 个单元，**并行**执行目标节点，收集所有输出（fan-out）。
- **Tree 模式**：将输入分组（`group_size`），逐层**聚合**（reduce），直到只剩一个结果（fan-out + reduce）。

```python
# Tree 模式的核心 reduction 循环
while len(current_messages) > 1:
    groups = group_messages(current_messages, group_size)
    # 并行执行每组
    with ThreadPoolExecutor(max_workers=max_parallel) as executor:
        for idx, group in enumerate(groups):
            future = executor.submit(self._execute_group, node, group_inputs, layer, idx)
    current_messages = layer_outputs  # 进入下一层
```

**关键价值**：无需修改节点定义，只需在**边配置**上声明 `dynamic_config: {type: tree, split: {...}}`，即可实现 MapReduce 式的大规模并行/聚合。这是**声明式分布式计算**在 Agent 编排中的首次实现。

---

### 机制4：Agent 技能系统（Agent Skills）

**位置**：`runtime/node/agent/skills/` + `entity/configs/node/skills.py`

ChatDev 实现了**运行时技能发现与加载**：

```python
# 从 .agents/skills/<skill_name>/SKILL.md 自动发现技能
DEFAULT_SKILLS_ROOT = REPO_ROOT / ".agents" / "skills"

# Agent 配置中启用 skills
skills:
  enabled: true
  allow: ["deep-research", "code-review"]  # 白名单

# 运行时注入给 LLM 的工具
- name: activate_skill      # 激活某个技能，加载其 SKILL.md 指令
- name: read_skill_file    # 读取技能目录下的附属文件
```

**关键设计**：Skill 不是硬编码代码，而是**Markdown 指令文件**（SKILL.md）。Agent 通过 `activate_skill` 工具动态加载指令到 system prompt，实现**零代码扩展 Agent 能力**。这与 MCP 的 tool 机制不同——它扩展的是**行为知识**而非**工具函数**。

---

### 机制5：阶段感知记忆挂载（Stage-Aware Memory Attachment）

**位置**：`runtime/node/agent/memory/memory_base.py` + `entity/configs/node/memory.py`

```python
@dataclass
class MemoryAttachmentConfig:
    name: str
    retrieve_stage: List[AgentExecFlowStage] | None  # PRE_GEN_THINKING / GEN / POST_GEN_THINKING / FINISHED
    top_k: int = 3
    similarity_threshold: float = -1.0
    read: bool = True
    write: bool = True
```

记忆挂载在**Agent 节点**上，而非全局。记忆检索发生在特定阶段：
- **PRE_GEN_THINKING**：生成前思考阶段检索历史经验
- **GEN**：实际生成阶段检索相关上下文
- **POST_GEN_THINKING**：生成后反思阶段检索对比依据
- **FINISHED**：执行结束后写入新记忆

**记忆评分函数**（`MemoryManager._score_memory`）综合了**时间衰减**、**长度因子**和**词级相关性**：
```python
def _score_memory(self, memory_item, query):
    age_hours = (current_time - memory_item.timestamp) / 3600
    time_decay = max(0.1, 1.0 - age_hours / (24 * 30))
    length_factor = ...  # 短文本 0.5，长文本 0.8，适中 1.0
    relevance = len(query_words & content_words) / len(query_words)
    return 0.7 * time_decay * length_factor + 0.3 * relevance
```

---

### 机制6：伪边与上下文窗口管理（Pseudo Edge + Context Window）

**位置**：`workflow/graph.py`（`_execute_node` 方法）

```python
# 节点执行后，通过 pseudo_edge 将输出回送到自身输入
if node.context_window != 0 and not context_restored:
    pseudo_condition = EdgeConditionConfig.from_dict("true", ...)
    pseudo_link = EdgeLink(target=node, trigger=False)
    pseudo_link.condition_config = pseudo_condition
    for output_msg in output_messages:
        self._process_edge_output(pseudo_link, output_msg, node)
```

每个节点可配置 `context_window`：
- `-1`：无限上下文，不清空
- `0`：执行后完全清空输入
- `N > 0`：保留最近 N 条输入，其余清除

`pseudo_edge` 实现了**自循环反馈**，使 Agent 节点能基于自身输出继续推理（类似 Chain-of-Thought 的自动延续）。

---

### 机制7：重试策略 DSL（Retry Policy DSL）

**位置**：`entity/configs/node/agent.py`（`AgentRetryConfig`）

```python
@dataclass
class AgentRetryConfig:
    enabled: bool = True
    max_attempts: int = 5
    min_wait_seconds: float = 1.0
    max_wait_seconds: float = 6.0
    retry_on_status_codes: List[int] = [408, 409, 425, 429, 500, 502, 503, 504]
    retry_on_exception_types: List[str] = ["RateLimitError", "APITimeoutError", ...]
    non_retry_exception_types: List[str] = []
    retry_on_error_substrings: List[str] = ["rate limit", "temporarily unavailable", ...]
```

支持**多层异常链遍历**（包括 `ExceptionGroup`）、**HTTP 状态码匹配**、**异常类型匹配**、**错误消息子串匹配**、**非重试异常黑名单**。使用 `tenacity` 库实现指数退避。

---

### 机制8：资源协调（Resource Manager）

**位置**：`workflow/executor/resource_manager.py`

通过**信号量**（`threading.Semaphore`）控制节点级别的并发资源：

```python
class ResourceManager:
    def guard_node(self, node: Node):
        requests = self._resolve_node_requests(node)  # 从 NodeCapabilities 解析
        with self._acquire_resources(requests):
            yield
```

节点注册时可声明 `resource_key` 和 `resource_limit`（如 `node_type:human` 限制为 1，确保同一时刻只有一个 human 节点等待用户输入）。

---

### 机制9：MacNet — 大规模 DAG 拓扑协作（研究分支）

**位置**：`macnet` 分支

MacNet 将链式编排扩展为**任意 DAG 拓扑**：
- 支持 tree、mesh、random 等多种拓扑生成
- 节点在拓扑序中执行，前驱解决方案通过**语言交互**（code diff + suggestions）传递给后继
- 支持 **>1000 个 agent** 协作而不超上下文限制（通过分层聚合）

Croto（Cross-Team Orchestration）进一步引入：
- **Greedy Aggregation**：贪婪选择最优子方案
- **Hierarchy Partitioning**：层次化任务分区
- **Pruning Strategy**：剪枝低效分支

```python
# graph.py (macnet branch)
def aggregate(self, prompt, retry_limit, unit_num, layer_directory, graph_depth, store_dir):
    self.pool = Pool(len(self.pre_solutions), unit_num, layer_directory, self.model)
    for i in range(retry_limit):
        new_codes = self.pool.state_pool_add(..., temperature=1 - self.depth / graph_depth)
        if new_codes:
            self.solution = new_codes
            return 0
```

---

### 机制10：Puppeteer — RL 动态编排（研究分支）

**位置**：`puppeteer` 分支

Puppeteer 引入**可学习的中央编排器**（Learnable Central Orchestrator）：
- 通过**强化学习**优化 agent 的激活顺序和选择
- 动态构建**上下文感知的推理路径**
- 提高推理质量的同时降低计算成本

---

## 4. 对 Mora 语言的借鉴建议

### 建议1：引入 `phase` 与 `composed_phase` 复合编排原语

**ChatDev 机制**：Phase 是带输入输出契约的编排单元，ComposedPhase 支持嵌套和循环终止条件。

**Mora 现状**：`orchestrate` 是扁平图编排，缺乏层级复合和条件循环。

**Mora 原语草案**：
```mora
// Phase 定义：带 break 条件的复合编排块
phase CodeReviewCycle {
    input: codes: string
    output: reviewed_codes: string
    break_when: output.contains("<INFO> Finished")
    max_iterations: 5
    
    node Reviewer = ai.chat(role: "code_reviewer", ...)
    node Fixer = ai.chat(role: "programmer", ...)
    
    edge Reviewer -> Fixer  carry_data: true
    edge Fixer -> Reviewer  condition: "not finished"
}

// ComposedPhase：将 Phase 作为子图节点使用
orchestrate DevWorkflow {
    node Design = ai.chat(...)
    node Coding = phase CodeReviewCycle  // 嵌套 Phase
    node Test = ai.chat(...)
    
    edge Design -> Coding
    edge Coding -> Test
}
```

---

### 建议2：引入 `map` / `tree` 动态边展开原语

**ChatDev 机制**：边级别声明 `dynamic_config: {type: map|tree, split: ...}`，实现并行 fan-out 和分层聚合。

**Mora 现状**：`orchestrate` 的边是静态 1:1 连接。

**Mora 原语草案**：
```mora
// Map 模式：将输入拆分为行，并行执行 Summarizer
orchestrate ParallelResearch {
    node Researcher = ai.chat(...)
    node Summarizer = ai.chat(...)
    
    edge Researcher -> Summarizer {
        dynamic: map
        split: by_line    // 按行拆分
        max_parallel: 10
    }
}

// Tree 模式：逐层聚合（如多数投票、方案合并）
orchestrate Consensus {
    node Proposer = ai.chat(...)  // 生成 N 个方案
    node Merger = ai.chat(...)    // 两两合并
    
    edge Proposer -> Merger {
        dynamic: tree
        split: by_item       // 按元素分组
        group_size: 3        // 每组合并 3 个
        max_parallel: 5
    }
}
```

---

### 建议3：引入 `memory.attach` 与阶段感知检索

**ChatDev 机制**：记忆挂载在 Agent 节点上，支持 `retrieve_stage` 指定 PRE_GEN / GEN / POST_GEN / FINISHED。

**Mora 现状**：有 `record` / `replay`，但缺乏细粒度阶段绑定和自动向量化检索。

**Mora 原语草案**：
```mora
memory experience_store: file("./experiences.json") with embedding

orchestrate CodingWithMemory {
    node Coder = ai.chat(role: "programmer", ...) {
        memory: experience_store {
            retrieve_at: [pre_gen, gen]   // 生成前和生成时检索
            write_at: finished              // 执行结束后写入
            top_k: 3
            similarity_threshold: 0.7
        }
    }
}
```

---

### 建议4：引入 `skill` 零代码行为扩展机制

**ChatDev 机制**：`.agents/skills/<name>/SKILL.md` 是 Markdown 指令文件，Agent 通过 `activate_skill` 工具动态加载到 system prompt。

**Mora 现状**：MCP 工具扩展是函数级，缺乏**行为知识**（how-to 指令）的声明式加载。

**Mora 原语草案**：
```mora
// 在 .agents/skills/deep-research/SKILL.md 中定义研究方法论
// Mora 运行时自动发现

orchestrate ResearchTask {
    node Researcher = ai.chat(...) {
        skills: ["deep-research", "citation-format"]  // 白名单
    }
}

// 运行时注入的工具
// activate_skill(skill_name: string) -> {instructions: string, allowed_tools: [...]}
// read_skill_file(skill_name: string, relative_path: string) -> string
```

---

### 建议5：引入 `retry_policy` 结构化配置

**ChatDev 机制**：`AgentRetryConfig` 支持 status_code、exception_type、error_substring、non_retryable 的多维匹配。

**Mora 现状**：可能有简单 retry，但缺乏结构化 DSL。

**Mora 原语草案**：
```mora
node APIAgent = ai.chat(provider: openai, ...) {
    retry_policy {
        max_attempts: 5
        backoff: exponential { min: 1s, max: 6s }
        retry_on_status: [429, 500, 502, 503]
        retry_on_exception: ["RateLimitError", "TimeoutError"]
        retry_on_message_contains: ["rate limit", "temporarily unavailable"]
        never_retry: ["AuthenticationError"]
    }
}
```

---

### 建议6：引入 `context_window` 与 `self_loop` 上下文管理

**ChatDev 机制**：`context_window` 控制节点输入保留策略，`pseudo_edge` 实现输出回送。

**Mora 现状**：缺乏节点级上下文窗口管理。

**Mora 原语草案**：
```mora
orchestrate ChainOfThought {
    node Thinker = ai.chat(...) {
        context_window: 5   // 保留最近 5 轮
        self_loop: true    // 输出自动回送为下一轮输入（直到条件满足）
        max_iterations: 20
    }
}
```

---

### 建议7：引入 `human` 节点作为一等公民

**ChatDev 机制**：`human` 节点类型暂停图执行，等待人工输入（CLI 或 Web）。

**Mora 现状**：`interrupt` 可能类似，但 `human` 节点在图中有明确类型、资源限制（`resource_limit: 1`）和输入模式。

**Mora 原语草案**：
```mora
orchestrate ReviewWorkflow {
    node Draft = ai.chat(...)
    node HumanReview = human {
        prompt: "请审阅上述草案并提供反馈（输入 'approve' 通过）"
        channel: cli  // 或 web / slack
        timeout: 300s
    }
    node Revise = ai.chat(...)
    
    edge Draft -> HumanReview
    edge HumanReview -> Revise  condition: "not approved"
    edge HumanReview -> END     condition: "approved"
}
```

---

### 建议8：引入 `thinking` 预/后生成钩子

**ChatDev 机制**：`ThinkingManager` 在 GEN 阶段前后插入反思/规划步骤。

**Mora 原语草案**：
```mora
orchestrate PlanningTask {
    node Planner = ai.chat(...) {
        thinking {
            pre_gen: reflection { prompt: "在回答前，先分析任务约束..." }
            post_gen: reflection { prompt: "生成后，检查是否遗漏..." }
        }
    }
}
```

---

### 风险/不适用项

| 项 | 风险 | 说明 |
|----|------|------|
| **RolePlaying 双角色对话** | ⚠️ 高 | 依赖 CAMEL 框架，对 Mora 来说太重。Mora 的 `ai.chat` 原语更适合单轮/多轮直接调用，双角色对话可通过两个 agent 节点 + 边循环模拟。 |
| **Vue 前端可视化** | ❌ 不适用 | 属于应用层，非语言原语。Mora 的编排可视化应由 IDE/CLI 工具链提供。 |
| **FileMemory / Mem0 集成** | ⚠️ 中 | 可直接借鉴，但 Mora 作为语言应提供 `memory` 抽象接口，具体后端（Mem0、Chroma、SQLite）由运行时插件实现。 |
| **MacNet 千级 Agent 拓扑** | ⚠️ 低 | 拓扑生成和 DAG 执行策略可借鉴，但千级规模对 Mora 当前架构可能过于超前。建议先支持 `map` / `tree` 动态边。 |
| **Puppeteer RL 编排器** | ⚠️ 高 | 研究性质，依赖 RL 训练循环。Mora 作为静态语言不适合内置 RL 运行时。可留作未来扩展点。 |
| **YAML 零代码配置** | ⚠️ 中 | ChatDev 的 YAML 配置非常庞大（如 ChatDev_v1.yaml 数百行）。Mora 是编程语言，不应追求零代码，但可支持从 YAML 导入子图作为 DSL 糖。 |

---

## 5. 与已有17项目的差异化

| 对比维度 | ChatDev | LangGraph | AIOS | mini-swe-agent | Mora (当前) |
|----------|---------|-----------|------|----------------|-------------|
| **编排范式** | 图编排（DAG+循环）+ Phase 链 | 图编排（StateGraph） | 内核级 Agent OS | 单 Agent 工具链 | 图编排（Flow） |
| **通信协议** | **Role-Playing 双角色对话** | 状态传递 | 系统调用 | 直接调用 | 消息传递 |
| **动态执行** | **边级 Map/Tree 动态展开** | 无原生支持 | 无 | 无 | 无 |
| **记忆机制** | **Stage-aware 挂载 + 多后端** | Checkpoint / MemorySaver | 无 | 无 | record/replay |
| **技能扩展** | **SKILL.md 行为知识加载** | Tool 节点 | 无 | 无 | MCP 工具 |
| **人类介入** | **human 节点（一等公民）** | interrupt | 无 | 无 | interrupt |
| **零代码** | **YAML 全配置** | Python 代码 | 不适用 | 不适用 | 脚本语言 |
| **规模扩展** | **MacNet 千级 Agent DAG** | 有限 | 单 Agent | 单 Agent | 小规模 |
| **RL 编排** | **Puppeteer 动态调度** | 无 | 无 | 无 | 无 |
| **跨团队** | **Croto 贪婪聚合+剪枝** | 无 | 无 | 无 | 无 |
| **自反思** | Phase 内置 + Thinking 钩子 | 需手动构建 | 无 | 无 | 无 |
| **资源协调** | **Semaphore 节点级并发** | 无 | 内核调度 | 无 | 无 |

### ChatDev 的独特之处（对 Mora 最有价值的 3 点）

1. **边级动态展开（Map/Tree）**：这是 ChatDev 2.0 最具工程价值的设计。将并行和聚合从**节点内部**移到**边配置**上，实现了声明式分布式计算。Mora 引入 `map` / `tree` 边修饰符可大幅提升大规模数据处理和多方案生成-聚合的表达力。

2. **Stage-Aware Memory Attachment**：记忆的挂载粒度到**节点**和**阶段**，而非全局。这使 Agent 能在正确的时间（思考前、生成时、反思后）检索正确的记忆。Mora 的 `memory.attach { retrieve_at: [...] }` 可借鉴此精细化设计。

3. **Agent Skills（Markdown 行为知识）**：与 MCP 的函数工具互补，Skill 扩展的是**方法论**（how to do）而非**功能**（what to call）。Mora 作为 AI-native 语言，内置 `skill` 原语可使 Agent 的"专业能力"成为语言级一等公民。

---

## 6. 结论

ChatDev 是**从学术原型（1.0）到工程平台（2.0）成功演进**的典范。其对 Mora 语言设计的核心启示可归纳为：

| 优先级 | 借鉴项 | 实现难度 | 价值评估 |
|--------|--------|----------|----------|
| P0 | `map` / `tree` 动态边展开 | 中 | 极大提升编排表达力 |
| P0 | `memory.attach` 阶段感知挂载 | 中 | 精细化记忆管理 |
| P1 | `phase` / `composed_phase` 复合编排 | 中 | 层级抽象，降低复杂工作流冗余 |
| P1 | `skill` 零代码行为扩展 | 低 | 与 MCP 互补，扩展 Agent 专业能力 |
| P1 | `human` 节点一等公民 | 低 | 改进人机协作体验 |
| P2 | `retry_policy` 结构化 DSL | 低 | 提升可靠性 |
| P2 | `context_window` + `self_loop` | 低 | 上下文管理 |
| P2 | `thinking` 预/后钩子 | 低 | 内建反思能力 |
| P3 | MacNet 拓扑生成 | 高 | 未来扩展点 |
| P3 | Puppeteer RL 编排 | 高 | 研究性质，暂缓 |

---

*报告结束。*
