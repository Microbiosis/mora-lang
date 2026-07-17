//! SSA 优化 pass（α.3）
//!
//! 基于 SSA 形式的优化：常量传播 (CP)、死代码消除 (DCE)、全局值编号 (GVN)。
//! 未来扩展：LICM、PRE、内联（α.5）。
//!
//! 约束：C2 手写 / I5 可回退（MORA_OPT=0 跳过）

use std::collections::{HashMap, HashSet};

use crate::common::BinaryOp;
use crate::mir::ssa::{MirSsaFunction, SsaInst, SsaReg, Terminator};
use crate::value::Value;

/// 对 MIR-plain 函数执行优化 pass
///
/// level == None → 跳过（直接跑 MIR-plain）
/// level == Basic → SSA 构造 + CP + DCE + GVN
/// level == Aggressive → +LICM + PRE + inline
pub fn optimize(func: &mut crate::mir::MirFunction, level: crate::mir::ssa::OptLevel) {
    if !level.enabled() {
        return;
    }

    // SSA 构造
    let mut ssa = crate::mir::ssa::construct(func);

    // 基础优化
    if level.enabled() {
        const_propagate(&mut ssa);
        dead_code_elim(&mut ssa);
        global_value_numbering(&mut ssa);
    }

    // 激进优化（α.5 实现）
    if level.aggressive() {
        // loop_invariant_motion(&mut ssa);
        // partial_redundancy_elim(&mut ssa);
        // inline_small_tasks(&mut ssa);
    }

    // Deconstruct: SSA → MIR-plain
    *func = crate::mir::ssa::deconstruct(&ssa);
}

// ── helpers ──

fn ssa_dst(inst: &SsaInst) -> SsaReg {
    match inst {
        SsaInst::Const(d, _)
        | SsaInst::Var(d, _)
        | SsaInst::BinaryOp(d, _, _, _)
        | SsaInst::Call(d, _, _)
        | SsaInst::ListLit(d, _)
        | SsaInst::DictLit(d, _)
        | SsaInst::Index(d, _, _)
        | SsaInst::IndexAssign(d, _, _)
        | SsaInst::MethodCall(d, _, _, _)
        | SsaInst::Pipe(d, _, _)
        | SsaInst::Prompt(d, _)
        | SsaInst::Copy(d, _)
        | SsaInst::Define(_, d)
        | SsaInst::Assign(_, d)
        | SsaInst::Expr(d) => *d,
    }
}

// ── 常量传播 (CP) ──

/// SSA 常量传播：
/// - Const 的值记录到 const_vals 表
/// - BinaryOp 两边都是常量时折叠（常量折叠），把 BinaryOp 改写为 Const
/// - Copy/Assign/Expr 中源是常量时，目标也标记为常量
fn const_propagate(ssa: &mut MirSsaFunction) {
    for block in &mut ssa.blocks {
        // 收集本块所有 dst reg 的最大值
        let mut max_dst = 0;
        for inst in &block.insts {
            let d = ssa_dst(inst);
            if d > max_dst {
                max_dst = d;
            }
        }
        // 也检查 phi dst 和 terminator
        for phi in &block.phis {
            if phi.dst > max_dst {
                max_dst = phi.dst;
            }
        }
        if let Terminator::Return(Some(r)) = &block.terminator
            && *r > max_dst {
                max_dst = *r;
            }
        let mut const_vals: Vec<Option<Value>> = vec![None; max_dst.saturating_add(1)];

        for inst in &mut block.insts {
            let dst = ssa_dst(inst);
            if dst >= const_vals.len() {
                continue;
            }

            // Const → 记录常量值
            if let SsaInst::Const(_, v) = inst {
                const_vals[dst] = Some(v.clone());
                continue;
            }

            // Copy(src) → 如果 src 是常量，dst 也是
            if let SsaInst::Copy(_, src) = inst {
                if let Some(ref v) = safe_get(&const_vals, *src) {
                    const_vals[dst] = Some(v.clone());
                }
                continue;
            }

            // BinaryOp 两边常量 → 折叠为 Const
            if let SsaInst::BinaryOp(_, l, op, r) = inst
                && let (Some(ref lv), Some(ref rv)) =
                    (safe_get(&const_vals, *l), safe_get(&const_vals, *r))
            {
                let result = crate::flow::eval_binary(lv.clone(), op, rv.clone());
                if let Ok(v) = result {
                    const_vals[dst] = Some(v.clone());
                    // 直接改写为 Const
                    *inst = SsaInst::Const(dst, v);
                    continue;
                }
            }

            // 其他指令：如果 src 是常量，dst 也标记
            let src_opt = match inst {
                SsaInst::Define(_, s) | SsaInst::Assign(_, s) | SsaInst::Expr(s) => Some(s),
                SsaInst::BinaryOp(_, _, _, _) => None, // 已处理
                _ => None,
            };
            if let Some(src) = src_opt
                && let Some(ref v) = safe_get(&const_vals, *src) {
                    const_vals[dst] = Some(v.clone());
                }
        }
    }
}

fn safe_get<T: Clone>(vals: &[Option<T>], idx: usize) -> Option<T> {
    vals.get(idx).and_then(|v| v.clone())
}

// ── 死代码消除 (DCE) ──

/// SSA-DCE：删除没有被任何后续指令/phi/terminator 使用的定义
fn dead_code_elim(ssa: &mut MirSsaFunction) {
    for block in &mut ssa.blocks {
        let mut used: HashSet<SsaReg> = HashSet::new();

        // terminator 中使用的 reg
        match &block.terminator {
            Terminator::JumpIf(c, _, _) | Terminator::JumpIfNot(c, _, _) => {
                used.insert(*c);
            }
            Terminator::Return(Some(r)) => {
                used.insert(*r);
            }
            _ => {}
        }

        // phi incoming 中使用的 reg
        for phi in &block.phis {
            for (_, src) in &phi.incoming {
                used.insert(*src);
            }
        }

        // 指令中使用的 reg
        for inst in &block.insts {
            match inst {
                SsaInst::BinaryOp(_, l, _, r) => {
                    used.insert(*l);
                    used.insert(*r);
                }
                SsaInst::Call(_, _, args) => {
                    for a in args {
                        used.insert(*a);
                    }
                }
                SsaInst::ListLit(_, items) => {
                    for r in items {
                        used.insert(*r);
                    }
                }
                SsaInst::DictLit(_, pairs) => {
                    for (_, v) in pairs {
                        used.insert(*v);
                    }
                }
                SsaInst::Index(_, obj, idx) => {
                    used.insert(*obj);
                    used.insert(*idx);
                }
                SsaInst::IndexAssign(_, obj, idx) => {
                    used.insert(*obj);
                    used.insert(*idx);
                }
                SsaInst::MethodCall(_, recv, _, args) => {
                    used.insert(*recv);
                    for a in args {
                        used.insert(*a);
                    }
                }
                SsaInst::Pipe(_, lhs, rhs) => {
                    used.insert(*lhs);
                    used.insert(*rhs);
                }
                SsaInst::Prompt(_, parts) => {
                    for r in parts {
                        used.insert(*r);
                    }
                }
                SsaInst::Copy(_, src) => {
                    used.insert(*src);
                }
                SsaInst::Define(_, src) => {
                    used.insert(*src);
                }
                SsaInst::Assign(_, src) => {
                    used.insert(*src);
                }
                SsaInst::Expr(src) => {
                    used.insert(*src);
                }
                SsaInst::Const(_, _) | SsaInst::Var(_, _) => {}
            }
        }

        // 收集可删除的指令索引（保留副作用）
        let mut remove = Vec::new();
        for (i, inst) in block.insts.iter().enumerate() {
            let dst = ssa_dst(inst);
            let is_side_effect = matches!(
                inst,
                SsaInst::Define(_, _)
                    | SsaInst::Assign(_, _)
                    | SsaInst::Expr(_)
                    | SsaInst::Call(_, _, _)
                    | SsaInst::MethodCall(_, _, _, _)
                    | SsaInst::Prompt(_, _)
            );
            if !used.contains(&dst) && !is_side_effect {
                remove.push(i);
            }
        }

        for i in remove.iter().rev() {
            block.insts.remove(*i);
        }
    }
}

// ── 全局值编号 (GVN) ──

/// 简化版 GVN：块内相同 BinaryOp(相同操作数) 合并为同一 reg
fn global_value_numbering(ssa: &mut MirSsaFunction) {
    for block in &mut ssa.blocks {
        let mut seen: HashMap<(String, SsaReg, SsaReg), SsaReg> = HashMap::new();
        let mut replace_map: HashMap<SsaReg, SsaReg> = HashMap::new();

        // 第一遍：找重复表达式
        for inst in &block.insts {
            if let SsaInst::BinaryOp(dst, l, op, r) = inst {
                let op_str = binary_op_to_string(op);
                let key = (op_str, *l, *r);
                if let Some(&existing) = seen.get(&key) {
                    replace_map.insert(*dst, existing);
                } else {
                    seen.insert(key, *dst);
                }
            }
        }

        // 第二遍：替换操作数
        for inst in &mut block.insts {
            apply_replacement(inst, &replace_map);
        }
    }
}

fn binary_op_to_string(op: &BinaryOp) -> String {
    match op {
        BinaryOp::Add => "Add".to_string(),
        BinaryOp::Sub => "Sub".to_string(),
        BinaryOp::Mul => "Mul".to_string(),
        BinaryOp::Div => "Div".to_string(),
        BinaryOp::Mod => "Mod".to_string(),
        BinaryOp::Equal => "Equal".to_string(),
        BinaryOp::NotEqual => "NotEqual".to_string(),
        BinaryOp::Greater => "Greater".to_string(),
        BinaryOp::Less => "Less".to_string(),
        BinaryOp::GreaterEqual => "GreaterEqual".to_string(),
        BinaryOp::LessEqual => "LessEqual".to_string(),
    }
}

fn apply_replacement(inst: &mut SsaInst, map: &HashMap<SsaReg, SsaReg>) {
    fn replace(reg: &mut SsaReg, map: &HashMap<SsaReg, SsaReg>) {
        if let Some(&new) = map.get(reg) {
            *reg = new;
        }
    }

    match inst {
        SsaInst::BinaryOp(_, l, _, r) => {
            replace(l, map);
            replace(r, map);
        }
        SsaInst::Call(_, _, args) => {
            for a in args.iter_mut() {
                replace(a, map);
            }
        }
        SsaInst::ListLit(_, items) => {
            for r in items.iter_mut() {
                replace(r, map);
            }
        }
        SsaInst::DictLit(_, pairs) => {
            for (_, v) in pairs.iter_mut() {
                replace(v, map);
            }
        }
        SsaInst::Index(_, obj, idx) => {
            replace(obj, map);
            replace(idx, map);
        }
        SsaInst::IndexAssign(_, obj, idx) => {
            replace(obj, map);
            replace(idx, map);
        }
        SsaInst::MethodCall(_, recv, _, args) => {
            replace(recv, map);
            for a in args.iter_mut() {
                replace(a, map);
            }
        }
        SsaInst::Pipe(_, lhs, rhs) => {
            replace(lhs, map);
            replace(rhs, map);
        }
        SsaInst::Prompt(_, parts) => {
            for r in parts.iter_mut() {
                replace(r, map);
            }
        }
        SsaInst::Copy(_, src) => replace(src, map),
        SsaInst::Define(_, src) => replace(src, map),
        SsaInst::Assign(_, src) => replace(src, map),
        SsaInst::Expr(src) => replace(src, map),
        SsaInst::Const(_, _) | SsaInst::Var(_, _) => {}
    }
}
