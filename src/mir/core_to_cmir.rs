//! v0.89: Core → CMIR 降维 — 9 层架构桥接第 2 段。
//!
//! 将 CoreInst（SSA 基元）降维为 CmirNode（并发感知）。
//!
//! 降维规则：
//! - 纯计算指令 → CmirNode::Pure(CoreInst)
//! - 效果指令 → CmirNode::Pure(CoreInst)（效果并发由 CMIR 层管理）
//! - 编排指令 → CmirNode::PregelGraph/MoAPipeline/MoERouter
//! - 未来：识别可并行区域，拆分为 BspSuperstep

use crate::mir::cmir::{CmirBlock, CmirNode};
use crate::mir::core::{CoreFunction, CoreInst};

/// 将 CoreFunction 降维为 CmirBlock。
pub fn core_to_cmir(func: &CoreFunction) -> CmirBlock {
    let mut nodes = Vec::new();
    for block in &func.blocks {
        for inst in &block.insts {
            nodes.push(lower_inst(inst));
        }
    }
    CmirBlock {
        nodes,
        result: None,
    }
}

/// 降维单条 CoreInst 为 CmirNode。
fn lower_inst(inst: &CoreInst) -> CmirNode {
    match inst {
        // 纯计算 → Pure
        CoreInst::Const(_, _)
        | CoreInst::Var(_, _)
        | CoreInst::Copy(_, _)
        | CoreInst::BinaryOp(_, _, _, _)
        | CoreInst::Call(_, _, _)
        | CoreInst::ClosureCall(_, _, _)
        | CoreInst::ListLit(_, _)
        | CoreInst::DictLit(_, _)
        | CoreInst::Index(_, _, _)
        | CoreInst::EnvLoad(_, _)
        | CoreInst::EnvStore(_, _)
        | CoreInst::EnvMutate(_, _)
        | CoreInst::EnumConstruct { .. }
        | CoreInst::StructConstruct { .. }
        | CoreInst::StructAccess(_, _, _)
        | CoreInst::ThunkForce(_, _)
        | CoreInst::Expr(_)
        | CoreInst::IndexAssign(_, _, _) => CmirNode::Pure(inst.clone()),

        // 效果 → Pure（CMIR 层管理并发策略）
        CoreInst::EffectPerform { .. }
        | CoreInst::EffectInstall { .. }
        | CoreInst::EffectRestore { .. } => CmirNode::Pure(inst.clone()),

        // 闭包 → Pure
        CoreInst::ClosureCreate { .. } => CmirNode::Pure(inst.clone()),

        // Thunk → Pure
        CoreInst::ThunkCreate { .. } => CmirNode::Pure(inst.clone()),

        // 枚举匹配 → Pure
        CoreInst::EnumMatch { .. } => CmirNode::Pure(inst.clone()),

        // 控制流 → Pure（CMIR 层不改变控制流语义）
        CoreInst::Branch { .. }
        | CoreInst::Jump(_)
        | CoreInst::Return(_)
        | CoreInst::Phi(_, _)
        | CoreInst::Unreachable => CmirNode::Pure(inst.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::core::{CoreBlock, CoreTerminator, EffectLabel};
    use crate::mir::effect::EffectRow;
    use crate::value::Value;

    #[test]
    fn pure_inst_passthrough() {
        let func = CoreFunction {
            params: vec![],
            blocks: vec![CoreBlock {
                id: 0,
                insts: vec![
                    CoreInst::Const(0, Value::Int(42)),
                    CoreInst::BinaryOp(1, 0, crate::common::BinaryOp::Add, 0),
                ],
                terminator: CoreTerminator::Return(Some(1)),
            }],
            entry: 0,
            effects: EffectRow::Empty,
            n_regs: 2,
        };
        let cmir = core_to_cmir(&func);
        assert_eq!(cmir.nodes.len(), 2);
        assert!(matches!(
            &cmir.nodes[0],
            CmirNode::Pure(CoreInst::Const(_, _))
        ));
        assert!(matches!(
            &cmir.nodes[1],
            CmirNode::Pure(CoreInst::BinaryOp(_, _, _, _))
        ));
    }

    #[test]
    fn effect_inst_as_pure() {
        let func = CoreFunction {
            params: vec![],
            blocks: vec![CoreBlock {
                id: 0,
                insts: vec![CoreInst::EffectPerform {
                    dst: 0,
                    label: EffectLabel::Ai,
                    args: vec![],
                }],
                terminator: CoreTerminator::Return(Some(0)),
            }],
            entry: 0,
            effects: EffectRow::Cons("Ai".to_string(), Box::new(EffectRow::Empty)),
            n_regs: 1,
        };
        let cmir = core_to_cmir(&func);
        assert!(matches!(
            &cmir.nodes[0],
            CmirNode::Pure(CoreInst::EffectPerform { .. })
        ));
    }
}
