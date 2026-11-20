//! v0.75.38: MirWitness — 轻量树骨架（typeck/LSP 消费面）。
//!
//! 去 AST 化终局的中间层：ParserV3 直接 emit MirInst（阶段 3），
//! 但 typeck（HM 推断）与 LSP（folding/semantic/definition/rename）
//! 需要语法树骨架。`MirWitness` 是 canonical 纯树结构（kind + span），
//! **无执行语义**——执行永远走 MirInst。
//!
//! v0.92: 原 `MirExpr` 平行世界已完全删除。MirWitness 曾是镜像
//! `MirExprKind` 的产物，如今是唯一的 parse-tree 类型；`from_expr` 等
//! 前向转换函数已随 MirExpr 一并移除。
//! - **独立 WitnessKind 枚举**（30 变体，captured_env 已删零消费死字段）。
//!
//! 复合类型同步镜像：WitnessCallee（MirCallee）、WitnessArm（MatchArm）、
//! WitnessParam（Param）、WitnessPattern（Pattern）。

use crate::common::{BinaryOp, Literal, Span};

/// 轻量树骨架节点 — kind + span，无执行语义。
#[derive(Debug, Clone, PartialEq)]
pub struct MirWitness {
    pub kind: WitnessKind,
    pub span: Span,
}

/// Witness 树节点种类 — 30 变体（captured_env 已删）。
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
        variants: Vec<WitnessEnumVariant>,
    },
    StructDef {
        name: String,
        type_params: Vec<String>,
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
        trait_name: Option<String>,
        trait_generics: Vec<crate::mir::hint::TypeHint>,
        target_type: String,
        target_generics: Vec<crate::mir::hint::TypeHint>,
        methods: Vec<WitnessImplMethod>,
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
    Import(String),
    // v0.80: algebraic effects witness（Stage 2/4 落地）。
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
    // v0.88: Quasiquote（反引号 `expr + ,unquote / ,,unquote-splice）
    /// segments 用 MirWitness 编码：Quote = Literal(String),
    /// Unquote = 被求值的子表达式 witness,
    /// UnquoteSplice = 子表达式 witness + Literal(Boolean("splice")) 标记。
    Quasiquote {
        segments: Vec<MirWitness>,
    },
    // v0.98: 显式 effect 签名声明（纯类型层 —— 不产 MirInst）。
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

    // ── v0.104.6 D472：9 层管线补齐 ──
    //
    // 此前这 4 类构造在 witness 层**完全没有表示** —— parser 只发射 MirInst、
    // 把结构信息丢进一个扁平的 `Sequence`。于是 9 层降级链
    // （witness → FCFG → MIR）**结构上就拿不到**它们，
    // `worker` / `transaction` / `eval` / `aggregate` 必然回落旧路径。
    //
    // 补齐后，9 层成为**唯一**编译路径的前提才成立。
    /// worker 块 —— 与 `MirInst::Worker { name, body }` 同构。
    Worker {
        name: String,
        body: Box<MirWitness>,
    },
    /// transaction 块 —— 与 `MirInst::Transaction { body, compensation }` 同构。
    Transaction {
        body: Box<MirWitness>,
        compensation: Box<MirWitness>,
    },
    /// eval 断言 —— 与 `MirInst::Eval { name, given_reg, expects, ... }` 对应。
    Eval {
        name: String,
        given: Box<MirWitness>,
        expects: Vec<MirWitness>,
        tolerance: Option<f64>,
        replay_path: Option<String>,
    },
    /// aggregate 贡献 —— 与 `MirInst::Aggregate { name, value }` 对应。
    Aggregate {
        name: String,
        value: Box<MirWitness>,
    },

    /// 管道 `x |> f` —— 承载 `MirInst::Pipe` 的两个操作数。
    ///
    /// v0.104.6 D472：此前管道在 witness 层被脱糖成 `Call`
    /// （`typeck/hm/infer.rs` 的 v0.75.20 记档：「`|` 在 parse_pipe 脱糖为
    /// Call（right(left)），HM 走 infer_call」，并据此把 `WitnessKind::Pipe`
    /// 当"死变体"移除）—— 但 **emit 路径发射的是 `MirInst::Pipe` 指令**。
    /// 两者不同构 ⇒ 9 层降维出 `Call`、emit 出 `Pipe`，差分必红
    /// （实测 `let x = 1 |> print`：pipeline 6 条 / original 7 条）。
    /// 补回该变体，让指令与 witness 重新同构。
    ///
    /// 类型推断口径**保持不变**（见 `typeck/hm/mod.rs`）：右侧是裸标识符 ⇒
    /// 值应用 `f(x)`；右侧是任意表达式 ⇒ 内建 `|>(x, expr)`。
    Pipe {
        left: Box<MirWitness>,
        right: Box<MirWitness>,
    },
    /// `commit` 语句 —— 与 `MirInst::Commit` 同构（unit 指令）。
    Commit,
    /// `rollback` 语句 —— 与 `MirInst::Rollback` 同构（unit 指令）。
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

/// 调用目标 — 镜像 MirCallee。
#[derive(Debug, Clone, PartialEq)]
pub enum WitnessCallee {
    Name(String),
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
            WitnessKind::NoGrad { body } => vec![body.as_ref()],
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
        WitnessKind::NoGrad { body } => out.push(*body),
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

/// v0.92: builtin 操作码（原在 `mir/expr/mod.rs`）。
/// WitnessCallee::Builtin 与 typeck HM 推断消费；与表达式树无关，故迁至 witness 层。
#[derive(Debug, Clone, PartialEq)]
pub enum BuiltinOp {
    Print,
    Assert,
    Not,
    Neg,
    Length,
}

/// Match arm — 镜像 MatchArm。
#[derive(Debug, Clone, PartialEq)]
pub struct WitnessArm {
    pub pattern: WitnessPattern,
    pub guard: Option<MirWitness>,
    pub body: MirWitness,
}

/// Pattern 变体 — 镜像 Pattern。
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

/// 参数 — 镜像 Param。
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

    // v0.92: MirExpr → MirWitness 正向转换已删除（parser 直接产出 witness）。
    // 以下测试改为 witness-native 构造，验证 witness 树自身的结构不变量。

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
