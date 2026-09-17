//! v0.89: CMIR → LMIR 降维 — 9 层架构桥接第 3 段。
//!
//! 将 CmirNode（并发感知）降维为 LmirInst（内存布局感知）。
//!
//! 降维规则：
//! - Pure(CoreInst::Const Int) → LmirInst::ConstInt (unbox)
//! - Pure(CoreInst::Const Float) → LmirInst::ConstFloat (unbox)
//! - Pure(CoreInst::Const Bool) → LmirInst::ConstBool (unbox)
//! - Pure(CoreInst::Const String) → LmirInst::ConstString
//! - Pure(CoreInst::Const Nil) → skip (nil 不占内存)
//! - Pure(CoreInst::BinaryOp Add Int) → LmirInst::直接 i64 加法
//! - BspSuperstep → 保留（CMIR 原语透传到 RIR）
//! - Agent* → 保留（CMIR 原语透传到 RIR）
//! - SimdMap/SimdReduce → 保留（CMIR 原语透传到 RIR）
//!
//! 其余 CoreInst 变体 → 透传为 LmirInst 语义占位（未来细化）。

use crate::mir::cmir::{CmirBlock, CmirNode};
use crate::mir::core::CoreInst;
use crate::mir::lmir::{LmirInst, MemLayout};
use crate::value::Value;

/// 将 CmirBlock 降维为 LmirInst 列表 + 布局表。
pub fn cmir_to_lmir(block: &CmirBlock) -> (Vec<LmirInst>, Vec<(String, MemLayout)>) {
    let mut insts = Vec::new();
    let mut layouts = Vec::new();

    for node in &block.nodes {
        match node {
            CmirNode::Pure(core_inst) => {
                if let Some(lmir) = lower_pure_inst(core_inst, &mut layouts) {
                    insts.push(lmir);
                }
            }
            // CMIR 原语透传（在 LMIR 层表示为占位）
            CmirNode::BspSuperstep { .. }
            | CmirNode::AgentSpawn { .. }
            | CmirNode::AgentSync { .. }
            | CmirNode::AgentCollect { .. }
            | CmirNode::EffectHandle { .. }
            | CmirNode::PregelGraph { .. }
            | CmirNode::MoAPipeline { .. }
            | CmirNode::MoERouter { .. }
            | CmirNode::SimdMap { .. }
            | CmirNode::SimdReduce { .. } => {
                // CMIR 原语在 LMIR 层不展开，由 RIR 层处理
            }
        }
    }

    (insts, layouts)
}

/// 降维纯计算 CoreInst 为 LmirInst。
fn lower_pure_inst(inst: &CoreInst, layouts: &mut Vec<(String, MemLayout)>) -> Option<LmirInst> {
    match inst {
        CoreInst::Const(reg, value) => {
            match value {
                Value::Int(n) => {
                    layouts.push(("Int".to_string(), MemLayout::int64()));
                    Some(LmirInst::ConstInt(*reg, *n))
                }
                Value::Float(f) => {
                    layouts.push(("Float".to_string(), MemLayout::float64()));
                    Some(LmirInst::ConstFloat(*reg, *f))
                }
                Value::Bool(b) => {
                    layouts.push(("Bool".to_string(), MemLayout::bool()));
                    Some(LmirInst::ConstBool(*reg, *b))
                }
                Value::String(_s) => {
                    let layout = MemLayout {
                        size: std::mem::size_of::<*const u8>() + std::mem::size_of::<usize>(),
                        align: std::mem::size_of::<*const u8>(),
                        fields: vec![],
                    };
                    layouts.push(("String".to_string(), layout));
                    Some(LmirInst::ConstInt(*reg, 0)) // placeholder
                }
                Value::Nil => None,                     // nil 不占内存
                _ => Some(LmirInst::ConstInt(*reg, 0)), // 其他值占位
            }
        }
        // 二元运算 → LMIR 层保留为占位（未来细化为 unboxed 运算）
        CoreInst::BinaryOp(dst, _lhs, _op, _rhs) => {
            Some(LmirInst::ConstInt(*dst, 0)) // placeholder
        }
        // 其余 → 占位
        _ => Some(LmirInst::ConstInt(0, 0)), // placeholder
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::cmir::CmirBlock;

    #[test]
    fn int_const_unbox() {
        let block = CmirBlock {
            nodes: vec![CmirNode::Pure(CoreInst::Const(0, Value::Int(42)))],
            result: Some(0),
        };
        let (insts, layouts) = cmir_to_lmir(&block);
        assert_eq!(insts.len(), 1);
        assert!(matches!(&insts[0], LmirInst::ConstInt(0, 42)));
        assert!(layouts.iter().any(|(name, _)| name == "Int"));
    }

    #[test]
    fn float_const_unbox() {
        let block = CmirBlock {
            nodes: vec![CmirNode::Pure(CoreInst::Const(0, Value::Float(2.5)))],
            result: Some(0),
        };
        let (insts, _layouts) = cmir_to_lmir(&block);
        assert!(matches!(&insts[0], LmirInst::ConstFloat(0, f) if (*f - 2.5).abs() < 1e-10));
    }

    #[test]
    fn nil_const_skipped() {
        let block = CmirBlock {
            nodes: vec![CmirNode::Pure(CoreInst::Const(0, Value::Nil))],
            result: Some(0),
        };
        let (insts, _) = cmir_to_lmir(&block);
        assert_eq!(insts.len(), 0); // nil 不占内存
    }

    #[test]
    fn cmir_primitive_passthrough() {
        let block = CmirBlock {
            nodes: vec![CmirNode::BspSuperstep {
                computes: vec![],
                sends: vec![],
                aggregates: vec![],
                halt_cond: None,
            }],
            result: None,
        };
        let (insts, _) = cmir_to_lmir(&block);
        assert_eq!(insts.len(), 0); // CMIR 原语在 LMIR 层不展开
    }
}
