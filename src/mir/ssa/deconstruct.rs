//! v0.75.58: SSA deconstruct — phi → copy 指令，SSA → MIR-plain（α.3）。
//! 自 ssa.rs 拆出（D6 单文件惯例）。construct（基本块划分/支配树/phi 插入/
//! 重命名）仍在 ssa.rs；本文件只负责从 SSA 形式回落到线性 MIR。

use std::collections::{HashMap, HashSet};

use crate::mir::{MirFunction, MirInst};

use super::{BlockId, MirSsaFunction, SsaInst, SsaReg, Terminator};

/// 寄存器复制方案：
/// - SSA Copy(dst, src) → MIR: 通过 `Define`+`Var` 或 `Assign`+`Var` 中转
///   实际采用：Assign(name, src) + Var(dst, name) 两步完成 copy
///   为每个 copy 生成一个唯一的临时 env 名称
/// - SSA SsaInst::Const/Var/BinaryOp/... 直接映射到对应 MirInst
/// - Terminator → Jump/JumpIf/JumpIfNot/Return/Break
pub fn deconstruct(ssa: &MirSsaFunction) -> MirFunction {
    let num_blocks = ssa.blocks.len();

    // 建立 ssa_reg → plain_reg 映射
    let mut ssa_to_plain: HashMap<SsaReg, usize> = HashMap::new();
    let mut next_plain_reg = 0;

    // 参数映射：plain reg 0..n 是函数参数
    for (i, (_, ssa_reg)) in ssa.params.iter().enumerate() {
        ssa_to_plain.insert(*ssa_reg, i);
        next_plain_reg = i + 1;
    }

    fn map_ssa(
        ssa_to_plain: &mut HashMap<SsaReg, usize>,
        next: &mut usize,
        ssa_reg: SsaReg,
    ) -> usize {
        *ssa_to_plain.entry(ssa_reg).or_insert_with(|| {
            let r = *next;
            *next += 1;
            r
        })
    }

    // 第一遍：收集所有 SSA 寄存器并映射
    // 遍历所有 block 的所有指令和 phi，建立完整映射
    let mut all_ssa_regs: HashSet<SsaReg> = HashSet::new();

    for (_, ssa_reg) in &ssa.params {
        all_ssa_regs.insert(*ssa_reg);
    }

    for block in &ssa.blocks {
        for phi in &block.phis {
            all_ssa_regs.insert(phi.dst);
            for (_, src) in &phi.incoming {
                all_ssa_regs.insert(*src);
            }
        }
        for inst in &block.insts {
            match inst {
                SsaInst::Const(d, _) | SsaInst::Var(d, _) => {
                    all_ssa_regs.insert(*d);
                }
                SsaInst::BinaryOp(d, l, _, r) => {
                    all_ssa_regs.insert(*d);
                    all_ssa_regs.insert(*l);
                    all_ssa_regs.insert(*r);
                }
                SsaInst::Call(d, _, args) => {
                    all_ssa_regs.insert(*d);
                    for a in args {
                        all_ssa_regs.insert(*a);
                    }
                }
                SsaInst::ListLit(d, items) => {
                    all_ssa_regs.insert(*d);
                    for i in items {
                        all_ssa_regs.insert(*i);
                    }
                }
                SsaInst::DictLit(d, pairs) => {
                    all_ssa_regs.insert(*d);
                    for (_, v) in pairs {
                        all_ssa_regs.insert(*v);
                    }
                }
                SsaInst::Index(d, o, i) => {
                    all_ssa_regs.insert(*d);
                    all_ssa_regs.insert(*o);
                    all_ssa_regs.insert(*i);
                }
                SsaInst::MethodCall(d, r, _, args) => {
                    all_ssa_regs.insert(*d);
                    all_ssa_regs.insert(*r);
                    for a in args {
                        all_ssa_regs.insert(*a);
                    }
                }
                SsaInst::Pipe(d, l, r) => {
                    all_ssa_regs.insert(*d);
                    all_ssa_regs.insert(*l);
                    all_ssa_regs.insert(*r);
                }
                SsaInst::Prompt(d, parts) => {
                    all_ssa_regs.insert(*d);
                    for p in parts {
                        all_ssa_regs.insert(*p);
                    }
                }
                SsaInst::IndexAssign(d, o, i) => {
                    all_ssa_regs.insert(*d);
                    all_ssa_regs.insert(*o);
                    all_ssa_regs.insert(*i);
                }
                SsaInst::Copy(d, s) => {
                    all_ssa_regs.insert(*d);
                    all_ssa_regs.insert(*s);
                }
                SsaInst::Define(_, s) | SsaInst::Assign(_, s) | SsaInst::Expr(s) => {
                    all_ssa_regs.insert(*s);
                }
            }
        }
        match &block.terminator {
            Terminator::JumpIf(c, _, _) | Terminator::JumpIfNot(c, _, _) => {
                all_ssa_regs.insert(*c);
            }
            Terminator::Return(Some(r)) => {
                all_ssa_regs.insert(*r);
            }
            _ => {}
        }
    }

    // 映射所有 SSA 寄存器
    for reg in &all_ssa_regs {
        map_ssa(&mut ssa_to_plain, &mut next_plain_reg, *reg);
    }

    // 收集 phi 的前驱 copy 信息
    // pred_id → Vec<(dst_p, src_ssa_reg)>
    let mut pred_copies: HashMap<BlockId, Vec<(usize, SsaReg)>> = HashMap::new();

    for block in &ssa.blocks {
        for phi in &block.phis {
            let dst_p = ssa_to_plain[&phi.dst];
            for (pred_id, src_ssa) in &phi.incoming {
                pred_copies
                    .entry(*pred_id)
                    .or_default()
                    .push((dst_p, *src_ssa));
            }
        }
    }

    // 临时 env 名计数器（用于 copy 中转）
    let mut tmp_name_counter = 0;
    let mut tmp_names: Vec<String> = Vec::new();
    fn next_tmp_name(counter: &mut usize, names: &mut Vec<String>) -> String {
        let name = format!(".tmp_{}", *counter);
        *counter += 1;
        names.push(name.clone());
        name
    }

    // 生成指令辅助函数
    fn ssa_inst_to_plain(
        inst: &SsaInst,
        ssa_to_plain: &HashMap<SsaReg, usize>,
        _next_plain_reg: &mut usize,
        tmp_name_counter: &mut usize,
        tmp_names: &mut Vec<String>,
    ) -> Vec<MirInst> {
        let mut out = Vec::new();
        let map_reg = |ssa_r: SsaReg| -> usize {
            *ssa_to_plain
                .get(&ssa_r)
                .unwrap_or_else(|| panic!("unmapped SSA reg {}", ssa_r))
        };

        match inst {
            SsaInst::Const(dst, v) => {
                out.push(MirInst::Const(map_reg(*dst), v.clone()));
            }
            SsaInst::Var(dst, name) => {
                out.push(MirInst::Var(map_reg(*dst), name.clone()));
            }
            SsaInst::BinaryOp(dst, l, op, r) => {
                out.push(MirInst::BinaryOp(
                    map_reg(*dst),
                    map_reg(*l),
                    op.clone(),
                    map_reg(*r),
                ));
            }
            SsaInst::Call(dst, callee, args) => {
                out.push(MirInst::Call(
                    map_reg(*dst),
                    callee.clone(),
                    args.iter().map(|a| map_reg(*a)).collect(),
                ));
            }
            SsaInst::ListLit(dst, items) => {
                out.push(MirInst::ListLit(
                    map_reg(*dst),
                    items.iter().map(|r| map_reg(*r)).collect(),
                ));
            }
            SsaInst::DictLit(dst, pairs) => {
                out.push(MirInst::DictLit(
                    map_reg(*dst),
                    pairs
                        .iter()
                        .map(|(k, v)| (k.clone(), map_reg(*v)))
                        .collect(),
                ));
            }
            SsaInst::Index(dst, obj, idx) => {
                out.push(MirInst::Index(map_reg(*dst), map_reg(*obj), map_reg(*idx)));
            }
            SsaInst::IndexAssign(dst, obj, idx) => {
                out.push(MirInst::IndexAssign(
                    map_reg(*dst),
                    map_reg(*obj),
                    map_reg(*idx),
                ));
            }
            SsaInst::MethodCall(dst, recv, method, args) => {
                out.push(MirInst::MethodCall(
                    map_reg(*dst),
                    map_reg(*recv),
                    method.clone(),
                    args.iter().map(|a| map_reg(*a)).collect(),
                ));
            }
            SsaInst::Pipe(dst, lhs, rhs) => {
                out.push(MirInst::Pipe(map_reg(*dst), map_reg(*lhs), map_reg(*rhs)));
            }
            SsaInst::Prompt(dst, parts) => {
                out.push(MirInst::Prompt(
                    map_reg(*dst),
                    parts.iter().map(|p| map_reg(*p)).collect(),
                ));
            }
            SsaInst::Copy(dst, src) => {
                // Copy src reg to dst reg: Assign(copy_var, src) + Var(dst, copy_var)
                let copy_var = next_tmp_name(tmp_name_counter, tmp_names);
                out.push(MirInst::Assign(copy_var.clone(), map_reg(*src)));
                out.push(MirInst::Var(map_reg(*dst), copy_var));
            }
            SsaInst::Define(name, src) => {
                out.push(MirInst::Define(name.clone(), map_reg(*src)));
            }
            SsaInst::Assign(name, src) => {
                out.push(MirInst::Assign(name.clone(), map_reg(*src)));
            }
            SsaInst::Expr(src) => {
                out.push(MirInst::Expr(map_reg(*src)));
            }
        }
        out
    }

    fn terminator_to_plain(
        term: &Terminator,
        ssa_to_plain: &HashMap<SsaReg, usize>,
        num_blocks: BlockId,
    ) -> MirInst {
        let map_reg = |ssa_r: SsaReg| -> usize {
            *ssa_to_plain
                .get(&ssa_r)
                .unwrap_or_else(|| panic!("unmapped SSA reg {}", ssa_r))
        };
        match term {
            Terminator::Jump(t) if *t >= num_blocks => MirInst::Return(None),
            Terminator::Jump(t) => MirInst::Jump(*t),
            // JumpIf(cond, true_t, false_t): cond truthy → true_t, else → false_t
            // If true_t == MAX (past end = exit), invert: MirInst::JumpIfNot(cond, false_t)
            //   meaning: if cond is falsy, jump to false_t (body); if truthy, fall through (exit)
            Terminator::JumpIf(cond, t, f) if *t >= num_blocks => {
                MirInst::JumpIfNot(map_reg(*cond), *f)
            }
            // If false_t == MAX (past end = exit), use: MirInst::JumpIf(cond, true_t)
            //   meaning: if cond is truthy, jump to true_t; if falsy, fall through (exit)
            Terminator::JumpIf(cond, t, _f) => MirInst::JumpIf(map_reg(*cond), *t),
            // JumpIfNot(cond, true_t, false_t): cond truthy → true_t, else → false_t
            // If false_t == MAX (past end = exit), invert: MirInst::JumpIf(cond, true_t)
            //   meaning: if cond is truthy, jump to true_t; if falsy, fall through (exit)
            Terminator::JumpIfNot(cond, t, f) if *f >= num_blocks => {
                MirInst::JumpIf(map_reg(*cond), *t)
            }
            // If true_t == MAX (past end = exit), invert: MirInst::JumpIfNot(cond, false_t)
            //   meaning: if cond is falsy, jump to false_t (body); if truthy, fall through (exit)
            Terminator::JumpIfNot(cond, t, f) if *t >= num_blocks => {
                MirInst::JumpIfNot(map_reg(*cond), *f)
            }
            Terminator::JumpIfNot(cond, _t, f) => MirInst::JumpIfNot(map_reg(*cond), *f),
            // v0.75.6: `Return(None)` 不发射 — Mora 无显式 return 时由
            // run_mir 隐式返回「最后产生 dst 的指令」，发射 `Return(None)`
            // 会在块首就短路（顶层块 Label 后第一条指令即 Return(None)），
            // 使隐式返回载体永远无法执行 → 返回值变成 Nil（SSA 等价性
            // 测试抓到的语义 bug）。丢弃后线性执行自然落到最后一条指令。
            Terminator::Return(None) => MirInst::Label(usize::MAX), // skipped below
            Terminator::Return(Some(r)) => MirInst::Return(Some(map_reg(*r))),
            Terminator::Break(t) if *t >= num_blocks => MirInst::Return(None),
            Terminator::Break(t) => MirInst::Break(*t),
            Terminator::Continue(t) if *t >= num_blocks => MirInst::Return(None),
            Terminator::Continue(t) => MirInst::Continue(*t),
            Terminator::Unreachable => MirInst::Return(None),
        }
    }

    // 第二遍：生成每个块的指令
    let mut blocks_body: Vec<Vec<MirInst>> = Vec::with_capacity(num_blocks);
    let mut label_positions: HashMap<BlockId, usize> = HashMap::new();

    for bid in 0..num_blocks {
        let block = &ssa.blocks[bid];
        let mut block_insts: Vec<MirInst> = Vec::new();

        // 1. Label 占位
        let label_pos = blocks_body.iter().map(|b| b.len()).sum::<usize>() + block_insts.len();
        block_insts.push(MirInst::Label(bid));
        label_positions.insert(bid, label_pos);

        // 2. 纯值指令
        for inst in &block.insts {
            block_insts.extend(ssa_inst_to_plain(
                inst,
                &ssa_to_plain,
                &mut next_plain_reg,
                &mut tmp_name_counter,
                &mut tmp_names,
            ));
        }

        // 3. terminator
        // v0.75.6: `Return(None)` 经 terminator_to_plain 映射为
        // `Label(usize::MAX)` 占位，此处跳过 — 丢弃该指令（线性执行
        // 自然隐式返回最后产生值，与 baseline 语义一致）。
        let term = terminator_to_plain(&block.terminator, &ssa_to_plain, num_blocks);
        if !matches!(term, MirInst::Label(id) if id == usize::MAX) {
            block_insts.push(term);
        }

        blocks_body.push(block_insts);
    }

    // 第三遍：在 pred 块的 terminator 之前插入 phi 的 copy 指令
    for (&pred_id, copies) in &pred_copies {
        if pred_id >= blocks_body.len() {
            continue;
        }
        let target = &mut blocks_body[pred_id];
        if target.is_empty() {
            continue;
        }

        // 找到 terminator 位置（最后一个 Label 之后的第一帧 control 指令）
        let term_idx = if let Some(idx) = target.iter().rposition(|i| {
            matches!(
                i,
                MirInst::Jump(_)
                    | MirInst::JumpIf(_, _)
                    | MirInst::JumpIfNot(_, _)
                    | MirInst::Return(_)
                    | MirInst::Break(_)
                    | MirInst::Continue(_)
            )
        }) {
            idx
        } else {
            target.len()
        };

        // 在 terminator 之前插入 copy 指令（逆序保持顺序）
        for (dst_p, src_ssa) in copies.iter().rev() {
            let src_p = map_ssa(&mut ssa_to_plain, &mut next_plain_reg, *src_ssa);
            // Copy: Assign(copy_var, src) + Var(dst, copy_var)
            let copy_var = next_tmp_name(&mut tmp_name_counter, &mut tmp_names);
            target.insert(term_idx, MirInst::Assign(copy_var.clone(), src_p));
            target.insert(term_idx, MirInst::Var(*dst_p, copy_var));
        }
    }

    // 第四遍：拼接所有块，补丁 Label 和 Jump 目标
    let mut body = Vec::new();
    for block_insts in blocks_body {
        body.extend(block_insts);
    }

    // 重建 label_positions（基于拼接后的 body）
    let mut label_positions2: HashMap<usize, usize> = HashMap::new();
    for (idx, inst) in body.iter().enumerate() {
        if let MirInst::Label(bid) = inst {
            label_positions2.insert(*bid, idx);
        }
    }

    // 补丁所有跳转目标
    for inst in body.iter_mut() {
        let new_target =
            |old: usize| -> usize { label_positions2.get(&old).copied().unwrap_or(old) };
        match inst {
            MirInst::Jump(t) => *t = new_target(*t),
            MirInst::JumpIf(_, t) => *t = new_target(*t),
            MirInst::JumpIfNot(_, t) => *t = new_target(*t),
            MirInst::Break(t) => *t = new_target(*t),
            MirInst::Continue(t) => *t = new_target(*t),
            _ => {}
        }
    }

    MirFunction {
        params: ssa.params.iter().map(|(name, _)| name.clone()).collect(),
        // v0.75.30: 还原声明型指令（TaskDef 等）到 body 头部 — 此前被 SSA
        // 丢弃导致 `--opt` 下 task main 消失。
        body: ssa.passthrough.iter().cloned().chain(body).collect(),
        n_regs: next_plain_reg,
    }
}
