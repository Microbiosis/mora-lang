# Mora-lang 原语灵感总纲 — 全量研究文档

> **研究跨度**: v0.34-v0.40 技术债清理 + 17 个开源项目深度解析  
> **文档定位**: 单一权威参考 — mora-lang v0.41+ 新功能路线图  
> **日期**: 2026-07-04

---

## 0. 执行摘要

### 研究范围

| 轮次 | 项目数 | 项目名 |
|---|---|---|
| 零信任审计 | 1 (自检) | mora-lang v0.34 自身 |
| 第 1 轮 | 1 | loongclaw (loong) |
| 第 2 轮 | 2 | mini-swe-agent, CLI-Anything |
| 第 3 轮 | 7 | AIOS, mimiclaw, OpenFugu, OpenInfer, MinerU, Headroom, Puter |
| 第 4 轮 | 4 | multi-agent-revenue-orchestrator, pi-agent/pi-mono, ai-coder-symphony, AgentMesh (MinimalFuture) |
| 第 5 轮 | 3 | vesh-agents, AgentMesh Go (hupe1980), AgentMesh (Solace) |
| **总计** | **17** | (含自检) |

### 技术债清理 (v0.34-v0.40)

| 版本 | P0 | P1 | P2 | Permanent | CI |
|---|---|---|---|---|---|
| v0.34 | 审计完成 | — | — | — | — |
| v0.35 | 20 | 0 | 0 | 0 | 0 |
| v0.36 | 0 | 12 | 2 | 2 (Channel, Type 8 variants) | 1 |
| v0.37 | 0 | 7 | 2 | 0 | 0 |
| v0.38 | 0 | 0 | 0 | 1 (numeric tower) | 0 |
| v0.39 | 0 | 0 | 0 | 0 (rename only) | 0 |
| v0.40 | 0 | 0 | 0 | 1 (env immutable snapshot) | 0 |
| **总计** | **20** | **19** | **4** | **4/5** (跨线程 env 简化完成) | **1** |

### 全局诊断

mora-lang v0.32-0.34 对 7 个灵感项目的集成采用了一致模式:
- ✅ 采纳了**命名和 API 形状** (函数名, 参数名, 返回类型)
- ❌ 简化了**算法核心** (线性扫描替代递归/ML/索引)
- ❌ 丢弃了**多层框架模式** (路由管道, 依赖注入, 布局模型链)

---

## 1. 项目级深度解析

### 1.1 loongclaw (loong) — 基础设施层

**仓库**: https://github.com/eastreams/loong (644 ⭐)  
**定位**: Rust 基础的垂直 AI 代理底座. 13-crate 严格无环 DAG, L0-L9 分层执行模型.

#### 核心原语提取

| 原语 | 精确位置 | mora-lang 映射 |
|---|---|---|
| **Capability 枚举** (13 variants) | `crates/contracts/src/contracts.rs:24-37` | `sandbox.key { file.read, web.fetch }` |
| **CapabilityToken** (token_id + allowed + expires_at + generation) | `crates/contracts/src/contracts.rs:44-52` | `Value::CapKey` |
| **PolicyEngine trait** (issue/authorize/revoke) | `crates/kernel/src/kernel.rs:42-58` | `sandbox.check_call(req)` |
| **PolicyExtensionChain** (Chain of Responsibility, 只收紧不放大) | `crates/kernel/src/policy_ext.rs:34-45` | Policy plugin system |
| **AuditSink** trait + SHA-256 哈希链 JSONL | `crates/kernel/src/audit.rs:34-204` | `audit.jsonl` file |
| **Fault** enum (Panic/CapViolation/TokenExpired/...) | `crates/contracts/src/task_state.rs:34-48` | `Fault` 替代 `String` 错误 |
| **TaskState** FSM (5 states, typed transitions) | `crates/contracts/src/task_state.rs:52-74` | `FlowSignal` 扩展 |
| **Core/Extension Adapter** (每平面 BTreeMap dispatch) | `crates/kernel/src/tool.rs:25-67` | `ToolPlane` 替代 `tool_registry` |
| **Provider→Channel→Connector** hierarchy | `crates/kernel/src/integration.rs` | I/O abstraction |
| **WorkUnitRecord** (12-state lifecycle + retry + blocking) | `crates/contracts/src/workflow_types.rs` | Task queue |
| **Plugin pipeline** (scan→translate→plan→bootstrap) | `crates/kernel/src/plugin.rs` | Package lifecycle |

#### 被采纳 vs 被遗漏

| 被采纳 (v0.41 候选) | 被遗漏 |
|---|---|
| Capability token 系统 | WASM 沙箱 (wasmtime) |
| AuditSink + hash chain | ed25519 签名验证 |
| Fault 类型化错误 | 13-crate DAG (mora 是单 crate) |
| 策略引擎 | 插件扫描/翻译管道 |

---

### 1.2 mini-swe-agent — 代理层

**仓库**: https://github.com/SWE-agent/mini-swe-agent (5.6k ⭐)  
**定位**: 100 行 Python 代理类, 只用 bash, 无工具调用界面. SWE-bench verified >74%.

#### 核心原语提取

| 原语 | 精确位置 | mora-lang 映射 |
|---|---|---|
| **Exception-as-flow**: `InterruptAgentFlow` 根异常 → 5 个子类 | `src/minisweagent/exceptions.py` | `FlowSignal` 扩展 + 类型化终止 |
| **线性消息历史**: 仅追加 `list[dict]`, `role: exit` 终止 | `src/minisweagent/agents/default.py:97-119` | `TraceCollector` v2 |
| **子进程隔离**: `Popen(shell=True, start_new_session=True)` + `os.killpg` | `src/minisweagent/environments/local.py:62-73` | `exec(cmd, timeout)` builtin |
| **COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT** sentinel | `src/minisweagent/environments/local.py:40-51` | Task completion protocol |
| **tenacity 重试**: 10 attempts, exp backoff 4s→60s, abort_exceptions | `src/minisweagent/models/utils/retry.py` | `ai.retry { attempts: 10 }` block |
| **BASH_TOOL**: 单一函数工具, `{"name":"bash", "command": string}` | `src/minisweagent/models/utils/actions_toolcall.py` | `exec.bash(cmd)` |

#### 被采纳 vs 被遗漏

| 被采纳 (v0.41 候选) | 被遗漏 |
|---|---|
| Exception-as-flow 控制模型 | Jinja2 模板渲染消息 |
| 子进程隔离 + 进程组清除 | litellm 成本计算器 |
| COMPLETE_TASK sentinel | 全局模型统计 (成本/调用限制) |
| 线性追加仅消息历史 | 轨迹浏览器 (trajectory browser) |

---

### 1.3 CLI-Anything — 工具生态层

**仓库**: https://github.com/HKUDS/CLI-Anything (44.7k ⭐)  
**定位**: 任何软件变代理原生 CLI — 7 阶段自动生成管道, 100+ 软件支持, 9+ 代理平台.

#### 核心原语提取

| 原语 | 精确位置 | mora-lang 映射 |
|---|---|---|
| **三层注册表**: `matrix_registry.json` (意图→能力→提供者) + `registry.json` (内部) + `public_registry.json` (外部) | 仓库根目录 | `mora-hub.json` + `mora-public.json` |
| **HARNESS.md**: 7 阶段声明式方法论文档, 代理消费 | `cli-anything-plugin/HARNESS.md` | `skill.md` 格式 |
| **SKILL.md 格式**: YAML 前置元数据 + "For AI Agents" 部分 | `skills/cli-anything-gimp/SKILL.md` | `mora-skill-{name}/SKILL.md` |
| **命名约定**: `cli-anything-{name}` 在 5+ 层强制执行 | 整个仓库 | `mora-skill-{name}` |
| **编辑/优化循环**: 差距分析 → 用户批准 → 增量变更 → 回归测试 | `/cli-anything:refine` | `mora refine script.mora "add X"` |
| **提供者分类**: 9 个 `kind` 值 (harness-cli, public-cli, python, native, api, ...) | `matrix_registry.json` | `ToolKind` enum |
| **不可变快照 + 审计轨迹**: bundle (不可变) + session (可变指针) + trajectory (仅追加日志) | `preview_bundle.py` | `recorder` v2 |

#### 被采纳 vs 被遗漏

| 被采纳 (v0.41 候选) | 被遗漏 |
|---|---|
| 双注册表模式 | 预览协议 (bundle+trajectory) |
| SKILL.md YAML 格式 | PEP 420 命名空间包 (Python-specific) |
| 提供者分类 | CLI-Hub 包管理器 |

---

### 1.4 AIOS — 代理调度器

**仓库**: https://github.com/agiresearch/AIOS  
**定位**: Python LLM 代理操作系统 — FIFO/RR 调度器, 4 线程每资源类型, 工具冲突映射.

#### 核心发现

| 声明 | 验证结果 |
|---|---|
| "中央调度器 FIFO/RR" | ✅ 确认 — `FIFOScheduler` (batch_interval=1s) + `RRScheduler` (time_slice=1s), 4 线程 |
| "Tool Manager hashmap 冲突锁" | ✅ 确认 — `tool_conflict_map` + `threading.Lock`, 冲突时**静默丢弃** |
| "Context snapshot (text/logits)" | ⚠️ 部分确认 — 仅 LLM 生成状态 (past_key_values), 非完整代理快照 |
| Agent lifecycle | ❌ 未发现 — 无显式终止协议, 仅 `self.active = False` |

#### mora-lang 补丁

| 补丁 | 优先级 | LOC |
|---|---|---|
| `tool_conflict_map` per-tool 并发控制 | P1 | ~40 |
| `ContextSnapshot` 序列化 (past_key_values 用于 HF 模型) | P2 | ~60 |

---

### 1.5 mimiclaw — 嵌入式 ReAct + Cron

**仓库**: https://github.com/memovai/mimiclaw  
**定位**: ESP32-S3 FreeRTOS C 代理 — 12 字段 cron, 心跳, 工具/skill 区分, GPIO 权限.

#### 核心发现

| 声明 | 验证结果 |
|---|---|
| "cron (9 字段 job)" | ❌ **12 字段** — `id, name, enabled, kind, interval_s, at_epoch, message, channel, chat_id, last_run, next_run, delete_after_run` |
| "heartbeat" | ✅ FreeRTOS auto-reload timer, 30min interval, reads HEARTBEAT.md |
| "tool/skill 区分" | ✅ 工具 = 注册 C 函数指针; 技能 = SPIFFS markdown 文档 |
| "path `..` 拒绝" | ✅ `strstr(path, "..")` + 前缀强制 |

#### mora-lang 补丁

| 补丁 | 优先级 | LOC |
|---|---|---|
| Job 结构加 `channel`/`chat_id`/`delete_after_run` 字段 | P1 | ~20 |
| `heartbeat.md` 可执行检查列表 | P2 | ~50 |
| 工具 vs 技能两层设计 | P2 | ~100 |

---

### 1.6 OpenFugu — 策略覆盖模型

**仓库**: https://github.com/trotsky1997/OpenFugu  
**定位**: 19.5K 参数 TRINITY 路由器 — 冻结 Qwen3-0.6B 隐藏状态上的线性分类器.

#### 核心发现

| 原语 | 精确位置 | 精度 |
|---|---|---|
| **TRINITY 路由器**: `VEC_LEN = 19456` (9216 SVF offsets + 10240 router head) | `openfugu/mini.py` | 19.5K 参数 |
| **per-turn 角色**: Worker(0)/Thinker(1)/Verifier(2), 5 turns max | `openfugu/mini.py` | Thinker 可覆盖路由器的下一个角色选择 |
| **DAG-as-data**: `model_id[]`, `subtasks[]`, `access_list[]` 三个等长列表 | `openfugu/ultra.py` | 声明式执行图 |
| **sep-CMA-ES**: diagonal CMA, λ=33, μ=16, 60 iterations | `train/train_trinity.py` | 仅标量 σ 显著移动 |
| **MockWorld**: per-domain 伯努利奖励矩阵 | `train/train_adaptive_pool.py` | 不是单一定罐响应 |

#### mora-lang 补丁

| 补丁 | 优先级 | LOC |
|---|---|---|
| DAG-as-data 声明式执行图 → `orchestrate` 原语 | P1 | ~80 |
| per-turn 角色 → `ai.role { worker / thinker / verifier }` | P2 | ~60 |
| MockWorld 增强 → `mock` 模块 per-domain 矩阵 | P2 | ~40 |

---

### 1.7 OpenInfer — 推理引擎

**仓库**: https://github.com/openinfer-project/openinfer  
**定位**: Rust/CUDA 推理引擎 — 缝合 vLLM 前端 + 本地引擎后端.

#### 核心发现

| 原语 | 精确位置 |
|---|---|
| **缝合架构**: vLLM HTTP frontend + native engine via Unix-domain socket ZMQ bridge | `openinfer-vllm-frontend/src/bridge.rs` |
| **两层 KV 缓存**: GPU `KvBuffer` + host DRAM via pegaflow | `openinfer-kv-offload/src/engine.rs` |
| **特性门控**: `#[cfg(feature = "qwen3")]` ModelType 编译时选择 | `openinfer-server/src/server_engine.rs` |
| **CUDA 图解码**: 批量 decode 路径消除内核启动开销 | `openinfer-qwen3/src/scheduler.rs` |
| **P2P RDMA 分解**: MetaServer gRPC + one-sided RDMA READ | `openinfer-kv-offload/src/engine.rs` |

#### mora-lang 补丁

| 补丁 | 优先级 | LOC |
|---|---|---|
| 特性门控模式 → `ai_infra.rs` 多后端编译选择 | P2 | ~30 |

---

### 1.8 MinerU — 文档解析

**仓库**: https://github.com/opendatalab/MinerU  
**定位**: 文档布局分析 — 3 种阅读顺序策略, 30+ BlockType, 图文配对.

#### mora-lang 差距 (v0.33 `reading_order` 113 行)

| 当前实现 | MinerU 实际 | 差距 |
|---|---|---|
| `GapTree`: `center_y → center_x` 排序 | **真正递归 XY-cut**: 投影轮廓分裂递归树 | 不是 gap-tree — 对不重叠块等同于 TopToBottom |
| `GroupBased`: `center_x → y` 排序 | `find_best_visual_parent()` 几何邻近匹配图文 | 无父子匹配 |
| `XyCut`: 平面排序 | `recursive_xy_cut()` 递归投影-轮廓分裂 | 不是递归的 |
| 无 ML | LayoutLM-based layoutreader (≤200 行) | 完全缺失 |

#### v0.41 补丁

| 补丁 | 优先级 | LOC |
|---|---|---|
| 递归 XY-cut 实现 | **P0** | ~50 |
| `find_best_visual_parent()` 几何匹配 | P2 | ~40 |

---

### 1.9 Headroom — 内容感知压缩

**仓库**: https://github.com/headroomlabs-ai/headroom  
**定位**: ContentRouter + SmartCrusher + CCR — Rust native detection, 5-dim scoring.

#### mora-lang 差距 (v0.33 `ccr` 165 行)

| 当前实现 | Headroom 实际 | 差距 |
|---|---|---|
| 顺序 u64 计数器 → 16-char hex | **SHA-256** 内容寻址 → 24-char hex | 哈希标识符与内容无关 |
| `InMemoryCcrStore` (HashMap) | **SQLite + WAL + TTL** (1800s expiry) | 无持久化 |
| `extract_hash`: 简单字符串分割 | ContentRouter: 11 种压缩策略 | 无路由 |
| 无缓存 | **两层缓存**: skip set + result cache | 无缓存 |
| 无检索工具注入 | LLM tool set auto-inject `headroom_retrieve` | 无注入 |

#### v0.41 补丁

| 补丁 | 优先级 | LOC |
|---|---|---|
| SHA-256 内容寻址替代顺序计数器 | **P1** | ~30 |
| SQLite-backed CcrStore (备选) | P2 | ~80 |

---

### 1.10 Puter — Web OS + 事件总线

**仓库**: https://github.com/HeyPuter/puter  
**定位**: TypeScript web OS — EventClient wildcard, 5 层 DI 容器, Service Extension.

#### mora-lang 差距 (v0.32 `event` 110 行 + v0.33 `sandbox` 209 行)

| 当前实现 | Puter 实际 | 差距 |
|---|---|---|
| `emit`: 线性扫描所有模式 O(patterns) | **O(segments)**: 遍历每个前缀, 从字面 `*` 键的 map 查找 | 性能漏洞 |
| fire-and-forget only | `emitAndWait` 顺序异步调度 | 无顺序调度 |
| `allow: Vec<String>` + `deny: Vec<String>` | iframe 隔离沙箱 | 仅路径+绑定验证 |
| 注释中提到 `thread_local!` | 未实现 | 占位 |

#### v0.41 补丁

| 补丁 | 优先级 | LOC |
|---|---|---|
| O(segments) 索引匹配取代线性扫描 | **P0** | ~30 |
| `thread_local!` 沙箱上下文实现 | P2 | ~40 |

---

### 1.11 pi-agent / pi-mono — 代理运行时

**仓库**: https://github.com/badlogic/pi-mono (TypeScript original) + https://github.com/Ashutosh0428/pi-agent (Python fork)  
**定位**: 全源码 monorepo — 双消息队列, 默认并行工具执行, 自检, 持久记忆, 数据泄漏防护.

| 原语 | 精确位置 | mora-lang 映射 |
|---|---|---|
| **双消息队列**: steering (注入运行中) + follow-up (注入结束后) | `packages/agent/src/agent.ts` | `bus.steer(task_id, msg)` / `bus.followup(task_id, msg)` |
| **默认并行工具执行**: `toolExecution: "parallel"` via `Promise.all` | `packages/agent/src/agent-loop.ts` | `exec.parallel([cmd1, cmd2])` |
| **递归防护**: `registry.without("delegate")` — 深度上限=1 | `src/pi_agent/agent.py` | `agent.task { ... }` 自动移除自身 |
| **自检**: `--reflect` — 一个额外受限审查轮次 | `src/pi_agent/agent.py:_reflection_pass` | `ai.reflect { max_turns: 5 }` |
| **持久记忆**: 追加日期子弹点到 `.pi/memory.md` | `src/pi_agent/tools/memory.py` | `memory.remember(fact)` / `memory.recall()` |
| **历史截断**: 在用户消息边界快照 | `src/pi_agent/agent.py:_history_for_request` | `context.trim(threshold)` |
| **数据泄漏防护**: 每个工具输出扫描密钥 | `src/pi_agent/agent.py:_dispatch` | `sandbox.guard { exfil: true }` |
| **提供者中立工具模式**: 单一 `to_schema()` → 每提供者翻译 | `src/pi_agent/tools/base.py` | `Tool` trait with `to_schema()` |
| **实时计划清单**: `update_plan` 工具 ⬜→⏳→✅ 状态 | `src/pi_agent/tools/planning.py` | `plan.update([{step, status}])` |

---

### 1.12 AgentMesh — 团队编排

**仓库**: https://github.com/zhayujie/AgentMesh  
**定位**: Python 多代理顺序编排 — LLM 作为路由器, 不是真正的 mesh 网络.

| 原语 | 精确位置 | mora-lang 映射 |
|---|---|---|
| **类型化 WebSocket 事件协议**: 7 个判别事件类型 | `agentmesh/common/models.py:55-115` | `bus.emit("agent_decision", typed_payload)` |
| **任务范围 pub-sub**: `subscribe_to_task` / `broadcast_to_task` | `agentmesh/api/websocket_manager.py` | `bus.subscribe(topic)` / `bus.publish(topic, msg)` |
| **共享输出上下文**: `TeamContext.agent_outputs` 累积 | `agentmesh/protocol/context.py` | `context.outputs` |
| **混合内存搜索**: 向量 + 关键词合并, 可配置权重 | `agentmesh/memory/manager.py` | `memory.search(query, mode: "hybrid")` |
| **工具阶段**: `PRE_PROCESS` vs `POST_PROCESS` | `agentmesh/tools/tool_manager.py` | `tool.register(name, fn, stage: "pre" | "post")` |
| **惰性工具发现**: 文件系统扫描 + `__all__` exports | `agentmesh/tools/tool_manager.py` | Plugin registry pattern |
| **上下文感知截断**: 令牌估算 + 最旧消息驱逐 | `agentmesh/protocol/agent.py:140-195` | `context.trim(threshold)` |

---

### 1.13 multi-agent-revenue-orchestrator — 架构蓝图

**仓库**: https://github.com/aadiieee/multi-agent-revenue-orchestrator (⚠️ 空仓)  
**定位**: README-only, 但架构蓝图内部一致.

| 原语 | 描述 | mora-lang 映射 |
|---|---|---|
| **共享上下文总线**: Redis pub/sub 黑板 | `orchestrate` context |
| **谓词路由**: `handoff_criteria: "meeting_booked OR high_intent_forecast"` | `orchestrate { on: expression }` |
| **YAML 配置文件**: per-profile agent pipeline | `agent { profile: "emea_midmarket" }` |
| **阶段门控 + 超时升级**: `escalation_threshold_days: 14` | `stage { timeout: 14d, escalate: ... }` |
| **验证器代理**: Omni Agent 跨渠道质量门 | `agent X { role: validator }` |
| **选择性激活**: `--agents research,personalization` CLI flag | `orchestrate { agents: [A, B] }` |

---

### 1.14 ai-coder-symphony — 空仓

**仓库**: https://github.com/novanandin9-netizen/ai-coder-symphony (⚠️ 空仓)  
**定位**: README-only, XOR 混淆下载页. 无源代码.

唯一可取概念: **静态角色分配** (每个代理固定角色: math_whisperer, code_forger, ui_sculptor, documentation agent) 和 **加权投票共识** (`consensus_method: "weighted_voting"`).

---

### 1.15 vesh-agents — 收入智能管线

**来源**: PyPI `vesh-agents` v0.1.1 (GitHub 仓库 404, PyPI 文档可用)  
**定位**: 基于 OpenAI Agents SDK 的 SaaS 收入智能框架. 6 个专门代理管道.

#### 核心发现

**管线架构** (硬编码 5 阶段管道):
```
DataConnector → EntityResolver → MetricComputer → AnomalyDetector → InsightReasoner
    ↑                ↑               ↑               ↑              ↑
 CSV/Stripe/     Blocking/       MRR/Churn/      Z-score/        BYOM LLM
 Postgres         Scoring         ARPU/NRR        Rate-of-change  Explanation
```

**6 个代理**:
| 代理 | 角色 | MCP 工具 |
|---|---|---|
| DataConnector | 从数据源提取数据 | `import_csv`, `extract_stripe`, `extract_postgres` |
| EntityResolver | 跨数据源匹配记录 | `resolve_entities` (阻塞+评分) |
| MetricComputer | 计算 SaaS 指标 (MRR, churn, ARPU, NRR, Quick Ratio) | `compute_metrics`, `list_metrics` |
| AnomalyDetector | 统计异常检测 (Z-score, 变化率) | `detect_anomalies` |
| InsightReasoner | 解释根因 (BYOM LLM) | `explain_anomaly` |
| Vesh Orchestrator | 协调管道 | `analyze_csv` (全管道) |

**关键模式**:
- **管线转交**: 编排器将任务委派给专家, 不是 LLM 路由——基于数据流的硬编码管道
- **无 LLM 快速路径**: `vesh analyze csv file.csv` 无需 LLM (指标计算是确定性的)
- **BYOM**: 支持 `litellm/anthropic/claude-sonnet-4`, `openai/gpt-4o`, 等
- **CLI + MCP 服务器**: 可从终端使用或集成到 Cursor/OpenCode/Claude Desktop
- **实体解析**: 跨 Stripe/Postgres/CSV 的阻塞评分

**mora-lang 映射**:
| 原语 | mora-lang 位置 |
|---|---|
| 无 LLM 确定性管线 | `orchestrate { pipeline: [A, B, C] }` with `llm: none` |
| 统计异常检测 | `data.anomaly(method: "zscore" | "rate_of_change")` |
| 实体解析 | `data.resolve(sources: [csv, stripe, postgres])` |
| MCP 服务器集成 | `mcp.serve(tools: [DataConnector, MetricComputer, ...])` |

---

### 1.16 AgentMesh Go (hupe1980) — Pregel BSP 图执行

**仓库**: https://github.com/hupe1980/agentmesh (6 ⭐, Go)  
**定位**: **生产级** 多代理编排框架, 由 Pregel 风格 BSP (批量同步并行) 图处理驱动.

> ⚠️ **注意**: 这是与 MinimalFuture/AgentMesh (Python, LLM 作为路由器) **不同的项目**. hupe1980/agentmesh 是 Go 编写, 生产级, 基于 Pregel BSP 的真正并行执行.

#### 核心发现

**Pregel BSP 执行模型**:
```
Superstep 0 → 所有节点并行处理传入消息 → 全局屏障 → Superstep 1 → ...
```
- 每个超步: 所有工作节点并行运行, 通过消息总线通信
- 全局屏障同步: 所有节点完成当前超步后才进入下一步
- 确定性执行: 相同输入 → 相同输出

**组件架构**:
```
应用层 (ReActAgent / SupervisorAgent / RAGAgent)
    ↓
图构建器 (验证拓扑, 流式 API)
    ↓
编译图 (不可变拓扑, Run() → events, 纯委托)
    ↓
接口层 (Structure / Executor / StateManager)
    ↓
执行器 (PregelExecutor BSP 超步, 并行工作节点, 消息总线)
       (SequentialExecutor 拓扑顺序, 单线程)
```

**关键特性**:
| 特性 | 实现 |
|---|---|
| **无锁状态管理** | 基于通道的状态, 带检查点 + 加密 + 签名 |
| **零拷贝恢复** | CoW 检查点复用已保存的 map, 仅在键变更时分配 |
| **WASM 沙箱** | 原生 WASM 沙箱, 集成完整性检查 |
| **OpenTelemetry** | 内置指标, 非阻塞事件总线扇出 |
| **A2A 协议** | 标准化多代理通信 |
| **MCP 支持** | 来自模型上下文协议服务器的动态工具发现 |
| **电路断路器 + 重试** | 可配置的重试策略 |
| **人性化回路** | 带条件守卫的审批工作流 |
| **Go 1.24+** | `iter.Seq2` 模式: `for msg, err := range graph.Run(ctx, input) { ... }` |

**mora-lang 映射**:
| 原语 | mora-lang 位置 |
|---|---|
| BSP 超步执行 | `orchestrate { steps: [step1, step2] }` with barrier sync |
| 无锁状态管理 | `context.atom(key)` — lock-free 通道 |
| 零拷贝检查点 | `checkpoint.save()` / `checkpoint.restore()` |
| WASM 沙箱 | Future `sandbox.wasm(code)` |
| 人性化回路 | `sandbox.approve { question: "..." }` — 条件守卫 |
| Go iter.Seq2 模式 | Rust `Iterator<Item = Result<Message, Error>>` |

---

### 1.17 Solace Agent Mesh — 事件驱动多代理

**仓库**: https://github.com/SolaceLabs/solace-agent-mesh  
**定位**: 事件驱动框架, 用于构建和编排多代理 AI 系统. 基于 Solace 消息总线.

**关键独有模式**:
- **主题路由**: 代理通过 `topic/subtopic/action` 层次命名空间通信 (不是点对点)
- **事件溯源**: 所有代理操作都是事件; 状态从事件日志重建
- **消息持久化**: 代理离线后重连可重放消息
- **动态代理发现**: 代理在启动时注册主题

**mora-lang 映射**: `bus.subscribe("agent.research.*")` — 通配符订阅, 类似 Solace 的主题路由.

---

## 2. 跨项目模式映射

### 2.1 重复模式 (3+ 项目)

| 模式 | 项目 | mora-lang 位置 |
|---|---|---|
| **策略引擎 + 能力令牌** | loongclaw, AIOS | `sandbox.key { ... }` |
| **审计日志 + 哈希链** | loongclaw, CLI-Anything (bundle trajectory) | `audit.jsonl` |
| **工具注册表 + 双注册表** | loongclaw, CLI-Anything, mimiclaw | `mora-hub.json` |
| **提供者分类 / ToolKind 枚举** | CLI-Anything (9 kinds), mimiclaw (tools vs skills), vesh-agents (pipeline agents) | `ToolKind` enum |
| **子进程隔离 + 进程组清除** | mini-swe-agent, pi-agent | `exec(cmd, timeout)` |
| **异常即流程** | mini-swe-agent | `FlowSignal` 扩展 |
| **持久记忆 (markdown)** | pi-agent, AgentMesh, mimiclaw | `memory.remember()` |
| **共享上下文 / 黑板** | revenue-orchestrator, AgentMesh, vesh-agents (pipeline context) | `context.outputs` |
| **编辑/优化循环** | CLI-Anything, pi-agent (--reflect) | `mora refine` |
| **管线转交 (非 LLM 路由)** | vesh-agents (硬编码管道), AgentMesh (LLM-based) | `orchestrate` |

### 2.2 独有模式 (1 项目)

| 模式 | 项目 | 描述 |
|---|---|---|
| **TRINITY 路由器 (19.5K params)** | OpenFugu | 冻结骨干上的线性分类器路由决策 |
| **双消息队列 (steering + follow-up)** | pi-mono | 外部中断不重启循环 |
| **DAG-as-data** | OpenFugu | 三个等长列表声明式执行图 |
| **递归 XY-cut** | MinerU | 递归投影轮廓分裂 |
| **SHA-256 内容寻址** | Headroom | 哈希与内容相关, 非顺序计数器 |
| **5 层 DI 容器** | Puter | config→clients→stores→services→controllers→drivers |
| **Pregel BSP 超步执行** | AgentMesh Go (hupe1980) | 全局屏障同步, 确定性执行 |
| **零拷贝 CoW 检查点恢复** | AgentMesh Go (hupe1980) | 10k+键检查点无 GC 峰值 |
| **WASM 沙箱** | AgentMesh Go (hupe1980), loongclaw | 带完整性检查的 WASM 隔离 |
| **无 LLM 确定性管线** | vesh-agents | 指标计算等无 LLM 快速路径 |
| **主题路由 (层次命名空间)** | Solace Agent Mesh | `topic/subtopic/action` 发布-订阅 |
| **事件溯源** | Solace Agent Mesh | 状态从事件日志重建 |

---

## 3. mora-lang v0.41+ 综合路线图

### 3.1 P0 — 修复现有模块的算法核心 (共 110 LOC)

| 补丁 | 灵感 | LOC |
|---|---|---|
| `event`: O(segments) 索引匹配替代线性扫描 | Puter | ~30 |
| `reading_order`: 递归 XY-cut 实现 | MinerU | ~50 |
| `ccr`: SHA-256 内容寻址替代顺序计数器 | Headroom | ~30 |

### 3.2 P1 — 新功能 (共 440 LOC)

| 补丁 | 灵感 | LOC |
|---|---|---|
| `sandbox.key { ... }` — Capability token system | loongclaw | ~200 |
| `audit.jsonl` — AuditSink + SHA-256 hash chain | loongclaw | ~200 |
| `exec.parallel([cmd1, cmd2])` — 并行子进程 | pi-mono | ~50 |
| `memory.remember()/recall()` — 持久记忆 (markdown) | pi-agent | ~80 |
| `bus.subscribe(topic)/publish(topic)` — pub-sub | AgentMesh | ~60 |
| `orchestrate { on: expression }` — 谓词路由 | revenue-orchestrator | ~80 |
| `sandbox.guard { exfil: true }` — 数据泄漏防护 | pi-agent | ~40 |
| Job 结构加 `channel`/`chat_id`/`delete_after_run` | mimiclaw | ~20 |
| `Fault` 枚举替代原始 String 错误 (10+ call sites) | loongclaw | ~80 |

### 3.3 P2 — 扩展 (共 560 LOC)

| 补丁 | 灵感 | LOC |
|---|---|---|
| `skill.md` 格式 + 双注册表 (`mora-hub.json` + `mora-public.json`) | CLI-Anything | ~150 |
| `ToolPlane` (Core/Extension adapter) 替代 `tool_registry` | loongclaw | ~150 |
| `ai.retry { attempts: 10, backoff: exponential }` | mini-swe-agent | ~50 |
| `ai.role { worker / thinker / verifier }` | OpenFugu | ~60 |
| `ai.reflect { max_turns: 5 }` — 自检回合 | pi-agent | ~40 |
| DAG-as-data → `orchestrate` 扩展 | OpenFugu | ~80 |
| `tool.register(name, fn, stage: "pre" | "post")` | AgentMesh | ~30 |
| `context.outputs` (共享代理输出) | AgentMesh | ~30 |
| `plan.update([{step, status}])` — 实时清单 | pi-agent | ~40 |
| `heartbeat.md` 可执行检查列表 | mimiclaw | ~50 |
| `context.trim(threshold)` — 智能截断 | pi-agent + AgentMesh | ~40 |
| `mora refine script.mora "add X"` — 增量编辑 | CLI-Anything | ~100 |

### 3.4 未来探索 (v1.0+)

| 补丁 | 灵感 | 备注 |
|---|---|---|
| WASM 沙箱 (wasmtime) | loongclaw, OpenInfer | 大重构 |
| TRINITY 路由器 (19.5K 参数线性分类器) | OpenFugu | 需要模型集成 |
| 两层 KV 缓存卸载 | OpenInfer | GPU-specific |
| ML-based layoutreader (LayoutLM) | MinerU | 需要 ML 运行时 |
| ContentRouter 11 种压缩策略 | Headroom | 大范围 |
| 5 层 DI 容器模式 | Puter | 架构级 |

---

## 4. v0.41 推荐执行计划 (首批 5 commit)

| # | Commit | LOC | 测试 |
|---|---|---|---|
| 1 | `fix(event): O(segments) indexed matching replaces linear scan (Puter)` | ~30 | +2 |
| 2 | `fix(reading_order): recursive XY-cut replaces flat sort (MinerU)` | ~50 | +3 |
| 3 | `feat(sandbox): CapKey + Capability enum — token-gated execution (loongclaw)` | ~200 | +5 |
| 4 | `feat(audit): AuditSink trait + SHA-256 chained JsonlAuditSink (loongclaw)` | ~200 | +4 |
| 5 | `feat(exec): exec.parallel() — concurrent subprocess execution (pi-mono)` | ~50 | +3 |
| **总计** | | **~530** | **+17** |

---

## 5. 文档结束标志

本文件为 mora-lang `RESEARCH_PRIMITIVES_MASTER.md` — 主仓库中**所有外部灵感项目的权威参考**. 每个 v0.41+ 功能提案应引用本文件中的对应项目部分.

> **维护说明**: 当探索新项目时, 追加到此文件. 当实现原语时, 在对应部分标注 `✅ DONE in vX.YZ`.
