//! SSA 构造与 deconstruct（α.3）
//!
//! MIR-plain（线性指令列表 + 直接索引跳转） → MIR-ssa（基本块 + phi + 单赋值）
//!
//! 算法（PHASE_ALPHA_IR_DESIGN.md §2.2）：
//! 1. 基本块划分 — Label 入口，terminator 出口
//! 2. 支配树（迭代数据流，CFG 块数少 ~20）
//! 3. 支配边界 + phi 插入
//! 4. 变量重命名（DFS）
//! 5. Deconstruct：phi → copy 指令，SSA → MIR-plain
//!
//! 约束：C2 手写 / I5 可回退（MORA_OPT=0 跳过）

use std::collections::{HashMap, HashSet, VecDeque};

use crate::common::BinaryOp;
use crate::value::Value;

use super::{MirFunction, MirInst};

/// SSA 寄存器（每个定义点分配唯一版本号）
pub type SsaReg = usize;
/// 基本块索引
pub type BlockId = usize;

/// SSA 形式的 MIR 函数
#[derive(Debug, Clone)]
pub struct MirSsaFunction {
    /// (param_name, ssa_reg) 映射
    pub params: Vec<(String, SsaReg)>,
    pub blocks: Vec<BasicBlock>,
    pub entry: BlockId,
    /// α.8: 每个 SSA 寄存器的推断类型（用于 JIT 编译）
    pub types: Vec<RegType>,
}

/// α.8: SSA 寄存器的推断类型
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RegType {
    Void,
    Int,
    Float,
    Bool,
    String,
    List(Box<RegType>), // list<T>
    Dict(Vec<RegType>), // dict: (key_type, val_type)
    Any,                // dyn 类型，不可特化
}

/// 基本块：phi + 纯值指令 + terminator
#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub id: BlockId,
    pub phis: Vec<Phi>,
    pub insts: Vec<SsaInst>,
    pub terminator: Terminator,
    pub preds: Vec<BlockId>,
    pub succs: Vec<BlockId>,
}

/// Phi 节点
#[derive(Debug, Clone)]
pub struct Phi {
    pub dst: SsaReg,
    pub incoming: Vec<(BlockId, SsaReg)>,
}

/// SSA 纯值指令
#[derive(Debug, Clone)]
pub enum SsaInst {
    Const(SsaReg, Value),
    Var(SsaReg, String),
    BinaryOp(SsaReg, SsaReg, BinaryOp, SsaReg),
    Call(SsaReg, String, Vec<SsaReg>),
    ListLit(SsaReg, Vec<SsaReg>),
    DictLit(SsaReg, Vec<(String, SsaReg)>),
    Index(SsaReg, SsaReg, SsaReg),
    IndexAssign(SsaReg, SsaReg, SsaReg),
    MethodCall(SsaReg, SsaReg, String, Vec<SsaReg>),
    Pipe(SsaReg, SsaReg, SsaReg),
    Prompt(SsaReg, Vec<SsaReg>),
    Copy(SsaReg, SsaReg),
    Define(String, SsaReg),
    Assign(String, SsaReg),
    Expr(SsaReg),
}

/// 基本块 terminator
#[derive(Debug, Clone)]
pub enum Terminator {
    Jump(BlockId),
    JumpIf(SsaReg, BlockId, BlockId),
    JumpIfNot(SsaReg, BlockId, BlockId),
    Return(Option<SsaReg>),
    Break(BlockId),
    Continue(BlockId),
    Unreachable,
}

// ── OptLevel ──

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OptLevel {
    None,
    Basic,
    Aggressive,
}

impl OptLevel {
    pub fn from_env() -> OptLevel {
        std::env::var("MORA_OPT")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(|n| match n {
                0 => OptLevel::None,
                1 => OptLevel::Basic,
                _ => OptLevel::Aggressive,
            })
            .unwrap_or(OptLevel::None)
    }

    pub fn enabled(&self) -> bool {
        self != &OptLevel::None
    }

    pub fn aggressive(&self) -> bool {
        self == &OptLevel::Aggressive
    }
}

impl Default for OptLevel {
    fn default() -> Self {
        Self::from_env()
    }
}

// ── construct: MIR-plain → MIR-ssa ──

/// 将 MIR-plain 转为 SSA 形式（含支配树、phi 插入、重命名）
pub fn construct(func: &MirFunction) -> MirSsaFunction {
    if func.body.is_empty() {
        return MirSsaFunction {
            params: func
                .params
                .iter()
                .enumerate()
                .map(|(i, name)| (name.clone(), i))
                .collect(),
            blocks: vec![BasicBlock {
                id: 0,
                phis: Vec::new(),
                insts: Vec::new(),
                terminator: Terminator::Return(None),
                preds: Vec::new(),
                succs: Vec::new(),
            }],
            entry: 0,
            types: Vec::new(),
        };
    }

    let label_to_pos = find_label_targets(&func.body);
    let block_starts = find_block_starts(&func.body, &label_to_pos);
    let bid_to_start = block_starts.clone();
    let start_to_bid: HashMap<usize, BlockId> = bid_to_start
        .iter()
        .enumerate()
        .map(|(bid, &start)| (start, bid))
        .collect();
    let label_to_bid: HashMap<usize, BlockId> = label_to_pos
        .iter()
        .map(|(&label_val, &pos)| (label_val, *start_to_bid.get(&pos).unwrap_or(&0)))
        .collect();

    let body_len = func.body.len();

    let num_blocks = bid_to_start.len();

    let mut blocks: Vec<BasicBlock> = (0..num_blocks)
        .map(|bid| {
            let start = bid_to_start[bid];
            let end = if bid + 1 < num_blocks {
                bid_to_start[bid + 1]
            } else {
                func.body.len()
            };
            let (insts, terminator) = split_into_ssa(
                &func.body[start..end],
                &label_to_bid,
                &start_to_bid,
                bid,
                body_len,
                num_blocks,
            );
            BasicBlock {
                id: bid,
                phis: Vec::new(),
                insts,
                terminator,
                preds: Vec::new(),
                succs: Vec::new(),
            }
        })
        .collect();

    for block in blocks.iter_mut() {
        let mut succs = Vec::new();
        match &block.terminator {
            Terminator::Jump(t) | Terminator::Break(t) | Terminator::Continue(t) => {
                if *t < num_blocks {
                    succs.push(*t);
                }
            }
            Terminator::JumpIf(_, tt, ft) | Terminator::JumpIfNot(_, tt, ft) => {
                if *tt < num_blocks {
                    succs.push(*tt);
                }
                if *ft < num_blocks {
                    succs.push(*ft);
                }
            }
            _ => {}
        }
        block.succs = succs;
    }
    let succs_by_bid: Vec<Vec<BlockId>> = blocks.iter().map(|b| b.succs.clone()).collect();
    for (bid, succs_copy) in succs_by_bid.iter().enumerate() {
        for &succ in succs_copy {
            if succ < num_blocks {
                blocks[succ].preds.push(bid);
            }
        }
    }

    let idom = compute_dominators(&blocks);
    let dom_frontier = compute_dominance_frontier(&blocks, &idom);
    let defs = collect_definitions(&blocks);

    let mut phi_map: HashMap<(BlockId, SsaReg), Phi> = HashMap::new();
    insert_phi_nodes(&blocks, &dom_frontier, &defs, &mut phi_map);
    rename_variables(&mut blocks, &phi_map);

    for block in blocks.iter_mut() {
        let block_phis: Vec<Phi> = phi_map
            .iter()
            .filter(|(key, _)| key.0 == block.id)
            .map(|(_, phi)| phi.clone())
            .collect();
        block.phis = block_phis;
    }

    MirSsaFunction {
        params: func
            .params
            .iter()
            .enumerate()
            .map(|(i, name)| (name.clone(), i))
            .collect(),
        blocks,
        entry: 0,
        types: Vec::new(), // 由 typeinfer 后续填充
    }
}

fn find_label_targets(body: &[MirInst]) -> HashMap<usize, usize> {
    let mut out = HashMap::new();
    for (i, inst) in body.iter().enumerate() {
        if let MirInst::Label(lbl) = inst {
            out.insert(*lbl, i);
        }
    }
    out
}

fn find_block_starts(body: &[MirInst], label_to_pos: &HashMap<usize, usize>) -> Vec<usize> {
    let mut starts = HashSet::new();
    starts.insert(0);

    for &pos in label_to_pos.values() {
        starts.insert(pos);
    }

    let body_len = body.len();

    for (i, inst) in body.iter().enumerate() {
        let is_term = matches!(
            inst,
            MirInst::Return(_)
                | MirInst::Jump(_)
                | MirInst::JumpIf(_, _)
                | MirInst::JumpIfNot(_, _)
                | MirInst::Break(_)
                | MirInst::Continue(_)
        );
        if is_term && i + 1 < body_len {
            starts.insert(i + 1);
        }

        let lbls: Vec<usize> = match inst {
            MirInst::Jump(l)
            | MirInst::JumpIf(_, l)
            | MirInst::JumpIfNot(_, l)
            | MirInst::Break(l)
            | MirInst::Continue(l) => vec![*l],
            _ => vec![],
        };
        for lbl in lbls {
            // α.0 lowering 不 emit Label 指令：Jump/Break/Continue 的 label 即 body 索引
            // 直接将跳转目标位置作为基本块起点插入（注释："α.0 不 emit Label 指令（label 即索引）"）
            if lbl < body_len && lbl > 0 {
                starts.insert(lbl);
            } else if let Some(pos) = label_to_pos.get(&lbl) {
                starts.insert(*pos);
            }
        }
    }

    let mut starts: Vec<usize> = starts.drain().collect();
    starts.sort();
    starts
}

fn split_into_ssa(
    insts: &[MirInst],
    label_to_bid: &HashMap<usize, BlockId>,
    start_to_bid: &HashMap<usize, BlockId>,
    bid: BlockId,
    total_body_len: usize,
    num_blocks: BlockId,
) -> (Vec<SsaInst>, Terminator) {
    let mut ssa_insts = Vec::new();

    fn resolve_jump_target(
        target: usize,
        label_to_bid: &HashMap<usize, BlockId>,
        start_to_bid: &HashMap<usize, BlockId>,
        bid: BlockId,
        _is_jump_if_not: bool,
        total_body_len: usize,
    ) -> BlockId {
        // First try label lookup
        if let Some(&t) = label_to_bid.get(&target) {
            return t;
        }
        // Then try exact position lookup (target is a block start)
        if let Some(&t) = start_to_bid.get(&target) {
            return t;
        }
        // Target past end of body → return (no successor)
        if target >= total_body_len {
            return BlockId::MAX;
        }
        // Find the block that CONTAINS the target position
        if let Some(t) = start_to_bid
            .iter()
            .filter(|&(&p, _)| p <= target)
            .max_by_key(|&(&p, _)| p)
            .map(|(_, &b)| b)
        {
            return t;
        }
        // Fall back to current block
        bid
    }

    for inst in insts {
        match inst {
            MirInst::Return(r) => {
                return (ssa_insts, Terminator::Return(r.map(|r| r as SsaReg)));
            }
            MirInst::Jump(l) => {
                let target =
                    resolve_jump_target(*l, label_to_bid, start_to_bid, bid, false, total_body_len);
                return if target == BlockId::MAX {
                    (ssa_insts, Terminator::Return(None))
                } else {
                    (ssa_insts, Terminator::Jump(target))
                };
            }
            MirInst::JumpIf(cond, l) => {
                let true_t =
                    resolve_jump_target(*l, label_to_bid, start_to_bid, bid, false, total_body_len);
                let fall_through = if bid + 1 < num_blocks {
                    bid + 1
                } else {
                    BlockId::MAX
                };
                return if true_t == BlockId::MAX {
                    (
                        ssa_insts,
                        Terminator::JumpIf(*cond as SsaReg, fall_through, BlockId::MAX),
                    )
                } else {
                    (
                        ssa_insts,
                        Terminator::JumpIf(*cond as SsaReg, true_t, fall_through),
                    )
                };
            }
            MirInst::JumpIfNot(cond, l) => {
                let false_t =
                    resolve_jump_target(*l, label_to_bid, start_to_bid, bid, true, total_body_len);
                let fall_through = if bid + 1 < num_blocks {
                    bid + 1
                } else {
                    BlockId::MAX
                };
                return if false_t == BlockId::MAX {
                    (
                        ssa_insts,
                        Terminator::JumpIfNot(*cond as SsaReg, fall_through, BlockId::MAX),
                    )
                } else {
                    (
                        ssa_insts,
                        Terminator::JumpIfNot(*cond as SsaReg, fall_through, false_t),
                    )
                };
            }
            MirInst::Break(l) => {
                let target =
                    resolve_jump_target(*l, label_to_bid, start_to_bid, bid, false, total_body_len);
                return (ssa_insts, Terminator::Break(target));
            }
            MirInst::Continue(l) => {
                let target =
                    resolve_jump_target(*l, label_to_bid, start_to_bid, bid, false, total_body_len);
                return (ssa_insts, Terminator::Continue(target));
            }
            MirInst::Label(_) => continue,

            MirInst::Const(dst, v) => {
                ssa_insts.push(SsaInst::Const(*dst as SsaReg, v.clone()));
            }
            MirInst::Var(dst, name) => {
                ssa_insts.push(SsaInst::Var(*dst as SsaReg, name.clone()));
            }
            MirInst::BinaryOp(dst, l, op, r) => {
                ssa_insts.push(SsaInst::BinaryOp(
                    *dst as SsaReg,
                    *l as SsaReg,
                    op.clone(),
                    *r as SsaReg,
                ));
            }
            MirInst::Call(dst, callee, args) => {
                ssa_insts.push(SsaInst::Call(
                    *dst as SsaReg,
                    callee.clone(),
                    args.iter().map(|r| *r as SsaReg).collect(),
                ));
            }
            MirInst::ListLit(dst, items) => {
                ssa_insts.push(SsaInst::ListLit(
                    *dst as SsaReg,
                    items.iter().map(|r| *r as SsaReg).collect(),
                ));
            }
            MirInst::DictLit(dst, pairs) => {
                ssa_insts.push(SsaInst::DictLit(
                    *dst as SsaReg,
                    pairs
                        .iter()
                        .map(|(k, v)| (k.clone(), *v as SsaReg))
                        .collect(),
                ));
            }
            MirInst::Index(dst, obj, idx) => {
                ssa_insts.push(SsaInst::Index(
                    *dst as SsaReg,
                    *obj as SsaReg,
                    *idx as SsaReg,
                ));
            }
            MirInst::IndexAssign(dst, obj, idx) => {
                ssa_insts.push(SsaInst::IndexAssign(
                    *dst as SsaReg,
                    *obj as SsaReg,
                    *idx as SsaReg,
                ));
            }
            MirInst::MethodCall(dst, recv, method, args) => {
                ssa_insts.push(SsaInst::MethodCall(
                    *dst as SsaReg,
                    *recv as SsaReg,
                    method.clone(),
                    args.iter().map(|r| *r as SsaReg).collect(),
                ));
            }
            MirInst::Pipe(dst, lhs, rhs) => {
                ssa_insts.push(SsaInst::Pipe(
                    *dst as SsaReg,
                    *lhs as SsaReg,
                    *rhs as SsaReg,
                ));
            }
            MirInst::Prompt(dst, parts) => {
                ssa_insts.push(SsaInst::Prompt(
                    *dst as SsaReg,
                    parts.iter().map(|r| *r as SsaReg).collect(),
                ));
            }
            MirInst::Define(name, reg) => {
                ssa_insts.push(SsaInst::Define(name.clone(), *reg as SsaReg));
            }
            MirInst::Assign(name, reg) => {
                ssa_insts.push(SsaInst::Assign(name.clone(), *reg as SsaReg));
            }
            MirInst::Expr(reg) => {
                ssa_insts.push(SsaInst::Expr(*reg as SsaReg));
            }
            MirInst::TaskDef { .. }
            | MirInst::ToolDef { .. }
            | MirInst::Import(_)
            | MirInst::WithConfig { .. }
            | MirInst::StreamFor { .. }
            | MirInst::MatchExpr { .. }
            | MirInst::MatchArm { .. }
            | MirInst::Closure { .. }
            | MirInst::TypeAlias { .. }
            | MirInst::EnumDef { .. }
            | MirInst::StructDef { .. }
            | MirInst::Transaction { .. }
            | MirInst::Send { .. }
            | MirInst::Receive { .. }
            | MirInst::Rollback
            // α.5: 宏/运行时/可观测性 — SSA 中跳过
            | MirInst::MacroDef { .. }
            | MirInst::Commit
            | MirInst::Worker { .. }
            | MirInst::Route(_)
            | MirInst::Observe { .. }
            | MirInst::Span { .. }
            | MirInst::RecordTokens { .. }
            // α.6: 文件 I/O — SSA 中跳过
            | MirInst::Save { .. }
            | MirInst::Load { .. }
            | MirInst::ReadFile { .. }
            | MirInst::WriteFile { .. }
            | MirInst::AppendFile { .. }
            | MirInst::ReadBytesFile { .. }
            | MirInst::WriteBytesFile { .. }
            // α.7: 类型系统 — SSA 中跳过
            | MirInst::TraitDef { .. }
            | MirInst::ImplDef { .. }
            // α.8: 高级特性 — SSA 中跳过
            | MirInst::Orchestrate { .. }
            | MirInst::Eval { .. }
            | MirInst::SkillDef { .. }
            | MirInst::PromptSection { .. }
            | MirInst::DocumentSection { .. } => {}
            | MirInst::DynTrait { .. } => {}
        }
    }

    (ssa_insts, Terminator::Return(None))
}

#[allow(clippy::needless_range_loop)]
fn compute_dominators(blocks: &[BasicBlock]) -> Vec<Option<BlockId>> {
    let n = blocks.len();
    if n == 0 {
        return vec![];
    }

    // Step 1: 计算每个块的支配集合（迭代不动点）
    // Dom(entry) = {entry}
    // Dom(b) = {b} ∪ ⋂_{p in preds(b)} Dom(p)
    let mut dom_sets: Vec<HashSet<BlockId>> = Vec::with_capacity(n);
    for bid in 0..n {
        let mut s = HashSet::new();
        if blocks[bid].preds.is_empty() {
            s.insert(bid);
        }
        dom_sets.push(s);
    }

    // entry 块的支配集合只包含自己
    if !blocks[0].preds.is_empty() {
        // 没有前驱的块才是 entry；如果 block 0 有前驱，说明它不是 entry
        // 找到真正的 entry（无前驱的块）
        let mut entry = 0;
        for bid in 0..n {
            if blocks[bid].preds.is_empty() {
                entry = bid;
                break;
            }
        }
        dom_sets[entry].clear();
        dom_sets[entry].insert(entry);
    }

    let mut changed = true;
    let mut iter = 0;
    while changed && iter < 100 {
        changed = false;
        iter += 1;
        for bid in 0..n {
            let preds = &blocks[bid].preds;
            if preds.is_empty() {
                continue;
            }

            // Dom(b) = {b} ∪ ⋂_{p in preds(b)} Dom(p)
            let mut new_dom: HashSet<BlockId> = HashSet::new();
            new_dom.insert(bid);

            let mut first = true;
            let mut intersection: HashSet<BlockId> = HashSet::new();
            for &pred in preds {
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

    // Step 2: 从支配集合推导 immediate dominator
    // idom(b) = d where d is in Dom(b) and Dom(d) is a proper subset of Dom(b)
    // 且 Dom(d) ∪ {b} 是 Dom(b) 的极大真子集
    let mut idom: Vec<Option<BlockId>> = vec![None; n];
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

    idom
}

#[allow(clippy::needless_range_loop)]
fn compute_dominance_frontier(
    blocks: &[BasicBlock],
    idom: &[Option<BlockId>],
) -> Vec<Vec<BlockId>> {
    let n = blocks.len();
    let mut dom_frontier: Vec<Vec<BlockId>> = vec![Vec::new(); n];

    for bid in 0..n {
        let preds = &blocks[bid].preds;
        if preds.is_empty() {
            continue;
        }

        for &pred in preds {
            let mut runner = pred;
            loop {
                dom_frontier[runner].push(bid);
                if let Some(next) = idom[runner] {
                    if next == bid || next == runner {
                        break;
                    }
                    runner = next;
                } else {
                    break;
                }
            }
        }
    }

    for df in &mut dom_frontier {
        df.sort();
        df.dedup();
    }

    dom_frontier
}

fn collect_definitions(blocks: &[BasicBlock]) -> HashMap<SsaReg, Vec<BlockId>> {
    let mut defs: HashMap<SsaReg, Vec<BlockId>> = HashMap::new();
    for block in blocks {
        for inst in &block.insts {
            // Define 不是真正的值定义，跳过（真实值由 Assign 提供）
            if let SsaInst::Define(_, _) = inst {
                continue;
            }
            let dst = ssa_dst(inst);
            defs.entry(dst).or_default().push(block.id);
        }
    }
    defs
}

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

fn insert_phi_nodes(
    _blocks: &[BasicBlock],
    dom_frontier: &[Vec<BlockId>],
    defs: &HashMap<SsaReg, Vec<BlockId>>,
    phi_map: &mut HashMap<(BlockId, SsaReg), Phi>,
) {
    let mut worklist: VecDeque<BlockId> = VecDeque::new();
    let mut visited: HashSet<BlockId> = HashSet::new();

    for block_ids in defs.values() {
        for &bid in block_ids {
            if !visited.contains(&bid) {
                worklist.push_back(bid);
            }
        }
    }

    while let Some(bid) = worklist.pop_front() {
        if visited.contains(&bid) {
            continue;
        }
        visited.insert(bid);

        let block_defs: Vec<SsaReg> = defs
            .iter()
            .filter(|(_, block_ids)| block_ids.contains(&bid))
            .map(|(&reg, _)| reg)
            .collect();

        for reg in block_defs {
            for &target in &dom_frontier[bid] {
                if let std::collections::hash_map::Entry::Vacant(e) = phi_map.entry((target, reg)) {
                    e.insert(Phi {
                        dst: reg,
                        incoming: Vec::new(),
                    });
                    if !visited.contains(&target) && !worklist.iter().any(|&x| x == target) {
                        worklist.push_back(target);
                    }
                }
            }
        }
    }
}

fn rename_variables(blocks: &mut [BasicBlock], phi_map: &HashMap<(BlockId, SsaReg), Phi>) {
    let num_blocks = blocks.len();
    if num_blocks == 0 {
        return;
    }

    let mut reg_counter = 0;
    let max_orig_reg = phi_map
        .values()
        .map(|p| p.dst)
        .chain(blocks.iter().flat_map(|b| b.insts.iter()).map(ssa_dst))
        .max()
        .unwrap_or(0);

    let mut rename_stack: Vec<Vec<SsaReg>> = vec![Vec::new(); max_orig_reg.saturating_add(1)];

    let block_phi_map: HashMap<BlockId, Vec<_>> =
        phi_map
            .iter()
            .fold(HashMap::new(), |mut acc, (&(bid, _), phi)| {
                acc.entry(bid)
                    .or_insert_with(Vec::new)
                    .push((phi.dst, phi.incoming.clone()));
                acc
            });

    let mut stack = vec![0];
    let mut visited: HashSet<BlockId> = HashSet::new();

    while let Some(bid) = stack.pop() {
        if visited.contains(&bid) {
            continue;
        }
        visited.insert(bid);

        let block = &mut blocks[bid];

        if let Some(phis) = block_phi_map.get(&bid) {
            for &(orig_dst, _) in phis {
                let new_dst = reg_counter;
                reg_counter += 1;
                if orig_dst < rename_stack.len() {
                    rename_stack[orig_dst].push(new_dst);
                }
            }
        }

        for inst in &mut block.insts {
            // Define 只是变量声明，不是值定义——它的 src 不参与 rename，也不推入 stack
            // 真实值由后续的 Assign 提供
            if let SsaInst::Define(_, _src) = inst {
                // 给 Define 分配新的 dst，但不修改 src，也不推入 rename_stack
                let new_dst = reg_counter;
                reg_counter += 1;
                set_dst(inst, new_dst);
                // src 保持不变（它是原始 MIR 中的寄存器编号，不是 SSA 版本）
                continue;
            }
            rename_reads(inst, &rename_stack);
            let old_dst = ssa_dst(inst);
            let new_dst = reg_counter;
            reg_counter += 1;
            set_dst(inst, new_dst);
            if old_dst < rename_stack.len() {
                rename_stack[old_dst].push(new_dst);
            }
        }

        match &mut block.terminator {
            Terminator::JumpIf(cond, _, _) | Terminator::JumpIfNot(cond, _, _) => {
                let old = *cond;
                if old < rename_stack.len() && !rename_stack[old].is_empty() {
                    *cond = *rename_stack[old].last().unwrap();
                }
            }
            Terminator::Return(Some(reg)) => {
                let old = *reg;
                if old < rename_stack.len() && !rename_stack[old].is_empty() {
                    *reg = *rename_stack[old].last().unwrap();
                }
            }
            _ => {}
        }

        for &succ in &block.succs {
            stack.push(succ);
        }
    }
}

fn set_dst(inst: &mut SsaInst, d: SsaReg) {
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

fn rename_reads(inst: &mut SsaInst, stack: &[Vec<SsaReg>]) {
    fn resolve(reg: SsaReg, stack: &[Vec<SsaReg>]) -> SsaReg {
        if reg < stack.len() && !stack[reg].is_empty() {
            *stack[reg].last().unwrap()
        } else {
            reg
        }
    }

    match inst {
        SsaInst::BinaryOp(_, l, _, r) => {
            *l = resolve(*l, stack);
            *r = resolve(*r, stack);
        }
        SsaInst::Call(_, _, args) => {
            for r in args.iter_mut() {
                *r = resolve(*r, stack);
            }
        }
        SsaInst::ListLit(_, items) => {
            for r in items.iter_mut() {
                *r = resolve(*r, stack);
            }
        }
        SsaInst::DictLit(_, pairs) => {
            for (_, v) in pairs.iter_mut() {
                *v = resolve(*v, stack);
            }
        }
        SsaInst::Index(_, obj, idx) => {
            *obj = resolve(*obj, stack);
            *idx = resolve(*idx, stack);
        }
        SsaInst::IndexAssign(_, obj, idx) => {
            *obj = resolve(*obj, stack);
            *idx = resolve(*idx, stack);
        }
        SsaInst::MethodCall(_, recv, _, args) => {
            *recv = resolve(*recv, stack);
            for r in args.iter_mut() {
                *r = resolve(*r, stack);
            }
        }
        SsaInst::Pipe(_, lhs, rhs) => {
            *lhs = resolve(*lhs, stack);
            *rhs = resolve(*rhs, stack);
        }
        SsaInst::Prompt(_, parts) => {
            for r in parts.iter_mut() {
                *r = resolve(*r, stack);
            }
        }
        SsaInst::Copy(_, src) => *src = resolve(*src, stack),
        SsaInst::Assign(_, src) => *src = resolve(*src, stack),
        SsaInst::Expr(src) => *src = resolve(*src, stack),
        // Define 的 src 是原始 MIR 寄存器编号，不参与 SSA rename
        SsaInst::Define(_, _) => {}
        SsaInst::Var(_, _) | SsaInst::Const(_, _) => {}
    }
}

// ── deconstruct: MIR-ssa → MIR-plain ──

/// 将 SSA 函数转回 MIR-plain（phi → copy 指令，BlockId → Label）。
///
/// 关键策略：
/// - 每个 phi 节点分配一个共享 MIR-plain 寄存器 dst_p
/// - 每个前驱块在 terminator 之前把 src 复制到 dst_p
/// - 当前块不再读 phi，直接从 dst_p 读（已经是最终值）
/// - 无 incoming 的 phi → 块开头 emit Const(Nil) 到 dst_p（默认值）
/// - 第二遍补丁：Jump 目标从 BlockId → body 索引
///
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
                // Copy src reg to dst reg: Assign(tmp, src) + Var(dst, tmp)
                let tmp = next_tmp_name(tmp_name_counter, tmp_names);
                out.push(MirInst::Assign(tmp.clone(), map_reg(*src)));
                out.push(MirInst::Var(map_reg(*dst), tmp));
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
            Terminator::Return(reg) => MirInst::Return(reg.map(map_reg)),
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
        block_insts.push(terminator_to_plain(
            &block.terminator,
            &ssa_to_plain,
            num_blocks,
        ));

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
            // Copy: Assign(tmp, src) + Var(dst, tmp)
            let tmp = next_tmp_name(&mut tmp_name_counter, &mut tmp_names);
            target.insert(term_idx, MirInst::Assign(tmp.clone(), src_p));
            target.insert(term_idx, MirInst::Var(*dst_p, tmp));
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
        body,
        n_regs: next_plain_reg,
    }
}
