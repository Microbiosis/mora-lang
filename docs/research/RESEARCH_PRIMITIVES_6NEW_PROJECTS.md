# Mora  Agent  — 6 

> ****: 2026-07-07  
> ****: 6 google/agents-cli, langchain-ai/langchain, langchain-ai/langgraph, langgraph4j/langgraph4j, luochang212/dive-into-langgraph, OpenBMB/ChatDev  
> ****:  17 RESEARCH_PRIMITIVES_MASTER_v2.md, v0.41-v0.48   
> ****:  mora ********/

---

## 0. 

 6  Agent  170k+ GitHub Stars 17 ** 12  mora ** **P05 P14 P23 **


- **LangChain/LangGraph **90k+35k stars Agent  **Pregel BSP Checkpoint Command Channel+Reducer **  mora `orchestrate` 
- **agents-cli**4.2k starsGoogle  **Agent  DevOps **——Eval-first 8 Skill-as-Code  mora 
- **ChatDev**33.7k stars** + **——Map/TreeSKILL.md  mora 
- **dive-into-langgraph******

---

## 1. 

### 1.1 google/agents-cli — Agent  DevOps 

|  |  |  Mora  |
|------|----------|---------------|
| **** | 8 Spec → Scaffold → Build → Orchestrate → Eval → Deploy → Publish → Observe | mora  `mora init` / `mora eval` / `mora deploy`  |
| **** | ADK Workflow  +  + JoinNode +  Worker | `workflow`  fan-out/fan-in  |
| **** | Eval-first LLM-as-judge +  +  | `eval`  `test`  |
| **** | State Prefix Namespacesession/user/app/temp  | `state`  |
| **** | Tool Confirmation Gates +  IAM  | `capability`  |
| **** | Session Rewind invocation | `rewind` / `replay`  |
| **** | Ambient AgentPub/Sub + Cron  +  | `ambient`  |
| ** Agent** | A2A Protocol + Agent Card  | `a2a`  |

### 1.2 langchain-ai/langchain — 

|  |  |  Mora  |
|------|----------|---------------|
| **** | `Runnable` invoke/batch/stream/ainvoke+ `\|`  |  Python  |
| **** | Message-Centric`content`  `str` + `list[dict]`  | `ai.chat`  |
| **Schema ** | `InjectedToolArg`  LLM  | `injected`  LLM schema  |
| **** | Retry/Timeout/Cache  | `@retry` / `@timeout` / `@cache`  |

### 1.3 langchain-ai/langgraph — 

|  |  |  Mora  |
|------|----------|---------------|
| **** | Pregel BSPBulk Synchronous Parallel | `orchestrate`  |
| **** | `Channel` + `Reducer`LastValue/Topic/BinaryOperatorAggregate | `@reduce(append\|add\|last\|custom)`  |
| **** | `channel_values` + `versions_seen` | `orchestrate @checkpoint(saver: ...)` |
| **** | `Command(goto=..., update=..., resume=...)`  | `command`  |
| **** | `interrupt()` + `Command(resume=...)` | `interrupt` v0.34  |
| **** | `Send`  Map-Reduce  | `spawn` / `send`  |
| **** | `get_state()` / `update_state()`  | `mora replay --fork`  |
| **** |  `checkpoint_ns`  | `subgraph`  |

### 1.4 langgraph4j/langgraph4j — Java 

|  |  |  Mora  |
|------|----------|---------------|
| **Schema ** | `Channel` // |  LangGraph  Channel+Reducer **** |
| **** | Memory/MySQL/PostgreSQL/Redis/DynamoDB  8  | mora **** |
| **Interrupt** | `CompileConfig.interruptsBefore()` + `InterruptionMetadata` | **** |
| **Subgraph ** | `ProcessedNodesEdgesAndConfig.process()`  |  |

### 1.5 luochang212/dive-into-langgraph — 

|  |  |  Mora  |
|------|----------|---------------|
| **** | `@before_model`, `@wrap_model_call`, `@dynamic_prompt`  8  | `middleware`  |
| **** | Runtime/ State/ Store |  |
| **** | `@dynamic_prompt`  State/Store  system prompt | `dynamic_prompt`  |
| **** | `SummarizationMiddleware`  | `context_policy { on_overflow: summarize }` |
| **MCP ** | `supervisord`  MCP  |  |

### 1.6 OpenBMB/ChatDev — 

|  |  |  Mora  |
|------|----------|---------------|
| **** | `edge`  `dynamic_config: {type: map\|tree, split: ...}` | `edge ... { dynamic: map, split: by_line }` |
| **** | `retrieve_stage: [pre_gen, gen, post_gen, finished]` | `memory: store { retrieve_at: [...], write_at: ... }` |
| **** | `phase` + `break_cycle`  | `phase ... { break_when: ..., max_iterations: 5 }` |
| **Skill ** | `activate_skill`  `.agents/skills/<name>/SKILL.md`  | `skills: ["deep-research"]`  |
| **Human ** | `human`  | `node Review = human { ... }` |
| ** DSL** | status_code / exception_type / error_substring / non_retryable  | `retry_policy { retry_on_status: [429, 503] }` |
| **** | `pseudo_edge` + `context_window`  | `context_window: 5, self_loop: true` |
| ** Agent** | MacNet  +  |  |
| **RL ** | Puppeteer  |  |

---

## 2.  17 

### 2.1 v0.41-v0.48

|  |  |  |
|------|------|------|
| `capability` / `policy` / `audit` | loongclaw |   |
| `exec.bash` +  | mini-swe-agent |   |
| `ai.retry` | mini-swe-agent |   |
| `interrupt` (5 ) | mini-swe-agent |   |
| `3-mode` (human/confirm/yolo) | mini-swe-agent |   |
| `SKILL.md`  | CLI-Anything |   |
| `tool_conflict_map` | AIOS |   |
| `mimiclaw` Cron + ReAct | mimiclaw |   |
| `sandbox`  |  |   |
| `observe` / `span` / `record_tokens` |  |   |
| `record` / `replay` / `diff` |  |   |
| `orchestrate`  |  |   |
| `refine`  |  |   |
| `semaphore` |  |   |

### 2.2 

 6  ×  17 ****

#### 6 

| # |  |  |  |
|---|------|------|-------------|
| 1 | **Pregel BSP ** | LangGraph |  AIOS  |
| 2 | **Channel + Reducer ** | LangGraph / LangGraph4j |  loongclaw  |
| 3 | **Command ** | LangGraph |  mini-swe-agent  interrupt  |
| 4 | **Send  Map-Reduce** | LangGraph |   |
| 5 | **Checkpoint ** | LangGraph / LangGraph4j |  `record` `checkpoint`  |
| 6 | **Eval-first ** | agents-cli |   Agent  |
| 7 | **A2A  Agent ** | agents-cli |   |
| 8 | **State Prefix Namespace** | agents-cli |   |
| 9 | **Session Rewind ** | agents-cli |  `replay` `rewind`  |
| 10 | **Ambient Agent ** | agents-cli |   |
| 11 | **Map/Tree** | ChatDev |   |
| 12 | **** | ChatDev |   |
| 13 | **Middleware ** | dive-into-langgraph |   |
| 14 | **** | dive-into-langgraph |  / |
| 15 | **** | dive-into-langgraph |   |
| 16 | **** | dive-into-langgraph |   |
| 17 | **InjectedToolArg ** | LangChain |   |
| 18 | **human ** | ChatDev |  3-mode  human  |
| 19 | **Skill ** | ChatDev |   SKILL.md  `activate_skill` |
| 20 | **Retry/Timeout/Cache ** | LangGraph / LangChain |   `ai.retry` |

### 2.3 

 20 **** 12 

---

## 3. 12 

### P0-1:  + ReducerState Channel + Reducer

****: LangGraph, LangGraph4j, ChatDev  
****:  `orchestrate` ****  
****:  Schema 

```mora
//  messages
orchestrate my_flow {
  state: { messages: [Message] }
  node A -> messages = [...]   // 
  node B -> messages = [...]   // 
}

//  Reducer
orchestrate my_flow {
  state: {
    messages: [Message] @append,      //  = 
    total_cost: number @add,          //  = 
    last_decision: string @last,      //  = 
    context: Context @merge(fn(old, new) -> ...)
  }
  node A -> messages = [...]   // 
  node B -> messages = [...]   // 
}
```

** P0**: LangGraph  Pregel BSP 

---

### P0-2: Checkpoint Persistence

****: LangGraph, LangGraph4j, agents-cli  
****:  `record` ****`replay` ************  
****:  `checkpoint`  `orchestrate` 

```mora
// 
orchestrate booking_flow @checkpoint(saver: "sqlite", thread: "user_123") {
  state: { ... }
  node check_availability
  node confirm_booking
  interrupt before confirm_booking  // 
}

// 
let result = booking_flow.resume(thread: "user_123", as_of: "confirm_booking")

// 
booking_flow.update_state(thread: "user_123", as_of: "check_availability", {
  dates: ["2026-08-01"]
})

// agents-cli  rewind
booking_flow.rewind(before_invocation: "confirm_booking")
```

** P0**:  Human-in-the-Loop LangGraph  checkpoint 

---

### P0-3: Command Command Dynamic Control Flow

****: LangGraph, ChatDev  
****:  `orchestrate` ****  
****:  `Command` 

```mora
// 
orchestrate my_flow {
  node classifier -> node A when output == "urgent"
  node classifier -> node B when output == "normal"
  node classifier -> node C when output == "spam"
}

// 
node classifier {
  let result = ai.chat p"Classify: {input}".tool("classify")
  
  //  + 
  return command {
    goto: result.category,
    update: { priority_score: result.confidence }
  }
}

// return { goto: "A", update: { priority_score: 0.9 } }
```

** P0**:  Supervisor/Swarm LangGraph  `Command(goto=...)`  Agent handoff 

---

### P0-4: Dynamic Dispatch / Send

****: LangGraph, ChatDev  
****:  `orchestrate` ****  
****:  `send` / `spawn`  Map-Reduce

```mora
// Map-Reduce 
node split_tasks {
  let tasks = ai.chat p"Split into subtasks: {goal}".tool("split")
  
  //  N 
  return tasks.map(t => send("process_task", { task: t }))
}

node process_task {
  input: { task: Task }
  let result = ai.chat p"Process: {task}"
  return { partial_result: result }
}

node join_results {
  //  process_task 
  input: { partial_results: [Result] @append }  // Reducer 
  let summary = ai.chat p"Summarize: {partial_results}"
  return { final: summary }
}

// split_tasks -> process_task 
edge split_tasks -> process_task { dynamic: map }
edge process_task -> join_results { dynamic: reduce }
```

** P0**: ChatDev  `dynamic_edge_executor.py`  LangGraph  `Send` 

---

### P0-5: Eval Eval-first Quality Gate

****: agents-cli  
****:  `test`  **Agent **  
****:  `eval`  `test` 

```mora
// 
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

//  Agent
eval run BookingEval on booking_agent {
  threshold: 0.85
  iterations: 5
}

// 
deploy booking_agent {
  gate: eval BookingEval >= 0.85
  target: cloud_run
}
```

** P0**: agents-cli  **"Eval-first, not test-first"**——Agent  mora """Agent "

---

### P1-6: State Prefix Namespace

****: agents-cli  
****:   
****: 

```mora
// 
let state = { step: 2, language: "zh", total_queries: 1000 }

// 
let state.step = 2                    // session  ephemeral
let state.user:preferred_language = "zh"  // user 
let state.app:total_queries += 1      // app 
let state.temp:intermediate = data   // temp 

//  orchestrate 
orchestrate my_flow {
  state: {
    "user:profile": UserProfile @persistent,   // 
    "app:metrics": Metrics @persistent,        // 
    "temp:draft": Draft @ephemeral             // 
  }
}
```

** P1**:  Agent agents-cli  ADK  session/user/app 

---

### P1-7: Middleware Middleware Pipeline

****: dive-into-langgraph  
****:  `ai.chat` PII   
****: 

```mora
//  ai.chat 
middleware global {
  before_model: budget_guard { max_tokens: 10000, max_cost_usd: 0.50 }
  before_model: pii_filter { mask: ["credit_card", "ssn"] }
  wrap_model_call: latency_logger { metric_name: "llm_latency" }
  after_model: context_compressor { on_overflow: summarize, max_messages: 20 }
}

//  orchestrate 
orchestrate customer_support {
  middleware: [
    dynamic_prompt {                       //  system prompt
      template: p"You are a {tone} support agent. User tier: {tier}."
      bind: { tone: state.user:tone, tier: state.user:tier }
    }
  ]
  node handle_request
}
```

** P1**:  Agent  LangGraph  `@before_model`  8  mora  `ai.chat` 

---

### P1-8: Injected Tool Arguments

****: LangChain  
****:  LLM schema**** LLM   
****: `injected` 

```mora
//  LLM schema LLM  user_id
fn query_db(sql: string, user_id: string) -> Result { ... }

// injected  LLM 
fn query_db(sql: string, user_id: string with injected) -> Result {
  // user_id  LLM  tool schema 
  // LLM  sql 
  ...
}

// 
let result = ai.chat p"Query active users" with tools=[query_db]
  where query_db.user_id = current_user.id  // 
```

** P1**: LangChain  `InjectedToolArg`  LLM /

---

### P1-9: A2A  Agent A2A Protocol

****: agents-cli  
****:  mora  Agent /  
****:  Google A2A 

```mora
//  Agent
a2a remote_scanner {
  card_url: "https://scanner.internal/.well-known/agent.json"
  capabilities: ["scan", "report"]
  auth: oauth2 { scope: "scanner:read" }
}

//  orchestrate 
orchestrate security_audit {
  node local_analysis -> remote_scanner.a2a {  //  Agent
    input: { target: state.target }
  }
  node report
}
```

** P1**: A2A  v0.9.1  mora agents-cli 

---

### P2-10: Ambient Agent Ambient / Event-Driven Agent

****: agents-cli  
****:  mora  Agent **-**/  
****:  Agent 

```mora
//  Agent
ambient nightly_report {
  trigger: cron("0 20 * * *")       //  8 
  agent: report_generator
  max_concurrent: 4
  retry: exponential { max: 5, base: 1min }
  on_failure: notify("ops@company.com")
}

// K8s CronJob / Cloud Scheduler / Eventarc 
```

** P2**:  cron/ K8s CronJob""""

---

### P2-11: Stage-Aware Memory Attachment

****: ChatDev  
****:  `memory`   
****: +

```mora
//  orchestrate 
orchestrate code_review {
  node reviewer {
    memory: store {
      retrieve_at: [pre_gen]          // 
      write_at: [finished]            // 
      scoring: { time_decay: 0.9, length_factor: 0.1 }  // 
    }
    let review = ai.chat p"Review this code: {code}"
    return { review }
  }
}
```

** P2**:  +  v0.50+ 

---

### P2-12: Context Compression Policy

****: dive-into-langgraph  
****:  `ai.chat`  token   
****: 

```mora
// 
context_policy global {
  max_messages: 20
  max_tokens: 10000
  on_overflow: summarize          // 
  // on_overflow: truncate_oldest
  // on_overflow: raise_error
}

// 
node long_conversation {
  context_policy: { max_messages: 50, on_overflow: summarize }
  let response = ai.chat p"{state.long_context}"
}
```

** P2**:  LangGraph  `SummarizationMiddleware` 

---

## 4. 

### v0.50 — P0 

|  |  |  |  |
|------|----------|----------|------|
| Channel + Reducer |  `orchestrate` state  +  |  |  orchestrate |
| Checkpoint |  `CheckpointSaver` trait + SQLite/ |  | Channel + Reducer |
| Command |  +  |  |  orchestrate |
| Dynamic Dispatch |  `Send`  +  |  | Command + Reducer |

### v0.51 — P0-5 + P1-6

|  |  |  |  |
|------|----------|----------|------|
| Eval |  `eval`  + LLM-as-judge  |  |  test  |
| State Namespace |  `state`  +  |  |  state |

### v0.52 — P1-7 + P1-8

|  |  |  |  |
|------|----------|----------|------|
| Middleware |  +  |  |  ai.chat |
| Injected Args |  schema  +  |  |  tool  |

### v0.53+ — P1-9 + P2-10/11/12

|  |  |  |  |
|------|----------|----------|------|
| A2A |  + Agent Card  |  |  |
| Ambient | K8s CronJob/Cloud Scheduler |  |  |
| Stage Memory |  +  |  |  |
| Context Compression |  +  |  | Middleware |

---

## 5.  17 

 23 17 + 6**** mora 

|  |  | mora  |  |
|------|----------|----------|------|
| **Capability ** | loongclaw | `capability`, `policy`, `audit` |  v0.34 |
| **Sandbox ** | mini-swe-agent, headroom | `exec.bash`, `sandbox.spawn` |  v0.34 |
| **Interrupt ** | mini-swe-agent | `interrupt FormatError { ... }` |  v0.34 |
| **Human-in-the-Loop** | mini-swe-agent, ChatDev | `3-mode`, `human`  |  v0.34 |
| **Skill ** | CLI-Anything, ChatDev | `SKILL.md`  |  v0.41 |
| **** |  | `orchestrate` |  v0.41 |
| **** | AIOS | `tool_conflict_map` |  v0.41 |
| **Cron ** | mimiclaw | `cron`  |  v0.41 |
| **** |  | `observe`, `span`, `record_tokens` |  v0.41 |
| **** |  | `record`, `replay`, `diff` |  v0.41 |
| **** |  | `semaphore` |  v0.49 |
| **Channel + Reducer** | LangGraph, LangGraph4j | **P0-1** | ⏳ v0.50 |
| **Checkpoint ** | LangGraph, LangGraph4j | **P0-2** | ⏳ v0.50 |
| **Command ** | LangGraph | **P0-3** | ⏳ v0.50 |
| **** | LangGraph, ChatDev | **P0-4** | ⏳ v0.50 |
| **Eval ** | agents-cli | **P0-5** | ⏳ v0.51 |
| **State Namespace** | agents-cli | **P1-6** | ⏳ v0.51 |
| **Middleware ** | dive-into-langgraph | **P1-7** | ⏳ v0.52 |
| **Injected Args** | LangChain | **P1-8** | ⏳ v0.52 |
| **A2A ** | agents-cli | **P1-9** | ⏳ v0.53 |
| **Ambient Agent** | agents-cli | **P2-10** | ⏳ v0.53+ |
| **Stage Memory** | ChatDev | **P2-11** | ⏳ v0.53+ |
| **Context Compression** | dive-into-langgraph | **P2-12** | ⏳ v0.53+ |

---

## 6. 

1. **A2A **: Google  v0.9.1 
2. **Context Compression**:  LLM 
3. **Ambient Agent**: K8s/Cloud Schedulermora 
4. **LangGraph **: LangGraph v0.6 → v1.0  breaking change API 
5. **ChatDev **: MacNet / Puppeteer 
6. **agents-cli  GCP**: Eval-firstAgent IdentityContext Caching  Google Cloud

---

## 7. 

|  | Stars |  |  |  |
|------|-------|------|----------|----------------|
| google/agents-cli | 4.2k | Python | Agent  DevOps  | Eval-first, State Namespace, A2A, Ambient |
| langchain-ai/langchain | 90k+ | Python |  | InjectedToolArg, Runnable  |
| langchain-ai/langgraph | 35.9k | Python |  | Pregel BSP, Channel+Reducer, Checkpoint, Command, Send |
| langgraph4j/langgraph4j | - | Java | Java  |  Channel+Reducer 8  |
| luochang212/dive-into-langgraph | 500+ | Python | LangGraph  | Middleware , , ,  |
| OpenBMB/ChatDev | 33.7k | Python |  Agent  | , , , Skill  |

---

* mora-lang v0.49+ *
