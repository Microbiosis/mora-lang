//! v0.22: 新 AST 结构（Arena 分配 + NodeId + 类型信息）
//!
//! 设计原则：
//! - **Arena 分配**：所有节点在连续内存中，减少堆分配
//! - **NodeId**：通过 ID 引用节点，支持增量编译
//! - **类型信息保留**：类型检查后保留类型信息
//! - **Visitor 模式**：解耦遍历逻辑

use crate::common::{BinaryOp, GenericParam, Literal, Span};
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

    // v0.50: Command 构造表达式
    Command {
        goto: Option<String>,
        update: Vec<(String, NodeId)>,
        resume: Option<NodeId>,
    },
    // v0.50: Send 动态派发
    Send {
        target: String,
        input: NodeId,
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
        variants: Vec<crate::common::EnumVariant>,
    },
    StructDef {
        name: String,
        generics: Vec<String>,
        fields: Vec<crate::common::StructField>,
    },

    // v0.25: Multi-Agent 协调
    Orchestrate {
        input_var: String,
        result_var: String,
        kind: OrchestrateKind,
    },

    // v0.25: Eval 原语 — Agent 行为回归测试
    Eval {
        name: String,
        given: NodeId,
        expects: Vec<NodeId>,
        tolerance: Option<f64>,
        replay_path: Option<String>,
    },

    // v0.25: Skill 原语 — 可复用能力包
    SkillDef {
        name: String,
        description: Option<String>,
        version: Option<String>,
        requires: Vec<String>,
        tasks: Vec<SkillTask>,
        verify: Option<SkillVerify>,
    },

    // v0.26: Prompt section 块 — 声明一段 system prompt 分段
    // 与 'prompt "name" do ... end' 语法对应
    // body 内允许的子语句形态通过 PromptSectionStmt 静态约束
    PromptSection {
        name: String,
        body: Vec<NodeId>,
    },

    // v0.26: Prompt section 块内子语句 — 静态区分 set role / set budget / read / tail
    PromptSet {
        key: String,
        value: NodeId,
    },
    PromptRead(NodeId),
    // tail(...) 复用普通 Expr(Call) — callee == "tail" 由解释器识别

    // v0.27: Document section 块 — 声明一段文档分段
    // 与 'document "name" do ... end' 语法对应
    // body 内允许的子语句形态: DocumentSet / DocumentRead
    DocumentSection {
        name: String,
        body: Vec<NodeId>,
    },

    // v0.27: Document section 块内子语句 — 静态区分 set origin / set max_pages / read
    DocumentSet {
        key: String,
        value: NodeId,
    },
    DocumentRead(NodeId),
}

/// v0.26: Prompt section 块内子语句
#[derive(Debug, Clone)]
pub enum PromptSectionStmt {
    /// `set role: <expr>` — 设置 role
    SetRole(NodeId),
    /// `set budget: <expr>` — 设置 byte 预算
    SetBudget(NodeId),
    /// `read <path>` — 读文件
    Read(NodeId),
    /// `tail <path>, max: <n>` — 取尾 N 行
    Tail { path: NodeId, max: NodeId },
}

/// v0.25: 编排模式
#[derive(Debug, Clone)]
pub enum OrchestrateKind {
    /// 线性管道：agent 依次执行
    Sequential { agents: Vec<OrchestrateAgent> },
    /// 有向图：带条件路由
    Graph {
        agents: Vec<OrchestrateAgent>,
        edges: Vec<OrchestrateEdge>,
    },
    /// 迭代精炼：重复执行直到条件满足
    Loop {
        agent: OrchestrateAgent,
        max_rounds: usize,
        exit_when: Option<NodeId>,
    },
    // v0.50: Pregel BSP 执行模型
    Pregel {
        agents: Vec<OrchestrateAgent>,
        edges: Vec<OrchestrateEdge>,
        state_schema: Vec<StateChannel>,
        checkpoint: Option<CheckpointConfig>,
        interrupt_points: Vec<InterruptPoint>,
    },
}

/// v0.25: 编排中的 Agent 声明
#[derive(Debug, Clone)]
pub struct OrchestrateAgent {
    pub name: String,
    pub with_config: Option<Vec<(String, NodeId)>>,
    pub task_expr: NodeId,
    pub verify_expr: Option<NodeId>,
}

/// v0.25: 编排中的边（Graph 模式）
#[derive(Debug, Clone)]
pub struct OrchestrateEdge {
    pub from: String,
    pub to: String,
    pub condition: Option<NodeId>,
    pub dynamic: Option<DynamicKind>, // v0.50
}

/// v0.50: 状态通道定义（Schema 声明）
#[derive(Debug, Clone)]
pub struct StateChannel {
    pub name: String,
    pub type_hint: Option<String>,
    pub reducer: ReducerKind,
}

/// v0.50: Reducer 合并语义
#[derive(Debug, Clone)]
pub enum ReducerKind {
    Last,          // 默认：覆盖
    Append,        // 列表追加
    Add,           // 数值相加
    Merge(NodeId), // 自定义合并函数
}

/// v0.50: 检查点配置
#[derive(Debug, Clone)]
pub struct CheckpointConfig {
    pub saver: String,             // "memory" | "sqlite"
    pub thread_id: Option<NodeId>, // 表达式
}

/// v0.50: 中断点
#[derive(Debug, Clone)]
pub struct InterruptPoint {
    pub node_name: String,
    pub when: InterruptWhen,
}

#[derive(Debug, Clone)]
pub enum InterruptWhen {
    Before,
    After,
}

/// v0.50: 动态派发类型
#[derive(Debug, Clone)]
pub enum DynamicKind {
    Map,    // 动态展开为 N 个并行任务
    Reduce, // 聚合 N 个并行结果
    FanOut, // 固定并行 worker
    FanIn,  // 等待汇聚
}

/// v0.25: Skill 中的任务定义
#[derive(Debug, Clone)]
pub struct SkillTask {
    pub name: String,
    pub params: Vec<(String, Option<String>)>,
    pub return_type: Option<String>,
    pub body: Vec<NodeId>,
}

/// v0.25: Skill 中的验证函数
#[derive(Debug, Clone)]
pub struct SkillVerify {
    pub params: Vec<(String, Option<String>)>,
    pub body: Vec<NodeId>,
}

// ===================================================================
// AstArena
// ===================================================================

/// AST 节点分配器
#[derive(Debug, Clone)]
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
            // v0.35 (P0-B1): previously .unwrap() panicked on dangling NodeId.
            // Skip child silently — visitor fallthrough handles missing data.
            if let Some(left_expr) = arena.get_expr(*left) {
                let _ = visitor.visit_expr(arena, left_expr);
            }
            if let Some(right_expr) = arena.get_expr(*right) {
                let _ = visitor.visit_expr(arena, right_expr);
            }
            visitor.visit_expr(arena, expr)
        }
        ExprKind::Pipe { left, right } => {
            if let Some(left_expr) = arena.get_expr(*left) {
                let _ = visitor.visit_expr(arena, left_expr);
            }
            if let Some(right_expr) = arena.get_expr(*right) {
                let _ = visitor.visit_expr(arena, right_expr);
            }
            visitor.visit_expr(arena, expr)
        }
        ExprKind::Call { args, .. } => {
            for arg in args {
                if let Some(arg_expr) = arena.get_expr(*arg) {
                    let _ = visitor.visit_expr(arena, arg_expr);
                }
            }
            visitor.visit_expr(arena, expr)
        }
        ExprKind::MethodCall { object, args, .. } => {
            if let Some(obj_expr) = arena.get_expr(*object) {
                let _ = visitor.visit_expr(arena, obj_expr);
            }
            for arg in args {
                if let Some(arg_expr) = arena.get_expr(*arg) {
                    let _ = visitor.visit_expr(arena, arg_expr);
                }
            }
            visitor.visit_expr(arena, expr)
        }
        ExprKind::Index { object, index } => {
            if let Some(obj_expr) = arena.get_expr(*object) {
                let _ = visitor.visit_expr(arena, obj_expr);
            }
            if let Some(idx_expr) = arena.get_expr(*index) {
                let _ = visitor.visit_expr(arena, idx_expr);
            }
            visitor.visit_expr(arena, expr)
        }
        ExprKind::Grouping(inner) => {
            if let Some(inner_expr) = arena.get_expr(*inner) {
                let _ = visitor.visit_expr(arena, inner_expr);
            }
            visitor.visit_expr(arena, expr)
        }
        ExprKind::Borrow { expr: inner } | ExprKind::BorrowMut { expr: inner } => {
            if let Some(inner_expr) = arena.get_expr(*inner) {
                let _ = visitor.visit_expr(arena, inner_expr);
            }
            visitor.visit_expr(arena, expr)
        }
        _ => visitor.visit_expr(arena, expr),
    }
}
