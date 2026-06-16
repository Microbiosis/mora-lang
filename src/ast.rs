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
    Let { name: String, type_hint: Option<String>, init: Expr, exported: bool, span: Span },
    Assign { name: String, value: Expr, span: Span },
    IndexAssign { object: Expr, index: Expr, value: Expr, span: Span },
    TaskDef { name: String, params: Vec<(String, Option<String>)>, return_type: Option<String>, body: Vec<Stmt>, exported: bool, span: Span },
    If { condition: Expr, then_branch: Vec<Stmt>, span: Span },
    For { var: String, var_type: Option<String>, iterable: Expr, body: Vec<Stmt>, span: Span },
    Try { try_block: Vec<Stmt>, catch_var: String, catch_type: Option<String>, catch_block: Vec<Stmt>, span: Span },
    Import { path: String, span: Span },
    Parallel { stmts: Vec<Stmt>, span: Span },
    Match { expr: Expr, arms: Vec<(Pattern, Vec<Stmt>)>, span: Span },
    Save { path: Expr, value: Expr, span: Span },
    Load { path: Expr, var: String, span: Span },
    ReadFile { path: Expr, var: String, span: Span },
    WriteFile { path: Expr, content: Expr, span: Span },
    AppendFile { path: Expr, content: Expr, span: Span },
    ReadBytesFile { path: Expr, var: String, span: Span },
    WriteBytesFile { path: Expr, content: Expr, span: Span },
    Return { value: Option<Expr>, span: Span },
    Expr(Expr),
    // v0.04.0: AI 原语
    With { bindings: Vec<(String, Expr)>, body: Vec<Stmt>, span: Span },
    StreamFor { prompt: Expr, var: String, body: Vec<Stmt>, span: Span },
    ToolDef { name: String, params: Vec<(String, Option<String>)>, return_type: Option<String>, body: Vec<Stmt>, exported: bool, span: Span },
    Break { span: Span },
    Continue { span: Span },
    // v0.04 终态: 云服务原生
    Serve { protocol: ServeProtocol, routes: Vec<RouteDecl>, body: Vec<Stmt>, span: Span },
    Route { name: String, target: Expr, span: Span },
    Observe { config: ObserveConfig, body: Vec<Stmt>, span: Span },
    Span { name: String, attributes: Vec<(String, Expr)>, body: Vec<Stmt>, span: Span },
    /// v0.04.0 终态补: 显式 token 计数（RFC §2.4 / §3.3）
    /// 语义: 累加到当前 TraceCollector，不触发预算超限
    RecordTokens { input: Expr, output: Expr, span: Span },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServeProtocol {
    Http { host: String, port: u16 },
    Mcp,
    Repl,
    Stdio,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Patch,
}

impl HttpMethod {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "GET" => Some(HttpMethod::Get),
            "POST" => Some(HttpMethod::Post),
            "PUT" => Some(HttpMethod::Put),
            "DELETE" => Some(HttpMethod::Delete),
            "PATCH" => Some(HttpMethod::Patch),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Delete => "DELETE",
            HttpMethod::Patch => "PATCH",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RouteDecl {
    HttpRoute { method: HttpMethod, path: String, handler: Expr },
    // v0.04 终态 Slice 5: ToolEntry 加 params + return_type 字段用于生成 JSON Schema
    ToolEntry { name: String, params: Vec<(String, Option<String>)>, return_type: Option<String>, handler: Expr },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ObserveConfig {
    Trace,
    Metrics,
    Otel { endpoint: Expr },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Binary { left: Box<Expr>, op: BinaryOp, right: Box<Expr>, span: Span },
    Pipe { left: Box<Expr>, right: Box<Expr>, span: Span },
    Call { callee: String, args: Vec<Box<Expr>>, span: Span },
    MethodCall { object: Box<Expr>, method: String, args: Vec<Box<Expr>>, span: Span },
    Index { object: Box<Expr>, index: Box<Expr>, span: Span },
    Closure { params: Vec<(String, Option<String>)>, return_type: Option<String>, body: Vec<Stmt>, span: Span },
    Match { expr: Box<Expr>, arms: Vec<(Pattern, Box<Expr>)>, span: Span },
    Literal(Literal),
    Variable(String, Span),
    Grouping(Box<Expr>, Span),
    // v0.04.0: AI 原语
    Prompt { parts: Vec<Expr>, span: Span },
    // v0.04 终态: route 调用
    RouteCall { name: String, args: Vec<Box<Expr>>, span: Span },
    // v0.04 终态补: ai_model(...) 路由元数据表达式（RFC §2.3）
    // 解析: ai_model("model-name", temperature: 0.7, max_tokens: 2000, system: "...")
    AiModelCall {
        model: Box<Expr>,
        temperature: Option<Box<Expr>>,
        max_tokens: Option<Box<Expr>>,
        system: Option<Box<Expr>>,
        span: Span,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    String(String, Span),
    Number(f64, Span),
    Bool(bool, Span),
    Nil(Span),
    List(Vec<Box<Expr>>, Span),
    Dict(Vec<(String, Box<Expr>)>, Span),
}

#[derive(Debug, Clone, PartialEq)]
pub enum BinaryOp {
    Add, Sub, Mul, Div, Mod,
    Equal, NotEqual,
    Greater, Less, GreaterEqual, LessEqual,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Literal(Literal),
    Variable(String),
    Wildcard,
    List(Vec<Pattern>),
    Dict(Vec<(String, Pattern)>),
}
