//! 共享类型定义
//!
//! 被 ast_v2 和其他模块共同引用的基础类型。
//! 这些类型不依赖任何 AST 的 StmtKind/ExprKind/NodeId，是纯粹的数据结构。

pub mod trait_info;

/// 源码位置信息：所有需要报错的 AST 节点带 line。
/// `column` 当前未使用（保留以备后续 LSP / 编辑器支持）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
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
    // v0.38: numeric tower — Int and Float.
    Int(i64, Span),
    Float(f64, Span),
    // v0.91: BigInt 字面量（来自 `<digits>n` 语法）。
    // 持有 num_bigint::BigInt 的克隆以避开 parser 与 value 层之间的耦合。
    BigInt(num_bigint::BigInt, Span),
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

/// 一元运算符（v0.91）
///
/// 此前 `-x` 在 parse/emit 阶段降级为 `0 - x`、`not x` 降级为 `0 == x`。
/// 引入 enum 是为类型系统明确化（unary 路径有独立 promotion 规则，
/// 特别是 BigInt/Float/Int 的 unary minus 需要区分行为）。
#[derive(Debug, Clone, PartialEq)]
pub enum UnaryOp {
    /// 取负：`Neg(x) = -x`
    Neg,
    /// 逻辑非：`Not(x) = x == 0`（沿用原有 truthiness 语义）
    Not,
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

/// v0.83: TEA Msg 变体 — 类似 EnumVariant，但 payload 字段更明确
///（None = 无 payload，Some(type) = 携带该类型的 payload）
#[derive(Debug, Clone, PartialEq)]
pub struct MsgVariant {
    pub name: String,
    /// None = unit variant（`Increment`），Some(type) = 带 payload（`SetStep(int)`）
    pub payload_type: Option<String>,
}
