# Mora-lang  v2.1 —  +  +  + 

> ****: v2.1 ( [RESEARCH_PRIMITIVES_MASTER.md](./RESEARCH_PRIMITIVES_MASTER.md) v1 + v2 )  
> ****: v0.34-v0.40  + 17  MCP  + v0.41-v0.50  + **v0.41-v0.48 **  
> ****: 2026-07-05 (v2), 2026-07-06 (v2.1 implementation tracking)  
> ****: (A)  17  MCP ; (C) v0.41  commit Rust ; (D) v0.41-v0.50 ; **(E) v2.1: v0.41-v0.48 11 commits , 200 tests + 9803 LOC**

---

## 0. 

### 0.1 v1 → v2 (2026-07-05)

|  |  |  |
|---|---|---|
| §1.1-1.17 | **A: ** | 17   /  / commit  MCP  |
| §1.10 Puter | **A: ** |  `EventClient.ts`  O(segments)  |
| §1.8 MinerU | **A: ** |  XY-Cut++ (arXiv:2504.10258) |
| §1.11 pi-mono | **A: ** |  `earendil-works/pi`  |
| §1.12 AgentMesh | **A: ** |  3  fork: arshadvani3 (P2P), agentmesh-protocol (TCP/IP for agents), Nuraj250 () |
| §6 | **C: ** | v0.41  commit  Rust  +  +  |
| §7 | **D: ** | v0.41-v0.50  + 12  patch  |
| §8 | **** | Puter  / MinerU XY-Cut++ / AgentMesh  |

---

## 1.  (MCP )

> ****:  /  / commit  **2026-07-05 MCP **  

### 1.1 loongclaw (loong) —  

****: https://github.com/eastreams/loong ( `loongclaw-ai/loongclaw`)  
**MCP  (2026-07-05)**: **640 ** ,  2026-03-05, dev , Apache-2.0/MIT  
****:  **"Loong"** (: ) "Lightweight, clear, and fully extensible AI agent infrastructure" Rust 

#### MCP 

|  (v1) | MCP  |
|---|---|
| 644  | 640   |
| 13-crate  DAG |  SDK contract + discovery-first + product mode  |
| Capability  (13 variants) |  , `crates/contracts/src/contracts.rs` |
| PolicyEngine trait |   |
| AuditSink + SHA-256  JSONL |   |

####  (v1 )

- **SDK Contract **:  internal/external  quickstart
- **Capability Promotion Contract**: runtime evidence → durable capability assets 
- **42+  providers, 25+ channels** —  v1 
- **Discovery-first + Product mode**  ()

#### mora-lang 

master doc §1.1  Capability/PolicyEngine/AuditSink/Fault/TaskState **** v0.41 
-  "Capability Promotion"  → mora  `sandbox.key { promotion: "review" }` 
- 42 providers / 25 channels mora v0.41 ****

---

### 1.2 mini-swe-agent —  

****: https://github.com/SWE-agent/mini-swe-agent  
**MCP  (2026-07-05)**: **5120 ** (search snippet) / **5450 ** (deepwiki), v2.2.8 (2026-03-24), 971 commits, 504 forks  
****: v2 , v1 → v2 migration guide 

#### MCP 

|  (v1) | MCP  |
|---|---|
| 100  Python  |  **"some 100 lines of python for the agent class"** —  |
| `>74% SWE-bench verified` |   |
| `Popen(shell=True, start_new_session=True)` + `os.killpg` |  **v2  `subprocess.run`** () |
| `tenacity` 10 attempts  |   |
| `BASH_TOOL`  |   |

####  (v1 )

- **v2 **:  `start_new_session` + `os.killpg`, `subprocess.run` — ****
- **Gemini 3 Pro 74% on SWE-bench** — 
- **Deployable**:  docker/podman/singularity/bublewrap/contree — sandbox 
- ** alert**: litellm 1.82.7-1.82.8  (2026-03-24 PR #794 )

#### mora-lang 

master doc §1.2  `exec.bash` **** mora-lang  **v1  `start_new_session` ** v2  `subprocess.run`
- v0.41 `exec(cmd, timeout)` builtin 
- P1  (`ai.retry { attempts: 10, backoff: exponential }`) 

---

### 1.3 CLI-Anything —  

****: https://github.com/HKUDS/CLI-Anything  
**MCP  (2026-07-05)**: **44306 ** (), v0.4.0 (2026-06-25), 110 contributors  
****: SKILL.md  CDN,  Hermes orchestration skill, CLI-Hub 

#### MCP 

|  (v1) | MCP  |
|---|---|
| 44.7k  | 44306  |
| `matrix_registry.json`  |   `public_registry.json` + `registry.json` + `--capability`  |
| SKILL.md YAML frontmatter  |   SKILL.md  (Anthropic ) |
| HARNESS.md 7  |   `cli-hub-meta-skill/SKILL.md`  |
|  `cli-anything-{name}` |   |

####  (v1 )

- **Live Catalog**:  `reeceyang.sgp1.cdn.digitaloceanspaces.com/SKILL.md` (commit a0825ba, 2026-04-10)
- **`cli-hub can "task"` ** —  `mora can "..."`
- **Pre-flight before install**: `cli-hub matrix preflight --json`  exit 3 = gaps — ****mora 
- **Skill Path in CLI Banner**:  SKILL.md  agent 

#### mora-lang 

master doc §1.3  + SKILL.md **** v1  SKILL.md v0.41 ** Anthropic  SKILL.md **

---

### 1.4 AIOS —  

****: https://github.com/agiresearch/AIOS  
**MCP  (2026-07-05)**: ,  v5 (2025-08-12 arXiv:2403.16971v5)  
****:  RR (default) > FIFO  2.1×

#### MCP 

|  (v1) | MCP  |
|---|---|
| "FIFO/RR scheduler" |  , RR  |
| "4 " |   |
| `tool_conflict_map` + `threading.Lock` |   |
| Context snapshot (text/logits) |   LLM  (past_key_values) |
|  |   `self.active = False` |

####  (v1 )

- **Cross-session LLM-call batching** —  OS  (4 )
- **Pluggable BaseScheduler policy seam** —  v1 

#### mora-lang 

master doc §1.4  P1  `tool_conflict_map`  v0.41 ** per-tool Mutex**mora  OS

---

### 1.5 mimiclaw —  ReAct + Cron 

****: https://github.com/memovai/mimiclaw  
**MCP  (2026-07-05)**: **5K **, bb10ea01 commit (2026-04), C/FreeRTOS, ESP32-S3  
****:  Feishu botWebSocket gatewayHTTP proxy 

#### MCP 

|  (v1) | MCP  |
|---|---|
| "12  cron job" |   12  (id, name, enabled, kind, interval_s, at_epoch, message, channel, chat_id, last_run, next_run, delete_after_run) —  |
| Heartbeat FreeRTOS timer 30min |   |
| Tool vs Skill  |  Tool = C , Skill = SPIFFS markdown |
| Path `..`  |   |

####  (v1 )

- **Dual-Core **: Core 0 = Telegram Poller / Serial CLI / Outbound Dispatch; Core 1 = Agent Loop
- **Message Bus Pattern**: FreeRTOS `xQueue` inbound + outbound
- **Channel **: telegram / feishu / websocket / serial
- **FemtoClaw **: $4  —  (ESP32  S3)

#### mora-lang 

master doc §1.5  P1  "Job  channel/chat_id/delete_after_run"  v0.41  **channel  Vec<Channel>** ** mimiclaw **

---

### 1.6 OpenFugu —  

****: https://github.com/trotsky1997/OpenFugu  
**MCP  (2026-07-05)**: ****  
****: master doc §1.6  (openfugu/mini.py, openfugu/ultra.py) ****

#### 

-  : ", "
- v0.41  OpenFugu  3 DAG-as-data, per-turn role, MockWorld**** OpenFugu 

---

### 1.7 OpenInfer —  

****: https://github.com/openinfer-project/openinfer  
**MCP  (2026-07-05)**: **510 ** (search) / **423 ** (deepwiki, ), v0.1.0 (2026-06-13)  
****:  0.1.0 ,  release,  Kimi-K2 trillion-param

#### MCP 

|  (v1) | MCP  |
|---|---|
| "vLLM  + native engine" |   **"Pure Rust + CUDA, no PyTorch"**  |
|  KV  |   (GPU + host DRAM via pegaflow/pegainfer) |
|  (`#[cfg(feature = "qwen3")]`) |   feature matrix: qwen3 / qwen35-4b / deepseek-v4 / kimi-k2 |
| CUDA  |   |
| P2P RDMA  |   (MetaServer gRPC + RDMA) |

####  (v1 )

- ** (green-ctx)**: "Co-locating Prefill and Decode on One GPU" — 
- **Triton + TileLang  AOT**:  build-time, runtime pure Rust
- **NCCL ≥ 2.27 ** for MoE 
- **OpenInfer 0.1.0 **: 

#### mora-lang 

master doc §1.7  P2 " → ai_infra.rs " ****v0.50+  P2  P1

---

### 1.8 MinerU —  

****: https://github.com/opendatalab/MinerU  
**MCP  (2026-07-05)**: **68K **, cee1fe13 commit (2026-06-11), Python  
****: ** XY-Cut++** (arXiv:2504.10258, 2025-04)

#### MCP 

|  (v1) | MCP  |
|---|---|
| `GapTree: center_y → center_x` |  XY-cut  projection-based recursive |
| `GroupBased: center_x → y` |   geometric proximity |
| `XyCut: ` |  ** `recursive_xy_cut()`** + **XY-Cut++** |
|  ML |  LayoutLM-based layoutreader  |

####  (v1 )

- **XY-Cut++  (arXiv:2504.10258)**:
  - Pre-mask cross-layout elements
  - Multi-granularity segmentation
  - Cross-modal matching
  - L-shaped region handling
- **VLM backend v2.5** (2026): pipeline + VLM + hybrid  backend
- **`{original_filename}_layout.pdf` **:  reading order

#### mora-lang 

****: master doc §1.8 P0  " XY-cut " ** XY-Cut++ ** recursive_xy_cut

 P0 :
```
reading_order.rs:
  sort_entries(entries, beta=2.0, density_threshold=0.9) ->
    _identify_cross_layout_elements(entries) ->
    _recursive_segment(remaining, prefer_horizontal_first) ->
    _merge_cross_layout_elements(sorted, cross_layout)
```

---

### 1.9 Headroom —  

****: https://github.com/headroomlabs-ai/headroom  
**MCP  (2026-07-05)**: **56561 ** ( v1 ), v0.30.0 (2026-07-03), 160 releases  
****: , 8  (proxy, MCP, library)

#### MCP 

|  (v1) | MCP  |
|---|---|
| SHA-256  |   |
| SQLite + WAL + TTL |   |
| ContentRouter 11  |   (8  + ) |
|  (skip set + result cache) |   |
| LLM tool set auto-inject |   |

####  (v1 )

- ** 60-95% tokens** — 
- **MCP server**:  MCP
- **Cursor / Claude Code / LangChain / OpenAI**  — 
- **160 releases** in ~6  — 

#### mora-lang 

master doc §1.9  P1  "SHA-256 "  v0.41 ** SHA-256 **SQLite  P2

---

### 1.10 Puter — Web OS +  

****: https://github.com/HeyPuter/puter  
**MCP  (2026-07-05)**: **42359 ** (search) / **42K ** (deepwiki),  2026-07  
****: AGPL-3.0, MCP server , EventClient 

####   master doc §1.10 

MCP  `src/backend/clients/event/EventClient.ts:62-67`:

```ts
emit(key: T, data: EventMap[T], meta: unknown) {
    const parts = key.split('.');
    for (let i = 0; i < parts.length; i++) {
        const matchKey = (
            i === parts.length - 1
                ? key
                : `${parts.slice(0, i + 1).join('.')}.*`
        ) as ListenKey;
        // ...  this.#eventListeners[matchKey] ...
    }
}
```

** master doc  P0 **: `emit` , O(segments) , ** listener**

#### MCP 

|  (v1) | MCP  |
|---|---|
| "O(segments) " |  **** |
| fire-and-forget |   |
| `emitAndWait`  |   |
| `allow: Vec<String>` + `deny: Vec<String>` |   () |
| 5  DI  |  config → clients → stores → services → controllers |

####  (v1 )

- **2026-06 MCP server ** (PR #3197) — `puter.mcp serve` 
- **Pass args to all events** (PR #3248, 2026-06-10) — lifecycle event 
- **Worker types** (PR #3185) — serverless 
- **Claude Fable 5 ** (PR #3238)
- **PostgreSQL database backend** (PR #3167) —  SQLite

#### mora-lang 

master doc §1.10 P0  "O(segments) " **** 100% 

** §6.1**

---

### 1.11 pi-mono / pi-agent —  

****:  **** → https://github.com/earendil-works/pi ( `badlogic/pi-mono`)  
**MCP  (2026-07-05)**: **65520 ** (earendil-works/pi), v0.80.2 (2026-06-23), 220 contributors  
****:  `earendil-works` , npm  `@earendil-works/pi-*`

#### MCP 

|  (v1) | MCP  |
|---|---|
|  (steering + follow-up) |  **API **: `agent.steer()` + `agent.followUp()` + `agent.setSteeringMode("all" \| "one-at-a-time")` |
|  |  `toolExecution: "parallel"` |
|  (`registry.without("delegate")`) |   |
| `--reflect`  |   `transformContext`  |
|  markdown |  `~/.pi/memory.md` |
|  |  ** Gondolin / OpenShell / Docker**  container  |
|  |  `to_schema()`  |

####  (v1 )

- **2026-06-10  release**: pi v0.80  Message Queue 
- **dhruv2mars/pi-queue **: pi-package 
- **Gondolin **: host  pi+provider auth, micro-VM  tools
- **Supply-chain hardening**:  pinned exact versions + `.npmrc min-release-age=2`
- **240 releases** in ~10 

#### mora-lang 

master doc §1.11 9  ****:
- **** ****mora  `sandbox.guard`  `sandbox.containerize`
- :  `badlogic/pi-mono` → `earendil-works/pi`

---

### 1.12 AgentMesh —  

**MCP **: AgentMesh ****,  fork:

| Fork |  |  | MCP  |
|---|---|---|---|
| **MinimalFuture/AgentMesh** (master doc ) | github.com/MinimalFuture/AgentMesh | Python LLM-as-router |  |
| **hupe1980/agentmesh** (master doc §1.16 ) | github.com/hupe1980/agentmesh | Go Pregel BSP  |  |
| **arshadvani3/AgentMesh**   | github.com/arshadvani3/AgentMesh | P2P agent discovery + reputation | 1 , 2026-05 |
| **agentmesh-protocol/agentmesh-sdk**   | github.com/agentmesh-protocol/agentmesh-sdk | "TCP/IP for agents" — Ed25519 + RFC-001 | 0 , 2026-03 |
| **rscheiwe/mesh**   (PyPI agentmesh-py v0.1.11) | github.com/rscheiwe/mesh | LangGraph-style  + Vel SDK |  |
| **Nuraj250/AgentMesh**   | github.com/Nuraj250/AgentMesh |  agent graph builder (Cytoscape.js) | 2  |

#### mora-lang 

master doc §1.12 "LLM-as-router"** fork **:
- **arshadvani3**  + circuit breaker  master doc 
- **agentmesh-protocol**  Ed25519 +  RPC "agent "
- **rscheiwe/mesh**  + streaming events  LangGraph

**v0.42+ **  fork :
- `agent.trust(score, decay)` — 
- `agent.protocol(envelope)` — RFC-style message envelope
- `agent.graph(nodes, edges)` — 

---

### 1.13 multi-agent-revenue-orchestrator —  

****: https://github.com/aadiieee/multi-agent-revenue-orchestrator  
**MCP  (2026-07-05)**: **1 ** ( 1), 2026-05-24 , 2026-07-01  push  
****:  → **README + Mermaid  + 6 **

#### MCP 

|  (v1) | MCP  |
|---|---|
|  |  Mermaid  "Context Bus" |
|  (`handoff_criteria`) |  README  |
| YAML  |   |
|  +  |   |
|  (Omni Agent) |  Mermaid  |
|  (--agents) |   |

#### 

- **6 **: Revenue / Research / Meeting Prep / Deal / Personalization / Omni
- **Apollo.io / Notion / Gmail / Slack** 
- **Awesome Skills ** (2026-06-16) — Claude Code / Codex / Cursor skill

#### mora-lang 

master doc §1.13 **** P1  (orchestrate { on: expression }) 

---

### 1.14 ai-coder-symphony —  

**MCP **:   
**v2 **:  v1 "" P3

---

### 1.15 vesh-agents —  

****: https://github.com/shailesht003/vesh-agents  
**MCP  (2026-07-05)**: PyPI 0.1.1 , GitHub  404 ( shailesht003/vesh-agents )  
****:  "Laxmi Agents"

#### MCP 

|  (v1) | MCP  |
|---|---|
| 5  |   |
| 6  |   |
| BYOM (litellm/anthropic/openai) |   |
|  LLM  |   () |
| MCP  |   mcpmarket.com |

#### mora-lang 

master doc §1.15  4  ****v0.41+  vesh ** LLM **

---

### 1.16 AgentMesh Go (hupe1980) — Pregel BSP  

****: https://github.com/hupe1980/agentmesh  
**MCP  (2026-07-05)**: 6  (), Go 1.24+  
****: ,  Go agent framework 

#### MCP 

|  (v1) | MCP  |
|---|---|
| Pregel BSP  |   |
|  () |   |
|  CoW  |   |
| WASM  |   |
| OpenTelemetry  |   |
| A2A  + MCP  |   |
| Go iter.Seq2  |   |

#### mora-lang 

master doc §1.16  ****v0.42+  BSP  `orchestrate { barrier: true }` 

---

### 1.17 Solace Agent Mesh —  

****: https://github.com/SolaceLabs/solace-agent-mesh  
**MCP  (2026-07-05)**: , SolaceLabs   
****: ,  (solace.com/products/agent-mesh)

#### MCP 

|  (v1) | MCP  |
|---|---|
|  (`topic/subtopic/action`) |   |
|  |   |
|  |   |
|  |   |
| Solace  |   |

#### 

- **Core plugins**  (`solace-agent-mesh-core-plugins`) — 
- **WebUI Gateway example** — 
- **IT Ticket Workflow ** — Adaptiv 

#### mora-lang 

master doc §1.17  `bus.subscribe("agent.research.*")` ****v0.41 

---

## 2.  (v2 )

### 2.1  (3+ ) — v2 

|  |  | v2  |
|---|---|---|
| ** + ** | loongclaw, AIOS |  |
| ** + ** | loongclaw, CLI-Anything |  |
| ** + ** | loongclaw, CLI-Anything, mimiclaw |  |
| ** / ToolKind ** | CLI-Anything (9), mimiclaw (tools vs skills), vesh-agents |  |
| ** + ** | mini-swe-agent v1, pi-agent |  mini-swe-agent v2  |
| **** | mini-swe-agent |  |
| ** (markdown)** | pi-agent, AgentMesh, mimiclaw |  |
| ** / ** | revenue-orchestrator, AgentMesh, vesh-agents |  |
| **/** | CLI-Anything, pi-agent (--reflect) |  |
| ** ( LLM )** | vesh-agents, AgentMesh |  |
| ** ** | Puter, Solace |  |
| **  (steering + follow-up)** | pi-mono |  |
| **  (Cytoscape.js)** | Nuraj250/AgentMesh |   |
| ** Agent  (Ed25519 + RFC)** | agentmesh-protocol |   |

### 2.2  (1 ) — v2 

|  |  |  |
|---|---|---|
| **TRINITY  (19.5K params)** | OpenFugu |  **** |
| **** | pi-mono |  |
| **DAG-as-data** | OpenFugu |  |
| ** XY-cut** | MinerU |  ** XY-Cut++** |
| **SHA-256 ** | Headroom |  |
| **5  DI ** | Puter | config→clients→stores→services→controllers |
| **Pregel BSP ** | AgentMesh Go (hupe1980) |  |
| ** CoW ** | AgentMesh Go | 10k+ GC  |
| **WASM ** | AgentMesh Go, loongclaw |  WASM  |
| ** LLM ** | vesh-agents |  LLM  |
| ** ()** | Solace Agent Mesh | `topic/subtopic/action` |
| **** | Solace Agent Mesh |  |
| ** P2P agent discovery + reputation** | arshadvani3/AgentMesh |  + circuit breaker |
| ** Sandbox = container (Gondolin)** | pi-mono | host  agent, micro-VM  tools |
| ** Supply-chain hardening (pinned exact + min-release-age)** | pi-mono | npm  |

---

## 3. mora-lang v0.41+  (v2 )

### 3.1 P0 — 

|  |  | LOC | v2  | v2.1  |
|---|---|---|---|---|
| `event`: O(segments)  | **Puter ()** | ~30 | 🟢  |  **DONE v0.41.0** (commit 2a5afa1) |
| `reading_order`: **XY-Cut++**  () | **MinerU ()** | ~60 | 🟢  (LOC +10) |  **DONE v0.41.1** (commit bb4ebf8) |
| `ccr`: SHA-256  | Headroom | ~30 | 🟢 | 🟡 **DEFERRED v0.49+** (master doc §3.3 future exploration) |

### 3.2 P1 —  ( 440 LOC)

* v1 §3.2*

### 3.3 P2 —  ( 560 LOC) — v2 

|  |  | LOC | v2  | v2.1  |
|---|---|---|---|---|
| `ai_infra`  | OpenInfer () | ~30 | 🟢  | 🟡 **DEFERRED v0.49+** |
| `bus.subscribe("a.b.*")`  | Puter + Solace () | +0 ( event ) | 🟢 |  **DONE v0.43.1** (commit d8bd9c2) |
| `sandbox.containerize` Gondolin  | pi-mono | ~50 |  v2  |  **DONE v0.44.0 REAL Docker** (commit 9c4e49b,  metadata-only ) |
| `agent.trust(score, decay)` | arshadvani3/AgentMesh | ~40 |  v2  (P3 ) | 🟡 **DEFERRED v0.49+** |
| `agent.protocol(envelope)` RFC-style | agentmesh-protocol | ~60 |  v2  (P3 ) | 🟡 **DEFERRED v0.49+** |
| () ToolPlane Core/Extension | loongclaw | ~150 | — (master doc §3.3) |  **DONE v0.45.0** (commit 4a42e5c) |
| () ai.retry | mini-swe-agent | ~50 | — (master doc §3.3) |  **DONE v0.45.0** (commit 4a42e5c) |
| () ai.role | OpenFugu | ~60 | — (master doc §3.3) |  **DONE v0.45.0** (commit 4a42e5c) |
| () SKILL.md +  | CLI-Anything | ~150 | — (master doc §3.3) |  **DONE v0.46.0** (commit 2498194) |
| () DAG-as-data | OpenFugu | ~80 | — (master doc §3.3) |  **DONE v0.47.0** (commit 4bebaa5) |
| () heartbeat.md | mimiclaw | ~50 | — (master doc §3.3) |  **DONE v0.47.0** (commit 4bebaa5) |
| () ai.context.trim | pi-agent+AgentMesh | ~40 | — (master doc §3.3) |  **DONE v0.47.0** (commit 4bebaa5) |
| () plan.update | pi-agent | ~40 | — (master doc §3.3) |  **DONE v0.48.0** (commit edab45e) |
| () mora.refine | CLI-Anything | ~100 | — (master doc §3.3) |  **DONE v0.48.0** (commit edab45e) |

### 3.4  (v1.0+) — v2  (v2.1 )

|  |  | v2  | v2.1  |
|---|---|---|---|
| WASM  (wasmtime) | loongclaw, OpenInfer |  | ⏸ DEFERRED v1.0+ |
| TRINITY  | OpenFugu |  ****:  | ⏸ DEFERRED v1.0+ |
|  KV  | OpenInfer | GPU-specific | ⏸ DEFERRED v1.0+ |
| ML-based layoutreader | MinerU |  ML  | ⏸ DEFERRED v1.0+ |
| ContentRouter 11  | Headroom |  | ⏸ DEFERRED v1.0+ |
| 5  DI  | Puter |  | ⏸ DEFERRED v1.0+ |
|  Pregel BSP  | hupe1980/agentmesh |  `orchestrate { barrier: true }` | ⏸ DEFERRED v0.49+ |
|  P2P agent  | arshadvani3 |  | ⏸ DEFERRED v0.49+ |
|  Gondolin micro-VM  | pi-mono | v1.0+ | ⏸ DEFERRED v1.0+ |
|  OpenShell policy-controlled  | pi-mono | v1.0+ | ⏸ DEFERRED v1.0+ |

---

## 4. v0.41  (v2 )

| # | Commit | LOC |  | v2  | v2.1  |
|---|---|---|---|---|---|
| 1 | `fix(event): O(segments) indexed matching (Puter, code-verified)` | ~30 | +2 | 🟢  |  **DONE v0.41.0** |
| 2 | `fix(reading_order): XY-Cut++ (MinerU algorithm upgrade)` | ~60 | +4 | 🟢  (LOC +10) |  **DONE v0.41.1** |
| 3 | `feat(sandbox): CapKey + Capability enum (loongclaw)` | ~200 | +5 | 🟢 |  **DONE v0.42.0** |
| 4 | `feat(audit): AuditSink + SHA-256 JsonlAuditSink (loongclaw)` | ~200 | +4 | 🟢 |  **DONE v0.42.1** |
| 5 | `feat(exec): exec.parallel() (pi-mono v1 subprocess isolation)` | ~50 | +3 |  ** v1  v2** |  **DONE v0.43.0** ( std threads,  tokio per project rule) |
| **** | | **~540** | **+18** | | **ALL  DONE v0.41.0 - v0.43.0** |

---

## 5. v2 

1. **Puter EventClient  100% ** — `emit` ,  P0 
2. **mini-swe-agent v2 ** — `subprocess.run`  `start_new_session`, mora  v1 
3. **MinerU ** — `recursive_xy_cut` → `XY-Cut++`, v0.41 
4. **pi-mono ** — `badlogic/pi-mono` → `earendil-works/pi`, 
5. **pi-mono ** —  `sandbox.guard`  `sandbox.containerize` Gondolin 
6. **AgentMesh  fork ** — 5 ,  arshadvani3 P2P + agentmesh-protocol TCP/IP
7. **OpenFugu ** — ,  v0.41 
8. **Headroom   10 ** — 56K, , 

---

## 6. v0.41  commit Rust  (Phase C)

>  commit ** Rust ** +  + 

### 6.1 `fix(event): O(segments) indexed matching (Puter)

**** (`event.rs` ~110 ): `emit` , O(patterns)

**** (Rust ):
```rust
// src/event.rs

use std::collections::HashMap;

/// 
type Handler = Arc<dyn Fn(&Event) + Send + Sync>;

/// 
/// key  "user.created"  "user.*"
#[derive(Default)]
pub struct EventBus {
    ///  -> 
    literal: HashMap<String, Vec<Handler>>,
    ///  ->  (key  "user.*"  "user.created.*")
    wildcard: HashMap<String, Vec<Handler>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// 
    pub fn on(&mut self, key: &str, handler: Handler) {
        if key.ends_with(".*") {
            self.wildcard
                .entry(key.to_string())
                .or_default()
                .push(handler);
        } else {
            self.literal
                .entry(key.to_string())
                .or_default()
                .push(handler);
        }
    }

    ///  — **O(segments)**  Puter EventClient.ts:62-67
    pub fn emit(&self, key: &str, payload: Event) {
        let parts: Vec<&str> = key.split('.').collect();

        //  1: 
        //   e.g. emit("a.b.c")  "a.*"  "a.b.*" 
        for i in 0..parts.len() {
            let prefix_key = format!("{}.*", parts[..=i].join("."));
            if let Some(handlers) = self.wildcard.get(&prefix_key) {
                for h in handlers {
                    h(&payload);
                }
            }
        }

        //  2: 
        if let Some(handlers) = self.literal.get(key) {
            for h in handlers {
                h(&payload);
            }
        }
    }
}
```

****:
|  | emit  | on  |
|---|---|---|
|  (v0.32-0.40 ) | **O(patterns)** | O(1) |
|  (Puter O(segments)) | **O(segments)** | O(1) |

****:
|  |  |
|---|---|
| `emit("a.b.c")` + `on("a.*", h)` |  h  ( i=0 ) |
| `emit("a.b.c")` + `on("a.b.*", h)` |  h  ( i=1 ) |
| `emit("a.b.c")` + `on("a.b.c.*", h)` |  h **** (i=2  "a.b.c.*" key="a.b.c" ) |
| `emit("a.b.c")` + `on("a.b.c", h)` |  h  () |
| `emit("a")` + `on("a.*", h)` |  h **** (parts.len()=1,  i=0..1,  "a.*"  i<parts.len(),  i+1 == parts.len()  1 == 1,  break) — ** Puter ** |

****:
```rust
#[test]
fn emit_literal_match_fires_handler() { /* +1 */ }

#[test]
fn emit_wildcard_match_fires_handler() { /* +1 */ }

#[test]
fn emit_with_no_subscribers_is_noop() { /* +1 */ }

#[test]
fn emit_with_multiple_wildcards_fires_all() { /* +1 */ }

#[test]
fn emit_complexity_is_o_segments_not_o_patterns() {
    // : 1000 , emit ,  < 100us
    /* +1 (perf benchmark) */
}
```

**LOC**: ~30, ** +5**

---

### 6.2 `fix(reading_order): XY-Cut++ (MinerU )

**** (`reading_order.rs` ~113 ): GapTree / GroupBased / XyCut  flat sort

**** (Rust ,  XY-Cut++):
```rust
// src/reading_order.rs

use std::collections::HashMap;

#[derive(Debug, Clone, Copy)]
pub struct BBox {
    pub x0: f32, pub y0: f32, pub x1: f32, pub y1: f32,
}

impl BBox {
    pub fn width(&self) -> f32 { self.x1 - self.x0 }
    pub fn height(&self) -> f32 { self.y1 - self.y0 }
    pub fn center_x(&self) -> f32 { (self.x0 + self.x1) / 2.0 }
    pub fn center_y(&self) -> f32 { (self.y0 + self.y1) / 2.0 }
}

const DEFAULT_BETA: f32 = 2.0;            // cross-layout 
const DEFAULT_DENSITY_THRESHOLD: f32 = 0.9; // 
const MIN_GAP_THRESHOLD: f32 = 5.0;        // 

///  MinerU XY-Cut++ 
pub fn sort_entries(entries: Vec<HashMap<String, serde_json::Value>>) 
    -> Vec<HashMap<String, serde_json::Value>> 
{
    let mut sortable: Vec<SortableEntry> = entries
        .into_iter()
        .enumerate()
        .filter_map(|(i, e)| Some(SortableEntry { 
            original_index: i, 
            payload: e.clone(),
            bbox: extract_bbox(&e)?,
        }))
        .collect();

    //  1:  cross-layout  ( > beta * max_width)
    let (cross_layout, remaining): (Vec<_>, Vec<_>) = sortable
        .into_iter()
        .partition(|e| is_cross_layout(e, DEFAULT_BETA));

    //  2:  (XY or YX based on density)
    let prefer_horizontal_first = compute_prefer_horizontal(&remaining);
    let sorted_main = recursive_segment(&remaining, prefer_horizontal_first);

    //  3:  cross-layout 
    merge_cross_layout_elements(sorted_main, cross_layout)
        .into_iter()
        .map(|e| e.payload)
        .collect()
}

fn is_cross_layout(entry: &SortableEntry, beta: f32) -> bool {
    // width > beta * median_width AND overlaps multiple columns
    entry.bbox.width() > beta * entry.bbox.width().max(1.0)
        && overlaps_multiple_columns(entry)
}

fn compute_prefer_horizontal(entries: &[SortableEntry]) -> bool {
    //  x  vs y 
    let x_density = x_coverage(entries);
    let y_density = y_coverage(entries);
    x_density > DEFAULT_DENSITY_THRESHOLD * y_density
}

fn recursive_segment(
    entries: &[SortableEntry], 
    prefer_horizontal: bool
) -> Vec<SortableEntry> {
    if entries.is_empty() { return vec![]; }
    if entries.len() == 1 { return entries.to_vec(); }

    let (primary, secondary) = if prefer_horizontal {
        //  x ,  gap 
        let projection = project_to_x(entries);
        let cuts = split_projection(&projection, MIN_GAP_THRESHOLD);
        apply_cuts(entries, &cuts, Axis::X)
    } else {
        let projection = project_to_y(entries);
        let cuts = split_projection(&projection, MIN_GAP_THRESHOLD);
        apply_cuts(entries, &cuts, Axis::Y)
    };

    // 
    let mut result = vec![];
    for sub in secondary {
        result.extend(recursive_segment(&sub, !prefer_horizontal));
    }
    result.extend(primary); //  append
    result
}

fn merge_cross_layout_elements(
    mut main: Vec<SortableEntry>,
    cross_layout: Vec<SortableEntry>,
) -> Vec<SortableEntry> {
    //  cross-layout 
    for ce in cross_layout {
        let insert_pos = find_insertion_point(&main, ce.bbox);
        main.insert(insert_pos, ce);
    }
    main
}
```

****:
|  |  |  |
|---|---|---|
|  (GapTree / XyCut) | O(n²) flat |   |
|  (XY-Cut++) | O(n log n) recursive |  beta + overlap_count |

****:
- : `recursive_segment` 
- : density 
- : 

****:
```rust
#[test]
fn sort_single_column_doc() { /* +1 () */ }

#[test]
fn sort_two_column_doc() { /* +1 () */ }

#[test]
fn sort_with_cross_layout_header() { /* +1 () */ }

#[test]
fn sort_with_figure_inset() { /* +1 (L-shape ) */ }

#[test]  
fn sort_complexity_below_o_n_squared() { /* +1 (benchmark) */ }
```

**LOC**: ~60 ( v1  ~50), ** +5**

---

### 6.3 `feat(sandbox): CapKey + Capability enum (loongclaw)

**mora-lang **:  capability token,  `allow/deny` 

**** (Rust ,  loongclaw contracts.rs):
```rust
// src/sandbox/capability.rs

use std::collections::BTreeSet;
use std::time::{SystemTime, Duration};

/// Capability  —  loongclaw 13 variants  mora 
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    FileRead,
    FileWrite,
    WebFetch,
    WebSearch,
    ExecBash,
    ExecParallel,
    MemoryRead,
    MemoryWrite,
    AuditEmit,
    BusSubscribe,
    BusPublish,
    AgentInvoke,
    AgentRegister,
}

impl Capability {
    ///  (mora )
    pub fn parse(s: &str) -> Result<Self, SandboxError> {
        match s {
            "file.read" => Ok(Self::FileRead),
            "file.write" => Ok(Self::FileWrite),
            "web.fetch" => Ok(Self::WebFetch),
            "web.search" => Ok(Self::WebSearch),
            "exec.bash" => Ok(Self::ExecBash),
            "exec.parallel" => Ok(Self::ExecParallel),
            "memory.read" => Ok(Self::MemoryRead),
            "memory.write" => Ok(Self::MemoryWrite),
            "audit.emit" => Ok(Self::AuditEmit),
            "bus.subscribe" => Ok(Self::BusSubscribe),
            "bus.publish" => Ok(Self::BusPublish),
            "agent.invoke" => Ok(Self::AgentInvoke),
            "agent.register" => Ok(Self::AgentRegister),
            _ => Err(SandboxError::UnknownCapability(s.to_string())),
        }
    }
}

///  —  loongclaw CapabilityToken
#[derive(Debug, Clone)]
pub struct CapabilityToken {
    pub token_id: u64,                      // 
    pub allowed: BTreeSet<Capability>,      // 
    pub denied: BTreeSet<Capability>,       //  ()
    pub expires_at: Option<SystemTime>,     // None = 
    pub generation: u32,                    // 
    pub created_at: SystemTime,
}

impl CapabilityToken {
    pub fn is_alive(&self, now: SystemTime) -> bool {
        match self.expires_at {
            None => true,
            Some(exp) => now < exp,
        }
    }

    pub fn permits(&self, cap: Capability) -> bool {
        // deny  (sane default: explicit deny overrides allow)
        if self.denied.contains(&cap) { return false; }
        if !self.is_alive(SystemTime::now()) { return false; }
        self.allowed.contains(&cap)
    }
}

/// Policy Engine trait —  loongclaw PolicyEngine
pub trait PolicyEngine: Send + Sync {
    fn issue(
        &mut self, 
        requestor: &str, 
        requested: BTreeSet<Capability>, 
        ttl: Option<Duration>
    ) -> Result<CapabilityToken, SandboxError>;

    fn authorize(
        &self, 
        token_id: u64, 
        capability: Capability
    ) -> Result<(), SandboxError>;

    fn revoke(&mut self, token_id: u64) -> Result<(), SandboxError>;
}

/// Mora builtin: `sandbox.key { file.read, web.fetch }`
pub fn builtin_key(
    vm: &mut Vm,
    args: Vec<Value>,
) -> Result<Value, RuntimeError> {
    let mut allowed = BTreeSet::new();
    for arg in args {
        if let Value::Str(s) = arg {
            let cap = Capability::parse(&s)
                .map_err(|e| RuntimeError::from(e))?;
            allowed.insert(cap);
        } else {
            return Err(RuntimeError::TypeError(
                "sandbox.key expects string args".into()
            ));
        }
    }

    let token = vm.sandbox.issue("user_script", allowed, None)?;
    Ok(Value::CapKey(token.token_id))
}

/// Mora builtin: `sandbox.check_call(req)`
pub fn builtin_check_call(
    vm: &mut Vm,
    args: Vec<Value>,
) -> Result<Value, RuntimeError> {
    let (Value::CapKey(token_id), Value::Str(cap_str)) = (args[0].clone(), args[1].clone()) 
        else { return Err(RuntimeError::TypeError("...".into())); };

    let token = vm.sandbox.get_token(token_id)
        .ok_or(RuntimeError::from(SandboxError::TokenExpired))?;
    
    let cap = Capability::parse(&cap_str)?;
    
    if token.permits(cap) {
        Ok(Value::Bool(true))
    } else {
        Err(RuntimeError::from(SandboxError::CapViolation {
            token_id,
            capability: cap,
        }))
    }
}
```

**PolicyExtensionChain** (,  v0.42+):
```rust
/// Chain of Responsibility —  policy , 
pub trait PolicyExtension: Send + Sync {
    fn name(&self) -> &str;
    fn check(
        &self, 
        request: &PolicyRequest, 
        next_allowed: bool
    ) -> bool;  // 
}
```

****:
```rust
#[test]
fn token_with_single_capability_authorizes_correctly() { /* +1 */ }

#[test]
fn token_without_capability_denies() { /* +1 */ }

#[test]
fn expired_token_denies_even_if_capability_granted() { /* +1 */ }

#[test]
fn deny_overrides_allow() { /* +1 */ }

#[test]
fn revoke_invalidates_token_immediately() { /* +1 */ }

#[test]
fn unknown_capability_string_errors() { /* +1 */ }
```

**LOC**: ~200, ** +6**

---

### 6.4 `feat(audit): AuditSink + SHA-256 chained JsonlAuditSink (loongclaw)

**mora-lang **:  audit 

**** (Rust ,  loongclaw audit.rs):
```rust
// src/audit/sink.rs

use std::io::{Write, BufWriter};
use std::fs::{File, OpenOptions};
use sha2::{Sha256, Digest};
use std::time::SystemTime;

/// Audit event — 
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditEvent {
    pub timestamp: SystemTime,
    pub actor: String,           // user / agent / tool / sandbox
    pub action: String,          // "tool.invoke" / "sandbox.issue" / "file.write"
    pub target: Option<String>,  //  / 
    pub payload: serde_json::Value,
    pub token_id: Option<u64>,   //  CapabilityToken
    pub prev_hash: String,       // 
    pub hash: String,            //  SHA-256
}

impl AuditEvent {
    ///  self.hash = SHA-256(canonical_json(self) + prev_hash)
    pub fn seal(&mut self) {
        let canonical = serde_json::to_string(&CanonicalEvent {
            timestamp: self.timestamp,
            actor: &self.actor,
            action: &self.action,
            target: &self.target,
            payload: &self.payload,
            token_id: self.token_id,
            prev_hash: &self.prev_hash,
        }).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(canonical.as_bytes());
        self.hash = format!("{:x}", hasher.finalize());
    }
}

/// AuditSink trait —  sink 
pub trait AuditSink: Send + Sync {
    fn write(&mut self, event: AuditEvent) -> Result<(), AuditError>;
    fn flush(&mut self) -> Result<(), AuditError>;
    fn verify_chain(&self) -> Result<(), AuditError>;
}

/// JSONL + SHA-256  sink —  loongclaw AuditSink
pub struct JsonlAuditSink {
    writer: BufWriter<File>,
    last_hash: String,  //  hash
    events_count: u64,
}

impl JsonlAuditSink {
    pub fn new(path: &str) -> Result<Self, AuditError> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
            last_hash: "0".repeat(64),  // genesis
            events_count: 0,
        })
    }
}

impl AuditSink for JsonlAuditSink {
    fn write(&mut self, mut event: AuditEvent) -> Result<(), AuditError> {
        event.prev_hash = self.last_hash.clone();
        event.seal();
        
        // JSONL 
        let line = serde_json::to_string(&event)?;
        writeln!(self.writer, "{}", line)?;
        
        self.last_hash = event.hash.clone();
        self.events_count += 1;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), AuditError> {
        self.writer.flush()?;
        Ok(())
    }

    fn verify_chain(&self) -> Result<(), AuditError> {
        // , 
        use std::io::{BufRead, BufReader};
        let file = File::open("audit.jsonl")?;  // 
        let reader = BufReader::new(file);
        let mut prev = "0".repeat(64);
        for (i, line) in reader.lines().enumerate() {
            let line = line?;
            let event: AuditEvent = serde_json::from_str(&line)?;
            if event.prev_hash != prev {
                return Err(AuditError::ChainBroken {
                    line: i, 
                    expected: prev, 
                    actual: event.prev_hash,
                });
            }
            let mut recomputed = event.clone();
            recomputed.seal();
            if recomputed.hash != event.hash {
                return Err(AuditError::HashMismatch {
                    line: i,
                });
            }
            prev = event.hash;
        }
        Ok(())
    }
}

/// Mora builtin: `audit.emit(actor, action, target, payload)`
pub fn builtin_audit_emit(
    vm: &mut Vm,
    args: Vec<Value>,
) -> Result<Value, RuntimeError> {
    let (actor, action, target, payload) = extract_audit_args(args)?;
    let event = AuditEvent {
        timestamp: SystemTime::now(),
        actor,
        action,
        target,
        payload: serde_json::to_value(payload)?,
        token_id: vm.current_token_id,
        prev_hash: String::new(),  // sink 
        hash: String::new(),
    };
    vm.audit_sink.write(event)?;
    Ok(Value::Unit)
}
```

****:
```rust
#[test]
fn write_appends_jsonl_line() { /* +1 */ }

#[test]
fn each_event_has_chained_hash() { /* +1 */ }

#[test]
fn verify_chain_passes_for_valid_log() { /* +1 */ }

#[test]
fn verify_chain_fails_on_tampered_event() { /* +1 */ }

#[test]
fn empty_log_verifies_as_genesis() { /* +1 */ }
```

**LOC**: ~200, ** +5**

---

### 6.5 `feat(exec): exec.parallel() (pi-mono v1 )

****:  **mini-swe-agent v1  `start_new_session` ** v2  `subprocess.run`, 

**** (Rust ,  tokio runtime):
```rust
// src/exec/parallel.rs

use tokio::process::{Command, Child};
use std::process::Stdio;
use std::collections::HashMap;
use std::time::Duration;

/// 
#[derive(Debug)]
pub struct ParallelResult {
    pub cmd: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub elapsed_ms: u64,
    pub pid: u32,
}

#[derive(Debug, Clone)]
pub struct ParallelOptions {
    pub timeout: Duration,
    pub max_concurrent: usize,        // 
    pub working_dir: Option<String>,
    pub env: HashMap<String, String>,
    pub kill_on_drop: bool,           // 
}

/// Mora builtin: `exec.parallel([cmd1, cmd2, cmd3], timeout=30s)`
pub async fn builtin_parallel(
    cmds: Vec<String>,
    opts: ParallelOptions,
) -> Result<Vec<ParallelResult>, RuntimeError> {
    use tokio::sync::Semaphore;
    use std::sync::Arc;

    let semaphore = Arc::new(Semaphore::new(opts.max_concurrent));
    
    let mut handles = vec![];
    for cmd_str in cmds {
        let permit = semaphore.clone().acquire_owned().await
            .map_err(|e| RuntimeError::Concurrency(e.to_string()))?;
        let opts = opts.clone();
        
        let handle = tokio::spawn(async move {
            let _permit = permit;  // 
            run_isolated_cmd(&cmd_str, &opts).await
        });
        handles.push(handle);
    }
    
    // 
    let mut results = vec![];
    for h in handles {
        match h.await {
            Ok(r) => results.push(r),
            Err(e) => return Err(RuntimeError::Join(e.to_string())),
        }
    }
    Ok(results)
}

async fn run_isolated_cmd(
    cmd_str: &str, 
    opts: &ParallelOptions
) -> Result<ParallelResult, RuntimeError> {
    use tokio::time::timeout;
    
    let start = std::time::Instant::now();
    
    // : **** (mini-swe-agent v1 )
    //    Unix: pre_exec + setpgid
    //    Windows: CREATE_NEW_PROCESS_GROUP
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(cmd_str)
       .stdout(Stdio::piped())
       .stderr(Stdio::piped())
       .stdin(Stdio::null())
       .kill_on_drop(true);  // tokio 
    
    #[cfg(unix)]
    {
        // ,  os.killpg 
        unsafe {
            cmd.pre_exec(|| {
                libc::setpgid(0, 0);
                Ok(())
            });
        }
    }
    
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NEW_PROCESS_GROUP = 0x00000200
        cmd.creation_flags(0x00000200);
    }
    
    if let Some(wd) = &opts.working_dir {
        cmd.current_dir(wd);
    }
    cmd.envs(&opts.env);
    
    let child = cmd.spawn().map_err(|e| RuntimeError::Exec(e.to_string()))?;
    let pid = child.id().unwrap_or(0);
    
    // 
    let output = match timeout(opts.timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(RuntimeError::Exec(e.to_string())),
        Err(_) => {
            // : 
            kill_process_group(pid);
            return Err(RuntimeError::Timeout(opts.timeout));
        }
    };
    
    Ok(ParallelResult {
        cmd: cmd_str.to_string(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code(),
        elapsed_ms: start.elapsed().as_millis() as u64,
        pid,
    })
}

#[cfg(unix)]
fn kill_process_group(pid: u32) {
    use nix::sys::signal::{killpg, Signal};
    use nix::unistd::Pid;
    let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGKILL);
}

#[cfg(windows)]
fn kill_process_group(pid: u32) {
    // Windows:  taskkill /F /T /PID
    let _ = Command::new("taskkill")
        .args(&["/F", "/T", "/PID", &pid.to_string()])
        .output();
}
```

****:
|  |  |  |
|---|---|---|
|  (mora v0.40 `exec.bash`) |   |   |
| mini-swe-agent v1 |  setpgid |  |
| mini-swe-agent v2 |  subprocess.run |  |
| ** (mora v0.41)** | ** setpgid + CREATE_NEW_PROCESS_GROUP** |  |

****:
-  cmd : 
- max_concurrent=0: semaphore.acquire()  ()
-  cmd :  cmd
-  fork : 

****:
```rust
#[tokio::test]
async fn parallel_runs_all_commands() { /* +1 */ }

#[tokio::test]
async fn parallel_respects_max_concurrent() { /* +1 */ }

#[tokio::test]
async fn parallel_kills_process_group_on_timeout() { 
    //  sleep 60, timeout 1s,  pid  kill
    /* +1 */
}

#[tokio::test]
async fn parallel_collects_stdout_per_command() { /* +1 */ }

#[tokio::test]
async fn parallel_returns_error_for_missing_binary() { /* +1 */ }
```

**LOC**: ~50, ** +5**

---

## 7. v0.41-v0.50  +  (Phase D)

> **v2.1 **: v0.41.0 → v0.43.1 (first wave 5 commits) + v0.44.0 → v0.48.0 (extended 6 commits) **  DONE** (2026-07-06)

### 7.1 12  patch 

```
                    
                      P0: event O(segments)   ← v0.41.0
                      Puter (code-verified) 
                    
                              
                              
                    
                     P0: reading_order        ← v0.41.1
                     XY-Cut++ (MinerU)      
                    
                              
                              
        
          P1: sandbox.key + Capability          ← v0.42.0
          P1: Fault enum (replace String err) 
        
                              
                              
        
          P1: audit.jsonl + AuditSink           ← v0.42.1
          ( sandbox.key  token_id ) 
        
                              
                              
                    
                     P1: exec.parallel        ← v0.43.0
                     pi-mono v1 isolation   
                    
                              
                              
        
         P1: memory.remember/recall (markdown)  ← v0.43.1
         P1: bus.subscribe/publish            
        
                              
                              
        
         P1: orchestrate { on: expression }    ← v0.44.0
         P1: sandbox.guard → containerize     
        
                              
                              
        
         P2: ToolPlane (loongclaw Core/Ext)    ← v0.45.0
         P2: ai.retry + tenacity-like         
         P2: ai.role + per-turn role          
        
                              
                              
        
         P2: skill.md + mora-hub.json           ← v0.46.0
         P2: DAG-as-data → orchestrate ext    
         P2: heartbeat.md executable          
        
                              
                              
        
         P2: ai.reflect, plan.update            ← v0.47.0
         P2: tool.register stage pre/post     
         P2: context.trim + context.outputs   
        
                              
                              
        
         P2: mora refine (CLI-Anything loop)    ← v0.48.0
         P3: agent.trust (arshadvani3 fork)   
        
                              
                              
        
         Future: BSP scheduler (hupe1980)      ← v0.49+
         Future: WASM sandbox (loongclaw)    
         Future: TRINITY router (OpenFugu) 
        
```

### 7.2 

|  | Patch  |  LOC |  |  |
|---|---|---|---|---|
| **v0.41.0** | 1 (event O(segments)) | 30 | +5 | Puter  |
| **v0.41.1** | 1 (reading_order XY-Cut++) | 60 | +5 | MinerU  |
| **v0.42.0** | 2 (sandbox.key + Fault) | 280 | +11 | loongclaw  |
| **v0.42.1** | 1 (audit.jsonl) | 200 | +5 | sandbox.key (token_id) |
| **v0.43.0** | 1 (exec.parallel) | 50 | +5 | pi-mono v1  |
| **v0.43.1** | 2 (memory + bus) | 140 | +8 | pi-agent  |
| **v0.44.0** | 2 (orchestrate + sandbox.containerize) | 130 | +8 | AgentMesh + pi-mono |
| **v0.45.0** | 3 (ToolPlane + retry + role) | 260 | +12 | loongclaw + mini-swe + OpenFugu |
| **v0.46.0** | 3 (skill.md + DAG + heartbeat) | 280 | +10 | CLI-Anything + OpenFugu + mimiclaw |
| **v0.47.0** | 3 (reflect + stage + trim) | 110 | +8 | pi-agent + AgentMesh |
| **v0.48.0** | 2 (refine + agent.trust) | 140 | +6 | CLI-Anything + arshadvani3 |
| **v0.49.0+** | Future BSP + WASM | TBD | TBD | hupe1980 + loongclaw |
| **** | **21 patches** | **~1680** | **~83** | 17  |

### 7.3 

|  patch |  patch |  |
|---|---|---|
| v0.42.0 sandbox.key | v0.42.1 audit.jsonl | audit event  token_id  |
| v0.42.0 sandbox.key | v0.44.0 sandbox.containerize | containerize  guard  |
| v0.43.0 exec.parallel | v0.44.0 orchestrate | orchestrate  parallel  |
| v0.41.0 event O(segments) | v0.43.1 bus | bus  event  |
| v0.42.0 Fault enum |  |  Fault  String |

### 7.4 v0.41  vs 

| | v0.41  ( v0.41.x) | v0.41-v0.50 () |
|---|---|---|
| Patch  | 5 | 21 |
|  LOC | 540 | 1680 |
|  | +18 | +83 |
|  | 1 minor | 9 minors |
|  |  |  (BSP/WASM ) |
|  | 1-2  | 9-12  |

---

## 8. v2  (v1 )

### 8.1 AgentMesh  (2026 )

```
        
          arshadvani3/AgentMesh (P2P)      ← 2026-05
          dynamic trust + circuit break  
        
                         ↓
        
          agentmesh-protocol SDK           ← 2026-03
          "TCP/IP for agents"            
          Ed25519 + RFC-001 envelope     
        
                         ↓
        
          hupe1980/agentmesh (BSP)         ← 
          Pregel superstep + CoW checkpt 
        
                         ↓
        
          rscheiwe/mesh (graph exec)       ← 2026
          LangGraph-style + Vel SDK      
        
                         ↓
        
          Nuraj250/AgentMesh (visual)      ← 2025
          Cytoscape.js + Socket.IO       
        
```

**mora-lang **:
- **v0.48+**: `agent.trust(score, decay)`  arshadvani3
- **v0.49+**: `agent.protocol(envelope)`  agentmesh-protocol
- **v0.50+**: `orchestrate { barrier: true }`  hupe1980 BSP

### 8.2 pi-mono  (Gondolin)

master doc §1.11  `sandbox.guard` :

```
       
          mora host process        ←  script + LLM API
          (sandbox.containerize) 
       
                  
       
                            
                            
              
    Gondolin            Docker 
    microVM             wrapper
    (Linux)             (any)  
              
                            
       
                  
       
         tool sandboxed            ←  tools
         bash, file, web, etc.   
       
```

**v0.44.0 **:
```rust
// Mora builtin
sandbox.containerize { 
    backend: "gondolin" | "docker" | "openshell",
    mounts: ["/data:ro", "/workspace:rw"],
    network: "isolated" | "host",
    cpu_limit: "2 cores",
    memory_limit: "4GB"
}
```

### 8.3 Puter EventClient 

**** (`src/backend/clients/event/EventClient.ts:62-67`):
```typescript
emit(key: T, data: EventMap[T], meta: unknown) {
    const parts = key.split('.');
    for (let i = 0; i < parts.length; i++) {
        const matchKey = (
            i === parts.length - 1
                ? key
                : `${parts.slice(0, i + 1).join('.')}.*`
        ) as ListenKey;
        // ...  map[matchKey]  ...
    }
}
```

****:
1. ** Map** (literal key + ".*" )
2. **emit **,  listener
3. ****: `(key, data, meta)` 
4. **Extension **: 

**v0.41 ** ( §6.1 ):
-  `HashMap<String, Vec<Handler>>` 
- : `(&Event, &EventMeta)`
- Extension  v0.42+ 

---

## 9. 

### 9.1 v2 

- ****:  v2  §6 
- ****: §7  v0.41-v0.50 
- ****:  §8 ()  §1.x 

### 9.2 

1. **OpenFugu **:  arXiv 
2. **mini-swe-agent v2 **: v0.41  v1 , 
3. **pi-mono **:  `badlogic/pi-mono`  `earendil-works/pi`
4. ** fork **: 5  AgentMesh , v0.48+ 

### 9.3 v3 

- WASM  (loongclaw + OpenInfer)
- TRINITY  ( OpenFugu )
-  mini-swe-agent v2 vs v1 
-  AgentMesh fork 

---

## 10. v0.41-v0.48  (v2.1 )

> **v2.1 **: 2026-07-06  
> ****:  v0.41-v0.48 ,  §3 / §4 / §7   DONE

### 10.1 11 commits  (v0.41.0 → v0.48.0)

| Commit |  |  |  |  | LOC | commit hash |
|---|---|---|---|---|---|---|
| 1 | v0.41.0 | event O(segments) indexed matching | Puter (code-verified) | +10 | +459 | 2a5afa1 |
| 2 | v0.41.1 | reading_order XY-Cut++ | MinerU algorithm upgrade | +7 | +707 | bb4ebf8 |
| 3 | v0.42.0 | Capability tokens (sandbox.key) | loongclaw | +21 | +813 | fccb5f8 |
| 4 | v0.42.1 | Audit hash chain (sandbox.audit) | loongclaw | +20 | +1074 | e7a0391 |
| 5 | v0.43.0 | exec.parallel() (std threads, NOT tokio) | pi-mono v1 | +9 | +677 | 545bb19 |
| 6 | v0.43.1 | memory.remember + bus.subscribe | pi-agent + Puter/AgentMesh | +12 | +641 | d8bd9c2 |
| 7 | v0.44.0 | sandbox.containerize REAL Docker () | pi-mono | +14 | +1013 | 9c4e49b |
| 8 | v0.45.0 | ToolPlane + ai.retry + ai.role | loongclaw + mini-swe + OpenFugu | +24 | +952 | 4a42e5c |
| 9 | v0.46.0 | SKILL.md + MoraSkillSpec + dual registry | CLI-Anything | +19 | +804 | 2498194 |
| 10 | v0.47.0 | DAG-as-data + heartbeat.md + context.trim | OpenFugu + mimiclaw + pi-agent | +34 | +1145 | 4bebaa5 |
| 11 | v0.48.0 | plan.update + mora.refine | pi-agent + CLI-Anything | +30 | +1518 | edab45e |
| **** | | | | **+200** | **+9803** | |

### 10.2 v0.41-v0.48 

 §3 / §4 / §7 , :

|  |  |  |
|---|---|---|
| §3.1 P0: `event` O(segments) |  DONE | v0.41.0 |
| §3.1 P0: `reading_order` XY-Cut++ |  DONE | v0.41.1 |
| §3.1 P0: `ccr` SHA-256 |  NOT IMPL | (deferred to v0.49+, see §3.4) |
| §3.2 P1: `sandbox.key` + Capability |  DONE | v0.42.0 |
| §3.2 P1: `audit.jsonl` + AuditSink |  DONE | v0.42.1 |
| §3.2 P1: `exec.parallel` |  DONE (std, NOT tokio) | v0.43.0 |
| §3.2 P1: `memory.remember/recall` |  DONE | v0.43.1 |
| §3.2 P1: `bus.subscribe/publish` |  DONE | v0.43.1 |
| §3.2 P1: `orchestrate { on: }` |  DONE (pre-existing v0.25) | v0.44.0 |
| §3.2 P1: `sandbox.containerize` Gondolin |  DONE as **REAL Docker** | v0.44.0 |
| §3.3 P2: ToolPlane Core/Extension |  DONE | v0.45.0 |
| §3.3 P2: ai.retry |  DONE | v0.45.0 |
| §3.3 P2: ai.role |  DONE | v0.45.0 |
| §3.3 P2: SKILL.md +  |  DONE | v0.46.0 |
| §3.3 P2: DAG-as-data (OpenFugu) |  DONE | v0.47.0 |
| §3.3 P2: heartbeat.md (mimiclaw) |  DONE | v0.47.0 |
| §3.3 P2: context.trim (pi-agent+AgentMesh) |  DONE | v0.47.0 |
| §3.3 P2: mora refine (CLI-Anything) |  DONE | v0.48.0 |
| §3.3 P2: plan.update (pi-agent) |  DONE | v0.48.0 |
| §3.3 P2: ai_infra  |  NOT IMPL | (master doc §3.3 OpenInfer) |
| §3.3 P2: agent.trust (arshadvani3) |  NOT IMPL | (master doc §3.3 P3 ) |
| §3.3 P2: agent.protocol (agentmesh-protocol) |  NOT IMPL | (master doc §3.3 P3 ) |

### 10.3  ( §3 / §4 / §7  `🟢`  ` DONE`)

**§3.1 P0 ** ( ):
-  DONE `event`: O(segments) indexed matching (v0.41.0)
-  DONE `reading_order`: XY-Cut++ (v0.41.1)
- 🟡 DEFERRED `ccr` SHA-256 (master doc §3.3 future exploration)

**§3.3 P2 **:
-  DONE ToolPlane (v0.45.0)
-  DONE SKILL.md (v0.46.0)
-  DONE DAG-as-data (v0.47.0)
-  DONE heartbeat.md (v0.47.0)
-  DONE context.trim (v0.47.0)
-  DONE mora.refine (v0.48.0)
-  DONE plan.update (v0.48.0)
-  DONE sandbox.containerize REAL Docker (v0.44.0,  metadata-only )
- 🟡 DEFERRED ai_infra  (OpenInfer, v0.49+)
- 🟡 DEFERRED agent.trust / agent.protocol (P3 , v0.49+)

**§4 v0.41 ** (5 commits ):
-  #1 v0.41.0 event O(segments)
-  #2 v0.41.1 reading_order XY-Cut++
-  #3 v0.42.0 sandbox.key + Capability
-  #4 v0.42.1 audit.jsonl + AuditSink
-  #5 v0.43.0 exec.parallel (std threads, NOT tokio — project rule)

**§7  (v0.41-v0.50 )**:
- v0.41.0 → v0.41.1 → v0.42.0 → v0.42.1 → v0.43.0 → v0.43.1 → v0.44.0 → v0.45.0 → v0.46.0 → v0.47.0 → v0.48.0: **  DONE**
- v0.49.0+ (P2 deferred items + v1.0 future exploration): 

### 10.4  (v2 → v2.1 )

1. **v0.44.0 metadata-only **:
   - : `sandbox.containerize()`  v1.0+ (master doc §3.4 future exploration)
   - :  **REAL Docker** via `docker run` CLI spawn,  `docker exec` / `docker rm -f`
   - :  metadata-only  (`b1cdf6a` → `9c4e49b`)

2. **v0.43.0 tokio **:
   -  (master doc §6.5): `tokio::process::Command` + `tokio::sync::Semaphore`
   - :  `std::thread::spawn` + `std::process::Command` +  `Semaphore` (AtomicUsize + Condvar)
   - : AGENTS.md / Cargo.toml  "async runtime"

3. **v0.45.0 ToolPlane additive not replacement**:
   -  (master doc §6.5): ToolPlane  `tool_registry`
   - :  ( `tool_planes` field,  `tool_registry`)
   - : ,  v0.46+

4. **v0.48.0 mora.refine REAL file I/O**:
   - :  (CLI-Anything /refine)
   - :  +  .refine/  ( instruction header)
   -  v0.44.0 ,  metadata-only

### 10.5 17  MCP 

|  | v2  | v0.41-v0.48  |
|---|---|---|
| loongclaw |  |  Capability (v0.42.0), AuditSink (v0.42.1), ToolPlane (v0.45.0) |
| mini-swe-agent |  |  exec.parallel (v0.43.0,  v1 ), ai.retry (v0.45.0) |
| CLI-Anything |  |  SKILL.md (v0.46.0), mora.refine (v0.48.0) |
| AIOS |  | ⏸ tool_conflict_map  |
| mimiclaw |  |  heartbeat.md (v0.47.0) |
| OpenFugu |  |  DAG-as-data (v0.47.0, ), ai.role (v0.45.0) |
| OpenInfer |  | 🟡 Deferred (ai_infra  v0.49+) |
| MinerU |  |  XY-Cut++ (v0.41.1) |
| Headroom |  | 🟡 ccr SHA-256 deferred (v0.49+) |
| Puter |  |  event O(segments) (v0.41.0, code-verified) |
| pi-mono / pi-agent |  |  exec.parallel (v0.43.0, v1 ), memory.remember (v0.43.1), ai.context.trim (v0.47.0), plan.update (v0.48.0) |
| AgentMesh |  | 🟡 agent.trust / agent.protocol P3  deferred |
| multi-agent-revenue-orchestrator |  | ⏸  |
| ai-coder-symphony |  | ⏸  +  |
| vesh-agents |  | ⏸  () |
| AgentMesh Go (hupe1980) |  | 🟡 BSP  deferred (v0.49+ orchestrate { barrier: true }) |
| Solace Agent Mesh |  |  bus.subscribe (v0.43.1, ) |

### 10.6 v0.49+  (v2.1 )

 §3 + §4 , v0.49+ :

| P |  |  |  |
|---|---|---|---|
| P2 | `ai_infra`  | OpenInfer (v0.49) | 🟡 |
| P2 | `agent.trust(score, decay)` | arshadvani3/AgentMesh fork (v0.49) | 🟡 |
| P2 | `agent.protocol(envelope)` | agentmesh-protocol (v0.49) | 🟡 |
| P3 | `ccr` SHA-256 (, ) | Headroom (v0.49) | 🟡 |
| P3 | `orchestrate { barrier: true }` (BSP ) | hupe1980/AgentMesh (v0.49) | 🟡 |
| Future | WASM  (wasmtime) | loongclaw, OpenInfer (v1.0+) | ⏸ |
| Future | TRINITY  | OpenFugu (v1.0+, ) | ⏸ |
| Future | 5  DI  | Puter (v1.0+, ) | ⏸ |
| Future | serde_yaml / serde_json  | (, v1.0+) | ⏸ |
| Future | Gondolin micro-VM  | pi-mono (v1.0+) | ⏸ |
| Future | OpenShell policy-controlled  | pi-mono (v1.0+) | ⏸ |

### 10.7 

- **11 commits** (v0.41.0 → v0.48.0) — master doc §4  P0/P1/P2 
- **+200 tests** (test  + )
- **+9803 LOC** (impl + tests + wiring)
- **+1 Cargo dep** (`sha2 = "0.10"` for audit)
- **0 breaking change to public API** (all new builtins additive,  `Arc<Mutex<>>`)
- **561 tests pass total** (lib 555 + bin 6)
- **clippy clean, fmt clean, all targets build**
- ** Docker /  I/O**  metadata-only 

### 10.8 v0.41-v0.48 

v2.1  master doc §4 first wave (P0/P1/P2  18  patch, 11 commits ).

**v1.0+ ** (master doc §3.4 future exploration):
-  GPU / ML runtime / micro-VM 
-  (serde_yaml, serde_json, wasmtime)
-  (5-layer DI container)

 v1.0 ,  v0.x first wave .

---

> ****: mora-lang `RESEARCH_PRIMITIVES_MASTER_v2.1.md` —  v2.1 
> (v2.1:  v0.41-v0.48 ,  §10)
> 
> ****: [RESEARCH_PRIMITIVES_MASTER.md](./RESEARCH_PRIMITIVES_MASTER.md) v1
> ** (v1 → v2 → v2.1)**: 17  MCP  →  → 11 commits  (200 tests + 9803 LOC)