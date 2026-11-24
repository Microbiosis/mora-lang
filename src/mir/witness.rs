//! v0.110.0: `MirWitness` —— 前端语法见证树（供 `mir::control::lower_program`、统一 HM 类型检查与 LSP 消费）。
//!
//! `ParserV3` 单遍解析源码生成 `Vec<MirWitness>`（携带精确 `Span`），随后：
//! 1. `mir::control::lower_program` 将其降低为显式基本块 `ControlProgram` SSA IR；
//! 2. `typeck::check_mir::check_program_witnesses` 在其上执行统一 HM 约束求解与 5 项 AI-Native 静态检查；
//! 3. `lsp::providers` 在其上提供语法结构与符号导航。
//!
//! - **独立 WitnessKind 枚举**（47 变体，captured_env 已删零消费死字段）。
//!
//! 复合类型（见证树的结构化分支）：WitnessCallee（调用目标）、WitnessArm（匹配臂）、
//! WitnessParam（形参）、WitnessPattern（模式）。

use crate::common::{BinaryOp, Literal, Span};

/// 轻量树骨架节点 — kind + span，无执行语义。
#[derive(Debug, Clone, PartialEq)]
pub struct MirWitness {
    pub kind: WitnessKind,
    pub span: Span,
}

/// Witness 树节点种类 — 47 变体（captured_env 已删）。
#[derive(Debug, Clone, PartialEq)]
pub enum WitnessKind {
    Literal(Literal),
    Variable(String),
    Binary {
        left: Box<MirWitness>,
        op: BinaryOp,
        right: Box<MirWitness>,
    },
    Call {
        callee: WitnessCallee,
        args: Vec<MirWitness>,
        named_args: Vec<(String, MirWitness)>,
    },
    MethodCall {
        receiver: Box<MirWitness>,
        method: String,
        args: Vec<MirWitness>,
        named_args: Vec<(String, MirWitness)>,
    },
    Closure {
        type_params: Vec<String>,
        params: Vec<WitnessParam>,
        body: Box<MirWitness>,
    },
    FnDef {
        name: String,
        /// Explicit generic type parameters (e.g. `task f<T, U>(…)`).
        type_params: Vec<String>,
        params: Vec<WitnessParam>,
        return_type: Option<crate::mir::hint::TypeHint>,
        /// Optional explicit effect row constraint (`! pure` / `! io, async`).
        effect_annotation: Option<crate::mir::effect::EffectRow>,
        body: Box<MirWitness>,
    },
    Match {
        scrutinee: Box<MirWitness>,
        arms: Vec<WitnessArm>,
    },
    If {
        cond: Box<MirWitness>,
        then: Box<MirWitness>,
        r#else: Option<Box<MirWitness>>,
    },
    Loop {
        var: String,
        iterable: Box<MirWitness>,
        body: Box<MirWitness>,
    },
    While {
        cond: Box<MirWitness>,
        body: Box<MirWitness>,
    },
    Or {
        left: Box<MirWitness>,
        right: Box<MirWitness>,
    },
    And {
        left: Box<MirWitness>,
        right: Box<MirWitness>,
    },
    List(Vec<MirWitness>),
    Dict(Vec<(String, MirWitness)>),
    DynTrait {
        expr: Box<MirWitness>,
        trait_name: String,
        generics: Vec<crate::mir::hint::TypeHint>,
    },
    LetBinding {
        name: String,
        type_hint: Option<crate::mir::hint::TypeHint>,
        value: Box<MirWitness>,
        init_body: Box<MirWitness>,
    },
    Assign {
        target: String,
        value: Box<MirWitness>,
    },
    IndexAssign {
        object: Box<MirWitness>,
        index: Box<MirWitness>,
        value: Box<MirWitness>,
    },
    Return(Option<Box<MirWitness>>),
    Break(String),
    Continue(String),
    TypeAlias {
        name: String,
        target: crate::mir::hint::TypeHint,
    },
    EnumDef {
        name: String,
        type_params: Vec<String>,
        const_params: Vec<String>,
        variants: Vec<WitnessEnumVariant>,
    },
    StructDef {
        name: String,
        type_params: Vec<String>,
        const_params: Vec<String>,
        fields: Vec<WitnessStructField>,
    },
    /// Wave 3 Stage 3C: Source-level trait definition (`trait Name<T>: Parent ... end`).
    TraitDef {
        name: String,
        type_params: Vec<String>,
        parents: Vec<String>,
        methods: Vec<WitnessTraitMethod>,
    },
    /// Wave 3 Stage 3C: Source-level trait or inherent `impl` block (`impl Trait for Target ... end` / `impl Target ... end`).
    ImplDef {
        type_params: Vec<String>,
        const_params: Vec<String>,
        trait_name: Option<String>,
        trait_generics: Vec<crate::mir::hint::TypeHint>,
        target_type: String,
        target_generics: Vec<crate::mir::hint::TypeHint>,
        methods: Vec<WitnessImplMethod>,
    },
    /// P4-1B / P5-1B: First-class source-level GPU compute `kernel name(params...) ... end` declaration.
    KernelDef {
        name: String,
        params: Vec<crate::common::kernel_ir::KernelParam>,
        shared: Vec<crate::common::kernel_ir::KernelSharedArray>,
        body: Vec<MirWitness>,
    },
    /// Wave 3 Stage 3C: Multi-dimensional zero-copy tensor slicing (`t[0..4, :, -1]`, `t[..., None]`).
    TensorSlice {
        target: Box<MirWitness>,
        axes: Vec<WitnessSliceAxis>,
    },
    /// Wave 3 Stage 3C: Autograd tape suppression block (`no_grad ... end`).
    NoGrad {
        body: Box<MirWitness>,
    },
    /// G2: Postfix error propagation operator (`expr?`) for `Result` / `Option` / nullable values.
    Try {
        expr: Box<MirWitness>,
    },
    /// G2: LIFO scope-exit explicit resource cleanup (`defer expr` / `defer do ... end` / `defer { ... }`).
    Defer {
        body: Box<MirWitness>,
    },
    Import(String),
    // v0.80: algebraic effects witness。
    //   Perform: 触发一个具名 effect。
    //   Handle: 安装 handler 围栏。
    Perform {
        effect: String,
        args: Vec<MirWitness>,
    },
    Handle {
        effect: String,
        body: Box<MirWitness>,
        handler: Box<MirWitness>,
        k_param: String,
    },
    MacroDef {
        name: String,
        params: Vec<String>,
        body: Box<MirWitness>,
    },
    Sequence(Vec<MirWitness>),
    // v0.88: Quasiquote（反引号 `expr + ,unquote / ,,unquote-splice)
    /// segments 用 MirWitness 编码：Quote = Literal(String),
    /// Unquote = 被求值的子表达式 witness,
    /// UnquoteSplice = 子表达式 witness + Literal(Bool(true)) 标记。
    Quasiquote {
        segments: Vec<MirWitness>,
    },
    // v0.98: 显式 effect 签名声明（纯类型层 —— 只被 typeck 消费）。
    //   `effect Name(Hint, ...) : Hint`   参数契约 + 结果契约
    //   `effect Name(Hint, ...)`          仅参数契约（结果 = Any）
    EffectSig {
        name: String,
        params: Vec<crate::mir::hint::TypeHint>,
        result: Option<crate::mir::hint::TypeHint>,
    },
    /// v0.103: `parallel ... end` — 并行块（spec §9.1/§9.2）。
    Parallel {
        body: Box<MirWitness>,
    },
    /// v0.103: `export <声明>` —— 把内层声明导出的名字标记为公开（spec §10.2）。
    /// `names` 是该声明产生的绑定名（静态可知，供 typeck 侧导入可见性判定）。
    Export {
        names: Vec<String>,
        decl: Box<MirWitness>,
    },

    // ── worker / transaction / eval / aggregate 的见证表示 ──
    //
    // 这 4 类构造必须在 witness 层有**结构化表示**：此前它们被压扁进一个
    // 扁平的 `Sequence`，结构信息（worker 批次、补偿段、断言期望集、贡献值）
    // **结构上就拿不到**，lower 只能回落成普通语句序列。
    //
    // 补齐后，lowering 才能把它们直接降成 `ControlInst::Parallel` /
    // `ControlInst::Transaction` / `ControlInst::EvalAssert`，或按约定
    // 求值出贡献值。
    /// worker 块 —— lower 为 `ControlInst::Parallel` 的单个 `ParallelSegment::WorkerBatch`。
    Worker {
        name: String,
        body: Box<MirWitness>,
    },
    /// transaction 块 —— lower 为 `ControlInst::Transaction { body_fn, comp_fn, .. }`。
    Transaction {
        body: Box<MirWitness>,
        compensation: Box<MirWitness>,
    },
    /// eval 断言 —— lower 为 `ControlInst::EvalAssert`。
    Eval {
        name: String,
        given: Box<MirWitness>,
        expects: Vec<MirWitness>,
        tolerance: Option<f64>,
        replay_path: Option<String>,
    },
    /// aggregate 贡献 —— lower 直接求值 `value`，语句值即贡献值。
    Aggregate {
        name: String,
        value: Box<MirWitness>,
    },

    /// 管道 `x |> f` —— 承载 `ControlInst::Pipe` 的两个操作数。
    ///
    /// witness 层必须保留独立的 `Pipe` 变体（而非脱糖成 `Call`）：否则
    /// typeck 与 lowering 两侧看到的结构不同构。
    ///
    /// 类型推断口径（见 `typeck/hm/mod.rs`）：右侧是裸标识符 ⇒ 按具名调用
    /// `f(x)` 求解；右侧是任意表达式 ⇒ 先求值右侧，再把左侧作为唯一实参
    /// 施加于它。
    Pipe {
        left: Box<MirWitness>,
        right: Box<MirWitness>,
    },
    /// `commit` 语句 —— lower 为 unit 指令 `ControlInst::Commit`。
    Commit,
    /// `rollback` 语句 —— lower 为 unit 指令 `ControlInst::Rollback`。
    Rollback,
    /// `spawn <target>` — launches an isolated concurrent task returning `TaskHandle<R>`.
    Spawn {
        target: WitnessSpawnTarget,
    },
    /// `select ... end` — fair multiplexing over channel recv/send, timeout, and default arms.
    Select {
        arms: Vec<WitnessSelectArm>,
    },
}

/// Target expression inside `spawn <target>`.
#[derive(Debug, Clone, PartialEq)]
pub enum WitnessSpawnTarget {
    Call {
        callee: WitnessCallee,
        args: Vec<MirWitness>,
        named_args: Vec<(String, MirWitness)>,
    },
    MethodCall {
        receiver: Box<MirWitness>,
        method: String,
        args: Vec<MirWitness>,
        named_args: Vec<(String, MirWitness)>,
    },
    Expr(Box<MirWitness>),
}

/// Wave 3 Stage 3C: Field declaration inside `struct Name<T> ... end`.
#[derive(Debug, Clone, PartialEq)]
pub struct WitnessStructField {
    pub name: String,
    pub type_hint: crate::mir::hint::TypeHint,
    pub default: Option<MirWitness>,
    pub tags: Vec<(String, String)>,
    pub span: Span,
}

/// Wave 3 Stage 3C: Variant declaration inside `enum Name<T> ... end`.
#[derive(Debug, Clone, PartialEq)]
pub struct WitnessEnumVariant {
    pub name: String,
    pub payload_types: Vec<crate::mir::hint::TypeHint>,
    pub span: Span,
}

/// Wave 3 Stage 3C: Method signature (and optional default body) inside `trait ... end`.
#[derive(Debug, Clone, PartialEq)]
pub struct WitnessTraitMethod {
    pub name: String,
    pub type_params: Vec<String>,
    pub params: Vec<WitnessParam>,
    pub return_type: Option<crate::mir::hint::TypeHint>,
    pub effect_annotation: Option<crate::mir::effect::EffectRow>,
    pub default_body: Option<MirWitness>,
    pub has_self: bool,
    pub span: Span,
}

/// Wave 3 Stage 3C: Method implementation inside `impl ... end`.
#[derive(Debug, Clone, PartialEq)]
pub struct WitnessImplMethod {
    pub name: String,
    pub type_params: Vec<String>,
    pub params: Vec<WitnessParam>,
    pub return_type: Option<crate::mir::hint::TypeHint>,
    pub effect_annotation: Option<crate::mir::effect::EffectRow>,
    pub body: MirWitness,
    pub has_self: bool,
    pub span: Span,
}

/// Wave 3 Stage 3C: One axis entry inside multi-dimensional tensor slicing `t[a, b:c:s, ..., nil]`.
#[derive(Debug, Clone, PartialEq)]
pub enum WitnessSliceAxis {
    Index(Box<MirWitness>),
    Range {
        start: Option<Box<MirWitness>>,
        stop: Option<Box<MirWitness>>,
        step: Option<Box<MirWitness>>,
    },
    Ellipsis,
    NewAxis,
}

/// One branch inside `select ... end`.
#[derive(Debug, Clone, PartialEq)]
pub enum WitnessSelectArm {
    Recv {
        binding: Option<String>,
        channel: Box<MirWitness>,
        body: Box<MirWitness>,
    },
    Send {
        channel: Box<MirWitness>,
        value: Box<MirWitness>,
        body: Box<MirWitness>,
    },
    Timeout {
        ms: Box<MirWitness>,
        body: Box<MirWitness>,
    },
    Default {
        body: Box<MirWitness>,
    },
}

/// 调用目标 —— 函数名 / 变量 / 方法 / 求值表达式 / 内建操作码。
#[derive(Debug, Clone, PartialEq)]
pub enum WitnessCallee {
    Name(String),
    /// P4-1A: Nominal constructor or associated call with explicit generic/const arguments (e.g. `Linear<4, 3>(w, b)`).
    GenericName {
        name: String,
        type_args: Vec<crate::mir::hint::TypeHint>,
    },
    Var(String),
    Method(String, String),
    Evaluated(Box<MirWitness>),
    Builtin(BuiltinOp),
}

impl MirWitness {
    /// v0.96: 按求值顺序枚举全部子 witness（无子节点返回空）。
    ///
    /// 供效果定位游走等树遍历使用；注意**不**对遍历语义做特殊处理 ——
    /// Handle 的 body/handler、Closure/FnDef 的 body 一视同仁地返回，
    /// 调用方（如 typeck 效果定位）自行决定跳过哪些子树。
    pub fn child_witnesses(&self) -> Vec<&MirWitness> {
        match &self.kind {
            WitnessKind::Literal(_)
            | WitnessKind::Variable(_)
            | WitnessKind::Break(_)
            | WitnessKind::Continue(_)
            | WitnessKind::Import(_)
            | WitnessKind::Return(None)
            | WitnessKind::TypeAlias { .. }
            | WitnessKind::EnumDef { .. }
            | WitnessKind::EffectSig { .. } => Vec::new(),
            WitnessKind::StructDef { fields, .. } => {
                fields.iter().filter_map(|f| f.default.as_ref()).collect()
            }
            WitnessKind::TraitDef { methods, .. } => {
                let mut out = Vec::new();
                for m in methods {
                    for p in &m.params {
                        if let Some(d) = &p.default {
                            out.push(d);
                        }
                    }
                    if let Some(b) = &m.default_body {
                        out.push(b);
                    }
                }
                out
            }
            WitnessKind::ImplDef { methods, .. } => {
                let mut out = Vec::new();
                for m in methods {
                    for p in &m.params {
                        if let Some(d) = &p.default {
                            out.push(d);
                        }
                    }
                    out.push(&m.body);
                }
                out
            }
            WitnessKind::KernelDef { body, .. } => body.iter().collect(),
            WitnessKind::TensorSlice { target, axes } => {
                let mut out = vec![target.as_ref()];
                for ax in axes {
                    match ax {
                        WitnessSliceAxis::Index(e) => out.push(e),
                        WitnessSliceAxis::Range { start, stop, step } => {
                            if let Some(s) = start {
                                out.push(s);
                            }
                            if let Some(e) = stop {
                                out.push(e);
                            }
                            if let Some(st) = step {
                                out.push(st);
                            }
                        }
                        WitnessSliceAxis::Ellipsis | WitnessSliceAxis::NewAxis => {}
                    }
                }
                out
            }
            WitnessKind::NoGrad { body } | WitnessKind::Defer { body } => vec![body.as_ref()],
            WitnessKind::Try { expr } => vec![expr.as_ref()],
            WitnessKind::Binary { left, right, .. } => vec![left.as_ref(), right.as_ref()],
            WitnessKind::Call {
                callee,
                args,
                named_args,
            } => {
                let mut out: Vec<&MirWitness> = Vec::new();
                if let WitnessCallee::Evaluated(e) = callee {
                    out.push(e.as_ref());
                }
                out.extend(args);
                out.extend(named_args.iter().map(|(_, v)| v));
                out
            }
            WitnessKind::MethodCall {
                receiver,
                args,
                named_args,
                ..
            } => {
                let mut out = vec![receiver.as_ref()];
                out.extend(args);
                out.extend(named_args.iter().map(|(_, v)| v));
                out
            }
            WitnessKind::Closure { params, body, .. } | WitnessKind::FnDef { params, body, .. } => {
                let mut out: Vec<&MirWitness> =
                    params.iter().filter_map(|p| p.default.as_ref()).collect();
                out.push(body.as_ref());
                out
            }
            WitnessKind::Match { scrutinee, arms } => {
                let mut out = vec![scrutinee.as_ref()];
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        out.push(g);
                    }
                    out.push(&arm.body);
                }
                out
            }
            WitnessKind::If { cond, then, r#else } => {
                let mut out = vec![cond.as_ref(), then.as_ref()];
                if let Some(e) = r#else {
                    out.push(e.as_ref());
                }
                out
            }
            WitnessKind::Loop { iterable, body, .. }
            | WitnessKind::While {
                cond: iterable,
                body,
            } => vec![iterable.as_ref(), body.as_ref()],
            WitnessKind::Or { left, right } | WitnessKind::And { left, right } => {
                vec![left.as_ref(), right.as_ref()]
            }
            WitnessKind::List(items) => items.iter().collect(),
            WitnessKind::Dict(entries) => entries.iter().map(|(_, v)| v).collect(),
            WitnessKind::DynTrait { expr, .. } => vec![expr.as_ref()],
            WitnessKind::LetBinding {
                value, init_body, ..
            } => vec![value.as_ref(), init_body.as_ref()],
            WitnessKind::Assign { value, .. } => vec![value.as_ref()],
            WitnessKind::IndexAssign {
                object,
                index,
                value,
            } => vec![object.as_ref(), index.as_ref(), value.as_ref()],
            WitnessKind::Return(Some(v)) => vec![v.as_ref()],
            WitnessKind::Perform { args, .. } => args.iter().collect(),
            // v0.104.6 D472：4 类新构造的子 witness 必须被枚举出来 ——
            // `child_witnesses` 是效果定位游走等树遍历的入口，
            // 漏一条就等于让子树对遍历者隐形。
            WitnessKind::Worker { body, .. } => vec![body.as_ref()],
            WitnessKind::Transaction {
                body, compensation, ..
            } => vec![body.as_ref(), compensation.as_ref()],
            WitnessKind::Eval { given, expects, .. } => {
                let mut out = vec![given.as_ref()];
                out.extend(expects);
                out
            }
            WitnessKind::Aggregate { value, .. } => vec![value.as_ref()],
            // v0.104.6 D472：管道与 commit / rollback 的子 witness
            WitnessKind::Pipe { left, right, .. } => vec![left.as_ref(), right.as_ref()],
            WitnessKind::Commit | WitnessKind::Rollback => Vec::new(),
            WitnessKind::Handle { body, handler, .. } => {
                vec![body.as_ref(), handler.as_ref()]
            }
            WitnessKind::MacroDef { body, .. } => vec![body.as_ref()],
            WitnessKind::Sequence(items) => items.iter().collect(),
            WitnessKind::Quasiquote { segments } => segments.iter().collect(),
            // v0.103: 并行块 body
            WitnessKind::Parallel { body } => vec![body.as_ref()],
            WitnessKind::Export { decl, .. } => vec![decl.as_ref()],
            WitnessKind::Spawn { target } => match target {
                WitnessSpawnTarget::Call {
                    callee,
                    args,
                    named_args,
                } => {
                    let mut out = Vec::new();
                    if let WitnessCallee::Evaluated(e) = callee {
                        out.push(e.as_ref());
                    }
                    out.extend(args);
                    out.extend(named_args.iter().map(|(_, v)| v));
                    out
                }
                WitnessSpawnTarget::MethodCall {
                    receiver,
                    args,
                    named_args,
                    ..
                } => {
                    let mut out = vec![receiver.as_ref()];
                    out.extend(args);
                    out.extend(named_args.iter().map(|(_, v)| v));
                    out
                }
                WitnessSpawnTarget::Expr(e) => vec![e.as_ref()],
            },
            WitnessKind::Select { arms } => {
                let mut out = Vec::new();
                for arm in arms {
                    match arm {
                        WitnessSelectArm::Recv { channel, body, .. } => {
                            out.push(channel.as_ref());
                            out.push(body.as_ref());
                        }
                        WitnessSelectArm::Send {
                            channel,
                            value,
                            body,
                        } => {
                            out.push(channel.as_ref());
                            out.push(value.as_ref());
                            out.push(body.as_ref());
                        }
                        WitnessSelectArm::Timeout { ms, body } => {
                            out.push(ms.as_ref());
                            out.push(body.as_ref());
                        }
                        WitnessSelectArm::Default { body } => out.push(body.as_ref()),
                    }
                }
                out
            }
        }
    }

    /// W2.3: Iterative pre-order DFS over `roots` using an explicit heap stack
    /// so deep witness chains never consume OS call-stack frames.
    pub fn visit_preorder<'a>(roots: &'a [MirWitness], mut f: impl FnMut(&'a MirWitness)) {
        let mut stack: Vec<&'a MirWitness> = roots.iter().rev().collect();
        while let Some(node) = stack.pop() {
            f(node);
            let children = node.child_witnesses();
            stack.extend(children.into_iter().rev());
        }
    }
}

fn push_owned_witness_children(kind: WitnessKind, out: &mut Vec<MirWitness>) {
    match kind {
        WitnessKind::Literal(_)
        | WitnessKind::Variable(_)
        | WitnessKind::Break(_)
        | WitnessKind::Continue(_)
        | WitnessKind::Import(_)
        | WitnessKind::TypeAlias { .. }
        | WitnessKind::EnumDef { .. }
        | WitnessKind::EffectSig { .. }
        | WitnessKind::Commit
        | WitnessKind::Rollback
        | WitnessKind::Return(None) => {}
        WitnessKind::StructDef { fields, .. } => {
            for f in fields {
                if let Some(d) = f.default {
                    out.push(d);
                }
            }
        }
        WitnessKind::TraitDef { methods, .. } => {
            for m in methods {
                for p in m.params {
                    if let Some(d) = p.default {
                        out.push(d);
                    }
                }
                if let Some(b) = m.default_body {
                    out.push(b);
                }
            }
        }
        WitnessKind::ImplDef { methods, .. } => {
            for m in methods {
                for p in m.params {
                    if let Some(d) = p.default {
                        out.push(d);
                    }
                }
                out.push(m.body);
            }
        }
        WitnessKind::KernelDef { body, .. } => {
            out.extend(body);
        }
        WitnessKind::TensorSlice { target, axes } => {
            out.push(*target);
            for ax in axes {
                match ax {
                    WitnessSliceAxis::Index(e) => out.push(*e),
                    WitnessSliceAxis::Range { start, stop, step } => {
                        if let Some(s) = start {
                            out.push(*s);
                        }
                        if let Some(e) = stop {
                            out.push(*e);
                        }
                        if let Some(st) = step {
                            out.push(*st);
                        }
                    }
                    WitnessSliceAxis::Ellipsis | WitnessSliceAxis::NewAxis => {}
                }
            }
        }
        WitnessKind::NoGrad { body } | WitnessKind::Defer { body } => out.push(*body),
        WitnessKind::Try { expr } => out.push(*expr),
        WitnessKind::Binary { left, right, .. }
        | WitnessKind::Or { left, right }
        | WitnessKind::And { left, right }
        | WitnessKind::Pipe { left, right } => {
            out.push(*left);
            out.push(*right);
        }
        WitnessKind::Call {
            callee,
            args,
            named_args,
        } => {
            if let WitnessCallee::Evaluated(e) = callee {
                out.push(*e);
            }
            out.extend(args);
            for (_, v) in named_args {
                out.push(v);
            }
        }
        WitnessKind::MethodCall {
            receiver,
            args,
            named_args,
            ..
        } => {
            out.push(*receiver);
            out.extend(args);
            for (_, v) in named_args {
                out.push(v);
            }
        }
        WitnessKind::Closure { params, body, .. } | WitnessKind::FnDef { params, body, .. } => {
            for p in params {
                if let Some(d) = p.default {
                    out.push(d);
                }
            }
            out.push(*body);
        }
        WitnessKind::Match { scrutinee, arms } => {
            out.push(*scrutinee);
            for arm in arms {
                if let Some(g) = arm.guard {
                    out.push(g);
                }
                out.push(arm.body);
            }
        }
        WitnessKind::If { cond, then, r#else } => {
            out.push(*cond);
            out.push(*then);
            if let Some(e) = r#else {
                out.push(*e);
            }
        }
        WitnessKind::Loop { iterable, body, .. }
        | WitnessKind::While {
            cond: iterable,
            body,
        } => {
            out.push(*iterable);
            out.push(*body);
        }
        WitnessKind::List(items) | WitnessKind::Sequence(items) => out.extend(items),
        WitnessKind::Dict(entries) => {
            for (_, v) in entries {
                out.push(v);
            }
        }
        WitnessKind::Quasiquote { segments } => out.extend(segments),
        WitnessKind::Perform { args, .. } => out.extend(args),
        WitnessKind::LetBinding {
            value, init_body, ..
        } => {
            out.push(*value);
            out.push(*init_body);
        }
        WitnessKind::Assign { value, .. }
        | WitnessKind::DynTrait { expr: value, .. }
        | WitnessKind::MacroDef { body: value, .. }
        | WitnessKind::Parallel { body: value }
        | WitnessKind::Export { decl: value, .. }
        | WitnessKind::Worker { body: value, .. }
        | WitnessKind::Aggregate { value, .. } => out.push(*value),
        WitnessKind::Return(Some(v)) => out.push(*v),
        WitnessKind::IndexAssign {
            object,
            index,
            value,
        } => {
            out.push(*object);
            out.push(*index);
            out.push(*value);
        }
        WitnessKind::Handle { body, handler, .. }
        | WitnessKind::Transaction {
            body,
            compensation: handler,
        } => {
            out.push(*body);
            out.push(*handler);
        }
        WitnessKind::Eval { given, expects, .. } => {
            out.push(*given);
            out.extend(expects);
        }
        WitnessKind::Spawn { target } => match target {
            WitnessSpawnTarget::Call {
                callee,
                args,
                named_args,
            } => {
                if let WitnessCallee::Evaluated(e) = callee {
                    out.push(*e);
                }
                out.extend(args);
                for (_, v) in named_args {
                    out.push(v);
                }
            }
            WitnessSpawnTarget::MethodCall {
                receiver,
                args,
                named_args,
                ..
            } => {
                out.push(*receiver);
                out.extend(args);
                for (_, v) in named_args {
                    out.push(v);
                }
            }
            WitnessSpawnTarget::Expr(e) => out.push(*e),
        },
        WitnessKind::Select { arms } => {
            for arm in arms {
                match arm {
                    WitnessSelectArm::Recv { channel, body, .. } => {
                        out.push(*channel);
                        out.push(*body);
                    }
                    WitnessSelectArm::Send {
                        channel,
                        value,
                        body,
                    } => {
                        out.push(*channel);
                        out.push(*value);
                        out.push(*body);
                    }
                    WitnessSelectArm::Timeout { ms, body } => {
                        out.push(*ms);
                        out.push(*body);
                    }
                    WitnessSelectArm::Default { body } => out.push(*body),
                }
            }
        }
    }
}

/// W2.3: Iterative `Drop` so freeing deep witness chains (e.g. long Pratt or unary chains)
/// uses a heap stack instead of overflowing the OS thread stack.
impl Drop for MirWitness {
    fn drop(&mut self) {
        if matches!(
            self.kind,
            WitnessKind::Literal(_)
                | WitnessKind::Variable(_)
                | WitnessKind::Break(_)
                | WitnessKind::Continue(_)
                | WitnessKind::Import(_)
                | WitnessKind::TypeAlias { .. }
                | WitnessKind::EnumDef { .. }
                | WitnessKind::EffectSig { .. }
                | WitnessKind::Commit
                | WitnessKind::Rollback
                | WitnessKind::Return(None)
        ) {
            return;
        }
        let mut stack: Vec<MirWitness> = Vec::new();
        let root_kind = std::mem::replace(&mut self.kind, WitnessKind::Commit);
        push_owned_witness_children(root_kind, &mut stack);
        while let Some(mut node) = stack.pop() {
            let kind = std::mem::replace(&mut node.kind, WitnessKind::Commit);
            push_owned_witness_children(kind, &mut stack);
        }
    }
}

/// builtin 操作码 —— 由 `WitnessCallee::Builtin` 携带，供 typeck HM 推断消费。
#[derive(Debug, Clone, PartialEq)]
pub enum BuiltinOp {
    Print,
    Assert,
    Not,
    Neg,
    Length,
}

/// Match arm —— 模式 + 可选守卫 + 臂体。
#[derive(Debug, Clone, PartialEq)]
pub struct WitnessArm {
    pub pattern: WitnessPattern,
    pub guard: Option<MirWitness>,
    pub body: MirWitness,
}

/// Pattern 变体 —— 通配 / 变量 / 字面量 / 元组 / 列表 / dict / 类型标注 / 枚举变体 / 结构体。
#[derive(Debug, Clone, PartialEq)]
pub enum WitnessPattern {
    Wildcard,
    Variable(String),
    Literal(Literal),
    Tuple(Vec<WitnessPattern>),
    List {
        head: Box<WitnessPattern>,
        tail: Box<WitnessPattern>,
    },
    /// v0.87: List vector pattern `[a, b, ..rest]`
    ListVec {
        elements: Vec<WitnessPattern>,
        rest: Option<Box<WitnessPattern>>,
    },
    Dict {
        required: Vec<(String, WitnessPattern)>,
        rest: bool,
    },
    TypeAscription {
        name: String,
        pattern: Box<WitnessPattern>,
    },
    /// Wave 3 Stage 3C: Enum variant pattern (`Enum::Variant` or `Enum::Variant(p1, p2)`).
    Variant {
        enum_name: Option<String>,
        variant: String,
        fields: Vec<WitnessPattern>,
    },
    /// Wave 3 Stage 3C: Nominal struct destructuring pattern (`Point { x: a, y: b }` or `Point { x, .. }`).
    Struct {
        name: String,
        fields: Vec<(String, WitnessPattern)>,
        rest: bool,
    },
}

/// 参数 —— 形参名 + 可选类型注解 + 可选默认值 witness。
#[derive(Debug, Clone, PartialEq)]
pub struct WitnessParam {
    pub name: String,
    pub type_hint: Option<crate::mir::hint::TypeHint>,
    pub default: Option<MirWitness>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{BinaryOp, Literal};

    // 见证树测试：直接以 witness-native 构造，验证 witness 树自身的结构不变量。

    fn lit(n: i64) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Literal(Literal::Int(n, Span::default())),
            span: Span::default(),
        }
    }

    fn var(name: &str) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Variable(name.to_string()),
            span: Span::default(),
        }
    }

    /// witness 树 span 保留 + Binary 结构不变量。
    #[test]
    fn witness_preserves_kind_and_span() {
        let w = MirWitness {
            kind: WitnessKind::Binary {
                left: Box::new(lit(1)),
                op: BinaryOp::Add,
                right: Box::new(var("x")),
            },
            span: Span { line: 3, column: 7 },
        };
        assert_eq!(w.span.line, 3);
        assert_eq!(w.span.column, 7);
        match &w.kind {
            WitnessKind::Binary {
                op, left, right, ..
            } => {
                assert_eq!(*op, BinaryOp::Add);
                assert!(matches!(
                    left.kind,
                    WitnessKind::Literal(Literal::Int(1, _))
                ));
                assert!(matches!(right.kind, WitnessKind::Variable(ref n) if n == "x"));
            }
            other => panic!("expected Binary, got {other:?}"),
        }
    }

    /// 全变体族覆盖：witness 树构造不 panic 且变体类别正确。
    #[test]
    fn witness_covers_all_variant_families() {
        let witnesses = [
            MirWitness {
                kind: WitnessKind::Literal(Literal::String("s".into(), Span::default())),
                span: Span::default(),
            },
            var("a"),
            MirWitness {
                kind: WitnessKind::Binary {
                    left: Box::new(lit(1)),
                    op: BinaryOp::Add,
                    right: Box::new(lit(2)),
                },
                span: Span::default(),
            },
            MirWitness {
                kind: WitnessKind::Call {
                    callee: WitnessCallee::Name("f".into()),
                    args: vec![lit(1)],
                    named_args: Vec::new(),
                },
                span: Span::default(),
            },
            MirWitness {
                kind: WitnessKind::If {
                    cond: Box::new(var("c")),
                    then: Box::new(lit(1)),
                    r#else: Some(Box::new(lit(2))),
                },
                span: Span::default(),
            },
            MirWitness {
                kind: WitnessKind::List(vec![lit(1), lit(2)]),
                span: Span::default(),
            },
            MirWitness {
                kind: WitnessKind::Dict(vec![("k".into(), lit(1))]),
                span: Span::default(),
            },
            MirWitness {
                kind: WitnessKind::Sequence(vec![lit(1), lit(2)]),
                span: Span::default(),
            },
        ];
        assert_eq!(witnesses.len(), 8);
        assert!(matches!(witnesses[0].kind, WitnessKind::Literal(_)));
        assert!(matches!(witnesses[1].kind, WitnessKind::Variable(_)));
        assert!(matches!(witnesses[2].kind, WitnessKind::Binary { .. }));
        assert!(matches!(witnesses[3].kind, WitnessKind::Call { .. }));
        assert!(matches!(witnesses[4].kind, WitnessKind::If { .. }));
        assert!(matches!(witnesses[5].kind, WitnessKind::List(_)));
        assert!(matches!(witnesses[6].kind, WitnessKind::Dict(_)));
        assert!(matches!(witnesses[7].kind, WitnessKind::Sequence(_)));
    }

    /// Closure 不再含 captured_env。
    #[test]
    fn closure_has_no_captured_env() {
        let w = MirWitness {
            kind: WitnessKind::Closure {
                type_params: vec![],
                params: vec![],
                body: Box::new(lit(1)),
            },
            span: Span::default(),
        };
        assert!(matches!(w.kind, WitnessKind::Closure { .. }));
    }

    #[test]
    fn deep_chain_drop_is_iterative() {
        let mut w = lit(1);
        for _ in 0..20_000 {
            w = MirWitness {
                kind: WitnessKind::Binary {
                    left: Box::new(w),
                    op: BinaryOp::Add,
                    right: Box::new(lit(1)),
                },
                span: Span::default(),
            };
        }
        drop(w);
    }
}
