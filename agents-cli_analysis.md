# 项目深度分析：google/agents-cli

> 分析师：AI Agent 架构与编程语言设计分析师
> 分析日期：2026-07-01
> 项目地址：https://github.com/google/agents-cli
> 版本：v1.0.0 GA（2026-07-01 发布）

---

## 1. 项目概览

| 维度 | 详情 |
|------|------|
| **星数** | 4,201 ★ / 454 Forks |
| **主要语言** | Python（100%） |
| **核心定位** | **"让任意编程助手成为 Google Cloud Agent 专家"** —— 它不是 Agent 框架本身，而是**面向编码 Agent 的 CLI + Skill 工具链**，用于构建、评估、部署基于 ADK（Agent Development Kit）的企业级 Agent。 |
| **许可证** | Apache-2.0 |
| **创建时间** | 2026-04-08（非常新的 Google 官方项目） |
| **最新版本** | v1.0.0（2026-07-01 GA） |
| **依赖框架** | Google ADK（google-adk ≥ 2.0）、Vertex AI、Click、Rich、uv |

> **关键认知**：`agents-cli` ≠ Agent 框架。它是 **Agent 的 DevOps 工具链**——类似于 `kubectl` 之于 Kubernetes，但目标用户是**编码 Agent**（Claude Code、Codex、Antigravity CLI 等）。

---

## 2. 核心架构解析

### 2.1 模块划分（src/google/agents/cli/）

```
main.py           # 根 Click 命令组，LazyGroup 懒加载所有子命令
_project.py       # 项目配置解析（agents-cli-manifest.yaml + pyproject.toml 回退）
_tools.py         # 外部工具解析（uv, npx, gcloud, terraform, git 等）
_runner.py        # 子进程运行器（run, run_resolved, popen_resolved_detached）
_click.py         # LazyGroup 实现（命令按需导入，加速启动）
_skills_check.py   # Skill 版本检查与更新

auth.py           # GCP / AI Studio 认证，ADC 解析
dev/              # 开发命令：install, lint, playground
run/              # 运行命令：本地/远程 Agent 运行，A2A/ADK SSE 协议切换
eval/             # 评估体系：generate, grade, compare, analyze, optimize, dataset synthesize
deploy/           # 部署：Agent Runtime, Cloud Run, GKE
infra/            # 基础设施：CI/CD, Terraform, 单项目部署
publish/          # 发布：Gemini Enterprise 注册（ADK / A2A 两种模式）
scaffold/         # 脚手架：create, enhance, upgrade，模板渲染（cookiecutter）
info/             # 项目信息查看
```

### 2.2 关键抽象

| 抽象 | 职责 | 对应代码位置 |
|------|------|-------------|
| `ProjectConfig` | 项目元数据（部署目标、Agent 目录、A2A 标志、CI/CD 配置等） | `_project.py` |
| `LazyGroup` | Click 命令按需加载，CLI 冷启动优化 | `_click.py` |
| `_DispatchTarget` | 本地运行 vs 远程运行 vs Agent Runtime 的统一路由 | `run/cmd_run.py` |
| `AgentEngineConfig` | Vertex AI Agent Runtime 部署规格 | `deploy/agent_runtime.py` |
| `EvaluationDataset` | 评估数据集（Vertex AI 原生格式） | `eval/` |

### 2.3 数据流（典型生命周期）

```
用户意图
  → Phase 0: .agents-cli-spec.md（手写 spec）
  → Phase 1: scaffold create（生成 ~72 文件：Agent 代码、测试、Terraform、CI/CD）
  → Phase 2: Build（编辑 app/agent.py，定义 Agent/Tool/Workflow）
  → Phase 3: Orchestrate（多 Agent 编排：Sequential/Parallel/Loop/Graph）
  → Phase 4: Eval（eval generate → eval grade → iterate 5-10 次）
  → Phase 5: Deploy（deploy → Agent Runtime / Cloud Run / GKE）
  → Phase 6: Publish（publish gemini-enterprise）
  → Phase 7: Observe（Cloud Trace + BigQuery analytics）
  → 生产数据 → 下一轮 Eval 数据集（闭环）
```

---

## 3. 关键机制与模式

### 机制1：Skill-as-Code（面向 AI 的模块化知识包）

`agents-cli` 最大的架构创新是 **Skill 系统**——不是传统代码库中的函数/模块，而是**给编码 Agent 阅读的 Markdown 知识包**。

**结构：**
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
...（完整 API 参考、最佳实践、代码片段）
```

**关键特性：**
- Skill 文件使用 **YAML frontmatter + Markdown body**，可被编码 Agent 直接解析
- 包含 `references/` 子目录，存放深度参考文档（如 `adk-python.md`, `adk-workflows.md`）
- **Always-active skill**（`google-agents-cli-workflow`）提供 8 阶段生命周期指导
- Skill 通过 `npx skills` 或 `agents-cli setup` 分发到各个 IDE（Claude Code、Cursor、Gemini CLI 等）

**代码片段（Skill 元数据解析）：**
```python
# 从 tree 结构看，skills/ 目录下每个 Skill 是一个独立目录：
skills/
  google-agents-cli-workflow/SKILL.md          # 8 阶段生命周期
  google-agents-cli-adk-code/SKILL.md          # ADK API 参考
  google-agents-cli-adk-code/references/adk-python.md
  google-agents-cli-adk-code/references/adk-workflows.md
  google-agents-cli-scaffold/SKILL.md          # 脚手架模板
  google-agents-cli-eval/SKILL.md            # 评估方法论
  google-agents-cli-deploy/SKILL.md          # 部署指南
  google-agents-cli-publish/SKILL.md         # 发布注册
  google-agents-cli-observability/SKILL.md   # 可观测性
```

---

### 机制2：Graph-based Workflow（确定性编排层）

ADK 2.0 引入的 `Workflow` 不是 LLM 驱动的，而是**显式图结构**——节点做工作，边定义流，`START` 是入口。

**API 签名：**
```python
from google.adk.workflow import Workflow, node, JoinNode, RetryConfig
from pydantic import BaseModel

# 节点：函数、Agent、Tool 都可以是节点
@node
def classify(node_input: str) -> str:
    return "urgent" if "urgent" in node_input else "normal"

# 边定义控制流
root_agent = Workflow(
    name="pipeline",
    edges=[
        ('START', classifier),
        (classifier, urgent_handler, "urgent"),   # 条件边
        (classifier, normal_handler, "normal"),
        (classifier, fallback_handler, '__DEFAULT__'),  # 默认路由
    ],
    max_concurrency=4,
    timeout=300,
)
```

**关键模式：**

| 模式 | 语法 | 用途 |
|------|------|------|
| 顺序链 | `[(START, a), (a, b), (b, c)]` | 经典 pipeline |
| 条件路由 | `(node, target, "route_name")` | 分支逻辑 |
| Fan-out | `(START, (branch_a, branch_b, branch_c))` | 并行执行 |
| Fan-in | `JoinNode` + `((a, b), join), (join, final)` | 汇聚结果 |
| 循环 | 节点返回 `Event(route="continue")` | 直到满足条件退出 |
| 并行 Worker | `@node(parallel_worker=True)` | 列表元素并发处理 |

**重要约束：**
- 无条件循环被拒绝（必须至少一条带路由的边）
- 在 Workflow 中 **禁用 `output_schema` 的 LLM Agent 会禁用工具调用**
- 函数节点默认 `rerun_on_resume=False`；LLM Agent 默认 `rerun_on_resume=True`

---

### 机制3：Eval-first Quality Gate（评估优先部署门控）

`agents-cli` 将 **评估** 提升为核心工程实践，不是可选的"测试"，而是**部署前的强制门控**。

**评估体系：**
```python
# 1. 生成 trace（运行 Agent 在数据集上）
agents-cli eval generate

# 2. 评分（LLM-as-judge + 自定义指标）
agents-cli eval grade --metrics final_response_quality,grounding

# 3. 对比（修复前后对比）
agents-cli eval compare prev.json latest.json

# 4. 分析失败模式聚类
agents-cli eval analyze --eval-result latest.json

# 5. 自动调优提示词
agents-cli eval optimize

# 6. 合成数据集（冷启动）
agents-cli eval dataset synthesize --count 10
```

**评估数据结构：**
```python
# Vertex AI EvaluationDataset 格式
class EvalCase:
    agent_data: dict        # 包含 turns, events, agents 映射
    responses: list         # ResponseCandidate 列表
    # 评分后附加 metric_results
```

**关键设计：**
- `eval generate` 使用 **子进程隔离** 每次加载 fresh agent（避免 asyncio.Lock 绑定错误事件循环）
- `eval grade` 支持 **本地自定义指标**（在 CLI 进程中运行）和 **远程指标**（Vertex AI CodeExecution sandbox）
- 失败案例被**丢弃**不写 artifact，确保下游只看见成功的 trace
- 明确区分：`pytest` 测代码正确性，`eval` 测 Agent 行为质量，`run` 做快速 smoke test

---

### 机制4：State Prefix Namespace（状态作用域前缀）

ADK 的 `Session.state` 不是扁平字典，而是**带前缀命名空间的分层状态系统**。

```python
# Session 级别（默认，当前对话）
state["booking_step"] = 2

# User 级别（跨会话持久）
state["user:preferred_language"] = "en"

# App 级别（全局）
state["app:total_queries"] = 1000

# Temp 级别（当前调用，不持久）
state["temp:intermediate_result"] = data
```

**价值：** 在单 Agent 中自然区分"本次对话状态"、"用户长期记忆"、"全局配置"，无需额外数据库抽象。

---

### 机制5：Tool Confirmation Gates（工具执行门控）

ADK 提供**多层次的工具执行控制**，不是简单的 allow/deny：

```python
from google.adk.tools import FunctionTool

# 1. 简单确认：每次执行前弹窗/暂停
sensitive_tool = FunctionTool(delete_record, require_confirmation=True)

# 2. 条件确认：金额超过阈值才需要审批
def needs_approval(amount: float, **kwargs) -> bool:
    return amount > 1000
transfer_tool = FunctionTool(transfer_money, require_confirmation=needs_approval)

# 3. 在工具内部请求确认
tool_context.request_confirmation(hint="Approve this transfer?")

# 4. 长运行工具：异步等待外部结果
from google.adk.tools import LongRunningFunctionTool
LongRunningFunctionTool(poll_external_job)
```

---

### 机制6：Session Rewind & Resumability（会话回滚与恢复）

```python
from google.adk.runners import InMemoryRunner

runner = InMemoryRunner(agent=root_agent, app_name="my_app")

# 回滚到指定调用之前的状态
await runner.rewind_async(
    user_id=user_id,
    session_id=session.id,
    rewind_before_invocation_id=invocation_id,  # 排他：恢复到这次调用之前
)
```

**价值：** 用户说"刚才那步错了，重来"——不需要重新启动整个对话，精确回滚到某次工具调用/Agent 调用之前。

---

### 机制7：Ambient Agent（事件驱动/后台 Agent）

传统 Agent 是"请求-响应"式的。ADK 支持**事件驱动的后台 Agent**：

```python
from google.adk.cli.fast_api import get_fast_api_app

app = get_fast_api_app(
    agents_dir=AGENTS_DIR,
    web=False,
    trigger_sources=["pubsub", "eventarc"],  # 启用 /apps/{app}/trigger/pubsub
)
```

**特性：**
- 通过 Pub/Sub 或 Eventarc 触发，无需 HTTP 请求
- 支持 **Cloud Scheduler cron 调度**（实现"每晚 8 点运行"）
- 自动处理 base64 解码、CloudEvent 解析、会话创建、并发信号量、指数退避重试
- 环境变量控制：`ADK_TRIGGER_MAX_CONCURRENT=10`, `ADK_TRIGGER_MAX_RETRIES=3`
- 输出通过结构化日志（JSON stdout → Cloud Logging）或工具集成（email/Slack/Jira）

---

### 机制8：A2A Protocol & A2UI（Agent 间协议与声明式 UI）

**A2A（Agent-to-Agent）协议**是 Google 提出的跨框架 Agent 通信标准，内置于 ADK：

```python
# 暴露为 A2A 服务
from google.adk.a2a.utils.agent_to_a2a import to_a2a
to_a2a(root_agent, port=8001)

# 消费远程 A2A Agent
from google.adk.agents.remote_a2a_agent import RemoteA2aAgent
remote = RemoteA2aAgent(
    name="remote_agent",
    agent_card="http://remote-host:8001/.well-known/agent.json",
)
```

**A2UI** 扩展：Agent 返回的不是纯文本，而是声明式 UI（卡片、表单、图表），客户端渲染。

---

### 机制9：Context Caching & Compaction（上下文缓存与压缩）

```python
from google.adk.apps import App
from google.adk.apps.app import EventsCompactionConfig
from google.adk.apps.llm_event_summarizer import LlmEventSummarizer

app = App(
    name="my_app",
    root_agent=root_agent,
    # 上下文缓存
    context_cache_config=ContextCacheConfig(
        min_tokens=2048,     # 仅当上下文超过此阈值才缓存
        ttl_seconds=1800,    # 缓存 30 分钟
        cache_intervals=10,  # 每 10 次调用重新缓存
    ),
    # 事件压缩（防止长会话溢出上下文窗口）
    events_compaction_config=EventsCompactionConfig(
        compaction_interval=20,   # 每 20 个事件压缩一次
        overlap_size=3,          # 保留最近 3 个事件保持连续性
        summarizer=LlmEventSummarizer(llm=Gemini(model="gemini-flash-latest")),
    ),
)
```

---

### 机制10：Agent Identity（Agent 独立 IAM 身份）

部署时可为每个 Agent 分配独立的 GCP 服务身份：

```python
# deploy --agent-identity
client.agent_engines.create(
    config={
        "identity_type": IdentityType.AGENT_IDENTITY,
        "display_name": display_name,
    }
)
# 然后绑定 IAM 角色：aiplatform.user, logging.logWriter, monitoring.metricWriter 等
```

**价值：** Agent 的权限与普通服务账户隔离，eval 规则中的"禁止破坏性操作"有**运行时 IAM 强制保障**——Agent  literally 无法执行未被授权的操作。

---

## 4. 对 Mora 语言的借鉴建议

### 建议1：引入 `eval` 原语 —— 评估优先的行为门控

**机制：** 将 `eval` 提升为一等语言原语，不是外部测试框架，而是 Agent 部署前的强制检查点。

**Mora 语法草案：**
```mora
// 定义评估数据集（YAML/JSON 内嵌）
eval dataset "incident-response" {
  case {
    input: "Database latency spike in us-east1"
    expect: {
      citation: contains("runbook-section-4.2"),
      destructive: false,
      root_cause_match: ~80%  // 近似匹配阈值
    }
  }
  case { ... }
}

// 评估执行（生成 trace + 评分）
eval run "incident-response" on agent my_agent {
  metrics: [citation_check, safety_guard, quality_judge]
  threshold: 0.85
}

// 评估作为部署门控
deploy my_agent to cloud {
  gate: eval "incident-response" >= 0.85
}
```

---

### 建议2：引入 `workflow` 原语 —— 显式图编排

**机制：** Mora 的 `orchestrate` 是动态/LLM 驱动的。建议增加**确定性图编排**原语，用于需要严格控制流的场景（如审批链、数据处理 pipeline）。

**Mora 语法草案：**
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

// 并行 fan-out
workflow parallel_search {
  node search_a: tool web_search
  node search_b: tool internal_kb_search
  node merge: llm { instruction: "Synthesize results from {search_a} and {search_b}" }
  
  edge START -> (search_a, search_b)
  edge (search_a, search_b) -> merge via join
}
```

---

### 建议3：引入 `state` 作用域前缀 —— 分层状态管理

**机制：** Mora 当前 `value` 和上下文管理可以借鉴 ADK 的 state prefix 设计，避免用户手动区分"session 变量"、"用户记忆"、"全局配置"。

**Mora 语法草案：**
```mora
// 默认 = session 作用域（当前对话）
let booking_step = 2

// 用户持久作用域（跨会话）
let user:preferred_language = "zh-CN"

// 应用全局作用域
let app:total_queries += 1

// 临时作用域（当前调用，不写入历史）
let temp:scratch = compute_intermediate()

// 在 ai.chat 中自然注入
with ai.chat {
  instruction: "User language: {user:preferred_language}"
}
```

---

### 建议4：引入 `confirm` / `gate` 原语 —— 工具执行门控

**机制：** Mora 已有 `sandbox` 和 `capability`，但缺少**用户/审批者介入**的显式门控。

**Mora 语法草案：**
```mora
// 声明工具需要确认
capability delete_database {
  require_confirmation: true
  // 或条件确认
  require_confirmation: (amount > 1000) when args.amount > 1000
}

// 在流程中显式请求人工输入
gate approve_transfer {
  prompt: "Approve transfer of ${amount} to {recipient}?"
  timeout: 300s
  on_timeout: reject
}

// 长运行工具（异步等待）
async tool poll_job_status(job_id: string) -> JobStatus {
  // 自动变为 LongRunningFunctionTool 语义
}
```

---

### 建议5：引入 `rewind` 原语 —— 精确会话回滚

**机制：** Mora 已有 `record/replay`，但 `replay` 通常是"从头播放"。`rewind` 是"精确回退到某次调用之前"，对交互式 Agent 更实用。

**Mora 语法草案：**
```mora
// 记录调用 ID（自动或显式）
with ai.chat {
  invoke: query_weather("Tokyo")
  tag: #weather_call
}

// 回滚到指定标签之前（保留更早的状态）
rewind before #weather_call

// 或回滚到指定时间点
rewind to 2026-07-01T10:00:00Z
```

---

### 建议6：引入 `ambient` / `trigger` 原语 —— 事件驱动后台 Agent

**机制：** Mora 当前面向交互式脚本，但缺少**无人值守的事件驱动**模式。

**Mora 语法草案：**
```mora
// 定义事件触发器
ambient monitor_incidents {
  trigger: cron("0 */5 * * * *")   // 每 5 分钟
  // 或 trigger: webhook("/hooks/incident")
  // 或 trigger: pubsub("projects/P/topics/incidents")
  
  agent: incident_agent
  
  max_concurrent: 4
  retry: 3 with backoff
  
  output: log structured  // 或 slack_notify, email_alert
}
```

---

### 建议7：引入 `a2a` 原语 —— 跨 Agent 通信协议

**机制：** Mora 的 `orchestrate` 是内部子 Agent 调度。建议增加**跨进程/跨网络**的 Agent 间通信原语。

**Mora 语法草案：**
```mora
// 声明远程 Agent 接口
a2a remote_security_agent {
  card_url: "https://security.internal/.well-known/agent.json"
  capabilities: [scan_vulnerability, generate_report]
}

// 在编排中调用远程 Agent
with orchestrate {
  local: triage_agent
  remote: remote_security_agent.analyze(input: suspicious_payload)
  merge: synthesis_agent
}
```

---

### 风险/不适用项

| 建议 | 风险 | 备注 |
|------|------|------|
| `eval` 原语 | 需要 LLM-as-judge 基础设施，非本地可完成 | 可设计为可插拔评估后端（本地启发式 / 远程 Vertex） |
| `workflow` 原语 | 与现有 `orchestrate` 语义重叠 | 应明确区分：`orchestrate` = LLM 动态调度，`workflow` = 确定性图 |
| `state` 前缀 | 增加语言复杂度 | 可作为可选的"高级模式"，默认扁平字典兼容 |
| `ambient` | 需要 cron/事件基础设施 | 初期可仅生成配置（如 Kubernetes CronJob），不实现运行时 |
| `a2a` | 协议尚在演进（A2A v0.9.1） | 建议作为实验性原语，不承诺稳定 ABI |
| Agent Identity | 强绑定 GCP IAM | 其他云平台需要适配层；可作为部署目标插件 |
| Context Caching | 依赖底层模型支持（Gemini 特有） | 不应作为通用语言特性，而是 model 配置参数 |

---

## 5. 与已有 17 项目的差异化

### 5.1 vs. LangChain / LangGraph

| 维度 | LangChain/LangGraph | agents-cli / ADK |
|------|---------------------|------------------|
| **定位** | Agent 框架（Python/JS 库） | Agent 的 DevOps 工具链（CLI + Skill） |
| **编排** | LangGraph 的 checkpoint/状态图 | ADK Workflow 的显式边 + 条件路由 + JoinNode |
| **评估** | LangSmith（可观测性 + 追踪） | 内置 `eval` 命令 + Vertex AI LLM-as-judge + 部署门控 |
| **部署** | 无原生部署，需自行容器化 | `deploy` 一键到 Agent Runtime / Cloud Run / GKE |
| **Skill 系统** | 无 | **Markdown Skill 包**——编码 Agent 的知识库 |
| **生命周期** | 库函数调用 | **8 阶段闭环**（Spec → Scaffold → Build → Orchestrate → Eval → Deploy → Publish → Observe） |
| **跨 Agent 通信** | 无标准协议 | **A2A 协议内置** + Agent Card 发现机制 |

### 5.2 vs. AutoGen / OpenAI Agents SDK

| 维度 | AutoGen / OpenAI SDK | agents-cli / ADK |
|------|----------------------|------------------|
| **Agent 定义** | 代码中实例化 | 代码 + 脚手架 + 模板配置 |
| **多 Agent 模式** | GroupChat / Handoff | SequentialAgent / ParallelAgent / LoopAgent / Workflow Graph |
| **人机交互** | `user_proxy` 代理 | `request_input` 工具 + `ResumabilityConfig` + 确认门控 |
| **评估** | 无内置评估框架 | **Eval-first 方法论**（generate → grade → compare → analyze → optimize） |
| **部署** | 无 | 原生支持 Cloud Run / GKE / Agent Runtime |
| **企业特性** | 弱 | **Agent Identity IAM**、BigQuery 分析、Cloud Trace、IAP、WIF |

### 5.3 vs. Kimi-CLI（当前运行环境）

| 维度 | Kimi-CLI | agents-cli / ADK |
|------|----------|------------------|
| **定位** | 通用 AI 助手 CLI（Orchestrator + 子 Agent） | 专门构建/部署 Google Cloud Agent 的工具链 |
| **Agent 类型** | 交互式对话 Agent | 交互式 + 事件驱动（Ambient） |
| **子 Agent** | `Agent` 工具创建子 Agent | ADK `sub_agents` + `AgentTool` + A2A 远程 Agent |
| **Skill 系统** | `SKILL.md` 文件（本系统） | **类似的 Markdown Skill 系统**（但分发给外部 IDE） |
| **编排** | `plan` + `coder` 子 Agent 类型 | `Workflow` 图 + `SequentialAgent` + `ParallelAgent` |
| **评估** | 无内置 | **Quality Flywheel**（eval 闭环） |
| **部署** | 本地运行 | 云原生部署（Agent Runtime 等） |
| **状态** | `Context` 压缩 + 会话持久化 | `Session` + `State` 前缀 + `Memory Bank` + 上下文缓存/压缩 |

### 5.4 agents-cli 的独特之处（总结）

1. **"元 Agent"定位**：它不是让你手写 Agent，而是**让你的编码 Agent 更擅长写 Agent**——通过 Skill 注入领域知识。
2. **评估即工程**：`eval` 不是附属品，是**与 build/deploy 并列的一等 CLI 命令**，有完整的 fix→iterate 方法论。
3. **8 阶段生命周期**：从 Spec 到 Observe 的完整闭环，强制"不写 spec 不 scaffold"、"不 eval 不 deploy"。
4. **生产级安全**：Agent Identity IAM、工具确认门控、IAP、WIF——Agent 不是"有权限"，而是**权限被精确控制**。
5. **云原生集成**：与 Vertex AI、Cloud Run、Pub/Sub、Eventarc、Cloud Scheduler 深度集成，不是"部署容器"而是"部署 Agent 运行时"。
6. **A2A 协议**：Google 主导的跨框架 Agent 通信标准，内置支持，降低生态锁定。

---

## 6. 结论

`google/agents-cli` 是一个**工程成熟度极高**的 Agent 工具链项目。其核心创新不在于某个算法突破，而在于**将 Agent 开发从"提示工程"提升为"软件工程"**：

- **Spec 驱动**：`.agents-cli-spec.md` 是 Single Source of Truth
- **模板化脚手架**：~72 文件的标准项目结构，一键生成
- **评估门控**：eval 分数是部署的唯一准入条件
- **确定性编排**：Workflow 图补足了 LLM 动态调度的不可控性
- **全生命周期**：从想法到生产观测的 8 阶段闭环

对 **Mora 语言**而言，最有价值的借鉴是：
1. **将 `eval` 提升为语言原语**（评估数据集定义 + 评分 + 门控）
2. **引入 `workflow` 作为确定性编排层**（与现有 `orchestrate` 形成动态/静态双轨）
3. **State 前缀作用域**（session / user / app / temp）
4. **工具确认门控**（`confirm` / `gate` 原语）
5. **会话 rewind**（精确回滚，不是全量 replay）
6. **Ambient Agent**（事件驱动、无人值守模式）

这些机制不是"功能叠加"，而是**弥补当前 Mora 从"脚本语言"到"生产级 Agent 语言"的关键缺口**。

---

> **数据来源**：本报告基于 `google/agents-cli` GitHub 仓库的公开源码、Skill 文档、ADK 参考手册和 Release Notes 进行深度分析。关键文件路径已在文中标注。
