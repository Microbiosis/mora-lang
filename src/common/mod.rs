//! 共享类型定义
//!
//! 被 parser_v3、mir、typeck 和 runtime 共同引用的基础类型。

pub mod dtype;
pub mod lock;
pub mod sync;
pub mod sys;
pub mod trait_info;

pub use dtype::{
    DType, Device, ScalarBinaryOut, ScalarValue, bf16_bits_to_f64, f16_bits_to_f64,
    f64_to_bf16_bits, f64_to_f16_bits, f64_to_fp8_e4m3_bits, f64_to_fp8_e5m2_bits,
    fp8_e4m3_bits_to_f64, fp8_e5m2_bits_to_f64, pack_i4_nibble, unpack_i4_nibble,
};

/// 源码位置信息：所有需要报错的节点带 line 与 column。
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

/// 字面量值
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    String(String, Span),
    Char(char, Span),
    // v0.38: numeric tower — default 64-bit Int(i64) and Float(f64).
    Int(i64, Span),
    Float(f64, Span),
    /// Wave 3 Stage 3B: Explicit hardware width integer literal (`8i8`, `255u8`, `-4i4`, `42i32`).
    TypedInt(i64, DType, Span),
    /// Wave 3 Stage 3B: Explicit hardware width float literal (`0.5f16`, `1.0bf16`, `1.5fp8`, `1.5fp8_e5m2`, `2.0f32`).
    TypedFloat(f64, DType, Span),
    Bool(bool, Span),
    Nil(Span),
}

/// 二元运算符
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    MatMul,
    Equal,
    NotEqual,
    Greater,
    Less,
    GreaterEqual,
    LessEqual,
}

/// 一元运算符（v0.91）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// 取负：`Neg(x) = -x`
    Neg,
    /// 逻辑非：`Not(x) = x == 0`
    Not,
}

/// 泛型参数（trait/impl/method 的类型参数）
#[derive(Debug, Clone, PartialEq)]
pub struct GenericParam {
    pub name: String,
    pub bound: Option<String>,
    pub span: Span,
}

/// 枚举变体（支持无载荷与多载荷变体）
#[derive(Debug, Clone, PartialEq)]
pub struct EnumVariant {
    pub name: String,
    pub payload_types: Vec<String>,
}

/// 结构体字段（支持默认值标记与编译期 `@tag(...)` 元数据）
#[derive(Debug, Clone, PartialEq)]
pub struct StructField {
    pub name: String,
    pub type_hint: String,
    pub has_default: bool,
    pub tags: Vec<(String, String)>,
}

/// 编译期结构体完整元信息（供 `Struct::schema()` / `std/mcp` 消费）
#[derive(Debug, Clone, PartialEq)]
pub struct StructDefInfo {
    pub name: String,
    pub type_params: Vec<String>,
    pub fields: Vec<StructField>,
}

/// 编译期枚举完整元信息
#[derive(Debug, Clone, PartialEq)]
pub struct EnumDefInfo {
    pub name: String,
    pub type_params: Vec<String>,
    pub variants: Vec<EnumVariant>,
}
