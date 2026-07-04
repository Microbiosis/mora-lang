//! 共享类型定义
//!
//! 被 ast_v2 和其他模块共同引用的基础类型。
//! 这些类型不依赖任何 AST 的 StmtKind/ExprKind/NodeId，是纯粹的数据结构。

/// 源码位置信息：所有需要报错的 AST 节点带 line。
/// `column` 当前未使用（保留以备后续 LSP / 编辑器支持）。
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

/// 字面量值（v2 版：List/Dict 使用 NodeId，不含 Expr 引用）
///
/// v1 版 Literal 在 ast.rs 中定义（含 `Box<Expr>`），此版供 v2 AST 使用。
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    String(String, Span),
    Char(char, Span),
    // v0.38: numeric tower — Int/Float distinct from Number legacy.
    Int(i64, Span),
    Float(f64, Span),
    Number(f64, Span),
    Bool(bool, Span),
    Nil(Span),
}

/// 二元运算符
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

/// 泛型参数（trait/impl/method 的类型参数）
///
/// 例如 `trait Foo<T>` / `impl<T> Foo<T> for Bar` 中的 `T`
#[derive(Debug, Clone, PartialEq)]
pub struct GenericParam {
    pub name: String,
    pub bound: Option<String>,
    pub span: Span,
}

/// 枚举变体
#[derive(Debug, Clone, PartialEq)]
pub struct EnumVariant {
    pub name: String,
    pub data: Option<String>, // 变体携带的数据类型
}

/// 结构体字段
#[derive(Debug, Clone, PartialEq)]
pub struct StructField {
    pub name: String,
    pub type_hint: String,
}
