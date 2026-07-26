# Mora v0.32+  —  7  AI 

> ****:  7  AI  (AIOS, MimiClaw, OpenFugu, OpenInfer, MinerU, Headroom, Puter),
> ****,  Mora ,  v0.32+ .
>
> ****:  README,  /  /  ( OpenFugu  sep-CMA-ES + SVF, Headroom 
> Rust SmartCrusher , MinerU  group-based layout + reading order, Puter  5  DI + Event Bus).
>
> ****: Mora v0.31 (SmartCrusher  + no-panic refactor),  `compress / document / mcp /
> http_server / orchestrate / event (v0.31+)` .

---

## 0. 

|  |  | Mora  |
|---|---|---|
| **AIOS** |  (FIFO/RR) + Tool Manager hashmap  + LLM Core  (3 ) + Context snapshot | // LLM  |
| **MimiClaw** | ReAct agent loop + message bus () + cron (6  struct) + heartbeat ( checklist) + tool/skill  |  ReAct cronheartbeatskill |
| **OpenFugu** | Policy-over-models (19K  router) + per-turn role (Worker/Thinker/Verifier) + DAG-as-data + evidence grading | Mora `orchestrate`  imperative,  DAG  |
| **OpenInfer** | "Stitch together"  ( vLLM frontend) + feature-gated kernels + Pegaflow KV  + prefix cache | Mora  OpenAI  serve + prefix cache |
| **MinerU** | Group-based layout (fig-caption ) + 3 reading order  (XY-cut / gap-tree / group) + multimodal specialist + lossless-first  | Mora `document`  grouped layout + reading order |
| **Headroom** | ContentRouter + SmartCrusher (statistical ) + CCR (Compress-Cache-Retrieve) + DocumentCompactor recursive walker + CcrStore trait | Mora v0.30 SmartCrusher ,  CCR + recursive walker |
| **Puter** | 5  DI  (clients/drivers/stores/services/controllers) + lifecycle hooks + EventClient wildcard (outer.*) + Service Extension  | Mora  DI + lifecycle + wildcard event + token compression |

---

## 1. 

### 1.1 `plan`  —  OpenFugu Conductor (P0)

****: Conductor LLM  forward pass  workflow DAG: 3  list
`model_id[N] / subtasks[N] / access_list[N][prev_indices]`. `access_list[i]` 
, . Executor  `t=0..N-1` , 
`access_list[t]`  context.

**Mora ** ():
```mora
let dag = plan {
    workers: ["gpt-4o", "claude-sonnet", "deepseek-coder"]
    steps: [
        { worker: 0, task: p"Research {topic}",  depends: [] }
        { worker: 1, task: p"Analyze findings",   depends: [0] }
        { worker: 2, task: p"Verify claims",      depends: [0, 1] }
        { worker: 0, task: p"Write final report", depends: [2] }
    ]
}
let report = dag.execute()  # , 
```

****:
- `src/plan/mod.rs` 
- `plan`  + `dag.execute()` builtin
-  `workers: [...]` + `steps: [...]` AST
- Runner  `depends` 
-  step  worker  LLM, prompt  step 

** Mora **: `orchestrate`  `plan`  (sequential/loop)

---

### 1.2 `react`  —  MimiClaw ReAct Loop (P0)

****: MimiClaw `agent_loop.c`  ReAct:
- `MIMI_AGENT_MAX_TOOL_ITER=10` ( 10 -)
- `MIMI_MAX_TOOL_CALLS=4` ( 4  tool call)
- :  LLM →  tool_use →  →  tool_result  context
- `Working Status` "thinking…"

**Mora ** ():
```mora
let agent = react {
    system: p"You are a research assistant",
    tools: [web_search, calc, file_read],
    max_iter: 10,        #  10 
    max_tools_per_turn: 4, #  4  tool call
    working_status: p"thinking...",
}

let answer = agent.run("What's the population of Tokyo?")
# :  LLM →  web_search →  →  LLM →  calc → ... → 
```

****:
- `src/react/mod.rs` 
- `react`  / `react.run(question)` builtin
-  `ai_infra::CacheWarmer` (v0.24  dead_code)  tool 
-  `prompt_section`  system prompt

**Mora **:  AI  `while iter < 10 { ... }`  — `react` 

---

### 1.3 `event` wildcard —  Puter EventClient (P0)

****: Puter `EventClient.emit("outer.gui.item.removed")`  listener:
-  `outer.gui.item.removed`
- `outer.gui.item.*` (single-segment wildcard)
- `outer.gui.*` 
- `outer.*` (catch-all)

Extension  listener (`extension.on(event, handler)`).

**Mora ** (v0.31): `bus.emit("file.changed")` + `bus.on("file.*")` —  dot prefix,
****: segment wildcard `*`. Puter  cache invalidation (`fs.last-change:<user_id>`),
Mora  "tool  metric " `tool.*.completed → metric.update`.

****:
- `src/event/bus.rs`  matcher  `*` segment
-  `bus.emit_and_wait("event", payload)`  await 
-  `bus.priority("event", prio)` listener 

**Mora ** ():
```mora
bus.on("tool.*.completed", fn(name, result) {
    metric.increment("tool_calls", {tool: name})
})
bus.on("ai.chat.*", fn(conv, msg) {
    memory.store(conv.id, msg)
})
```

---

### 1.4 `document.grouped_layout` —  MinerU Group-based Layout (P1)

****: MinerU  "",  **group**:
- figure + caption  1  group
- table + title + footnote 
- molecule + identifier 
- Group  layout tree , 

**Mora **: `document.parse`  flat `[{block}, {block}, ...]`,  caption-table .

****:
- `src/document/grouped.rs` 
-  `DocumentBackend` trait
-  `GroupedDocument` struct
-  bbox  + 
-  `group.to_rag_chunks()` builtin  RAG-ready 

**Mora ** ():
```mora
let doc = document.parse("paper.pdf", {group: true})
let chunks = doc.grouped.to_rag_chunks()  # [chunk_with_caption+table+footnote, ...]
```

---

### 1.5 `document.reading_order` —  MinerU 3  (P1)

****: MinerU 3 reading order :
1. **XY-cut**:  dominant whitespace ,  binary reading tree
2. **Gap-tree**:  inter-block whitespace +  + 
3. **Group-based**:  group  caption-figure  ( 1.4 )

**Mora **: `document.text()` ,  PDF 

****:
- `src/document/reading_order.rs` 
- 3  chain
-  `Block { content, bbox, reading_order_idx }` 

**Mora ** ():
```mora
let doc = document.parse("paper.pdf")
let ordered = doc.reading_order({strategy: "xycut + group"})
```

---

### 1.6 `schedule`  —  MimiClaw Cron (P1)

****: MimiClaw `cron_job_t` 6 :
- `id` (8-char hex) / `name` (32 char) / `kind` (EVERY/AT) /
  `interval_s` / `at_epoch` / `message` / `channel` / `chat_id` / `delete_after_run`
- 60s tick loop, JSON 

**Mora **: 0

****:
- `src/schedule/mod.rs` 
- `schedule` builtin + `list_jobs` + `remove_job`
-  `bus`  (`bus.emit("schedule.tick", job)`)
-  `Conversation`  struct

**Mora ** ():
```mora
let id = schedule({
    name: "daily_summary",
    kind: "every",
    interval_s: 86400,
    message: p"Generate daily summary of news"
})
schedule.list()        # [{id, name, ...}, ...]
schedule.remove(id)    # 
```

---

### 1.7 `heartbeat`  —  MimiClaw Heartbeat Service (P2)

****: 30min  `HEARTBEAT.md` checklist,  `[ ]` ()  agent.
:  file scan,  cron , ** agent **.

**Mora **: 0

**Mora ** ():
```mora
heartbeat({
    file: "TODO.md",
    interval_min: 30,
    prompt: p"Check TODO.md and act on pending items"
})
```

---

### 1.8 `skill`  —  MimiClaw Skills (P1)

****: Skills = `/spiffs/skills/*.md` markdown . Tool = C function (atomic action),
Skill = markdown workflow. :
- Title + Description (H1) + Steps + Examples
- `extract_title/description`  summary  system prompt
-  read_file  ( context)

**Mora **: 0 ( `tool_def`  skill)

****:
- `src/skill/mod.rs` 
- `skill.load("./skills/")` builtin
- `skill.list()`  title + description 
- `skill.read(name)` 
- `skill.inject_summary(system_prompt)`  prompt

**Mora ** ():
```mora
let sys = skill.inject_summary(p"You are an assistant")
# sys  "- **Daily Briefing**: ... (read with: skill.read('daily-briefing'))"
```

---

### 1.9 `sandbox`  —  AIOS + Puter (P1)

****:
- **AIOS Access Manager**: hashmap agent_id → privilege_group
- **Puter iframe sandbox**:  `allow-popups-to-escape-sandbox` + URL 
- **MimiClaw path validation**: read_file/write_file  `..` 

**Mora **: 0

****:
- `src/sandbox/mod.rs` 
- `sandbox`  + `tool.with_sandbox(allow, deny)` builtin
-  `file.read` / `file.write`  validate

**Mora ** ():
```mora
sandbox("agent_smith", {
    allow: ["memory.*", "ai.chat(mock)"],
    deny: ["file.write", "shell.*", "http.*"],
    memory_limit_mb: 64,
    timeout_s: 30,
    on_violation: "kill",  # "warn" | "kill" | "throw"
})
```

---

### 1.10 `policy`  —  AIOS LLM Core + OpenFugu TRINITY (P2)

****: OpenFugu TRINITY  19K  router  "which worker for which query". 
 sep-CMA-ES (gradient-free, ). Mora  LLM routing policy.

****:
- `src/policy/mod.rs` 
- `policy.train(router, dataset)` builtin
- `policy.predict(query, workers) → worker_id` builtin
-  Mora `route` + `orchestrate` 

---

### 1.11 `ccr`  —  Headroom Compress-Cache-Retrieve (P1)

****: Headroom  "lossy but recoverable" :
- Lossless  → Lossy , ** CcrStore**
-  `<<ccr:HASH,KIND,SIZE>>` marker (12-char SHA-256 hex + kind + size)
- LLM  tool call  (`headroom_retrieve`)
- CcrStore trait: InMemory (default) / Redis / S3

**Mora **: 0 (v0.30 SmartCrusher  lossless-first  CCR)

****:
- `src/ccr/store.rs` (trait + InMemoryCcrStore impl)
-  `crush_json` : lossy  CcrStore
- `mora.ccr.retrieve("HASH")` builtin

**Mora ** ():
```mora
let r = compress.smart_json(big_data, {target_ratio: 0.1})
#  <<ccr:abc123def456,kv,42>> marker
let original = mora.ccr.retrieve("abc123def456")
```

---

### 1.12 `prefix_cache` builtin —  OpenInfer (P2)

****: OpenInfer warm prefix cache —  prompt prefix  KV cache, TTFT .
**Mora **: 0

****:
- `src/ai/prefix_cache.rs` 
- `mora.ai.prefix_cache({capacity: 1000})` builtin
-  key = `p"..."`  hash
-  prompt template parsing

---

### 1.13 `mora serve --openai`  —  OpenInfer (P1)

****: OpenInfer  vLLM Rust frontend (OpenAI ). Mora  HTTP server,
 OpenAI  endpoint —  OpenAI SDK / LangChain / LlamaIndex  mora .

****:
- `src/http_server.rs`  `/v1/chat/completions` 
-  Mora  AI call
-  binary : `mora serve script.mora --port 8080 --openai`

**Mora ** ():
```bash
$ mora serve --openai examples/agent.mora --port 8080
#  http://localhost:8080/v1/chat/completions
#  OpenAI SDK 
```

---

### 1.14 `ai.chat` with role —  OpenFugu TRINITY (P1)

****: OpenFugu TRINITY  3 role: Worker () / Thinker () / Verifier ().
Mora  `ai.chat`  role .

**Mora ** ():
```mora
let worker_out = ai.chat(p"Code: {task}", role: "worker")
let think_out = ai.chat(p"Verify worker output: {worker_out}", role: "thinker")
let verif_out = ai.chat(p"Accept? y/n: {think_out}", role: "verifier")
```

---

### 1.15 `tiered_memory` builtin —  OpenInfer Pegaflow + MimiClaw SPIFFS (P2)

****: Pegaflow KV cache  HBM→DRAM→SSD→RDMA. MimiClaw  SPIFFS (flash) 
persistent storage. Mora `Conversation`  hot/warm , .

****:
- `src/memory/tiered.rs` 
- `tiered_memory({hot: ram, warm: file, cold: s3})` builtin
-  LRU 

---

### 1.16 `lifecycle`  —  Puter (P2)

****: Puter 3 lifecycle hook:
- `onServerStart()`: server  (DB migration, timer start)
- `onServerPrepareShutdown()`: 
- `onServerShutdown()`: 

**Mora **: 0

**Mora ** ():
```mora
lifecycle {
    on_start: {
        db.migrate()
        bus.on("ai.chat.*", metric.update)
    }
    on_stop: {
        bus.flush()
        memory.flush_all()
    }
}
```

---

## 2. 

### 2.1 DI  — Puter 5  (P3)

****: `clients → drivers → stores → services → controllers`, .
Service .

**Mora **: 0 (Mora  file-level,  runtime DI)

****:
- `src/di/container.rs` 
- `di.register("db", db_instance)` builtin
- `di.resolve("auth")`  service

**Mora ** ():
```mora
let container = di.new()
container.register("db", db.sqlite("app.db"))
container.register("cache", cache.lru({capacity: 1000}))
container.register("auth", auth.service({db: container.get("db")}))

let auth = container.get("auth")
```

---

### 2.2 Error Gradation — OpenFugu evidence grade (P3)

****: OpenFugu  6  evidence grade (🟢 EXEC /  CODE / 🟣 DATA / 🟡 DOC / 🟠 INFER /  DARK)
 claim .

**Mora **: 0

****:
- `src/diagnostics/grade.rs` 
- `let g = grade.claim("Mora is fast", based_on: ["bench 10s", "test pass"])` builtin
-  `[grade: 🟢 confidence 0.95]`

**Mora ** ():
```mora
let g = grade.claim("crush_json saves 80% tokens",
    based_on: [
        "src/compress/json.rs 12 unit tests",
        "compress_demo.mora run output"
    ])
print(g)  # "🟢 EXEC (high confidence: bench + e2e verified)"
```

---

### 2.3 Lossless-First recursive walker — Headroom + MinerU (P0)

****: Headroom `DocumentCompactor.walk`  JSON .
MinerU  fast mode  "lossless" ( text layer  extract),  OCR.

**Mora **: v0.30 SmartCrusher  top-level List.  object/array  list .

****:
-  `crush_json`  nested 
-  `try_lossless_compact_nested` builtin
-  compact  (csv-schema, markdown-kv)

**Mora ** ():
```mora
let nested = {
    "user_data": [
        {"id": 1, "name": "alice"},
        ...
    ],
    "metadata": {...}
}
let r = compress.smart_json(nested, {recursive: true})
#  lossless compaction,  lossy + CCR
```

---

### 2.4 Mock  — OpenFugu + OpenInfer (P0)

****: OpenFugu `--mock` mode  sep-CMA-ES . OpenInfer  torch,
 Rust mock . Mora  mock AI  (OpenAI  stub), .

**Mora **: `AiConfig`  mock ,  `compress_demo.mora`  demo 

****:
- `src/mock/registry.rs`  mock 
- `mock.register("ai.chat", fn(prompt) { return "[mock response]" })`
- `mock.mode("ai")`  AI 

---

### 2.5 Cross-page merge — MinerU (P2)

****: MinerU cross-page consolidation: , , .

**Mora **: 0

****:
- `src/document/cross_page.rs` 
-  `grouped_layout` (1.4)
- `doc.merge_cross_page()` builtin

---

## 3. Mora 

|  |  |  |  |
|---|---|---|---|
| `compress.json` | v0.30 SmartCrusher |  DocumentCompactor recursive walker | Headroom |
| `event` | v0.31 dot-separated |  wildcard `outer.*` | Puter |
| `memory.store/recall` |  hashmap |  tiered (hot/warm/cold) | OpenInfer + MimiClaw |
| `document.parse` | 6 backend |  grouped layout + reading order | MinerU |
| `ai.chat` | mock  |  role  | OpenFugu |
| `route` | 3 model  |  learned policy | OpenFugu + AIOS |
| `tool_def` |  |  sandbox  | AIOS + Puter |

---

## 4. 

### v0.32 () — 4-6 
- [ ] **#1.1 plan (DAG)** — 1.5 
- [ ] **#1.2 react (ReAct )** — 1.5 
- [ ] **#1.3 event wildcard** — 0.5 
- [ ] **#1.13 OpenAI  serve** — 1 
- [ ] **#2.3 Lossless-First recursive walker** — 1 
- [ ] **#2.4 Mock ** — 0.5 

### v0.33 — 6-8 
- [ ] **#1.4 document.grouped_layout** — 2 
- [ ] **#1.5 document.reading_order** — 2 
- [ ] **#1.6 schedule cron** — 1 
- [ ] **#1.8 skill** — 1 
- [ ] **#1.9 sandbox** — 1 
- [ ] **#1.11 ccr** — 1.5 

### v0.34+ 
- [ ] **#1.7 heartbeat** — 0.5 
- [ ] **#1.10 policy** — 2 
- [ ] **#1.12 prefix_cache** — 1 
- [ ] **#1.14 ai.chat role** — 0.5 
- [ ] **#1.15 tiered_memory** — 1.5 
- [ ] **#1.16 lifecycle** — 0.5 
- [ ] **#2.1 DI ** — 2 
- [ ] **#2.2 Error Gradation** — 1 
- [ ] **#2.5 cross-page merge** — 1.5 

---

## 5.  ()

1. ****:  Mora  (compress / document / event / memory), 
2. **0 **:  v0.29 Global Constraint  (z-score  stdlib, Pegaflow-style tiered  file API)
3. ****: , 
4. ****:  builtin  `trace` / `observe` 
5. ****:  builtin  unit + e2e test
6. **Mora **: , 
7. **panic 0 **:  v0.31 panic refactor , lexer/parser  panic
8. **module **:  `src/<name>/mod.rs`,  lib.rs

---

## 6.  Mora 

 AGENTS.md  **""**: v0.32+  breaking change  API.
 v0.30 SmartCrusher  recent breaking change, v0.32 ****,  v0.30  API.

****:
- `compress.text / json / summary` 
- `ai.chat / ai.stream / p"..."` AI 
- `document.parse` 6 backend
- `mcp_server / http_server / lsp` 

****:
- `event`  — dot-separated ,  `*` wildcard 
- `ai.AiConfig`  —  `role`  optional, 
- `tool_def` —  `sandbox`  optional

---

## 7. 

|  |  |  |
|---|---|---|
| AIOS | https://github.com/agiresearch/AIOS | `aios_kernel/scheduler/`, `aios_kernel/llm_cores/` |
| MimiClaw | https://github.com/memovai/mimiclaw | `main/agent/agent_loop.c`, `main/cron/cron_service.c`, `main/skills/skill_loader.c` |
| OpenFugu | https://github.com/trotsky1997/OpenFugu | `openfugu/mini.py` (FuguRouter), `openfugu/ultra.py` (ConductorExecutor) |
| OpenInfer | https://open-infer.org/blog/openinfer-010/ | vLLM Rust frontend, Pegaflow KV  |
| MinerU | https://arxiv.org/html/2512.15098v2 | §2.2 Group-based Layout, §2.8 Reading Order |
| Headroom | https://github.com/chopratejas/headroom | `crates/headroom-core/src/transforms/smart_crusher/` |
| Puter | https://github.com/HeyPuter/puter | `src/backend/server.ts`, `src/backend/clients/event/EventClient.ts` |
