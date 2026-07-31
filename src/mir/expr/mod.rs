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
use std::sync::Arc;

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
                captured_env: Arc::new(EnvSnapshot {
                    captured_names: vec![],
                    captured_values: vec![],
                }),
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

    Pipe {
        lhs: Box<MirExpr>,
        rhs: Box<MirExpr>, // Usually a call expression
    },

    // Functions/Closures
    Closure {
        params: Vec<Param>,
        body: Box<MirExpr>,
        /// Captured environment snapshot (for closures crossing scopes)
        captured_env: Arc<EnvSnapshot>,
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

    Grouping(Box<MirExpr>),

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

    /// Execute expression as statement (discard result)
    Expr(Box<MirExpr>),

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

///  Environment snapshot for closures
#[derive(Debug, Clone, PartialEq)]
pub struct EnvSnapshot {
    pub captured_names: Vec<String>,
    pub captured_values: Vec<Value>,
}

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
    /// Task expression in MIR form (consumed during lowering, set to None)
    pub task_mir_expr: Option<MirExpr>,

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

///  Builtin operation (placeholder for typeck)
#[derive(Debug, Clone, PartialEq)]
pub enum BuiltinOp {
    Print,
    Assert,
    Not,
    Length,
    // Add more as needed
}
