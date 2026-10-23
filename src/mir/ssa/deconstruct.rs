//! v0.75.58: SSA deconstruct — phi → copy 指令，SSA → MIR-plain（α.3）。
//! 自 ssa.rs 拆出（D6 单文件惯例）。construct（基本块划分/支配树/phi 插入/
//! 重命名）仍在 ssa.rs；本文件只负责从 SSA 形式回落到线性 MIR。

use std::collections::{HashMap, HashSet};

use super::{BlockId, MirSsaFunction, Phi, SsaInst, SsaReg, Terminator};

use crate::mir::{MirFunction, MirInst};

use crate::value::Value;

/// 一条 `SsaInst` 读到的全部 SSA 寄存器（含不透明指令的输入）。
fn collect_ssa_reads(inst: &SsaInst, out: &mut HashSet<SsaReg>) {
    match inst {
        SsaInst::Const(_, _) | SsaInst::Var(_, _) => {}
        SsaInst::BinaryOp(_, l, _, r) => {
            out.insert(*l);
            out.insert(*r);
        }
        SsaInst::Call(_, _, args) => out.extend(args),
        SsaInst::CallValue(_, callee, args) => {
            out.insert(*callee);
            out.extend(args);
        }
        SsaInst::ListLit(_, items) => out.extend(items),
        SsaInst::DictLit(_, pairs) => out.extend(pairs.iter().map(|(_, v)| *v)),
        SsaInst::Index(_, obj, idx) => {
            out.insert(*obj);
            out.insert(*idx);
        }
        SsaInst::IndexAssign(_, obj, idx) => {
            out.insert(*obj);
            out.insert(*idx);
        }
        SsaInst::MethodCall(_, recv, _, args) => {
            out.insert(*recv);
            out.extend(args);
        }
        SsaInst::Pipe(_, lhs, rhs) => {
            out.insert(*lhs);
            out.insert(*rhs);
        }
        SsaInst::Prompt(_, parts) => out.extend(parts),
        SsaInst::Copy(_, src) | SsaInst::Define(_, src) | SsaInst::Assign(_, src) => {
            out.insert(*src);
        }
        SsaInst::Expr(src) => {
            out.insert(*src);
        }
        SsaInst::Opaque(m) => out.extend(m.input_regs()),
    }
}

/// 「跳到函数末尾」的跳转目标哨兵 —— 第四遍拼接完 body 后补成 `body.len()`。
///
/// 与 `MirInst::Label(usize::MAX)`（跳过哨兵）**不是同一个变体**，不会撞。
const EXIT_JUMP: usize = usize::MAX;

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
                SsaInst::CallValue(d, callee, args) => {
                    all_ssa_regs.insert(*d);
                    all_ssa_regs.insert(*callee);
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
                // v0.104.6 D417：不透明指令的寄存器与普通指令同处一个
                // SSA 空间（construct 已把它纳入重命名），故照常收集。
                SsaInst::Opaque(m) => {
                    for r in m.input_regs() {
                        all_ssa_regs.insert(r);
                    }
                    if let Some(w) = m.written_reg() {
                        all_ssa_regs.insert(w);
                    }
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

    // v0.104.6 D418：phi 的前驱 copy 有两道前置条件，缺一不可。
    //
    // ① **结果真被读到**。循环变量走的是 **env**
    //    （`Var(dst,"total")` / `Assign("total", src)`），而 `SsaInst::Var`
    //    **不读寄存器** ⇒ 循环头为 `total` / `i` 建的 phi 没有任何消费者。
    //    可它们的 `incoming` 仍会被发成 copy，于是前驱块去读「只在循环体里
    //    定义过」的 SSA 号 —— 那个寄存器**没有生产者** ⇒ DAG 执行器按
    //    「输入寄存器就绪」激活节点，该节点永不激活 ⇒ 整条链饿死 ⇒
    //    `--opt=1` **静默无输出、退出码 0**（实测 `loop_break.mora`）。
    //
    // ② **源寄存器有生产者**。`rename_variables` 记录 incoming 时，若该前驱
    //    路径上这个变量**根本没有定义**，`resolve_reg` 会回落到原始编号 ——
    //    那是一个谁都不写的 SSA 号。这类边同样不能发 copy。
    let produced: HashSet<SsaReg> = {
        let mut s: HashSet<SsaReg> = HashSet::new();
        for block in &ssa.blocks {
            for phi in &block.phis {
                s.insert(phi.dst);
            }
            for inst in &block.insts {
                if let SsaInst::Define(_, _) | SsaInst::Assign(_, _) = inst {
                    continue;
                }
                if let Some(d) = crate::mir::ssa::ssa_dst(inst) {
                    s.insert(d);
                }
            }
        }
        s
    };

    let mut phi_result_used: HashSet<SsaReg> = HashSet::new();
    for block in &ssa.blocks {
        for inst in &block.insts {
            collect_ssa_reads(inst, &mut phi_result_used);
        }
        match &block.terminator {
            Terminator::JumpIf(c, _, _) | Terminator::JumpIfNot(c, _, _) => {
                phi_result_used.insert(*c);
            }
            Terminator::Return(Some(r)) => {
                phi_result_used.insert(*r);
            }
            _ => {}
        }
    }

    // 收集 phi 的前驱 copy 信息
    // pred_id → Vec<(dst_p, src_ssa_reg)>
    let mut pred_copies: HashMap<BlockId, Vec<(usize, SsaReg)>> = HashMap::new();
    // 无有效 incoming 的 phi：没有任何 copy 会写它 ⇒ 目标寄存器**没有生产者**。
    // 默认值放在**块首**（`Label` 之后）而不是「每条无效边各放一次」：无效边
    // 来自一条没有定义的路径（走不到），而块首这一个常量对所有入口都成立；
    // 有有效 incoming 的 phi **不放**默认值 —— 它们的值由前驱尾部的 copy 写入，
    // 块首再写一次会把它盖掉（WAR）。
    let mut phi_defaults: HashMap<BlockId, Vec<usize>> = HashMap::new();

    // **必须物化**的 phi：结果被读到，或被别人（传递地）当 incoming 源引用。
    // 只看「直接被读到」会漏掉中间环节 —— 实测（`loop_for_break.mora`）：
    // 循环头为「列表」建的 phi 本身没人读，下游块的 phi 却从它取值，
    // 于是那条 `Copy(_, <没人写的寄存器>)` 让 DAG 节点永不激活 ⇒
    // `--opt=1` 静默无输出、退出码 0。
    let phi_dst_of: HashMap<SsaReg, (BlockId, &Phi)> = ssa
        .blocks
        .iter()
        .flat_map(|b| b.phis.iter().map(move |p| (p.dst, (b.id, p))))
        .collect();
    let mut wanted: HashSet<SsaReg> = phi_result_used.clone();
    let mut frontier: Vec<SsaReg> = wanted.iter().copied().collect();
    while let Some(d) = frontier.pop() {
        let Some((_, phi)) = phi_dst_of.get(&d) else {
            continue;
        };
        for (_, src) in &phi.incoming {
            if produced.contains(src) && wanted.insert(*src) {
                frontier.push(*src);
            }
        }
    }

    for block in &ssa.blocks {
        for phi in &block.phis {
            if !wanted.contains(&phi.dst) {
                continue;
            }
            let dst_p = ssa_to_plain[&phi.dst];
            let mut has_source = false;
            for (pred_id, src_ssa) in &phi.incoming {
                if !produced.contains(src_ssa) {
                    continue;
                }
                has_source = true;
                pred_copies
                    .entry(*pred_id)
                    .or_default()
                    .push((dst_p, *src_ssa));
            }
            if !has_source {
                phi_defaults.entry(block.id).or_default().push(dst_p);
            }
        }
    }

    // 生成指令辅助函数
    fn ssa_inst_to_plain(inst: &SsaInst, ssa_to_plain: &HashMap<SsaReg, usize>) -> Vec<MirInst> {
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
            SsaInst::CallValue(dst, callee, args) => {
                out.push(MirInst::CallValue(
                    map_reg(*dst),
                    map_reg(*callee),
                    args.iter().map(|a| map_reg(*a)).collect(),
                ));
            }
            SsaInst::Prompt(dst, parts) => {
                out.push(MirInst::Prompt(
                    map_reg(*dst),
                    parts.iter().map(|p| map_reg(*p)).collect(),
                ));
            }
            SsaInst::Copy(dst, src) => {
                // v0.104.6 D420：直接寄存器拷贝。
                //
                // 此前经 env 中转：`Assign(".tmp_N", src)` + `Var(dst, ".tmp_N")`。
                // **那条路径是静默失效的** —— `Environment::assign` 对**尚未
                // 绑定**的名字返回 false 且**什么都不写**（`value.rs:1007`），
                // `.tmp_N` 从未被 define ⇒ 写入被丢弃 ⇒ `Var` 读回 Nil。
                //
                // 实测症状（D418 删掉 phi 门后暴露）：`let x = if … end` 在
                // `--opt=1` 下 x 变成 **nil**。凡经过 SSA 的 `Copy`
                // （`if` 的汇合值、phi copy）都会静默丢值；不经 SSA 的程序
                // 里 `.tmp_N` 恰好也读不到，但那里的值本来就是 Nil，
                // 于是缺陷被 env 往返本身掩盖了。
                out.push(MirInst::Copy(map_reg(*dst), map_reg(*src)));
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
            // v0.104.6 D417：**原位发射**。此前这类指令被 `construct` 抽进
            // `MirSsaFunction::passthrough`、在此处由 body 头部 `chain` 前插
            // —— 顺序被打乱、寄存器仍是重命名前的旧号（D310/D304 的共同根因）。
            // 现在它们在块内指令流里就位，顺序与寄存器都已由 SSA 保证。
            SsaInst::Opaque(m) => {
                let mapped = m.map_regs(&mut |r| map_reg(r));
                out.push(match mapped.written_reg() {
                    Some(w) => mapped.with_written_reg(map_reg(w)),
                    None => mapped,
                });
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
            // execute 隐式返回「最后产生 dst 的指令」，发射 `Return(None)`
            // 会在块首就短路（顶层块 Label 后第一条指令即 Return(None)），
            // 使隐式返回载体永远无法执行 → 返回值变成 Nil（SSA 等价性
            // 测试抓到的语义 bug）。丢弃后线性执行自然落到最后一条指令。
            Terminator::Return(None) => MirInst::Label(usize::MAX), // skipped below
            Terminator::Return(Some(r)) => MirInst::Return(Some(map_reg(*r))),
            Terminator::Break(t) if *t >= num_blocks => MirInst::Jump(EXIT_JUMP),
            Terminator::Break(t) => MirInst::Break(*t),
            Terminator::Continue(t) if *t >= num_blocks => MirInst::Jump(EXIT_JUMP),
            Terminator::Continue(t) => MirInst::Continue(*t),
            // v0.104.6 D419：跳到函数末尾。**不能**降成 `Return(None)` ——
            // 那会被上面「丢弃」分支吃掉，这条跳转就彻底消失，后续块会被
            // 线性执行 ⇒ 两个分支都跑（副作用重复，D313）。
            //
            // 也不能降成 `Return(None)`（它返回 Nil，丢掉隐式返回值）。发一条
            // 真跳转、由第四遍补成 `body.len()`：`execute` 见 `pc >= body.len()`
            // 即结束并走隐式返回，`dag_analyze` 则解析不到目标 → 该节点无出边
            // —— 两个执行器都与 opt=off 一致。
            Terminator::JumpExit => MirInst::Jump(EXIT_JUMP),
            Terminator::Unreachable => MirInst::Label(usize::MAX), // skipped below
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
            block_insts.extend(ssa_inst_to_plain(inst, &ssa_to_plain));
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
            // 直接寄存器拷贝。不用 `Assign(tmp, src) + Var(dst, tmp)` 的 env
            // 中转：循环归纳变量每轮都要过一次 env 往返，任何一次不保真的
            // 值（宿主对象 / 闭包 / 大整数）都会在**静默**的情况下改变语义。
            target.insert(term_idx, MirInst::Copy(*dst_p, src_p));
        }
    }

    // 第三遍之二：给无 incoming 的 phi 落默认值
    for (block_id, dsts) in &phi_defaults {
        let Some(target) = blocks_body.get_mut(*block_id) else {
            continue;
        };
        // 插在 Label 之后、其余指令之前（phi 的值先于本块的一切被读）
        let at = target
            .iter()
            .position(|i| matches!(i, MirInst::Label(_)))
            .map(|p| p + 1)
            .unwrap_or(0);
        for dst_p in dsts.iter().rev() {
            target.insert(at, MirInst::Const(*dst_p, Value::Nil));
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
    //
    // v0.104.6 D419：`EXIT_JUMP`（跳到函数末尾）补成**重建后 body 的长度** ——
    // 两个执行器都据此结束：`execute` 的 `pc >= body.len()`，以及
    // `dag_analyze` 解析不到目标节点（该 Jump 节点没有出边）。
    let exit_target = body.len();
    for inst in body.iter_mut() {
        let new_target = |old: usize| -> usize {
            if old == EXIT_JUMP {
                exit_target
            } else {
                label_positions2.get(&old).copied().unwrap_or(old)
            }
        };
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
        body,
        // v0.104.6 D417：`next_plain_reg` 就是**精确**帧大小 ——
        // `map_ssa` 为每一个出现过的 SSA 寄存器分配了 `0..next_plain_reg`
        // 的 plain 号，不透明指令的寄存器同样经这条路径。
        // 此前取 `max(next_plain_reg, orig_n_regs)`（D304）只是为了给
        // 「原样回插、仍是重命名前旧号」的透传指令兜住越界 ——
        // 那是**掩盖失同步**，不是正确的上界。
        n_regs: next_plain_reg,
        ..Default::default()
    }
}
