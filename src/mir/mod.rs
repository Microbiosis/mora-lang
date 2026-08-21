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
// v0.89: Core IR — 9 层架构第 3 层（函数式基元指令集，SSA 100% 覆盖）
pub mod core;
// v0.78: EffectRow — algebraic effect 的类型表示（Stage 1/4 落地）
pub mod effect;
pub mod expr;
// v0.89: FCFG — 9 层 IR 架构第 1 层（Node<M> 泛型 + 结构化 CFG）
pub mod fcfg;
// v0.89: FCFG → MIR 桥接层（Node<()> 降维为 MirInst 线性序列）
pub mod fcfg_lower;
pub mod hint;
pub mod host;
pub mod jit;
pub mod lower;
pub mod opt;
pub mod optimize;
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

pub use expr::MirExpr;
pub use vm::run_mir;
// lower_program removed in Phase A (v0.55) — use lower_mir_exprs instead

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
    /// arms: (pattern_str, condition_reg_or_None, body_mir_func, output_reg)
    MatchExpr {
        val: Reg,
        arms: Vec<(String, Option<Reg>, Box<MirFunction>, Reg)>,
    },

    // ── 语句指令（副作用）──
    Define(String, Reg),
    Assign(String, Reg),
    Expr(Reg),

    /// 模式匹配分支：cond_reg 非空时表示条件守卫，空时表示默认分支
    /// 由 Match lowering 生成多个 MatchArm，解释器依次匹配
    MatchArm {
        cond_reg: Option<Reg>,
        body: Box<MirFunction>,
    },

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

    /// α.2: tool 定义。body 是嵌套 MirFunction；params/return_type 用于 schema。
    /// 解释器注册为 Value::Tool 到 environment + ToolDef 到 tool_registry。
    ToolDef {
        name: String,
        description: String,
        params: Vec<String>,
        return_type: Option<String>,
        body: Box<MirFunction>,
        exported: bool,
    },

    /// α.2: import 语句。解释器读文件+解析+执行（委托 AST 路径）。
    Import(String),

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

    // ── 类型定义语句（α.3: 与 AST execute 语义一致）──
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

    // ── v0.83: TEA (The Elm Architecture) 语法层 ──
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
    /// v0.75.31: Send 保留（写独立 dynamic_sends 缓冲，不污染变量环境；
    /// pregel 引擎的 pending_sends/combiner/ADVANCE 投递机制是活的）。
    Send {
        value: Reg,
        target: String,
    },

    /// v0.75.83: aggregate — 向 per-super-step 聚合器贡献值。
    /// 经 h_aggregate push 到 MirHost 缓冲，Pregel 引擎超步末收集归约
    /// （与 Send/dynamic_sends 同构；引擎侧 aggregator_contribute 归约）。
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

    /// α.5: worker — 并发 worker。body 顺序执行（与 AST 语义一致）。
    Worker {
        name: String,
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

    /// α.5: span — 追踪 span。执行 body，name 记录但不执行实际追踪。
    Span {
        name: String,
        body: Box<MirFunction>,
    },

    // v0.75.81: RecordTokens 已删除 — 死原语（零构造点）。token 记录由
    // `TraceCollector::record_tokens`（trace_collector.rs）+ `AiRuntime::
    // record_tokens`（runtime/ai.rs）承担，AI 调用后真实记录（ai_helpers.rs）。
    // 与 v0.75.26 StreamFor 删除同一先例：语义被运行时路径取代。

    // ── 文件 I/O（α.6: Save/Load/ReadFile/WriteFile/AppendFile/ReadBytesFile/WriteBytesFile）──
    /// α.6: save — 将 value 序列化为文件。
    Save {
        path: Reg,
        value: Reg,
    },

    /// α.6: load — 从文件加载 JSON 值并绑定到 var。
    Load {
        path: Reg,
        var: String,
    },

    /// α.6: read_file — 读取文件为字符串，绑定到 var。
    ReadFile {
        path: Reg,
        var: String,
    },

    /// α.6: write_file — 将 content 写入文件。
    WriteFile {
        path: Reg,
        content: Reg,
    },

    /// α.6: append_file — 将 content 追加到文件。
    AppendFile {
        path: Reg,
        content: Reg,
    },

    /// α.6: read_bytes_file — 读取文件为字节数组，绑定到 var。
    ReadBytesFile {
        path: Reg,
        var: String,
    },

    /// α.6: write_bytes_file — 将 hex 字节写入文件。
    WriteBytesFile {
        path: Reg,
        content: Reg,
    },

    // ── 类型系统（α.7: TraitDef/ImplDef）──
    /// v0.55: trait def — 完全 MIR-native，methods 是 MirTraitMethod 而非 ast_v2::TraitMethod。
    TraitDef {
        name: String,
        parents: Vec<String>,
        methods: Vec<crate::mir::expr::MirTraitMethod>,
        /// prelowered method bodies (parallel to methods)，让默认实现走 run_mir。
        method_bodies: Vec<MirFunction>,
    },

    /// v0.55: impl def — 完全 MIR-native。
    ImplDef {
        trait_name: String,
        trait_generics: Vec<String>,
        for_type: String,
        for_generics: Vec<String>,
        methods: Vec<crate::mir::expr::MirFnDef>,
        /// prelowered method bodies (parallel to methods)。
        method_bodies: Vec<MirFunction>,
    },

    // ── 高级特性（α.8: orchestrate/skill/prompt/document/eval）──
    /// v0.55: orchestrate — 编排执行。
    Orchestrate {
        input_var: String,
        result_var: String,
        kind: Box<crate::mir::expr::MirOrchestrateKind>,
    },

    /// α.8: eval — 断言测试。
    Eval {
        name: String,
        given_reg: Reg,
        expects: Vec<Reg>,
        tolerance: Option<f64>,
        replay_path: Option<String>,
    },

    /// v0.55: skill def — 完全 MIR-native。
    SkillDef {
        name: String,
        description: Option<String>,
        version: Option<String>,
        requires: Vec<String>,
        tasks: Vec<crate::mir::expr::MirSkillTask>,
        /// prelowered task bodies (parallel to tasks)。
        task_bodies: Vec<MirFunction>,
        verify: Option<crate::mir::expr::MirSkillVerify>,
        /// α.11: prelowered verify body。
        verify_body: Option<MirFunction>,
    },

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
