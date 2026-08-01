//! v0.58: SSA Pattern 平行框架（Phase H.3）
//!
//! 简化版 SsaPattern + SsaRewriteRule 演示：
//! - 模式匹配 `SsaInst::Const`（单条匹配，演示框架）
//! - SSA 规则不通过 `apply_rules` 批量调用，而是嵌入到 SSA pass 中
//!
//! 完整 SSA 优化框架（如 CP/DCE/GVN）需要 dataflow 分析，
//! 见 `mir/opt.rs::const_propagate` 等现有实现。

use crate::common::BinaryOp;
use crate::mir::ssa::SsaInst;
use crate::mir::ssa::SsaReg;
use crate::value::Value;

/// SsaReg matcher — 匹配任意 SsaReg 或绑定为具名变量
#[derive(Debug, Clone, PartialEq)]
pub enum SsaRegMatcher {
    /// 匹配任意 SsaReg（不绑定）
    Any,
    /// 匹配并绑定为 name
    Bind(String),
}

/// SsaPattern 枚举（Phase H.3 演示版）
///
/// v0.58 仅支持最常用的几条指令。完整覆盖与 MirPattern 平行。
#[derive(Debug, Clone, PartialEq)]
pub enum SsaPattern {
    /// `SsaInst::Const(dst, value)`
    Const {
        dst: SsaRegMatcher,
        value: ValueMatcher,
    },
    /// `SsaInst::BinaryOp(dst, lhs, op, rhs)`
    BinaryOp {
        dst: SsaRegMatcher,
        op: OpMatcher,
        lhs: SsaRegMatcher,
        rhs: SsaRegMatcher,
    },
}

/// Value matcher（SSA 版，与 MirPattern 共享语义）
#[derive(Debug, Clone, PartialEq)]
pub enum ValueMatcher {
    Any,
    Bind(String),
    Exact(Value),
}

/// BinaryOp matcher（SSA 版）
#[derive(Debug, Clone, PartialEq)]
pub enum OpMatcher {
    Any,
    Bind(String),
    Exact(BinaryOp),
}

/// SSA Pattern 匹配结果 — 变量绑定
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SsaBindings {
    bindings: Vec<(String, SsaBindingValue)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SsaBindingValue {
    Reg(SsaReg),
    Value(Value),
    Op(BinaryOp),
}

impl SsaBindings {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: impl Into<String>, value: SsaBindingValue) {
        self.bindings.push((key.into(), value));
    }

    pub fn get_reg(&self, key: &str) -> Option<SsaReg> {
        self.bindings
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| match v {
                SsaBindingValue::Reg(r) => Some(*r),
                _ => None,
            })
    }

    pub fn get_value(&self, key: &str) -> Option<&Value> {
        self.bindings
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| match v {
                SsaBindingValue::Value(v) => Some(v),
                _ => None,
            })
    }

    pub fn get_op(&self, key: &str) -> Option<&BinaryOp> {
        self.bindings
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| match v {
                SsaBindingValue::Op(o) => Some(o),
                _ => None,
            })
    }
}

/// SsaPattern 匹配 trait
pub trait SsaMatch {
    fn matches(&self, inst: &SsaInst) -> Option<SsaBindings>;
}

impl SsaMatch for SsaPattern {
    fn matches(&self, inst: &SsaInst) -> Option<SsaBindings> {
        match (self, inst) {
            (
                SsaPattern::Const {
                    dst: dst_m,
                    value: value_m,
                },
                SsaInst::Const(dst, value),
            ) => {
                let mut b = SsaBindings::new();
                if let Some(name) = dst_m.match_and_bind(*dst) {
                    b.insert(name, SsaBindingValue::Reg(*dst));
                }
                if let Some(name) = value_m.match_and_bind(value) {
                    b.insert(name, SsaBindingValue::Value(value.clone()));
                }
                Some(b)
            }
            (
                SsaPattern::BinaryOp {
                    dst: dst_m,
                    op: op_m,
                    lhs: lhs_m,
                    rhs: rhs_m,
                },
                SsaInst::BinaryOp(dst, lhs, op, rhs),
            ) => {
                let mut b = SsaBindings::new();
                if let Some(name) = dst_m.match_and_bind(*dst) {
                    b.insert(name, SsaBindingValue::Reg(*dst));
                }
                if let Some(name) = lhs_m.match_and_bind(*lhs) {
                    b.insert(name, SsaBindingValue::Reg(*lhs));
                }
                if let Some(name) = op_m.match_and_bind(op) {
                    b.insert(name, SsaBindingValue::Op(op.clone()));
                }
                if let Some(name) = rhs_m.match_and_bind(*rhs) {
                    b.insert(name, SsaBindingValue::Reg(*rhs));
                }
                Some(b)
            }
            _ => None,
        }
    }
}

impl SsaRegMatcher {
    fn match_and_bind(&self, _reg: SsaReg) -> Option<String> {
        match self {
            SsaRegMatcher::Any => None,
            SsaRegMatcher::Bind(name) => Some(name.clone()),
        }
    }
}

impl ValueMatcher {
    fn match_and_bind(&self, _value: &Value) -> Option<String> {
        match self {
            ValueMatcher::Any => None,
            ValueMatcher::Bind(name) => Some(name.clone()),
            ValueMatcher::Exact(_) => None,
        }
    }
}

impl OpMatcher {
    fn match_and_bind(&self, _op: &BinaryOp) -> Option<String> {
        match self {
            OpMatcher::Any => None,
            OpMatcher::Bind(name) => Some(name.clone()),
            OpMatcher::Exact(_) => None,
        }
    }
}

/// Phase H.3 演示：SSA 常量折叠规则
///
/// 仅匹配「BinaryOp 两边已知常量」→ 折叠为 Const
/// 此规则**不执行 dataflow 分析**——调用方需在外部提供
/// 已知的常量值（`extern_const_values: &HashMap<SsaReg, Value>`）。
pub struct SsaConstFoldingRule<'a> {
    /// 外部 dataflow 状态：reg → 已知常量值
    pub const_values: &'a std::collections::HashMap<SsaReg, Value>,
}

impl<'a> SsaConstFoldingRule<'a> {
    pub fn new(const_values: &'a std::collections::HashMap<SsaReg, Value>) -> Self {
        Self { const_values }
    }

    /// 尝试对单条 SsaInst 应用常量折叠
    ///
    /// 返回 Some(new_inst) 表示应替换为 new_inst；
    /// 返回 None 表示无可应用重写。
    pub fn try_fold(&self, inst: &SsaInst) -> Option<SsaInst> {
        if let SsaInst::BinaryOp(dst, l, op, r) = inst
            && let (Some(lv), Some(rv)) = (self.const_values.get(l), self.const_values.get(r))
            && let Ok(v) = crate::flow::eval_binary(lv.clone(), op, rv.clone())
        {
            return Some(SsaInst::Const(*dst, v));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_const_folding(const_values: &HashMap<SsaReg, Value>) -> SsaConstFoldingRule<'_> {
        SsaConstFoldingRule::new(const_values)
    }

    #[test]
    fn test_ssa_pattern_const_match() {
        let pat = SsaPattern::Const {
            dst: SsaRegMatcher::Bind("d".into()),
            value: ValueMatcher::Bind("v".into()),
        };
        let inst = SsaInst::Const(7, Value::Int(42));
        let b = pat.matches(&inst).unwrap();
        assert_eq!(b.get_reg("d"), Some(7));
        assert_eq!(b.get_value("v"), Some(&Value::Int(42)));
    }

    #[test]
    fn test_ssa_pattern_no_match_different_variant() {
        let pat = SsaPattern::Const {
            dst: SsaRegMatcher::Any,
            value: ValueMatcher::Any,
        };
        let inst = SsaInst::BinaryOp(0, 1, BinaryOp::Add, 2);
        assert!(pat.matches(&inst).is_none());
    }

    #[test]
    fn test_ssa_pattern_binaryop_bind() {
        let pat = SsaPattern::BinaryOp {
            dst: SsaRegMatcher::Bind("d".into()),
            op: OpMatcher::Bind("op".into()),
            lhs: SsaRegMatcher::Bind("a".into()),
            rhs: SsaRegMatcher::Bind("b".into()),
        };
        let inst = SsaInst::BinaryOp(0, 1, BinaryOp::Add, 2);
        let b = pat.matches(&inst).unwrap();
        assert_eq!(b.get_reg("d"), Some(0));
        assert_eq!(b.get_op("op"), Some(&BinaryOp::Add));
    }

    #[test]
    fn test_const_folding_folds_binaryop() {
        let mut consts = HashMap::new();
        consts.insert(1, Value::Int(2));
        consts.insert(2, Value::Int(3));
        let rule = make_const_folding(&consts);

        let inst = SsaInst::BinaryOp(0, 1, BinaryOp::Add, 2);
        let result = rule.try_fold(&inst);
        assert!(matches!(result, Some(SsaInst::Const(0, Value::Int(5)))));
    }

    #[test]
    fn test_const_folding_skips_when_missing_consts() {
        let consts = HashMap::new(); // 空
        let rule = make_const_folding(&consts);

        let inst = SsaInst::BinaryOp(0, 1, BinaryOp::Add, 2);
        assert!(rule.try_fold(&inst).is_none());
    }
}
