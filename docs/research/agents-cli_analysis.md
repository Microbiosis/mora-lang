# google/agents-cli

> AI Agent 
> 2026-07-01
> https://github.com/google/agents-cli
> v1.0.0 GA2026-07-01 

---

## 1. 

|  |  |
|------|------|
| **** | 4,201  / 454 Forks |
| **** | Python100% |
| **** | **" Google Cloud Agent "** ——  Agent ** Agent  CLI + Skill ** ADKAgent Development Kit Agent |
| **** | Apache-2.0 |
| **** | 2026-04-08 Google  |
| **** | v1.0.02026-07-01 GA |
| **** | Google ADKgoogle-adk ≥ 2.0Vertex AIClickRichuv |

> ****`agents-cli` ≠ Agent  **Agent  DevOps **—— `kubectl`  Kubernetes** Agent**Claude CodeCodexAntigravity CLI 

---

## 2. 

### 2.1 src/google/agents/cli/

```
main.py           #  Click LazyGroup 
_project.py       # agents-cli-manifest.yaml + pyproject.toml 
_tools.py         # uv, npx, gcloud, terraform, git 
_runner.py        # run, run_resolved, popen_resolved_detached
_click.py         # LazyGroup 
_skills_check.py   # Skill 

auth.py           # GCP / AI Studio ADC 
dev/              # install, lint, playground
run/              # / Agent A2A/ADK SSE 
eval/             # generate, grade, compare, analyze, optimize, dataset synthesize
deploy/           # Agent Runtime, Cloud Run, GKE
infra/            # CI/CD, Terraform, 
publish/          # Gemini Enterprise ADK / A2A 
scaffold/         # create, enhance, upgradecookiecutter
info/             # 
```

### 2.2 

|  |  |  |
|------|------|-------------|
| `ProjectConfig` | Agent A2A CI/CD  | `_project.py` |
| `LazyGroup` | Click CLI  | `_click.py` |
| `_DispatchTarget` |  vs  vs Agent Runtime  | `run/cmd_run.py` |
| `AgentEngineConfig` | Vertex AI Agent Runtime  | `deploy/agent_runtime.py` |
| `EvaluationDataset` | Vertex AI  | `eval/` |

### 2.3 

```

  → Phase 0: .agents-cli-spec.md spec
  → Phase 1: scaffold create ~72 Agent TerraformCI/CD
  → Phase 2: Build app/agent.py Agent/Tool/Workflow
  → Phase 3: Orchestrate Agent Sequential/Parallel/Loop/Graph
  → Phase 4: Evaleval generate → eval grade → iterate 5-10 
  → Phase 5: Deploydeploy → Agent Runtime / Cloud Run / GKE
  → Phase 6: Publishpublish gemini-enterprise
  → Phase 7: ObserveCloud Trace + BigQuery analytics
  →  →  Eval 
```

---

## 3. 

### 1Skill-as-Code AI 

`agents-cli`  **Skill **——/** Agent  Markdown **

****
```markdown
---
name: google-agents-cli-adk-code
metadata:
  author: Google
  version: 1.0.0
  requires:
    bins: [agents-cli]
---

# ADK Python Cheatsheet
... API 
```

****
- Skill  **YAML frontmatter + Markdown body** Agent 
-  `references/`  `adk-python.md`, `adk-workflows.md`
- **Always-active skill**`google-agents-cli-workflow` 8 
- Skill  `npx skills`  `agents-cli setup`  IDEClaude CodeCursorGemini CLI 

**Skill **
```python
#  tree skills/  Skill 
skills/
  google-agents-cli-workflow/SKILL.md          # 8 
  google-agents-cli-adk-code/SKILL.md          # ADK API 
  google-agents-cli-adk-code/references/adk-python.md
  google-agents-cli-adk-code/references/adk-workflows.md
  google-agents-cli-scaffold/SKILL.md          # 
  google-agents-cli-eval/SKILL.md            # 
  google-agents-cli-deploy/SKILL.md          # 
  google-agents-cli-publish/SKILL.md         # 
  google-agents-cli-observability/SKILL.md   # 
```

---

### 2Graph-based Workflow

ADK 2.0  `Workflow`  LLM ****——`START` 

**API **
```python
from google.adk.workflow import Workflow, node, JoinNode, RetryConfig
from pydantic import BaseModel

# AgentTool 
@node
def classify(node_input: str) -> str:
    return "urgent" if "urgent" in node_input else "normal"

# 
root_agent = Workflow(
    name="pipeline",
    edges=[
        ('START', classifier),
        (classifier, urgent_handler, "urgent"),   # 
        (classifier, normal_handler, "normal"),
        (classifier, fallback_handler, '__DEFAULT__'),  # 
    ],
    max_concurrency=4,
    timeout=300,
)
```

****

|  |  |  |
|------|------|------|
|  | `[(START, a), (a, b), (b, c)]` |  pipeline |
|  | `(node, target, "route_name")` |  |
| Fan-out | `(START, (branch_a, branch_b, branch_c))` |  |
| Fan-in | `JoinNode` + `((a, b), join), (join, final)` |  |
|  |  `Event(route="continue")` |  |
|  Worker | `@node(parallel_worker=True)` |  |

****
- 
-  Workflow  ** `output_schema`  LLM Agent **
-  `rerun_on_resume=False`LLM Agent  `rerun_on_resume=True`

---

### 3Eval-first Quality Gate

`agents-cli`  **** ""****

****
```python
# 1.  trace Agent 
agents-cli eval generate

# 2. LLM-as-judge + 
agents-cli eval grade --metrics final_response_quality,grounding

# 3. 
agents-cli eval compare prev.json latest.json

# 4. 
agents-cli eval analyze --eval-result latest.json

# 5. 
agents-cli eval optimize

# 6. 
agents-cli eval dataset synthesize --count 10
```

****
```python
# Vertex AI EvaluationDataset 
class EvalCase:
    agent_data: dict        #  turns, events, agents 
    responses: list         # ResponseCandidate 
    #  metric_results
```

****
- `eval generate`  ****  fresh agent asyncio.Lock 
- `eval grade`  **** CLI  ****Vertex AI CodeExecution sandbox
- **** artifact trace
- `pytest` `eval`  Agent `run`  smoke test

---

### 4State Prefix Namespace

ADK  `Session.state` ****

```python
# Session 
state["booking_step"] = 2

# User 
state["user:preferred_language"] = "en"

# App 
state["app:total_queries"] = 1000

# Temp 
state["temp:intermediate_result"] = data
```

****  Agent """"""

---

### 5Tool Confirmation Gates

ADK **** allow/deny

```python
from google.adk.tools import FunctionTool

# 1. /
sensitive_tool = FunctionTool(delete_record, require_confirmation=True)

# 2. 
def needs_approval(amount: float, **kwargs) -> bool:
    return amount > 1000
transfer_tool = FunctionTool(transfer_money, require_confirmation=needs_approval)

# 3. 
tool_context.request_confirmation(hint="Approve this transfer?")

# 4. 
from google.adk.tools import LongRunningFunctionTool
LongRunningFunctionTool(poll_external_job)
```

---

### 6Session Rewind & Resumability

```python
from google.adk.runners import InMemoryRunner

runner = InMemoryRunner(agent=root_agent, app_name="my_app")

# 
await runner.rewind_async(
    user_id=user_id,
    session_id=session.id,
    rewind_before_invocation_id=invocation_id,  # 
)
```

**** ""——/Agent 

---

### 7Ambient Agent/ Agent

 Agent "-"ADK ** Agent**

```python
from google.adk.cli.fast_api import get_fast_api_app

app = get_fast_api_app(
    agents_dir=AGENTS_DIR,
    web=False,
    trigger_sources=["pubsub", "eventarc"],  #  /apps/{app}/trigger/pubsub
)
```

****
-  Pub/Sub  Eventarc  HTTP 
-  **Cloud Scheduler cron **" 8 "
-  base64 CloudEvent 
- `ADK_TRIGGER_MAX_CONCURRENT=10`, `ADK_TRIGGER_MAX_RETRIES=3`
- JSON stdout → Cloud Loggingemail/Slack/Jira

---

### 8A2A Protocol & A2UIAgent  UI

**A2AAgent-to-Agent** Google  Agent  ADK

```python
#  A2A 
from google.adk.a2a.utils.agent_to_a2a import to_a2a
to_a2a(root_agent, port=8001)

#  A2A Agent
from google.adk.agents.remote_a2a_agent import RemoteA2aAgent
remote = RemoteA2aAgent(
    name="remote_agent",
    agent_card="http://remote-host:8001/.well-known/agent.json",
)
```

**A2UI** Agent  UI

---

### 9Context Caching & Compaction

```python
from google.adk.apps import App
from google.adk.apps.app import EventsCompactionConfig
from google.adk.apps.llm_event_summarizer import LlmEventSummarizer

app = App(
    name="my_app",
    root_agent=root_agent,
    # 
    context_cache_config=ContextCacheConfig(
        min_tokens=2048,     # 
        ttl_seconds=1800,    #  30 
        cache_intervals=10,  #  10 
    ),
    # 
    events_compaction_config=EventsCompactionConfig(
        compaction_interval=20,   #  20 
        overlap_size=3,          #  3 
        summarizer=LlmEventSummarizer(llm=Gemini(model="gemini-flash-latest")),
    ),
)
```

---

### 10Agent IdentityAgent  IAM 

 Agent  GCP 

```python
# deploy --agent-identity
client.agent_engines.create(
    config={
        "identity_type": IdentityType.AGENT_IDENTITY,
        "display_name": display_name,
    }
)
#  IAM aiplatform.user, logging.logWriter, monitoring.metricWriter 
```

**** Agent eval ""** IAM **——Agent  literally 

---

## 4.  Mora 

### 1 `eval`  —— 

****  `eval`  Agent 

**Mora **
```mora
// YAML/JSON 
eval dataset "incident-response" {
  case {
    input: "Database latency spike in us-east1"
    expect: {
      citation: contains("runbook-section-4.2"),
      destructive: false,
      root_cause_match: ~80%  // 
    }
  }
  case { ... }
}

//  trace + 
eval run "incident-response" on agent my_agent {
  metrics: [citation_check, safety_guard, quality_judge]
  threshold: 0.85
}

// 
deploy my_agent to cloud {
  gate: eval "incident-response" >= 0.85
}
```

---

### 2 `workflow`  —— 

**** Mora  `orchestrate` /LLM **** pipeline

**Mora **
```mora
workflow pipeline {
  node classify: llm {
    model: "gemini-flash"
    instruction: "Classify the input as urgent or normal"
    output_schema: { priority: "urgent" | "normal" }
  }
  
  node urgent_handler: agent escalation_agent
  node normal_handler: agent standard_agent
  
  edge START -> classify
  edge classify -> urgent_handler when classify.priority == "urgent"
  edge classify -> normal_handler when classify.priority == "normal"
  edge classify -> fallback_handler default
}

//  fan-out
workflow parallel_search {
  node search_a: tool web_search
  node search_b: tool internal_kb_search
  node merge: llm { instruction: "Synthesize results from {search_a} and {search_b}" }
  
  edge START -> (search_a, search_b)
  edge (search_a, search_b) -> merge via join
}
```

---

### 3 `state`  —— 

**** Mora  `value`  ADK  state prefix "session """""

**Mora **
```mora
//  = session 
let booking_step = 2

// 
let user:preferred_language = "zh-CN"

// 
let app:total_queries += 1

// 
let temp:scratch = compute_intermediate()

//  ai.chat 
with ai.chat {
  instruction: "User language: {user:preferred_language}"
}
```

---

### 4 `confirm` / `gate`  —— 

**** Mora  `sandbox`  `capability`**/**

**Mora **
```mora
// 
capability delete_database {
  require_confirmation: true
  // 
  require_confirmation: (amount > 1000) when args.amount > 1000
}

// 
gate approve_transfer {
  prompt: "Approve transfer of ${amount} to {recipient}?"
  timeout: 300s
  on_timeout: reject
}

// 
async tool poll_job_status(job_id: string) -> JobStatus {
  //  LongRunningFunctionTool 
}
```

---

### 5 `rewind`  —— 

**** Mora  `record/replay` `replay` ""`rewind` "" Agent 

**Mora **
```mora
//  ID
with ai.chat {
  invoke: query_weather("Tokyo")
  tag: #weather_call
}

// 
rewind before #weather_call

// 
rewind to 2026-07-01T10:00:00Z
```

---

### 6 `ambient` / `trigger`  ——  Agent

**** Mora ****

**Mora **
```mora
// 
ambient monitor_incidents {
  trigger: cron("0 */5 * * * *")   //  5 
  //  trigger: webhook("/hooks/incident")
  //  trigger: pubsub("projects/P/topics/incidents")
  
  agent: incident_agent
  
  max_concurrent: 4
  retry: 3 with backoff
  
  output: log structured  //  slack_notify, email_alert
}
```

---

### 7 `a2a`  ——  Agent 

**** Mora  `orchestrate`  Agent **/** Agent 

**Mora **
```mora
//  Agent 
a2a remote_security_agent {
  card_url: "https://security.internal/.well-known/agent.json"
  capabilities: [scan_vulnerability, generate_report]
}

//  Agent
with orchestrate {
  local: triage_agent
  remote: remote_security_agent.analyze(input: suspicious_payload)
  merge: synthesis_agent
}
```

---

### /

|  |  |  |
|------|------|------|
| `eval`  |  LLM-as-judge  |  /  Vertex |
| `workflow`  |  `orchestrate`  | `orchestrate` = LLM `workflow` =  |
| `state`  |  | "" |
| `ambient` |  cron/ |  Kubernetes CronJob |
| `a2a` | A2A v0.9.1 |  ABI |
| Agent Identity |  GCP IAM |  |
| Context Caching | Gemini  |  model  |

---

## 5.  17 

### 5.1 vs. LangChain / LangGraph

|  | LangChain/LangGraph | agents-cli / ADK |
|------|---------------------|------------------|
| **** | Agent Python/JS  | Agent  DevOps CLI + Skill |
| **** | LangGraph  checkpoint/ | ADK Workflow  +  + JoinNode |
| **** | LangSmith +  |  `eval`  + Vertex AI LLM-as-judge +  |
| **** |  | `deploy`  Agent Runtime / Cloud Run / GKE |
| **Skill ** |  | **Markdown Skill **—— Agent  |
| **** |  | **8 **Spec → Scaffold → Build → Orchestrate → Eval → Deploy → Publish → Observe |
| ** Agent ** |  | **A2A ** + Agent Card  |

### 5.2 vs. AutoGen / OpenAI Agents SDK

|  | AutoGen / OpenAI SDK | agents-cli / ADK |
|------|----------------------|------------------|
| **Agent ** |  |  +  +  |
| ** Agent ** | GroupChat / Handoff | SequentialAgent / ParallelAgent / LoopAgent / Workflow Graph |
| **** | `user_proxy`  | `request_input`  + `ResumabilityConfig` +  |
| **** |  | **Eval-first **generate → grade → compare → analyze → optimize |
| **** |  |  Cloud Run / GKE / Agent Runtime |
| **** |  | **Agent Identity IAM**BigQuery Cloud TraceIAPWIF |

### 5.3 vs. Kimi-CLI

|  | Kimi-CLI | agents-cli / ADK |
|------|----------|------------------|
| **** |  AI  CLIOrchestrator +  Agent | / Google Cloud Agent  |
| **Agent ** |  Agent |  + Ambient |
| ** Agent** | `Agent`  Agent | ADK `sub_agents` + `AgentTool` + A2A  Agent |
| **Skill ** | `SKILL.md`  | ** Markdown Skill ** IDE |
| **** | `plan` + `coder`  Agent  | `Workflow`  + `SequentialAgent` + `ParallelAgent` |
| **** |  | **Quality Flywheel**eval  |
| **** |  | Agent Runtime  |
| **** | `Context`  +  | `Session` + `State`  + `Memory Bank` + / |

### 5.4 agents-cli 

1. **" Agent"** Agent** Agent  Agent**—— Skill 
2. ****`eval` ** build/deploy  CLI ** fix→iterate 
3. **8 ** Spec  Observe " spec  scaffold"" eval  deploy"
4. ****Agent Identity IAMIAPWIF——Agent ""****
5. **** Vertex AICloud RunPub/SubEventarcCloud Scheduler """ Agent "
6. **A2A **Google  Agent 

---

## 6. 

`google/agents-cli` **** Agent ** Agent """"**

- **Spec **`.agents-cli-spec.md`  Single Source of Truth
- ****~72 
- ****eval 
- ****Workflow  LLM 
- **** 8 

 **Mora **
1. ** `eval` ** +  + 
2. ** `workflow` ** `orchestrate` /
3. **State **session / user / app / temp
4. ****`confirm` / `gate` 
5. ** rewind** replay
6. **Ambient Agent**

""** Mora """ Agent "**

---

> **** `google/agents-cli` GitHub Skill ADK  Release Notes 
