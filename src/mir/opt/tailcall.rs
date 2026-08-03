//! v0.75.60: 自 opt.rs 按 pass 组拆出（D6 单文件惯例）。
//! TailCallOpt pass 实现。

use crate::mir::ssa::MirSsaFunction;

use super::*;

pub(super) fn tail_call_optimize(ssa: &mut MirSsaFunction) {
    // 收集尾调用位置（先只读，再写入，避免借位冲突）
    let mut tail_calls: Vec<(BlockId, usize)> = Vec::new();

    for block in &ssa.blocks {
        // 找尾部的 Call / MethodCall
        let tail_call = block.insts.iter().enumerate().rev().find(|(_, inst)| {
            matches!(inst, SsaInst::Call(_, _, _))
                || matches!(inst, SsaInst::MethodCall(_, _, _, _))
        });

        if let Some((call_idx, _call_inst)) = tail_call {
            // 检查 Call 之后只有副作用指令（Expr/Define/Assign）
            let is_tail_position = ((call_idx + 1)..block.insts.len()).all(|i| {
                matches!(
                    block.insts[i],
                    SsaInst::Expr(_) | SsaInst::Define(_, _) | SsaInst::Assign(_, _)
                )
            });
            let terminator_is_return = matches!(block.terminator, Terminator::Return(_));

            if is_tail_position && terminator_is_return {
                tail_calls.push((block.id, call_idx));
            }
        }
    }

    // 第二遍：标记尾调用（将 Return 替换为 Unreachable，让 deconstruct 省略多余的返回）
    for (bid, _call_idx) in tail_calls {
        if bid < ssa.blocks.len() {
            let block = &mut ssa.blocks[bid];
            // 确认 Call 仍然在尾位置（避免索引漂移）
            let still_tail = block.insts.iter().enumerate().rev().find(|(_, inst)| {
                matches!(inst, SsaInst::Call(_, _, _))
                    || matches!(inst, SsaInst::MethodCall(_, _, _, _))
            });
            let is_still_tail = if let Some((idx, _)) = still_tail {
                ((idx + 1)..block.insts.len()).all(|i| {
                    matches!(
                        block.insts[i],
                        SsaInst::Expr(_) | SsaInst::Define(_, _) | SsaInst::Assign(_, _)
                    )
                })
            } else {
                false
            };
            if is_still_tail {
                // 尾调用 → 直接返回 Call 结果，无需额外 Return 指令
                block.terminator = Terminator::Unreachable;
            }
        }
    }
}
