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
    Try { try_block: Vec<Stmt>, catch_var: String, catch_block: Vec<Stmt>, span: Span },
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
