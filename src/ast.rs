#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum Stmt {
    Let { name: String, type_hint: Option<String>, init: Expr, exported: bool },
    Assign { name: String, value: Expr },
    IndexAssign { object: Expr, index: Expr, value: Expr },
    TaskDef { name: String, params: Vec<(String, Option<String>)>, body: Vec<Stmt>, exported: bool },
    If { condition: Expr, then_branch: Vec<Stmt> },
    For { var: String, iterable: Expr, body: Vec<Stmt> },
    Try { try_block: Vec<Stmt>, catch_var: String, catch_block: Vec<Stmt> },
    Import { path: String },
    Parallel { stmts: Vec<Stmt> },
    Match { expr: Expr, arms: Vec<(Pattern, Vec<Stmt>)> },
    Save { path: Expr, value: Expr },
    Load { path: Expr, var: String },
    Return { value: Option<Expr> },
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Binary { left: Box<Expr>, op: BinaryOp, right: Box<Expr> },
    Pipe { left: Box<Expr>, right: Box<Expr> },
    Call { callee: String, args: Vec<Expr> },
    MethodCall { object: Box<Expr>, method: String, args: Vec<Expr> },
    Index { object: Box<Expr>, index: Box<Expr> },
    Closure { params: Vec<(String, Option<String>)>, body: Vec<Stmt> },
    Match { expr: Box<Expr>, arms: Vec<(Pattern, Expr)> },
    Literal(Literal),
    Variable(String),
    Grouping(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    String(String),
    Number(f64),
    Bool(bool),
    Nil,
    List(Vec<Expr>),
    Dict(Vec<(String, Expr)>),
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
