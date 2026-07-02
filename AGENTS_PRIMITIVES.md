# Mora v0.32+ 原语路线图 — 从 7 个 AI 基础设施项目提取

> **目的**: 通过深度源码分析 7 个 AI 基础设施项目 (AIOS, MimiClaw, OpenFugu, OpenInfer, MinerU, Headroom, Puter),
> 提取**实现原理**, 映射到 Mora 语言原语, 形成 v0.32+ 演进路线图.
>
> **方法**: 不仅看 README, 深入分析源码 / 架构 / 关键算法 (如 OpenFugu 的 sep-CMA-ES + SVF, Headroom 的
> Rust SmartCrusher 端口, MinerU 的 group-based layout + reading order, Puter 的 5 层 DI + Event Bus).
>
> **项目基线**: Mora v0.31 (SmartCrusher 完整 + no-panic refactor), 已有 `compress / document / mcp /
> http_server / orchestrate / event (v0.31+)` 等模块.

---

## 0. 项目调研摘要

| 项目 | 核心机制 | Mora 现状差距 |
|---|---|---|
| **AIOS** | 中央调度器 (FIFO/RR) + Tool Manager hashmap 冲突锁 + LLM Core 抽象 (3 后端) + Context snapshot | 缺调度器/冲突锁/统一 LLM 接口 |
| **MimiClaw** | ReAct agent loop + message bus (统一入口) + cron (6 字段 struct) + heartbeat (周期性 checklist) + tool/skill 区分 | 缺 ReAct 内置、cron、heartbeat、skill |
| **OpenFugu** | Policy-over-models (19K 参数 router) + per-turn role (Worker/Thinker/Verifier) + DAG-as-data + evidence grading | Mora `orchestrate` 是 imperative, 缺 DAG 数据结构 |
| **OpenInfer** | "Stitch together" 架构 (复用 vLLM frontend) + feature-gated kernels + Pegaflow KV 分层 + prefix cache | Mora 缺 OpenAI 兼容 serve + prefix cache |
| **MinerU** | Group-based layout (fig-caption 配对) + 3 reading order 策略 (XY-cut / gap-tree / group) + multimodal specialist + lossless-first 短路 | Mora `document` 缺 grouped layout + reading order |
| **Headroom** | ContentRouter + SmartCrusher (statistical 字段检测) + CCR (Compress-Cache-Retrieve) + DocumentCompactor recursive walker + CcrStore trait | Mora v0.30 SmartCrusher 基础版, 缺 CCR + recursive walker |
| **Puter** | 5 层 DI 容器 (clients/drivers/stores/services/controllers) + lifecycle hooks + EventClient wildcard (outer.*) + Service Extension 注册 | Mora 缺 DI + lifecycle + wildcard event + token compression |

---

## 1. 直接源自某个项目的高价值原语

### 1.1 `plan` 原语 — 灵感 OpenFugu Conductor (P0)

**机制**: Conductor LLM 在单次 forward pass 发射完整 workflow DAG: 3 个等长 list
`model_id[N] / subtasks[N] / access_list[N][prev_indices]`. `access_list[i]` 只引用
前序步骤, 强制拓扑有效. Executor 按 `t=0..N-1` 顺序执行, 每步收集
`access_list[t]` 索引的输出作为 context.

**Mora 语法** (草案):
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
let report = dag.execute()  # 按拓扑序执行, 注入依赖输出
```

**实施**:
- `src/plan/mod.rs` 新模块
- `plan` 关键字 + `dag.execute()` builtin
- 解析 `workers: [...]` + `steps: [...]` AST
- Runner 按 `depends` 拓扑排序
- 每个 step 调对应 worker 的 LLM, prompt 注入依赖 step 的输出

**复用 Mora 现有**: `orchestrate` 可作为 `plan` 的简化版 (sequential/loop)

---

### 1.2 `react` 原语 — 灵感 MimiClaw ReAct Loop (P0)

**机制**: MimiClaw `agent_loop.c` 实现 ReAct:
- `MIMI_AGENT_MAX_TOOL_ITER=10` (最多 10 轮思考-行动循环)
- `MIMI_MAX_TOOL_CALLS=4` (单次响应最多 4 个 tool call)
- 每轮: 调 LLM → 解析 tool_use → 执行工具 → 把 tool_result 注入下一轮 context
- `Working Status` 先发"thinking…"占位消息

**Mora 语法** (草案):
```mora
let agent = react {
    system: p"You are a research assistant",
    tools: [web_search, calc, file_read],
    max_iter: 10,        # 最多 10 轮
    max_tools_per_turn: 4, # 单次响应最多 4 个 tool call
    working_status: p"thinking...",
}

let answer = agent.run("What's the population of Tokyo?")
# 内部自动: 调 LLM → 触发 web_search → 注入结果 → 调 LLM → 触发 calc → ... → 最终回答
```

**实施**:
- `src/react/mod.rs` 新模块
- `react` 表达式 / `react.run(question)` builtin
- 复用 `ai_infra::CacheWarmer` (v0.24 已写但 dead_code) 缓存 tool 结果
- 复用 `prompt_section` 模块拼装 system prompt

**Mora 现状**: 用户用 AI 必须自己写 `while iter < 10 { ... }` 循环 — `react` 内置后零代码

---

### 1.3 `event` wildcard — 灵感 Puter EventClient (P0)

**机制**: Puter `EventClient.emit("outer.gui.item.removed")` 触发所有匹配 listener:
- 精确 `outer.gui.item.removed`
- `outer.gui.item.*` (single-segment wildcard)
- `outer.gui.*` 
- `outer.*` (catch-all)

Extension 自动注册 listener (`extension.on(event, handler)`).

**Mora 现状** (v0.31): `bus.emit("file.changed")` + `bus.on("file.*")` — 已有 dot prefix,
**缺**: segment wildcard `*`. Puter 用它做 cache invalidation (`fs.last-change:<user_id>`),
Mora 可用于 "tool 完成时触发 metric 更新" `tool.*.completed → metric.update`.

**实施**:
- `src/event/bus.rs` 扩展 matcher 支持 `*` segment
- 加 `bus.emit_and_wait("event", payload)` 同步 await 版本
- 加 `bus.priority("event", prio)` listener 优先级

**Mora 语法** (草案):
```mora
bus.on("tool.*.completed", fn(name, result) {
    metric.increment("tool_calls", {tool: name})
})
bus.on("ai.chat.*", fn(conv, msg) {
    memory.store(conv.id, msg)
})
```

---

### 1.4 `document.grouped_layout` — 灵感 MinerU Group-based Layout (P1)

**机制**: MinerU 不用 "每个块独立", 而是用 **group**:
- figure + caption 配对为 1 个 group
- table + title + footnote 配对
- molecule + identifier 配对
- Group 作为 layout tree 的内部节点, 跨页跨列保留语义

**Mora 现状**: `document.parse` 返回 flat `[{block}, {block}, ...]`, 丢 caption-table 关联.

**实施**:
- `src/document/grouped.rs` 新模块
- 复用 `DocumentBackend` trait
- 加 `GroupedDocument` struct
- 用 bbox 重叠度 + 距离启发式配对
- 加 `group.to_rag_chunks()` builtin 输出 RAG-ready 块

**Mora 语法** (草案):
```mora
let doc = document.parse("paper.pdf", {group: true})
let chunks = doc.grouped.to_rag_chunks()  # [chunk_with_caption+table+footnote, ...]
```

---

### 1.5 `document.reading_order` — 灵感 MinerU 3 策略 (P1)

**机制**: MinerU 3 reading order 算法:
1. **XY-cut**: 递归按 dominant whitespace 划分, 生成 binary reading tree
2. **Gap-tree**: 用 inter-block whitespace + 几何接近度 + 对齐线索
3. **Group-based**: 用 group 内 caption-figure 等语义关联 (与 1.4 配合)

**Mora 现状**: `document.text()` 按物理顺序, 多列 PDF 乱序

**实施**:
- `src/document/reading_order.rs` 新模块
- 3 算法可单独调用或 chain
- 输出 `Block { content, bbox, reading_order_idx }` 列表

**Mora 语法** (草案):
```mora
let doc = document.parse("paper.pdf")
let ordered = doc.reading_order({strategy: "xycut + group"})
```

---

### 1.6 `schedule` 原语 — 灵感 MimiClaw Cron (P1)

**机制**: MimiClaw `cron_job_t` 6 字段:
- `id` (8-char hex) / `name` (32 char) / `kind` (EVERY/AT) /
  `interval_s` / `at_epoch` / `message` / `channel` / `chat_id` / `delete_after_run`
- 60s tick loop, JSON 持久化

**Mora 现状**: 0

**实施**:
- `src/schedule/mod.rs` 新模块
- `schedule` builtin + `list_jobs` + `remove_job`
- 复用 `bus` 触发 (`bus.emit("schedule.tick", job)`)
- 复用 `Conversation` 或新 struct

**Mora 语法** (草案):
```mora
let id = schedule({
    name: "daily_summary",
    kind: "every",
    interval_s: 86400,
    message: p"Generate daily summary of news"
})
schedule.list()        # [{id, name, ...}, ...]
schedule.remove(id)    # 删除
```

---

### 1.7 `heartbeat` 原语 — 灵感 MimiClaw Heartbeat Service (P2)

**机制**: 30min 周期扫描 `HEARTBEAT.md` checklist, 发现 `[ ]` (未完成) 触发 agent.
关键: 周期性 file scan, 不需 cron 注册, **让 agent 主动起来做事**.

**Mora 现状**: 0

**Mora 语法** (草案):
```mora
heartbeat({
    file: "TODO.md",
    interval_min: 30,
    prompt: p"Check TODO.md and act on pending items"
})
```

---

### 1.8 `skill` 原语 — 灵感 MimiClaw Skills (P1)

**机制**: Skills = `/spiffs/skills/*.md` markdown 教学文件. Tool = C function (atomic action),
Skill = markdown workflow. 关键设计:
- Title + Description (H1) + Steps + Examples
- `extract_title/description` 解析器只注入 summary 到 system prompt
- 按需 read_file 全文 (节省 context)

**Mora 现状**: 0 (有 `tool_def` 但无 skill)

**实施**:
- `src/skill/mod.rs` 新模块
- `skill.load("./skills/")` builtin
- `skill.list()` 返回 title + description 索引
- `skill.read(name)` 全文
- `skill.inject_summary(system_prompt)` 拼到 prompt

**Mora 语法** (草案):
```mora
let sys = skill.inject_summary(p"You are an assistant")
# sys 现在含 "- **Daily Briefing**: ... (read with: skill.read('daily-briefing'))"
```

---

### 1.9 `sandbox` 原语 — 灵感 AIOS + Puter (P1)

**机制**:
- **AIOS Access Manager**: hashmap agent_id → privilege_group
- **Puter iframe sandbox**: 显式 `allow-popups-to-escape-sandbox` + URL 限制
- **MimiClaw path validation**: read_file/write_file 拒绝 `..` 路径

**Mora 现状**: 0

**实施**:
- `src/sandbox/mod.rs` 新模块
- `sandbox` 块 + `tool.with_sandbox(allow, deny)` builtin
- 强制 `file.read` / `file.write` 路径 validate

**Mora 语法** (草案):
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

### 1.10 `policy` 原语 — 灵感 AIOS LLM Core + OpenFugu TRINITY (P2)

**机制**: OpenFugu TRINITY 用 19K 参数 router 学习 "which worker for which query". 训
练用 sep-CMA-ES (gradient-free, 离散路由决策). Mora 缺 LLM routing policy.

**实施**:
- `src/policy/mod.rs` 新模块
- `policy.train(router, dataset)` builtin
- `policy.predict(query, workers) → worker_id` builtin
- 复用 Mora `route` + `orchestrate` 抽象

---

### 1.11 `ccr` 原语 — 灵感 Headroom Compress-Cache-Retrieve (P1)

**机制**: Headroom 关键的 "lossy but recoverable" 设计:
- Lossless 路径失败 → Lossy 路径, **原值存档到 CcrStore**
- 输出含 `<<ccr:HASH,KIND,SIZE>>` marker (12-char SHA-256 hex + kind + size)
- LLM 通过 tool call 拉回原值 (`headroom_retrieve`)
- CcrStore trait: InMemory (default) / Redis / S3

**Mora 现状**: 0 (v0.30 SmartCrusher 是 lossless-first 但无 CCR)

**实施**:
- `src/ccr/store.rs` (trait + InMemoryCcrStore impl)
- 扩展 `crush_json` 内部: lossy 路径自动写 CcrStore
- `mora.ccr.retrieve("HASH")` builtin

**Mora 语法** (草案):
```mora
let r = compress.smart_json(big_data, {target_ratio: 0.1})
# 输出含 <<ccr:abc123def456,kv,42>> marker
let original = mora.ccr.retrieve("abc123def456")
```

---

### 1.12 `prefix_cache` builtin — 灵感 OpenInfer (P2)

**机制**: OpenInfer warm prefix cache — 相同 prompt prefix 直接命中 KV cache, TTFT 大幅降低.
**Mora 现状**: 0

**实施**:
- `src/ai/prefix_cache.rs` 新模块
- `mora.ai.prefix_cache({capacity: 1000})` builtin
- 缓存 key = `p"..."` 模板编译后 hash
- 命中时跳过 prompt template parsing

---

### 1.13 `mora serve --openai` 模式 — 灵感 OpenInfer (P1)

**机制**: OpenInfer 复用 vLLM Rust frontend (OpenAI 协议). Mora 现在有手写 HTTP server,
缺 OpenAI 兼容 endpoint — 让任何 OpenAI SDK / LangChain / LlamaIndex 直接调 mora 脚本.

**实施**:
- `src/http_server.rs` 加 `/v1/chat/completions` 路由
- 转 Mora 内部 AI call
- 单 binary 命令: `mora serve script.mora --port 8080 --openai`

**Mora 语法** (命令行):
```bash
$ mora serve --openai examples/agent.mora --port 8080
# 等价于启动 http://localhost:8080/v1/chat/completions
# 任何 OpenAI SDK 直接调
```

---

### 1.14 `ai.chat` with role — 灵感 OpenFugu TRINITY (P1)

**机制**: OpenFugu TRINITY 用 3 role: Worker (干活的) / Thinker (思考的) / Verifier (验证的).
Mora 当前 `ai.chat` 无 role 概念.

**Mora 语法** (草案):
```mora
let worker_out = ai.chat(p"Code: {task}", role: "worker")
let think_out = ai.chat(p"Verify worker output: {worker_out}", role: "thinker")
let verif_out = ai.chat(p"Accept? y/n: {think_out}", role: "verifier")
```

---

### 1.15 `tiered_memory` builtin — 灵感 OpenInfer Pegaflow + MimiClaw SPIFFS (P2)

**机制**: Pegaflow KV cache 分层 HBM→DRAM→SSD→RDMA. MimiClaw 用 SPIFFS (flash) 作
persistent storage. Mora `Conversation` 已有 hot/warm 思路, 缺统一接口.

**实施**:
- `src/memory/tiered.rs` 新模块
- `tiered_memory({hot: ram, warm: file, cold: s3})` builtin
- 自动按 LRU 迁移

---

### 1.16 `lifecycle` 关键字 — 灵感 Puter (P2)

**机制**: Puter 3 lifecycle hook:
- `onServerStart()`: server 启动后 (DB migration, timer start)
- `onServerPrepareShutdown()`: 停接受新请求
- `onServerShutdown()`: 关闭连接

**Mora 现状**: 0

**Mora 语法** (草案):
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

## 2. 跨项目共性原语

### 2.1 DI 容器 — Puter 5 层 (P3)

**机制**: `clients → drivers → stores → services → controllers`, 严格单向依赖.
Service 通过构造器注入.

**Mora 现状**: 0 (Mora 模块化是 file-level, 无 runtime DI)

**实施**:
- `src/di/container.rs` 新模块
- `di.register("db", db_instance)` builtin
- `di.resolve("auth")` 注入 service

**Mora 语法** (草案):
```mora
let container = di.new()
container.register("db", db.sqlite("app.db"))
container.register("cache", cache.lru({capacity: 1000}))
container.register("auth", auth.service({db: container.get("db")}))

let auth = container.get("auth")
```

---

### 2.2 Error Gradation — OpenFugu evidence grade (P3)

**机制**: OpenFugu 用 6 级 evidence grade (🟢 EXEC / 🔵 CODE / 🟣 DATA / 🟡 DOC / 🟠 INFER / 🔴 DARK)
标记每条 claim 的可信度.

**Mora 现状**: 0

**实施**:
- `src/diagnostics/grade.rs` 新模块
- `let g = grade.claim("Mora is fast", based_on: ["bench 10s", "test pass"])` builtin
- 输出 `[grade: 🟢 confidence 0.95]`

**Mora 语法** (草案):
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

**机制**: Headroom `DocumentCompactor.walk` 递归遍历整个 JSON 树找可压缩点.
MinerU 默认 fast mode 是 "lossless" (有 text layer 就直接 extract), 失败才 OCR.

**Mora 现状**: v0.30 SmartCrusher 只处理 top-level List. 嵌套 object/array 里的 list 不压缩.

**实施**:
- 扩展 `crush_json` 接受 nested 结构
- 加 `try_lossless_compact_nested` builtin
- 输出 compact 格式 (csv-schema, markdown-kv)

**Mora 语法** (草案):
```mora
let nested = {
    "user_data": [
        {"id": 1, "name": "alice"},
        ...
    ],
    "metadata": {...}
}
let r = compress.smart_json(nested, {recursive: true})
# 默认走 lossless compaction, 失败才 lossy + CCR
```

---

### 2.4 Mock 模式统一接口 — OpenFugu + OpenInfer (P0)

**机制**: OpenFugu `--mock` mode 训练 sep-CMA-ES 验证算法. OpenInfer 不引入 torch,
全用 Rust mock 测. Mora 已有 mock AI 模式 (OpenAI 不可用时返回 stub), 但分散在多处.

**Mora 现状**: `AiConfig` 默认 mock 模式, 但 `compress_demo.mora` 之类的 demo 没法跑

**实施**:
- `src/mock/registry.rs` 统一 mock 接口
- `mock.register("ai.chat", fn(prompt) { return "[mock response]" })`
- `mock.mode("ai")` 启用 AI 模拟

---

### 2.5 Cross-page merge — MinerU (P2)

**机制**: MinerU cross-page consolidation: 同段落跨页合并, 表格跨页续接, 反应图跨页.

**Mora 现状**: 0

**实施**:
- `src/document/cross_page.rs` 新模块
- 复用 `grouped_layout` (1.4)
- `doc.merge_cross_page()` builtin

---

## 3. Mora 已有但待增强

| 原语 | 现状 | 增强方向 | 灵感 |
|---|---|---|---|
| `compress.json` | v0.30 SmartCrusher | 加 DocumentCompactor recursive walker | Headroom |
| `event` | v0.31 dot-separated | 加 wildcard `outer.*` | Puter |
| `memory.store/recall` | 基础 hashmap | 加 tiered (hot/warm/cold) | OpenInfer + MimiClaw |
| `document.parse` | 6 backend | 加 grouped layout + reading order | MinerU |
| `ai.chat` | mock 模式 | 加 role 参数 | OpenFugu |
| `route` | 3 model 静态 | 加 learned policy | OpenFugu + AIOS |
| `tool_def` | 静态注册 | 加 sandbox 权限 | AIOS + Puter |

---

## 4. 实施路线图

### v0.32 (近期) — 4-6 周
- [ ] **#1.1 plan (DAG)** — 1.5 周
- [ ] **#1.2 react (ReAct 循环)** — 1.5 周
- [ ] **#1.3 event wildcard** — 0.5 周
- [ ] **#1.13 OpenAI 兼容 serve** — 1 周
- [ ] **#2.3 Lossless-First recursive walker** — 1 周
- [ ] **#2.4 Mock 模式统一** — 0.5 周

### v0.33 — 6-8 周
- [ ] **#1.4 document.grouped_layout** — 2 周
- [ ] **#1.5 document.reading_order** — 2 周
- [ ] **#1.6 schedule cron** — 1 周
- [ ] **#1.8 skill** — 1 周
- [ ] **#1.9 sandbox** — 1 周
- [ ] **#1.11 ccr** — 1.5 周

### v0.34+ 远期
- [ ] **#1.7 heartbeat** — 0.5 周
- [ ] **#1.10 policy** — 2 周
- [ ] **#1.12 prefix_cache** — 1 周
- [ ] **#1.14 ai.chat role** — 0.5 周
- [ ] **#1.15 tiered_memory** — 1.5 周
- [ ] **#1.16 lifecycle** — 0.5 周
- [ ] **#2.1 DI 容器** — 2 周
- [ ] **#2.2 Error Gradation** — 1 周
- [ ] **#2.5 cross-page merge** — 1.5 周

---

## 5. 设计原则 (贯穿所有原语)

1. **复用优先**: 优先扩展 Mora 现有模块 (compress / document / event / memory), 不创造平行体系
2. **0 新外部依赖**: 与 v0.29 Global Constraint 一致 (z-score 用 stdlib, Pegaflow-style tiered 用现有 file API)
3. **正交性**: 每个新原语可独立启用, 互不依赖
4. **可观测**: 所有 builtin 产出 `trace` / `observe` 兼容事件
5. **可测试**: 每个新 builtin 必须有 unit + e2e test
6. **Mora 风格**: 英文关键字, 错误信息中英双语
7. **panic 0 容忍**: 复用 v0.31 panic refactor 模式, lexer/parser 不允许 panic
8. **module 化**: 新原语独立 `src/<name>/mod.rs`, 不进主 lib.rs

---

## 6. 与 Mora 现状的兼容性

按 AGENTS.md 规则 **"不维护旧版本兼容"**: v0.32+ 可直接 breaking change 现有 API.
但 v0.30 SmartCrusher 已是 recent breaking change, v0.32 主要是**新增**, 不破坏 v0.30 已有 API.

**不破坏**:
- `compress.text / json / summary` 字符串策略
- `ai.chat / ai.stream / p"..."` AI 原语
- `document.parse` 6 backend
- `mcp_server / http_server / lsp` 服务

**可能破坏**:
- `event` 关键字 — dot-separated 语义保留, 加 `*` wildcard 是扩展
- `ai.AiConfig` 结构 — 加 `role` 字段是 optional, 默认不变
- `tool_def` — 加 `sandbox` 块是 optional

---

## 7. 关键参考链接

| 项目 | 链接 | 关键源码 |
|---|---|---|
| AIOS | https://github.com/agiresearch/AIOS | `aios_kernel/scheduler/`, `aios_kernel/llm_cores/` |
| MimiClaw | https://github.com/memovai/mimiclaw | `main/agent/agent_loop.c`, `main/cron/cron_service.c`, `main/skills/skill_loader.c` |
| OpenFugu | https://github.com/trotsky1997/OpenFugu | `openfugu/mini.py` (FuguRouter), `openfugu/ultra.py` (ConductorExecutor) |
| OpenInfer | https://open-infer.org/blog/openinfer-010/ | vLLM Rust frontend, Pegaflow KV 分层 |
| MinerU | https://arxiv.org/html/2512.15098v2 | §2.2 Group-based Layout, §2.8 Reading Order |
| Headroom | https://github.com/chopratejas/headroom | `crates/headroom-core/src/transforms/smart_crusher/` |
| Puter | https://github.com/HeyPuter/puter | `src/backend/server.ts`, `src/backend/clients/event/EventClient.ts` |
