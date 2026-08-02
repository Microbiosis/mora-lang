//! v0.75.38: MirWitness — 轻量树骨架（typeck/LSP 消费面）。
//!
//! 去 AST 化终局的中间层：ParserV3 最终直接 emit MirInst（阶段 3），
//! 但 typeck（HM 推断）与 LSP（folding/semantic/definition/rename）
//! 需要语法树骨架。`MirWitness` 是镜像 `MirExprKind` 的纯树结构
//! （kind + span），**无执行语义**——执行永远走 MirInst。
//!
//! 设计决策（阶段 2 用户确认）：
//! - **独立 WitnessKind 枚举**（镜像 MirExprKind 全部 30 变体），与
//!   MirExprKind 并存；阶段 3/4 消除 MirExpr 时 WitnessKind 胜出。
//! - **captured_env 已随 MirExprKind 删除**（零消费死字段）。
//! - 转换函数 `from_expr`：MirExpr → MirWitness 递归映射（30 变体
//!   逐一对应），往返一致性由单元测试锁定。
//!
//! 复合类型同步镜像：WitnessCallee（MirCallee）、WitnessArm（MatchArm）、
//! WitnessParam（Param）、WitnessPattern（Pattern）、WitnessOrchestrateKind
//! （MirOrchestrateKind）、WitnessAgentDef / WitnessEdgeDef。

use crate::common::{BinaryOp, Literal, Span};
use crate::mir::MirFunction;
use crate::mir::expr::{
    MatchArm, MirAgentDef, MirCallee, MirEdgeDef, MirExpr, MirExprKind, MirOrchestrateKind, Param,
    Pattern,
};

/// 轻量树骨架节点 — kind + span，无执行语义。
#[derive(Debug, Clone, PartialEq)]
pub struct MirWitness {
    pub kind: WitnessKind,
    pub span: Span,
}

impl MirWitness {
    /// 递归转换：MirExpr → MirWitness（30 变体逐一映射）。
    /// 阶段 3 parser 直接产出 witness 前，消费面经此桥接。
    pub fn from_expr(expr: &MirExpr) -> MirWitness {
        MirWitness {
            kind: WitnessKind::from_kind(&expr.kind),
            span: expr.span,
        }
    }

    /// 顶层序列转换辅助。
    pub fn from_exprs(exprs: &[MirExpr]) -> Vec<MirWitness> {
        exprs.iter().map(MirWitness::from_expr).collect()
    }
}

impl WitnessKind {
    fn from_kind(kind: &MirExprKind) -> WitnessKind {
        match kind {
            MirExprKind::Literal(lit) => WitnessKind::Literal(lit.clone()),
            MirExprKind::Variable(name) => WitnessKind::Variable(name.clone()),
            MirExprKind::Binary { left, op, right } => WitnessKind::Binary {
                left: Box::new(MirWitness::from_expr(left)),
                op: op.clone(),
                right: Box::new(MirWitness::from_expr(right)),
            },
            MirExprKind::Call { callee, args } => WitnessKind::Call {
                callee: WitnessCallee::from_callee(callee),
                args: args.iter().map(MirWitness::from_expr).collect(),
            },
            MirExprKind::MethodCall {
                receiver,
                method,
                args,
            } => WitnessKind::MethodCall {
                receiver: Box::new(MirWitness::from_expr(receiver)),
                method: method.clone(),
                args: args.iter().map(MirWitness::from_expr).collect(),
            },
            MirExprKind::Closure { params, body } => WitnessKind::Closure {
                params: params.iter().map(WitnessParam::from_param).collect(),
                body: Box::new(MirWitness::from_expr(body)),
            },
            MirExprKind::FnDef {
                name,
                params,
                return_type,
                body,
            } => WitnessKind::FnDef {
                name: name.clone(),
                params: params.iter().map(WitnessParam::from_param).collect(),
                return_type: return_type.clone(),
                body: Box::new(MirWitness::from_expr(body)),
            },
            MirExprKind::Match { scrutinee, arms } => WitnessKind::Match {
                scrutinee: Box::new(MirWitness::from_expr(scrutinee)),
                arms: arms.iter().map(WitnessArm::from_arm).collect(),
            },
            MirExprKind::If { cond, then, r#else } => WitnessKind::If {
                cond: Box::new(MirWitness::from_expr(cond)),
                then: Box::new(MirWitness::from_expr(then)),
                r#else: r#else.as_ref().map(|e| Box::new(MirWitness::from_expr(e))),
            },
            MirExprKind::Loop {
                var,
                iterable,
                body,
            } => WitnessKind::Loop {
                var: var.clone(),
                iterable: Box::new(MirWitness::from_expr(iterable)),
                body: Box::new(MirWitness::from_expr(body)),
            },
            MirExprKind::While { cond, body } => WitnessKind::While {
                cond: Box::new(MirWitness::from_expr(cond)),
                body: Box::new(MirWitness::from_expr(body)),
            },
            MirExprKind::Or { left, right } => WitnessKind::Or {
                left: Box::new(MirWitness::from_expr(left)),
                right: Box::new(MirWitness::from_expr(right)),
            },
            MirExprKind::And { left, right } => WitnessKind::And {
                left: Box::new(MirWitness::from_expr(left)),
                right: Box::new(MirWitness::from_expr(right)),
            },
            MirExprKind::List(items) => {
                WitnessKind::List(items.iter().map(MirWitness::from_expr).collect())
            }
            MirExprKind::Dict(entries) => WitnessKind::Dict(
                entries
                    .iter()
                    .map(|(k, v)| (k.clone(), MirWitness::from_expr(v)))
                    .collect(),
            ),
            MirExprKind::DynTrait {
                expr,
                trait_name,
                generics,
            } => WitnessKind::DynTrait {
                expr: Box::new(MirWitness::from_expr(expr)),
                trait_name: trait_name.clone(),
                generics: generics.clone(),
            },
            MirExprKind::Prompt { parts } => WitnessKind::Prompt {
                parts: parts.iter().map(MirWitness::from_expr).collect(),
            },
            MirExprKind::LetBinding {
                name,
                type_hint,
                value,
                init_body,
            } => WitnessKind::LetBinding {
                name: name.clone(),
                type_hint: type_hint.clone(),
                value: Box::new(MirWitness::from_expr(value)),
                init_body: Box::new(MirWitness::from_expr(init_body)),
            },
            MirExprKind::Assign { target, value } => WitnessKind::Assign {
                target: target.clone(),
                value: Box::new(MirWitness::from_expr(value)),
            },
            MirExprKind::IndexAssign {
                object,
                index,
                value,
            } => WitnessKind::IndexAssign {
                object: Box::new(MirWitness::from_expr(object)),
                index: Box::new(MirWitness::from_expr(index)),
                value: Box::new(MirWitness::from_expr(value)),
            },
            MirExprKind::Return(v) => {
                WitnessKind::Return(v.as_ref().map(|e| Box::new(MirWitness::from_expr(e))))
            }
            MirExprKind::Break(label) => WitnessKind::Break(label.clone()),
            MirExprKind::Continue(label) => WitnessKind::Continue(label.clone()),
            MirExprKind::Orchestrate {
                input_var,
                result_var,
                kind,
            } => WitnessKind::Orchestrate {
                input_var: input_var.clone(),
                result_var: result_var.clone(),
                kind: Box::new(WitnessOrchestrateKind::from_kind(kind)),
            },
            MirExprKind::TypeAlias { name, target } => WitnessKind::TypeAlias {
                name: name.clone(),
                target: target.clone(),
            },
            MirExprKind::EnumDef { name, variants } => WitnessKind::EnumDef {
                name: name.clone(),
                variants: variants.clone(),
            },
            MirExprKind::StructDef { name, fields } => WitnessKind::StructDef {
                name: name.clone(),
                fields: fields.clone(),
            },
            MirExprKind::Import(path) => WitnessKind::Import(path.clone()),
            MirExprKind::MacroDef { name, params } => WitnessKind::MacroDef {
                name: name.clone(),
                params: params.clone(),
            },
            MirExprKind::Sequence(exprs) => {
                WitnessKind::Sequence(exprs.iter().map(MirWitness::from_expr).collect())
            }
        }
    }
}

/// Witness 树节点种类 — 镜像 MirExprKind 全部 30 变体（captured_env 已删）。
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
        return_type: Option<crate::typeck::Type>,
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
        generics: Vec<crate::typeck::Type>,
    },
    Prompt {
        parts: Vec<MirWitness>,
    },
    LetBinding {
        name: String,
        type_hint: Option<crate::typeck::Type>,
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
        target: crate::typeck::Type,
    },
    EnumDef {
        name: String,
        variants: Vec<String>,
    },
    StructDef {
        name: String,
        fields: Vec<(String, crate::typeck::Type)>,
    },
    Import(String),
    MacroDef {
        name: String,
        params: Vec<String>,
    },
    Sequence(Vec<MirWitness>),
}

/// 调用目标 — 镜像 MirCallee。
#[derive(Debug, Clone, PartialEq)]
pub enum WitnessCallee {
    Name(String),
    Var(String),
    Method(String, String),
    Evaluated(Box<MirWitness>),
    Builtin(crate::mir::expr::BuiltinOp),
}

impl WitnessCallee {
    fn from_callee(callee: &MirCallee) -> WitnessCallee {
        match callee {
            MirCallee::Name(n) => WitnessCallee::Name(n.clone()),
            MirCallee::Var(n) => WitnessCallee::Var(n.clone()),
            MirCallee::Method(obj, m) => WitnessCallee::Method(obj.clone(), m.clone()),
            MirCallee::Evaluated(e) => WitnessCallee::Evaluated(Box::new(MirWitness::from_expr(e))),
            MirCallee::Builtin(op) => WitnessCallee::Builtin(op.clone()),
        }
    }
}

/// Match arm — 镜像 MatchArm。
#[derive(Debug, Clone, PartialEq)]
pub struct WitnessArm {
    pub pattern: WitnessPattern,
    pub guard: Option<MirWitness>,
    pub body: MirWitness,
}

impl WitnessArm {
    fn from_arm(arm: &MatchArm) -> WitnessArm {
        WitnessArm {
            pattern: WitnessPattern::from_pattern(&arm.pattern),
            guard: arm.guard.as_ref().map(MirWitness::from_expr),
            body: MirWitness::from_expr(&arm.body),
        }
    }
}

/// 模式匹配 — 镜像 Pattern。
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
    Dict {
        required: Vec<(String, WitnessPattern)>,
        rest: bool,
    },
    TypeAscription {
        name: String,
        pattern: Box<WitnessPattern>,
    },
}

impl WitnessPattern {
    pub fn from_pattern(p: &Pattern) -> WitnessPattern {
        match p {
            Pattern::Wildcard => WitnessPattern::Wildcard,
            Pattern::Variable(n) => WitnessPattern::Variable(n.clone()),
            Pattern::Literal(lit) => WitnessPattern::Literal(lit.clone()),
            Pattern::Tuple(items) => {
                WitnessPattern::Tuple(items.iter().map(WitnessPattern::from_pattern).collect())
            }
            Pattern::List { head, tail } => WitnessPattern::List {
                head: Box::new(WitnessPattern::from_pattern(head)),
                tail: Box::new(WitnessPattern::from_pattern(tail)),
            },
            Pattern::Dict { required, rest } => WitnessPattern::Dict {
                required: required
                    .iter()
                    .map(|(k, v)| (k.clone(), WitnessPattern::from_pattern(v)))
                    .collect(),
                rest: *rest,
            },
            Pattern::TypeAscription { name, pattern } => WitnessPattern::TypeAscription {
                name: name.clone(),
                pattern: Box::new(WitnessPattern::from_pattern(pattern)),
            },
        }
    }
}

/// 参数 — 镜像 Param。
#[derive(Debug, Clone, PartialEq)]
pub struct WitnessParam {
    pub name: String,
    pub type_hint: Option<crate::typeck::Type>,
    pub default: Option<MirWitness>,
}

impl WitnessParam {
    fn from_param(p: &Param) -> WitnessParam {
        WitnessParam {
            name: p.name.clone(),
            type_hint: p.type_hint.clone(),
            default: p.default.as_ref().map(MirWitness::from_expr),
        }
    }
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
        state_schema: Vec<crate::mir::expr::MirStateChannel>,
        checkpoint: Option<crate::mir::expr::MirCheckpointConfig>,
        interrupt_points: Vec<crate::mir::expr::MirInterruptPoint>,
        adjacency: std::collections::HashMap<String, Vec<String>>,
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
                exit_when: exit_when.as_ref().map(MirWitness::from_expr),
            },
            MirOrchestrateKind::Graph { agents, edges } => WitnessOrchestrateKind::Graph {
                agents: agents.iter().map(WitnessAgentDef::from_agent).collect(),
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
            task_expr: MirWitness::from_expr(&agent.task_expr),
            verify_expr: agent.verify_expr.as_ref().map(MirWitness::from_expr),
            with_config: agent.with_config.as_ref().map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), MirWitness::from_expr(v)))
                    .collect()
            }),
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
            condition_expr: edge.condition_expr.as_ref().map(MirWitness::from_expr),
            condition_body: edge.condition_body.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{BinaryOp, Literal};

    fn lit(n: i64) -> MirExpr {
        MirExpr::lit(Literal::Int(n, Span::default()), Span::default())
    }

    fn var(name: &str) -> MirExpr {
        MirExpr::var(name.to_string(), Span::default())
    }

    /// 往返一致性：from_expr 后变体一一对应、span 保留。
    #[test]
    fn from_expr_preserves_kind_and_span() {
        let expr = MirExpr {
            kind: MirExprKind::Binary {
                left: Box::new(lit(1)),
                op: BinaryOp::Add,
                right: Box::new(var("x")),
            },
            span: Span { line: 3, column: 7 },
        };
        let w = MirWitness::from_expr(&expr);
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

    /// 全变体覆盖：构造一个覆盖主要类别的程序，from_expr 不 panic 且结构保留。
    #[test]
    fn from_expr_covers_all_variant_families() {
        let exprs = vec![
            MirExpr::lit(
                Literal::String("s".into(), Span::default()),
                Span::default(),
            ),
            var("a"),
            MirExpr::binop(BinaryOp::Add, lit(1), lit(2), Span::default()),
            MirExpr::call(MirCallee::Name("f".into()), vec![lit(1)], Span::default()),
            MirExpr::if_else(var("c"), lit(1), Some(lit(2)), Span::default()),
            MirExpr::list(vec![lit(1), lit(2)], Span::default()),
            MirExpr::dict(vec![("k".into(), lit(1))], Span::default()),
            MirExpr {
                kind: MirExprKind::Sequence(vec![lit(1), lit(2)]),
                span: Span::default(),
            },
        ];
        let witnesses = MirWitness::from_exprs(&exprs);
        assert_eq!(witnesses.len(), 8);
        // 逐类确认
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
        let expr = MirExpr::closure(vec![], lit(1), Span::default());
        let w = MirWitness::from_expr(&expr);
        assert!(matches!(w.kind, WitnessKind::Closure { .. }));
    }
}
