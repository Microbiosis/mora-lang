//! v0.58: MIR Pattern 匹配框架（Phase H）
//!
//! 完全 MIR-native：不引入任何 AST 节点。Pattern 直接匹配 `MirInst` 变体。
//!
//! 设计目标：
//! - 模式匹配 `MirInst`（不是 `ExprKind`/`StmtKind` — 后者已删除）
//! - 抽取变量绑定（dst/lhs/rhs 等 Reg）供 RewriteRule 使用
//! - 简洁：仅覆盖最常用变体（v0.1），未来按需扩展

use crate::common::BinaryOp;
use crate::mir::{Label, MirInst, Reg};
use crate::value::Value;

/// Pattern matcher for `MirInst` — 完全 MIR-native
///
/// 每种 pattern 描述一类指令结构。例如：
/// - `Const { dst, value }` 匹配 `MirInst::Const(dst, value)`
/// - `ConstBool { dst, value }` 匹配 `MirInst::Const(dst, Bool(v))`
/// - `BinaryOp { dst, op, lhs, rhs }` 匹配 `MirInst::BinaryOp(dst, lhs, op, rhs)`
/// - `Jump { target }` 匹配 `MirInst::Jump(target)`
/// - `Call { dst, name }` 匹配 `MirInst::Call(dst, name, _)`
/// - `Label { target }` 匹配 `MirInst::Label(target)`
#[derive(Debug, Clone, PartialEq)]
pub enum MirPattern {
    /// 匹配任意指令（用于 body-level pass 如 DeadAfterReturn）
    Any,
    /// `MirInst::Const(dst, value)` — 匹配任意常量
    Const {
        dst: RegMatcher,
        value: ValueMatcher,
    },
    /// `MirInst::Const(dst, Bool(v))` — 专门匹配布尔常量
    ConstBool {
        dst: RegMatcher,
        value: bool,
    },
    /// `MirInst::BinaryOp(dst, lhs, op, rhs)`
    BinaryOp {
        dst: RegMatcher,
        op: OpMatcher,
        lhs: RegMatcher,
        rhs: RegMatcher,
    },
    /// `MirInst::Jump(target)`
    Jump { target: LabelMatcher },
    /// `MirInst::JumpIf(cond_reg, target)`
    JumpIf {
        cond: RegMatcher,
        target: LabelMatcher,
    },
    /// `MirInst::JumpIfNot(cond_reg, target)`
    JumpIfNot {
        cond: RegMatcher,
        target: LabelMatcher,
    },
    /// `MirInst::Return(value)` — `value` 为 `Option<Reg>`
    Return { value: RegOptMatcher },
    /// `MirInst::Call(dst, name, args)` — 函数调用
    Call {
        dst: RegMatcher,
        name: CallNameMatcher,
        args: RegsMatcher,
    },
    /// `MirInst::Label(target)` — 基本块边界标记
    Label { target: LabelMatcher },
}

/// Match 模式：捕获变量绑定
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MatchBindings {
    bindings: Vec<(String, BindingValue)>,
}

/// Pattern 提取出的值（Reg / Value / Label / BinaryOp）
#[derive(Debug, Clone, PartialEq)]
pub enum BindingValue {
    Reg(Reg),
    Value(Value),
    Label(Label),
    Op(BinaryOp),
    RegOpt(Option<Reg>),
}

impl MatchBindings {
    /// 构造空绑定表
    pub fn new() -> Self {
        Self::default()
    }

    /// 插入一个绑定
    pub fn insert(&mut self, key: impl Into<String>, value: BindingValue) {
        self.bindings.push((key.into(), value));
    }

    /// 按名称查询 Reg 绑定
    pub fn get_reg(&self, key: &str) -> Option<Reg> {
        self.bindings
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| match v {
                BindingValue::Reg(r) => Some(*r),
                _ => None,
            })
    }

    /// 按名称查询 Value 绑定
    pub fn get_value(&self, key: &str) -> Option<&Value> {
        self.bindings.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
            BindingValue::Value(v) => Some(v),
            _ => None,
        })
    }

    /// 按名称查询 Label 绑定
    pub fn get_label(&self, key: &str) -> Option<Label> {
        self.bindings
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| match v {
                BindingValue::Label(l) => Some(*l),
                _ => None,
            })
    }

    /// 按名称查询 BinaryOp 绑定
    pub fn get_op(&self, key: &str) -> Option<&BinaryOp> {
        self.bindings.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
            BindingValue::Op(o) => Some(o),
            _ => None,
        })
    }

    /// 按名称查询 Option<Reg> 绑定
    pub fn get_reg_opt(&self, key: &str) -> Option<Option<Reg>> {
        self.bindings
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| match v {
                BindingValue::RegOpt(r) => Some(*r),
                _ => None,
            })
    }

    /// 绑定数量
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

/// Reg matcher — 匹配任意 Reg 或绑定为具名变量
#[derive(Debug, Clone, PartialEq)]
pub enum RegMatcher {
    /// 匹配任意 Reg（不绑定）
    Any,
    /// 匹配并绑定为 name
    Bind(String),
}

/// Option<Reg> matcher — 匹配 Some/None
#[derive(Debug, Clone, PartialEq)]
pub enum RegOptMatcher {
    /// 匹配 Some(any reg)
    Some(RegMatcher),
    /// 匹配 None
    None,
}

/// Value matcher — 匹配特定值或通配
#[derive(Debug, Clone, PartialEq)]
pub enum ValueMatcher {
    /// 匹配任意值（不绑定）
    Any,
    /// 匹配并绑定为 name
    Bind(String),
    /// 匹配特定值（精确比较）
    Exact(Value),
}

/// Label matcher — 匹配任意 Label 或绑定
#[derive(Debug, Clone, PartialEq)]
pub enum LabelMatcher {
    /// 匹配任意 Label（不绑定）
    Any,
    /// 匹配并绑定为 name
    Bind(String),
}

/// BinaryOp matcher — 匹配特定操作符或通配
#[derive(Debug, Clone, PartialEq)]
pub enum OpMatcher {
    /// 匹配任意操作符（不绑定）
    Any,
    /// 匹配并绑定为 name
    Bind(String),
    /// 匹配特定操作符（精确比较）
    Exact(BinaryOp),
}

/// Call name matcher — 匹配调用目标名称
#[derive(Debug, Clone, PartialEq)]
pub enum CallNameMatcher {
    /// 匹配任意函数名（不绑定）
    Any,
    /// 匹配并绑定为 name
    Bind(String),
    /// 匹配特定函数名（精确比较）
    Exact(String),
}

/// Regs matcher — 匹配寄存器列表（用于 Call args）
#[derive(Debug, Clone, PartialEq)]
pub enum RegsMatcher {
    /// 匹配任意参数列表
    Any,
}

/// Pattern matching trait
pub trait Match {
    /// 尝试匹配一条 MIR 指令，返回 Some(bindings) 或 None
    fn matches(&self, inst: &MirInst) -> Option<MatchBindings>;
}

impl Match for MirPattern {
    fn matches(&self, inst: &MirInst) -> Option<MatchBindings> {
        match (self, inst) {
            // Any: 匹配任意指令（用于 body-pass 规则如 DeadAfterReturn）
            (MirPattern::Any, _) => Some(MatchBindings::new()),
            (
                MirPattern::ConstBool { dst: dst_m, value },
                MirInst::Const(dst, Value::Bool(v)),
            ) if *v == *value => {
                let mut b = MatchBindings::new();
                if let Some(name) = dst_m.match_and_bind(*dst) {
                    b.insert(name, BindingValue::Reg(*dst));
                }
                Some(b)
            }
            (
                MirPattern::Const {
                    dst: dst_m,
                    value: value_m,
                },
                MirInst::Const(dst, value),
            ) => {
                let mut b = MatchBindings::new();
                if let Some(name) = dst_m.match_and_bind(*dst) {
                    b.insert(name, BindingValue::Reg(*dst));
                }
                if let Some(name) = value_m.match_and_bind(value) {
                    b.insert(name, BindingValue::Value(value.clone()));
                }
                Some(b)
            }
            (
                MirPattern::BinaryOp {
                    dst: dst_m,
                    op: op_m,
                    lhs: lhs_m,
                    rhs: rhs_m,
                },
                MirInst::BinaryOp(dst, lhs, op, rhs),
            ) => {
                let mut b = MatchBindings::new();
                if let Some(name) = dst_m.match_and_bind(*dst) {
                    b.insert(name, BindingValue::Reg(*dst));
                }
                if let Some(name) = lhs_m.match_and_bind(*lhs) {
                    b.insert(name, BindingValue::Reg(*lhs));
                }
                if let Some(name) = op_m.match_and_bind(op) {
                    b.insert(name, BindingValue::Op(op.clone()));
                }
                if let Some(name) = rhs_m.match_and_bind(*rhs) {
                    b.insert(name, BindingValue::Reg(*rhs));
                }
                Some(b)
            }
            (MirPattern::Jump { target: t_m }, MirInst::Jump(target)) => {
                let mut b = MatchBindings::new();
                if let Some(name) = t_m.match_and_bind(*target) {
                    b.insert(name, BindingValue::Label(*target));
                }
                Some(b)
            }
            (MirPattern::JumpIf { cond: c_m, target: t_m }, MirInst::JumpIf(cond, target)) => {
                let mut b = MatchBindings::new();
                if let Some(name) = c_m.match_and_bind(*cond) {
                    b.insert(name, BindingValue::Reg(*cond));
                }
                if let Some(name) = t_m.match_and_bind(*target) {
                    b.insert(name, BindingValue::Label(*target));
                }
                Some(b)
            }
            (MirPattern::JumpIfNot { cond: c_m, target: t_m }, MirInst::JumpIfNot(cond, target)) => {
                let mut b = MatchBindings::new();
                if let Some(name) = c_m.match_and_bind(*cond) {
                    b.insert(name, BindingValue::Reg(*cond));
                }
                if let Some(name) = t_m.match_and_bind(*target) {
                    b.insert(name, BindingValue::Label(*target));
                }
                Some(b)
            }
            (
                MirPattern::Return { value: v_m },
                MirInst::Return(value),
            ) => {
                let mut b = MatchBindings::new();
                if let Some(name) = v_m.match_and_bind(*value) {
                    b.insert(name, BindingValue::RegOpt(*value));
                }
                Some(b)
            }
            (
                MirPattern::Call {
                    dst: dst_m,
                    name: name_m,
                    args: _args_m,
                },
                MirInst::Call(dst, name, _args),
            ) => {
                let mut b = MatchBindings::new();
                if let Some(n) = dst_m.match_and_bind(*dst) {
                    b.insert(n, BindingValue::Reg(*dst));
                }
                if let Some(n) = name_m.match_and_bind(name) {
                    b.insert(n, BindingValue::Value(Value::String(name.clone())));
                }
                Some(b)
            }
            (MirPattern::Label { target: t_m }, MirInst::Label(target)) => {
                let mut b = MatchBindings::new();
                if let Some(name) = t_m.match_and_bind(*target) {
                    b.insert(name, BindingValue::Label(*target));
                }
                Some(b)
            }
            _ => None,
        }
    }
}

impl RegMatcher {
    /// 匹配 Reg 并可选绑定。返回 Some(name) 表示绑定为命名变量，None 表示通配
    fn match_and_bind(&self, _reg: Reg) -> Option<String> {
        match self {
            RegMatcher::Any => None,
            RegMatcher::Bind(name) => Some(name.clone()),
        }
    }
}

impl RegOptMatcher {
    fn match_and_bind(&self, value: Option<Reg>) -> Option<String> {
        match (self, value) {
            (RegOptMatcher::None, None) => None,
            (RegOptMatcher::Some(m), Some(r)) => m.match_and_bind(r),
            _ => None, // 类型不匹配
        }
    }
}

impl ValueMatcher {
    fn match_and_bind(&self, value: &Value) -> Option<String> {
        match self {
            ValueMatcher::Any => None,
            ValueMatcher::Bind(name) => Some(name.clone()),
            ValueMatcher::Exact(v) if v == value => None,
            ValueMatcher::Exact(_) => None, // 不匹配，但为 Some(bindings) 的字段
        }
    }
}

impl LabelMatcher {
    fn match_and_bind(&self, _label: Label) -> Option<String> {
        match self {
            LabelMatcher::Any => None,
            LabelMatcher::Bind(name) => Some(name.clone()),
        }
    }
}

impl OpMatcher {
    fn match_and_bind(&self, op: &BinaryOp) -> Option<String> {
        match self {
            OpMatcher::Any => None,
            OpMatcher::Bind(name) => Some(name.clone()),
            OpMatcher::Exact(o) if o == op => None,
            OpMatcher::Exact(_) => None,
        }
    }
}

impl CallNameMatcher {
    fn match_and_bind(&self, name: &str) -> Option<String> {
        match self {
            CallNameMatcher::Any => None,
            CallNameMatcher::Bind(n) => Some(n.clone()),
            CallNameMatcher::Exact(n) if n == name => None,
            CallNameMatcher::Exact(_) => None,
        }
    }
}

impl RegsMatcher {
    #[allow(unused)]
    fn match_and_bind(&self, _args: &[Reg]) -> Option<String> {
        match self {
            RegsMatcher::Any => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;

    fn inst_const_int(dst: Reg, n: i64) -> MirInst {
        MirInst::Const(dst, Value::Int(n))
    }

    fn inst_binaryop(dst: Reg, lhs: Reg, op: BinaryOp, rhs: Reg) -> MirInst {
        MirInst::BinaryOp(dst, lhs, op, rhs)
    }

    #[test]
    fn match_const_int_wildcard() {
        let pat = MirPattern::Const {
            dst: RegMatcher::Any,
            value: ValueMatcher::Any,
        };
        let inst = inst_const_int(7, 42);
        assert!(pat.matches(&inst).is_some());
    }

    #[test]
    fn match_const_int_bind() {
        let pat = MirPattern::Const {
            dst: RegMatcher::Bind("d".into()),
            value: ValueMatcher::Bind("v".into()),
        };
        let inst = inst_const_int(7, 42);
        let b = pat.matches(&inst).unwrap();
        assert_eq!(b.get_reg("d"), Some(7));
        assert_eq!(b.get_value("v"), Some(&Value::Int(42)));
    }

    #[test]
    fn match_binaryop_bind_all() {
        let pat = MirPattern::BinaryOp {
            dst: RegMatcher::Bind("d".into()),
            op: OpMatcher::Bind("op".into()),
            lhs: RegMatcher::Bind("a".into()),
            rhs: RegMatcher::Bind("b".into()),
        };
        let inst = inst_binaryop(0, 1, BinaryOp::Add, 2);
        let b = pat.matches(&inst).unwrap();
        assert_eq!(b.get_reg("d"), Some(0));
        assert_eq!(b.get_reg("a"), Some(1));
        assert_eq!(b.get_reg("b"), Some(2));
        assert_eq!(b.get_op("op"), Some(&BinaryOp::Add));
        assert_eq!(b.len(), 4);
    }

    #[test]
    fn no_match_different_variant() {
        let pat = MirPattern::Const {
            dst: RegMatcher::Any,
            value: ValueMatcher::Any,
        };
        let inst = MirInst::Jump(0);
        assert!(pat.matches(&inst).is_none());
    }

    #[test]
    fn match_return_with_some() {
        let pat = MirPattern::Return {
            value: RegOptMatcher::Some(RegMatcher::Bind("r".into())),
        };
        let inst = MirInst::Return(Some(5));
        let b = pat.matches(&inst).unwrap();
        assert_eq!(b.get_reg_opt("r"), Some(Some(5)));
    }
}
