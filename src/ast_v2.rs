//! v0.22: 新 AST 结构（Arena 分配 + NodeId + 类型信息）
//!
//! 设计原则：
//! - **Arena 分配**：所有节点在连续内存中，减少堆分配
//! - **NodeId**：通过 ID 引用节点，支持增量编译
//! - **类型信息保留**：类型检查后保留类型信息
//! - **Visitor 模式**：解耦遍历逻辑

use crate::ast::{
    BinaryOp, GenericParam, Literal, Span,
};
use crate::typeck::Type;

/// 模式 (v2: 使用 NodeId)
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Wildcard,
    Literal(Literal),
    Variable(String),
    List {
        prefix: Vec<Pattern>,
        rest: Option<String>,
    },
    Dict(Vec<(String, Pattern)>),
    Guard {
        pattern: Box<Pattern>,
        condition: NodeId,
    },
}

/// 函数定义 (v2: 使用 NodeId)
#[derive(Debug, Clone, PartialEq)]
pub struct FnDef {
    pub name: String,
    pub params: Vec<(String, Option<String>)>,
    pub return_type: Option<String>,
    pub body: Vec<NodeId>,
    pub span: Span,
}

/// Trait 方法 (v2: 使用 NodeId)
#[derive(Debug, Clone, PartialEq)]
pub struct TraitMethod {
    pub name: String,
    pub params: Vec<(String, Option<String>)>,
    pub return_type: Option<String>,
    pub body: Vec<NodeId>,
    pub generics: Vec<GenericParam>,
    pub span: Span,
}

/// 可观测性配置 (v2: 使用 NodeId)
#[derive(Debug, Clone, PartialEq)]
pub enum ObserveConfig {
    Trace,
    Metrics,
    Otel { endpoint: NodeId },
}

// ===================================================================
// NodeId
// ===================================================================

/// 节点 ID（Arena 中的索引）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub usize);

// ===================================================================
// TypedExpr
// ===================================================================

/// 带类型信息的表达式节点
#[derive(Debug, Clone)]
pub struct TypedExpr {
    pub id: NodeId,
    pub kind: ExprKind,
    pub span: Span,
    pub ty: Option<Type>,
}

/// 表达式种类（无 Span、无 Type）
#[derive(Debug, Clone)]
pub enum ExprKind {
    // 字面量
    Literal(Literal),

    // 变量
    Variable(String),

    // 二元操作
    Binary {
        left: NodeId,
        op: BinaryOp,
        right: NodeId,
    },

    // 管道
    Pipe {
        left: NodeId,
        right: NodeId,
    },

    // 函数调用
    Call {
        callee: String,
        args: Vec<NodeId>,
    },

    // 方法调用
    MethodCall {
        object: NodeId,
        method: String,
        args: Vec<NodeId>,
    },

    // 索引
    Index {
        object: NodeId,
        index: NodeId,
    },

    // 闭包
    Closure {
        params: Vec<(String, Option<String>)>,
        return_type: Option<String>,
        body: Vec<NodeId>,
    },

    // 模式匹配
    Match {
        expr: NodeId,
        arms: Vec<(Pattern, NodeId)>,
    },

    // 模板字符串
    Prompt {
        parts: Vec<NodeId>,
    },

    // 路由调用
    RouteCall {
        name: String,
        args: Vec<NodeId>,
    },

    // AI 模型调用
    AiModelCall {
        model: NodeId,
        temperature: Option<NodeId>,
        max_tokens: Option<NodeId>,
        system: Option<NodeId>,
    },

    // 错误传播
    Question {
        expr: NodeId,
    },

    // 命名空间引用
    NamespaceRef {
        namespace: String,
        name: String,
    },

    // dyn trait 类型标注
    DynTrait {
        generics: Vec<String>,
        trait_name: String,
    },

    // 分组
    Grouping(NodeId),

    // 列表字面量
    List(Vec<NodeId>),

    // 字典字面量
    Dict(Vec<(String, NodeId)>),

    // v0.21: 不可变借用
    Borrow {
        expr: NodeId,
    },

    // v0.21: 可变借用
    BorrowMut {
        expr: NodeId,
    },
}

// ===================================================================
// TypedStmt
// ===================================================================

/// 带类型信息的语句节点
#[derive(Debug, Clone)]
pub struct TypedStmt {
    pub id: NodeId,
    pub kind: StmtKind,
    pub span: Span,
}

/// 语句种类
#[derive(Debug, Clone)]
pub enum StmtKind {
    // 变量绑定
    Let {
        name: String,
        type_hint: Option<String>,
        init: NodeId,
        exported: bool,
    },

    // 变量赋值
    Assign {
        name: String,
        value: NodeId,
    },

    // 索引赋值
    IndexAssign {
        object: NodeId,
        index: NodeId,
        value: NodeId,
    },

    // 函数定义
    TaskDef {
        name: String,
        lifetime_params: Vec<String>,
        params: Vec<(String, Option<String>)>,
        return_type: Option<String>,
        body: Vec<NodeId>,
        exported: bool,
    },

    // 条件
    If {
        condition: NodeId,
        then_branch: Vec<NodeId>,
        else_branch: Vec<NodeId>,
    },

    // 循环
    For {
        var: String,
        var_type: Option<String>,
        iterable: NodeId,
        body: Vec<NodeId>,
    },

    // 返回
    Return {
        value: Option<NodeId>,
    },

    // 导入
    Import {
        path: String,
    },

    // 并行
    Parallel {
        stmts: Vec<NodeId>,
    },

    // 模式匹配
    Match {
        expr: NodeId,
        arms: Vec<(Pattern, Vec<NodeId>)>,
    },

    // 文件操作
    Save {
        path: NodeId,
        value: NodeId,
    },
    Load {
        path: NodeId,
        var: String,
    },
    ReadFile {
        path: NodeId,
        var: String,
    },
    WriteFile {
        path: NodeId,
        content: NodeId,
    },
    AppendFile {
        path: NodeId,
        content: NodeId,
    },
    ReadBytesFile {
        path: NodeId,
        var: String,
    },
    WriteBytesFile {
        path: NodeId,
        content: NodeId,
    },

    // 表达式语句
    Expr(NodeId),

    // With 块
    With {
        bindings: Vec<(String, NodeId)>,
        body: Vec<NodeId>,
    },

    // 流式循环
    StreamFor {
        prompt: NodeId,
        var: String,
        body: Vec<NodeId>,
    },

    // 工具定义
    ToolDef {
        name: String,
        params: Vec<(String, Option<String>)>,
        return_type: Option<String>,
        body: Vec<NodeId>,
        exported: bool,
    },

    // Break/Continue
    Break,
    Continue,

    // 路由
    Route {
        name: String,
        target: NodeId,
    },

    // 可观测性
    Observe {
        config: ObserveConfig,
        body: Vec<NodeId>,
    },
    Span {
        name: String,
        attributes: Vec<(String, NodeId)>,
        body: Vec<NodeId>,
    },
    RecordTokens {
        input: NodeId,
        output: NodeId,
    },

    // Trait 系统
    TraitDef {
        name: String,
        generics: Vec<GenericParam>,
        parents: Vec<String>,
        trait_where: Vec<GenericParam>,
        methods: Vec<TraitMethod>,
    },
    ImplDef {
        generics: Vec<GenericParam>,
        trait_generics: Vec<String>,
        trait_name: String,
        for_type: String,
        for_generics: Vec<String>,
        where_clause: Vec<GenericParam>,
        methods: Vec<FnDef>,
    },

    // v0.19: Worker 并发
    Worker {
        name: String,
        body: Vec<NodeId>,
    },
    Send {
        value: NodeId,
        target: String,
    },
    Receive {
        var: String,
        source: String,
    },

    // v0.19: 事务
    Transaction {
        body: Vec<NodeId>,
        compensation: Vec<NodeId>,
    },
    Commit,
    Rollback,

    // v0.20: 宏定义
    MacroDef {
        name: String,
        params: Vec<String>,
        body: Vec<NodeId>,
    },

    // v0.23: 类型系统增强
    TypeAlias {
        name: String,
        generics: Vec<String>,
        target: String,
    },
    EnumDef {
        name: String,
        generics: Vec<String>,
        variants: Vec<crate::ast::EnumVariant>,
    },
    StructDef {
        name: String,
        generics: Vec<String>,
        fields: Vec<crate::ast::StructField>,
    },
}

// ===================================================================
// AstArena
// ===================================================================

/// AST 节点分配器
#[derive(Debug)]
pub struct AstArena {
    pub exprs: Vec<TypedExpr>,
    pub stmts: Vec<TypedStmt>,
}

impl Default for AstArena {
    fn default() -> Self {
        Self::new()
    }
}

impl AstArena {
    pub fn new() -> Self {
        Self {
            exprs: Vec::new(),
            stmts: Vec::new(),
        }
    }

    /// 分配表达式节点
    pub fn alloc_expr(&mut self, kind: ExprKind, span: Span) -> NodeId {
        let id = NodeId(self.exprs.len());
        self.exprs.push(TypedExpr {
            id,
            kind,
            span,
            ty: None,
        });
        id
    }

    /// 分配语句节点
    pub fn alloc_stmt(&mut self, kind: StmtKind, span: Span) -> NodeId {
        let id = NodeId(self.stmts.len()); // 使用 stmts 的索引
        self.stmts.push(TypedStmt { id, kind, span });
        id
    }

    /// 获取表达式
    pub fn get_expr(&self, id: NodeId) -> Option<&TypedExpr> {
        self.exprs.get(id.0)
    }

    /// 获取可变表达式
    pub fn get_expr_mut(&mut self, id: NodeId) -> Option<&mut TypedExpr> {
        self.exprs.get_mut(id.0)
    }

    /// 获取语句
    pub fn get_stmt(&self, id: NodeId) -> Option<&TypedStmt> {
        self.stmts.get(id.0)
    }

    /// 设置表达式类型
    pub fn set_type(&mut self, id: NodeId, ty: Type) {
        if let Some(expr) = self.exprs.get_mut(id.0) {
            expr.ty = Some(ty);
        }
    }

    /// 获取表达式类型
    pub fn get_type(&self, id: NodeId) -> Option<&Type> {
        self.exprs.get(id.0)?.ty.as_ref()
    }
}

// ===================================================================
// AstVisitor
// ===================================================================

/// AST 访问者 trait
pub trait AstVisitor<T> {
    fn visit_expr(&mut self, arena: &AstArena, expr: &TypedExpr) -> T;
    fn visit_stmt(&mut self, arena: &AstArena, stmt: &TypedStmt) -> T;
    fn visit_pattern(&mut self, pat: &Pattern) -> T;
}

/// 遍历工具函数
pub fn walk_expr<V: AstVisitor<T>, T>(visitor: &mut V, arena: &AstArena, expr: &TypedExpr) -> T {
    match &expr.kind {
        ExprKind::Literal(_) | ExprKind::Variable(_) => visitor.visit_expr(arena, expr),
        ExprKind::Binary { left, right, .. } => {
            let _ = visitor.visit_expr(arena, arena.get_expr(*left).unwrap());
            let _ = visitor.visit_expr(arena, arena.get_expr(*right).unwrap());
            visitor.visit_expr(arena, expr)
        }
        ExprKind::Pipe { left, right } => {
            let _ = visitor.visit_expr(arena, arena.get_expr(*left).unwrap());
            let _ = visitor.visit_expr(arena, arena.get_expr(*right).unwrap());
            visitor.visit_expr(arena, expr)
        }
        ExprKind::Call { args, .. } => {
            for arg in args {
                let _ = visitor.visit_expr(arena, arena.get_expr(*arg).unwrap());
            }
            visitor.visit_expr(arena, expr)
        }
        ExprKind::MethodCall { object, args, .. } => {
            let _ = visitor.visit_expr(arena, arena.get_expr(*object).unwrap());
            for arg in args {
                let _ = visitor.visit_expr(arena, arena.get_expr(*arg).unwrap());
            }
            visitor.visit_expr(arena, expr)
        }
        ExprKind::Index { object, index } => {
            let _ = visitor.visit_expr(arena, arena.get_expr(*object).unwrap());
            let _ = visitor.visit_expr(arena, arena.get_expr(*index).unwrap());
            visitor.visit_expr(arena, expr)
        }
        ExprKind::Grouping(inner) => {
            let _ = visitor.visit_expr(arena, arena.get_expr(*inner).unwrap());
            visitor.visit_expr(arena, expr)
        }
        ExprKind::Borrow { expr: inner } | ExprKind::BorrowMut { expr: inner } => {
            let _ = visitor.visit_expr(arena, arena.get_expr(*inner).unwrap());
            visitor.visit_expr(arena, expr)
        }
        _ => visitor.visit_expr(arena, expr),
    }
}
