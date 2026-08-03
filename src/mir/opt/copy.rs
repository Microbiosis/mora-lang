//! v0.75.60: 自 opt.rs 按 pass 组拆出（D6 单文件惯例）。
//! CopyProp pass 实现 + next_free_reg / replace_reg_in_terminator 辅助。

use crate::mir::ssa::{MirSsaFunction, SsaInst, SsaReg, Terminator};

use super::loops::apply_reg_replace;
use super::simple::ssa_dst;
use super::*;

pub(super) fn next_free_reg(ssa: &MirSsaFunction) -> SsaReg {
    let mut max_reg = 0;
    for block in &ssa.blocks {
        for inst in &block.insts {
            let d = ssa_dst(inst);
            if d > max_reg {
                max_reg = d;
            }
        }
        for phi in &block.phis {
            if phi.dst > max_reg {
                max_reg = phi.dst;
            }
        }
        if let Terminator::Return(Some(r)) = &block.terminator
            && *r > max_reg
        {
            max_reg = *r;
        }
    }
    max_reg + 1
}

// ── 拷贝传播 (α.4) ──

/// SSA 拷贝传播：将 `r1 = Copy(r2)` 替换为直接引用 r2，并消除拷贝指令
///
/// 算法（逐块）：
/// 1. 扫描每块找 `Copy(dst, src)` 指令
/// 2. 将 dst 之后的所有使用替换为 src
/// 3. 标记该 Copy 为可删除
pub(super) fn copy_propagate(ssa: &mut MirSsaFunction) {
    for block in &mut ssa.blocks {
        // 收集所有拷贝：(dst, src, 指令索引)
        let mut copies: Vec<(SsaReg, SsaReg, usize)> = Vec::new();
        for (idx, inst) in block.insts.iter().enumerate() {
            if let SsaInst::Copy(dst, src) = inst {
                copies.push((*dst, *src, idx));
            }
        }

        // 对每个拷贝，将后续使用替换为源
        for (dst, src, copy_idx) in copies {
            // 替换 dst 在此块中的使用
            block.insts.iter_mut().enumerate().for_each(|(i, inst)| {
                if i > copy_idx {
                    apply_reg_replace(inst, dst, src);
                }
            });

            // 替换 terminator 中的使用
            replace_reg_in_terminator(&mut block.terminator, dst, src);

            // 替换 phi incoming 中的使用
            for phi in &mut block.phis {
                for (_, reg) in &mut phi.incoming {
                    if *reg == dst {
                        *reg = src;
                    }
                }
            }
        }
    }
}

fn replace_reg_in_terminator(terminator: &mut Terminator, old_reg: SsaReg, new_reg: SsaReg) {
    match terminator {
        Terminator::JumpIfNot(reg, _, _) | Terminator::JumpIf(reg, _, _) => {
            if *reg == old_reg {
                *reg = new_reg;
            }
        }
        Terminator::Return(reg_opt) => {
            if let Some(reg) = reg_opt
                && *reg == old_reg
            {
                *reg = new_reg;
            }
        }
        _ => {}
    }
}

// ── 循环强度缩减 (α.6) ──

/// 循环强度缩减：将循环内的乘法/除法替换为加法/减法
///
/// 算法：
/// 1. 找出循环内的归纳变量（如 `iv = iv_in + 1`）
/// 2. 找出依赖于归纳变量的乘法表达式（如 `x = y * iv`）
/// 3. 将 `x = y * iv` 替换为递推：`x' = x + y`（递增）或 `x' = x - y`（递减）
/// 4. 初始化递推变量在循环外
pub(super) type LsrOps = Vec<(BlockId, Vec<(SsaReg, SsaReg, crate::common::BinaryOp)>)>;
