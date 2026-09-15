//! v0.89: FCFG (Frontend Control Flow Graph) — 9 层 IR 架构第 1 层。
//!
//! `Node<M>` 是 IR 管线的泛型基础节点。通过元数据参数 `M` 区分阶段：
//! - `Node<()>` = FCFG（无类型，parser 直接输出）
//! - `Node<TypeInfo>` = EHIR（带类型标签，typeck 后）
//!
//! 设计原则（来自用户 5 点验证建议 #1）：
//! - FCFG 和 EHIR 物理合并为 `Node<M>` 泛型，逻辑层保持分离
//! - 遍历逻辑只写一次，`Node<()>` 时忽略 meta，`Node<TypeInfo>` 时消费 meta
//! - 结构化控制流（If/While/For/Match），非 Jump/Label
//!
//! 与现有 IR 的关系：
//! - WitnessKind (30 变体) → Node<M> 的子集映射（树形 → 结构化 CFG；typeck 消费面）
//! - MirInst (50 变体) → Node<M> 不直接映射（MirInst 是线性的，Node 是结构化的）

use crate::common::{BinaryOp, Literal, Span};
use crate::mir::effect::EffectRow;
use crate::typeck::Type;

// ===================================================================
// Node<M> — 泛型 IR 节点
// ===================================================================

/// 泛型 IR 节点 — 通过 `M` 参数区分阶段。
///
/// - `M = ()`：FCFG（无类型信息）
/// - `M = TypeInfo`：EHIR（带类型标签）
///
/// 每个节点有一个 `meta: M` 字段，遍历时可选择性消费或忽略。
/// `span` 是源码位置，所有层共享（用于错误报告和影子表映射）。
#[derive(Debug, Clone)]
pub enum Node<M> {
    // ── 值产生 ──
    Literal {
        reg: Reg,
        value: Literal,
        span: Span,
        meta: M,
    },
    Variable {
        reg: Reg,
        name: String,
        span: Span,
        meta: M,
    },
    BinaryOp {
        dst: Reg,
        lhs: Reg,
        op: BinaryOp,
        rhs: Reg,
        span: Span,
        meta: M,
    },
    Call {
        dst: Reg,
        callee: Reg,
        /// 函数名（已知时填充，桥接层直接使用）。
        callee_name: Option<String>,
        args: Vec<Reg>,
        span: Span,
        meta: M,
    },
    MethodCall {
        dst: Reg,
        receiver: Reg,
        method: String,
        args: Vec<Reg>,
        span: Span,
        meta: M,
    },
    /// 逻辑或（短路求值）。WitnessKind::Or 对应。
    Or {
        dst: Reg,
        lhs: Reg,
        rhs: Reg,
        span: Span,
        meta: M,
    },
    /// 逻辑与（短路求值）。WitnessKind::And 对应。
    And {
        dst: Reg,
        lhs: Reg,
        rhs: Reg,
        span: Span,
        meta: M,
    },
    /// dyn Trait 强制转换。WitnessKind::DynTrait 对应。
    DynTrait {
        dst: Reg,
        src: Reg,
        trait_name: String,
        span: Span,
        meta: M,
    },
    /// AI prompt 模板 p"..."。WitnessKind::Prompt 对应。
    Prompt {
        dst: Reg,
        parts: Vec<Reg>,
        span: Span,
        meta: M,
    },
    /// 匿名闭包表达式 fn(x) ... end。WitnessKind::Closure 对应。
    ClosureExpr {
        dst: Reg,
        params: Vec<Param>,
        body: Block<M>,
        span: Span,
        meta: M,
    },
    ListLit {
        dst: Reg,
        items: Vec<Reg>,
        span: Span,
        meta: M,
    },
    DictLit {
        dst: Reg,
        entries: Vec<(String, Reg)>,
        span: Span,
        meta: M,
    },
    Index {
        dst: Reg,
        obj: Reg,
        idx: Reg,
        span: Span,
        meta: M,
    },

    // ── 控制流（结构化，非 Jump/Label）──
    If {
        cond: Reg,
        then: Block<M>,
        else_: Option<Block<M>>,
        span: Span,
        meta: M,
    },
    While {
        cond: Block<M>,
        body: Block<M>,
        span: Span,
        meta: M,
    },
    For {
        var: String,
        iter: Reg,
        body: Block<M>,
        span: Span,
        meta: M,
    },
    Match {
        /// match 表达式的统一结果寄存器 — 所有 arm 的 output_reg
        /// 共用此寄存器（h_match_expr 写入，消费者读取）。
        dst: Reg,
        scrutinee: Reg,
        arms: Vec<MatchArm<M>>,
        span: Span,
        meta: M,
    },
    Return {
        value: Option<Reg>,
        span: Span,
        meta: M,
    },
    Break {
        /// v0.90.4: 跳转 label（witness_to_fcfg 用 loop_stack 填）。
        /// fcfg_lower 不再依赖独立 loop_stack（label 由节点携带）。
        label: usize,
        span: Span,
        meta: M,
    },
    Continue {
        label: usize,
        span: Span,
        meta: M,
    },

    // ── 绑定 ──
    Let {
        name: String,
        type_ann: Option<TypeAnnotation>,
        value: Reg,
        body: Block<M>,
        span: Span,
        meta: M,
    },
    Assign {
        name: String,
        value: Reg,
        span: Span,
        meta: M,
    },
    IndexAssign {
        obj: Reg,
        idx: Reg,
        value: Reg,
        span: Span,
        meta: M,
    },

    // ── 声明（编译期注册，不进运行时）──
    FnDef {
        name: String,
        params: Vec<Param>,
        return_ann: Option<TypeAnnotation>,
        body: Block<M>,
        span: Span,
        meta: M,
    },
    TypeAlias {
        name: String,
        target: TypeAnnotation,
        span: Span,
        meta: M,
    },
    EnumDef {
        name: String,
        variants: Vec<Variant>,
        span: Span,
        meta: M,
    },
    StructDef {
        name: String,
        fields: Vec<(String, TypeAnnotation)>,
        span: Span,
        meta: M,
    },
    TraitDef {
        name: String,
        methods: Vec<TraitMethod>,
        span: Span,
        meta: M,
    },
    ImplDef {
        trait_name: String,
        for_type: TypeAnnotation,
        methods: Vec<(String, Block<M>)>,
        span: Span,
        meta: M,
    },
    Import {
        path: String,
        span: Span,
        meta: M,
    },
    MacroDef {
        name: String,
        params: Vec<String>,
        body: Block<M>,
        span: Span,
        meta: M,
    },

    // ── 代数效果 ──
    Perform {
        /// perform 表达式的结果寄存器（handler 决定具体值）。
        dst: Reg,
        effect: String,
        args: Vec<Reg>,
        span: Span,
        meta: M,
    },
    Handle {
        effect: String,
        body: Block<M>,
        handler: Block<M>,
        k_param: String,
        span: Span,
        meta: M,
    },

    // ── TEA ──
    ModelDef {
        name: String,
        fields: Vec<(String, TypeAnnotation)>,
        span: Span,
        meta: M,
    },
    MsgDef {
        name: String,
        variants: Vec<Variant>,
        span: Span,
        meta: M,
    },
    UpdateDef {
        name: String,
        params: Vec<Param>,
        body: Block<M>,
        span: Span,
        meta: M,
    },
    AppDef {
        name: String,
        model: String,
        msg: String,
        init: Block<M>,
        update: Block<M>,
        view: Block<M>,
        span: Span,
        meta: M,
    },
    // ── v0.102: 声明式范式（逻辑式/关系式）──
    /// 关系定义 —— 编译期子句模板数据随节点携带（lower 直接取用）。
    RelDef {
        name: String,
        clauses: Vec<crate::rel::Clause>,
        span: Span,
        meta: M,
    },
    /// solve 查询 —— goal 是目标构建体块。
    Solve {
        limit: Option<usize>,
        query_vars: Vec<String>,
        anon_vars: Vec<String>,
        goal: Block<M>,
        span: Span,
        meta: M,
    },
    // ── v0.103: 命名 section 声明 ──
    PromptSection {
        name: String,
        body: Block<M>,
        span: Span,
        meta: M,
    },
    DocumentSection {
        name: String,
        body: Block<M>,
        span: Span,
        meta: M,
    },
    // ── v0.103: 可观测性块 ──
    Observe {
        config: String,
        body: Block<M>,
        span: Span,
        meta: M,
    },
    Span {
        name: String,
        tags: Vec<(String, String)>,
        body: Block<M>,
        span: Span,
        meta: M,
    },
    Parallel {
        body: Block<M>,
        span: Span,
        meta: M,
    },
    Export {
        names: Vec<String>,
        decl: Block<M>,
        span: Span,
        meta: M,
    },

    // ── 元编程 ──
    Quasiquote {
        dst: Reg,
        segments: Vec<QuasiquoteSegment>,
        span: Span,
        meta: M,
    },

    // ── 编排 ──
    Orchestrate {
        input_var: String,
        result_var: String,
        kind: OrchestrateKind<M>,
        span: Span,
        meta: M,
    },

    // ── 配置块 ──
    WithConfig {
        bindings: Vec<(String, Reg)>,
        body: Block<M>,
        span: Span,
        meta: M,
    },

    // ── 序列 ──
    Sequence {
        nodes: Vec<Node<M>>,
        span: Span,
        meta: M,
    },

    // ── 表达式语句（丢弃结果）──
    Expr {
        reg: Reg,
        span: Span,
        meta: M,
    },
}

// ===================================================================
// 辅助类型
// ===================================================================

/// 寄存器标识符。
pub type Reg = usize;

/// 基本块 — 一组顺序执行的节点。
#[derive(Debug, Clone)]
pub struct Block<M> {
    pub nodes: Vec<Node<M>>,
    /// 块的最后一个寄存器（表达式块的值）。
    pub result: Option<Reg>,
}

/// 模式匹配分支。
#[derive(Debug, Clone)]
pub struct MatchArm<M> {
    pub pattern: Pattern,
    pub guard: Option<Reg>,
    pub body: Block<M>,
}

/// 函数参数。
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub type_ann: Option<TypeAnnotation>,
    pub default: Option<Literal>,
}

/// 类型注解（源码级，未解析）。
#[derive(Debug, Clone)]
pub struct TypeAnnotation(pub String);

/// 枚举/消息变体。
#[derive(Debug, Clone)]
pub struct Variant {
    pub name: String,
    pub payload: Option<TypeAnnotation>,
}

/// Trait 方法声明。
#[derive(Debug, Clone)]
pub struct TraitMethod {
    pub name: String,
    pub params: Vec<Param>,
    pub return_ann: Option<TypeAnnotation>,
}

/// 模式。
#[derive(Debug, Clone)]
pub enum Pattern {
    Wildcard,
    Variable(String),
    Literal(Literal),
    Tuple(Vec<Pattern>),
    List(Vec<Pattern>),
    ListVec { head: Vec<Pattern>, tail: Option<Box<Pattern>> },
    Dict(Vec<(String, Pattern)>),
    TypeAscription(Box<Pattern>, TypeAnnotation),
}

/// Quasiquote 段。
#[derive(Debug, Clone)]
pub enum QuasiquoteSegment {
    Quote(String),
    Unquote(Reg),
    UnquoteSplice(Reg),
}

/// 编排类型。
#[derive(Debug, Clone)]
pub enum OrchestrateKind<M> {
    Sequential,
    Loop { body: Block<M> },
    Graph { vertices: Vec<String>, edges: Vec<(String, String)> },
    Pregel { config: PregelConfig<M> },
    MoA { layers: Vec<MoALayer<M>> },
    MoE { experts: Vec<String>, router: String, top_k: usize },
}

/// Pregel 配置。
#[derive(Debug, Clone)]
pub struct PregelConfig<M> {
    pub vertices: Vec<String>,
    pub edges: Vec<(String, String)>,
    pub compute: Block<M>,
    pub combine: Option<Block<M>>,
    pub max_supersteps: Option<usize>,
}

/// MoA 层。
#[derive(Debug, Clone)]
pub struct MoALayer<M> {
    pub proposers: Vec<Block<M>>,
    pub aggregator: Block<M>,
}

// ===================================================================
// EHIR 元数据
// ===================================================================

/// EHIR 阶段的元数据 — 每个节点携带类型信息。
#[derive(Debug, Clone)]
pub struct TypeInfo {
    /// 推断出的类型。
    pub ty: Type,
    /// 该节点的效果行。
    pub effects: EffectRow,
    /// 源码位置。
    pub span: Span,
}

impl TypeInfo {
    /// 创建带类型的 TypeInfo。
    pub fn new(ty: Type, effects: EffectRow, span: Span) -> Self {
        Self { ty, effects, span }
    }

    /// 创建未知类型的 TypeInfo（TypeAny + Empty effect）。
    pub fn unknown(span: Span) -> Self {
        Self {
            ty: Type::Any,
            effects: EffectRow::Empty,
            span,
        }
    }
}

// ===================================================================
// 类型别名
// ===================================================================

/// FCFG — 无类型的前端控制流图。
pub type Fcfg = Node<()>;

/// EHIR — 带类型的早期高 IR。
pub type Ehir = Node<TypeInfo>;

/// FCFG 块。
pub type FcfgBlock = Block<()>;

/// EHIR 块。
pub type EhirBlock = Block<TypeInfo>;

// ===================================================================
// Node<M> 辅助方法
// ===================================================================

impl<M> Node<M> {
    /// 获取节点的源码位置。
    pub fn span(&self) -> Span {
        match self {
            Node::Literal { span, .. }
            | Node::Variable { span, .. }
            | Node::BinaryOp { span, .. }
            | Node::Call { span, .. }
            | Node::MethodCall { span, .. }
            | Node::Or { span, .. }
            | Node::And { span, .. }
            | Node::DynTrait { span, .. }
            | Node::Prompt { span, .. }
            | Node::ClosureExpr { span, .. }
            | Node::ListLit { span, .. }
            | Node::DictLit { span, .. }
            | Node::Index { span, .. }
            | Node::If { span, .. }
            | Node::While { span, .. }
            | Node::For { span, .. }
            | Node::Match { span, .. }
            | Node::Return { span, .. }
            | Node::Break { span, .. }
            | Node::Continue { span, .. }
            | Node::Let { span, .. }
            | Node::Assign { span, .. }
            | Node::IndexAssign { span, .. }
            | Node::FnDef { span, .. }
            | Node::TypeAlias { span, .. }
            | Node::EnumDef { span, .. }
            | Node::StructDef { span, .. }
            | Node::TraitDef { span, .. }
            | Node::ImplDef { span, .. }
            | Node::Import { span, .. }
            | Node::MacroDef { span, .. }
            | Node::Perform { span, .. }
            | Node::Handle { span, .. }
            | Node::ModelDef { span, .. }
            | Node::MsgDef { span, .. }
            | Node::UpdateDef { span, .. }
            | Node::AppDef { span, .. }
            | Node::RelDef { span, .. }
            | Node::Solve { span, .. }
            | Node::PromptSection { span, .. }
            | Node::DocumentSection { span, .. }
            | Node::Observe { span, .. }
            | Node::Span { span, .. }
            | Node::Parallel { span, .. }
            | Node::Export { span, .. }
            | Node::Quasiquote { span, .. }
            | Node::Orchestrate { span, .. }
            | Node::WithConfig { span, .. }
            | Node::Sequence { span, .. }
            | Node::Expr { span, .. } => *span,
        }
    }

    /// 获取节点的 meta 引用。
    pub fn meta(&self) -> &M {
        match self {
            Node::Literal { meta, .. }
            | Node::Variable { meta, .. }
            | Node::BinaryOp { meta, .. }
            | Node::Call { meta, .. }
            | Node::MethodCall { meta, .. }
            | Node::Or { meta, .. }
            | Node::And { meta, .. }
            | Node::DynTrait { meta, .. }
            | Node::Prompt { meta, .. }
            | Node::ClosureExpr { meta, .. }
            | Node::ListLit { meta, .. }
            | Node::DictLit { meta, .. }
            | Node::Index { meta, .. }
            | Node::If { meta, .. }
            | Node::While { meta, .. }
            | Node::For { meta, .. }
            | Node::Match { meta, .. }
            | Node::Return { meta, .. }
            | Node::Break { meta, .. }
            | Node::Continue { meta, .. }
            | Node::Let { meta, .. }
            | Node::Assign { meta, .. }
            | Node::IndexAssign { meta, .. }
            | Node::FnDef { meta, .. }
            | Node::TypeAlias { meta, .. }
            | Node::EnumDef { meta, .. }
            | Node::StructDef { meta, .. }
            | Node::TraitDef { meta, .. }
            | Node::ImplDef { meta, .. }
            | Node::Import { meta, .. }
            | Node::MacroDef { meta, .. }
            | Node::Perform { meta, .. }
            | Node::Handle { meta, .. }
            | Node::ModelDef { meta, .. }
            | Node::MsgDef { meta, .. }
            | Node::UpdateDef { meta, .. }
            | Node::AppDef { meta, .. }
            | Node::RelDef { meta, .. }
            | Node::Solve { meta, .. }
            | Node::PromptSection { meta, .. }
            | Node::DocumentSection { meta, .. }
            | Node::Observe { meta, .. }
            | Node::Span { meta, .. }
            | Node::Parallel { meta, .. }
            | Node::Export { meta, .. }
            | Node::Quasiquote { meta, .. }
            | Node::Orchestrate { meta, .. }
            | Node::WithConfig { meta, .. }
            | Node::Sequence { meta, .. }
            | Node::Expr { meta, .. } => meta,
        }
    }
}
