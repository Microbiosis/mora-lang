//! v0.110.0: Direct explicit-CFG optimization passes for [`ControlProgram`].
//!
//! All passes operate directly on [`ControlFunction`] `BasicBlock`s and SSA
//! block parameters, preserving CFG invariants and strict [`Purity`] semantics:
//! - [`Purity::Fallible`] and [`Purity::Effectful`] operations are never
//!   discarded by DCE and never speculated out of loops by LICM.
//! - Constant folding uses [`crate::flow::eval_binary`] directly for exact
//!   numeric-tower parity with the runtime VM (and preserves runtime errors on
//!   invalid ops like `1i / 0i`).
//!
//! Pass set, by level:
//! - `None`: no pass runs.
//! - `Basic`: constant/copy propagation + constant folding, CFG simplification,
//!   dead-code elimination and block compaction (up to 2 rounds).
//! - `Aggressive`: `Basic` plus GVN/CSE and LICM (up to 4 rounds).

use std::collections::{HashMap, HashSet, VecDeque};

use crate::flow::eval_binary;
use crate::value::Value;

/// Optimization level for Control SSA optimization passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptLevel {
    None,
    Basic,
    Aggressive,
}

impl OptLevel {
    /// Parse CLI `--opt=N` option into an [`OptLevel`].
    pub fn from_arg(value: &str) -> Option<OptLevel> {
        value.parse::<u32>().ok().map(|n| match n {
            0 => OptLevel::None,
            1 => OptLevel::Basic,
            _ => OptLevel::Aggressive,
        })
    }

    /// Read `MORA_OPT` environment variable fallback (`0` -> `None`, `1` -> `Basic`, `>=2` -> `Aggressive`).
    pub fn from_env() -> OptLevel {
        std::env::var("MORA_OPT")
            .ok()
            .and_then(|v| OptLevel::from_arg(&v))
            .unwrap_or(OptLevel::None)
    }

    /// Returns `true` if any optimization passes should run.
    pub fn enabled(&self) -> bool {
        self != &OptLevel::None
    }

    /// Returns `true` when aggressive passes (GVN/CSE, LICM) are enabled.
    pub fn aggressive(&self) -> bool {
        self == &OptLevel::Aggressive
    }
}

impl Default for OptLevel {
    fn default() -> Self {
        Self::from_env()
    }
}

use super::ir::{
    BasicBlock, BlockId, BuiltinFnId, ControlConst, ControlFunction, ControlInst, ControlProgram,
    ControlTerminator, Purity, UnaryOp, VReg,
};

/// Optimize every function in `prog` at the requested [`OptLevel`].
pub fn optimize_program(prog: &mut ControlProgram, level: OptLevel) {
    if !level.enabled() {
        return;
    }
    let max_rounds = if level.aggressive() { 4 } else { 2 };
    for func in &mut prog.functions {
        for _ in 0..max_rounds {
            let mut changed = false;
            changed |= const_and_copy_prop(func);
            if level.aggressive() {
                changed |= gvn_cse(func);
                changed |= licm_hoist_pure(func);
            }
            changed |= simplify_cfg(func);
            changed |= dce_and_compact(func);
            if !changed {
                break;
            }
        }
    }
}

/// Canonical float key preserving signed zero and distinct NaN bit patterns.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ConstKey {
    Nil,
    Bool(bool),
    Int(i64),
    FloatBits(u64),
    ScalarBits(crate::common::DType, u64),
    String(String),
    Char(char),
    BuiltinFn(BuiltinFnId),
}

fn const_key(c: &ControlConst) -> Option<ConstKey> {
    match c {
        ControlConst::Nil => Some(ConstKey::Nil),
        ControlConst::Bool(b) => Some(ConstKey::Bool(*b)),
        ControlConst::Int(i) => Some(ConstKey::Int(*i)),
        ControlConst::Float(f) => Some(ConstKey::FloatBits(f.to_bits())),
        ControlConst::Scalar(s) => {
            let (dt, bits) = s.bit_key();
            Some(ConstKey::ScalarBits(dt, bits))
        }
        ControlConst::String(s) => Some(ConstKey::String(s.clone())),
        ControlConst::Char(ch) => Some(ConstKey::Char(*ch)),
        ControlConst::BuiltinFn(id) => Some(ConstKey::BuiltinFn(*id)),
        ControlConst::BuiltinModule(_) | ControlConst::MissingArg => None,
    }
}

fn const_is_truthy(c: &ControlConst) -> bool {
    match c {
        ControlConst::Nil | ControlConst::MissingArg => false,
        ControlConst::Bool(b) => *b,
        ControlConst::Int(i) => *i != 0,
        ControlConst::Float(f) => *f != 0.0 && !f.is_nan(),
        ControlConst::Scalar(s) => s.is_truthy(),
        ControlConst::String(s) => !s.is_empty(),
        ControlConst::Char(c) => *c != '\0',
        ControlConst::BuiltinFn(_) | ControlConst::BuiltinModule(_) => true,
    }
}

fn value_to_control_const(v: &Value) -> Option<ControlConst> {
    match v {
        Value::Nil => Some(ControlConst::Nil),
        Value::Bool(b) => Some(ControlConst::Bool(*b)),
        Value::Int(i) => Some(ControlConst::Int(*i)),
        Value::Float(f) => Some(ControlConst::Float(*f)),
        Value::Scalar(s) => Some(ControlConst::Scalar(*s)),
        Value::String(s) => Some(ControlConst::String(s.clone())),
        Value::Char(c) => Some(ControlConst::Char(*c)),
        _ => None,
    }
}

fn resolve_alias(aliases: &HashMap<VReg, VReg>, mut r: VReg) -> VReg {
    let mut hops = 0usize;
    while let Some(&next) = aliases.get(&r) {
        if next == r || hops > aliases.len() {
            break;
        }
        r = next;
        hops += 1;
    }
    r
}

/// RPO constant propagation, constant folding (exact via `eval_binary`), copy propagation,
/// and constant branch folding (`Branch` -> `Jump`).
fn const_and_copy_prop(func: &mut ControlFunction) -> bool {
    let mut changed = false;
    let rpo = func.rpo_order();
    let mut consts: HashMap<VReg, ControlConst> = HashMap::new();
    let mut aliases: HashMap<VReg, VReg> = HashMap::new();

    for bid in rpo {
        let block = &mut func.blocks[bid.0 as usize];
        for inst in &mut block.insts {
            // Forward existing copy aliases into operand reads first.
            let prev_dst = inst.dst();
            inst.remap_regs(|r| {
                if Some(r) == prev_dst {
                    r
                } else {
                    let canon = resolve_alias(&aliases, r);
                    if canon != r {
                        changed = true;
                    }
                    canon
                }
            });

            match inst {
                ControlInst::Const(dst, c) => {
                    consts.insert(*dst, c.clone());
                }
                ControlInst::Copy(dst, src) => {
                    let canon = resolve_alias(&aliases, *src);
                    if canon != *dst {
                        aliases.insert(*dst, canon);
                        if let Some(c) = consts.get(&canon).cloned() {
                            consts.insert(*dst, c.clone());
                            *inst = ControlInst::Const(*dst, c);
                            changed = true;
                        }
                    }
                }
                ControlInst::Unary(dst, op, src) => {
                    if let Some(c) = consts.get(src).cloned() {
                        let folded_opt = match op {
                            UnaryOp::Truthy => Some(ControlConst::Bool(const_is_truthy(&c))),
                            UnaryOp::Not => Some(ControlConst::Bool(!const_is_truthy(&c))),
                            UnaryOp::Neg => match c {
                                ControlConst::Int(n) => n.checked_neg().map(ControlConst::Int),
                                ControlConst::Float(f) => Some(ControlConst::Float(-f)),
                                ControlConst::Scalar(s) => s.neg().ok().map(ControlConst::Scalar),
                                _ => None,
                            },
                        };
                        if let Some(folded) = folded_opt {
                            consts.insert(*dst, folded.clone());
                            *inst = ControlInst::Const(*dst, folded);
                            changed = true;
                        }
                    }
                }
                ControlInst::Binary(dst, lhs, op, rhs) => {
                    if let (Some(lc), Some(rc)) = (consts.get(lhs), consts.get(rhs))
                        && let Ok(val) = eval_binary(lc.to_value(), op, rc.to_value())
                        && let Some(folded) = value_to_control_const(&val)
                    {
                        let d = *dst;
                        consts.insert(d, folded.clone());
                        *inst = ControlInst::Const(d, folded);
                        changed = true;
                    }
                }
                _ => {}
            }
        }

        block.terminator.remap_regs(|r| {
            let canon = resolve_alias(&aliases, r);
            if canon != r {
                changed = true;
            }
            canon
        });

        if let ControlTerminator::Branch {
            cond,
            then_bb,
            then_args,
            else_bb,
            else_args,
        } = &block.terminator
            && let Some(c) = consts.get(cond)
        {
            let take_then = const_is_truthy(c);
            let new_term = if take_then {
                ControlTerminator::Jump {
                    target: *then_bb,
                    args: then_args.clone(),
                }
            } else {
                ControlTerminator::Jump {
                    target: *else_bb,
                    args: else_args.clone(),
                }
            };
            block.terminator = new_term;
            changed = true;
        }
    }

    changed
}

/// Dominator-tree scoped Global Value Numbering / CSE over deterministic [`Purity::Pure`]
/// scalar expressions (`Const`, `Unary`).
fn gvn_cse(func: &mut ControlFunction) -> bool {
    #[derive(Clone, PartialEq, Eq, Hash)]
    enum ExprKey {
        Const(ConstKey),
        Unary(UnaryOp, VReg),
    }

    let n = func.blocks.len();
    let idom = func.compute_idom();
    let mut dom_children = vec![Vec::new(); n];
    for (idx, d) in idom.iter().enumerate() {
        if let Some(parent) = d
            && parent.0 as usize != idx
        {
            dom_children[parent.0 as usize].push(BlockId(idx as u32));
        }
    }

    let mut changed = false;
    let mut table: HashMap<ExprKey, VReg> = HashMap::new();

    #[derive(Clone, Copy)]
    enum Action {
        Enter(BlockId),
        Leave(usize),
    }

    let mut undo_stack: Vec<(ExprKey, Option<VReg>)> = Vec::new();
    let mut work = vec![Action::Enter(func.entry)];

    while let Some(action) = work.pop() {
        match action {
            Action::Leave(mark) => {
                while undo_stack.len() > mark {
                    if let Some((key, prev)) = undo_stack.pop() {
                        match prev {
                            Some(v) => {
                                table.insert(key, v);
                            }
                            None => {
                                table.remove(&key);
                            }
                        }
                    }
                }
            }
            Action::Enter(bid) => {
                let mark = undo_stack.len();
                work.push(Action::Leave(mark));

                let block = &mut func.blocks[bid.0 as usize];
                for inst in &mut block.insts {
                    let key_opt = match inst {
                        ControlInst::Const(_, c) => const_key(c).map(ExprKey::Const),
                        ControlInst::Unary(_, op, src) => Some(ExprKey::Unary(*op, *src)),
                        _ => None,
                    };
                    if let (Some(key), Some(dst)) = (key_opt, inst.dst()) {
                        if let Some(&leader) = table.get(&key) {
                            *inst = ControlInst::Copy(dst, leader);
                            changed = true;
                        } else {
                            let prev = table.insert(key.clone(), dst);
                            undo_stack.push((key, prev));
                        }
                    }
                }

                // Visit dom children in reverse push order so left-to-right order runs naturally.
                for &child in dom_children[bid.0 as usize].iter().rev() {
                    work.push(Action::Enter(child));
                }
            }
        }
    }

    changed
}

/// Hoist loop-invariant [`Purity::Pure`] instructions out of natural loops into a
/// preheader block when safe (never speculating [`Purity::Fallible`] or [`Purity::Effectful`]).
fn licm_hoist_pure(func: &mut ControlFunction) -> bool {
    let n = func.blocks.len();
    let idom = func.compute_idom();
    let preds = func.predecessors();
    let rpo = func.rpo_order();

    // Group natural loop bodies keyed by loop header `h`.
    let mut loops_by_header: HashMap<BlockId, HashSet<BlockId>> = HashMap::new();
    for &u in &rpo {
        for h in func.blocks[u.0 as usize].terminator.successors() {
            if func.dominates(&idom, h, u) {
                let body = loops_by_header.entry(h).or_default();
                body.insert(h);
                body.insert(u);
                let mut q = VecDeque::from([u]);
                while let Some(cur) = q.pop_front() {
                    if cur == h {
                        continue;
                    }
                    for &p in &preds[cur.0 as usize] {
                        if body.insert(p) {
                            q.push_back(p);
                        }
                    }
                }
            }
        }
    }

    if loops_by_header.is_empty() {
        return false;
    }

    // Map VReg -> defining BlockId for quick loop-invariance tests.
    let mut def_block: HashMap<VReg, BlockId> = HashMap::new();
    for &bid in &rpo {
        let b = &func.blocks[bid.0 as usize];
        for &p in &b.params {
            def_block.insert(p, bid);
        }
        for inst in &b.insts {
            if let Some(d) = inst.dst() {
                def_block.insert(d, bid);
            }
        }
    }

    let mut changed = false;
    let headers: Vec<BlockId> = loops_by_header.keys().copied().collect();

    for h in headers {
        if h == func.entry || (h.0 as usize) >= n {
            continue;
        }
        let body = &loops_by_header[&h];
        // Find all outside predecessors entering `h`.
        let cur_preds = func.predecessors();
        let outside_preds: Vec<BlockId> = cur_preds[h.0 as usize]
            .iter()
            .copied()
            .filter(|p| !body.contains(p))
            .collect();
        if outside_preds.is_empty() {
            continue;
        }

        // Collect pure loop-invariant instructions defined in loop blocks that dominate all uses.
        // Only hoist instructions whose inputs are defined strictly outside `body` (or already hoisted).
        let mut invariant_regs: HashSet<VReg> = HashSet::new();
        let mut hoist_list: Vec<(BlockId, usize)> = Vec::new();
        loop {
            let mut found_one = false;
            for &bid in body {
                let block = &func.blocks[bid.0 as usize];
                for (iidx, inst) in block.insts.iter().enumerate() {
                    if inst.purity() != Purity::Pure {
                        continue;
                    }
                    let Some(dst) = inst.dst() else {
                        continue;
                    };
                    if invariant_regs.contains(&dst) {
                        continue;
                    }
                    let all_inputs_outside = inst.input_regs().iter().all(|r| {
                        invariant_regs.contains(r)
                            || def_block.get(r).is_some_and(|db| !body.contains(db))
                    });
                    if all_inputs_outside {
                        invariant_regs.insert(dst);
                        hoist_list.push((bid, iidx));
                        found_one = true;
                    }
                }
            }
            if !found_one {
                break;
            }
        }

        if hoist_list.is_empty() {
            continue;
        }

        // Locate or insert a loop preheader for `h`.
        let preheader_bb = if outside_preds.len() == 1 {
            let p = outside_preds[0];
            if matches!(
                func.blocks[p.0 as usize].terminator,
                ControlTerminator::Jump { target, .. } if target == h
            ) {
                p
            } else {
                create_preheader(func, h, &outside_preds)
            }
        } else {
            create_preheader(func, h, &outside_preds)
        };

        // Extract hoisted instructions preserving order and append them to `preheader_bb`.
        let mut hoist_by_block: HashMap<BlockId, HashSet<usize>> = HashMap::new();
        for (bid, iidx) in &hoist_list {
            hoist_by_block.entry(*bid).or_default().insert(*iidx);
        }

        let mut hoisted_insts = Vec::with_capacity(hoist_list.len());
        for (bid, iidx) in hoist_list {
            let inst = func.blocks[bid.0 as usize].insts[iidx].clone();
            hoisted_insts.push(inst);
        }
        for (bid, indices) in hoist_by_block {
            let mut idx = 0usize;
            func.blocks[bid.0 as usize].insts.retain(|_| {
                let keep = !indices.contains(&idx);
                idx += 1;
                keep
            });
        }
        func.blocks[preheader_bb.0 as usize]
            .insts
            .extend(hoisted_insts);
        for d in invariant_regs {
            def_block.insert(d, preheader_bb);
        }
        changed = true;
    }

    changed
}

fn create_preheader(
    func: &mut ControlFunction,
    header: BlockId,
    outside_preds: &[BlockId],
) -> BlockId {
    let header_params = func.blocks[header.0 as usize].params.len();
    let pre_id = BlockId(func.blocks.len() as u32);
    let mut pre_params = Vec::with_capacity(header_params);
    for _ in 0..header_params {
        let r = VReg(func.n_regs);
        func.n_regs += 1;
        pre_params.push(r);
    }
    func.blocks.push(BasicBlock {
        id: pre_id,
        params: pre_params.clone(),
        insts: Vec::new(),
        terminator: ControlTerminator::Jump {
            target: header,
            args: pre_params,
        },
    });
    for &p in outside_preds {
        let term = &mut func.blocks[p.0 as usize].terminator;
        match term {
            ControlTerminator::Jump { target, .. } if *target == header => {
                *target = pre_id;
            }
            ControlTerminator::Branch {
                then_bb, else_bb, ..
            } => {
                if *then_bb == header {
                    *then_bb = pre_id;
                }
                if *else_bb == header {
                    *else_bb = pre_id;
                }
            }
            _ => {}
        }
    }
    pre_id
}

/// Simplify trivial jump-only blocks, collapse single-predecessor `Jump` chains, and
/// prune trivial identical `BlockParam`s.
fn simplify_cfg(func: &mut ControlFunction) -> bool {
    let mut changed = false;

    // 1. Thread empty Jump blocks without params: `A -> jump B; B: [] jump C(args)` => `A -> jump C(args)`.
    let mut redirects: HashMap<BlockId, (BlockId, Vec<VReg>)> = HashMap::new();
    for b in &func.blocks {
        if b.id == func.entry || !b.params.is_empty() || !b.insts.is_empty() {
            continue;
        }
        if let ControlTerminator::Jump { target, args } = &b.terminator
            && *target != b.id
        {
            redirects.insert(b.id, (*target, args.clone()));
        }
    }
    if !redirects.is_empty() {
        for b in &mut func.blocks {
            match &mut b.terminator {
                ControlTerminator::Jump { target, args } => {
                    if let Some((new_t, new_a)) = redirects.get(target) {
                        *target = *new_t;
                        *args = new_a.clone();
                        changed = true;
                    }
                }
                ControlTerminator::Branch {
                    then_bb,
                    then_args,
                    else_bb,
                    else_args,
                    ..
                } => {
                    if let Some((new_t, new_a)) = redirects.get(then_bb) {
                        *then_bb = *new_t;
                        *then_args = new_a.clone();
                        changed = true;
                    }
                    if let Some((new_t, new_a)) = redirects.get(else_bb) {
                        *else_bb = *new_t;
                        *else_args = new_a.clone();
                        changed = true;
                    }
                }
                _ => {}
            }
        }
    }

    // 2. Fold identical Branch targets with identical args into Jump.
    for b in &mut func.blocks {
        if let ControlTerminator::Branch {
            then_bb,
            then_args,
            else_bb,
            else_args,
            ..
        } = &b.terminator
            && then_bb == else_bb
            && then_args == else_args
        {
            b.terminator = ControlTerminator::Jump {
                target: *then_bb,
                args: then_args.clone(),
            };
            changed = true;
        }
    }

    // 3. Single-predecessor Jump merge: `A -> jump B(args)` with `preds[B] == [A]`, `B != entry`, `A != B`.
    let preds = func.predecessors();
    let rpo = func.rpo_order();
    let mut merged = HashSet::new();
    for &a_id in &rpo {
        if merged.contains(&a_id) {
            continue;
        }
        let next_info = match &func.blocks[a_id.0 as usize].terminator {
            ControlTerminator::Jump { target, args } => Some((*target, args.clone())),
            _ => None,
        };
        let Some((b_id, edge_args)) = next_info else {
            continue;
        };
        if b_id == func.entry || b_id == a_id || merged.contains(&b_id) {
            continue;
        }
        if preds[b_id.0 as usize].len() != 1 || preds[b_id.0 as usize][0] != a_id {
            continue;
        }
        // Check `a_id` doesn't mention `b_id` elsewhere (already guaranteed since `Jump` only has 1 target).
        merged.insert(b_id);
        let b_params = std::mem::take(&mut func.blocks[b_id.0 as usize].params);
        let b_insts = std::mem::take(&mut func.blocks[b_id.0 as usize].insts);
        let b_term = std::mem::replace(
            &mut func.blocks[b_id.0 as usize].terminator,
            ControlTerminator::Unreachable,
        );

        let mut copies = Vec::with_capacity(b_params.len());
        for (p, a) in b_params.into_iter().zip(edge_args) {
            copies.push(ControlInst::Copy(p, a));
        }

        let a_blk = &mut func.blocks[a_id.0 as usize];
        a_blk.insts.extend(copies);
        a_blk.insts.extend(b_insts);
        a_blk.terminator = b_term;
        changed = true;
    }

    changed
}

/// Remove dead [`Purity::Pure`] definitions and compact unreachable basic blocks.
fn dce_and_compact(func: &mut ControlFunction) -> bool {
    let mut changed = false;
    let rpo = func.rpo_order();
    let reachable: HashSet<BlockId> = rpo.iter().copied().collect();

    loop {
        let mut used_regs: HashSet<VReg> = HashSet::new();
        for &bid in &rpo {
            let b = &func.blocks[bid.0 as usize];
            for inst in &b.insts {
                for r in inst.input_regs() {
                    used_regs.insert(r);
                }
            }
            for r in b.terminator.input_regs() {
                used_regs.insert(r);
            }
        }

        let mut deleted_any = false;
        for &bid in &rpo {
            let b = &mut func.blocks[bid.0 as usize];
            let before = b.insts.len();
            b.insts.retain(|inst| {
                if inst.purity() != Purity::Pure {
                    return true;
                }
                match inst.dst() {
                    Some(d) => used_regs.contains(&d),
                    None => true,
                }
            });
            if b.insts.len() != before {
                deleted_any = true;
                changed = true;
            }
        }
        if !deleted_any {
            break;
        }
    }

    if reachable.len() < func.blocks.len() {
        let mut remap: HashMap<BlockId, BlockId> = HashMap::new();
        let mut new_blocks = Vec::with_capacity(reachable.len());
        for b in &func.blocks {
            if reachable.contains(&b.id) {
                let new_id = BlockId(new_blocks.len() as u32);
                remap.insert(b.id, new_id);
                let mut cloned = b.clone();
                cloned.id = new_id;
                new_blocks.push(cloned);
            }
        }
        for b in &mut new_blocks {
            match &mut b.terminator {
                ControlTerminator::Jump { target, .. } => {
                    *target = remap[target];
                }
                ControlTerminator::Branch {
                    then_bb, else_bb, ..
                } => {
                    *then_bb = remap[then_bb];
                    *else_bb = remap[else_bb];
                }
                ControlTerminator::Perform { resume_bb, .. } => {
                    *resume_bb = remap[resume_bb];
                }
                ControlTerminator::Return(_)
                | ControlTerminator::TailCall { .. }
                | ControlTerminator::Halt(_)
                | ControlTerminator::Unreachable => {}
            }
        }
        func.entry = remap[&func.entry];
        func.blocks = new_blocks;
        changed = true;
    }

    changed
}
