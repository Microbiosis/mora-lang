//! v0.75.60: 自 opt.rs 按 pass 组拆出（D6 单文件惯例）。
//! ConstProp / DeadCodeElim / Gvn 三个 pass 实现 + 共享辅助。

use std::collections::{HashMap, HashSet};

use super::*;

use crate::common::BinaryOp;

use crate::value::Value;

/// 指令定义的 SSA 寄存器；**不写任何寄存器**时为 `None`（见
/// [`crate::mir::ssa::ssa_dst`]）。
pub(super) fn ssa_dst(inst: &SsaInst) -> Option<SsaReg> {
    crate::mir::ssa::ssa_dst(inst)
}

// ── 常量传播 (CP) ──

/// SSA 常量传播：
/// - Const 的值记录到 const_vals 表
/// - BinaryOp 两边都是常量时折叠（常量折叠），把 BinaryOp 改写为 Const
/// - Copy/Assign/Expr 中源是常量时，目标也标记为常量
pub(super) fn const_propagate(ssa: &mut MirSsaFunction) {
    for block in &mut ssa.blocks {
        // 收集本块所有 dst reg 的最大值
        let mut max_dst = 0;
        for inst in &block.insts {
            if let Some(d) = ssa_dst(inst)
                && d > max_dst
            {
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
            && *r > max_dst
        {
            max_dst = *r;
        }
        let mut const_vals: Vec<Option<Value>> = vec![None; max_dst.saturating_add(1)];

        for inst in &mut block.insts {
            let Some(dst) = ssa_dst(inst) else {
                continue;
            };
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
                // 不透明指令的读不在这里解析（`map_regs` 拥有寄存器知识），
                // 且它可能是 effectful（`Perform` / `Handle` / `Import` …）——
                // **不得**因为「dst 看起来没被读」就传播常量值。
                _ => None,
            };
            if let Some(src) = src_opt
                && let Some(ref v) = safe_get(&const_vals, *src)
            {
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
pub(super) fn dead_code_elim(ssa: &mut MirSsaFunction) {
    // v0.104.6 D418：phi 的 `incoming` 是**跨块**引用 —— 循环体里的
    // `__idx += 1` 只被「循环头那条 phi」读，而那条 phi 在**另一个块**里。
    // 局部 `used` 看不见它 ⇒ 该定义在它自己的块里被判成死代码删掉 ⇒
    // 回边 copy 去读一个已被删除的寄存器 ⇒ 循环变量永不递增。
    //
    // 实测（`loop_for_break.mora`，`--opt=1` 静默无输出）：`BinaryOp(idx,…)`
    // 被删，回边只剩 `Copy(phi_dst, <从未定义的寄存器>)`。
    //
    // 正确做法是 phi 源寄存器**全函数**入账 —— 它本来就是跨块的。
    let mut cross_block_used: HashSet<SsaReg> = HashSet::new();
    for block in &ssa.blocks {
        for phi in &block.phis {
            for (_, src) in &phi.incoming {
                cross_block_used.insert(*src);
            }
        }
    }

    for block in &mut ssa.blocks {
        let mut used: HashSet<SsaReg> = cross_block_used.clone();

        // terminator 中使用的 reg
        match &block.terminator {
            Terminator::JumpIf(c, _, _) | Terminator::JumpIfNot(c, _, _) => {
                used.insert(*c);
            }
            Terminator::Return(Some(r)) => {
                used.insert(*r);
            }
            // v0.75.6: `Return(None)` 是 Mora 顶层函数的隐式返回 — run_mir
            // 在无显式 Return 时返回「最后一条产生 dst 的指令」的值。
            // 若不把该 dst 计入 used，DCE 会把它当死代码删除 → 函数返回值
            // 从真实值变成 Nil（等价性测试抓到的 Basic 管线语义 bug）。
            //
            // v0.104.6 D419：`JumpExit`（跳到函数末尾）**同样要走隐式返回** ——
            // 它降维成一条真跳转，落地时 `pc >= body.len()`，返回的仍是「最后
            // 产生 dst 的指令」。漏掉它 ⇒ `let x = if … end` 在 `--opt=1` 下
            // 变成 Nil（实测）。
            //
            // v0.104.6 **D474**：原实现带一个 `.filter(|&d| d > 0)` ——
            // 它把**寄存器 0**排除在「隐式返回的值」之外。而 reg 0 恰恰是
            // 「程序只有一条语句、值就在第一条指令里」的情形：
            //
            // ```text
            // 1        →  MIR = [Const(0, 1.0)]   →  reg 0 被 DCE 删掉
            // ```
            //
            // ⇒ `--opt=1` / `--opt=2` 下这类程序的返回值从真实值变成 **Nil**
            // （实测 `Float(1.0)` → `Nil`；`let x = 1 / x` 则正常，因为它的
            // 值落在 reg 2）。
            //
            // 该过滤没有任何依据：`ssa_dst` 返回的就是「该指令写的寄存器」，
            // reg 0 与其它寄存器**没有区别**（`SsaReg` 空间从 0 起编号）。
            Terminator::Return(None) | Terminator::JumpExit => {
                if let Some(last_dst) = block.insts.iter().rev().find_map(ssa_dst) {
                    used.insert(last_dst);
                }
            }
            _ => {}
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
                SsaInst::CallValue(_, callee, args) => {
                    used.insert(*callee);
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
                // v0.104.6 D417：不透明指令的读由 `MirInst::input_regs`
                // 拥有，SSA 层无从枚举 —— 保守地把它们全部当「被使用」。
                SsaInst::Opaque(m) => {
                    for r in m.input_regs() {
                        used.insert(r);
                    }
                }
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
                    // v0.104.6 D417：不透明指令**一律保留**。SSA 不为其
                    // 建模语义，无法证明它无副作用 —— `Perform` / `Handle` /
                    // `Send` / `Import` / `Transaction` … 删除它们等于把副作用
                    // 静默删掉（本项目头号失败模式）。宁可少删，不可错删。
                    | SsaInst::Opaque(_)
            );
            if !is_side_effect && dst.is_some_and(|d| !used.contains(&d)) {
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
pub(super) fn global_value_numbering(ssa: &mut MirSsaFunction) {
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
        SsaInst::CallValue(_, callee, args) => {
            replace(callee, map);
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
        // GVN 把被合并的 `dst` 记进 `replace_map`；不透明指令若继续引用旧号，
        // 读到的就是**那份已被合并掉的定义**。
        SsaInst::Opaque(m) => *m = m.map_regs(&mut |r| map.get(&r).copied().unwrap_or(r)),
        SsaInst::Const(_, _) | SsaInst::Var(_, _) => {}
    }
}
