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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use crate::flow::eval_binary;
use crate::value::Value;

use super::liveness::{
    SlotLiveness, collect_pattern_globals, compute_private_main_globals, compute_slot_liveness,
    compute_terminator_kill_mask, compute_vreg_live_in, inst_clobbers_local, inst_local_uses,
    is_borrowing_builtin, is_projecting_method, is_valid_borrow_inst, is_valid_borrow_terminator,
    method_arg_borrows, method_receiver_borrows, pattern_mentions_local, program_mentions_eval,
};

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
    BasicBlock, BlockId, BuiltinFnId, CalleeTarget, ControlConst, ControlFunction, ControlInst,
    ControlProgram, ControlTerminator, FunctionKind, GlobalSlot, KillMask, LocalSlot, Purity,
    SymId, SymbolTable, UnaryOp, VReg, verify_program,
};

/// Optimize every function in `prog` at the requested [`OptLevel`], and run the P6-1 Perceus /
/// FBIP ownership, borrow-inference, and edge-drop pass across all [`OptLevel`]s.
pub fn optimize_program(prog: &mut ControlProgram, level: OptLevel) {
    if level.enabled() {
        strip_perceus_annotations(prog);
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
    perceus_ownership_pass(prog);
    debug_assert!(
        verify_program(prog).is_ok(),
        "optimize_program produced invalid ControlProgram: {:?}",
        verify_program(prog).err()
    );
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
        inst_kill_masks: Vec::new(),
        term_kill_mask: KillMask::Empty,
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

// ─── P6-1: Lean 4 Perceus / FBIP Ownership, Borrow Inference & Edge-Drop Pass ───

/// Normalize any previously inserted Perceus instructions/masks back to canonical SSA form so
/// `perceus_ownership_pass` is strictly idempotent across repeated optimization rounds.
fn strip_perceus_annotations(prog: &mut ControlProgram) {
    for func in &mut prog.functions {
        for b in &mut func.blocks {
            b.inst_kill_masks.clear();
            b.term_kill_mask = KillMask::Empty;
            b.insts.retain_mut(|inst| match inst {
                ControlInst::DropReg(_) | ControlInst::DropLocal(_) => false,
                ControlInst::MoveLocal(d, s) | ControlInst::BorrowLocal(d, s) => {
                    *inst = ControlInst::LoadLocal(*d, *s);
                    true
                }
                ControlInst::MoveGlobal(d, g) => {
                    *inst = ControlInst::LoadGlobal(*d, *g);
                    true
                }
                _ => true,
            });
        }
    }
}

/// Infer `ControlFunction::param_borrows` across `prog` via greatest fixed-point iteration
/// (*Counting Immutable Beans*, §4 Borrow Inference).
fn infer_param_borrows(prog: &mut ControlProgram) {
    let mut borrows: Vec<Vec<bool>> = prog
        .functions
        .iter()
        .map(|f| {
            let allow_borrow = matches!(
                f.kind,
                FunctionKind::Task
                    | FunctionKind::Closure
                    | FunctionKind::ImplMethod
                    | FunctionKind::TraitDefault
            );
            (0..f.params.len())
                .map(|i| {
                    allow_borrow
                        && !f.param_has_default.get(i).copied().unwrap_or(false)
                        && !f.shared_locals.get(i).copied().unwrap_or(false)
                })
                .collect()
        })
        .collect();

    let mut changed = true;
    while changed {
        changed = false;
        for f_idx in 0..prog.functions.len() {
            let func = &prog.functions[f_idx];
            for p_idx in 0..func.params.len() {
                if !borrows[f_idx][p_idx] {
                    continue;
                }
                let p_slot = LocalSlot(p_idx as u16);
                let mut escapes = false;

                // 1. Check direct slot writes or closure captures.
                for b in &func.blocks {
                    for inst in &b.insts {
                        if let ControlInst::StoreLocal(s, _) = inst
                            && *s == p_slot
                        {
                            escapes = true;
                            break;
                        }
                        if let ControlInst::MatchPattern { pattern, .. } = inst
                            && pattern_mentions_local(pattern, p_slot)
                        {
                            escapes = true;
                            break;
                        }
                        if !matches!(
                            inst,
                            ControlInst::LoadLocal(_, _)
                                | ControlInst::BorrowLocal(_, _)
                                | ControlInst::MoveLocal(_, _)
                                | ControlInst::IsMissingArg(_, _)
                        ) && inst_local_uses(inst).contains(&p_slot)
                        {
                            escapes = true;
                            break;
                        }
                    }
                    if escapes {
                        break;
                    }
                }
                if escapes {
                    borrows[f_idx][p_idx] = false;
                    changed = true;
                    continue;
                }

                // 2. Compute transitive set of VRegs & unshared LocalSlots holding or projecting
                //    `p_slot` (via LoadLocal / StoreLocal / Copy / Index / ProjectField / CFG edges).
                let mut tainted_locals: HashSet<LocalSlot> = HashSet::new();
                tainted_locals.insert(p_slot);
                let mut tainted: HashSet<VReg> = HashSet::new();
                let mut reg_changed = true;
                while reg_changed {
                    reg_changed = false;
                    for b in &func.blocks {
                        for inst in &b.insts {
                            match inst {
                                ControlInst::LoadLocal(d, s)
                                | ControlInst::BorrowLocal(d, s)
                                | ControlInst::MoveLocal(d, s)
                                    if tainted_locals.contains(s) =>
                                {
                                    if tainted.insert(*d) {
                                        reg_changed = true;
                                    }
                                }
                                ControlInst::StoreLocal(s, src)
                                    if tainted.contains(src)
                                        && *s != p_slot
                                        && !func
                                            .shared_locals
                                            .get(s.0 as usize)
                                            .copied()
                                            .unwrap_or(false)
                                        && tainted_locals.insert(*s) =>
                                {
                                    reg_changed = true;
                                }
                                ControlInst::Copy(d, src)
                                    if tainted.contains(src) && tainted.insert(*d) =>
                                {
                                    reg_changed = true;
                                }
                                ControlInst::Index(d, obj, _)
                                    if tainted.contains(obj) && tainted.insert(*d) =>
                                {
                                    reg_changed = true;
                                }
                                ControlInst::ProjectField { dst, receiver, .. }
                                    if tainted.contains(receiver) && tainted.insert(*dst) =>
                                {
                                    reg_changed = true;
                                }
                                ControlInst::MethodCall {
                                    dst,
                                    receiver,
                                    method,
                                    named_args,
                                    ..
                                } if named_args.is_empty()
                                    && tainted.contains(receiver)
                                    && is_projecting_method(prog.symbols.resolve(*method))
                                    && tainted.insert(*dst) =>
                                {
                                    reg_changed = true;
                                }
                                _ => {}
                            }
                        }
                        match &b.terminator {
                            ControlTerminator::Jump { target, args } => {
                                let params = &func.blocks[target.0 as usize].params;
                                for (a, p) in args.iter().zip(params.iter()) {
                                    if tainted.contains(a) && tainted.insert(*p) {
                                        reg_changed = true;
                                    }
                                }
                            }
                            ControlTerminator::Branch {
                                then_bb,
                                then_args,
                                else_bb,
                                else_args,
                                ..
                            } => {
                                let t_params = &func.blocks[then_bb.0 as usize].params;
                                for (a, p) in then_args.iter().zip(t_params.iter()) {
                                    if tainted.contains(a) && tainted.insert(*p) {
                                        reg_changed = true;
                                    }
                                }
                                let e_params = &func.blocks[else_bb.0 as usize].params;
                                for (a, p) in else_args.iter().zip(e_params.iter()) {
                                    if tainted.contains(a) && tainted.insert(*p) {
                                        reg_changed = true;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }

                // 3. Check whether any tainted LocalSlot or VReg is captured or used in an
                //    owned/escaping position.
                for b in &func.blocks {
                    for inst in &b.insts {
                        if !matches!(
                            inst,
                            ControlInst::LoadLocal(..)
                                | ControlInst::BorrowLocal(..)
                                | ControlInst::MoveLocal(..)
                                | ControlInst::IsMissingArg(..)
                        ) && inst_local_uses(inst)
                            .iter()
                            .any(|s| tainted_locals.contains(s))
                        {
                            escapes = true;
                            break;
                        }
                        match inst {
                            ControlInst::LoadLocal(..)
                            | ControlInst::BorrowLocal(..)
                            | ControlInst::MoveLocal(..)
                            | ControlInst::IsMissingArg(..)
                            | ControlInst::Copy(..)
                            | ControlInst::ProjectField { .. } => {}
                            ControlInst::StoreLocal(s, src) => {
                                if tainted.contains(src)
                                    && (*s == p_slot
                                        || func
                                            .shared_locals
                                            .get(s.0 as usize)
                                            .copied()
                                            .unwrap_or(false))
                                {
                                    escapes = true;
                                }
                            }
                            ControlInst::Unary(_, UnaryOp::Truthy | UnaryOp::Not, _)
                            | ControlInst::GetConstGeneric { .. }
                            | ControlInst::TryProbe(..) => {}
                            ControlInst::Index(_, _, idx) => {
                                if tainted.contains(idx) {
                                    escapes = true;
                                }
                            }
                            ControlInst::Binary(
                                _,
                                _,
                                crate::common::BinaryOp::Equal
                                | crate::common::BinaryOp::NotEqual
                                | crate::common::BinaryOp::Less
                                | crate::common::BinaryOp::LessEqual
                                | crate::common::BinaryOp::Greater
                                | crate::common::BinaryOp::GreaterEqual
                                | crate::common::BinaryOp::MatMul
                                | crate::common::BinaryOp::BitAnd
                                | crate::common::BinaryOp::BitOr
                                | crate::common::BinaryOp::BitXor
                                | crate::common::BinaryOp::Shl
                                | crate::common::BinaryOp::Shr,
                                _,
                            ) => {}
                            ControlInst::Call {
                                target: CalleeTarget::Builtin(bid),
                                named_args,
                                ..
                            } if named_args.is_empty() && is_borrowing_builtin(*bid) => {}
                            ControlInst::ModuleCall {
                                module:
                                    crate::value::BuiltinKind::Stats | crate::value::BuiltinKind::Linalg,
                                method,
                                named_args,
                                ..
                            } if named_args.is_empty()
                                && matches!(
                                    prog.symbols.resolve(*method),
                                    "mean"
                                        | "sum"
                                        | "min"
                                        | "max"
                                        | "median"
                                        | "stddev"
                                        | "var"
                                        | "norm"
                                        | "dot"
                                ) => {}
                            ControlInst::Call {
                                target: CalleeTarget::Fn(callee_id),
                                args,
                                named_args,
                                ..
                            } => {
                                let Some(callee) = prog.functions.get(callee_id.0 as usize) else {
                                    escapes = true;
                                    break;
                                };
                                for (arg_i, r) in args.iter().enumerate() {
                                    if tainted.contains(r)
                                        && !borrows
                                            .get(callee_id.0 as usize)
                                            .and_then(|v| v.get(arg_i))
                                            .copied()
                                            .unwrap_or(false)
                                    {
                                        escapes = true;
                                        break;
                                    }
                                }
                                if !escapes {
                                    for (kw_sym, r) in named_args {
                                        if tainted.contains(r) {
                                            let borrowed_kw = callee
                                                .params
                                                .iter()
                                                .position(|&p| p == *kw_sym)
                                                .and_then(|idx| {
                                                    borrows
                                                        .get(callee_id.0 as usize)
                                                        .and_then(|v| v.get(idx))
                                                        .copied()
                                                })
                                                .unwrap_or(false);
                                            if !borrowed_kw {
                                                escapes = true;
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                            ControlInst::MethodCall {
                                receiver,
                                method,
                                args,
                                named_args,
                                ..
                            } if named_args.is_empty() => {
                                let recv_ok = method_receiver_borrows(
                                    &prog.functions,
                                    &prog.symbols,
                                    *method,
                                    args.len(),
                                    Some(&borrows),
                                );
                                if tainted.contains(receiver) && !recv_ok {
                                    escapes = true;
                                }
                                for (ai, r) in args.iter().enumerate() {
                                    if tainted.contains(r)
                                        && !method_arg_borrows(
                                            &prog.functions,
                                            &prog.symbols,
                                            *method,
                                            args.len(),
                                            ai,
                                            Some(&borrows),
                                        )
                                    {
                                        escapes = true;
                                        break;
                                    }
                                }
                            }
                            other => {
                                if other.input_regs().iter().any(|r| tainted.contains(r)) {
                                    escapes = true;
                                }
                            }
                        }
                        if escapes {
                            break;
                        }
                    }
                    if escapes {
                        break;
                    }
                    match &b.terminator {
                        ControlTerminator::Jump { .. } | ControlTerminator::Branch { .. } => {}
                        ControlTerminator::TailCall {
                            target: CalleeTarget::Fn(callee_id),
                            args,
                        } => {
                            for (arg_i, r) in args.iter().enumerate() {
                                if tainted.contains(r)
                                    && !borrows
                                        .get(callee_id.0 as usize)
                                        .and_then(|v| v.get(arg_i))
                                        .copied()
                                        .unwrap_or(false)
                                {
                                    escapes = true;
                                    break;
                                }
                            }
                        }
                        other => {
                            if other.input_regs().iter().any(|r| tainted.contains(r)) {
                                escapes = true;
                                break;
                            }
                        }
                    }
                }

                if escapes {
                    borrows[f_idx][p_idx] = false;
                    changed = true;
                }
            }
        }
    }

    for (func, bvec) in prog.functions.iter_mut().zip(borrows) {
        func.param_borrows = bvec;
    }
}

/// P6-1 Perceus / Functional-But-In-Place (FBIP) compiler pass:
/// 1. Infers interprocedural borrowed parameter summaries (`ControlFunction::param_borrows`).
/// 2. Upgrades final live `LoadLocal`s of unshared locals to ownership-transferring `MoveLocal`
///    and inserts `DropLocal` on asymmetric branch edges.
/// 3. Upgrades the final live read of a `<main>`-private global to `MoveGlobal`, transferring
///    ownership out of its slot — unless an embedding environment captures the program globals
///    after `<main>` returns (REPL chunk / `eval`), in which case they must stay in place.
/// 4. Inserts `DropReg` for unused effectful destination registers and asymmetric branch edges,
///    then computes exact arbitrary-arity `inst_kill_masks` and `term_kill_mask` bitsets.
pub fn perceus_ownership_pass(prog: &mut ControlProgram) {
    strip_perceus_annotations(prog);
    infer_param_borrows(prog);

    let uses_eval = program_mentions_eval(prog);
    let private_main_globals = if uses_eval {
        HashSet::new()
    } else {
        compute_private_main_globals(prog)
    };
    let exit_globals: HashSet<GlobalSlot> = if prog.preserve_globals_at_exit {
        private_main_globals.clone()
    } else {
        HashSet::new()
    };
    let entry_fn = prog.entry;

    for f_idx in 0..prog.functions.len() {
        let stub = ControlFunction {
            id: prog.functions[f_idx].id,
            name: prog.functions[f_idx].name,
            kind: prog.functions[f_idx].kind,
            params: prog.functions[f_idx].params.clone(),
            param_has_default: Vec::new(),
            param_borrows: prog.functions[f_idx].param_borrows.clone(),
            local_names: Vec::new(),
            upvalues: Vec::new(),
            n_locals: 0,
            shared_locals: Vec::new(),
            n_regs: 0,
            entry: BlockId(0),
            blocks: Vec::new(),
            handler_arg_slots: Vec::new(),
            handler_k_slot: None,
        };
        let mut func = std::mem::replace(&mut prog.functions[f_idx], stub);
        let is_main = func.id == entry_fn;
        apply_perceus_to_function(
            &mut func,
            &prog.functions,
            &prog.symbols,
            !uses_eval,
            is_main,
            &private_main_globals,
            &exit_globals,
        );
        prog.functions[f_idx] = func;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum CoalesceSlot {
    Local(LocalSlot),
    Global(GlobalSlot),
}

type ProjLoadCandidate = (VReg, usize, Vec<usize>);

/// P6-1 Perceus/FBIP: coalesce sibling nominal `ControlInst::ProjectField` reads of a dead local
/// or `<main>`-private global struct slot so they share a single load register.
///
/// Because `lower.rs` statically emits `ProjectField` exclusively for proven nominal struct fields,
/// coalescing redundant loads lets Phase A upgrade the single remaining load to `MoveLocal` /
/// `MoveGlobal`, and at runtime each `ProjectField` extracts its own distinct field from the
/// uniquely-owned `StructInstance` in $O(1)$ regardless of field projection order.
fn coalesce_struct_projection_loads(
    func: &mut ControlFunction,
    is_main: bool,
    private_main_globals: &HashSet<GlobalSlot>,
) {
    let mut uses: HashMap<VReg, Vec<(usize, usize)>> = HashMap::new();
    for (bi, block) in func.blocks.iter().enumerate() {
        for (pc, inst) in block.insts.iter().enumerate() {
            for r in inst.input_regs() {
                uses.entry(r).or_default().push((bi, pc));
            }
        }
        for r in block.terminator.input_regs() {
            uses.entry(r).or_default().push((bi, usize::MAX));
        }
    }

    for (bi, block) in func.blocks.iter_mut().enumerate() {
        let mut slot_epoch: HashMap<CoalesceSlot, usize> = HashMap::new();
        let mut by_slot_epoch: BTreeMap<(CoalesceSlot, usize), Vec<ProjLoadCandidate>> =
            BTreeMap::new();

        for (pc, inst) in block.insts.iter().enumerate() {
            match inst {
                ControlInst::StoreLocal(s, _)
                | ControlInst::FreshLocalCell(s)
                | ControlInst::DropLocal(s) => {
                    *slot_epoch.entry(CoalesceSlot::Local(*s)).or_insert(0) += 1;
                    continue;
                }
                ControlInst::StoreGlobal(g, _) => {
                    *slot_epoch.entry(CoalesceSlot::Global(*g)).or_insert(0) += 1;
                    continue;
                }
                ControlInst::MatchPattern { pattern, .. } => {
                    for idx in 0..func.n_locals {
                        let s = LocalSlot(idx);
                        if pattern_mentions_local(pattern, s) {
                            *slot_epoch.entry(CoalesceSlot::Local(s)).or_insert(0) += 1;
                        }
                    }
                    if is_main {
                        let mut pg = HashSet::new();
                        collect_pattern_globals(pattern, &mut pg);
                        for g in pg {
                            *slot_epoch.entry(CoalesceSlot::Global(g)).or_insert(0) += 1;
                        }
                    }
                    continue;
                }
                _ => {}
            }

            let (dst, cslot) = match inst {
                ControlInst::LoadLocal(d, s) => {
                    if func
                        .shared_locals
                        .get(s.0 as usize)
                        .copied()
                        .unwrap_or(false)
                        || func
                            .param_borrows
                            .get(s.0 as usize)
                            .copied()
                            .unwrap_or(false)
                    {
                        continue;
                    }
                    (*d, CoalesceSlot::Local(*s))
                }
                ControlInst::LoadGlobal(d, g) if is_main && private_main_globals.contains(g) => {
                    (*d, CoalesceSlot::Global(*g))
                }
                _ => continue,
            };
            let Some(user_pcs) = uses.get(&dst) else {
                continue;
            };
            if user_pcs.is_empty() {
                continue;
            }
            let mut projections = Vec::new();
            let mut pure = true;
            for &(ubi, u) in user_pcs {
                if ubi != bi || u == usize::MAX || u <= pc {
                    pure = false;
                    break;
                }
                match &block.insts[u] {
                    ControlInst::ProjectField { receiver, .. } if *receiver == dst => {
                        projections.push(u);
                    }
                    _ => {
                        pure = false;
                        break;
                    }
                }
            }
            if pure && !projections.is_empty() {
                let epoch = slot_epoch.get(&cslot).copied().unwrap_or(0);
                by_slot_epoch
                    .entry((cslot, epoch))
                    .or_default()
                    .push((dst, pc, projections));
            }
        }
        if by_slot_epoch.is_empty() {
            continue;
        }

        let mut drop_pcs: Vec<usize> = Vec::new();
        for ((cslot, _epoch), group) in by_slot_epoch {
            let total_projs: usize = group.iter().map(|g| g.2.len()).sum();
            if total_projs < 2 {
                continue;
            }

            let mut ordered_projs: Vec<(usize, SymId)> = Vec::new();
            for (_, _, projections) in &group {
                for &p in projections {
                    if let ControlInst::ProjectField { field, .. } = &block.insts[p] {
                        ordered_projs.push((p, *field));
                    }
                }
            }
            ordered_projs.sort_unstable_by_key(|&(p, _)| p);
            let mut distinct: Vec<SymId> = ordered_projs.iter().map(|&(_, s)| s).collect();
            distinct.sort_unstable();
            distinct.dedup();
            if distinct.len() != ordered_projs.len() {
                continue;
            }

            let first_load_pc = group[0].1;
            let last_load_pc = group.last().map(|g| g.1).unwrap_or(first_load_pc);
            let last_proj_pc = ordered_projs
                .last()
                .map(|&(p, _)| p)
                .unwrap_or(first_load_pc);
            let span_end = last_load_pc.max(last_proj_pc);
            let load_pcs: HashSet<usize> = group.iter().map(|g| g.1).collect();

            let mut conflict = false;
            for (pc, inst) in block
                .insts
                .iter()
                .enumerate()
                .take(span_end + 1)
                .skip(first_load_pc)
            {
                match cslot {
                    CoalesceSlot::Local(slot) => {
                        if pc > first_load_pc && inst_clobbers_local(inst, slot) {
                            conflict = true;
                            break;
                        }
                        if !load_pcs.contains(&pc) && inst_local_uses(inst).contains(&slot) {
                            conflict = true;
                            break;
                        }
                    }
                    CoalesceSlot::Global(gslot) => {
                        if pc > first_load_pc {
                            let pat_writes = match inst {
                                ControlInst::MatchPattern { pattern, .. } => {
                                    let mut pg = HashSet::new();
                                    collect_pattern_globals(pattern, &mut pg);
                                    pg.contains(&gslot)
                                }
                                _ => false,
                            };
                            if matches!(inst, ControlInst::StoreGlobal(g, _) if *g == gslot)
                                || pat_writes
                            {
                                conflict = true;
                                break;
                            }
                        }
                        if !load_pcs.contains(&pc)
                            && matches!(
                                inst,
                                ControlInst::LoadGlobal(_, g) | ControlInst::MoveGlobal(_, g)
                                    if *g == gslot
                            )
                        {
                            conflict = true;
                            break;
                        }
                    }
                }
            }
            if conflict {
                continue;
            }

            let keep_dst = group[0].0;
            for (_, load_pc, _) in &group[1..] {
                drop_pcs.push(*load_pc);
            }
            for &(p_pc, f_sym) in &ordered_projs {
                if let ControlInst::ProjectField { dst, .. } = &block.insts[p_pc] {
                    block.insts[p_pc] = ControlInst::ProjectField {
                        dst: *dst,
                        receiver: keep_dst,
                        field: f_sym,
                    };
                }
            }
        }

        if !drop_pcs.is_empty() {
            drop_pcs.sort_unstable();
            drop_pcs.dedup();
            for pc in drop_pcs.into_iter().rev() {
                block.insts.remove(pc);
            }
        }
    }
}

/// Rewrite still-live `LoadLocal(dst, slot)` instructions that feed exclusively borrowed
/// call/builtin/method/tail-call sites later in the same basic block into `BorrowLocal(dst, slot)`.
fn promote_borrowed_call_loads(
    func: &mut ControlFunction,
    all_funcs: &[ControlFunction],
    symbols: &SymbolTable,
) {
    let mut reg_uses: HashMap<VReg, Vec<(usize, usize)>> = HashMap::new();
    for (bi, block) in func.blocks.iter().enumerate() {
        for (pc, inst) in block.insts.iter().enumerate() {
            for r in inst.input_regs() {
                reg_uses.entry(r).or_default().push((bi, pc));
            }
        }
        for r in block.terminator.input_regs() {
            reg_uses.entry(r).or_default().push((bi, usize::MAX));
        }
    }

    for (bi, block) in func.blocks.iter_mut().enumerate() {
        let mut upgrades: Vec<(usize, VReg, LocalSlot)> = Vec::new();
        for (pc, inst) in block.insts.iter().enumerate() {
            let ControlInst::LoadLocal(dst, slot) = *inst else {
                continue;
            };
            if func
                .shared_locals
                .get(slot.0 as usize)
                .copied()
                .unwrap_or(false)
            {
                continue;
            }
            let Some(uses) = reg_uses.get(&dst) else {
                continue;
            };
            if uses.is_empty() {
                continue;
            }
            let target_pc = uses[0].1;
            if uses[0].0 != bi
                || target_pc <= pc
                || uses.iter().any(|&(ub, up)| ub != bi || up != target_pc)
            {
                continue;
            }
            let valid_consumer = if target_pc == usize::MAX {
                is_valid_borrow_terminator(&block.terminator, dst, all_funcs)
            } else {
                is_valid_borrow_inst(&block.insts[target_pc], dst, all_funcs, symbols)
            };
            if !valid_consumer {
                continue;
            }
            let end_excl = target_pc.saturating_add(1).min(block.insts.len());
            let clobbered = block.insts[pc + 1..end_excl]
                .iter()
                .any(|inner| inst_clobbers_local(inner, slot));
            if !clobbered {
                upgrades.push((pc, dst, slot));
            }
        }
        for (pc, dst, slot) in upgrades {
            block.insts[pc] = ControlInst::BorrowLocal(dst, slot);
        }
    }
}

fn apply_perceus_to_function(
    func: &mut ControlFunction,
    all_funcs: &[ControlFunction],
    symbols: &SymbolTable,
    allow_slot_moves: bool,
    is_main: bool,
    private_main_globals: &HashSet<GlobalSlot>,
    exit_globals: &HashSet<GlobalSlot>,
) {
    let rpo = func.rpo_order();
    let reachable: HashSet<BlockId> = rpo.iter().copied().collect();

    // ── Phase 0: coalesce sibling struct field projections so that the single
    //    remaining load of the struct can become an ownership transfer. This must run
    //    before Phase A, which decides which loads become transfers. ──
    if allow_slot_moves {
        coalesce_struct_projection_loads(func, is_main, private_main_globals);
    }

    // ── Phase A: LocalSlot & private-<main> GlobalSlot backward liveness ──
    if allow_slot_moves {
        let is_eligible_local = |s: LocalSlot| -> bool {
            (s.0 as usize) < (func.n_locals as usize)
                && !func
                    .shared_locals
                    .get(s.0 as usize)
                    .copied()
                    .unwrap_or(false)
                && !func
                    .param_borrows
                    .get(s.0 as usize)
                    .copied()
                    .unwrap_or(false)
        };

        let no_exit_globals: HashSet<GlobalSlot> = HashSet::new();
        let exit_seed: &HashSet<GlobalSlot> = if is_main {
            exit_globals
        } else {
            &no_exit_globals
        };
        let SlotLiveness {
            local_live_in,
            local_live_out,
            global_live_out,
            ..
        } = compute_slot_liveness(func, &is_eligible_local, private_main_globals, exit_seed);

        let preds = func.predecessors();
        for &bid in &rpo {
            let mut entry_local_drops: BTreeSet<LocalSlot> = BTreeSet::new();
            if bid != func.entry {
                for &p in &preds[bid.0 as usize] {
                    if !reachable.contains(&p) {
                        continue;
                    }
                    for &s in &local_live_out[p.0 as usize] {
                        if !local_live_in[bid.0 as usize].contains(&s) {
                            entry_local_drops.insert(s);
                        }
                    }
                }
            } else {
                for p_idx in 0..func.params.len() {
                    let s = LocalSlot(p_idx as u16);
                    if is_eligible_local(s) && !local_live_in[bid.0 as usize].contains(&s) {
                        entry_local_drops.insert(s);
                    }
                }
            }

            let mut live_l = local_live_out[bid.0 as usize].clone();
            let mut live_g: HashSet<GlobalSlot> = global_live_out[bid.0 as usize].clone();

            let block = &mut func.blocks[bid.0 as usize];
            for inst in block.insts.iter_mut().rev() {
                match inst {
                    ControlInst::StoreLocal(s, _) if is_eligible_local(*s) => {
                        live_l.remove(s);
                    }
                    ControlInst::LoadLocal(dst, s) if is_eligible_local(*s) => {
                        let d = *dst;
                        let slot = *s;
                        if !live_l.contains(&slot) {
                            *inst = ControlInst::MoveLocal(d, slot);
                        }
                        live_l.insert(slot);
                    }
                    ControlInst::StoreGlobal(g, _)
                        if is_main && private_main_globals.contains(g) =>
                    {
                        live_g.remove(g);
                    }
                    ControlInst::LoadGlobal(dst, g)
                        if is_main && private_main_globals.contains(g) =>
                    {
                        let d = *dst;
                        let slot = *g;
                        if !live_g.contains(&slot) {
                            *inst = ControlInst::MoveGlobal(d, slot);
                        }
                        live_g.insert(slot);
                    }
                    other => {
                        for s in inst_local_uses(other) {
                            if is_eligible_local(s) {
                                live_l.insert(s);
                            }
                        }
                    }
                }
            }

            if !entry_local_drops.is_empty() {
                let mut prefix: Vec<ControlInst> = entry_local_drops
                    .into_iter()
                    .map(ControlInst::DropLocal)
                    .collect();
                prefix.append(&mut block.insts);
                block.insts = prefix;
            }
        }
        promote_borrowed_call_loads(func, all_funcs, symbols);
    }

    // ── Phase B: VReg backward liveness, asymmetric branch & dead-dst `DropReg`, and KillMasks ──
    let live_in = compute_vreg_live_in(func, &reachable);
    let preds = func.predecessors();

    for &bid in &rpo {
        let mut entry_reg_drops: BTreeSet<VReg> = BTreeSet::new();
        for &param_r in &func.blocks[bid.0 as usize].params {
            if !live_in[bid.0 as usize].contains(&param_r) {
                entry_reg_drops.insert(param_r);
            }
        }
        if bid != func.entry {
            for &p in &preds[bid.0 as usize] {
                if !reachable.contains(&p) {
                    continue;
                }
                let p_term = &func.blocks[p.0 as usize].terminator;
                if let ControlTerminator::Branch {
                    then_bb,
                    then_args,
                    else_bb,
                    else_args,
                    ..
                } = p_term
                {
                    if *then_bb == bid && *else_bb != bid && reachable.contains(else_bb) {
                        let else_blk = &func.blocks[else_bb.0 as usize];
                        for &r in &live_in[else_bb.0 as usize] {
                            if !else_blk.params.contains(&r)
                                && !live_in[bid.0 as usize].contains(&r)
                            {
                                entry_reg_drops.insert(r);
                            }
                        }
                        for &r in else_args {
                            if !live_in[bid.0 as usize].contains(&r) {
                                entry_reg_drops.insert(r);
                            }
                        }
                    }
                    if *else_bb == bid && *then_bb != bid && reachable.contains(then_bb) {
                        let then_blk = &func.blocks[then_bb.0 as usize];
                        for &r in &live_in[then_bb.0 as usize] {
                            if !then_blk.params.contains(&r)
                                && !live_in[bid.0 as usize].contains(&r)
                            {
                                entry_reg_drops.insert(r);
                            }
                        }
                        for &r in then_args {
                            if !live_in[bid.0 as usize].contains(&r) {
                                entry_reg_drops.insert(r);
                            }
                        }
                    }
                }
            }
        }

        let mut live_after: HashSet<VReg> = HashSet::new();
        for succ in func.blocks[bid.0 as usize].terminator.successors() {
            if reachable.contains(&succ) {
                let sblk = &func.blocks[succ.0 as usize];
                for &r in &live_in[succ.0 as usize] {
                    if !sblk.params.contains(&r) {
                        live_after.insert(r);
                    }
                }
            }
        }
        for r in func.blocks[bid.0 as usize].terminator.input_regs() {
            live_after.insert(r);
        }

        let old_insts = std::mem::take(&mut func.blocks[bid.0 as usize].insts);
        let mut rev_out: Vec<ControlInst> =
            Vec::with_capacity(old_insts.len() + entry_reg_drops.len());
        for inst in old_insts.into_iter().rev() {
            let dead_dst = match (&inst, inst.dst()) {
                (
                    ControlInst::Const(
                        _,
                        ControlConst::Nil
                        | ControlConst::Bool(_)
                        | ControlConst::Int(_)
                        | ControlConst::Float(_),
                    ),
                    _,
                ) => None,
                (_, Some(d)) if !live_after.contains(&d) => Some(d),
                _ => None,
            };
            if let Some(d) = inst.dst() {
                live_after.remove(&d);
            }
            for r in inst.input_regs() {
                live_after.insert(r);
            }
            if let Some(d) = dead_dst {
                rev_out.push(ControlInst::DropReg(d));
            }
            rev_out.push(inst);
        }
        for r in entry_reg_drops.into_iter().rev() {
            rev_out.push(ControlInst::DropReg(r));
        }
        rev_out.reverse();
        func.blocks[bid.0 as usize].insts = rev_out;
    }

    // Recompute exact `live_in` after `DropReg` insertion and populate `inst_kill_masks` & `term_kill_mask`.
    let live_in = compute_vreg_live_in(func, &reachable);
    for &bid in &rpo {
        let mut live: HashSet<VReg> = HashSet::new();
        for succ in func.blocks[bid.0 as usize].terminator.successors() {
            if reachable.contains(&succ) {
                let sblk = &func.blocks[succ.0 as usize];
                for &r in &live_in[succ.0 as usize] {
                    if !sblk.params.contains(&r) {
                        live.insert(r);
                    }
                }
            }
        }

        let term_mask = compute_terminator_kill_mask(func, bid, &reachable, &live_in, &live);
        for r in func.blocks[bid.0 as usize].terminator.input_regs() {
            live.insert(r);
        }

        let block = &mut func.blocks[bid.0 as usize];
        block.term_kill_mask = term_mask;
        let mut masks = vec![KillMask::Empty; block.insts.len()];
        for (pc, inst) in block.insts.iter().enumerate().rev() {
            if let ControlInst::DropReg(r) = inst {
                live.remove(r);
                continue;
            }
            if let Some(d) = inst.dst() {
                live.remove(&d);
            }
            let inputs = inst.input_regs();
            let suppress_kill = matches!(
                inst,
                ControlInst::Quasiquote(..) | ControlInst::Assert(..) | ControlInst::KeepAlive(..)
            );
            let mut m = KillMask::Empty;
            for (k, &r) in inputs.iter().enumerate().rev() {
                if !suppress_kill && !live.contains(&r) {
                    m.set(k);
                }
                live.insert(r);
            }
            masks[pc] = m;
        }
        block.inst_kill_masks = masks;
    }
}
