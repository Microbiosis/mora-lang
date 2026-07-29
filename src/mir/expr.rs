//! α.0: MirExpr — MIR 表达式树
//! 
//! ## 设计哲学
//! 
//! "One representation to rule them all"
//! 
//! MirExpr 是 MIR 的**表达式形式**，可以直接从 Parser 输出，无需经过 AST v2 中间层。
//! 它保留了表达式的层次结构（相对于 MirInst 的线性指令），但已经包含了类型信息。
//! 
//! ## 核心特性
//! - 类型内嵌：TypedMirExpr 包含 Type 注解，HM Inference 可直接工作
//! - 无缝降级：可转换为 MirInst 序列（通过 lowering pass）
//! - LSP ready: Span + Type → hover/reference/rename 精确支持
//! 
//! ## 与 AST v2 对比
//! 
//! | 特性 | AST v2 (TypedExpr) | MirExpr (TypedMirExpr) |
//! |------|-------------------|------------------------|
//! | 内存 | Arena + NodeId | Boxed tree (no arena) |
//! | 类型 | 分离存储 | 内嵌在节点 |
//! | 执行 | → MirInst | ↓ MirInst 或直接 eval |
//! | HM 推断 | 独立 pass | 内联或 post-pass |
//! 
//! ## 使用场景
//! 1. Phase 1: Parser 输出 MirExpr (跳过 AST v2)
//! 2. Phase 2: HM Inference targets MirExpr
//! 3. Phase 3: LSP shows MirExpr types
//! 4. Future: Remove AST v2 entirely

use crate::common::BinaryOp;
use crate::typeck::Type;

/// 虚拟寄存器索引（同 MirInst::Reg）
pub type Reg = usize;

/// MirExpr — MIR 表达式树
/// 
/// 这是**单源真理**表示，Parser 直接输出 MirExpr，不需要经过 AST v2。
#[derive(Debug, Clone, PartialEq)]
pub enum MirExpr {
    // ── 字面量 ────────────────────────────────────────────────
    /// 字面量常量
    Literal(Literal),
    
    /// 变量引用
    Variable(String),
    
    // ── 运算表达式 ────────────────────────────────────────────
    /// 二元运算
    BinOp {
        op: BinaryOp,
        left: Box<MirExpr>,
        right: Box<MirExpr>,
    },
    
    /// 管道运算符 lhs |> callee
    Pipe {
        lhs: Box<MirExpr>,
        callee: Box<MirExpr>, // 可以是变量名或闭包
    },
    
    // ── 函数调用 ──────────────────────────────────────────────
    /// 函数调用 callee(args...)
    Call {
        callee: String, // 函数名（非表达式）
        args: Vec<MirExpr>,
    },
    
    /// 方法调用 receiver.method(args...)
    MethodCall {
        receiver: Box<MirExpr>,
        method: String,
        args: Vec<MirExpr>,
    },
    
    /// 索引访问 obj[index]
    Index {
        object: Box<MirExpr>,
        index: Box<MirExpr>,
    },
    
    // ── 复合数据结构 ──────────────────────────────────────────
    /// 列表字面量 [a, b, c]
    List(Vec<MirExpr>),
    
    /// 字典字面量 {key: val, ...}
    Dict(Vec<(String, MirExpr)>),
    
    // ── 闭包和函数 ────────────────────────────────────────────
    /// 匿名闭包 {params} -> body
    Closure {
        params: Vec<String>,
        body: Box<MirExpr>, // single expression body
    },
    
    /// 命名函数定义 fn foo(params) { body }
    FunctionDef {
        name: String,
        params: Vec<String>,
        return_type: Option<Type>,
        body: Box<MirFunction>, // full function body (stmts)
    },
    
    // ── 模式匹配 ──────────────────────────────────────────────
    /// Match 表达式 match expr { arms... }
    Match {
        scrutinee: Box<MirExpr>,
        arms: Vec<MatchArm>,
    },
    
    // ── AI 原生特性 ───────────────────────────────────────────
    /// Prompt 字符串模板 p"hello {name}!"
    Prompt {
        parts: Vec<PromptPart>,
    },
    
    /// AI 模型调用 ai.chat(prompt, config?)
    AiModelCall {
        route: String, // 模型路由名称
        prompt: Box<MirExpr>,
        config: Option<AIConfig>,
    },
    
    // ── 类型系统 ──────────────────────────────────────────────
    /// dyn Trait 包装
    DynTrait {
        expr: Box<MirExpr>,
        trait_name: String,
        generics: Vec<String>,
    },
    
    /// 组括号 (expr) — 用于优先级控制
    Grouping(Box<MirExpr>),
    
    // ── Borrow 操作符 &mut expr / &expr
    Borrow {
        mutable: bool,
        expr: Box<MirExpr>,
    },
    
    // ── 命令和控制流 ─────────────────────────────────────────
    /// Command goto/update/resume
    Command {
        goto: Option<String>,
        update: Option<(String, Box<MirExpr>)>,
        resume: Option<String>,
    },
    
    /// Send task to agent
    Send {
        target: String,
        input: MirExpr,
    },
    
    // ── 特殊构造 ──────────────────────────────────────────────
    /// Namespace ref (for imports)
    NamespaceRef(String),
    
    /// JSON Schema placeholder for tool parameters
    JsonSchemaPlaceholder(String), // serialized JSON string
    
    /// Eval test case
    EvalTest {
        given: MirExpr,
        expects: Vec<MirExpr>,
        tolerance: Option<f64>,
    },
    
    /// Orchestrate DAG
    Orchestrate {
        kind: MirOrchestrateKind,
        input_var: String,
        result_var: String,
    },
}

/// TypedMirExpr — 带类型注解的 MirExpr
/// 
/// HM Inference 直接工作在这个类型上，无需额外的类型检查 pass。
#[derive(Debug, Clone, PartialEq)]
pub struct TypedMirExpr {
    pub kind: MirExpr,
    pub span: crate::common::Span,
    pub ty: Type,
}

impl TypedMirExpr {
    /// Create a new typed expression with inferred type
    pub fn new(kind: MirExpr, span: crate::common::Span, ty: Type) -> Self {
        Self { kind, span, ty }
    }

    /// Create from an already typed expression (unwrap)
    pub fn from_typed(expr: Self) -> MirExpr {
        expr.kind
    }
}

/// MirExpr 的子类型：MatchArm
#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: String, // pattern 字符串表示（如 "Some(_)"）
    pub condition: Option<MirExpr>, // guard 条件
    pub body: MirFunction, // 匹配的 body
}

/// MirExpr 的子类型：PromptPart
#[derive(Debug, Clone, PartialEq)]
pub enum PromptPart {
    /// 普通文本
    Text(String),
    /// 插值表达式 {expr}
    Interpolation(MirExpr),
}

/// MirExpr 的子类型：AIConfig
#[derive(Debug, Clone, PartialEq)]
pub struct AIConfig {
    pub model: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<usize>,
    pub budget: Option<usize>,
}

/// MirExpr 的子类型：Literal
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Bool(bool),
    Number(f64),
    Int(i64),
    Char(char),
    String(String),
    Nil,
    Prompt(String), // p"..." literal
}

/// MirFunction — MIR 函数（语句序列）
/// 
/// MirExpr 是表达式树，MirFunction 是完整的函数（可包含多条语句）。
/// 这是 AST→MIR lowering 的目标产物，也是 Parser V3 的直接输出。
#[derive(Debug, Clone, PartialEq)]
pub struct MirFunction {
    pub name: Option<String>, // None 表示匿名函数/closure
    pub params: Vec<String>,
    pub return_type: Option<Type>,
    /// 混合体：既可以是纯表达式，也可以是语句序列
    /// Body 中的每个元素都是 value-producing，最后返回最后一个元素的值
    pub body: Vec<TypedMirExpr>,
    pub n_regs: usize, // 寄存器分配计数（SSA pass 用）
}

impl MirFunction {
    /// Create an empty function
    pub fn new(name: Option<String>, params: Vec<String>) -> Self {
        Self {
            name,
            params,
            return_type: None,
            body: Vec::new(),
            n_regs: 0,
        }
    }

    /// Set return type
    pub fn with_return_type(mut self, ty: Option<Type>) -> Self {
        self.return_type = ty;
        self
    }
}

/// MirOrchestrateKind — Pregel orchestration 类型
#[derive(Debug, Clone, PartialEq)]
pub enum MirOrchestrateKind {
    /// Pregel BSP engine
    Pregel {
        agents: Vec<String>,
        edges: Vec<(String, String)>, // from_agent -> to_agent
        state_schema: std::collections::HashMap<String, Type>,
        checkpoint: Option<String>,
        interrupt_points: Vec<u32>,
    },
    /// DAG execution
    Dag {
        nodes: Vec<DagNode>,
        edges: Vec<(String, String)>,
    },
}

/// DagNode — DAG 编排节点
#[derive(Debug, Clone, PartialEq)]
pub struct DagNode {
    pub id: String,
    pub name: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

/// MirStmt — MIR 语句（独立的副作用产生器）
#[derive(Debug, Clone, PartialEq)]
pub enum MirStmt {
    Define(String, TypedMirExpr),
    Assign(String, TypedMirExpr),
    Expr(TypedMirExpr),
    Return(Option<TypedMirExpr>),
    If {
        condition: TypedMirExpr,
        then_branch: Vec<MirStmt>,
        else_branch: Vec<MirStmt>,
    },
    For {
        var: String,
        iterable: TypedMirExpr,
        body: Vec<MirStmt>,
    },
    Break,
    Continue,
    TaskDef {
        name: String,
        params: Vec<String>,
        body: MirFunction,
    },
    ToolDef {
        name: String,
        description: String,
        params: Vec<String>,
        return_type: Option<Type>,
        body: MirFunction,
        exported: bool,
    },
    TraitDef {
        name: String,
        parents: Vec<String>,
        methods: Vec<MirTraitMethod>,
    },
    ImplDef {
        trait_name: String,
        for_type: String,
        methods: Vec<MirTraitMethod>,
    },
    Import(String),
    WithConfig {
        bindings: Vec<(String, TypedMirExpr)>,
        body: Vec<MirStmt>,
    },
}

/// MirTraitMethod — Trait 方法定义
#[derive(Debug, Clone, PartialEq)]
pub struct MirTraitMethod {
    pub name: String,
    pub params: Vec<(String, Option<Type>)>,
    pub return_type: Option<Type>,
    pub has_self: bool,
}

/// MirExpr Pattern — for pattern matching
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Wildcard,
    Variable(String),
    Literal(Literal),
    // TODO: Add more patterns during migration (Tuple, List, Dict, etc.)
}

// =============================================================================
// Helper Functions
// =============================================================================

/// 创建一个字面量表达式
pub fn lit_literal(lit: Literal) -> MirExpr {
    MirExpr::Literal(lit)
}

/// 创建一个变量引用
pub fn lit_string(s: String) -> MirExpr {
    MirExpr::Literal(Literal::String(s))
}

/// 创建一个整数
pub fn lit_int(n: i64) -> MirExpr {
    MirExpr::Literal(Literal::Int(n))
}

/// 创建一个浮点数
pub fn lit_float(f: f64) -> MirExpr {
    MirExpr::Literal(Literal::Number(f))
}

/// 创建一个布尔值
pub fn lit_bool(b: bool) -> MirExpr {
    MirExpr::Literal(Literal::Bool(b))
}

/// 创建一个 nil
pub fn lit_nil() -> MirExpr {
    MirExpr::Literal(Literal::Nil)
}

// Import Pregel types at module level
pub mod pregel_types;
pub use pregel_types::*;
