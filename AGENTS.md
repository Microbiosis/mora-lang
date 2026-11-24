# Agent Rules for mora-lang

本文件约束所有 AI 编码助手在此仓库中的工作方式。

> ## 0. 最高优先级原则：架构优先 + 完整实现（v0.78 用户硬性约束）
>
> **这是整个项目的第一性原则，凌驾于其他所有规则之上。**
>
> ### 0.1 不要因噎废食，不能做保守派
>
> 实现一个功能**必须**完整实现它。任何「最小可用」「先做能跑的版本」「简易版」都属于补丁化妥协，**禁止**。
>
> - 禁止用「未来扩展性」做借口回退到「最简实现」
> - 禁止「先做能跑的部分」分阶段交付一个不完整的功能
> - 禁止「功能先这样，refactor later」—— 此刻不做，将来永不会做
> - 禁止「向后兼容优先」压在「架构正确」之上
>
> ### 0.2 实现功能必须从底层架构开始构建地基
>
> 当我们决定实现一个功能（algebraic effects / row-polymorphic types / Control SSA IR / Go-style concurrency / tensor compute graphs / 等），必须先回答：
>
> 1. **这个功能的底层原语是什么**？（如 algebraic effects 的原语是 perform/handle + effect rows + multi-shot delimited continuation frame）
> 2. **它的类型基础是什么**？（如 EffectRow enum、Type::Arrow、ControlInst::Perform / ControlInst::Spawn / ControlInst::Select）
> 3. **它对现有架构的最小侵入点是什么**？（如 ControlProgram 指令与验证器、typeck::hm 约束求解、runtime::vm 堆帧状态机）
> 4. **它的完整语义需要哪些配套**？（如 EffectRow 包含检查、ML Value Restriction、TaskScheduler / AsyncIoReactor 协作取消）
>
> 把这些底层原语 + 类型基础 + 配套**全部**先建好，再写语法糖。**不能**先写一个 syntax sugar 然后说「底层以后再说」—— 「以后再说」就是补丁。
>
> ### 0.3 最小实现 = 补丁 = 因小失大 = 违反项目约束
>
> 任何「最小实现」实际意味着：
>
> - 引入**隐式语义**（用户看不见的副作用）—— 违反 §6 透明性
> - 制造**未来技术债**（需要二次重构）—— 违反 §6 扩展成本最小化
> - 与**项目定位脱节**（Mora 的核心定位是 **AI-native 静态类型语言**，面向大模型底层、计算图与算子、大模型推理/训练与资源动态调控，同时具备 Go 式云原生并发属性并对 HTTP 与 MCP 服务提供语言原语级友好支持；纯字面量语法糖做不到）—— 违反 §6 架构正确性
> - 让**测试失去验证意义**（没有类型基础的实现只能 smoke test）—— 违反 §3 测试同步原则
>
> ### 0.4 决策矩阵：当「保守」vs「完整」冲突时
>
> | 维度 | 保守（禁止） | 完整（要求） |
> |------|-------------|--------------|
> | 工作量 | 「改动小」 | 「长远正确」 |
> | 风险 | 「不破坏现有」 | 「必须修对」 |
> | 时间 | 「先实现，再说」 | 「地基优先」 |
> | 兼容 | 「保留旧的兼容层」 | 「直接迁移到正确设计」 |
> | 测试 | 「skip 一些边角」 | 「类型系统强制的全测试」 |
> | 重构 | 「以后再说」 | 「现在做对」 |
>
> ### 0.5 实施前自检清单
>
> 实现任何功能前，回答以下问题，**全数回答"是"才能动手**：
>
> - [ ] 是否能从底层原语（types/IR/traits）推导完整语义？
> - [ ] 是否能在不做兼容层的前提下迁移现有调用点？
> - [ ] 是否覆盖了完整的状态（包括错误路径、并发、原子性）？
> - [ ] 是否与项目已承诺的范式（functional/AI-native）保持一致？
> - [ ] 是否提供了类型系统级别的保证（而非运行期 assert）？
> - [ ] 是否避免了引入"未来可重做"的钩子？
>
> 任何一项回答"否"，必须先回答"否"的具体原因，再决定是否能进行。
>
> ### 0.6 上下文实例（v0.78+ ~ v0.110.0 起强制）
>
> - **Wave 1（架构解耦与编译器瘦身）**：核心只保留语言原语与 7 个基础内建模块（`json`/`file`/`exec`/`math`/`stats`/`linalg`/`random`），上层应用耦合构造彻底迁出，`BigInt` 独立至纯 Mora 标准库 `std/math/bigint.mora`（v0.110.0 起 Rust 侧不再有 BigInt 类型）。
> - **Wave 2（单一 Control SSA 管线、堆帧 VM 与 Go 式云原生并发）**：不做双轨兼容桥，直接收口至 `MirWitness[] → ControlProgram` 单遍 SSA IR、堆帧状态机 VM（`O(1)` 尾调用 + 多发定界续延 `Value::Continuation`）、自研零依赖 `TaskScheduler` / `AsyncIoReactor`、Go 式 `spawn` / 不变 `channel<T>` / 公平 `select` / `context` / `sleep`、非阻塞异步 TCP（`tcp_listen` / `tcp_connect`）与 `FsPolicy` / `CheckedPath` 沙箱隔离。
> - **Wave 3（精确数值、统一 HM 求解器、一等 Tensor、5 项静态检查与模型语法）**：Stage 3A 严格分离 `Int`/`Float` 字面量与低位宽后缀（`f16`/`bf16`/`fp8`/`i4`..`i64`）、`**`/`@`/`!` 及单一 `Span` 追踪 HM 约束求解器（`check_program_witnesses`）；Stage 3B 构建一等 64B 对齐 `Tensor<DType, Shape, Device>` 与 5 项静态检查（DType/Shape/Device/Resource/Effect）；Stage 3C 落地名义 `struct`/`enum`/`@tag`、源码级 `trait`/`impl`/`dyn Trait`、具名与默认参数、零拷贝多维切片及反向模式 `grad`/`value_and_grad`/`no_grad`。
> - **Wave 4（AI 原生计算图、硬件 GPU 后端、动态显存调控与云原生标准库闭环）**：原生 `TensorComputeDag`（`trace_dag` 4 遍算子融合 `FusedFlashAttention`/`FusedRmsNormLinear`/`FusedLinearAct`/`FusedElementwise`、`DagMemoryPlan`、`DagPlanCache`）、前向 `jvp` 与多磁带任意阶微分 `grad^n` & HVP、零依赖动态加载 CUDA PTX / Vulkan SPIR-V / Metal MSL / CPU AVX2+NEON 硬件后端、`KVPageAllocator` 与 64B Slab `MemoryPool` 动态资源调控、零源码 `MORABC02` 预编译字节码 `mora build` 单二进制与 `.morac` 直跑，以及纯 Mora `std/net/http`（含 Router/Chunked/Keep-Alive/413/SSE）与 `std/mcp` 标准库。
> - **Phase P3 & P4（`v0.110.1` ~ `v0.111.0`，0 外部依赖全栈深化）**：P3 落地持久化 `DeviceVramBuffer` VRAM 常驻与原生 `f16`/`bf16` 全 5 核零回退、纯 Mora `std/nn/{safetensors,gguf}` 权重标准库、零依赖原生异步 TLS 1.2/1.3（Schannel/OpenSSL）与 `mora fmt`/`mora test` 工具链；P4 落地 `struct`/`enum`/`impl` 编译期 Const Generics 与 9 运算符 trait 重载、源码级 GPU `kernel` 块（PTX/SPIR-V/MSL/CPU）、16B 未装箱 `RegSlot` 与 `CapturedFrames` M:N 协作式纤程挂起、`DagCostModel` + DType 安全代数优化 + 零依赖严格 W^X x86-64 AVX2/SSE2 原生 JIT、分布式 `ProcessGroup`（进程内保 VRAM + `AsyncIoReactor` TCP Ring）与纳秒级 `TraceProfiler` + 纯 Mora `std/trace.mora`。
> - **Phase P5（`v0.112.0`，0 外部依赖生产级端到端 LLM 栈）**：P5-1A 完成全仓 `MoraResult<T>` 统一、静态资源配额、统一环境 `EffectRow`、纯 Mora `std/json` 与 Go-1.22 式循环每轮独立闭包捕获（`FreshLocalCell`）；P5-1B 落地 GPU `kernel` v2（`shared` 共享内存、`sync_threads()`、2D/3D 网格、`buf<f16|bf16|u8|i32>` 与位运算，PTX/SPIR-V/MSL/CPU 四端对等）；P5-2 落地内容寻址 RadixAttention 前缀树 + GPU KV swap-out/swap-in 及严格 W^X x86-64 AVX2+FMA `FusedLinearAct` 寄存器分块 GEMM JIT；P5-3 落地 2 阶段带宽最优 TCP Ring AllReduce、P2P `send`/`recv`、2D `TP × DP` `split` 与发射保序 `all_reduce_async`；P5-4/P5-5 落地递归 PyTree `grad`/`value_and_grad`、词法模块导入作用域、纯 Mora `std/nn/{layers,optim,tokenizer,serving}` 全栈 LLM 库、`mora profile`/`mora bench` CLI 及旗舰端到端示例 `examples/llm_train_and_serve_e2e.mora`。
>
> ### 0.7 违反本原则的处理
>
> 如果 AI 助手提交了「最小实现」型 PR，**用户会拒绝 review**。这不是
> 用户偏好，是项目定位本身决定的：Mora 是 **AI-native 静态类型语言**（面向大模型底层、计算图与算子、大模型推理/训练与资源动态调控，兼具 Go 式云原生并发与 HTTP/MCP 原语友好支持），
> 这个定位的工程现实是**类型系统与 SSA 编译器/运行时内核强制**承诺的实现完整度——任何妥协
> 都是对项目定位的背叛。
>
> ---
>
> 后续 §1~§7 是这一原则在不同时期的具体规则化展开。**当 §1~§7 与 §0 冲突时，以 §0 为准。**

## 1. 信息来源：默认走 MCP 搜索，训练数据仅作辅助

> **核心规则**：凡是涉及**外部知识**的信息，必须通过 MCP 搜索工具获取，不要仅依赖训练数据。

### 1.1 适用范围

以下场景**必须**先调用 MCP 搜索：

- 任何第三方库的 API、配置、版本兼容性（如未来引入的 `serde` / `tokio` 等）
- 任何外部协议/规范的最新动态（如 MCP 协议、HTTP 标准、语言标准库）
- 当前日期附近的发布说明、breaking change、CVE
- 官方推荐的 best practice、迁移指南
- 用户给出的链接之外、需要补全的"周边"知识

### 1.2 例外（可直接用训练数据）

- Mora 语言自身的语法、语义、执行管线 —— 直接读 `src/` 与 `docs/`（**项目无 AST**，v0.55 去 AST 化后已无 AST 解释器；v0.110.0 Wave 1/2 已移除旧 `src/interpreter/`、9 层 FCFG/EHIR/Core/DAG/JIT 与 HAMT `value/persistent.rs`）
  - **单一生产主路径（单遍编译 + Control SSA IR）**：`source → Lexer::scan_tokens → ParserV3::compile (parse_witnesses → MirWitness[] + mir::control::lower_program → ControlProgram SSA IR + mir::control::verify_program) → typeck::check_mir::check_program_witnesses (统一 Span 追踪 HM 约束求解器 + 跨模块 import 符号校验) → mir::control::optimize_program (OptLevel 0/1/2) → runtime::vm::execute_program`
  - **单一执行内核与并发底座**：`src/runtime/vm/`（堆帧状态机 VM，`Vec<CallFrame>`，`O(1)` 尾调用帧复用 + 多发定界续延 `Value::Continuation`）+ `src/runtime/{core,scheduler,reactor,fs}.rs`（`CoreRuntime` + 进程级共享 `EngineShared { scheduler: TaskScheduler, reactor: AsyncIoReactor }`，支持 Go 式 `spawn` / 不变 `channel<T>` / 公平 `select` / 分层取消 `context` / 可取消 `sleep`、`AsyncTcpListener` 以及 `FsPolicy` / `CheckedPath` 文件系统边界隔离），**没有第二条编译或解释路径**
  - **测试临时目录的唯一入口**：`src/test_support.rs` 的 `TempDir` / `TempFile`（v0.104.6 D475）。根目录、命名、回收策略都只此一处；**别处不得直接调 `std::env::temp_dir()`** —— `tests/test_tempdir_hygiene.rs` 守卫会判失败。此前全仓 163 处散落写法（5+ 种命名、4 种回收策略）在 `%TEMP%` 累积了 9 205 个空目录。
- 项目内部命名、模块结构、历史 commit 含义 —— 直接 `grep` / `git log`
- 通用编程语言基础（Rust 语法、Python 语法等）

### 1.3 可用工具

本仓库默认使用 **`mcp__anysearch__*` 系列** 作为 MCP 搜索入口：

- `mcp__anysearch__search` — 单次搜索，可指定领域（`general` / `code` / `academic` 等）
- `mcp__anysearch__batch_search` — 多 query 并发搜索
- `mcp__anysearch__extract` — 抓取指定 URL 内容
- `mcp__anysearch__get_sub_domains` — 查询垂直子域枚举

涉及代码/API 调研时优先 `domain="code"`；通用事实用默认。

### 1.4 工作流

1. **先识别**：判断问题是否需要"外部最新事实"。是 → 进入第 2 步；否 → 直接答/读代码。
2. **先搜后答**：在写出结论、代码、命令前，至少完成一次 MCP 搜索；如搜索结果与训练数据冲突，**以搜索为准**，并在答复里标注来源。
3. **不确定就再搜**：对版本号、参数名、错误码、API 形态拿不准时，宁可多搜一次，不要凭印象写。
4. **结果引用**：在最终答复里给出关键结论对应的来源 URL，便于人工复核。

## 2. 其它通用约定

- 改代码前先 `grep` 现有实现，避免重复造轮子。
- 涉及 Rust 依赖变更，优先搜索 crate 最新稳定版本和迁移说明，再改 `Cargo.toml`。
- 提交信息遵循仓库现有风格（参考近期 `git log`）。

## 3. 代码质量

- **clippy**: 所有代码必须通过 `cargo clippy --all-targets --all-features -- -D warnings`
- **测试**: 新功能必须有对应测试，测试数量只增不减
- **测试同步原则**: 任何代码修改，若导致现有测试失败、或使测试失去验证意义，必须在同一提交内完成测试更新。禁止"先改代码、后补测试"的分离操作。
  - 新增功能：同步补充覆盖该功能的测试用例。
  - 修改行为：同步更新受影响的测试断言与预期输出。
  - 删除代码：同步删除或替换对应的测试。
  - 重构代码：确保测试在重构前后行为一致；若测试本身耦合实现细节，一并重构测试。
  - 判定标准：如果 reviewer 能在不更新任何测试的情况下合并你的 PR，说明测试覆盖不足，或本次修改未触及测试——两者都需警惕。
- **unwrap**: 生产代码中避免 `unwrap()`，使用 `expect("有意义的错误信息")`
- **模块化**: 大文件应拆分为多个模块 (value.rs, flow.rs 等)

## 4. 语言设计参考

- **语言规范**: `docs/mora-spec.md` (20 章)
- **影响分析**: `docs/influences.md` (9 语言)
- **学习计划**: `docs/learning-plan.md` (6 阶段)
- **变更日志**: `CHANGELOG.md`

## 5. 版本管理

- 版本号遵循语义化版本 (Semantic Versioning)
- v0.x 阶段可能有 breaking change
- 每个版本的变更必须记录在 CHANGELOG.md

## 6. 最小修改原则（元原则）

> **本节是 §0 的具体化展开。** 当 §0 与本节冲突时，**以 §0 为准**。

角色定位：你是一位资深软件架构师与代码审查专家。在提出任何代码修改建议时，你必须遵循「最小修改原则」。

### 核心定义

「最小修改」并非指当前代码变更的行数最少、文件最少或表面改动最小，而是指：本次修改对项目未来可维护性、扩展性与问题排查造成的长期影响范围最小化。

### 关键约束：不要行使保守政策

**不要因保守策略而阻碍正确修改。** 本项目不接受「为了兼容旧代码而保留错误设计」的做法。若接口、数据结构或行为语义存在设计缺陷，修改方案应当直接修正至正确状态，而非叠加兼容层、弃用标记或适配代码。所有不兼容变更必须通过版本化机制（如语义化版本号、迁移文档、CHANGELOG）进行显式管理，而非通过代码层面的兼容逻辑进行隐式掩盖。

### 判断标准

在评估修改方案时，按以下优先级排序：

1. **架构正确性优先**：若现有设计存在根本性缺陷（如错误的抽象层级、隐式耦合、违背单一职责），必须直接重构至正确结构，不允许以「改动小」为由在错误架构上打补丁。
2. **未来可维护性优先**：优先选择使后续开发者（包括你自己）最容易理解、测试和调试的方案，而非当前实现最简短的方案。
3. **扩展成本最小化**：修改不应引入隐含的依赖耦合或技术债。评估标准：若未来需求变更 10%，本次修改是否会导致 50% 的代码需要联动调整？
4. **问题隔离性**：修改应当局限在明确的模块边界内，避免产生跨模块的副作用。新增问题应能被快速定位到本次修改的范围内。
5. **回滚友好度**：修改方案应当支持在出现问题时，能够以最小成本回滚或切换实现，而不引发级联故障。回滚依赖版本控制，而非代码层面的开关逻辑。
6. **测试同步更新**：任何修改若导致既有测试失败，必须同步更新测试代码以反映新的正确行为。不允许通过「兼容旧测试」来掩盖语义变更。测试是修改正确性的唯一可信凭证。

### 禁止行为

- ❌ 仅以「改动少」为由选择 hack、硬编码或绕过现有架构的捷径。
- ❌ 引入隐式全局状态、魔术数字或未经文档说明的副作用。
- ❌ 为节省当前工时，将复杂度转移给未来的维护者。
- ❌ 为维持向后兼容，在错误接口上叠加适配层、弃用装饰器或兼容分支。
- ❌ 修改行为语义后，不更新对应测试，或让测试继续断言旧行为。

### 输出要求

当你建议修改时，必须简要说明：

1. 当前方案对未来维护的潜在风险；
2. 你建议的方案如何降低长期影响；
3. 本次修改是否涉及不兼容变更；若是，必须提供显式迁移路径（如版本升级说明、API 变更清单），而非代码兼容层；
4. 测试更新的具体范围与验证方式；
5. 若存在权衡，明确指出现阶段多付出的成本与未来节省的成本。
## 7. 概念分类准则：名字是检索线索，不是类型判定（v0.75.31 教训）

> 来源：v0.75.31 讨论「两个 env」（进程环境变量 `std::env` vs 运行时
> `crate::value::Environment`）。当时的分析框架按「叫什么」把两个标识符
> 归为一类做混淆分析——但按「做什么」它们四条维度（作用域/生命周期/
> 类型/访问面）没有一条相同。混淆是叙述的产物，不是代码的事实。

在分析、归类、重构任何概念时：

1. **按「做什么」分类，不按「叫什么」**：两个标识符共享一个词（如
   "env"）不构成同类。判定必须依据：作用域、生命周期、类型、访问面、
   数据流是否相交。
2. **全限定名是事实，短名是检索线索**：`std::env::var` 和
   `crate::value::Environment` 在代码里从不碰撞；混淆只存在于省略限定名
   的叙述里。分析时始终带限定名。
3. **叙述不得制造代码中不存在的分类**：若我提出的「同类对比」无法在
   代码中找到共享的实际交互（数据流、类型关系、调用链），则该对比是
   我叙述的产物，应撤销而非展开。
4. **涉及「X 与 Y 是不是一回事」的论断**：必须先给出 grep/调用链证据
   证明二者有真实交互，再下结论；无证据则结论为「命名巧合，非同源」。

### 验证方式

- 每次做概念归类或「是否同类」判断时，输出必须包含证据（grep 行号 /
  数据流 / 调用链），或明确标注「命名巧合，非同源」。
- 若发现前一分析犯了「按名字归类」的错误，修正结论 + 更新本节（本节
  本身允许被更准确的教训修订）。
---

## Setup commands

- Install deps: `cargo build`
- Start dev:    `cargo run -- <script.mora>`
- Build:        `cargo build --release` (produces `mora` and `mora-lsp` binaries)
- Test:         `cargo test --all-targets`
- Lint:         `cargo clippy --all-targets --all-features -- -D warnings`
- Fmt:          `cargo fmt --all -- --check` (CI-enforced)
- Typecheck:    `cargo check --all-targets`

## Project layout

- `src/` — core compiler, unified HM type checker (Const Generics + 9 operator traits + 5 static checks + PyTree AD), Control SSA IR (`MORABC02` codec + `FreshLocalCell`), 16B unboxed `RegSlot` M:N stackful VM (`CapturedFrames` + lexical `ModuleGlobalsInner` imports), `TensorComputeDag` (`DagCostModel` + algebraic opt + strict W^X x86-64 AVX2+FMA `native_jit` for elementwise & `FusedLinearAct` GEMM), VRAM-resident CUDA/Vulkan/Metal/SIMD backends (`f64`/`f32`/`f16`/`bf16`) + source-level GPU `kernel` v2 compiler (`kernel_compiler`), physical-VRAM RadixAttention `KVPageAllocator` (prefix trie + swap) & `MemoryPool`, distributed `ProcessGroup` v2 (2-phase Ring AllReduce + P2P + 2D `split` + `all_reduce_async`, `distributed.rs`), nanosecond `TraceProfiler` (`profiler.rs`), zero-dependency native scheduler/reactor/TLS (`runtime/tls`), CLI toolchain (`mora fmt`, `mora test`, `mora profile`, `mora bench`, `mora build`), and binaries (`mora`, `mora-lsp`)
- `std/` — pure-Mora standard libraries (`std/nn/{layers,optim,tokenizer,serving,safetensors,gguf}.mora`, `std/json.mora`, `std/math/bigint.mora`, `std/testing.mora`, `std/net/tls.mora`, `std/net/http.mora` with HTTPS/Router/Chunked/Keep-Alive/SSE + `open_sse_post`, `std/mcp.mora` with Tools/Resources/Prompts/`Struct::schema()`/HTTPS/Stdio, and `std/trace.mora` with `trace_http_middleware()` & Chrome Trace JSON), embedded in the binary via `mora::resolve_std_module`
- `tests/` — integration tests + property-based tests (self-hosted `std/testing.mora` generator)
- `examples/` — runnable Mora demo scripts (`hm_basic_demo.mora`, `ai_compute_dag_and_autograd.mora`, `ml_weights_safetensors_gguf.mora`, `cloud_http_https_mcp.mora`, `llm_train_and_serve_e2e.mora`; verified by `tests/p3_cli_fmt_test_and_examples.rs`)
- `docs/` — language spec, influences, learning plan, architecture diagram
- `scripts/` — repo utility scripts (PowerShell helpers)
- `editors/` — editor integrations (`vscode/`, `neovim/`, `helix/`, `sublime/`, `vim/`, `emacs/`)
- `vendor/` — vendored code / sample input
- `test_data/` — test fixtures

## Code style

- Rust edition 2024, MSRV 1.85
- Default `rustfmt` (no `.rustfmt.toml` checked in)
- Clippy: `cargo clippy --all-targets --all-features -- -D warnings` (CI-enforced, treat warnings as errors)
- Run `cargo fmt --all` before committing
- Prefer `expect("meaningful message")` over `unwrap()` in production paths

### Toolchain policy

- The project builds on the **Rust stable release channel only** — no nightly lane.
- v0.104.6 dropped the `nightly` entry that used to sit in the `ci.yml` test matrix.
  Reason (measured, not assumed): chasing nightly-only lints buys no coverage here, only per-release churn.
- No `rust-toolchain.toml` and no `rust-version` in `Cargo.toml`: the toolchain floats with
  `stable` by design, matching CI's `dtolnay/rust-toolchain@stable`.

## Testing instructions

- Unit + integration tests: `cargo test --all-targets` (CI matrix: Ubuntu / Windows / macOS, stable only)
- Library tests only: `cargo test --lib`
- Pure-Mora std library: exercised through the embedded modules (`cargo test --test std_math_bigint_and_testing`)
- Property-based tests: self-hosted generator in `std/testing.mora` driven by the deterministic SplitMix64 PRNG (see `tests/proptest_*.rs`)
- LSP smoke: `cargo run --release --example lsp_smoke -- ./target/release/mora-lsp`
- Add tests for every new behavior — see existing `tests/*.rs` files for the pattern
- All tests must pass before opening a PR

## Security

- License: BSD-3-Clause (`LICENSE` in repo root)
- Never commit secrets — `.env` and tool caches are in `.gitignore`
- `AGENTS.md` is intentionally tracked as the single project-level AI rules source of truth