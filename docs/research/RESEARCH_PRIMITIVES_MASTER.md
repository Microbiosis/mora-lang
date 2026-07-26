# Mora-lang  — 

> ****: v0.34-v0.40  + 17   
> ****:  — mora-lang v0.41+   
> ****: 2026-07-04

---

## 0. 

### 

|  |  |  |
|---|---|---|
|  | 1 () | mora-lang v0.34  |
|  1  | 1 | loongclaw (loong) |
|  2  | 2 | mini-swe-agent, CLI-Anything |
|  3  | 7 | AIOS, mimiclaw, OpenFugu, OpenInfer, MinerU, Headroom, Puter |
|  4  | 4 | multi-agent-revenue-orchestrator, pi-agent/pi-mono, ai-coder-symphony, AgentMesh (MinimalFuture) |
|  5  | 3 | vesh-agents, AgentMesh Go (hupe1980), AgentMesh (Solace) |
| **** | **17** | () |

###  (v0.34-v0.40)

|  | P0 | P1 | P2 | Permanent | CI |
|---|---|---|---|---|---|
| v0.34 |  | — | — | — | — |
| v0.35 | 20 | 0 | 0 | 0 | 0 |
| v0.36 | 0 | 12 | 2 | 2 (Channel, Type 8 variants) | 1 |
| v0.37 | 0 | 7 | 2 | 0 | 0 |
| v0.38 | 0 | 0 | 0 | 1 (numeric tower) | 0 |
| v0.39 | 0 | 0 | 0 | 0 (rename only) | 0 |
| v0.40 | 0 | 0 | 0 | 1 (env immutable snapshot) | 0 |
| **** | **20** | **19** | **4** | **4/5** ( env ) | **1** |

### 

mora-lang v0.32-0.34  7 :
-  ** API ** (, , )
-  **** (/ML/)
-  **** (, , )

---

## 1. 

### 1.1 loongclaw (loong) — 

****: https://github.com/eastreams/loong (644 )  
****: Rust  AI . 13-crate  DAG, L0-L9 .

#### 

|  |  | mora-lang  |
|---|---|---|
| **Capability ** (13 variants) | `crates/contracts/src/contracts.rs:24-37` | `sandbox.key { file.read, web.fetch }` |
| **CapabilityToken** (token_id + allowed + expires_at + generation) | `crates/contracts/src/contracts.rs:44-52` | `Value::CapKey` |
| **PolicyEngine trait** (issue/authorize/revoke) | `crates/kernel/src/kernel.rs:42-58` | `sandbox.check_call(req)` |
| **PolicyExtensionChain** (Chain of Responsibility, ) | `crates/kernel/src/policy_ext.rs:34-45` | Policy plugin system |
| **AuditSink** trait + SHA-256  JSONL | `crates/kernel/src/audit.rs:34-204` | `audit.jsonl` file |
| **Fault** enum (Panic/CapViolation/TokenExpired/...) | `crates/contracts/src/task_state.rs:34-48` | `Fault`  `String`  |
| **TaskState** FSM (5 states, typed transitions) | `crates/contracts/src/task_state.rs:52-74` | `FlowSignal`  |
| **Core/Extension Adapter** ( BTreeMap dispatch) | `crates/kernel/src/tool.rs:25-67` | `ToolPlane`  `tool_registry` |
| **Provider→Channel→Connector** hierarchy | `crates/kernel/src/integration.rs` | I/O abstraction |
| **WorkUnitRecord** (12-state lifecycle + retry + blocking) | `crates/contracts/src/workflow_types.rs` | Task queue |
| **Plugin pipeline** (scan→translate→plan→bootstrap) | `crates/kernel/src/plugin.rs` | Package lifecycle |

####  vs 

|  (v0.41 ) |  |
|---|---|
| Capability token  | WASM  (wasmtime) |
| AuditSink + hash chain | ed25519  |
| Fault  | 13-crate DAG (mora  crate) |
|  | / |

---

### 1.2 mini-swe-agent — 

****: https://github.com/SWE-agent/mini-swe-agent (5.6k )  
****: 100  Python ,  bash, . SWE-bench verified >74%.

#### 

|  |  | mora-lang  |
|---|---|---|
| **Exception-as-flow**: `InterruptAgentFlow`  → 5  | `src/minisweagent/exceptions.py` | `FlowSignal`  +  |
| ****:  `list[dict]`, `role: exit`  | `src/minisweagent/agents/default.py:97-119` | `TraceCollector` v2 |
| ****: `Popen(shell=True, start_new_session=True)` + `os.killpg` | `src/minisweagent/environments/local.py:62-73` | `exec(cmd, timeout)` builtin |
| **COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT** sentinel | `src/minisweagent/environments/local.py:40-51` | Task completion protocol |
| **tenacity **: 10 attempts, exp backoff 4s→60s, abort_exceptions | `src/minisweagent/models/utils/retry.py` | `ai.retry { attempts: 10 }` block |
| **BASH_TOOL**: , `{"name":"bash", "command": string}` | `src/minisweagent/models/utils/actions_toolcall.py` | `exec.bash(cmd)` |

####  vs 

|  (v0.41 ) |  |
|---|---|
| Exception-as-flow  | Jinja2  |
|  +  | litellm  |
| COMPLETE_TASK sentinel |  (/) |
|  |  (trajectory browser) |

---

### 1.3 CLI-Anything — 

****: https://github.com/HKUDS/CLI-Anything (44.7k )  
****:  CLI — 7 , 100+ , 9+ .

#### 

|  |  | mora-lang  |
|---|---|---|
| ****: `matrix_registry.json` (→→) + `registry.json` () + `public_registry.json` () |  | `mora-hub.json` + `mora-public.json` |
| **HARNESS.md**: 7 ,  | `cli-anything-plugin/HARNESS.md` | `skill.md`  |
| **SKILL.md **: YAML  + "For AI Agents"  | `skills/cli-anything-gimp/SKILL.md` | `mora-skill-{name}/SKILL.md` |
| ****: `cli-anything-{name}`  5+  |  | `mora-skill-{name}` |
| **/**:  →  →  →  | `/cli-anything:refine` | `mora refine script.mora "add X"` |
| ****: 9  `kind`  (harness-cli, public-cli, python, native, api, ...) | `matrix_registry.json` | `ToolKind` enum |
| ** + **: bundle () + session () + trajectory () | `preview_bundle.py` | `recorder` v2 |

####  vs 

|  (v0.41 ) |  |
|---|---|
|  |  (bundle+trajectory) |
| SKILL.md YAML  | PEP 420  (Python-specific) |
|  | CLI-Hub  |

---

### 1.4 AIOS — 

****: https://github.com/agiresearch/AIOS  
****: Python LLM  — FIFO/RR , 4 , .

#### 

|  |  |
|---|---|
| " FIFO/RR" |   — `FIFOScheduler` (batch_interval=1s) + `RRScheduler` (time_slice=1s), 4  |
| "Tool Manager hashmap " |   — `tool_conflict_map` + `threading.Lock`, **** |
| "Context snapshot (text/logits)" |   —  LLM  (past_key_values),  |
| Agent lifecycle |   — ,  `self.active = False` |

#### mora-lang 

|  |  | LOC |
|---|---|---|
| `tool_conflict_map` per-tool  | P1 | ~40 |
| `ContextSnapshot`  (past_key_values  HF ) | P2 | ~60 |

---

### 1.5 mimiclaw —  ReAct + Cron

****: https://github.com/memovai/mimiclaw  
****: ESP32-S3 FreeRTOS C  — 12  cron, , /skill , GPIO .

#### 

|  |  |
|---|---|
| "cron (9  job)" |  **12 ** — `id, name, enabled, kind, interval_s, at_epoch, message, channel, chat_id, last_run, next_run, delete_after_run` |
| "heartbeat" |  FreeRTOS auto-reload timer, 30min interval, reads HEARTBEAT.md |
| "tool/skill " |   =  C ;  = SPIFFS markdown  |
| "path `..` " |  `strstr(path, "..")` +  |

#### mora-lang 

|  |  | LOC |
|---|---|---|
| Job  `channel`/`chat_id`/`delete_after_run`  | P1 | ~20 |
| `heartbeat.md`  | P2 | ~50 |
|  vs  | P2 | ~100 |

---

### 1.6 OpenFugu — 

****: https://github.com/trotsky1997/OpenFugu  
****: 19.5K  TRINITY  —  Qwen3-0.6B .

#### 

|  |  |  |
|---|---|---|
| **TRINITY **: `VEC_LEN = 19456` (9216 SVF offsets + 10240 router head) | `openfugu/mini.py` | 19.5K  |
| **per-turn **: Worker(0)/Thinker(1)/Verifier(2), 5 turns max | `openfugu/mini.py` | Thinker  |
| **DAG-as-data**: `model_id[]`, `subtasks[]`, `access_list[]`  | `openfugu/ultra.py` |  |
| **sep-CMA-ES**: diagonal CMA, λ=33, μ=16, 60 iterations | `train/train_trinity.py` |  σ  |
| **MockWorld**: per-domain  | `train/train_adaptive_pool.py` |  |

#### mora-lang 

|  |  | LOC |
|---|---|---|
| DAG-as-data  → `orchestrate`  | P1 | ~80 |
| per-turn  → `ai.role { worker / thinker / verifier }` | P2 | ~60 |
| MockWorld  → `mock`  per-domain  | P2 | ~40 |

---

### 1.7 OpenInfer — 

****: https://github.com/openinfer-project/openinfer  
****: Rust/CUDA  —  vLLM  + .

#### 

|  |  |
|---|---|
| ****: vLLM HTTP frontend + native engine via Unix-domain socket ZMQ bridge | `openinfer-vllm-frontend/src/bridge.rs` |
| ** KV **: GPU `KvBuffer` + host DRAM via pegaflow | `openinfer-kv-offload/src/engine.rs` |
| ****: `#[cfg(feature = "qwen3")]` ModelType  | `openinfer-server/src/server_engine.rs` |
| **CUDA **:  decode  | `openinfer-qwen3/src/scheduler.rs` |
| **P2P RDMA **: MetaServer gRPC + one-sided RDMA READ | `openinfer-kv-offload/src/engine.rs` |

#### mora-lang 

|  |  | LOC |
|---|---|---|
|  → `ai_infra.rs`  | P2 | ~30 |

---

### 1.8 MinerU — 

****: https://github.com/opendatalab/MinerU  
****:  — 3 , 30+ BlockType, .

#### mora-lang  (v0.33 `reading_order` 113 )

|  | MinerU  |  |
|---|---|---|
| `GapTree`: `center_y → center_x`  | ** XY-cut**:  |  gap-tree —  TopToBottom |
| `GroupBased`: `center_x → y`  | `find_best_visual_parent()`  |  |
| `XyCut`:  | `recursive_xy_cut()` - |  |
|  ML | LayoutLM-based layoutreader (≤200 ) |  |

#### v0.41 

|  |  | LOC |
|---|---|---|
|  XY-cut  | **P0** | ~50 |
| `find_best_visual_parent()`  | P2 | ~40 |

---

### 1.9 Headroom — 

****: https://github.com/headroomlabs-ai/headroom  
****: ContentRouter + SmartCrusher + CCR — Rust native detection, 5-dim scoring.

#### mora-lang  (v0.33 `ccr` 165 )

|  | Headroom  |  |
|---|---|---|
|  u64  → 16-char hex | **SHA-256**  → 24-char hex |  |
| `InMemoryCcrStore` (HashMap) | **SQLite + WAL + TTL** (1800s expiry) |  |
| `extract_hash`:  | ContentRouter: 11  |  |
|  | ****: skip set + result cache |  |
|  | LLM tool set auto-inject `headroom_retrieve` |  |

#### v0.41 

|  |  | LOC |
|---|---|---|
| SHA-256  | **P1** | ~30 |
| SQLite-backed CcrStore () | P2 | ~80 |

---

### 1.10 Puter — Web OS + 

****: https://github.com/HeyPuter/puter  
****: TypeScript web OS — EventClient wildcard, 5  DI , Service Extension.

#### mora-lang  (v0.32 `event` 110  + v0.33 `sandbox` 209 )

|  | Puter  |  |
|---|---|---|
| `emit`:  O(patterns) | **O(segments)**: ,  `*`  map  |  |
| fire-and-forget only | `emitAndWait`  |  |
| `allow: Vec<String>` + `deny: Vec<String>` | iframe  | + |
|  `thread_local!` |  |  |

#### v0.41 

|  |  | LOC |
|---|---|---|
| O(segments)  | **P0** | ~30 |
| `thread_local!`  | P2 | ~40 |

---

### 1.11 pi-agent / pi-mono — 

****: https://github.com/badlogic/pi-mono (TypeScript original) + https://github.com/Ashutosh0428/pi-agent (Python fork)  
****:  monorepo — , , , , .

|  |  | mora-lang  |
|---|---|---|
| ****: steering () + follow-up () | `packages/agent/src/agent.ts` | `bus.steer(task_id, msg)` / `bus.followup(task_id, msg)` |
| ****: `toolExecution: "parallel"` via `Promise.all` | `packages/agent/src/agent-loop.ts` | `exec.parallel([cmd1, cmd2])` |
| ****: `registry.without("delegate")` — =1 | `src/pi_agent/agent.py` | `agent.task { ... }`  |
| ****: `--reflect` —  | `src/pi_agent/agent.py:_reflection_pass` | `ai.reflect { max_turns: 5 }` |
| ****:  `.pi/memory.md` | `src/pi_agent/tools/memory.py` | `memory.remember(fact)` / `memory.recall()` |
| ****:  | `src/pi_agent/agent.py:_history_for_request` | `context.trim(threshold)` |
| ****:  | `src/pi_agent/agent.py:_dispatch` | `sandbox.guard { exfil: true }` |
| ****:  `to_schema()` →  | `src/pi_agent/tools/base.py` | `Tool` trait with `to_schema()` |
| ****: `update_plan`  →⏳→  | `src/pi_agent/tools/planning.py` | `plan.update([{step, status}])` |

---

### 1.12 AgentMesh — 

****: https://github.com/zhayujie/AgentMesh  
****: Python  — LLM ,  mesh .

|  |  | mora-lang  |
|---|---|---|
| ** WebSocket **: 7  | `agentmesh/common/models.py:55-115` | `bus.emit("agent_decision", typed_payload)` |
| ** pub-sub**: `subscribe_to_task` / `broadcast_to_task` | `agentmesh/api/websocket_manager.py` | `bus.subscribe(topic)` / `bus.publish(topic, msg)` |
| ****: `TeamContext.agent_outputs`  | `agentmesh/protocol/context.py` | `context.outputs` |
| ****:  + ,  | `agentmesh/memory/manager.py` | `memory.search(query, mode: "hybrid")` |
| ****: `PRE_PROCESS` vs `POST_PROCESS` | `agentmesh/tools/tool_manager.py` | `tool.register(name, fn, stage: "pre" | "post")` |
| ****:  + `__all__` exports | `agentmesh/tools/tool_manager.py` | Plugin registry pattern |
| ****:  +  | `agentmesh/protocol/agent.py:140-195` | `context.trim(threshold)` |

---

### 1.13 multi-agent-revenue-orchestrator — 

****: https://github.com/aadiieee/multi-agent-revenue-orchestrator ( )  
****: README-only, .

|  |  | mora-lang  |
|---|---|---|
| ****: Redis pub/sub  | `orchestrate` context |
| ****: `handoff_criteria: "meeting_booked OR high_intent_forecast"` | `orchestrate { on: expression }` |
| **YAML **: per-profile agent pipeline | `agent { profile: "emea_midmarket" }` |
| ** + **: `escalation_threshold_days: 14` | `stage { timeout: 14d, escalate: ... }` |
| ****: Omni Agent  | `agent X { role: validator }` |
| ****: `--agents research,personalization` CLI flag | `orchestrate { agents: [A, B] }` |

---

### 1.14 ai-coder-symphony — 

****: https://github.com/novanandin9-netizen/ai-coder-symphony ( )  
****: README-only, XOR . .

: **** (: math_whisperer, code_forger, ui_sculptor, documentation agent)  **** (`consensus_method: "weighted_voting"`).

---

### 1.15 vesh-agents — 

****: PyPI `vesh-agents` v0.1.1 (GitHub  404, PyPI )  
****:  OpenAI Agents SDK  SaaS . 6 .

#### 

**** ( 5 ):
```
DataConnector → EntityResolver → MetricComputer → AnomalyDetector → InsightReasoner
    ↑                ↑               ↑               ↑              ↑
 CSV/Stripe/     Blocking/       MRR/Churn/      Z-score/        BYOM LLM
 Postgres         Scoring         ARPU/NRR        Rate-of-change  Explanation
```

**6 **:
|  |  | MCP  |
|---|---|---|
| DataConnector |  | `import_csv`, `extract_stripe`, `extract_postgres` |
| EntityResolver |  | `resolve_entities` (+) |
| MetricComputer |  SaaS  (MRR, churn, ARPU, NRR, Quick Ratio) | `compute_metrics`, `list_metrics` |
| AnomalyDetector |  (Z-score, ) | `detect_anomalies` |
| InsightReasoner |  (BYOM LLM) | `explain_anomaly` |
| Vesh Orchestrator |  | `analyze_csv` () |

****:
- ****: ,  LLM ——
- ** LLM **: `vesh analyze csv file.csv`  LLM ()
- **BYOM**:  `litellm/anthropic/claude-sonnet-4`, `openai/gpt-4o`, 
- **CLI + MCP **:  Cursor/OpenCode/Claude Desktop
- ****:  Stripe/Postgres/CSV 

**mora-lang **:
|  | mora-lang  |
|---|---|
|  LLM  | `orchestrate { pipeline: [A, B, C] }` with `llm: none` |
|  | `data.anomaly(method: "zscore" | "rate_of_change")` |
|  | `data.resolve(sources: [csv, stripe, postgres])` |
| MCP  | `mcp.serve(tools: [DataConnector, MetricComputer, ...])` |

---

### 1.16 AgentMesh Go (hupe1980) — Pregel BSP 

****: https://github.com/hupe1980/agentmesh (6 , Go)  
****: **** ,  Pregel  BSP () .

>  ****:  MinimalFuture/AgentMesh (Python, LLM ) ****. hupe1980/agentmesh  Go , ,  Pregel BSP .

#### 

**Pregel BSP **:
```
Superstep 0 →  →  → Superstep 1 → ...
```
- : , 
- : 
- :  → 

****:
```
 (ReActAgent / SupervisorAgent / RAGAgent)
    ↓
 (,  API)
    ↓
 (, Run() → events, )
    ↓
 (Structure / Executor / StateManager)
    ↓
 (PregelExecutor BSP , , )
       (SequentialExecutor , )
```

****:
|  |  |
|---|---|
| **** | ,  +  +  |
| **** | CoW  map,  |
| **WASM ** |  WASM ,  |
| **OpenTelemetry** | ,  |
| **A2A ** |  |
| **MCP ** |  |
| ** + ** |  |
| **** |  |
| **Go 1.24+** | `iter.Seq2` : `for msg, err := range graph.Run(ctx, input) { ... }` |

**mora-lang **:
|  | mora-lang  |
|---|---|
| BSP  | `orchestrate { steps: [step1, step2] }` with barrier sync |
|  | `context.atom(key)` — lock-free  |
|  | `checkpoint.save()` / `checkpoint.restore()` |
| WASM  | Future `sandbox.wasm(code)` |
|  | `sandbox.approve { question: "..." }` —  |
| Go iter.Seq2  | Rust `Iterator<Item = Result<Message, Error>>` |

---

### 1.17 Solace Agent Mesh — 

****: https://github.com/SolaceLabs/solace-agent-mesh  
****: ,  AI .  Solace .

****:
- ****:  `topic/subtopic/action`  ()
- ****: ; 
- ****: 
- ****: 

**mora-lang **: `bus.subscribe("agent.research.*")` — ,  Solace .

---

## 2. 

### 2.1  (3+ )

|  |  | mora-lang  |
|---|---|---|
| ** + ** | loongclaw, AIOS | `sandbox.key { ... }` |
| ** + ** | loongclaw, CLI-Anything (bundle trajectory) | `audit.jsonl` |
| ** + ** | loongclaw, CLI-Anything, mimiclaw | `mora-hub.json` |
| ** / ToolKind ** | CLI-Anything (9 kinds), mimiclaw (tools vs skills), vesh-agents (pipeline agents) | `ToolKind` enum |
| ** + ** | mini-swe-agent, pi-agent | `exec(cmd, timeout)` |
| **** | mini-swe-agent | `FlowSignal`  |
| ** (markdown)** | pi-agent, AgentMesh, mimiclaw | `memory.remember()` |
| ** / ** | revenue-orchestrator, AgentMesh, vesh-agents (pipeline context) | `context.outputs` |
| **/** | CLI-Anything, pi-agent (--reflect) | `mora refine` |
| ** ( LLM )** | vesh-agents (), AgentMesh (LLM-based) | `orchestrate` |

### 2.2  (1 )

|  |  |  |
|---|---|---|
| **TRINITY  (19.5K params)** | OpenFugu |  |
| ** (steering + follow-up)** | pi-mono |  |
| **DAG-as-data** | OpenFugu |  |
| ** XY-cut** | MinerU |  |
| **SHA-256 ** | Headroom | ,  |
| **5  DI ** | Puter | config→clients→stores→services→controllers→drivers |
| **Pregel BSP ** | AgentMesh Go (hupe1980) | ,  |
| ** CoW ** | AgentMesh Go (hupe1980) | 10k+ GC  |
| **WASM ** | AgentMesh Go (hupe1980), loongclaw |  WASM  |
| ** LLM ** | vesh-agents |  LLM  |
| ** ()** | Solace Agent Mesh | `topic/subtopic/action` - |
| **** | Solace Agent Mesh |  |

---

## 3. mora-lang v0.41+ 

### 3.1 P0 —  ( 110 LOC)

|  |  | LOC |
|---|---|---|
| `event`: O(segments)  | Puter | ~30 |
| `reading_order`:  XY-cut  | MinerU | ~50 |
| `ccr`: SHA-256  | Headroom | ~30 |

### 3.2 P1 —  ( 440 LOC)

|  |  | LOC |
|---|---|---|
| `sandbox.key { ... }` — Capability token system | loongclaw | ~200 |
| `audit.jsonl` — AuditSink + SHA-256 hash chain | loongclaw | ~200 |
| `exec.parallel([cmd1, cmd2])` —  | pi-mono | ~50 |
| `memory.remember()/recall()` —  (markdown) | pi-agent | ~80 |
| `bus.subscribe(topic)/publish(topic)` — pub-sub | AgentMesh | ~60 |
| `orchestrate { on: expression }` —  | revenue-orchestrator | ~80 |
| `sandbox.guard { exfil: true }` —  | pi-agent | ~40 |
| Job  `channel`/`chat_id`/`delete_after_run` | mimiclaw | ~20 |
| `Fault`  String  (10+ call sites) | loongclaw | ~80 |

### 3.3 P2 —  ( 560 LOC)

|  |  | LOC |
|---|---|---|
| `skill.md`  +  (`mora-hub.json` + `mora-public.json`) | CLI-Anything | ~150 |
| `ToolPlane` (Core/Extension adapter)  `tool_registry` | loongclaw | ~150 |
| `ai.retry { attempts: 10, backoff: exponential }` | mini-swe-agent | ~50 |
| `ai.role { worker / thinker / verifier }` | OpenFugu | ~60 |
| `ai.reflect { max_turns: 5 }` —  | pi-agent | ~40 |
| DAG-as-data → `orchestrate`  | OpenFugu | ~80 |
| `tool.register(name, fn, stage: "pre" | "post")` | AgentMesh | ~30 |
| `context.outputs` () | AgentMesh | ~30 |
| `plan.update([{step, status}])` —  | pi-agent | ~40 |
| `heartbeat.md`  | mimiclaw | ~50 |
| `context.trim(threshold)` —  | pi-agent + AgentMesh | ~40 |
| `mora refine script.mora "add X"` —  | CLI-Anything | ~100 |

### 3.4  (v1.0+)

|  |  |  |
|---|---|---|
| WASM  (wasmtime) | loongclaw, OpenInfer |  |
| TRINITY  (19.5K ) | OpenFugu |  |
|  KV  | OpenInfer | GPU-specific |
| ML-based layoutreader (LayoutLM) | MinerU |  ML  |
| ContentRouter 11  | Headroom |  |
| 5  DI  | Puter |  |

---

## 4. v0.41  ( 5 commit)

| # | Commit | LOC |  |
|---|---|---|---|
| 1 | `fix(event): O(segments) indexed matching replaces linear scan (Puter)` | ~30 | +2 |
| 2 | `fix(reading_order): recursive XY-cut replaces flat sort (MinerU)` | ~50 | +3 |
| 3 | `feat(sandbox): CapKey + Capability enum — token-gated execution (loongclaw)` | ~200 | +5 |
| 4 | `feat(audit): AuditSink trait + SHA-256 chained JsonlAuditSink (loongclaw)` | ~200 | +4 |
| 5 | `feat(exec): exec.parallel() — concurrent subprocess execution (pi-mono)` | ~50 | +3 |
| **** | | **~530** | **+17** |

---

## 5. 

 mora-lang `RESEARCH_PRIMITIVES_MASTER.md` — ****.  v0.41+ .

> ****: , . ,  ` DONE in vX.YZ`.
