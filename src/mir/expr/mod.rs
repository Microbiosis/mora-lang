//! v0.54: Unified MIR Expressions (Phase γ - AST to MIR Migration)
//!
//! ****:  AST v2 → MIR
//!
//! ## Design Goals
//!
//! 1. **MIR as Single IR**: All language features expressible in MIR without AST v2
//! 2. **Backward Compatible**: Existing code continues working via adapters
//! 3. **Type Safety**: Expressions carry type annotations inline
//! 4. **LSP Integration**: Direct support for hover/completion on MIR types
//!
//! ## Structure Overview
//!
//! | Module | Purpose |
//! |--------|---------|
//! | `MirExpr` | Expression tree embedded in linear MIR |
//! | `MirCallee` | Function/method reference resolution |
//! | `Pattern` | Matching patterns (shared with AST v2) |
//!
//! ## Usage Pattern
//!
//! ```rust,ignore
//! // Parser v3 directly produces `Vec<MirExpr>`. Below is an illustrative
//! // example showing how MirExpr values fit together; see the parser_v3
//! // and tests under `tests/parser_v3_minimal.rs` for real usage.
//! use mora_lang::common::{BinaryOp, Literal, Span};
//! use mora_lang::mir::MirExpr;
//!
//! let span = Span::default();
//! let program = vec![
//!     MirExpr::lit(Literal::Int(42, span.clone()), span.clone()),
//!     MirExpr::binop(
//!         BinaryOp::Add,
//!         MirExpr::var("x".to_string(), span.clone()),
//!         MirExpr::lit(Literal::Int(10, span.clone()), span.clone()),
//!         span,
//!     ),
//! ];
//! ```
//!
//! (Historical `MirExpr::Let { ... }` / `MirExpr::Lit(...)` constructors
//! shown in earlier docs are no longer accurate post-Phase γ.4.)
//!
//! ## Migration Status
//!
//! ✅ v0.54: Initial expression structure
//! ⏳ v0.55: Parser migration
//! ⏳ v0.56: Complete AST v2 removal

use crate::common::{BinaryOp, Literal, Span};
use crate::mir::MirFunction;
use crate::typeck::Type;
use crate::value::{MergeStrategy, Value};
use std::collections::HashMap;

// ===================================================================
// Core Expression Types
// ===================================================================

///  Unified expression that can appear anywhere in MIR
#[derive(Debug, Clone, PartialEq)]
pub struct MirExpr {
    /// The expression kind (syntax tree node)
    pub kind: MirExprKind,

    /// Source location for error messages and LSP
    pub span: Span,
}

impl MirExpr {
    /// Create a new literal expression
    pub fn lit(lit: Literal, span: Span) -> Self {
        Self {
            kind: MirExprKind::Literal(lit),
            span,
        }
    }

    /// v0.80: 从 MirWitness 反向构造 MirExpr（用于 handle block 的 body/handler
    /// 独立 lowering：parser 在 emit_handle_w 中调用 lower_block_witness_to_mir）。
    pub fn from_witness(w: crate::mir::witness::MirWitness) -> Self {
        // 第一版简化：把 witness 包成 Sequence(MirExpr::from_kind(w.kind))。
        // MirExprKind 与 WitnessKind 是镜像（v0.55 后定义对齐）。
        let expr_kind = witness_kind_to_expr_kind(w.kind);
        Self {
            kind: MirExprKind::Sequence(vec![Self {
                kind: expr_kind,
                span: w.span,
            }]),
            span: w.span,
        }
    }

    /// Create a variable reference
    pub fn var(name: impl Into<String>, span: Span) -> Self {
        Self {
            kind: MirExprKind::Variable(name.into()),
            span,
        }
    }

    /// Create a binary operation
    pub fn binop(op: BinaryOp, left: Self, right: Self, span: Span) -> Self {
        Self {
            kind: MirExprKind::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
            },
            span,
        }
    }

    /// Create a function call
    pub fn call(callee: MirCallee, args: Vec<Self>, span: Span) -> Self {
        Self {
            kind: MirExprKind::Call { callee, args },
            span,
        }
    }

    /// Create a closure
    pub fn closure(params: Vec<Param>, body: Self, span: Span) -> Self {
        Self {
            kind: MirExprKind::Closure {
                params,
                body: Box::new(body),
            },
            span,
        }
    }

    /// Create a list literal
    pub fn list(items: Vec<Self>, span: Span) -> Self {
        Self {
            kind: MirExprKind::List(items),
            span,
        }
    }

    /// Create a dictionary literal
    pub fn dict(entries: Vec<(String, Self)>, span: Span) -> Self {
        Self {
            kind: MirExprKind::Dict(entries),
            span,
        }
    }

    /// Create an if/else expression
    pub fn if_else(cond: Self, then: Self, r#else: Option<Self>, span: Span) -> Self {
        Self {
            kind: MirExprKind::If {
                cond: Box::new(cond),
                then: Box::new(then),
                r#else: r#else.map(Box::new),
            },
            span,
        }
    }
}

/// v0.80: WitnessKind → MirExprKind 转换（独立 helper — 从已存在的
/// `MirExprKind::from_kind` 反方向走）。用于 handle block 的 body/handler
/// 独立 lowering 路径（parser 把 handle block 的 witness 子树包成 MirExpr）。
///
/// 第一版（Stage 2.0）：每个 WitnessKind 走最直接的等价 MirExprKind。
/// 语句型（LetBinding / Return 等）包成 Sequence 内的单条 expression。
fn witness_kind_to_expr_kind(wk: crate::mir::witness::WitnessKind) -> MirExprKind {
    use crate::mir::witness::WitnessKind;
    match wk {
        // 语句型：序列化为「Sequence([expr_or_stmt])」 —— Stage 2.0 单 statement
        // 即可（第一版 let x = expr 转为 Expression{ Variable(x) }）。
        WitnessKind::LetBinding { name, value, init_body, type_hint } => MirExprKind::LetBinding {
            name,
            type_hint: type_hint.map(|th| th.0),
            value: Box::new(MirExpr::from_witness(*value)),
            init_body: Box::new(MirExpr::from_witness(*init_body)),
        },
        // 表达式型：直接转（见 MirExprKind 各 variant 的构造）
        WitnessKind::Literal(lit) => MirExprKind::Literal(lit),
        WitnessKind::Variable(name) => MirExprKind::Variable(name),
        WitnessKind::Binary { left, op, right } => MirExprKind::Binary {
            left: Box::new(MirExpr::from_witness(*left)),
            op,
            right: Box::new(MirExpr::from_witness(*right)),
        },
        WitnessKind::And { left, right } => MirExprKind::And {
            left: Box::new(MirExpr::from_witness(*left)),
            right: Box::new(MirExpr::from_witness(*right)),
        },
        WitnessKind::Or { left, right } => MirExprKind::Or {
            left: Box::new(MirExpr::from_witness(*left)),
            right: Box::new(MirExpr::from_witness(*right)),
        },
        WitnessKind::Call { callee, args } => {
            // WitnessCallee → MirCallee 转换
            let mir_callee = match callee {
                crate::mir::witness::WitnessCallee::Name(n) => crate::mir::expr::MirCallee::Name(n),
                crate::mir::witness::WitnessCallee::Var(n) => crate::mir::expr::MirCallee::Var(n),
                other => crate::mir::expr::MirCallee::Var(format!("{:?}", other)),
            };
            MirExprKind::Call {
                callee: mir_callee,
                args: args.into_iter().map(MirExpr::from_witness).collect(),
            }
        }
        WitnessKind::MethodCall { receiver, method, args } => MirExprKind::MethodCall {
            receiver: Box::new(MirExpr::from_witness(*receiver)),
            method,
            args: args.into_iter().map(MirExpr::from_witness).collect(),
        },
        WitnessKind::Closure { params, body } => {
            // WitnessParam → expr::Param 转换。
            // 注：Stage 2.0 第一版忽略 default（MirExpr::Param 有 Box<MirExpr>，
            // 而 WitnessParam.default 是 Option<MirWitness>；类型不匹配需递归 from_witness）。
            let mir_params = params
                .into_iter()
                .map(|wp| crate::mir::expr::Param {
                    name: wp.name,
                    type_hint: wp.type_hint.map(|th| th.0),
                    default: None,
                })
                .collect();
            MirExprKind::Closure {
                params: mir_params,
                body: Box::new(MirExpr::from_witness(*body)),
            }
        }
        WitnessKind::FnDef { name, params, return_type, body } => {
            let mir_params = params
                .into_iter()
                .map(|wp| crate::mir::expr::Param {
                    name: wp.name,
                    type_hint: wp.type_hint.map(|th| th.0),
                    default: None,
                })
                .collect();
            MirExprKind::FnDef {
                name,
                params: mir_params,
                return_type: return_type.map(|th| th.0),
                body: Box::new(MirExpr::from_witness(*body)),
            }
        }
        WitnessKind::Match { scrutinee, arms } => {
            // WitnessArm → MatchArm 转换（pattern 已是 WitnessPattern；
            // 第一版降级为 Wildcard —— 完整 from_pattern 反向是 Stage 2.x 升级内容）。
            let mir_arms = arms
                .into_iter()
                .map(|wa| crate::mir::expr::MatchArm {
                    pattern: crate::mir::expr::Pattern::Wildcard,
                    guard: wa.guard.map(MirExpr::from_witness),
                    body: MirExpr::from_witness(wa.body),
                })
                .collect();
            MirExprKind::Match {
                scrutinee: Box::new(MirExpr::from_witness(*scrutinee)),
                arms: mir_arms,
            }
        }
        WitnessKind::If { cond, then, r#else } => MirExprKind::If {
            cond: Box::new(MirExpr::from_witness(*cond)),
            then: Box::new(MirExpr::from_witness(*then)),
            r#else: r#else.map(|b| Box::new(MirExpr::from_witness(*b))),
        },
        WitnessKind::List(items) => MirExprKind::List(
            items.into_iter().map(MirExpr::from_witness).collect(),
        ),
        WitnessKind::Dict(entries) => MirExprKind::Dict(
            entries.into_iter().map(|(k, v)| (k, MirExpr::from_witness(v))).collect(),
        ),
        WitnessKind::Prompt { parts } => MirExprKind::Prompt {
            parts: parts.into_iter().map(MirExpr::from_witness).collect(),
        },
        WitnessKind::Loop { var, iterable, body } => MirExprKind::Loop {
            var,
            iterable: Box::new(MirExpr::from_witness(*iterable)),
            body: Box::new(MirExpr::from_witness(*body)),
        },
        WitnessKind::While { cond, body } => MirExprKind::While {
            cond: Box::new(MirExpr::from_witness(*cond)),
            body: Box::new(MirExpr::from_witness(*body)),
        },
        WitnessKind::Return(v) => MirExprKind::Return(
            v.map(|b| Box::new(MirExpr::from_witness(*b))),
        ),
        WitnessKind::Assign { target, value } => MirExprKind::Assign {
            target,
            value: Box::new(MirExpr::from_witness(*value)),
        },
        WitnessKind::IndexAssign { object, index, value } => MirExprKind::IndexAssign {
            object: Box::new(MirExpr::from_witness(*object)),
            index: Box::new(MirExpr::from_witness(*index)),
            value: Box::new(MirExpr::from_witness(*value)),
        },
        // v0.80: algebraic effects —— Perform/Handle 在 v0.80 单遍编译下不再走 lower 路径
        // （parser emit_handle_w 直接 emit MirInst::Handle），但 WitnessKind 仍携带。
        WitnessKind::Perform { effect, args } => MirExprKind::Perform {
            effect,
            args: args.into_iter().map(MirExpr::from_witness).collect(),
        },
        WitnessKind::Handle { effect, body, handler, k_param } => MirExprKind::Handle {
            effect,
            body: Box::new(MirExpr::from_witness(*body)),
            handler: Box::new(MirExpr::from_witness(*handler)),
            k_param,
        },
        // 其他简单 wrapper 类型
        WitnessKind::Sequence(stmts) => MirExprKind::Sequence(
            stmts.into_iter().map(MirExpr::from_witness).collect(),
        ),
        // fallthrough：未知 variant — 退化为空 sequence（不破坏编译）
        _ => MirExprKind::Sequence(vec![]),
    }
}

///  Expression kinds (AST-like syntax tree within MIR)
#[derive(Debug, Clone, PartialEq)]
pub enum MirExprKind {
    // Simple Literals (primitive values)
    Literal(Literal),

    // Variables (scoped references)
    Variable(String),

    // Operations (computed values)
    Binary {
        left: Box<MirExpr>,
        op: BinaryOp,
        right: Box<MirExpr>,
    },

    // Function/Application
    Call {
        callee: MirCallee,
        args: Vec<MirExpr>,
    },

    MethodCall {
        receiver: Box<MirExpr>,
        method: String,
        args: Vec<MirExpr>,
    },

    // Functions/Closures
    /// v0.75.38: captured_env 已删除 — 全仓库零消费死字段（仅内部构造，
    /// typeck/lower/parser 均不读取；闭包捕获在运行时由 handler 实现）。
    Closure {
        params: Vec<Param>,
        body: Box<MirExpr>,
    },

    /// Nested function definition (not closure - has its own scope)
    FnDef {
        name: String,
        params: Vec<Param>,
        return_type: Option<Type>,
        body: Box<MirExpr>,
    },

    // Control Flow
    Match {
        scrutinee: Box<MirExpr>,
        arms: Vec<MatchArm>,
    },

    If {
        cond: Box<MirExpr>,
        then: Box<MirExpr>,
        r#else: Option<Box<MirExpr>>,
    },

    /// v0.55: for loop
    Loop {
        var: String,
        iterable: Box<MirExpr>,
        body: Box<MirExpr>,
    },

    /// v0.55: while loop
    While {
        cond: Box<MirExpr>,
        body: Box<MirExpr>,
    },

    /// v0.55: logical or (short-circuit)
    Or {
        left: Box<MirExpr>,
        right: Box<MirExpr>,
    },

    /// v0.55: logical and (short-circuit)
    And {
        left: Box<MirExpr>,
        right: Box<MirExpr>,
    },

    // Collections
    List(Vec<MirExpr>),
    Dict(Vec<(String, MirExpr)>),

    // Advanced Features
    DynTrait {
        expr: Box<MirExpr>,
        trait_name: String,
        generics: Vec<Type>,
    },

    Prompt {
        parts: Vec<MirExpr>,
    },

    // Binding & Mutation
    LetBinding {
        name: String,
        type_hint: Option<Type>,
        value: Box<MirExpr>,
        init_body: Box<MirExpr>,
    },

    Assign {
        target: String,
        value: Box<MirExpr>,
    },

    /// Variable assignment with index (list/dict mutation)
    IndexAssign {
        object: Box<MirExpr>,
        index: Box<MirExpr>,
        value: Box<MirExpr>,
    },

    /// Return from function
    Return(Option<Box<MirExpr>>),

    /// Break/continue for loops
    Break(String),
    Continue(String),

    Orchestrate {
        input_var: String,
        result_var: String,
        kind: Box<MirOrchestrateKind>,
    },

    /// Type alias: `type Bytes = number`
    TypeAlias {
        name: String,
        target: Type,
    },

    /// Enum definition: `enum Color Red Green Blue end`
    EnumDef {
        name: String,
        variants: Vec<String>,
    },

    /// Struct definition: `struct Point x: number y: number end`
    StructDef {
        name: String,
        fields: Vec<(String, Type)>,
    },

    /// Import statement: `import "std/io"`
    Import(String),

    /// Macro definition: `macro greet(name) ... end`
    MacroDef {
        name: String,
        params: Vec<String>,
    },

    /// v0.80: algebraic effects expression forms（Stage 2/4 落地）。
    ///
    /// Perform: `perform Effect(args)` — 触发一个具名 effect。
    /// Parse-time 校验：args 必须表达式（by MirExpr 二级树）。
    /// Lowering 后 emit `MirInst::Perform(dst, effect, args)`。
    Perform {
        effect: String,
        args: Vec<MirExpr>,
    },

    /// Handle: `handle Effect { body } { handler }` — 安装 effect handler。
    /// body 与 handler 都是 MirExpr 块 lowering 出的 MirFunction。
    /// Stage 2.x 升级：handler 可使用 `resume "k" resume-value` 续名续。
    Handle {
        effect: String,
        body: Box<MirExpr>,
        handler: Box<MirExpr>,
        k_param: String,
    },

    /// Sequence of expressions (blocks with multiple statements)
    Sequence(Vec<MirExpr>),
}

// ===================================================================
// Integration with existing MIR instructions
// ===================================================================

///  Combined representation: can be either expression or statement
#[derive(Debug, Clone)]
pub enum MirInstOrExpr {
    /// Value-producing expression
    Expr(MirExpr),

    /// Side-effect statement
    Stmt(MirStmt),
}

impl From<MirExpr> for MirInstOrExpr {
    fn from(expr: MirExpr) -> Self {
        MirInstOrExpr::Expr(expr)
    }
}

impl From<MirStmt> for MirInstOrExpr {
    fn from(stmt: MirStmt) -> Self {
        MirInstOrExpr::Stmt(stmt)
    }
}

// ===================================================================
// Migration Module: AST v2 → MirExpr Equivalents (Phase γ.4)
// ===================================================================
//
// This module provides MirExpr-native replacements for AST v2 types.

///  Function/method call target
#[derive(Debug, Clone, PartialEq)]
pub enum MirCallee {
    /// Named function: `foo`
    Name(String),
    /// Variable holding a function: `f`
    Var(String),
    /// Method call: `obj.method`
    Method(String, String),
    /// Evaluated expression that produces a callable
    Evaluated(Box<MirExpr>),
    /// Builtin operation
    Builtin(BuiltinOp),
}

///  Match arm for pattern matching
#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub guard: Option<MirExpr>,
    pub body: MirExpr,
}

///  Parameter definition
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub type_hint: Option<Type>,
    pub default: Option<MirExpr>,
}

///  Pattern matching variants
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    /// Wildcard pattern: `_`
    Wildcard,
    /// Variable binding: `x`
    Variable(String),
    /// Literal pattern: `42`, `"hello"`, `true`
    Literal(Literal),
    /// Tuple pattern: `(a, b, c)`
    Tuple(Vec<Pattern>),
    /// List pattern: `[head | tail]`
    List {
        head: Box<Pattern>,
        tail: Box<Pattern>,
    },
    /// Dict pattern: `{key: value, ..}`
    Dict {
        required: Vec<(String, Pattern)>,
        rest: bool,
    },
    /// Type ascription: `x: Type`
    TypeAscription { name: String, pattern: Box<Pattern> },
}

// ===================================================================
// Statement Types (MIR-level)
// ===================================================================

///  MIR-level statements
#[derive(Debug, Clone, PartialEq)]
pub enum MirStmt {
    /// Variable definition: `let x = expr`
    Let {
        name: String,
        value: MirExpr,
    },

    /// Variable assignment: `x = expr`
    Assign {
        name: String,
        value: MirExpr,
    },

    /// Expression statement (discard result)
    Expr(MirExpr),

    /// Return from function
    Return(Option<MirExpr>),

    /// Break/continue for loops
    Break(String),
    Continue(String),
}

// ===================================================================
// Orchestrate Types
// ===================================================================

///  Orchestrate kind (sequential/loop/graph)
#[derive(Debug, Clone, PartialEq)]
pub enum MirOrchestrateKind {
    Sequential {
        agents: Vec<MirAgentDef>,
    },
    Loop {
        agents: Vec<MirAgentDef>,
        rounds: Option<u64>,
        exit_when: Option<MirExpr>,
    },
    Graph {
        agents: Vec<MirAgentDef>,
        edges: Vec<MirEdgeDef>,
    },
    /// v0.50: Pregel BSP-style orchestrate
    Pregel {
        agents: Vec<MirAgentDef>,
        edges: Vec<MirEdgeDef>,
        state_schema: Vec<MirStateChannel>,
        checkpoint: Option<MirCheckpointConfig>,
        interrupt_points: Vec<MirInterruptPoint>,
        adjacency: HashMap<String, Vec<String>>,
    },
    /// v0.75.84: MoA（Mixture-of-Agents，arXiv:2406.04692）— 分层多模型协作。
    /// 每层 N 个 proposer LLM 并行生成 → 聚合器 LLM 综合 → 传下一层。
    /// `h_orchestrate` 展开为 pregel 图（每层 proposer 并行 + 聚合 agent，
    /// 静态边层间传递），零新引擎机制。
    Moa {
        /// MoA 层数（论文：l 层，通常 2-3 层；末层单聚合器）。
        layers: usize,
        /// 每层 proposer 模型列表（同层复用；论文 n 个异构模型并行）。
        proposers: Vec<String>,
        /// 聚合器模型（每层聚合 + 末层最终输出）。
        aggregator: String,
        /// 初始 prompt（MoA 每层基于前层输出的「原文」继续；聚合 prompt
        /// 由引擎按 Aggregate-and-Synthesize 模板生成）。
        prompt: MirExpr,
    },
    /// v0.75.85: MoE（Mixture-of-Experts，Shazeer 2017 稀疏门控）— 稀疏激活。
    /// router 语言面 fn 打分 → top-k 稀疏（只跑被选专家）→ 加权组合
    /// （引擎侧 Float 自由，不受语言数值塔约束）。与 MoA 的区别：MoA 全
    /// 部专家跑 + LLM 聚合综合（协作）；MoE 只跑部分专家 + 数值加权（稀疏）。
    Moe {
        /// 专家定义：名 → 函数闭包（fn(x) → number）或模型配置
        /// （{model: "..."}，输出 String）。见 MirMoeExpert。
        experts: Vec<MirMoeExpert>,
        /// 路由器（门控）：语言面 fn(x) → Dict(专家名 → 分数)。
        router: MirExpr,
        /// 稀疏度：只激活分数最高 top_k 个专家（标准配置 2，k=1 可行）。
        top_k: usize,
        /// 模型专家的 prompt（含 {input} 插值）。
        prompt: MirExpr,
    },
}

/// v0.75.85: MoE 专家定义 — 名 + 定义表达式。
/// def 执行后为 Value::Closure（函数专家，数值输出）或 Value::Dict
/// （{model: "..."}，模型专家，String 输出）。
#[derive(Debug, Clone, PartialEq)]
pub struct MirMoeExpert {
    pub name: String,
    pub def: MirExpr,
}

///  Agent definition in orchestrate
#[derive(Debug, Clone, PartialEq)]
pub struct MirAgentDef {
    pub name: String,
    pub task_expr: MirExpr,
    pub verify_expr: Option<MirExpr>,
    pub with_config: Option<HashMap<String, MirExpr>>,

    /// Pre-lowered task body (populated during lowering, starts empty)
    pub task_body: MirFunction,
    /// v0.72: Pre-lowered combiner body. When multiple sends target this
    /// vertex, the engine folds them with `(current, incoming) -> Value`
    /// before delivering. Identity (default): last-write-wins (current = incoming).
    pub combiner_body: Option<MirFunction>,
}

///  Edge definition in orchestrate graph
#[derive(Debug, Clone, PartialEq)]
pub struct MirEdgeDef {
    pub from: String,
    pub to: String,
    pub condition_expr: Option<MirExpr>,
    pub condition_body: Option<MirFunction>,
}

// ===================================================================
// Compatibility aliases (v0.55 migration helpers)
// ===================================================================

///  Alias for MirAgentDef (used by parser_v3 and orchestrate code)
pub type MirOrchestrateAgent = MirAgentDef;

///  Alias for MirEdgeDef (used by parser_v3 and orchestrate code)
pub type MirOrchestrateEdge = MirEdgeDef;

// ===================================================================
// Placeholder types for future MIR features
// ===================================================================

///  Trait method definition
#[derive(Debug, Clone, PartialEq)]
pub struct MirTraitMethod {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Option<String>,
    pub body: Option<MirFunction>,
}

///  Function definition in impl block
#[derive(Debug, Clone, PartialEq)]
pub struct MirFnDef {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Option<String>,
    pub body: Option<MirFunction>,
}

///  Skill task definition
#[derive(Debug, Clone, PartialEq)]
pub struct MirSkillTask {
    pub name: String,
    pub description: Option<String>,
    pub params: Vec<Param>,
    pub body: Option<MirFunction>,
}

///  Skill verification definition
#[derive(Debug, Clone, PartialEq)]
pub struct MirSkillVerify {
    pub name: String,
    pub given: Vec<String>,
    pub expects: Vec<String>,
    pub params: Vec<Param>,
    pub body: Option<MirFunction>,
}

///  Checkpoint configuration (placeholder for v0.50)
#[derive(Debug, Clone, PartialEq)]
pub struct MirCheckpointConfig {
    pub saver: String,
    pub thread_id: Option<Box<MirExpr>>,
    pub interval: Option<u64>,
    pub max_checkpoints: Option<usize>,
}

///  Interrupt point definition (placeholder for v0.50)
#[derive(Debug, Clone, PartialEq)]
pub struct MirInterruptPoint {
    pub node_name: String,
    pub when: MirInterruptWhen,
}

///  Interrupt when condition (placeholder for v0.50)
#[derive(Debug, Clone, PartialEq)]
pub enum MirInterruptWhen {
    Before,
    After,
    Timeout(u64),
    Condition(MirExpr),
    Manual,
}

///  Reducer kind for dynamic edges (placeholder for v0.50)
#[derive(Debug, Clone, PartialEq)]
pub enum MirReducerKind {
    Last,
    Append,
    Add,
    /// v0.75.5: G-Set（grow-only set）— 通道上并集累积（List/Dict 语义），
    /// 对应 `MergeStrategy::GrowOnlySet`。
    GrowOnly,
    Merge(MirExpr),
    Sum,
    Product,
    Concat,
    Custom(String),
}

/// v0.60: Map Pregel reducer to CRDT merge strategy.
///
/// `Merge`, `Sum`, `Product`, `Concat`, and `Custom` have no direct
/// static mapping and return `None` — these require custom execution.
///
/// NOTE: `Append` maps to `MergeStrategy::Append` for Environment-level
/// merges (two-dict merge), but the Pregel engine handles `Append`
/// separately in `apply_write()` with stream-accumulation semantics
/// (push individual writes into a list). The two paths are intentionally
/// different.
impl MirReducerKind {
    pub fn to_merge_strategy(&self) -> Option<MergeStrategy> {
        match self {
            MirReducerKind::Last => Some(MergeStrategy::LastWriteWins),
            MirReducerKind::Append => Some(MergeStrategy::Append),
            MirReducerKind::Add => Some(MergeStrategy::Add),
            MirReducerKind::GrowOnly => Some(MergeStrategy::GrowOnlySet),
            MirReducerKind::Merge(_)
            | MirReducerKind::Sum
            | MirReducerKind::Product
            | MirReducerKind::Concat
            | MirReducerKind::Custom(_) => None,
        }
    }
}

///  State channel definition (placeholder for v0.50)
#[derive(Debug, Clone, PartialEq)]
pub struct MirStateChannel {
    pub name: String,
    pub ty: String,
    pub reducer: MirReducerKind,
}

///  Pregel configuration bundle (v0.57: MIR-native engine entry)
#[derive(Debug, Clone, PartialEq)]
pub struct MirPregelConfig {
    pub agents: Vec<MirAgentDef>,
    pub edges: Vec<MirEdgeDef>,
    pub state_schema: Vec<MirStateChannel>,
    pub checkpoint: Option<MirCheckpointConfig>,
    pub interrupt_points: Vec<MirInterruptPoint>,
    pub adjacency: HashMap<String, Vec<String>>,
    /// v0.71: Per-super-step global aggregators. Each agent can contribute
    /// a value via `h_aggregate(name, value)`; the engine reduces across
    /// all contributions per step and exposes the result as `aggregator_<name>`.
    pub aggregators: Vec<MirAggregatorDef>,
    /// v0.72: Centralized coordinator hook. Runs once per super-step after
    /// UPDATE and before ADVANCE. Used for global coordination logic
    /// (e.g., dynamic topology decisions based on aggregator state).
    pub master_compute: Option<MirFunction>,
}

/// v0.71: Aggregator definition (per-super-step global reducer).
#[derive(Debug, Clone, PartialEq)]
pub struct MirAggregatorDef {
    pub name: String,
    pub ty: String,
    pub initial: Value,
    /// Per-step reducer: Add, Max, Min, Last, Concat.
    pub reducer: AggregatorKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AggregatorKind {
    Add,
    Max,
    Min,
    Last,
    Concat,
}

/// v0.75.83: 聚合器贡献 — agent 经 `aggregate name, value` 语句提交，
/// h_aggregate push 到 MirHost 缓冲，Pregel 引擎超步末收集并经
/// aggregator_contribute 归约（与 SendTask/dynamic_sends 同构）。
#[derive(Debug, Clone, PartialEq)]
pub struct AggregatorContribution {
    pub name: String,
    pub value: Value,
}

///  Builtin operation (placeholder for typeck)
#[derive(Debug, Clone, PartialEq)]
pub enum BuiltinOp {
    Print,
    Assert,
    Not,
    Length,
    // Add more as needed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::MergeStrategy;

    #[test]
    fn to_merge_strategy_maps_reducers() {
        assert_eq!(
            MirReducerKind::Last.to_merge_strategy(),
            Some(MergeStrategy::LastWriteWins)
        );
        assert_eq!(
            MirReducerKind::Append.to_merge_strategy(),
            Some(MergeStrategy::Append)
        );
        assert_eq!(
            MirReducerKind::Add.to_merge_strategy(),
            Some(MergeStrategy::Add)
        );
        // v0.75.5: G-Set reducer 映射到 grow-only set 策略
        assert_eq!(
            MirReducerKind::GrowOnly.to_merge_strategy(),
            Some(MergeStrategy::GrowOnlySet)
        );
        // 自定义 reducer 无静态映射
        assert_eq!(
            MirReducerKind::Merge(MirExpr::var("x", Span::default())).to_merge_strategy(),
            None
        );
        assert_eq!(MirReducerKind::Sum.to_merge_strategy(), None);
        assert_eq!(MirReducerKind::Product.to_merge_strategy(), None);
        assert_eq!(MirReducerKind::Concat.to_merge_strategy(), None);
        assert_eq!(
            MirReducerKind::Custom("fn".into()).to_merge_strategy(),
            None
        );
    }
}
