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
//! WitnessParam（Param）、WitnessPattern（Pattern）、WitnessOrchestrateKind
//! （MirOrchestrateKind）、WitnessAgentDef / WitnessEdgeDef。

use crate::common::{BinaryOp, Literal, Span};
use crate::mir::MirFunction;
use crate::mir::orchestrate::{MirAgentDef, MirEdgeDef, MirOrchestrateKind};

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
    },
    MethodCall {
        receiver: Box<MirWitness>,
        method: String,
        args: Vec<MirWitness>,
    },
    Closure {
        params: Vec<WitnessParam>,
        body: Box<MirWitness>,
    },
    FnDef {
        name: String,
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
    Prompt {
        parts: Vec<MirWitness>,
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
    Orchestrate {
        input_var: String,
        result_var: String,
        kind: Box<WitnessOrchestrateKind>,
    },
    TypeAlias {
        name: String,
        target: crate::mir::hint::TypeHint,
    },
    EnumDef {
        name: String,
        variants: Vec<String>,
    },
    StructDef {
        name: String,
        fields: Vec<(String, crate::mir::hint::TypeHint)>,
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
    // v0.83: TEA (The Elm Architecture) 语法糖 witness
    /// Model 定义 — 状态结构（类似 StructDef，但语义是 Model 容器）
    ModelDef {
        name: String,
        fields: Vec<(String, crate::mir::hint::TypeHint)>,
    },
    /// Msg 定义 — tagged union（每个变体可携带 payload 类型）
    MsgDef {
        name: String,
        variants: Vec<crate::common::MsgVariant>,
    },
    /// Update 函数 — `(Model, Msg) -> (Model, Cmd)`
    UpdateDef {
        name: String,
        params: Vec<crate::mir::witness::WitnessParam>,
        return_type: Option<crate::mir::hint::TypeHint>,
        body: Box<MirWitness>,
    },
    /// App 定义 — 完整 TEA app
    AppDef {
        name: String,
        model_name: String,
        msg_name: String,
        init_w: Box<MirWitness>,
        update_w: Box<MirWitness>,
        view_w: Box<MirWitness>,
    },
    // v0.85: with 块（配置桥接）— 镜像 MirInst::WithConfig
    WithConfig {
        bindings: Vec<(String, MirWitness)>,
        body: Box<MirWitness>,
    },
    // v0.88: Quasiquote（反引号 `expr + ,unquote / ,,unquote-splice）
    /// segments 用 MirWitness 编码：Quote = Literal(String),
    /// Unquote = 被求值的子表达式 witness,
    /// UnquoteSplice = 子表达式 witness + Literal(Boolean("splice")) 标记。
    Quasiquote {
        segments: Vec<MirWitness>,
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

/// v0.92: builtin 操作码（原在 `mir/expr/mod.rs`）。
/// WitnessCallee::Builtin 与 typeck HM 推断消费；与表达式树无关，故迁至 witness 层。
#[derive(Debug, Clone, PartialEq)]
pub enum BuiltinOp {
    Print,
    Assert,
    Not,
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
}

/// 参数 — 镜像 Param。
#[derive(Debug, Clone, PartialEq)]
pub struct WitnessParam {
    pub name: String,
    pub type_hint: Option<crate::mir::hint::TypeHint>,
    pub default: Option<MirWitness>,
}

/// Orchestrate 种类 — 镜像 MirOrchestrateKind。
#[derive(Debug, Clone, PartialEq)]
pub enum WitnessOrchestrateKind {
    Sequential {
        agents: Vec<WitnessAgentDef>,
    },
    Loop {
        agents: Vec<WitnessAgentDef>,
        rounds: Option<u64>,
        exit_when: Option<MirWitness>,
    },
    Graph {
        agents: Vec<WitnessAgentDef>,
        edges: Vec<WitnessEdgeDef>,
    },
    Pregel {
        agents: Vec<WitnessAgentDef>,
        edges: Vec<WitnessEdgeDef>,
        state_schema: Vec<crate::mir::orchestrate::MirStateChannel>,
        checkpoint: Option<crate::mir::orchestrate::MirCheckpointConfig>,
        interrupt_points: Vec<crate::mir::orchestrate::MirInterruptPoint>,
        adjacency: std::collections::HashMap<String, Vec<String>>,
    },
    /// v0.75.84: MoA（Mixture-of-Agents）— 分层多模型协作声明。
    Moa {
        layers: usize,
        proposers: Vec<String>,
        aggregator: String,
        prompt: Box<MirWitness>,
    },
    /// v0.75.85: MoE（Mixture-of-Experts）— 稀疏门控声明。
    Moe {
        experts: Vec<MirWitness>,
        router: Box<MirWitness>,
        top_k: usize,
        prompt: Box<MirWitness>,
    },
}

impl WitnessOrchestrateKind {
    pub fn from_kind(kind: &MirOrchestrateKind) -> WitnessOrchestrateKind {
        match kind {
            MirOrchestrateKind::Sequential { agents } => WitnessOrchestrateKind::Sequential {
                agents: agents.iter().map(WitnessAgentDef::from_agent).collect(),
            },
            MirOrchestrateKind::Loop {
                agents,
                rounds,
                exit_when,
            } => WitnessOrchestrateKind::Loop {
                agents: agents.iter().map(WitnessAgentDef::from_agent).collect(),
                rounds: *rounds,
                exit_when: exit_when.clone(),
            },
            MirOrchestrateKind::Graph { agents, edges } => WitnessOrchestrateKind::Graph {                agents: agents.iter().map(WitnessAgentDef::from_agent).collect(),
                edges: edges.iter().map(WitnessEdgeDef::from_edge).collect(),
            },
            MirOrchestrateKind::Pregel {
                agents,
                edges,
                state_schema,
                checkpoint,
                interrupt_points,
                adjacency,
            } => WitnessOrchestrateKind::Pregel {
                agents: agents.iter().map(WitnessAgentDef::from_agent).collect(),
                edges: edges.iter().map(WitnessEdgeDef::from_edge).collect(),
                state_schema: state_schema.clone(),
                checkpoint: checkpoint.clone(),
                interrupt_points: interrupt_points.clone(),
                adjacency: adjacency.clone(),
            },
            // v0.75.84: MoA — 编译到 pregel 图，witness 记录声明参数。
            MirOrchestrateKind::Moa {
                layers,
                proposers,
                aggregator,
                prompt,
                prompt_fn: _,
                ..
            } => WitnessOrchestrateKind::Moa {
                layers: *layers,
                proposers: proposers.clone(),
                aggregator: aggregator.clone(),
                prompt: Box::new(prompt.clone()),
            },
            // v0.75.85: MoE — 稀疏门控（router 打分 → top-k → 加权）。
            MirOrchestrateKind::Moe {
                experts,
                router,
                top_k,
                prompt,
                router_fn: _,
                prompt_fn: _,
                ..
            } => WitnessOrchestrateKind::Moe {
                experts: experts.iter().map(|e| e.def.clone()).collect(),
                router: Box::new(router.clone()),
                top_k: *top_k,
                prompt: Box::new(prompt.clone()),
            },
        }
    }
}

/// Agent 定义 — 镜像 MirAgentDef。
#[derive(Debug, Clone, PartialEq)]
pub struct WitnessAgentDef {
    pub name: String,
    pub task_expr: MirWitness,
    pub verify_expr: Option<MirWitness>,
    pub with_config: Option<std::collections::HashMap<String, MirWitness>>,
    pub task_body: MirFunction,
    pub combiner_body: Option<MirFunction>,
}

impl WitnessAgentDef {
    fn from_agent(agent: &MirAgentDef) -> WitnessAgentDef {
        WitnessAgentDef {
            name: agent.name.clone(),
            task_expr: agent.task_expr.clone(),
            verify_expr: agent.verify_expr.clone(),
            with_config: agent.with_config.clone(),
            task_body: agent.task_body.clone(),
            combiner_body: agent.combiner_body.clone(),
        }
    }
}

/// Edge 定义 — 镜像 MirEdgeDef。
#[derive(Debug, Clone, PartialEq)]
pub struct WitnessEdgeDef {
    pub from: String,
    pub to: String,
    pub condition_expr: Option<MirWitness>,
    pub condition_body: Option<MirFunction>,
}

impl WitnessEdgeDef {
    fn from_edge(edge: &MirEdgeDef) -> WitnessEdgeDef {
        WitnessEdgeDef {
            from: edge.from.clone(),
            to: edge.to.clone(),
            condition_expr: edge.condition_expr.clone(),
            condition_body: edge.condition_body.clone(),
        }
    }
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
                params: vec![],
                body: Box::new(lit(1)),
            },
            span: Span::default(),
        };
        assert!(matches!(w.kind, WitnessKind::Closure { .. }));
    }
}
