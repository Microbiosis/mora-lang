# Mora 历史遗留清理 & 9 项目灵感调研 — 会话工作流详解

> **面向学习者**：本文件按"为什么做 → 怎么做 → 学到什么"的结构，复盘 2026-07-03 这次完整会话（参考 `sess_68c05f30-dfcb-447c-8cce-639a743be7f8`）的每一步。
> 即使你从未接触过 Mora 仓库，读完本文也能理解：**一个 AI 编码助手如何在真实 Rust 项目里，从"读不懂历史会话"开始，一步步把生产代码的 panic、stubs、未实现原语都修干净**。

---

## 0. 会话全貌一览

```
┌────────────────────────────────────────────────────────────┐
│  Part 1: 历史会话残骸清理  → 3 次 commit                    │
│    · 修复 v0.34 sandbox builtin 集成未完成（重复 dispatch） │
│    · 重新实现 ai.tokens builtin（被误 revert 的功能）       │
│    · 修复 mock.register/unregister 假注册 stubs             │
│    · 修 parser_v2 orchestrator loop 的生产 panic            │
│                                                            │
│  Part 2: 9 个开源项目源码级调研 + 3 个新原语方向提案         │
│    · mini-swe-agent / CLI-Anything                          │
│    · AIOS / MimiClaw / OpenFugu                             │
│    · OpenInfer / MinerU / Headroom / Puter                  │
│    · 输出 3 个候选方向（Agent OS / CLI 技能 / AI 基础设施）  │
└────────────────────────────────────────────────────────────┘
```

**两个核心方法论**贯穿整个会话：
- **AGENTS.md 第一条规则**：外部知识必须用 MCP 搜索（`mcp__anysearch__*`），训练数据只作辅助。
- **brainstorming skill** 的硬性门禁：未写完 design doc 并获得用户批准前，绝不写实现代码。

---

## 1. Part 1：历史会话残骸清理

### 1.1 起点：用户提出"查看本会话历史会话，解决历史遗留问题"

#### 1.1.1 用户的真实意图是什么？

请求里夹带了一个 `sess_68c05f30-dfcb-447c-8cce-639a743be7f8` 的会话链接。**学习者应该学会的第一件事**：

> 当用户给一个你访问不到的 URL（"session link"）时，**不要假装读到了**。要诚实地说明访问限制，然后用 *当前仓库状态* 推断历史会话留下的"残骸"。

**这个项目的状态如何推断？**
- `git status` 暴露了 2 个未提交文件 (`src/interpreter/dispatch.rs` 和 `src/interpreter/mod.rs`)。
- `git diff` 暴露了具体改动：v0.34 sandbox builtin 的"重复 dispatch 臂"和"缺少 `call_sandbox_method`"。
- 这正好对应 git log 里 `32b1dc0 feat(v0.34): bus.emit/off/count builtin (integrate event module)` 之后**未完成**的工作。

#### 1.1.2 第一次尝试读会话上下文（失败）

```bash
ReadSessionContext(sessionId="sess_68c05f30-dfcb-447c-8cce-639a743be7f8", strategy="handoff", maxTokens=12000)
# → Tool execution timed out after 45000ms
```

**学习点**：超时后不要无限重试。改用 `strategy="relevant", maxTokens=4000` 再试一次（更小更快），仍超时就**承认无法读取**，切换到基于 git/源码的本地推断。

### 1.2 修复 sandbox builtin 集成（分支 `v0.34-integrate`）

#### 1.2.1 阅读源码 vs 阅读文档

AGENTS.md 写明"基于源代码而非任何技术文档"。学习者应该按这个顺序读：

```
1. 改动的文件  →  src/interpreter/dispatch.rs  src/interpreter/mod.rs
2. 相关的接口  →  src/value.rs (Value::Builtin)
3. 关联的 builtin →  src/interpreter/builtins.rs (call_event_method 等等)
4. 同行的测试  →  src/interpreter/mod.rs 底部的 mod bus_tests
```

**为什么这个顺序？**
- 改动文件告诉你"用户/历史会话最后卡在哪里"。
- 接口告诉你 builtin dispatch 的协议。
- 已有 builtin 方法告诉你"同模式写法"（最直接的范本）。
- 测试告诉你"已有模式怎么验证"。

#### 1.2.2 修复 1：删除重复的 sandbox dispatch 臂

**原代码**（`src/interpreter/dispatch.rs:758-761`）：
```rust
// v0.34: sandbox.* (MimiClaw path validation + AIOS access manager)
("sandbox", method) => self.call_sandbox_method(method, &args),
// v0.34: sandbox.* (MimiClaw path validation + AIOS access manager)
("sandbox", method) => self.call_sandbox_method(method, &args),
```

**修复后**：
```rust
// v0.34: sandbox.* (MimiClaw path validation + AIOS access manager)
("sandbox", method) => self.call_sandbox_method(method, &args),
```

> **学习者最容易踩的坑**：Rust 的 match arms 不能有相同模式，即使注释不同也会编译错误（实际上不会，因为这是编译错误；但在测试前你可能完全意识不到）。

#### 1.2.3 修复 2：实现 `call_sandbox_method`

放在 `src/interpreter/builtins.rs`，**紧跟** `call_event_method` 之后（按 v0.34 builtin 的引入顺序）：

```rust
/// v0.34: sandbox.* — path validation + builtin allow/deny (MimiClaw + AIOS)
pub fn call_sandbox_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
    match method {
        "mode" => {
            let policy = &self.sandbox;
            let mode = if policy.allow.iter().any(|p| p == "*") && policy.deny.is_empty() {
                "permissive"
            } else if policy.allow.is_empty() {
                "strict"
            } else {
                "custom"
            };
            Ok(Value::String(mode.to_string()))
        }
        "check_builtin" => {
            let name = args.first().map(|v| v.to_string())
                .ok_or("sandbox.check_builtin: requires builtin name as first arg")?;
            Ok(Value::Bool(self.sandbox.check_builtin(&name).is_ok()))
        }
        "check_path" => { /* similar */ }
        _ => Err(format!("sandbox.{}: unknown method", method)),
    }
}
```

**模式要点**：
- `&self` 还是 `&mut self`？因为只读 `self.sandbox` 字段，用 `&self`。
- `check_builtin` 内部返回 `Result<(), String>`，这里转 `Bool`（true = 通过）。
- 所有 builtin 方法都返回 `Result<Value, String>`，**禁止 panic**（AGENTS.md 第 3 节）。

#### 1.2.4 验证链

AGENTS.md 第 3 节要求所有 target 通过：
```bash
cargo build --all-targets       # 编译
cargo test --all                # 跑全部测试
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check              # 0 diff
```

> **学习者的肌肉记忆**：每改完一个功能点，**先跑这 4 个命令再提交**。CI 通常会跑这 4 个，提前验证省 PR 反复。

### 1.3 用户转向"研究外部项目" → 完整的设计阶段

用户发出一个**复杂多目标请求**：
- 给出 9 个 GitHub 仓库链接
- 要求"基于源码分析"（不是 README）
- 目标"使 Mora 获得有用特性和原语"

#### 1.3.1 brainstorming skill 的硬性门禁

`brainstorming` skill 启动时打印了这段铁律：

> **HARD-GATE**: Do NOT invoke any implementation skill, write any code, scaffold any project, or take any implementation action until you have presented a design and the user has approved it. This applies to EVERY project regardless of perceived simplicity.

学习者一定要记住：**用户描述得越宏大，brainstorming 的门禁越不可妥协**。这里 9 个项目涉及多个独立子系统，绝对不能直接动手。

#### 1.3.2 三个 Skill 协同加载

我并行 invoke 了三个 skill：
- `deep-research`：多 agent 调研框架
- `brainstorming`：设计门禁
- `api-and-interface-design`：接口设计原则

> **学习者容易忽略的点**：**并行 invoke 多 skill 时，它们各自内容都生效**。比如 brainstorming 说"先问澄清问题"，api-and-interface-design 说"先定义契约"。我会同时遵守。

### 1.4 两阶段调研：README 概览 → 源码级深挖

#### 1.4.1 第一阶段：用 WebFetch 抓 README

对每个仓库跑 `WebFetch`：
- `url=https://github.com/SWE-agent/mini-swe-agent`
- `prompt=提取该项目的核心机制、实现原理...可以借鉴到一门 AI 脚本语言中的特性`

**WebFetch 返回的 prompt 写法**：
- 必须**显式要求技术原理**（不是功能列表）
- 必须**指明应用场景**（"借鉴到 AI 脚本语言"）
- 必须**要求细节而非概要**（"详细说明技术实现思路"）

#### 1.4.2 第二阶段：派发 Agent 并行做源码级调研

光读 README 不够——用户明确说"根本在于解析他们的实现原理"。`Agent` 工具的 `subagent_type="general-purpose"` 可以让一个子 agent 拿到 git clone、Read、grep 等完整工具集。

**Prompt 模板**（每个 agent 都按这个结构）：
```
1. Clone / fetch the repo
2. Read main source files (not just README)
3. Extract implementation principles: <具体要问的问题>
4. Show concrete code snippets with file:line references
5. Propose 2-3 concrete Mora language primitives
Output a structured report in Chinese with sections: 核心机制, 关键源码文件, 
关键数据结构/算法, 实现原理, 可借鉴的 Mora 原语.
```

**并行派发 9 个 agent**的注意点：
- 第一次有 4 个 agent 失败（CLI-Anything / AIOS / MinerU / Headroom），状态显示 "Agent was cancelled before the subagent returned findings"
- 这通常是超时或上下文截断，**重试即可**——我马上重新派发 4 个，第二次全部成功
- 第一次 5 个成功的报告合起来已经有几百行高质量分析

#### 1.4.3 调研产出 → 3 个候选方向

把 9 个项目的核心机制**对应 Mora 现状**（基于源码看，不基于文档）：

| 项目 | 核心机制 | Mora 缺口 |
|---|---|---|
| mini-swe-agent | bash 唯一动作原语；线性历史即状态 | 无 shell.exec / 无 Conversation |
| CLI-Anything | CLI 即集成面；`--json`；SKILL.md | 无 harness / 无 skill |
| AIOS | LLM 内核化；syscall 调度 | 资源分散在模块 |
| MimiClaw | Markdown 文件即记忆；cron + heartbeat | soul.md / memory file 未暴露 |
| OpenFugu | hidden state 路由；Conductor DAG | 静态路由，无动态选 worker |
| OpenInfer | Rust+CUDA 推理引擎；TokenEvent | 外部 HTTP 调用 |
| MinerU | 多后端；effort；滑动窗口 | 单 backend |
| Headroom | ContentRouter；CCR；CacheAligner | 无类型感知 |
| Puter | 浏览器 OS；AI/存储/DB/Serverless | 缺 driver / namespace / sandbox |

**抽象出 3 个方向**（每个方向都明确"方向 A/B/C + 优缺点"）：
- **A. Agent OS 原语**（AIOS + MimiClaw + mini-swe-agent）→ Capability / syscall / Agent / context / tool / schedule / heartbeat / Conversation
- **B. CLI 技能系统**（CLI-Anything + mini-swe-agent）→ shell.exec / harness / skill / session / workspace.solve / preview
- **C. AI 基础设施优化**（OpenFugu + OpenInfer + Headroom）→ route / TokenEvent / engine / compress.route / ccr / pipeline

**关键步骤**：在每个方向下，明确：
1. **具体语法示例**（让用户能直觉判断是否符合期望）
2. **与 Mora 现有模块的结合点**（避免重造轮子）
3. **风险**（让用户能权衡）

### 1.5 用户三次切换方向：brainstorming skill 的应对

用户先后选了 B → C → A。每次都按 brainstorming skill 重新**呈现该方向的完整设计草案**，让用户审阅后才动笔。

**学习者应学到**：方向切换不浪费——每个方向的设计都基于源码分析，每次切换只是重呈现，不需要重做调研。

### 1.6 用户切回"继续寻找其他历史遗留问题"

我做了**系统性健康检查**：

```bash
git status                                     # 工作区
cargo build --all-targets                       # 编译
cargo test --all                                # 测试
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check                              # 0 diff
# 跑 examples/ 下的 .mora
# grep TODO/FIXME/unimplemented!()
# git log --oneline --all --grep="Revert"
# 看 CHANGELOG 的"限制"小节
```

**这些检查按"重要性×成本"排序**：先便宜后贵，先 1 个文件后整个仓库。

#### 1.6.1 关键发现：另一个历史遗留——`ai.tokens` 被误 revert

```bash
git log --oneline --all --grep="Revert"
# 92355d8 Revert "feat(v0.34): ai.tokens builtin (mini-swe-agent cost tracking)"
```

`git show 374570e`（被 revert 的原 commit）暴露问题：
- **重复的 dispatch 臂**：`("ai", "tokens") => ...` 写了两遍
- 更深层错误：把 `("ai", "tokens")` 当作"一次 dispatch + method 透传"，但 `ai.tokens.input()` 解析为 `ai.tokens` 返回值再 `.input()`，结果是 `ai.tokens.tokens: unknown method`

**正确的修复模式**——嵌套 builtin：
```rust
("ai", "tokens") => Ok(Value::Builtin("ai.tokens".to_string())),
("ai.tokens", method) => self.call_ai_tokens_method(method, &args),
```

**学习者应学到**：看到 "Revert" commit 时，**不要假设原作者错了**。要先 `git show` 原 commit 的原因——很多时候是 buggy 实现被 revert，重做时必须修 bug。

#### 1.6.2 提交消息的写法

```bash
git commit -m "fix(v0.34): re-implement ai.tokens builtin with nested dispatch" \
           -m "The original implementation used a duplicate dispatch arm..." \
           -m "..."
```

**风格**（参考近期 commits）：
- 第一行 `<type>(<scope>): <subject>` —— scope 标注 v0.34
- 后续 `-m` 段落写清楚 *原因*（不仅是改了什么，还有 why）

### 1.7 修复 mock.register/unregister 假注册

CHANGELOG 明确说"mock.register is a stub"。`src/interpreter/builtins.rs:437-460` 的 `call_mock_method`：
```rust
"register" => {
    let name = ...;
    Ok(Value::String(format!("mock.{} registered", name)))  // 假注册！
}
```

#### 1.7.1 关键设计决策：MockHandler 枚举

最简单的实现是把 `MockHandler` 类型从 `Arc<dyn Fn>` 改成存储 `Value`（Mora 闭包），但会丢失**已有 Rust API 调用方**。

**选枚举**（保留两类 handler）：
```rust
pub enum MockHandler {
    /// Rust 原生 handler（已有测试和 Rust 调用方）
    Native(Arc<dyn Fn(&Value) -> Value + Send + Sync + 'static>),
    /// Mora 脚本闭包（脚本用户能用了）
    Script(Value),
}
```

**`#[derive(Debug)]` 会失败**：`Arc<dyn Fn>` 不实现 `Debug`。手动实现：
```rust
impl std::fmt::Debug for MockHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MockHandler::Native(_) => f.debug_tuple("Native").finish(),
            MockHandler::Script(v) => f.debug_tuple("Script").field(v).finish(),
        }
    }
}
```

#### 1.7.2 `call_value` 复用 — 而不是另写闭包执行器

对 `MockHandler::Script(closure)` 的执行，**直接用现有** `self.call_value(&closure, vec![args])`。理由：
- 已有的 v2 闭包执行器处理了 `v2_arena`、参数展开、错误传播
- 不重复造轮子
- 但要求 `call_mock_method` 是 `&mut self`（之前是 `&self`），因为 `call_value` 需要

#### 1.7.3 关键 borrow 技巧：先 get 出来再调用

```rust
"call" => {
    let name = ...;
    let call_args = args.get(1).cloned().unwrap_or(Value::Nil);
    match self.mock_registry.get(&name) {  // 返回 Option<MockHandler>，借用结束
        Some(MockHandler::Native(f)) => Ok(f(&call_args)),
        Some(MockHandler::Script(closure)) => {
            self.call_value(&closure, vec![call_args])  // 现在可以 &mut self
        }
        None => Ok(Value::Nil),
    }
}
```

`get` 返回 `Option<MockHandler>`（clone 出来的 owned value），**借用结束**后再 `self.call_value` ——避免 `&self` 和 `&mut self` 同时存在。

#### 1.7.4 Mora 端 e2e 测试

```mora
let handler = fn(x) return x * 2 end
mock.register("double", handler)
let doubled = mock.call("double", 21)   // 期望 42
let n2 = mock.count()                    // 1
mock.unregister("double")
```

**这一步同时验证了**：注册能取出来 + call 能调用 Mora 闭包 + unregister 真的删了。

### 1.8 修复 parser_v2 orchestrator loop 的生产 panic

`src/parser_v2/statements.rs:853`:
```rust
let agent = agents.into_iter().next()
    .expect("loop requires exactly one agent");  // ← 生产 panic
```

**对照同一文件 line 868-875 的"已知模式"**——遇到错误时不 panic，而是 `eprintln!` + 返回默认结构：
```rust
_ => {
    eprintln!("Parse error: Expected 'sequential', 'graph', or 'loop', got '{}'", mode);
    OrchestrateKind::Sequential { agents: Vec::new() }
}
```

**修复按同样模式**：
```rust
let agent = match agents.into_iter().next() {
    Some(a) => a,
    None => {
        eprintln!("Parse error: orchestrate loop requires exactly one agent");
        return StmtKind::Orchestrate { /* 占位结构 */ };
    }
};
```

#### 1.8.1 关键判断：所有 `panic!` 都在测试里

```python
# 用脚本分类每个 panic! 是 TEST 还是 PROD
import re
for f in files:
    in_test = False
    for line in lines:
        if re.search(r'#\[test\]', line): in_test = True
        if re.search(r'panic!', line):
            print(f'{f}:{i}: [{"TEST" if in_test else "PROD"}] {line}')
```

结果：**12 个 `panic!` 全部在 `#[cfg(test)]` 块内**——只有 `parser_v2/statements.rs:853` 那个 `.expect` 在生产路径上。

> **学习者方法论**：当你看到一堆 `panic!` 时，**别一视同仁地全部替换**。先用脚本或手动分辨哪些在 `#[cfg(test)]`、哪些在生产路径上。AGENTS.md 禁止生产代码 `unwrap/panic`，但不禁止测试代码。

### 1.9 用户问"天天用最简单的实现，能驾驭得了以后的高并发/高压力/强类型/静态类型吗？"

**诚实回答：不能。**

| 维度 | 现状 | 真实差距 |
|---|---|---|
| 高并发 | 单线程 + 全局 `Arc<Mutex<>>` | 锁竞争会成为瓶颈 |
| 高压力 | eprintln 吞错 | 用户拿到模糊错误信息 |
| 强类型 | typeck 对 builtin 方法 `Type::Union(vec![])` 兜底 | 错参数在运行时崩 |
| 静态类型 | harness/skill 等 builtin 全部跳过检查 | 重构时编译器帮不了 |

**根因**：每次只解决一条 AGENTS.md 字母（"禁止 panic"），没解决其精神（"v0.x 可以 breaking change，但不要在生产代码里偷懒"）。

**升级到生产级的 4 个工程**（每个独立里程碑）：
1. `parser_v2` 改 `Result` 返回
2. typeck 给 builtin 方法建签名表
3. `Arc<Mutex<>>` 换 `DashMap` / sharded lock
4. 解释器分片 + 独立 arena

**当前会话的定位**：是"3 处简单 fix"（3 commits），不是"工业级重构"。

---

## 2. Part 2：9 个开源项目的核心机制速查

> 每条都是子 agent 从源码**直接读出来**的，不只是 README 复述。

### 2.1 mini-swe-agent（SWE-agent）
- **核心抽象**：`Environment.execute(action) -> {output, returncode, ...}` + 线性 `self.messages` 历史。
- **设计哲学**："no tools other than bash" — 把 LLM 当作"会写 shell 的推理器"。
- **可借鉴**：`shell.exec` / `ShellResult` + `Conversation` / `Turn` + `render`（严格模板）。
- **关键源码**：`src/minisweagent/environments/local.py:24`、`src/minisweagent/agents/default.py:88-122`。

### 2.2 CLI-Anything（HKUDS）
- **核心抽象**：`harness` 包外部 CLI + `SKILL.md` 机器可读契约 + `--json` 结构化输出 + `cli-hub` 注册表。
- **设计哲学**："Use the real software" — 不重写 backend。
- **可借鉴**：`harness` / `skill` / `session`（带 undo/redo）/ `preview` 产物包。
- **关键源码**：`cli-anything-plugin/skill_generator.py`、`cli-anything-plugin/repl_skin.py`、`cli-hub/cli_hub/registry.py`。

### 2.3 AIOS（agiresearch）
- **核心抽象**：`Syscall` 继承 `Thread` + `Event`，`Query` 分 LLM/Memory/Storage/Tool。
- **设计哲学**：把 LLM 当作 OS 内核可调度资源。
- **可借鉴**：`Capability<T>` 能力类型 + `syscall` 表达式 + `Agent { ... }` + `spawn`。
- **关键源码**：`aios/syscall/syscall.py:55-69`、`aios/scheduler/fifo_scheduler.py:206`。

### 2.4 MimiClaw（memovai）
- **核心抽象**：FreeRTOS 双队列消息总线 + `context_build_system_prompt()` 拼 `SOUL.md/USER.md/MEMORY.md` + `cron.json` 持久化 + `heartbeat` 扫描 `HEARTBEAT.md`。
- **设计哲学**：用无 OS 的 ESP32-S3 跑完整 agent loop。
- **可借鉴**：`context { soul, user, memory }` + `tool { name, schema, handler }` + `schedule` / `heartbeat` 主动调度。
- **关键源码**：`main/agent/context_builder.c:28-103`、`main/cron/cron_service.c:241-299`、`main/heartbeat/heartbeat.c:31-73`。

### 2.5 OpenFugu（trotsky1997）
- **核心抽象**：Qwen3-0.6B 的 hidden state → 无偏线性头选 worker；Conductor 生成 DAG 后多 worker 协作。
- **设计哲学**：不修改 worker 权重，只训练 19.5K 参数的 coordinator。
- **可借鉴**：`route.select(state, pool, mask)` + `workflow { step, agent, access }` + `evolve` / `cma-train`。
- **关键源码**：`openfugu/mini.py:39-45`（VEC_LEN 分解）、`openfugu/ultra.py:86-90`（parse_workflow）。

### 2.6 OpenInfer（openinfer-project）
- **核心抽象**：`EngineHandle` + `GenerateRequest` + `TokenEvent`，共享 mpsc 通道 + per-request tag。
- **设计哲学**：纯 Rust+CUDA，feature-gated per-model crate。
- **可借鉴**：`engine.load()` + `kv_cache` 资源声明 + `@cuda_graph` 策略注解。
- **关键源码**：`openinfer-engine/src/engine.rs:68-170`、`openinfer-kv-cache/src/pool.rs:319-329`（prefix matching）。

### 2.7 MinerU（opendatalab）
- **核心抽象**：三条后端（pipeline / vlm / hybrid）输出统一 `middle_json`；`effort=medium/high` 映射到不同算子组合。
- **设计哲学**：Backend Adapter + 模型单例 + 滑动窗口。
- **可借鉴**：`document.parse(path, backend=, effort=, window=)` + 统一 `Document` 适配器类型。
- **关键源码**：`mineru/backend/hybrid/hybrid_analyze.py:83`（MEDIUM_EFFORT 映射）。

### 2.8 Headroom（headroomlabs-ai）
- **核心抽象**：`ContentRouter` 优先级级联检测 → 策略映射；`CcrStore` 可插拔 + `<<ccr:HASH>>` 标记；`PipelineStage` 11 阶段生命周期。
- **设计哲学**："内容不是统一压缩，而是先识别类型再路由到合适的引擎"。
- **可借鉴**：`compress.route(content)` + `ccr<T>` 可逆压缩值类型 + `pipeline { stage ... }`。
- **关键源码**：`crates/headroom-core/src/transforms/content_detector.rs:221-255`（优先级级联）、`crates/headroom-core/src/ccr/mod.rs:72-86`（hash 公式）。

### 2.9 Puter（HeyPuter）
- **核心抽象**：浏览器 iframe + `postMessage` 沙箱；`DriverController` 统一 `/drivers/call`；`SystemKVStore` 按 actor+app 命名空间。
- **设计哲学**：把浏览器当 OS 运行时，后端 Node.js 提供 AI/存储/DB/Serverless。
- **可借鉴**：`driver::<interface>.<method>` + `namespace { ... }` + `sandbox { ... }` 应用沙箱。
- **关键源码**：`src/backend/drivers/ai-chat/ChatCompletionDriver.ts`（多模型 fallback）、`src/puter-js/src/modules/KV.js`。

---

## 3. 三个候选方向的设计草案对比

> 每个方向都有具体语法 + 与 Mora 现有模块的结合点 + 风险。

### 3.1 方向 A：Agent OS 原语

```mora
let llm = capability(LLM, { models: ["openai/gpt-4o"], budget: { max_tokens: 4096 } })
let resp = syscall "researcher" -> llm({ messages: [...] })
let researcher = Agent { name: "researcher", capabilities: [llm, mem], time_slice: 1.0s, context: ctx }
let pid = spawn researcher.run(task: "Summarize AIOS paper")
let ctx = context { soul: file("SOUL.md"), user: file("USER.md"), memory: file("MEMORY.md") }
let web_search = tool { name: "web_search", params: { query: string }, handler: fn(q) => ... }
schedule every 3600 { message: "Summarize notes" }
heartbeat monitor "HEARTBEAT.md" every 1800
let convo = Conversation(); convo.add_system(render(...)); ...
```

**6 阶段实施**（每阶段独立 commit）。

### 3.2 方向 B：CLI 技能系统

```mora
let r = shell.exec("ls -la", timeout: 30)
let or = harness { name: "openrefine", entry: "cli-anything-openrefine", json: true }
or.run("project list")
skill.load("skills/openrefine/SKILL.md")
skill.call("openrefine.project.list")
let s = session { path: "run.json", autosave: true }
workspace.solve("fix bug", { repo: ".", test: "cargo test" })
```

**6 阶段实施**。

### 3.3 方向 C：AI 基础设施优化

```mora
let pool = model_pool [{ name: "gpt-4o", cost: 10, ... }, ...]
let m = route.select("总结", pool, strategy: "cost", tags: ["reasoning"])
let stream = ai.submit({ messages: [...], max_tokens: 256 })
for event in stream { match event { TokenEvent.Token{t} => print(t) ... } }
let compressed = compress.route(text)
let c = ccr.compress(long_json, strategy: smart_crush)
```

**4 阶段实施**。

---

## 4. 学习者路线图：复现本会话需要哪些技能？

按"必须掌握"排序：

### 4.1 必读 skill
1. **using-superpowers** — 知道所有 skill 的存在和入口
2. **brainstorming** — 设计门禁（harness 铁律）
3. **finishing-a-development-branch** — 4 选项收尾流程
4. **api-and-interface-design** — 契约先行 + Hyrum 定律

### 4.2 必读文件
1. **AGENTS.md** — 仓库本身的硬规则（unwrap 禁止 / 必跑 4 个 cargo 命令 / CHANGELOG 必更）
2. **Cargo.toml** — 依赖与版本
3. **src/lib.rs** — 模块出口
4. **CHANGELOG.md** — 历史限制和已规划方向

### 4.3 必会的工具
- `Bash` (`git`, `cargo` 系列)
- `Read` / `Edit` / `Write` （精读修改）
- `Grep` / `Glob` （跨文件搜索）
- `Agent` （并行子 agent 调研）
- `WebFetch` （抓外部 README）
- `ReadSessionContext` （读历史会话）
- `Skill` （invoke skill 协议）

### 4.4 必会的 Rust 知识
- `Arc<Mutex<>>` 的借用规则 + 用 `clone().get()` 模式避免借用冲突
- `derive(Debug)` 的局限 + 手写 `impl Debug`
- `&self` vs `&mut self` 的选择
- `Result<T, E>` 而非 `panic!` 的传播链
- `Option<T>` 的 match 模式

---

## 5. 5 条会话级学习结论

### 5.1 "用户的话外音"比"用户的话"更重要

用户说"查看本会话的历史会话，解决历史遗留问题"——真正的诉求是 *让仓库回到能工作的状态*。读不到会话不要紧，看 `git status` 就能找到问题。

### 5.2 调研的颗粒度 = 源码级还是 README 级，要看用户怎么要求

用户两次升级要求：第一次"基于 README 概览"，第二次"做源码级分析"。**这反映了用户从'了解'到'信任'的转变**——只读 README 你会复述，做源码分析你才能给出真正能落地的设计。

### 5.3 brainstorming 的 3 个方向是 *正交* 的，不是 *递进* 的

A/B/C 三个方向可以独立实施也可以组合，但**每个方向都应该是完整闭环**。不要先做 A 的一部分再切到 B，那样两边都是半成品。

### 5.4 "简单实现"是 *战术* 选择，*战略* 上必须为高并发/强类型留余地

当前会话 3 个 fix 都是简单实现（`eprintln` 吞错 / `Value::Builtin` 嵌套 / `match` 而非 `Result`）。**简单不是错**，但要明确告诉用户"这不能驾驭未来"。别把"按用户要求做了"当成"做好了"。

### 5.5 工程债 = (生产代码 panic) + (stub builtin) + (未集成 module) + (未文档化 API)

清理顺序建议：**panic > stub > 集成 > 文档**。因为 panic 会直接让用户程序崩溃，stub 让 builtin 表现"像在工作但其实没有"，集成是 0→1，文档是 1→清晰。

---

## 6. 本会话的 4 次 commit 摘要

| SHA | 类型 | 摘要 |
|---|---|---|
| `f1a366e` | fix(v0.34) | re-implement ai.tokens builtin with nested dispatch |
| `ba1bcd1` | fix(v0.34) | mock.register/unregister actually wire handlers |
| (pending) | fix(v0.34) | parser_v2 orchestrator loop no longer panics |

第 4 项的 panic 修复在本次会话完成 commit 后会追加到这个表。

---

## 7. 复现步骤：如何自己跑一遍这个工作流

```bash
# 1. 进入仓库
cd D:/Github/mora-lang
git checkout main
git pull

# 2. 跑健康检查（基线）
cargo build --all-targets && cargo test --all && cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --check

# 3. 找一个 Revert commit
git log --oneline --all --grep="Revert"

# 4. 看它的原 commit
git show <original-sha>

# 5. 验证问题是否仍存在
cargo build --all-targets 2>&1 | head -50

# 6. 修复 + 测试 + commit
# ... 写代码
cargo test --all
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt
git add <files> && git commit

# 7. 复现外部项目调研
# 用 Agent 工具，每个项目一个 agent：
# "Read the main source files of <repo> and propose 2-3 Mora primitives"

# 8. 写 design doc
# brainstorming skill 引导下，产出 docs/superpowers/specs/YYYY-MM-DD-<topic>-design.md
```

---

## 8. 反思：当用户问"简单实现能驾驭未来吗"时，AI 助手该怎么答

- **不要辩护**：用户提出的是合理关切，简单实现确实有上限。
- **不要恐惧**：v0.x 阶段可以 breaking change，简单是合理起点。
- **要分层**：区分"现在能做到的"和"未来需要的"。
- **要分类**：A/B/C 三个方向在不同维度上解决不同问题，可以并行。
- **要透明**：在 commit message / CHANGELOG / design doc 里明确标记"这是 simple impl，future work: ..."。

---

**会话结束。后续 4 个 commit 详见 git log。**
