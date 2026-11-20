//! MIR 层的用户显式类型注解边界类型（[`TypeHint`]）。
//!
//! ## 设计
//!
//! [`TypeHint`] 透明包装 [`crate::typeck::Type`]，用于在 [`crate::mir::witness::MirWitness`]
//! 中区分「用户在源码中显式书写的类型注解」与「统一 HM 约束求解器的推断产物」。
//! 通过 `from_type` / `to_type` / `into_type` 在 `parser_v3`、`mir` 与 `typeck` 之间
//! 保持清晰的模块边界。

use std::fmt;

/// MIR 层的用户显式类型注解包装类型。
#[derive(Debug, Clone, PartialEq)]
pub struct TypeHint(pub crate::typeck::Type);

impl TypeHint {
    /// 从 [`crate::typeck::Type`] 构造 `TypeHint`。
    pub fn from_type(ty: crate::typeck::Type) -> Self {
        TypeHint(ty)
    }

    /// 借用内部 [`crate::typeck::Type`]，供 HM 约束求解器消费。
    pub fn to_type(&self) -> &crate::typeck::Type {
        &self.0
    }

    /// 消费并取出内部 [`crate::typeck::Type`]。
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
