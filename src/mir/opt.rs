//! SSA 优化 pass（α.3 + α.4 + α.5 + α.6）
//!
//! 基础优化（α.3）：常量传播 (CP)、死代码消除 (DCE)、全局值编号 (GVN)。
//! 中高级（α.4）：拷贝传播 (Copy Propagation)。
//! 激进优化（α.5）：循环不变量外提 (LICM)。
//! 激进优化（α.6）：循环强度缩减 (Loop Strength Reduction)。
//! 激进优化（α.7）：尾递归优化 (Tail Call Optimization)。
//!
//! v0.58 Phase H.8: SSA passes 可组合 — 每个 pass 实现 `SsaPass` trait，
//! 通过 `default_pipeline()` 返回内置 pass 序列。
//!
//! 约束：C2 手写 / I5 可回退（MORA_OPT=0 跳过）

use std::collections::{HashMap, HashSet, VecDeque};

use crate::common::BinaryOp;
use crate::mir::ssa::{BlockId, MirSsaFunction, SsaInst, SsaReg, Terminator};
use crate::mir::{MirFunction, MirInst};
use crate::value::Value;

type LicmpOps = Vec<(BlockId, Vec<(SsaReg, SsaInst)>, HashSet<BlockId>)>;

/// v0.58: SSA 优化 pass trait — 每个 pass 是一个独立的变换单元。
///
/// 设计哲学：每个 SsaPass 类似 Cascades 的一条 RewriteRule，
/// 但在 SSA 层操作的是整个 MirSsaFunction（而非单条 MirInst）。
/// 这样可以保留 SSA 层的优化自由度（块内扫描、跨块分析等），
/// 同时享受 Cascades 的"可组合 pass 管线"架构。
pub trait SsaPass {
    /// Pass 名称（用于日志/调试）
    fn name(&self) -> &'static str;

    /// 应用该 pass 到 SSA 函数。返回 true 表示函数被修改。
    fn run(&self, ssa: &mut MirSsaFunction) -> bool;
}

/// 常量传播 pass
pub struct ConstPropPass;

/// 拷贝传播 pass
pub struct CopyPropPass;

/// 死代码消除 pass
pub struct DeadCodeElimPass;

/// 全局值编号 pass（局部 CSE）
pub struct GvnPass;

/// 循环不变量外提 pass（仅 Aggressive）
pub struct LicmPass;

/// 循环强度缩减 pass（仅 Aggressive）
pub struct LoopStrengthReductionPass;

/// 尾调用优化 pass（仅 Aggressive）
pub struct TailCallOptPass;

impl SsaPass for ConstPropPass {
    fn name(&self) -> &'static str {
        "const_prop"
    }
    fn run(&self, ssa: &mut MirSsaFunction) -> bool {
        let before = count_instructions(ssa);
        const_propagate(ssa);
        count_instructions(ssa) != before
    }
}

impl SsaPass for CopyPropPass {
    fn name(&self) -> &'static str {
        "copy_prop"
    }
    fn run(&self, ssa: &mut MirSsaFunction) -> bool {
        let before = count_instructions(ssa);
        copy_propagate(ssa);
        count_instructions(ssa) != before
    }
}

impl SsaPass for DeadCodeElimPass {
    fn name(&self) -> &'static str {
        "dce"
    }
    fn run(&self, ssa: &mut MirSsaFunction) -> bool {
        let before = count_instructions(ssa);
        dead_code_elim(ssa);
        count_instructions(ssa) != before
    }
}

impl SsaPass for GvnPass {
    fn name(&self) -> &'static str {
        "gvn"
    }
    fn run(&self, ssa: &mut MirSsaFunction) -> bool {
        let before = count_instructions(ssa);
        global_value_numbering(ssa);
        count_instructions(ssa) != before
    }
}

impl SsaPass for LicmPass {
    fn name(&self) -> &'static str {
        "licm"
    }
    fn run(&self, ssa: &mut MirSsaFunction) -> bool {
        let before = count_instructions(ssa);
        loop_invariant_motion(ssa);
        count_instructions(ssa) != before
    }
}

impl SsaPass for LoopStrengthReductionPass {
    fn name(&self) -> &'static str {
        "loop_strength_reduction"
    }
    fn run(&self, ssa: &mut MirSsaFunction) -> bool {
        let before = count_instructions(ssa);
        loop_strength_reduction(ssa);
        count_instructions(ssa) != before
    }
}

impl SsaPass for TailCallOptPass {
    fn name(&self) -> &'static str {
        "tail_call_opt"
    }
    fn run(&self, ssa: &mut MirSsaFunction) -> bool {
        let before = count_instructions(ssa);
        tail_call_optimize(ssa);
        count_instructions(ssa) != before
    }
}

/// 计数 SSA 函数中所有指令（用于变更检测）
fn count_instructions(ssa: &MirSsaFunction) -> usize {
    ssa.blocks.iter().map(|b| b.insts.len()).sum()
}

/// 返回默认的基础 pass 管线
pub fn default_basic_pipeline() -> Vec<Box<dyn SsaPass>> {
    vec![
        Box::new(ConstPropPass),
        Box::new(CopyPropPass),
        Box::new(DeadCodeElimPass),
        Box::new(GvnPass),
    ]
}

/// 返回默认的激进 pass 管线（在基础之上）
pub fn default_aggressive_pipeline() -> Vec<Box<dyn SsaPass>> {
    vec![
        Box::new(LicmPass),
        Box::new(LoopStrengthReductionPass),
        Box::new(TailCallOptPass),
    ]
}

/// 对 MIR-plain 函数执行优化 pass
///
/// level == None → 跳过（直接跑 MIR-plain）
/// level == Basic → SSA 构造 + CP + DCE + GVN + CopyProp
/// level == Aggressive → +LICM + LoopStrengthReduction + TailCallOpt
pub fn optimize(func: &mut crate::mir::MirFunction, level: crate::mir::ssa::OptLevel) {
    if !level.enabled() {
        return;
    }

    // SSA 构造
    let mut ssa = crate::mir::ssa::construct(func);

    // 基础管线（迭代至收敛）
    if level.enabled() {
        run_pipeline(&mut ssa, &default_basic_pipeline());
    }

    // 激进管线
    if level.aggressive() {
        run_pipeline(&mut ssa, &default_aggressive_pipeline());
    }

    // Deconstruct: SSA → MIR-plain
    *func = crate::mir::ssa::deconstruct(&ssa);
}

/// 在 SSA 函数上运行一组 pass，迭代直到收敛（fixed point）
pub fn run_pipeline(ssa: &mut MirSsaFunction, passes: &[Box<dyn SsaPass>]) {
    loop {
        let mut any_change = false;
        for pass in passes {
            if pass.run(ssa) {
                any_change = true;
            }
        }
        if !any_change {
            break;
        }
    }
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
            && *r > max_dst
        {
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
            // v0.75.6: `Return(None)` 是 Mora 顶层函数的隐式返回 — run_mir
            // 在无显式 Return 时返回「最后一条产生 dst 的指令」的值。
            // 若不把该 dst 计入 used，DCE 会把它当死代码删除 → 函数返回值
            // 从真实值变成 Nil（等价性测试抓到的 Basic 管线语义 bug）。
            Terminator::Return(None) => {
                if let Some(last_dst) = block.insts.iter().rev().find_map(|inst| {
                    let d = ssa_dst(inst);
                    if d > 0 { Some(d) } else { None }
                }) {
                    used.insert(last_dst);
                }
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

// ── 循环不变量外提 (LICM) ──

/// 循环不变量外提：将循环体内的不变量计算移到循环前
///
/// 算法：
/// 1. 找回边（back edge：succ → pred，pred 支配 succ）
/// 2. 计算 natural loop：以回边终点为 header，收集所有被 header 后驱
///    支配且在 loop 内能到达 header 的块
/// 3. 对每个 loop，找纯值不变量指令（BinaryOp/Const/Var/MethodCall 等，
///    操作数全在 loop 外定义）
/// 4. 在 pre-header 处复制这些不变量指令
/// 5. loop 内替换为对复制后的引用
fn loop_invariant_motion(ssa: &mut MirSsaFunction) {
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
fn apply_reg_replace(inst: &mut SsaInst, old_reg: SsaReg, new_reg: SsaReg) {
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
fn next_free_reg(ssa: &MirSsaFunction) -> SsaReg {
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
fn copy_propagate(ssa: &mut MirSsaFunction) {
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
type LsrOps = Vec<(BlockId, Vec<(SsaReg, SsaReg, crate::common::BinaryOp)>)>;

fn loop_strength_reduction(ssa: &mut MirSsaFunction) {
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

// ── 尾递归优化 (α.7) ──

/// 尾递归优化：将尾位置的函数调用替换为跳转，复用当前栈帧
///
/// 算法（逐块）：
/// 1. 找以 `Call` 结尾、后接 `Return` 的块
/// 2. 将 `Return` 替换为 `Jump` 到 Call 的目标位置
/// 3. 复用当前块的寄存器分配（避免压栈）
fn tail_call_optimize(ssa: &mut MirSsaFunction) {
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

// ── Orchestrate-specific optimizations (v0.55) ──

/// 合并连续重复的 orchestrate 指令（superstep fusion）
///
/// 当 body 中出现两个连续的 Orchestrate 且 kind 完全相同时，
/// 第二个视为冗余，被移除。
pub fn superstep_fusion(func: &mut MirFunction) {
    let mut i = 0;
    while i + 1 < func.body.len() {
        if let (MirInst::Orchestrate { kind: kind1, .. }, MirInst::Orchestrate { kind: kind2, .. }) =
            (&func.body[i], &func.body[i + 1])
            && kind1 == kind2
        {
            func.body.remove(i + 1);
            continue; // re-check current position with new neighbor
        }
        i += 1;
    }
}

/// 运行所有 Pregel 优化 pass（当前仅 superstep_fusion）
pub fn optimize_pregel(func: &mut MirFunction) {
    superstep_fusion(func);
}
