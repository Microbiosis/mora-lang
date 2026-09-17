//! mora Intermediate Representation (MIR) — α.0
//!
//! 寄存器式线性指令序列。AST → MIR lowering 产物，也是 MIR 解释器执行格式。
//! SSA 构造 pass（MIR-plain → MIR-ssa）在 α.3 加入，此处先只有 MIR-plain。
//!
//! α.0 覆盖范围：Const / Var / Copy / BinaryOp / Call / Define / Assign /
//! IndexAssign / Expr / Label / Jump / JumpIf / JumpIfNot / Return / Break /
//! Continue / ListLit / DictLit / Index / MethodCall / Pipe / Prompt /
//! MatchArm / TaskDef / ToolDef / Import / WithConfig
//! （StreamFor 已于 v0.75.26 删除，见下）
//!
//! v0.75.79: 新增 Copy(dst, src) 纯寄存器拷贝 — if 表达式结果寄存器化
//! （不再经 env 临时名 `__if_result` 传递）。
//!
//! v0.75.81: Transaction / Rollback / Commit 经事务块前端激活（spec 9.3）；
//! Eval（断言原语）经 eval 语句前端激活。RecordTokens / Route 已删除 —
//! token 记录由 TraceCollector / AiRuntime 承担，路由由 Value::Router 承担。
//!
//! v0.75.26: StreamFor 已删除 — 死原语（零构造点、零测试引用、语义被
//! ai.chat 的 stream:true 参数路径取代；handler 空转：prompt_reg/var 被忽略、
//! body 仅执行一次并丢弃）。流式语义若需 MIR 指令级支持，重新设计而非复活旧形状。

use crate::common::BinaryOp;
use crate::value::Value;

pub mod cache;
// v0.89: CMIR — 9 层架构第 5 层（并发降维：BSP/Agent/SIMD）
pub mod cmir;
// v0.89: Core → CMIR 桥接
pub mod core_to_cmir;
// v0.89: CMIR → LMIR 桥接
pub mod cmir_to_lmir;
// v0.89: Core IR — 9 层架构第 3 层（函数式基元指令集，SSA 100% 覆盖）
pub mod core;
// v0.78: EffectRow — algebraic effect 的类型表示（Stage 1/4 落地）
pub mod effect;
// v0.92: expr 模块已删除 —— MirExpr 平行 AST 世界完全消除（P0.3）。
// 全部调用方迁移到 `mir::witness::MirWitness` / `mir::orchestrate::*`。
// v0.91: orchestrate runtime types — 从 expr 层迁出，独立模块。
pub mod orchestrate;
// v0.89: EHIR → Core 桥接
pub mod ehir_to_core;
// v0.89: FCFG — 9 层架构第 1 层（Node<M> 泛型 + 结构化 CFG）
pub mod fcfg;
// v0.89: FCFG → MIR 桥接层（Node<()> 降维为 MirInst 线性序列）
pub mod fcfg_lower;
pub mod hint;
pub mod host;
pub mod jit;
// v0.89: LMIR — 9 层架构第 6 层（内存布局降维：Unbox/Alloc/GC/FFI）
pub mod lmir;
// v0.89: LMIR → RIR 连接（LayoutTable 填充 + Unbox 接入）
pub mod lmir_to_rir;
pub mod lower;
// v0.90: witness → FCFG 转换器（9 层切换的核心：FCFG 生产者）
pub mod witness_to_fcfg;
// v0.90: 9 层管线驱动 + 差分验证
pub mod opt;
pub mod optimize;
pub mod pipeline;
// v0.89: RIR/JIR — 9 层架构第 7-8 层（运行时 + JIT 接口）
pub mod rir;
pub mod ssa;
// v0.75.38: MirWitness 轻量树骨架（typeck/LSP 消费面，去 AST 化中间层）
pub mod witness;

// v0.59: DAG IR — dataflow analysis from linear MIR
pub mod dag;
pub mod handlers;
mod inst; // v0.75.56: MirInst metadata + dispatch（经 handlers::inst re-export）
pub use inst::*; // 保持 crate::mir::dst() 等旧路径
pub mod vm;

pub use vm::run_mir;
// lower_program removed in Phase A (v0.55) — use lower_mir_witnesses instead
// v0.92: `pub use expr::MirExpr` 已删除 —— expr 模块整体移除（P0.3）。

// ── 9 层 IR 架构 re-exports ──
pub use cmir::{CmirBlock, CmirNode, ConcurrencyMode};
pub use core::{CoreBlock, CoreFunction, CoreInst, CoreTerminator};
pub use fcfg::{Block as FcfgBlock, Ehir, Fcfg, Node, TypeInfo};
pub use fcfg_lower::lower_fcfg;
pub use lmir::{LmirInst, LmirType, MemLayout};
pub use rir::{
    CompiledFunction, FunctionVersionTable, JitBackend, JitEntry, JitError, LayoutTable,
    LegacyJitBackend,
};

/// 虚拟寄存器索引（无限数量，lowering 时计数器分配）
pub type Reg = usize;

/// 跳转目标（body 中的指令索引）
pub type Label = usize;

/// 一个 MIR 函数 = 一段脚本或一个 task body
#[derive(Debug, Clone, PartialEq)]
pub struct MirFunction {
    pub params: Vec<String>,
    pub body: Vec<MirInst>,
    pub n_regs: usize,
    /// v0.78: 累积的 effect row。Empty = pure (backward-compatible default)。
    /// lowering 时由 mir/lower.rs::MirExprLowerer::classify_call_effect 累积。
    /// 阶段 2 引入 Type::Arrow 时，本字段与 HM 类型系统对接。
    pub effects: effect::EffectRow,
}

impl Default for MirFunction {
    #[allow(clippy::derivable_impls)]
    fn default() -> Self {
        Self {
            params: Vec::new(),
            body: Vec::new(),
            n_regs: 0,
            effects: effect::EffectRow::default(),
        }
    }
}

/// match 表达式的单个 arm（`MirInst::MatchExpr` 的元素类型）。
///
/// v0.104.3: 守卫由 `Option<Reg>` 改为 `Option<Box<MirFunction>>` —— 守卫
/// 引用**模式绑定变量**，而绑定只在匹配成功时才进入 env，故守卫必须像
/// arm body 一样**延迟求值**（`h_match_expr` 在绑定之后调用它）。
pub type MatchArmInst = (String, Option<Box<MirFunction>>, Box<MirFunction>, Reg);

/// MIR 指令（α.0 + α.1 子集）
// 允许 large_enum_variant：ImplDef / SkillDef 携带完整函数体（Vec<MirFunction> /
// Option<MirFunction>），属于 IR 设计，改 Box 需大面积改构造/匹配，收益不高。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum MirInst {
    // ── 值指令（产生结果到 dst 寄存器）──
    Const(Reg, Value),
    Var(Reg, String),
    /// v0.75.79: 寄存器拷贝 dst = regs[src]（纯计算，零 env 访问）。
    /// 表达式合并结果用：if/match 的分支值经 Copy 直写公共 dst，
    /// 不再经 env 临时名（`__if_result`）传递 —— Assign 写未定义变量
    /// 静默失败（env.assign 找不到绑定返回 false）导致分支值丢失。
    Copy(Reg, Reg),
    BinaryOp(Reg, Reg, BinaryOp, Reg),
    /// 函数调用。callee 是名字（ExprKind::Call 的 callee 是 String），非寄存器
    Call(Reg, String, Vec<Reg>),
    /// α.1: 列表字面量 [r0, r1, ...]
    ListLit(Reg, Vec<Reg>),
    /// α.1: 字典字面量 {key: val, ...}（key 是 String，val 是 Reg）
    DictLit(Reg, Vec<(String, Reg)>),
    /// α.1: 索引 obj[idx] → dst
    Index(Reg, Reg, Reg),
    /// α.1: 索引赋值 obj[idx] = val（返回赋值结果）
    IndexAssign(Reg, Reg, Reg),
    /// α.1: 方法调用 recv.method(args) → dst
    MethodCall(Reg, Reg, String, Vec<Reg>),
    /// α.1: 管道 lhs |> callee → dst（callee 是 reg 里的可调用值）
    Pipe(Reg, Reg, Reg),
    /// α.1: p"..." 模板拼接（不触发 AI，只拼接 parts 的字符串形式）
    Prompt(Reg, Vec<Reg>),
    /// α.0: 模式匹配表达式。arms 依次尝试，命中第一个即返回 arm_val。
    ///
    /// v0.104.3: 守卫由 **`Option<Reg>` 改为 `Option<Box<MirFunction>>`**。
    ///
    /// **缺陷背景（守卫恒为假、`when` 完全失效）**：守卫表达式此前在外层
    /// 寄存器空间求值，产物是「求值时读到的寄存器」。但守卫通常引用**模式
    /// 绑定变量**（`x when x > 0`），而绑定发生在 `h_match_expr` 匹配成功
    /// **之时**（`self_match_pattern` 把 `val` define 进 env）—— 外层求值
    /// 时该变量尚不存在，读到 `Nil` → `is_truthy(Nil) == false` → **该 arm
    /// 恒被跳过**（实测 `match -5 { x when x > 0 => …; x when x < 0 => …; _ }`
    /// 返回首个 arm 的结果而非 `x < 0` 分支）。fixture `match_guard.mora`
    /// 之所以"通过"，只因取值 42/0 恰好让首守卫为真 —— 属侥幸。
    ///
    /// 修法：守卫与 arm body 同构 —— **延迟到匹配时求值**的 MirFunction，
    /// 由 `h_match_expr` 在模式绑定之后调用（此时绑定变量已在 env 中），
    /// 再按真值决定是否采用该 arm。arm 元组第 3 项现为 `(守卫, body)` 两个
    /// MirFunction。
    ///
    /// arms: `(pattern_str, guard_mir_or_None, body_mir_func, output_reg)`
    MatchExpr {
        val: Reg,
        arms: Vec<MatchArmInst>,
    },

    // ── 语句指令（副作用）──
    Define(String, Reg),
    Assign(String, Reg),
    Expr(Reg),

    /// α.2: task 定义。body 是嵌套 MirFunction，解释器递归执行。
    TaskDef {
        name: String,
        params: Vec<String>,
        body: Box<MirFunction>,
    },

    /// α.10: 闭包字面量。body 是嵌套 MirFunction（独立寄存器空间），
    /// 解释器构造 Value::Closure { mir_body: Arc<MirFunction> }。
    /// 调用时 dispatch 直接走 run_mir。
    Closure {
        dst: Reg,
        params: Vec<String>,
        body: Box<MirFunction>,
    },
    /// α.12: dyn Trait 包装。解释器构造 Value::TraitObject { data, trait_name }。
    /// vtable 派发由 call_method 的 TraitObject 分支经 dispatch_trait_method
    /// 处理。
    DynTrait {
        dst: Reg,
        src: Reg,
        trait_generics: Vec<String>,
        trait_name: String,
    },

    /// α.2: import 语句。解释器读文件+解析+执行（委托 AST 路径）。
    Import(String),

    /// v0.103: `export <声明>` —— 把名字标记为模块对外公开（spec §10.2）。
    ///
    /// 紧跟在被导出的声明之后，把该名字写入环境的导出集
    /// （`Environment::mark_exported`）。`import` 只合并导出集内的名字，
    /// 未标记的绑定是模块私有 —— 这是此前完全缺失的可见性机制
    /// （`Environment::define` 的 `exported` 参数被忽略）。
    ExportMark(String),

    /// α.2: with 块。bindings 设置 AI config，body 执行后恢复。
    /// 解释器保存/恢复 current_ai_config。
    /// v0.75.43: jit=true 时 body 经 copy-and-patch JIT（纯线性 Int 子集）
    /// 编译执行，未覆盖指令回落 run_mir。
    WithConfig {
        bindings: Vec<(String, Reg)>,
        body: Box<MirFunction>,
        jit: bool,
    },

    // v0.80: 代数效应（Stage 2/4 落地）— Koka-style perform/handle。
    //
    // 设计合约（与 docs/fp-impl-roadmap.md §2.1 一致）：
    //
    // **Perform**: 执行一个具名 effect。
    // - `dst`: 返回值寄存器（None 表示 effect 无返回值）。
    // - `effect`: effect 标签（字符串），例如 "Ai", "Fs", "Bsp"。
    //   与 EffectRow::Cons(head, ...) 中的 head 同名。
    // - `args`: 参数寄存器列表（解释器从 regs[args[i]] 取值）。
    //
    // 运行时机制：
    // 1. 解释器从 regs[args[i]] 取所有参数值为 Vec<Value>
    // 2. 调用 interp.perform_effect(effect, args) → Option<Value>
    //    - 若有 handle 块已安装 handler → handler 处理
    //    - 否则返回 None → 编译期漏检报错（unhandled effect: ...）
    // 3. 结果写入 regs[dst]
    //
    // **Handle**: 安装 effect handler（围栏式 delimited binding）。
    // - `effect`: 拦截的 effect 标签
    // - `body`: handler 管辖的子 MirFunction（解释器递归执行）
    // - `handler`: handler 实现的子 MirFunction（解释器递归执行）
    // - `k_param`: 在 handler 中 resume 续名的参数名（如同 lambda 参数）
    // - `k_dst`: resume 调用的结果寄存器（handler 末尾的 resume "k" 续名）
    //
    // 运行时机制（delimited continuation）：
    // 1. 保存当前环境的 clone 给 body
    // 2. install_effect_handler(effect, this_handler)
    // 3. execute body via run_mir
    // 4. 卸载 handler（restore_effect_handler）
    //
    // 类型约束（Stage 2.2 强制）：
    // - body 必须有 EffectRow 包含 `effect` 标签
    // - handler 必须是 Arrow(args, return_type, EffectRow') 形式
    // - 整个 handle 块的 effect = body.effects - {effect} + handler.effects
    //
    // 与 Koka/Eff/Frank 的差异：
    // - Koka: handlers 是普通函数 +
    //   resume 可多次调用（multi-shot）；本实现第一版是 single-shot
    // - Frank: handler 是普通函数（无特殊语法）；本实现 handler 是 MirFunction
    // - 共同点：handler 是 first-class value，可在栈上 install/take/restore
    Perform {
        dst: Reg,
        effect: String,
        args: Vec<Reg>,
    },
    Handle {
        effect: String,
        body: Box<MirFunction>,
        handler: Box<MirFunction>,
        k_param: String,
        k_dst: Reg,
    },

    // v0.75.26: StreamFor 已删除——死原语（零构造点、零测试引用、语义被
    // ai.chat 的 stream:true 参数路径取代；handler 空转：prompt_reg/var 被忽略、
    // body 仅执行一次并丢弃）。流式语义若需 MIR 指令级支持，重新设计而非复活旧形状。

    // ── v0.103: 移除 10 个零 producer 死 IR 原语 ──
    //
    // 此前遗留：MatchArm/ToolDef/SkillDef + 7 个文件 I/O（Save/Load/ReadFile/
    // WriteFile/AppendFile/ReadBytesFile/WriteBytesFile）共 10 个变体。
    // 这些变体在 parser/lower/fcfg_lower 三个生产路径均无构造点：
    // - v0.55 去 AST 化迁移后未迁移的旧 AST 遗留；
    // - v0.85+ 的 `file.*` builtin 已完全取代 7 个文件 I/O 的能力（带 sandbox
    //   路径校验）；
    // - v0.103 实现的 export 语义取代了 ToolDef.exported 参数设计；
    // - MatchExpr 已内联 arms 字段，MatchArm 不再是独立构造点。
    //
    // **Send / Halt 保留**：Send 在 `src/pregel/mod.rs` 测试 fixture 中直接
    // 构造（2287, 2543, 2547, 2563, 2659, 2675），删除会破 6 个 BSP 引擎
    // 单元测试；Halt 在 `tests/tier0_replacement.rs` 中直接构造（241, 258），
    // 是「手工构造驱动 IR」既定模式的实例 —— 与已删除的 Route/Receive/
    // StreamFor 同类前提，但存在「被测试 fixture 直接构造」的事实。
    //
    // **删除代价**：移除一处枚举变体 + 8 处 match 穷举 + handler 函数
    // + cost/ssa/pipeline 中的跳过列表条目。零行为变化（无生产路径
    // 能产出这些变体）。

    // ── 文件 I/O 替代 ──
    //   file.* builtin 已覆盖 Save/Load/ReadFile/WriteFile/AppendFile/
    //   ReadBytesFile/WriteBytesFile 的全部功能 —— save "path", value
    //   等同于 file.write_text；load 等同于 file.read_text 等。

    // ── 类型系统 ──
    /// α.3: 类型别名。定义 `name` → `target` 的字符串映射。
    TypeAlias {
        name: String,
        target: String,
    },

    /// α.3: 枚举定义。定义 `name` → Dict(variant_name → String)。
    EnumDef {
        name: String,
        variants: Vec<crate::common::EnumVariant>,
    },

    /// α.3: 结构体定义。定义 `name` → Closure(构造器)。
    StructDef {
        name: String,
        fields: Vec<crate::common::StructField>,
    },

    // ── v0.103: TEA (The Elm Architecture) 语法层 ──
    /// Model 定义 — 强类型状态容器（类似 StructDef，但语义是「不可变 Model 容器」）。
    /// v0.83 阶段 E 注册到 env 为 Type::TeaModel。
    ModelDef {
        name: String,
        fields: Vec<crate::common::StructField>,
    },
    /// Msg 定义 — tagged union，每个变体可携带 payload。
    /// v0.83 阶段 E 注册到 env 为 Type::TeaMsg。
    MsgDef {
        name: String,
        variants: Vec<crate::common::MsgVariant>,
    },
    /// Update 函数定义 — `fn(Model, Msg) -> (Model, Cmd)`。
    /// v0.83 阶段 E 注册到 env 为 Type::TeaUpdate (Type::Arrow with effect row)。
    UpdateDef {
        name: String,
        params: Vec<String>,
        body: Box<MirFunction>,
    },
    /// App 定义 — 完整 TEA app（init/update/view 三个 MirFunction）。
    /// v0.83 阶段 E 注册到 env 为 Type::TeaApp，构造 Value::TeaApp。
    AppDef {
        name: String,
        model_name: String,
        msg_name: String,
        init_mir: Box<MirFunction>,
        update_mir: Box<MirFunction>,
        view_mir: Box<MirFunction>,
    },

    // ── v0.102: 声明式范式（逻辑式/关系式）──
    /// 关系定义 — 注册 `Value::Relation { name, clauses }`；同名 rel 定义
    /// 经 h_rel_def 累积子句（Prolog consult 语义）。
    /// clauses 是编译期子句模板（头/体可含 `Term::Param` 与构造形态），
    /// 搜索期由引擎 rename 为 fresh 逻辑变量。
    RelDef {
        name: String,
        clauses: Vec<crate::rel::Clause>,
    },
    /// solve 查询 — 运行 goal 构建体（新作用域，查询变量已注入）得到
    /// `Value::Goal`，引擎交错搜索后把查询变量投影 reify 成列表写 dst。
    /// limit: None = run*（全部解）；Some(n) = run N（前 n 个解）。
    /// query_vars: 目标语法中 `?` 隐式逻辑变量名（按首次出现序），
    /// h_solve 按序分配 `Value::LogicVar(i)` 并注入构建环境。
    Solve {
        dst: Reg,
        limit: Option<usize>,
        query_vars: Vec<String>,
        /// v0.102: 存在性匿名变量（`_`）—— 分配 fresh 逻辑变量但不投影。
        anon_vars: Vec<String>,
        goal: Box<MirFunction>,
    },

    // ── 宏定义（α.5: 与 AST execute_macro_def 语义一致）──
    /// α.5: macro def — 注册 Value::Macro(name, params, body) 到环境。
    /// body 是宏体的 MIR 编译结果（由 parser_v3 emit_macro_def_w 经子
    /// EmitContext 编译而来）。调用时以 call_args 绑定 params，在子 env
    /// 中 run_mir 执行 body（v0.83 完整实现，不再跳过宏体）。
    MacroDef {
        name: String,
        params: Vec<String>,
        body: Box<MirFunction>,
    },

    // ── 运行时特性（α.4: transaction / worker）──
    /// α.4: 事务。body 执行成功则正常返回；失败则执行 compensation 后返回错误。
    Transaction {
        body: Box<MirFunction>,
        compensation: Box<MirFunction>,
    },

    /// α.4: send — 发送值到 worker channel（target 是 channel 名称）。
    /// v0.93: 经 h_send 提交 `Effect::Send`（effect-as-data），执行器在
    /// worker 边界取走并按确定顺序 merge（不污染变量环境）；pregel 引擎的
    /// pending_sends/combiner/ADVANCE 投递机制是活的。
    Send {
        value: Reg,
        target: String,
    },

    /// v0.75.83: aggregate — 向 per-super-step 聚合器贡献值。
    /// v0.93: 经 h_aggregate 提交 `Effect::Contribute`（与 Send 同一
    /// Effects 数据通道），引擎侧 aggregator_contribute 归约。
    Aggregate {
        name: String,
        value: Reg,
    },

    // v0.75.31: Receive 已删除 — 语义漂移的死原语：h_receive 读共享
    // Environment 当消息源（把「变量作用域」当「消息队列」）；MirInst::
    // Receive 全仓零构造（src+tests）。pregel 的接收由引擎 input_<channel>
    // 注入实现（非 Receive 指令）。Message 语义统一由引擎投递。
    /// α.4: rollback — 触发事务回滚（返回 "Transaction rolled back" 错误）。
    Rollback,

    /// α.5: worker — 并发 worker 单元。
    ///
    /// v0.103: 真正并发执行（此前文档写「顺序执行」且 handler 是单线程
    /// run_isolated —— spec §9.2 承诺「Worker 并发」名存实亡）。每个 worker
    /// 在独立宿主克隆（`MirHost::clone_box`）上运行，Effect 经 `Effects`
    /// 数据通道回传父宿主（与 Pregel 并行 worker 同一机制）。
    Worker {
        name: String,
        body: Box<MirFunction>,
    },

    /// v0.103: 并行块 —— `parallel ... end`（spec §9.1）。
    ///
    /// 块内各 `worker` 声明并发执行，其余语句在块边界顺序执行。声明为零个
    /// worker 时退化为普通块（顺序执行 body，spec §9.1 的 `let a = ...` 形式）。
    Parallel {
        body: Box<MirFunction>,
    },

    /// α.5: commit — 事务提交（no-op，事务块内语句，spec 9.3）。
    Commit,

    // v0.75.81: Route 已删除 — 死原语（零构造点）。路由由显式 API
    // `Value::Router` + `.route()`/`.listen()` 方法（dispatch.rs）承担，
    // 与 v0.06.7 的 serve/http 关键字移除同一收敛方向。
    /// α.5: observe — 可观测性块。执行 body，配置信息记录但无副作用。
    Observe {
        config: String,
        body: Box<MirFunction>,
    },

    /// α.5: span — 追踪 span（v0.103: 接入 TraceCollector 真实记录）。
    Span {
        name: String,
        /// span 属性（`tags {k: "v", ...}`）。
        tags: Vec<(String, String)>,
        body: Box<MirFunction>,
    },

    // v0.75.81: RecordTokens 已删除 — 死原语（零构造点）。token 记录由
    // `TraceCollector::record_tokens`（trace_collector.rs）+ `AiRuntime::
    // record_tokens`（runtime/ai.rs）承担，AI 调用后真实记录（ai_helpers.rs）。
    // 与 v0.75.26 StreamFor 删除同一先例：语义被运行时路径取代。

    // v0.103: Save/Load/ReadFile/WriteFile/AppendFile/ReadBytesFile/
    // WriteBytesFile 已删除 —— 零 producer 死 IR（三个生产路径
    // parser_v3/lower/fcfg_lower 均无构造点），能力由带 sandbox 路径
    // 校验的 `file.*` builtin 完整承担（v0.85+）。spec 仅 §14.1 陈旧
    // 关键字表提及，无任何语义章节。

    // ── 类型系统（α.7: TraitDef/ImplDef）──
    /// v0.55: trait def — 完全 MIR-native，methods 是 MirTraitMethod 而非 ast_v2::TraitMethod。
    /// v0.92: types re-exported from `crate::mir::orchestrate::*` (消除 expr/ 跨层引用)。
    TraitDef {
        name: String,
        parents: Vec<String>,
        methods: Vec<crate::mir::orchestrate::MirTraitMethod>,
        /// prelowered method bodies (parallel to methods)，让默认实现走 run_mir。
        method_bodies: Vec<MirFunction>,
    },

    /// v0.55: impl def — 完全 MIR-native。
    ImplDef {
        trait_name: String,
        trait_generics: Vec<String>,
        for_type: String,
        for_generics: Vec<String>,
        methods: Vec<crate::mir::orchestrate::MirFnDef>,
        /// prelowered method bodies (parallel to methods)。
        method_bodies: Vec<MirFunction>,
    },

    // ── 高级特性（α.8: orchestrate/skill/prompt/document/eval）──
    /// v0.55: orchestrate — 编排执行。
    Orchestrate {
        input_var: String,
        result_var: String,
        kind: Box<crate::mir::orchestrate::MirOrchestrateKind>,
    },

    /// α.8: eval — 断言测试。
    Eval {
        name: String,
        given_reg: Reg,
        expects: Vec<Reg>,
        tolerance: Option<f64>,
        replay_path: Option<String>,
    },

    /// v0.55: skill def 已删除（v0.103）—— 零 producer 死 IR：三个生产路径
    /// 均无构造点，能力由 `skill.*` builtin 承担。

    /// α.8: prompt section — 扫描 body，构建 Value::PromptSection 到环境。
    PromptSection {
        name: String,
        body: Box<MirFunction>,
    },

    /// α.8: document section — 扫描 body，构建 Value::DocumentSection 到环境。
    DocumentSection {
        name: String,
        body: Box<MirFunction>,
    },

    // ── 控制流（替代 FlowSignal 枚举传返）──
    Label(Label),
    Jump(Label),
    JumpIf(Reg, Label),
    JumpIfNot(Reg, Label),
    Return(Option<Reg>),
    /// v0.75: vote_to_halt — agent 主动声明"我完成了，除非收到 Send 否则
    /// 不再被调度"。BSP 引擎据此将顶点置为 Halted。线性上下文中等价于 Return。
    Halt(Option<Reg>),
    /// α.1: break 到指定 label（循环出口）
    Break(Label),
    /// α.1: continue 到指定 label（循环增量处）
    Continue(Label),
    /// v0.88: 反引号 quasiquote —— 编译期提取代码段结构，运行时重组为 Mora 源码字符串，
    /// 返回 `Value::Code`。
    /// 与 `quote(expr)` 对称：quote 冻结整个源码；quasiquote 选择性冻结。
    ///
    /// 实现策略（对齐 Prompt 指令的 register-ref 模式）：
    /// - emit 阶段：遇到 `` `expr `` → 递归 emit 子表达式，同时建立 QuasiquoteSegment 列表
    ///   - 静态源码片段 → Quote(src_text)
    ///   - `,expr` unquote → 先 emit expr → 记录 dst_reg → Unquote(dst_reg)
    ///   - `,,expr` splice → 先 emit expr → 记录 dst_reg → UnquoteSplice(dst_reg)
    /// - 执行阶段：h_quasiquote 遍历 segments，Quote 直接拼入，Unquote 取寄存器值
    ///   经 Mora Display 格式化为代码字符串，UnquoteSplice 取 List 展平为逗号分隔代码。
    Quasiquote {
        dst: Reg,
        segments: Vec<QuasiquoteSegment>,
    },
}

/// v0.88: Quasiquote 代码段类型 — 反引号 `` ` `` 内的结构化片段。
///
/// 编译期已知边界（emit 阶段构建），运行时按类型求值/拼接。
/// 语义（Lisp 系）：
///   - Quote(src)         → 原样保留源码 `src`（静态文字）
///   - Unquote(reg)       → 读取 `regs[reg]`，经 Mora Display 格式化为代码字符串
///   - UnquoteSplice(reg) → 读取 `regs[reg]`（期望为 List），展平为 `item1, item2, ...` 代码
///
/// Register-ref 设计对齐 Prompt 指令：emit 阶段已求值 unquote 子表达式，
/// handler 只负责按 register 取数拼接，与 Prompt 的 parts 寄存器列表同构。
#[derive(Debug, Clone, PartialEq)]
pub enum QuasiquoteSegment {
    Quote(String),
    Unquote(Reg),
    UnquoteSplice(Reg),
}

impl MirFunction {
    // Label 在 body 中的实际索引。lowering 时 Label 占位，finish 时回填。
    // α.0 简化：Label 指令本身就是目标，Jump 的 label 是 body 索引。
    pub fn label_index(&self, label: Label) -> usize {
        label
    }
}
