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

    /// v0.88: Quasiquote — Lisp-style `` `expr `` with unquote/comma.
    /// MirExpr-level representation for the legacy parse→lower path.
    /// Each MirExpr in segments represents one quasiquote segment:
    ///   - Literal(String(s)) → Quote(s) — static source text
    ///   - Variable(name) → Unquote — resolve name to Reg during lowering
    ///   - Call{Name("splice"), [expr]} → UnquoteSplice — resolve expr to Reg
    ///
    /// The emit path (compile) handles quasiquote directly as MirInst.
    /// This variant mirrors that for path equivalence (proptest).
    QuasiquoteExpr(Vec<MirExpr>),

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
    /// v0.87: List vector pattern: `[a, b, ..rest]` or `[a, b]`.
    /// `elements` are the prefix patterns; `rest` is an optional rest-variable pattern.
    ListVec {
        elements: Vec<Pattern>,
        rest: Option<Box<Pattern>>,
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
// v0.91: orchestrate types 已迁至 `src/mir/orchestrate/mod.rs`。
// 上方 re-export 提供向后兼容；Stage 4 完成后删除本段注释。
// ===================================================================

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

///  Builtin operation (placeholder for typeck)
#[derive(Debug, Clone, PartialEq)]
pub enum BuiltinOp {
    Print,
    Assert,
    Not,
    Length,
    // Add more as needed
}

// ===================================================================
// v0.91: Transition re-exports — orchestrate types 已迁至 mir/orchestrate
// ===================================================================
// 以下类型在 v0.91 从 expr/mod.rs 迁出至独立模块 `mir/orchestrate`。
// 当前保留本 re-export 作为向后兼容过渡；Stage 4 完成后移除，
// 所有调用方直接使用 `crate::mir::orchestrate::*`。

pub use crate::mir::orchestrate::{
    AggregatorContribution, AggregatorKind, MirAggregatorDef, MirAgentDef, MirCheckpointConfig,
    MirEdgeDef, MirInterruptPoint, MirInterruptWhen, MirMoeExpert, MirOrchestrateKind,
    MirPregelConfig, MirReducerKind, MirStateChannel,
};

///  Alias for MirAgentDef (used by parser_v3 and orchestrate code)
pub type MirOrchestrateAgent = MirAgentDef;

///  Alias for MirEdgeDef (used by parser_v3 and orchestrate code)
pub type MirOrchestrateEdge = MirEdgeDef;

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
