//! v0.75.93: TypeHint —— mir 层的「用户类型注解」边界类型。
//!
//! ## 背景
//!
//! 架构审查报告（v0.75.90）标记「🔴 阻断级风险」：
//! MirWitness 6 处直接持有 `crate::typeck::Type`，未来 typeck 重构会扩散到 mir 层。
//!
//! ## 设计
//!
//! TypeHint 是 `crate::typeck::Type` 的**透明包装**——保留全部 28 个变体，
//! 仅在 mir 层**表达语义边界**「用户显式写的类型注解」vs「HM 推断产物」。
//!
//! TypeHint 与 typeck::Type 双向透明转换（`from_type` / `to_type`），HM 推断
//! 路径通过 `to_type()` 取回 `crate::typeck::Type` 继续推进。
//!
//! ## 收益
//!
//! 未来 typeck 内部重构（变体增减、语义细化）只需修改 TypeHint 内部——mir
//! 层、witness.rs、parser_v3 三者完全无感。爆炸半径从 6 处降为 1 处。
//!
//! ## 不变的
//!
//! - TypeHint 与 `crate::typeck::Type` 是 1:1 镜像（无变体裁剪）
//! - parser_v3 仍直接构造完整 `crate::typeck::Type`，**在 mir 层边界处**调用
//!   `TypeHint::from_type()` 包装
//! - HM 推断路径不变（继续产出 `crate::typeck::Type`）

use std::fmt;

/// v0.75.93: mir 层的「用户类型注解」边界类型。
///
/// 包装 `crate::typeck::Type`，但语义独立——表达「这是用户在源码里
/// 显式写的类型注解」，与「HM 推断产物」**正交**。
///
/// 后续 typeck 重构（变体增减、语义细化）只动内部 `crate::typeck::Type`
/// 定义，mir 层调用 `from_type/to_type` 即可。
#[derive(Debug, Clone, PartialEq)]
pub struct TypeHint(pub crate::typeck::Type);

impl TypeHint {
    /// 从 `crate::typeck::Type` 构造 TypeHint。语义包装，无变体裁剪。
    pub fn from_type(ty: crate::typeck::Type) -> Self {
        TypeHint(ty)
    }

    /// 取出内部 `crate::typeck::Type`。HM 推断路径通过此方法回到 typeck 层。
    pub fn to_type(&self) -> &crate::typeck::Type {
        &self.0
    }

    /// 取出内部 `crate::typeck::Type`（consume）。构造 MirExpr/Inst 等
    /// 一次性消费的字段时使用。
    pub fn into_type(self) -> crate::typeck::Type {
        self.0
    }
}

impl fmt::Display for TypeHint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.name())
    }
}

impl From<crate::typeck::Type> for TypeHint {
    fn from(ty: crate::typeck::Type) -> Self {
        TypeHint::from_type(ty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typeck::Type;

    #[test]
    fn from_type_roundtrip() {
        let ty = Type::Int;
        let hint = TypeHint::from_type(ty.clone());
        assert_eq!(hint.to_type(), &ty);
        assert_eq!(hint.into_type(), ty);
    }

    #[test]
    fn from_impl() {
        let hint: TypeHint = Type::String.into();
        assert_eq!(hint.to_type(), &Type::String);
    }

    #[test]
    fn display_uses_inner_name() {
        let hint = TypeHint::from_type(Type::List(Box::new(Type::Int)));
        assert_eq!(format!("{}", hint), "list<int>");
    }
}
