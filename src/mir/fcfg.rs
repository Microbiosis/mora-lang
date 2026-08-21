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
//! - MirExprKind (26 变体) → Node<M> 的子集映射（树形 → 结构化 CFG）
//! - MirInst (50 变体) → Node<M> 不直接映射（MirInst 是线性的，Node 是结构化的）
//! - WitnessKind (30 变体) → Node<M> 的子集映射（typeck 消费面）

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
#[derive(Debug, Clone)]
pub enum Node<M> {
    // ── 值产生 ──
    Literal {
        reg: Reg,
        value: Literal,
        meta: M,
    },
    Variable {
        reg: Reg,
        name: String,
        meta: M,
    },
    BinaryOp {
        dst: Reg,
        lhs: Reg,
        op: BinaryOp,
        rhs: Reg,
        meta: M,
    },
    Call {
        dst: Reg,
        callee: Reg,
        args: Vec<Reg>,
        meta: M,
    },
    MethodCall {
        dst: Reg,
        receiver: Reg,
        method: String,
        args: Vec<Reg>,
        meta: M,
    },
    ListLit {
        dst: Reg,
        items: Vec<Reg>,
        meta: M,
    },
    DictLit {
        dst: Reg,
        entries: Vec<(String, Reg)>,
        meta: M,
    },
    Index {
        dst: Reg,
        obj: Reg,
        idx: Reg,
        meta: M,
    },

    // ── 控制流（结构化，非 Jump/Label）──
    If {
        cond: Reg,
        then: Block<M>,
        else_: Option<Block<M>>,
        meta: M,
    },
    While {
        cond: Block<M>,
        body: Block<M>,
        meta: M,
    },
    For {
        var: String,
        iter: Reg,
        body: Block<M>,
        meta: M,
    },
    Match {
        scrutinee: Reg,
        arms: Vec<MatchArm<M>>,
        meta: M,
    },
    Return {
        value: Option<Reg>,
        meta: M,
    },
    Break {
        meta: M,
    },
    Continue {
        meta: M,
    },

    // ── 绑定 ──
    Let {
        name: String,
        type_ann: Option<TypeAnnotation>,
        value: Reg,
        body: Block<M>,
        meta: M,
    },
    Assign {
        name: String,
        value: Reg,
        meta: M,
    },
    IndexAssign {
        obj: Reg,
        idx: Reg,
        value: Reg,
        meta: M,
    },

    // ── 声明（编译期注册，不进运行时）──
    FnDef {
        name: String,
        params: Vec<Param>,
        return_ann: Option<TypeAnnotation>,
        body: Block<M>,
        meta: M,
    },
    TypeAlias {
        name: String,
        target: TypeAnnotation,
        meta: M,
    },
    EnumDef {
        name: String,
        variants: Vec<Variant>,
        meta: M,
    },
    StructDef {
        name: String,
        fields: Vec<(String, TypeAnnotation)>,
        meta: M,
    },
    TraitDef {
        name: String,
        methods: Vec<TraitMethod>,
        meta: M,
    },
    ImplDef {
        trait_name: String,
        for_type: TypeAnnotation,
        methods: Vec<(String, Block<M>)>,
        meta: M,
    },
    Import {
        path: String,
        meta: M,
    },
    MacroDef {
        name: String,
        params: Vec<String>,
        body: Block<M>,
        meta: M,
    },

    // ── 代数效果 ──
    Perform {
        effect: String,
        args: Vec<Reg>,
        meta: M,
    },
    Handle {
        effect: String,
        body: Block<M>,
        handler: Block<M>,
        k_param: String,
        meta: M,
    },

    // ── TEA ──
    ModelDef {
        name: String,
        fields: Vec<(String, TypeAnnotation)>,
        meta: M,
    },
    MsgDef {
        name: String,
        variants: Vec<Variant>,
        meta: M,
    },
    UpdateDef {
        name: String,
        params: Vec<Param>,
        body: Block<M>,
        meta: M,
    },
    AppDef {
        name: String,
        model: String,
        msg: String,
        init: Block<M>,
        update: Block<M>,
        view: Block<M>,
        meta: M,
    },

    // ── 元编程 ──
    Quasiquote {
        dst: Reg,
        segments: Vec<QuasiquoteSegment>,
        meta: M,
    },

    // ── 编排 ──
    Orchestrate {
        input_var: String,
        result_var: String,
        kind: OrchestrateKind<M>,
        meta: M,
    },

    // ── 配置块 ──
    WithConfig {
        bindings: Vec<(String, Reg)>,
        body: Block<M>,
        meta: M,
    },

    // ── 序列 ──
    Sequence {
        nodes: Vec<Node<M>>,
        meta: M,
    },

    // ── 表达式语句（丢弃结果）──
    Expr {
        reg: Reg,
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
