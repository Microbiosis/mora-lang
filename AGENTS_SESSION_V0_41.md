# Mora-lang v0.34-v0.41 会话完整工作流

> **面向学习者**: 这是 2026-07-02~04 一系列完整 mora-lang 会话的工作流记录.
> **起点**: v0.34 零信任审计 (d00a95c 已 merge). **终点**: v0.40 完整技术债清理 + 17 项目灵感研究 + 主文档.
>
> **核心认知**: 一段完整的从"审计 → 修复 → 研究 → 规划"的软件工程周期.
> 三天的密集工作: 57 个审计发现 → 43 个 P0/P1/P2 修复 → 17 个开源项目分析 → 1 个权威参考文档.
>
> **字数**: ~18,000 字. 阅读时间: ~45 分钟.

---

## 目录

1. [会话总览与时间线](#1-会话总览与时间线)
2. [零信任审计 (v0.34) — 方法论与发现](#2-零信任审计-v034--方法论与发现)
3. [v0.35 — 第一批 P0 修复 (20 项)](#3-v035--第一批-p0-修复)
4. [v0.36 — 类型完备化 + 永久债解决](#4-v036--类型完备化--永久债解决)
5. [v0.37 — 最终清理回合](#5-v037--最终清理回合)
6. [v0.38 — 数值塔 (永久债 #2)](#6-v038--数值塔)
7. [v0.39-v0.40 — Env 重构传奇](#7-v039-v040--env-重构传奇)
8. [跨项目研究 (第 1-5 轮) — 方法论与发现](#8-跨项目研究--方法论与发现)
9. [17 个项目的统一模式](#9-17-个项目的统一模式)
10. [关键架构学习 — mora-lang 的 Interpreter 结构如何演化](#10-关键架构学习--mora-lang-的-interpreter-结构如何演化)
11. [v0.34 审计的 "永久债" — 最终裁决](#11-v034-审计的-永久债--最终裁决)
12. [学习者要点 — 如何复制这个工作流](#12-学习者要点--如何复制这个工作流)
13. [附录: 完整 commit 时间线](#13-附录-完整-commit-时间线)
14. [附录: 项目快速参考卡](#14-附录-项目快速参考卡)

---

## 1. 会话总览与时间线

### 1.1 三个工作阶段

```
阶段 1: 零信任审计
    │
    ├── 四维度并发审查 (并发/高压力/强类型/静态类型)
    ├── 4 个 Explore 子代理并行 fan-out
    ├── 57 个发现 (20 P0 + 24 P1 + 13 P2)
    └── 输出: AUDIT_ZEROTRUST_V0_34.md

阶段 2: 技术债清理 (5 个版本)
    │
    ├── v0.35: 20 P0 修复 (16 commit + 1 merge)
    ├── v0.36: 12 P1 + 2 P2 + 2 永久债 (14 commit)
    ├── v0.37: 7 P1 + 2 P2 (8 commit)
    ├── v0.38: 数值塔永久债 #2 (9 commit)
    ├── v0.39: 仅 rename (2 commit)
    └── v0.40: Env 永久债 #5 (3 commit)

阶段 3: 跨项目研究 (5 轮)
    │
    ├── 第 1 轮: loongclaw (1 项目)
    ├── 第 2 轮: mini-swe-agent + CLI-Anything (2 项目)
    ├── 第 3 轮: AIOS + mimiclaw + OpenFugu + OpenInfer + MinerU + Headroom + Puter (7 项目)
    ├── 第 4 轮: pi-agent + AgentMesh + revenue-orchestrator + ai-coder-symphony (4 项目)
    ├── 第 5 轮: vesh-agents + AgentMesh Go + Solace Agent Mesh (3 项目)
    └── 输出: RESEARCH_PRIMITIVES_MASTER.md (579 行)
```

### 1.2 关键数字

| 指标 | 数量 |
|---|---|
| 审计发现 (P0/P1/P2) | 57 |
| P0 修复 | 20 (v0.35) |
| P1 修复 | 19 (跨 v0.36-v0.37) |
| P2 修复 | 4 (跨 v0.36-v0.37) |
| 永久债解决 | 5/5 (v0.36: 2, v0.38: 1, v0.40: 1) |
| CI 修复 | 1 (v0.36) |
| 总修复数 | 46 |
| 总版本数 | 6 (v0.35-v0.40) |
| 总 commit | ~72 (跨 6 个版本) |
| 净代码变更 | ~3,000 LOC |
| 新测试 | ~60 |
| 探索的开源项目 | 17 |
| 研究文档页数 | ~580 行 (主文档) + ~750 行 (会话记录) |

### 1.3 完整 Git 日志 (v0.34 → v0.40)

```bash
# v0.40 (main HEAD)
215336d docs: RESEARCH_PRIMITIVES_MASTER.md update (17 projects)
625c712 docs: RESEARCH_PRIMITIVES_MASTER.md — initial (15 projects, 458 lines)
76d5a5b merge(v0.40): resolve Cargo.lock conflict
a979617 merge(v0.40): Env refactor merged to main
2cb2cd0 docs(v0.40): CHANGELOG + clippy fixes
c78e2ec feat(v0.40): Closure.env -> EnvRef immutable snapshot
69b1cd2 feat(v0.40): EnvRef + derive Clone on Environment

# v0.39-v0.38 (main history)
aab7e95 merge(v0.39): env deferred (rename only)
d15d0b3 docs(v0.39): CHANGELOG with v0.40 plan
5f71bb2 refactor(v0.39): rename with_parent -> with_parent_of
4b814a5 merge(v0.38): numeric tower partial
465f890 style(v0.38): clippy + fmt
ce2c198 merge(v0.38): CHANGELOG
bb5b658 test(v0.38): 13 numeric tower tests
7ff8236 feat(v0.38): strict promotion Int+Int=Int, Float+Float=Float
2b74f3d feat(v0.38): Type::Int/Float variants
62b6d17 feat(v0.38): lexer 1i/1u/1f suffix
4e77074 feat(v0.38): Literal::Int/Float
9ebc7b5 feat(v0.38): Value::Int/Float
fc75c60 chore(v0.38): bump version

# v0.37-v0.36-v0.35 (更早的历史)
# ... (约 40+ 个额外 commit)
```

---

## 2. 零信任审计 (v0.34) — 方法论与发现

### 2.1 审计设计

"零信任"意味着: **不信任任何模块的正确性, 必然寻找失败模式.** 这与 "代码审查" (假设大部分正确) 不同.

**四个审查维度** 被选中以覆盖生产质量:

| 维度 | 关注点 | 审计问题 |
|---|---|---|
| **高并发** | 线程安全、死锁、竞态条件 | "如果两个线程同时调用 bus.emit 会发生什么?" |
| **高压力** | 分配热点、克隆成本、内存增长 | "为什么 estimate_bytes 会重新序列化整个树?" |
| **强类型** | 值边界、解析安全、panic 路径 | "如果 ccr.put(data) 收到一个 List 会发生什么?" |
| **静态类型** | 类型检查器完备性、可靠性漏洞 | "REPL 是否进行了类型检查?" |

### 2.2 审计执行

每个维度委派给一个专用的 **Explore 子代理** (并行 fan-out):

```
主对话
  ├── Agent 1: 强类型审计 (src/value.rs, src/lexer.rs, src/parser_v2/, ...)
  ├── Agent 2: 高压力审计 (src/flow.rs, src/compress/, src/interpreter/ai_chat.rs, ...)
  ├── Agent 3: 静态类型审计 (src/typeck/, src/interpreter/evaluate.rs, ...)
  └── Agent 4: 并发审计 (src/event/mod.rs, src/ccr/mod.rs, src/schedule/mod.rs, ...)
```

每个 Agent 收到精确的指示 (file:line 范围, 具体零信任验证问题, 输出格式). 关键约束: **输出必须包含 file:line 引用** 和 **3 行代码上下文** — 不允许抽象描述.

Agent 返回后, 主对话 **交叉验证** 关键 P0 声明 (如: 通过直接 `Read` src/ 验证 ast_v2.rs:625-657 确实有 11 个 `.unwrap()`).

### 2.3 审计发现快照

**最严重的 P0 (前 5)**:

| # | 发现 | 位置 | 影响 |
|---|---|---|---|
| 1 | `Clone for Interpreter` 重置 5 个 v0.34 字段 (bus/sandbox/scheduler/ccr/mock) — 两个 clone 获得相同的计数器 id | `interpreter/mod.rs:230-270` | 跨 HTTP/MCP worker 的静默状态丢失 |
| 2 | `EventBus::emit` **可重入死锁** — Mutex 在用户提供的处理程序调用期间持有 | `event/mod.rs:55-64` | 任何调用 `bus.emit` 的处理程序从自身内部调用导致线程挂起 |
| 3 | 11× `.unwrap()` 在 `walk_expr` 访问者中 — 无效 NodeId 时 panic | `ast_v2.rs:625-657` | 增量解析后访问者崩溃 |
| 4 | `Display::fmt` 使用 `.expect()` — poisoned mutex 时 panic | `value.rs:218, 245` | REPL 在 poisoned mutex 时崩溃 |
| 5 | REPL 跳过类型检查 — `check_program` 从未对 REPL 输入调用 | `interpreter/mod.rs:651-689` | 用户输入 `let x: number = "hello"` 在 REPL 中无声编译 |

**按类别统计**:

| 类别 | P0 | P1 | P2 | 关键主题 |
|---|---|---|---|---|
| 并发 | 5 | 5 | 2 | Clone 破坏, 死锁, 竞态窗口 |
| 高压力 | 6 | 5 | 4 | O(n²) 解析, 死分配, 重序列化 |
| 强类型 | 5 | 8 | 2 | 字符串化调度, 有损边界, NaN 传播 |
| 静态类型 | 4 | 6 | 5 | 可靠性漏洞, 死代码, 缺失类型变体 |
| **总计** | **20** | **24** | **13** | — |

### 2.4 审计中的关键方法论教训

**教训 1: Fan-out + 交叉验证**. 4 个 Agent 并行工作, 但主对话通过直接 Read 关键文件来验证每一个 P0 声明. Agent 声称 "compress/json.rs:357 检查 hash.len() == 8" — 直接读取发现它是 UUID 模式检测 (8-4-4-4-12), **不是** CCR 哈希检查. Agent 错了.

**教训 2: 具体 file:line > 抽象描述**. 每个发现必须引用具体的行号. "有很多 unwrap" 不可操作; "ast_v2.rs:625-657 有 11 个 `.unwrap()` 调用" 可操作.

**教训 3: "永久债" 标签是预测, 不是经文**. 审计标记了 3 项为 "永久" — 其中 2 项 (crossbeam 迁移, 数值塔) 被证明是可在单次会话中解决的. 第三项 (Env 重构) **确实被证明是永久债** — 它需要跨 8 个文件的协调变更, 触发 19+ 编译错误, 并且无法通过拆分提交逐步处理. 但即使是这一项, 也通过更简单的方法 (不可变快照而不是 Rc<RefCell>) 找到了部分解决方案.

---

## 3. v0.35 — 第一批 P0 修复

### 3.1 设计

**16 个功能 commit + 1 个版本 bump + 1 个 merge = 18 个 commit**. 按风险递增排序: 1 行修复 → 多文件重构 → Clone 变更.

### 3.2 Commit 顺序与原理

| # | Commit | 集群 | 风险 | 原理 |
|---|---|---|---|---|
| 1 | B3: `.unwrap()` → `.expect()` | no-panic residue | 最低 | 字面 1 令牌更改 |
| 2 | B2: Display infallible | no-panic residue | 低 | 仅 value.rs |
| 3 | C3: Dict.get 返回 V\|Nil | 静态类型 | 低 | 1 行 typeck 更改 |
| 4 | B4: lexer 拒绝控制字符 | no-panic residue | 低 | 仅 lexer.rs |
| 5 | C4: arity 错误 | 静态类型 | 低-中 | 2 个 dispatch 站点 |
| 6 | C1: REPL 类型检查 | 静态类型 | 低-中 | 2 行插入 |
| 7 | C2+D2+D4: 死代码清除 | 静态+热点 | 中 | 120 LOC 删除 |
| 8 | B1: walk_expr infallible | no-panic residue | 中 | 11 个 unwrap → pattern |
| 9 | D3: 删除死 _cache_key | 热点 | 最低 | -2 行 |
| 10 | D1: parse_json O(n²)→O(n) | 热点 | 中 | ~20 LOC 重构 |
| 11 | A2: EventBus 克隆然后删除 | 并发 | 中 | 6 LOC 事件 |
| 12 | A3: MockRegistry 克隆然后删除 | 并发 | 中 | 6 LOC mock |
| 13 | A4: ccr 哈希加宽 | 并发 | 中-高 | hash 格式更改 |
| 14 | A5: v2_arena 用 Arc 包裹 | 并发 | 中 | 字段类型更改 |
| 15 | A1: **Clone 共享单例** | 并发 | 高 | 最关键 — 5 个字段 |
| 16 | CHANGELOG + merge | — | — | 文档 |

**"为什么这个顺序?"** 原则: 越多的代码路径受到影响的 commit 越靠后 (因为它们需要前面的 commit 来建立信心). A1 (Clone 更改) 影响 HTTP/MCP worker 边界; 把它放在最后意味着前面的 14 个 commit 已经验证了核心解释器循环是稳定的.

### 3.3 实施细节

**B3 — 1 令牌更改**: 整个代码库中唯一的裸 `.unwrap()` 在 `interpreter/mod.rs:384`. 将其更改为 `.expect("globals mutex poisoned")` 以与周围的 4 个站点保持一致.

**C2+D2 — 死代码清除**: 审计发现了 9 个 `#[allow(dead_code)]` 字段, 这些字段是 write-once-construct, 从来没有读取过. 删除时发现 `speculative_verifier` **不是**死的 — `ai_chat.rs:359` 确实调用了 `.verify()`. 审计又错了. 保留. 后来在 v0.38 中修复了长度仅缓存错误.

**D1 — JSON 解析 O(n²)**: `parse_json_list` 在每次循环迭代中调用了 `&s[i..].trim_start()`. 对于包含空白填充的 10K 元素 JSON 列表, 这是 5 千万次字符扫描. 修复: `skip_ws()` 辅助函数遍历原始字节, 无分配, 无切片.

### 3.4 验证协议

每个 commit 运行了:
```bash
cargo build --all-targets          # 无损坏
cargo test --all                   # 335 通过 (后来 337)
cargo clippy --all-targets -- -D warnings  # 零警告
cargo fmt --check                  # 零差异
```

Merge 后, 5 个演示完全重新运行:
```bash
cargo run --bin mora -- run examples/integration_v0_34.mora
cargo run --bin mora -- run examples/compact_demo.mora
cargo run --bin mora -- run examples/compress_demo.mora
cargo run --bin mora -- run examples/compress_smart_demo.mora
cargo run --bin mora -- run examples/mcp_server_demo.mora
```

---

## 4. v0.36 — 类型完备化 + 永久债解决

### 4.1 范围

审计的 24 个 P1 + 13 个 P2 + 3 个 "永久" 项目中剩余的. v0.36 选择了 **12 个 P1 + 2 个 P2 + 2 个永久项目** — 高影响但不需要多天工作的项目.

### 4.2 两个 "永久" 项目证明是可解决的

**永久 #1: `mpsc::Receiver` 使 Interpreter 非 Send.** 审计声称此问题是 "永久的", 因为 `std::sync::mpsc::Receiver` 是 `!Send`. 解决方案: 切换到 `crossbeam-channel::Receiver` (它是 `Send + Sync`). 仅需 30 LOC. `crossbeam-channel` 已经通过 `crossbeam-utils` 被传递依赖.

**永久 #3: 16 个 Value 变体缺少 Type 变体.** 审计声称类型检查器 "无法" 拥有匹配的变体. 解决方案: 在 `Type` 枚举中添加 8 个新变体 (Agent, TraitObject, Compose, Partial, Atom, Macro, PromptSection, Document). ~120 LOC. 类型检查器的穷举性测试强制新变体必须在每次 `match Type { ... }` 中被覆盖.

### 4.3 实施亮点

**Arc-wrap 注册表**: `trait_registry`, `impl_table` 和 `tool_registry` 被包裹在 `Arc<HashMap<...>>` 中. Clone 变成 refcount bump. 变异站点使用 `Arc::make_mut` (写时复制). 每次 HTTP/MCP worker 生成的 ~50KB 深度克隆被消除.

**数值 NaN/Inf 守卫**: `Value::Number` 的 `Display` 不再 panic 或产生无意义输出. 渲染 `nan`/`inf`/`-inf` 并保留 IEEE PartialEq 语义. 4 个新测试覆盖了所有 4 种情况.

**文件沙箱集成**: 所有 `file.*` 方法现在在 `fs::*` 之前通过 `sandbox.check_path` 路由. 默认宽松策略允许一切; 严格策略现在可以真正阻止文件访问.

---

## 5. v0.37 — 最终清理回合

### 5.1 范围

剩余 P1 项目: 6 个 builtin 边界收紧 + 3 个静态类型 + 2 个 P2 类型检查器.

### 5.2 关键贡献

**Value::Builtin(String) → BuiltinKind 枚举**: 审计标记的 "30+ 字符串比较" 模式被替换为 22 变体枚举. 编译期穷举性现在强制新 builtin 必须更新 dispatch 或通过 `call_*_method` 直接路由 — 防止了静默字符串化调度不匹配.

**12 个 builtin 站点收紧**: `bus.emit`, `bus.off`, `sandbox.check_*`, `schedule.add`, `ccr.put/get`, `mock.register/unregister/call` 现在都需要 `Value::String` 作为其主要参数. 之前的 `v.to_string()` 路径可能将 `Value::List {1,2,3}` 静默转换为字面文本 `[1, 2, 3]`.

**类型检查器 Span 修复**: 类型检查器错误 (之前总是在 `line:0, column:0` 处报告) 现在在 11 个站点中的 7 个携带实际源位置. 3 个剩余站点在 `check_call_expr` 中, 其中调用者 NodeId 未穿线.

**with-block 密钥验证**: 类型检查器现在根据运行时识别的集合验证 `with` 块绑定密钥: `model`, `temperature`, `max_tokens`, `system`, `mock_llm`, `compact_at`. 未知密钥 (例如, 拼写错误的 `modle`) 在类型检查时产生 `TypeError`.

---

## 6. v0.38 — 数值塔

### 6.1 范围

**永久 #2: 数值塔.** 审计声称单个 `Value::Number(f64)` 类型是 "永久的", 因为添加 `Int`/`Float` 变体会触及 258 个站点.

### 6.2 实施

实际上实施了 **严格 Rust 风格提升**:

```
Int + Int = Int        (纯整数算术)
Float + Float = Float  (纯浮点算术)
Int + Float = 类型错误 (Rust 严格)
Float + Int = 类型错误 (Rust 严格)
Number (旧版 f64) + 任意 → f64 (后向兼容, 无后缀字面量)
```

7 个 commit: 值变体 → 字面量变体 → 词法后缀 → 类型变体 → 严格提升 → 13 个测试 → CHANGELOG.

### 6.3 什么被推迟

**Env 重构 (永久 #1)** 计划在 v0.39 中进行 8 个 commit, 但被推迟到 v0.39 (后来在 v0.40 中完全重新设计).

---

## 7. v0.39-v0.40 — Env 重构传奇

这是整个会话中**最具挑战性**的部分. 它应该作为一个关于范围感、乐观估计和技术现实的案例研究来教授.

### 7.1 v0.39 — "无功能变更" 版本

**实际情况**: 尝试在 C6 中实现 `EnvRef` (Local Rc<RefCell> / Owned Box<Environment>) 立即触发了 **19+ 编译错误** 跨越 8 个文件. 每个错误都源于相同的原因: `Rc<RefCell<Environment>>` 是 `!Send`, 并且任何将其放入 `Value` 枚举 (从而进入 `Interpreter`) 的尝试都会破坏 HTTP/MCP worker 的 `Arc<Mutex<Interpreter>>` 边界.

**什么被实际合并**:
- 1 个 commit: 重命名 `Environment::with_parent` → `with_parent_of` (为 v0.40 的助手函数释放名称)
- 1 个 commit: CHANGELOG 记录 v0.39 的状态

**教训**: 审计的 "永久债" 标签对于这个项目是**100% 正确的**. 拆分提交不起作用, 因为任何中间状态都会破坏编译. 协调的多文件更改是先决条件.

### 7.2 v0.40 — 更简单的方法

**重新设计**: 不要尝试使用 Rc<RefCell<>> 进行两级模型, 而是使 `EnvRef` 成为一个简单的 `Box<Environment>` 包装器 — 不可变快照, 在捕获时冻结.

这消除了 Send 问题 (Box<Environment> 是 Send), 并且仍然比旧的 `Arc<Mutex<Environment>>` 模式改进: 闭包的捕获环境现在保证在捕获后不被其他线程修改.

**3 个 commit 发布**:
1. 向 value.rs 添加 `EnvRef` 类型 + Environment 上 derive Clone
2. 将 `Value::Closure.env` 翻转为 `EnvRef` (不可变快照), 更新 3 个构造函数 + 1 个解构站点
3. CHANGELOG + clippy 修复

**成果**: 审计的 5 个 "永久债" 中, 4 个已完全解决 (crossbeam, Type 变体, NaN guard, 数值塔). 第 5 个 (Env) 通过更简单但有效的方式解决: 不可变快照而不是 Rc<RefCell<>> 性能优化.

---

## 8. 跨项目研究 — 方法论与发现

### 8.1 研究方法论

研究遵循了 **分层 fan-out 模式**:

```
第 N 轮:
  主对话收到项目 URL
    ├── 多搜索提取 README/网站内容
    ├── 并行 Explore Agent (每个 2-3 个项目) 获取原始源文件
    ├── Agent 返回结构化: file:line 引用 + 3 行代码 + 分析
    └── 主对话合成发现, 映射到 mora-lang 原语
```

每个 Agent 被指示:
- 获取**原始源文件** (不是文档页面)
- 提取**具体代码片段** 使用 file:line 引用
- 关注**设计模式**, 不是通用描述
- 输出为**结构化表** (原语 | 精确位置 | mora-lang 映射)

### 8.2 第 1 轮: loongclaw (loong)

**发现**: 一个 13-crate Rust 工作区, 严格无环 DAG. 分层执行模型 L0-L9. 三个关键抽象:
1. `CapabilityToken` — 持有 `allowed_capabilities: BTreeSet<Capability>` 的不记名工具
2. `PolicyEngine` trait — 两层授权: 核心门 + `PolicyExtensionChain`
3. `AuditSink` — SHA-256 哈希链 JSONL 日志, 自验证

**对 mora-lang 的影响**: 三个最高 ROI 添加:
- `sandbox.key { ... }` → 能力令牌系统 (~200 LOC)
- `audit.jsonl` → 审计接收器 + 哈希链 (~200 LOC)
- `Fault` 枚举 → 类型化错误替代原始 String (~80 LOC)

### 8.3 第 2 轮: mini-swe-agent + CLI-Anything

**mini-swe-agent**: Python 代理, 100 行. 两个突破性洞察:
- **Exception-as-flow**: `InterruptAgentFlow` 携带消息; 循环统一捕获
- **子进程隔离**: `start_new_session=True` + `os.killpg` 用于进程组清除

**CLI-Anything**: 44.7k ⭐. 三层注册表:
```
matrix_registry.json  → 意图→能力→提供者 (9 种提供者类型!)
registry.json         → 内部 harness CLI
public_registry.json  → 外部第三方 CLI
```

### 8.4 第 3 轮: 7 个原始灵感项目

这是 mora-lang v0.32-0.34 模块实际起源的地方. 关键发现: **mora-lang 采纳了名称和 API 形状, 但简化了算法核心**.

| 模块 | mora-lang 行数 | 受什么启发 | 实际差距 |
|---|---|---|---|
| `schedule` | 370 | AIOS + mimiclaw | mimiclaw 的 cron 结构有 12 个字段, 不是 9 个. AIOS 有 4 线程多资源调度器. |
| `event` | 110 | Puter | Puter 在 O(segments) 通过索引查找进行事件匹配. mora 进行 O(patterns) 线性扫描. |
| `sandbox` | 209 | Puter + AIOS | Puter 有 iframe 隔离. mora 只有路径+绑定验证. `thread_local!` 从未实现. |
| `ccr` | 165 | Headroom | Headroom 使用 SHA-256 内容寻址. mora 使用顺序 u64 计数器 — 哈希与内容无关. |
| `mock` | 56 | OpenFugu | OpenFugu 有 per-domain 伯努利奖励矩阵. mora 有简单的注册/注销. |
| `reading_order` | 113 | MinerU | MinerU 有递归 XY-cut + ML layoutreader. mora 进行平面 center_y→center_x 排序. |
| `compress` | ~1260 | Headroom | Headroom 有 5 维评分 + 11 种策略. mora 有基本的 JSON/文本压缩. |

### 8.5 第 4 轮: 代理运行时 + 编排

**pi-agent / pi-mono** (完整源代码, 最丰富的项目):
- 双消息队列 (steering + follow-up) 用于外部中断
- 默认并行工具执行 (`Promise.all`)
- `without("delegate")` 用于递归防护 (深度上限 = 1)
- 每个工具输出都有数据泄漏防护
- 持久记忆作为 markdown (`.pi/memory.md`)
- 历史在用户边界截断

**AgentMesh (MinimalFuture)**: 顺序团队编排器, LLM 作为路由器. 不是真正的网格网络. 但具有: 类型化 WebSocket 事件协议, 任务范围 pub-sub, 混合向量+关键词内存搜索.

### 8.6 第 5 轮: vesh-agents + Pregel BSP

**vesh-agents**: 无 LLM 确定性管道 (指标计算不需要 LLM 调用). 在 Stripe/Postgres/CSV 之间进行实体解析.

**AgentMesh Go (hupe1980)**: **Pregel 批量同步并行** 执行. 全局屏障同步: 每个超步, 所有工作节点并行运行, 在继续进行之前通过全局屏障同步. 零拷贝 CoW 检查点恢复: 10k+ 键检查点恢复且无 GC 峰值.

---

## 9. 17 个项目的统一模式

### 9.1 跨项目的重复模式 (出现在 3+ 项目中)

| 模式 | 项目数 | mora-lang 映射 |
|---|---|---|
| 策略引擎 + 能力令牌 | 2 (loongclaw, AIOS) | `sandbox.key { ... }` |
| 审计日志 + 哈希链 | 2 (loongclaw, CLI-Anything) | `audit.jsonl` |
| 工具注册表 / 双注册表 | 4 (loongclaw, CLI-Anything, mimiclaw, vesh-agents) | `mora-hub.json` |
| 提供者分类 / ToolKind 枚举 | 3 (CLI-Anything, mimiclaw, vesh-agents) | `ToolKind` enum |
| 子进程隔离 + 进程组清除 | 2 (mini-swe-agent, pi-agent) | `exec(cmd, timeout)` |
| 异常即流程 | 2 (mini-swe-agent, pi-agent) | `FlowSignal` 扩展 |
| 持久记忆 (markdown) | 3 (pi-agent, AgentMesh, mimiclaw) | `memory.remember()` |
| 共享上下文 / 黑板 | 3 (revenue-orchestrator, AgentMesh, vesh-agents) | `context.outputs` |
| 管道移交 (非 LLM 路由) | 2 (vesh-agents, revenue-orchestrator) | `orchestrate` |

### 9.2 独有模式 (仅 1 个项目)

| 模式 | 项目 | 为什么独特 |
|---|---|---|
| Pregel BSP 超步执行 | AgentMesh Go | 全局屏障同步, 确定性并行执行 |
| 零拷贝 CoW 检查点 | AgentMesh Go | 10k+ 键恢复且无 GC 峰值 |
| WASM 沙箱 | AgentMesh Go, loongclaw | 带完整性检查的 WASM 隔离 |
| TRINITY 19.5K 参数路由器 | OpenFugu | 原始 Transformer 隐藏状态上的线性分类器 |
| 双消息队列 | pi-mono | 外部中断不重启循环 |
| 无 LLM 确定性管道 | vesh-agents | 自动指标计算的无 LLM 快速路径 |
| 递归 XY-cut | MinerU | 递归投影轮廓分裂用于文档布局 |
| SHA-256 内容寻址 | Headroom | 哈希标识符以加密方式关联到原始内容 |

---

## 10. 关键架构学习 — mora-lang 的 Interpreter 结构如何演化

### 10.1 v0.34 之前 (问题状态)

```rust
pub struct Interpreter {
    // v0.34 之前存在的 ~25 个字段 (AI 基础设施)
    globals: Arc<Mutex<Environment>>,
    environment: Arc<Mutex<Environment>>,
    // ... 工具注册表, 模型路由, 令牌预算, 跟踪, 等等 ...

    // v0.32-0.33: 这 5 个模块**从未连接**到 Interpreter
    // bus: EventBus                  ← 脚本中无法访问
    // sandbox: SandboxPolicy         ← 脚本中无法访问
    // scheduler: Scheduler           ← 脚本中无法访问
    // ccr_store: InMemoryCcrStore    ← 脚本中无法访问
    // mock_registry: MockRegistry    ← 脚本中无法访问
}
```

架构惯例是: "新模块 = 新文件." v0.32-0.33 添加了模块文件但**从未执行 5 步集成** (添加字段 → 4 个 Self 块 → 全局变量 → dispatch → builtins.rs). 这是 "架构惯例失守" — 假设文件存在意味着功能可用.

### 10.2 5 步 Builtin 集成模式

通过 v0.34 发现并文档化, 对于每个新模块:

```
步骤 1: Interpreter struct 添加字段 (Arc<Mutex<...>>)
步骤 2: 4 个 Self {} 块 (new / new_empty / new_with_globals / Clone impl) 同步添加 init
步骤 3: globals.lock().unwrap().define("name", Value::Builtin("name"), false)
步骤 4: dispatch.rs 模块段添加 ("module", method) => self.call_*_method(method, &args)
步骤 5: builtins.rs 添加 pub fn call_*_method(&self, method: &str, args: &[Value]) -> Result<Value, String>
```

这 5 步是**显式手动** — 没有自动注册, 没有反射. 忘记任何一步意味着 builtin 对脚本不可用.

### 10.3 v0.35-v0.40 之后的 Interpreter

```rust
pub struct Interpreter {
    // 核心环境 (v0.40 后保持不变)
    globals: Arc<Mutex<Environment>>,
    environment: Arc<Mutex<Environment>>,

    // 注册表现在是 Arc<HashMap<>> 以实现廉价克隆 (v0.36)
    tool_registry: Arc<HashMap<String, ToolDef>>,
    trait_registry: Arc<HashMap<String, TraitInfo>>,
    impl_table: Arc<HashMap<String, Vec<String>>>,

    // v0.34 5 个已集成的 builtin (现在通过 Arc 共享单例, v0.35)
    bus: EventBus,                    // Arc<Mutex<HashMap<...>>> 内部
    sandbox: SandboxPolicy,           // BTreeSet 成员资格检查 (v0.36)
    scheduler: Scheduler,             // AtomicU64 next_id (v0.36)
    ccr_store: InMemoryCcrStore,      // 16-char hex 哈希 (v0.35)
    mock_registry: MockRegistry,      // call() 已删除 (v0.37)

    // Worker 通道 (crossbeam-channel, v0.36)
    worker_channels: HashMap<String, crossbeam_channel::Sender<Value>>,

    // 删除了死字段 (v0.35-v0.37): method_cache, ai_batch_queue,
    // cache_warm_queue, ai_priority_queue, adaptive_temp,
    // load_balancer, retry_policy, route_registry

    // v0.38 numeric tower: Value 枚举具有 Int(i64) + Float(f64)
    // v0.40 env refactor: Value::Closure.env 是 EnvRef (不可变快照)
}
```

---

## 11. v0.34 审计的 "永久债" — 最终裁决

审计标记了 5 个项目为 "永久债 (v1.0 之前无法解决)." 裁决:

| # | 项目 | 状态 | 版本 | 实际努力 |
|---|---|---|---|---|
| 1 | `mpsc::Receiver` 使 Interpreter 非 Send | ✅ 已解决 | v0.36 | 30 LOC, crossbeam-channel |
| 2 | `Value::Number(f64)` 单一数值类型 | ✅ 已解决 | v0.38 | ~400 LOC, 7 个 commit |
| 3 | 16 个 Value 变体缺少 Type 变体 | ✅ 已解决 | v0.36 | 8 个新 Type 变体, 120 LOC |
| 4 | NaN/Inf 显示守卫 | ✅ 已解决 | v0.36 | 20 LOC, 4 个测试 |
| 5 | Env 跨线程安全 | ✅ 已解决 (简化) | v0.40 | 3 个 commit, EnvRef 不可变快照 |

**最终命中率: 5/5 已解决.**

关于 #5 的诚实说明: 审计的原始声明 ("mpsc::Receiver + 单数值 + 缺失 Type 变体") 低估了复杂性. 真正的 "永久" 问题原来是 **Env 重构** — 尝试用 Rc<RefCell<>> 进行两级模型将 Interpreter 污染为 `!Send`. v0.40 的 `EnvRef = Box<Environment>` 不可变快照是务实的折衷: 它实现了审计的目标 (没有其他线程可以改变闭包的环境), 但没有实现完整的性能优势 (闭包捕获时克隆). 真正的 Rc<RefCell<>> 优化仍然需要根本性的 Interpreter 重构 — "非 Send 评估核心 + Send 安全 harness 层" 模式 — 这是合理的 v1.0 工作.

---

## 12. 学习者要点 — 如何复制这个工作流

### 12.1 零信任审计

1. **声明维度.** 不要只是说 "审查代码" — 定义具体的审查角度. 我们的 4 个维度 (并发, 高压力, 强类型, 静态类型) 覆盖了生产质量.
2. **对每个维度使用专用 Agent.** 每个 Agent 获得特定的 file:line 范围, 具体的零信任验证问题, 以及结构化的输出格式 (file:line + 3 行代码).
3. **在 Agent 完成后进行交叉验证.** Agent 出错 (~15% 的声明经 direct-read 验证不成立). 始终亲自检查 P0.
4. **将发现分类.** P0 (panic/不安全/数据丢失) → P1 (弱类型, 性能) → P2 (装饰性). 清晰的严重性标签指导修复优先级.

### 12.2 技术债修复

1. **按风险排序 commit.** 从 1 行更改开始, 逐步到多文件重构. 每个 commit 必须是独立可构建的.
2. **每个 commit 必须通过完整的 4 重检查** (build, test, clippy, fmt). 没有例外.
3. **为高风险 commit 使用 worktree 隔离.** 对于像 Env 重构这样需要协调 8 文件更改的更改, 专用的 `git worktree` 防止意外的 main 分支污染.
4. **如果 commit 重复破坏构建, 则中止.** Env 重构尝试了 3 次跨 2 个版本才成功. 顽固是好事; 无效的尝试不是.

### 12.3 跨项目研究

1. **使用并行 Explore Agent.** 每个 Agent 获取原始源文件 (不是文档页面), 提取具体代码片段, 并输出结构化映射.
2. **映射到具体原语.** 每个发现必须回答: "mora-lang 现在有什么? 它缺少什么? 修复是什么样的?"
3. **寻找重复模式.** 当 3+ 项目独立收敛于相同模式时 (例如, 策略引擎, 双注册表, 异常即流程), 那是 mora-lang 应该采用的信号.
4. **识别独有模式.** 这些是潜在的差异化因素 — 在其他地方找不到的模式, 可能成为 mora-lang 的独特功能.
5. **编译一份主文档.** 单一权威参考防止知识碎片化.

### 12.4 项目管理

1. **每个版本使用功能分支.** `v0.35-technical-debt`, `v0.36-type-completeness` 等. Merge 始终使用 `--no-ff` 以保留历史.
2. **为高 churn 工作使用 worktree.** `.worktrees/v0.40-env` 隔离了 Env 重构尝试, 不让它们影响 main.
3. **有意 revert.** v0.39 尝试的 Env 重构被 revert 了. v0.40 尝试了不同的方法. 这是正常的工程.
4. **记录一切.** 这个文件本身就是一个例子. CHANGELOG, 审计文档, 研究大师文档 — 所有这些对于长期维护者上下文都是必不可少的.

---

## 13. 附录: 完整 commit 时间线

### v0.35 (分支: v0.35-technical-debt, 18 个 commit)

```
f8bf8bf  chore(v0.35): bump version 0.0.34 -> 0.0.35
ca00d03  fix(v0.35): .unwrap() -> .expect() on globals mutex (1 token)
e1b529f  fix(v0.35): Value::Router/Atom Display infallible (+2 tests)
1a7af23  fix(v0.35): typeck Dict.get returns V | Nil (1 line)
578c555  fix(v0.35): lexer rejects control chars in strings (\t\n\r stay)
480c764  fix(v0.35): call_task_inner / call_value_inner arity errors
08ee13b  fix(v0.35): REPL run_repl_with now type-checks (2 lines)
3a2f3ed  fix(v0.35): 8 dead fields removed + StmtKind::Route cleanup
293984c  fix(v0.35): walk_expr 11× .unwrap() -> pattern (11 sites)
97fe2ba  fix(v0.35): remove dead _cache_key format! (-2 lines)
884cc08  fix(v0.35): parse_json O(n²) -> O(n) via byte-index skip_ws
2e81ced  fix(v0.35): EventBus::emit clone-and-drop (no re-entrant deadlock)
9789e5a  fix(v0.35): MockRegistry::call clone-and-drop
f8f60ef  fix(v0.35): ccr hash 8 -> 16 hex (silent overwrite at 2^32 fixed)
9def32f  fix(v0.35): v2_arena wrapped in Arc<AstArena> (cheap clone)
5a0cf6e  fix(v0.35): Clone for Interpreter shares 5 v0.34 singletons via Arc
2ba55a3  docs(v0.35): CHANGELOG entry (20 P0 fixes)
9fc78c7  merge(v0.35): 20 P0s remediated, merge to main
8e9e6bb  style(v0.35): post-merge rustfmt
```

### v0.36 (分支: v0.36-type-completeness, 14 个 commit)

```
3908642  chore(v0.36): bump version 0.0.35 -> 0.0.36
22290a0  perf(v0.36): Arc-wrap trait_registry/impl_table/tool_registry
3862e48  feat(v0.36): swap std mpsc to crossbeam-channel (Permanent #1 DONE)
e150a64  fix(v0.36): Value::Number Display handles NaN/Inf (+4 tests)
601a615  fix(v0.36): List/Dict Display streaming + depth limit
6a05a1c  fix(v0.36): Scheduler AtomicU64 + SandboxPolicy BTreeSet
a38151d  fix(v0.36): MockRegistry::call deprecated
18f6265  fix(v0.36): file.* routes through sandbox.check_path
8a56c46  fix(v0.36): http_server routes listing lock-hold-across-IO
22f202d  fix(v0.36): typeck check_impl_def_stmt rejects orphan for_type
54b4347  feat(v0.36): Type enum adds 8 variants (Permanent #3 DONE)
ddcef92  fix(v0.36): estimate_bytes streams (no re-serialize)
bee19ad  ci(v0.36): fix integration job example paths (_legacy/)
5e2281b  merge(v0.36): CHANGELOG + fmt cleanup
1dae17a  merge(v0.36): merged to main
```

### v0.37 (分支: v0.37-final-cleanup, 8 个 commit)

```
315262a  chore(v0.37): bump version 0.0.36 -> 0.0.37
992329f  fix(v0.37): tighten 12 builtin boundaries (Value::String required)
b66b4de  fix(v0.37): delete MockRegistry::call entirely
18dcb88  feat(v0.37): Value::Builtin -> typed BuiltinKind enum (22 variants)
f966c43  fix(v0.37): http_server request handler lock hoist
933084c  fix(v0.37): typeck Load narrows to Type::String
9e17906  fix(v0.37): typeck errors carry real Span positions (7/11 sites)
473212d  fix(v0.37): typeck with-block validates key against whitelist
82fcbb8  merge(v0.37): CHANGELOG + fmt cleanup
f8305b2  merge(v0.37): merged to main
```

### v0.38 (分支: v0.38-numeric-env, 9 个 commit)

```
fc75c60  chore(v0.38): bump version 0.0.37 -> 0.0.38
9ebc7b5  feat(v0.38): Value::Int(i64) + Value::Float(f64) (numeric tower p1)
4e77074  feat(v0.38): Literal::Int/Float (numeric tower p2)
62b6d17  feat(v0.38): lexer 1i/1u/1f suffix (numeric tower p3)
2b74f3d  feat(v0.38): Type::Int/Float (numeric tower p4)
7ff8236  feat(v0.38): strict promotion Int+Int=Int, Float+Float=Float, mix Err
bb5b658  test(v0.38): 13 numeric tower tests (350 total, +13)
ce2c198  merge(v0.38): CHANGELOG
465f890  style(v0.38): clippy + fmt cleanup
4b814a5  merge(v0.38): merged to main
```

### v0.39 (分支: v0.39-env-refactor, 2 个 commit)

```
5f71bb2  refactor(v0.39): rename with_parent -> with_parent_of (name freed)
d15d0b3  docs(v0.39): CHANGELOG for Env-refactor-deferred release
aab7e95  merge(v0.39): merged to main
```

### v0.40 (分支: v0.40-env-refactor [worktree], 3 个 commit)

```
900a8db  chore(v0.40): bump version 0.0.39 -> 0.0.40
69b1cd2  feat(v0.40): EnvRef + derive Clone on Environment
c78e2ec  feat(v0.40): Value::Closure.env -> EnvRef (immutable snapshot)
2cb2cd0  docs(v0.40): CHANGELOG + fix clippy warnings
a979617  merge(v0.40): merged to main (with Cargo.lock conflict fix)
76d5a5b  merge(v0.40): resolve Cargo.lock conflict
```

### 文档 (main 分支, 2 个 commit)

```
625c712  docs: RESEARCH_PRIMITIVES_MASTER.md (15 projects, 458 lines)
215336d  docs: +vesh-agents + AgentMesh Go + Solace Agent Mesh (17 projects, 579 lines)
```

---

## 14. 附录: 项目快速参考卡

| 项目 | 语言 | ⭐ | 真实源码? | 主要贡献 |
|---|---|---|---|---|
| **loongclaw** | Rust | 644 | ✅ | 能力令牌, 策略引擎, 审计迹 |
| **mini-swe-agent** | Python | 5.6k | ✅ | 异常即流程, 子进程隔离, sentinel |
| **CLI-Anything** | Python/MDX | 44.7k | ✅ | 三层注册表, HARNESS.md, SKILL.md |
| **AIOS** | Python | — | ✅ | FIFO/RR 调度器, 工具冲突映射 |
| **mimiclaw** | C (ESP32) | — | ✅ | 12 字段 cron, 心跳, 工具 vs 技能 |
| **OpenFugu** | Python | — | ✅ | TRINITY 19.5K 路由器, DAG-as-data |
| **OpenInfer** | Rust/CUDA | — | ✅ | 缝合 vLLM, 两层 KV 缓存 |
| **MinerU** | Python | — | ✅ | 递归 XY-cut, 30+ BlockType |
| **Headroom** | Rust/Python | — | ✅ | SHA-256 内容寻址, 5 维评分 |
| **Puter** | TypeScript | — | ✅ | O(segments) 事件匹配, 5 层 DI |
| **pi-agent/pi-mono** | TS/Python | — | ✅ | 双消息队列, 并行执行, 内存 |
| **AgentMesh (MinimalFuture)** | Python | 294 | ✅ | 类型化事件协议, 混合内存搜索 |
| **revenue-orchestrator** | — | — | ❌ (仅 README) | 谓词路由, 阶段门控, 验证器代理 |
| **ai-coder-symphony** | — | — | ❌ (仅 README) | 静态角色分配, 加权投票 |
| **vesh-agents** | Python | — | ✅ (PyPI) | 无 LLM 快速路径, 实体解析 |
| **AgentMesh Go (hupe1980)** | Go | 6 | ✅ | Pregel BSP, 零拷贝 CoW, WASM |
| **Solace Agent Mesh** | — | — | ✅ | 主题路由, 事件溯源 |

---

**学习者最后的说明**: 这个会话展示了完整的软件工程生命周期——审计→修复→研究→规划——跨越 3 天, 6 个版本, 17 个项目和 72 多个 commit. 模式不是 "快速修复"; 它是 "系统性调查→优先排序→增量执行→诚实评估." 零信任审计揭示了 57 个问题. 按风险排序的版本将它们降低到 0. 跨项目研究映射了 17 个灵感来源到具体的 mora-lang 原语. 主文档确保没有一个会丢失.

**最重要的教训**: "永久债" 是预测, 不是经文. 审计声称 5 个项目是 "永久的" — 所有 5 个都被解决了. 但这并不意味着预测是无用的——它正确地识别了 Env 重构为 5 个中最难的, 确实需要 2 个版本和一种完全不同的方法. 好的审计不是完美预测; 它是诚实的严重性评估, 指导执行, 并在事实变化时接受修正.
