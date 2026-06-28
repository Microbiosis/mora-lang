/// v11 typeck 定位信息：所有需要报错的 AST 节点带 line（最少开销）。
/// `column` 当前未使用（保留以备后续 LSP / 编辑器支持）。
/// 节点没设的 span 默认为 Span::default()（line=0），typeck 报错时退到 "unknown line"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub line: usize,
    pub column: usize,
}

impl Span {
    pub fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }
    pub fn at_line(line: usize) -> Self {
        Self { line, column: 0 }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum Stmt {
    Let {
        name: String,
        type_hint: Option<String>,
        init: Expr,
        exported: bool,
        span: Span,
    },
    Assign {
        name: String,
        value: Expr,
        span: Span,
    },
    IndexAssign {
        object: Expr,
        index: Expr,
        value: Expr,
        span: Span,
    },
    TaskDef {
        name: String,
        lifetime_params: Vec<String>, // v0.21: 生命周期参数 <'a, 'b>
        params: Vec<(String, Option<String>)>,
        return_type: Option<String>,
        body: Vec<Stmt>,
        exported: bool,
        span: Span,
    },
    If {
        condition: Expr,
        then_branch: Vec<Stmt>,
        span: Span,
    },
    For {
        var: String,
        var_type: Option<String>,
        iterable: Expr,
        body: Vec<Stmt>,
        span: Span,
    },
    Import {
        path: String,
        span: Span,
    },
    Parallel {
        stmts: Vec<Stmt>,
        span: Span,
    },
    /// v0.19: Worker 声明 (Ballerina 启发)
    Worker {
        name: String,
        body: Vec<Stmt>,
        span: Span,
    },
    /// v0.19: 发送消息 `-> value to worker`
    Send {
        value: Expr,
        target: String,
        span: Span,
    },
    /// v0.19: 接收消息 `let x = <- worker`
    Receive {
        var: String,
        source: String,
        span: Span,
    },
    /// v0.19: 事务块 (Ballerina 启发)
    Transaction {
        body: Vec<Stmt>,
        compensation: Vec<Stmt>,
        span: Span,
    },
    /// v0.19: 提交事务
    Commit {
        span: Span,
    },
    /// v0.19: 回滚事务
    Rollback {
        span: Span,
    },
    /// v0.20: 宏定义 (Common Lisp 启发)
    MacroDef {
        name: String,
        params: Vec<String>,
        body: Vec<Stmt>,
        span: Span,
    },
    Match {
        expr: Expr,
        arms: Vec<(Pattern, Vec<Stmt>)>,
        span: Span,
    },
    Save {
        path: Expr,
        value: Expr,
        span: Span,
    },
    Load {
        path: Expr,
        var: String,
        span: Span,
    },
    ReadFile {
        path: Expr,
        var: String,
        span: Span,
    },
    WriteFile {
        path: Expr,
        content: Expr,
        span: Span,
    },
    AppendFile {
        path: Expr,
        content: Expr,
        span: Span,
    },
    ReadBytesFile {
        path: Expr,
        var: String,
        span: Span,
    },
    WriteBytesFile {
        path: Expr,
        content: Expr,
        span: Span,
    },
    Return {
        value: Option<Expr>,
        span: Span,
    },
    Expr(Expr),
    // v0.04.0: AI 原语
    With {
        bindings: Vec<(String, Expr)>,
        body: Vec<Stmt>,
        span: Span,
    },
    StreamFor {
        prompt: Expr,
        var: String,
        body: Vec<Stmt>,
        span: Span,
    },
    ToolDef {
        name: String,
        params: Vec<(String, Option<String>)>,
        return_type: Option<String>,
        body: Vec<Stmt>,
        exported: bool,
        span: Span,
    },
    Break {
        span: Span,
    },
    Continue {
        span: Span,
    },
    // v0.04: 云服务原生（serve as 语法糖已移除，走显式 Router/McpServer API）
    Route {
        name: String,
        target: Expr,
        span: Span,
    },
    Observe {
        config: ObserveConfig,
        body: Vec<Stmt>,
        span: Span,
    },
    Span {
        name: String,
        attributes: Vec<(String, Expr)>,
        body: Vec<Stmt>,
        span: Span,
    },
    /// v0.04.0 终态补: 显式 token 计数（RFC §2.4 / §3.3）
    /// 语义: 累加到当前 TraceCollector，不触发预算超限
    RecordTokens {
        input: Expr,
        output: Expr,
        span: Span,
    },
    // v0.08: trait 系统
    TraitDef {
        name: String,
        generics: Vec<GenericParam>,
        parents: Vec<String>,
        /// v0.09 完整版: trait 自身的 where 子句（与 impl 共享 trait_where 字段）
        trait_where: Vec<GenericParam>,
        methods: Vec<TraitMethod>,
        span: Span,
    },
    ImplDef {
        generics: Vec<GenericParam>,
        trait_generics: Vec<String>,
        trait_name: String,
        for_type: String,
        for_generics: Vec<String>,
        where_clause: Vec<GenericParam>,
        methods: Vec<FnDef>,
        span: Span,
    },
    // v0.23: 类型别名 (type alias)
    TypeAlias {
        name: String,
        generics: Vec<String>,
        target: String,
        span: Span,
    },
    // v0.23: 枚举类型
    EnumDef {
        name: String,
        generics: Vec<String>,
        variants: Vec<EnumVariant>,
        span: Span,
    },
    // v0.23: 结构体类型
    StructDef {
        name: String,
        generics: Vec<String>,
        fields: Vec<StructField>,
        span: Span,
    },
}

/// v0.23: 枚举变体
#[derive(Debug, Clone, PartialEq)]
pub struct EnumVariant {
    pub name: String,
    pub data: Option<String>, // 变体携带的数据类型
}

/// v0.23: 结构体字段
#[derive(Debug, Clone, PartialEq)]
pub struct StructField {
    pub name: String,
    pub type_hint: String,
}

/// v0.09: 泛型参数（trait/impl/method 的类型参数）
///   例如 `trait Foo<T>` / `impl<T> Foo<T> for Bar` 中的 `T`
///   v0.09 简化: bound 是 trait 名（如 "Comparable"），v0.10 改为 Vec<TraitRef>
#[derive(Debug, Clone, PartialEq)]
pub struct GenericParam {
    pub name: String,
    /// v0.09: bound 作为字符串（trait 名），v0.10 强化为结构化
    pub bound: Option<String>,
    pub span: Span,
}

/// v0.08: trait 方法签名（v0.08.3: body 可选——非空表示默认实现）
#[derive(Debug, Clone, PartialEq)]
pub struct TraitMethod {
    pub name: String,
    pub params: Vec<(String, Option<String>)>,
    pub return_type: Option<String>,
    /// v0.08.3: 默认实现 body。空 = 纯签名；非空 = 默认实现（impl 可省略）
    pub body: Vec<Stmt>,
    /// v0.09: trait method 自己的泛型参数（少见，类比 Rust trait 默认方法）
    pub generics: Vec<GenericParam>,
    pub span: Span,
}

/// v0.08: 通用函数定义（用于 impl 块方法体）
#[derive(Debug, Clone, PartialEq)]
pub struct FnDef {
    pub name: String,
    pub params: Vec<(String, Option<String>)>,
    pub return_type: Option<String>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ObserveConfig {
    Trace,
    Metrics,
    Otel { endpoint: Expr },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Binary {
        left: Box<Expr>,
        op: BinaryOp,
        right: Box<Expr>,
        span: Span,
    },
    Pipe {
        left: Box<Expr>,
        right: Box<Expr>,
        span: Span,
    },
    Call {
        callee: String,
        args: Vec<Box<Expr>>,
        span: Span,
    },
    MethodCall {
        object: Box<Expr>,
        method: String,
        args: Vec<Box<Expr>>,
        span: Span,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
        span: Span,
    },
    Closure {
        params: Vec<(String, Option<String>)>,
        return_type: Option<String>,
        body: Vec<Stmt>,
        span: Span,
    },
    /// v0.21: 不可变借用 `&expr`
    Borrow {
        expr: Box<Expr>,
        span: Span,
    },
    /// v0.21: 可变借用 `&mut expr`
    BorrowMut {
        expr: Box<Expr>,
        span: Span,
    },
    Match {
        expr: Box<Expr>,
        arms: Vec<(Pattern, Box<Expr>)>,
        span: Span,
    },
    Literal(Literal),
    Variable(String, Span),
    Grouping(Box<Expr>, Span),
    // v0.04.0: AI 原语
    Prompt {
        parts: Vec<Expr>,
        span: Span,
    },
    // v0.04: route 调用
    RouteCall {
        name: String,
        args: Vec<Box<Expr>>,
        span: Span,
    },
    // v0.04补: ai_model(...) 路由元数据表达式（RFC §2.3）
    // 解析: ai_model("model-name", temperature: 0.7, max_tokens: 2000, system: "...")
    AiModelCall {
        model: Box<Expr>,
        temperature: Option<Box<Expr>>,
        max_tokens: Option<Box<Expr>>,
        system: Option<Box<Expr>>,
        span: Span,
    },
    // v0.06.2: ? 操作符（Result<T,E> 的早 return 语法糖）
    Question {
        expr: Box<Expr>,
        span: Span,
    },
    // v0.07.1: NamespaceRef — IDENT::IDENT 解析, 如 Router::new / McpServer::new
    NamespaceRef {
        namespace: String,
        name: String,
        span: Span,
    },
    // v0.08: dyn trait 类型标注
    DynTrait {
        generics: Vec<String>,
        trait_name: String,
        span: Span,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    String(String, Span),
    /// v0.x: 单字符字面量（`'a'`）
    Char(char, Span),
    Number(f64, Span),
    Bool(bool, Span),
    Nil(Span),
    List(Vec<Box<Expr>>, Span),
    Dict(Vec<(String, Box<Expr>)>, Span),
}

#[derive(Debug, Clone, PartialEq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Equal,
    NotEqual,
    Greater,
    Less,
    GreaterEqual,
    LessEqual,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Literal(Literal),
    Variable(String),
    Wildcard,
    /// 列表模式：`[a, b, c]` 或 `[a, b, ...rest]`
    /// 前缀模式数 + 可选 rest 变量
    List {
        prefix: Vec<Pattern>,
        rest: Option<String>, // ...rest 变量名
    },
    Dict(Vec<(String, Pattern)>),
    /// v0.16: 守卫模式 (Prolog 启发)
    /// `pattern when condition`
    Guard {
        pattern: Box<Pattern>,
        condition: Box<Expr>,
    },
}
