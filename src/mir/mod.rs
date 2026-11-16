//! v0.110.0: 核心 MIR 子系统（Control SSA + Witness + Effect Row + Opt Hints）

pub mod control;
pub mod effect;
pub mod hint;
pub mod witness;

/// 函数参数定义（含可选类型注解和默认值 witness）
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub type_hint: Option<crate::typeck::Type>,
    pub default: Option<crate::mir::witness::MirWitness>,
}
