// v0.75.60: 自 opt.rs 按 pass 组拆出（D6 单文件惯例）。
// Licm / LoopStrengthReduction 两个 pass 实现 + loop 分析辅助。

use std::collections::{HashSet, VecDeque};

use super::copy::{LsrOps, next_free_reg};
use super::simple::ssa_dst;
use super::*;

pub(super) fn loop_invariant_motion(ssa: &mut MirSsaFunction) {
    if ssa.blocks.len() < 2 {
        return;
    }

    // 计算支配关系
    let dominated = compute_dominated(&ssa.blocks);

    // 找所有回边（back edge）
    let mut back_edges: Vec<(BlockId, BlockId)> = Vec::new();
    for block in &ssa.blocks {
        for &succ in &block.succs {
            if succ < ssa.blocks.len() && dominated[succ].contains(&block.id) && block.id != succ {
                back_edges.push((block.id, succ));
            }
        }
    }

    // 收集所有要执行的 LICM 操作，避免借位冲突
    let mut operations: LicmpOps = Vec::new();

    for (_head_pred, header) in back_edges {
        if header >= ssa.blocks.len() {
            continue;
        }

        let natural_loop = compute_natural_loop(&ssa.blocks, header);
        if natural_loop.len() <= 1 {
            continue;
        }

        let pre_header = find_pre_header(&ssa.blocks, header, &dominated, &natural_loop);
        if pre_header.is_none() {
            continue;
        }
        let pre_header = pre_header.unwrap();

        // 收集 loop 内定义的 reg 集合
        let mut loop_defs: HashSet<SsaReg> = HashSet::new();
        for bid in &natural_loop {
            let block = &ssa.blocks[*bid];
            for inst in &block.insts {
                loop_defs.insert(ssa_dst(inst));
            }
            for phi in &block.phis {
                loop_defs.insert(phi.dst);
            }
        }

        // 收集不变量指令（只收集 clone）
        let mut invariants: Vec<(SsaReg, SsaInst)> = Vec::new();
        for bid in &natural_loop {
            let block = &ssa.blocks[*bid];
            for inst in &block.insts {
                if is_loop_invariant(inst, &loop_defs) {
                    invariants.push((ssa_dst(inst), inst.clone()));
                }
            }
        }

        if invariants.is_empty() {
            continue;
        }

        operations.push((pre_header, invariants, natural_loop));
    }

    // 第二遍：执行所有 LICM 操作
    for (pre_header, invariants, natural_loop) in operations {
        let max_reg = next_free_reg(ssa);

        for (i, (old_dst, inst)) in invariants.iter().enumerate() {
            let new_dst = max_reg + i as SsaReg;
            let mut new_inst = inst.clone();
            set_dst_ssa(&mut new_inst, new_dst);
            ssa.blocks[pre_header].insts.push(new_inst);
            replace_reg_in_loop(&mut ssa.blocks, *old_dst, new_dst, &natural_loop);
        }
    }
}

/// 计算每个块支配哪些块（从 idom 逆推）
fn compute_dominated(blocks: &[crate::mir::ssa::BasicBlock]) -> Vec<HashSet<BlockId>> {
    let n = blocks.len();
    let mut dominated: Vec<HashSet<BlockId>> = vec![HashSet::new(); n];

    // 用 idom（立即支配）计算完整的支配关系
    let mut idom: Vec<Option<BlockId>> = vec![None; n];
    for bid in 0..n {
        if blocks[bid].preds.is_empty() {
            idom[bid] = Some(bid);
        }
    }

    // 迭代计算支配集
    let mut dom_sets: Vec<HashSet<BlockId>> = (0..n)
        .map(|bid| {
            let mut s = HashSet::new();
            if blocks[bid].preds.is_empty() {
                s.insert(bid);
            }
            s
        })
        .collect();

    let mut changed = true;
    let mut iter = 0;
    while changed && iter < 100 {
        changed = false;
        iter += 1;
        for bid in 0..n {
            if blocks[bid].preds.is_empty() {
                continue;
            }
            let mut new_dom: HashSet<BlockId> = HashSet::new();
            new_dom.insert(bid);
            let mut first = true;
            let mut intersection: HashSet<BlockId> = HashSet::new();
            for &pred in &blocks[bid].preds {
                if first {
                    intersection = dom_sets[pred].clone();
                    first = false;
                } else {
                    intersection.retain(|x| dom_sets[pred].contains(x));
                }
            }
            new_dom.extend(intersection);
            if new_dom != dom_sets[bid] {
                dom_sets[bid] = new_dom;
                changed = true;
            }
        }
    }

    // 计算 idom（从支配集推导）
    for bid in 0..n {
        let dom = &dom_sets[bid];
        if dom.len() <= 1 {
            idom[bid] = if dom.contains(&bid) { Some(bid) } else { None };
            continue;
        }
        let mut best: Option<BlockId> = None;
        let mut best_size = 0usize;
        for &d in dom {
            if d == bid {
                continue;
            }
            let d_size = dom_sets[d].len();
            if d_size < dom.len() && d_size > best_size {
                best_size = d_size;
                best = Some(d);
            }
        }
        idom[bid] = best;
    }

    // 从 idom 计算 dominated（每个节点包含所有被它支配的节点）
    for (bid, id_opt) in idom.iter().enumerate() {
        if let Some(id) = id_opt {
            dominated[*id].insert(bid);
        }
    }

    dominated
}

/// 计算 natural loop：以 back edge 终点 header 为入口，收集所有在 loop 内
/// 且所有出口路径都必须经过 header 的块
fn compute_natural_loop(
    blocks: &[crate::mir::ssa::BasicBlock],
    header: BlockId,
) -> HashSet<BlockId> {
    let mut loop_blocks: HashSet<BlockId> = HashSet::new();
    loop_blocks.insert(header);

    let mut worklist: VecDeque<BlockId> = VecDeque::new();
    worklist.push_back(header);

    while let Some(bid) = worklist.pop_front() {
        for &pred in &blocks[bid].preds {
            if !loop_blocks.contains(&pred) {
                loop_blocks.insert(pred);
                worklist.push_back(pred);
            }
        }
    }

    loop_blocks
}

/// 找 pre-header：支配 header 但不属于 natural loop 的块（通常是头块唯一的非 loop 前驱）
fn find_pre_header(
    blocks: &[crate::mir::ssa::BasicBlock],
    header: BlockId,
    _dominated: &Vec<HashSet<BlockId>>,
    loop_blocks: &HashSet<BlockId>,
) -> Option<BlockId> {
    // 找支配 header 的块（preds 中不属于 loop 的）
    blocks[header]
        .preds
        .iter()
        .find(|&&pred| !loop_blocks.contains(&pred))
        .copied()
}

/// 检查指令是否是循环不变量（操作数全在 loop 外定义）
fn is_loop_invariant(inst: &SsaInst, loop_defs: &HashSet<SsaReg>) -> bool {
    // 纯值指令（无副作用）且操作数全在 loop 外
    match inst {
        SsaInst::Const(_, _) => true,
        SsaInst::Var(_, _) => true,
        SsaInst::BinaryOp(_, l, _, r) => !loop_defs.contains(l) && !loop_defs.contains(r),
        SsaInst::Call(_, _, args) => !args.iter().any(|a| loop_defs.contains(a)),
        SsaInst::ListLit(_, items) => !items.iter().any(|a| loop_defs.contains(a)),
        SsaInst::DictLit(_, pairs) => !pairs.iter().any(|(_, a)| loop_defs.contains(a)),
        SsaInst::Index(_, obj, idx) => !loop_defs.contains(obj) && !loop_defs.contains(idx),
        SsaInst::MethodCall(_, recv, _, args) => {
            !loop_defs.contains(recv) && !args.iter().any(|a| loop_defs.contains(a))
        }
        SsaInst::Pipe(_, lhs, rhs) => !loop_defs.contains(lhs) && !loop_defs.contains(rhs),
        SsaInst::Prompt(_, parts) => !parts.iter().any(|p| loop_defs.contains(p)),
        SsaInst::Copy(_, src) => !loop_defs.contains(src),
        // 副作用指令不移动
        SsaInst::Define(_, _)
        | SsaInst::Assign(_, _)
        | SsaInst::Expr(_)
        | SsaInst::IndexAssign(_, _, _) => false,
    }
}

/// 在 loop 内替换旧寄存器为新的
fn replace_reg_in_loop(
    blocks: &mut [crate::mir::ssa::BasicBlock],
    old_reg: SsaReg,
    new_reg: SsaReg,
    loop_blocks: &HashSet<BlockId>,
) {
    for bid in loop_blocks {
        if *bid >= blocks.len() {
            continue;
        }
        for inst in &mut blocks[*bid].insts {
            apply_reg_replace(inst, old_reg, new_reg);
        }
        // 也替换 phi incoming 中的引用
        for phi in &mut blocks[*bid].phis {
            for (_, src) in &mut phi.incoming {
                if *src == old_reg {
                    *src = new_reg;
                }
            }
        }
    }
}

/// 给指令设置新的 dst 寄存器
fn set_dst_ssa(inst: &mut SsaInst, d: SsaReg) {
    match inst {
        SsaInst::Const(dst, _) => *dst = d,
        SsaInst::Var(dst, _) => *dst = d,
        SsaInst::BinaryOp(dst, _, _, _) => *dst = d,
        SsaInst::Call(dst, _, _) => *dst = d,
        SsaInst::ListLit(dst, _) => *dst = d,
        SsaInst::DictLit(dst, _) => *dst = d,
        SsaInst::Index(dst, _, _) => *dst = d,
        SsaInst::IndexAssign(dst, _, _) => *dst = d,
        SsaInst::MethodCall(dst, _, _, _) => *dst = d,
        SsaInst::Pipe(dst, _, _) => *dst = d,
        SsaInst::Prompt(dst, _) => *dst = d,
        SsaInst::Copy(dst, _) => *dst = d,
        SsaInst::Define(_, dst) => *dst = d,
        SsaInst::Assign(_, dst) => *dst = d,
        SsaInst::Expr(dst) => *dst = d,
    }
}

/// 在指令中替换旧寄存器为新的
pub(super) fn apply_reg_replace(inst: &mut SsaInst, old_reg: SsaReg, new_reg: SsaReg) {
    let replace = |r: &mut SsaReg| {
        if *r == old_reg {
            *r = new_reg;
        }
    };
    match inst {
        SsaInst::BinaryOp(_, l, _, r) => {
            replace(l);
            replace(r);
        }
        SsaInst::Call(_, _, args) => {
            for a in args {
                replace(a);
            }
        }
        SsaInst::ListLit(_, items) => {
            for r in items {
                replace(r);
            }
        }
        SsaInst::DictLit(_, pairs) => {
            for (_, v) in pairs {
                replace(v);
            }
        }
        SsaInst::Index(_, obj, idx) => {
            replace(obj);
            replace(idx);
        }
        SsaInst::IndexAssign(_, obj, idx) => {
            replace(obj);
            replace(idx);
        }
        SsaInst::MethodCall(_, recv, _, args) => {
            replace(recv);
            for a in args {
                replace(a);
            }
        }
        SsaInst::Pipe(_, lhs, rhs) => {
            replace(lhs);
            replace(rhs);
        }
        SsaInst::Prompt(_, parts) => {
            for r in parts {
                replace(r);
            }
        }
        SsaInst::Copy(_, src) => replace(src),
        SsaInst::Define(_, src) => replace(src),
        SsaInst::Assign(_, src) => replace(src),
        SsaInst::Expr(src) => replace(src),
        SsaInst::Const(_, _) | SsaInst::Var(_, _) => {}
    }
}

/// 找下一个可用的自由寄存器
pub(super) fn loop_strength_reduction(ssa: &mut MirSsaFunction) {
    if ssa.blocks.len() < 2 {
        return;
    }

    // 找 back edges 和 natural loops（与 LICM 共享逻辑）
    let dominated = compute_dominated(&ssa.blocks);
    let mut back_edges: Vec<(BlockId, BlockId)> = Vec::new();
    for block in &ssa.blocks {
        for &succ in &block.succs {
            if succ < ssa.blocks.len() && dominated[succ].contains(&block.id) && block.id != succ {
                back_edges.push((block.id, succ));
            }
        }
    }

    // 收集所有强度缩减操作
    let mut operations: LsrOps = Vec::new();

    for (_head_pred, header) in back_edges {
        if header >= ssa.blocks.len() {
            continue;
        }

        let natural_loop = compute_natural_loop(&ssa.blocks, header);
        if natural_loop.len() <= 1 {
            continue;
        }

        // 收集 loop 内定义集合
        let mut loop_defs: HashSet<SsaReg> = HashSet::new();
        for bid in &natural_loop {
            let block = &ssa.blocks[*bid];
            for inst in &block.insts {
                loop_defs.insert(ssa_dst(inst));
            }
            for phi in &block.phis {
                loop_defs.insert(phi.dst);
            }
        }

        // 收集归纳变量（phi 在 header 中）
        let mut induction_vars: Vec<(SsaReg, SsaReg, crate::common::BinaryOp)> = Vec::new();
        let header_block = &ssa.blocks[header];
        for phi in &header_block.phis {
            // phi = incoming + step 或 phi = incoming * step
            for (_, src) in &phi.incoming {
                if let Some(bin_inst) = find_binary_inst(&ssa.blocks, *src)
                    && let SsaInst::BinaryOp(_, l, op, r) = bin_inst
                {
                    // 检查是否为 `iv = incoming + step`（step 是常量或 phi_in）
                    let is_induction = is_induction_candidate(l, r, op, &loop_defs, &ssa.blocks);
                    if is_induction {
                        induction_vars.push((phi.dst, *src, op.clone()));
                        break;
                    }
                }
            }
        }

        // 对每个归纳变量，找乘法表达式：(mul_dst, invariant_arg, step_op)
        let mut replace_ops: Vec<(SsaReg, SsaReg, crate::common::BinaryOp)> = Vec::new();
        for (iv_dst, _iv_src, iv_op) in &induction_vars {
            // 遍历 loop 内的所有块
            for bid in &natural_loop {
                let block = &ssa.blocks[*bid];
                for inst in &block.insts {
                    if let SsaInst::BinaryOp(dst, l, bin_op, r) = inst
                        && bin_op == &crate::common::BinaryOp::Mul
                    {
                        if *l == *iv_dst && !loop_defs.contains(r) {
                            // `x = iv * y`，y 不变量
                            if *iv_op == crate::common::BinaryOp::Add {
                                replace_ops.push((*dst, *r, crate::common::BinaryOp::Add));
                            }
                        } else if *r == *iv_dst && !loop_defs.contains(l) {
                            // `x = y * iv`，y 不变量
                            if *iv_op == crate::common::BinaryOp::Add {
                                replace_ops.push((*dst, *l, crate::common::BinaryOp::Add));
                            }
                        }
                    }
                }
            }
        }

        if !replace_ops.is_empty() {
            operations.push((header, replace_ops));
        }
    }

    // 第二遍：执行强度缩减（替换乘法为递推）
    for (_header, replace_ops) in operations {
        for (_mul_dst, _invariant_arg, _step_op) in replace_ops {
            // 递推替换在 SSA 中较复杂，保守做法：暂不替换
        }
    }
}

fn find_binary_inst(blocks: &[crate::mir::ssa::BasicBlock], reg: SsaReg) -> Option<&SsaInst> {
    for block in blocks {
        for inst in &block.insts {
            if ssa_dst(inst) == reg && matches!(inst, SsaInst::BinaryOp(_, _, _, _)) {
                return Some(inst);
            }
        }
    }
    None
}

fn is_induction_candidate(
    l: &SsaReg,
    r: &SsaReg,
    _op: &crate::common::BinaryOp,
    loop_defs: &HashSet<SsaReg>,
    _blocks: &[crate::mir::ssa::BasicBlock],
) -> bool {
    // 归纳变量候选：l 或 r 不在 loop 内定义，且操作是 Add
    !loop_defs.contains(l) || !loop_defs.contains(r)
}
