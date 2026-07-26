# OpenBMB/ChatDev 

> 2026-07-07
> ChatDev 2.0 (DevAll) main  + ChatDev 1.0 (Legacy) chatdev1.0  + MacNet/Croto/Puppeteer 
> AI Agent 

---

## 1. 

|  |  |
|------|------|
| **GitHub Stars** | 33.7k |
| **** | Python (68.6%) + Vue (28.6%) |
| **** | ****ChatDev 2.0 DevAllChatDev 1.0 |
| **** | Apache-2.0 |
| **** | ChatDev (arXiv:2307.07924), MacNet (ICLR 2025), Puppeteer (NeurIPS 2025), Croto (arXiv:2406.08979) |

ChatDev ********
- **1.0** `Phase`  +  `RolePlaying`  + 
- **MacNet**DAG  agent 
- **Croto**Cross-Team Orchestration
- **Puppeteer**RL 
- **2.0 (DevAll)**YAML  `agent` / `human` / `subgraph` / `python` / `loop` 

---

## 2. 

### 2.1 2.0 DevAll

```
ChatDev/
 server/           # FastAPI REST API + WebSocket
 frontend/         # Vue 3 Workflow / Launch / Tutorial
 runtime/          # 
    node/         # agent, human, python, passthrough, literal, loop, subgraph
       executor/       # AgentNodeExecutor, HumanNodeExecutor...
       agent/          # LLM MemoryThinkingSkills
          memory/     # MemoryBase / MemoryManagerSimple/File/Blackboard/Mem0
          thinking/   # ThinkingManagerPre/Post Generation Reflection
          skills/     # AgentSkillManagerSKILL.md 
          providers/  # OpenAI / Gemini / 
       registry.py     # 
    edge/         # 
        conditions/     # FunctionEdge / KeywordEdge
        processors/     # Payload 
 workflow/         # 
    graph.py            # GraphExecutorDAG//
    graph_context.py    # GraphContext
    graph_manager.py    # GraphManager
    cycle_manager.py    # CycleManager
    runtime.py          # RuntimeBuilder + RuntimeContext
    executor/           #  +  + 
        resource_manager.py   # Semaphore 
        dynamic_edge_executor.py  # Map/Tree 
 entity/           # dataclass + schema
    configs/      # Node/Edge/Graph/Memory/Thinking/Skill 
    messages.py   # Message / MessageBlock / AttachmentRef / ToolCallPayload
    enums.py      # Role, Stage, InputMode, LogLevel...
 functions/        #  Python 
 schema_registry/  #  schema Provider/Memory/Thinking
 yaml_instance/    # ChatDev_v1.yaml, deep_research_v1.yaml...
```

### 2.2 

#### Graph Model
ChatDev 2.0 **** DAG + 

- **Node**`agent`LLM `human``subgraph``python``passthrough``literal``loop_counter` / `loop_timer`
- **Edge** `condition``function` / `keyword` `carry_data``keep_message``clear_context`****`dynamic_config`
- **GraphContext**

#### Execution Flow
```
TaskInput → GraphExecutor.execute_graph()
  → GraphManager.build_graph() [ / ]
  → _build_memories_and_thinking() []
  → 
      - DAG → DagExecutionStrategy
      - Cycle → CycleExecutionStrategy
      - MajorityVoting → MajorityVoteStrategy
  → _execute_node(node) 
      →  dynamic_configMap/Tree
      → NodeExecutor.execute() [AgentNodeExecutor / HumanNodeExecutor / ...]
      → _process_edge_output() [ + ]
  → _collect_all_outputs() + _save_memories() + ResultArchiver.export()
```

### 2.3 Message System

ChatDev ****`entity/messages.py`

```python
@dataclass
class Message:
    role: MessageRole       # system / user / assistant / tool
    content: MessageContent  # str | List[MessageBlock] | List[Dict]
    name: Optional[str]
    tool_call_id: Optional[str]
    metadata: Dict[str, Any]
    tool_calls: List[ToolCallPayload]
    keep: bool = False           # 
    preserve_role: bool = False

@dataclass
class MessageBlock:
    type: MessageBlockType  # text / image / audio / video / file / data
    text: Optional[str]
    attachment: Optional[AttachmentRef]  # /
    data: Dict[str, Any]
```

****`AttachmentRef`  `local_path``remote_file_id``data_uri`base64******LLM **`FunctionCallOutputEvent` `MessageBlock` 

---

## 3. 

### 1Role-Playing Communication Protocol

****`chatdev1.0/chatdev/phase.py` CAMEL 

ChatDev 1.0 **** `Phase`  agent  `assistant_role`  `user_role` ****

```python
class Phase(ABC):
    def chatting(self, chat_env, task_prompt, assistant_role_name, user_role_name, ...):
        #  RolePlaying 
        role_play_session = RolePlaying(
            assistant_role_name=assistant_role_name,
            user_role_name=user_role_name,
            assistant_role_prompt=...,
            user_role_prompt=...,
            task_prompt=task_prompt,
            ...
        )
        #  seminar_conclusion  chat_turn_limit
        for i in range(chat_turn_limit):
            assistant_response, user_response = role_play_session.step(input_user_msg, ...)
            if assistant_response.msg.info:  #  <INFO> 
                seminar_conclusion = assistant_response.msg.content
                break
```

****`seminar_conclusion`  Phase **** `<INFO> Finished` `update_chat_env()` ********

---

### 2ComposedPhase + Cycle Breaking

****`chatdev1.0/chatdev/composed_phase.py`

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

`break_cycle()` **** `CodeCompleteAll`  `unimplemented_file == ""` `CodeReview`  `"Finished" in modification_conclusion` ****

---

### 3Dynamic Edge Expansion — Map / Tree

****`workflow/executor/dynamic_edge_executor.py`

ChatDev 2.0 ****

```python
class DynamicEdgeConfig(BaseConfig):
    type: str              # "map" or "tree"
    split: SplitConfig     #  JSON 
    config: BaseConfig       # MapDynamicConfig / TreeDynamicConfig
```

- **Map ** N ****fan-out
- **Tree **`group_size`****reducefan-out + reduce

```python
# Tree  reduction 
while len(current_messages) > 1:
    groups = group_messages(current_messages, group_size)
    # 
    with ThreadPoolExecutor(max_workers=max_parallel) as executor:
        for idx, group in enumerate(groups):
            future = executor.submit(self._execute_group, node, group_inputs, layer, idx)
    current_messages = layer_outputs  # 
```

******** `dynamic_config: {type: tree, split: {...}}` MapReduce /**** Agent 

---

### 4Agent Agent Skills

****`runtime/node/agent/skills/` + `entity/configs/node/skills.py`

ChatDev ****

```python
#  .agents/skills/<skill_name>/SKILL.md 
DEFAULT_SKILLS_ROOT = REPO_ROOT / ".agents" / "skills"

# Agent  skills
skills:
  enabled: true
  allow: ["deep-research", "code-review"]  # 

#  LLM 
- name: activate_skill      #  SKILL.md 
- name: read_skill_file    # 
```

****Skill **Markdown **SKILL.mdAgent  `activate_skill`  system prompt** Agent ** MCP  tool ——********

---

### 5Stage-Aware Memory Attachment

****`runtime/node/agent/memory/memory_base.py` + `entity/configs/node/memory.py`

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

**Agent **
- **PRE_GEN_THINKING**
- **GEN**
- **POST_GEN_THINKING**
- **FINISHED**

****`MemoryManager._score_memory`************
```python
def _score_memory(self, memory_item, query):
    age_hours = (current_time - memory_item.timestamp) / 3600
    time_decay = max(0.1, 1.0 - age_hours / (24 * 30))
    length_factor = ...  #  0.5 0.8 1.0
    relevance = len(query_words & content_words) / len(query_words)
    return 0.7 * time_decay * length_factor + 0.3 * relevance
```

---

### 6Pseudo Edge + Context Window

****`workflow/graph.py``_execute_node` 

```python
#  pseudo_edge 
if node.context_window != 0 and not context_restored:
    pseudo_condition = EdgeConditionConfig.from_dict("true", ...)
    pseudo_link = EdgeLink(target=node, trigger=False)
    pseudo_link.condition_config = pseudo_condition
    for output_msg in output_messages:
        self._process_edge_output(pseudo_link, output_msg, node)
```

 `context_window`
- `-1`
- `0`
- `N > 0` N 

`pseudo_edge` **** Agent  Chain-of-Thought 

---

### 7 DSLRetry Policy DSL

****`entity/configs/node/agent.py``AgentRetryConfig`

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

**** `ExceptionGroup`**HTTP ************** `tenacity` 

---

### 8Resource Manager

****`workflow/executor/resource_manager.py`

****`threading.Semaphore`

```python
class ResourceManager:
    def guard_node(self, node: Node):
        requests = self._resolve_node_requests(node)  #  NodeCapabilities 
        with self._acquire_resources(requests):
            yield
```

 `resource_key`  `resource_limit` `node_type:human`  1 human 

---

### 9MacNet —  DAG 

****`macnet` 

MacNet ** DAG **
-  treemeshrandom 
- ****code diff + suggestions
-  **>1000  agent** 

CrotoCross-Team Orchestration
- **Greedy Aggregation**
- **Hierarchy Partitioning**
- **Pruning Strategy**

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

### 10Puppeteer — RL 

****`puppeteer` 

Puppeteer ****Learnable Central Orchestrator
- **** agent 
- ****
- 

---

## 4.  Mora 

### 1 `phase`  `composed_phase` 

**ChatDev **Phase ComposedPhase 

**Mora **`orchestrate` 

**Mora **
```mora
// Phase  break 
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

// ComposedPhase Phase 
orchestrate DevWorkflow {
    node Design = ai.chat(...)
    node Coding = phase CodeReviewCycle  //  Phase
    node Test = ai.chat(...)
    
    edge Design -> Coding
    edge Coding -> Test
}
```

---

### 2 `map` / `tree` 

**ChatDev ** `dynamic_config: {type: map|tree, split: ...}` fan-out 

**Mora **`orchestrate`  1:1 

**Mora **
```mora
// Map  Summarizer
orchestrate ParallelResearch {
    node Researcher = ai.chat(...)
    node Summarizer = ai.chat(...)
    
    edge Researcher -> Summarizer {
        dynamic: map
        split: by_line    // 
        max_parallel: 10
    }
}

// Tree 
orchestrate Consensus {
    node Proposer = ai.chat(...)  //  N 
    node Merger = ai.chat(...)    // 
    
    edge Proposer -> Merger {
        dynamic: tree
        split: by_item       // 
        group_size: 3        //  3 
        max_parallel: 5
    }
}
```

---

### 3 `memory.attach` 

**ChatDev ** Agent  `retrieve_stage`  PRE_GEN / GEN / POST_GEN / FINISHED

**Mora ** `record` / `replay`

**Mora **
```mora
memory experience_store: file("./experiences.json") with embedding

orchestrate CodingWithMemory {
    node Coder = ai.chat(role: "programmer", ...) {
        memory: experience_store {
            retrieve_at: [pre_gen, gen]   // 
            write_at: finished              // 
            top_k: 3
            similarity_threshold: 0.7
        }
    }
}
```

---

### 4 `skill` 

**ChatDev **`.agents/skills/<name>/SKILL.md`  Markdown Agent  `activate_skill`  system prompt

**Mora **MCP ****how-to 

**Mora **
```mora
//  .agents/skills/deep-research/SKILL.md 
// Mora 

orchestrate ResearchTask {
    node Researcher = ai.chat(...) {
        skills: ["deep-research", "citation-format"]  // 
    }
}

// 
// activate_skill(skill_name: string) -> {instructions: string, allowed_tools: [...]}
// read_skill_file(skill_name: string, relative_path: string) -> string
```

---

### 5 `retry_policy` 

**ChatDev **`AgentRetryConfig`  status_codeexception_typeerror_substringnon_retryable 

**Mora ** retry DSL

**Mora **
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

### 6 `context_window`  `self_loop` 

**ChatDev **`context_window` `pseudo_edge` 

**Mora **

**Mora **
```mora
orchestrate ChainOfThought {
    node Thinker = ai.chat(...) {
        context_window: 5   //  5 
        self_loop: true    // 
        max_iterations: 20
    }
}
```

---

### 7 `human` 

**ChatDev **`human` CLI  Web

**Mora **`interrupt`  `human` `resource_limit: 1`

**Mora **
```mora
orchestrate ReviewWorkflow {
    node Draft = ai.chat(...)
    node HumanReview = human {
        prompt: " 'approve' "
        channel: cli  //  web / slack
        timeout: 300s
    }
    node Revise = ai.chat(...)
    
    edge Draft -> HumanReview
    edge HumanReview -> Revise  condition: "not approved"
    edge HumanReview -> END     condition: "approved"
}
```

---

### 8 `thinking` /

**ChatDev **`ThinkingManager`  GEN /

**Mora **
```mora
orchestrate PlanningTask {
    node Planner = ai.chat(...) {
        thinking {
            pre_gen: reflection { prompt: "..." }
            post_gen: reflection { prompt: "..." }
        }
    }
}
```

---

### /

|  |  |  |
|----|------|------|
| **RolePlaying ** |   |  CAMEL  Mora Mora  `ai.chat` / agent  +  |
| **Vue ** |   | Mora  IDE/CLI  |
| **FileMemory / Mem0 ** |   |  Mora  `memory` Mem0ChromaSQLite |
| **MacNet  Agent ** |   |  DAG  Mora  `map` / `tree`  |
| **Puppeteer RL ** |   |  RL Mora  RL  |
| **YAML ** |   | ChatDev  YAML  ChatDev_v1.yaml Mora  YAML  DSL  |

---

## 5. 17

|  | ChatDev | LangGraph | AIOS | mini-swe-agent | Mora () |
|----------|---------|-----------|------|----------------|-------------|
| **** | DAG++ Phase  | StateGraph |  Agent OS |  Agent  | Flow |
| **** | **Role-Playing ** |  |  |  |  |
| **** | ** Map/Tree ** |  |  |  |  |
| **** | **Stage-aware  + ** | Checkpoint / MemorySaver |  |  | record/replay |
| **** | **SKILL.md ** | Tool  |  |  | MCP  |
| **** | **human ** | interrupt |  |  | interrupt |
| **** | **YAML ** | Python  |  |  |  |
| **** | **MacNet  Agent DAG** |  |  Agent |  Agent |  |
| **RL ** | **Puppeteer ** |  |  |  |  |
| **** | **Croto +** |  |  |  |  |
| **** | Phase  + Thinking  |  |  |  |  |
| **** | **Semaphore ** |  |  |  |  |

### ChatDev  Mora  3 

1. **Map/Tree** ChatDev 2.0 ********Mora  `map` / `tree` -

2. **Stage-Aware Memory Attachment********** Agent Mora  `memory.attach { retrieve_at: [...] }` 

3. **Agent SkillsMarkdown ** MCP Skill ****how to do****what to callMora  AI-native  `skill`  Agent ""

---

## 6. 

ChatDev **1.02.0** Mora 

|  |  |  |  |
|--------|--------|----------|----------|
| P0 | `map` / `tree`  |  |  |
| P0 | `memory.attach`  |  |  |
| P1 | `phase` / `composed_phase`  |  |  |
| P1 | `skill`  |  |  MCP  Agent  |
| P1 | `human`  |  |  |
| P2 | `retry_policy`  DSL |  |  |
| P2 | `context_window` + `self_loop` |  |  |
| P2 | `thinking` / |  |  |
| P3 | MacNet  |  |  |
| P3 | Puppeteer RL  |  |  |

---

**
