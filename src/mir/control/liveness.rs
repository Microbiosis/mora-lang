//! P6-1 Perceus / FBIP: shared storage-slot liveness analysis.
//!
//! Single source of truth for the backward `LocalSlot` / `GlobalSlot` liveness used by BOTH
//!
//! - the Perceus ownership pass (`opt.rs`), which turns a final live read of a slot into an
//!   ownership transfer (`MoveLocal` / `MoveGlobal`), and
//! - the IR verifier (`ir.rs`), which rejects any transfer that would kill a still-live slot.
//!
//! Keeping one implementation guarantees that the optimizer and the verifier agree on exactly
//! which reads are final. A divergence would either silently drop a live value at runtime or
//! reject valid code, so both sides MUST derive liveness from this module.

use std::collections::HashSet;

use super::ir::{
    BlockId, BuiltinFnId, CalleeTarget, CaptureSource, CompiledPattern, ControlFunction,
    ControlInst, ControlSelectArm, ControlTerminator, FunctionKind, GlobalKind, GlobalSlot,
    KillMask, LocalSlot, ParallelSegment, PatternTarget, SymId, SymbolTable, VReg,
};

/// Collect the local slots a compiled pattern binds (or reads) in.
pub(crate) fn pattern_mentions_local(pat: &CompiledPattern, target_slot: LocalSlot) -> bool {
    match pat {
        CompiledPattern::Bind(PatternTarget::Local(s)) => *s == target_slot,
        CompiledPattern::Tuple(xs) | CompiledPattern::Variant { fields: xs, .. } => {
            xs.iter().any(|p| pattern_mentions_local(p, target_slot))
        }
        CompiledPattern::ListVec { elements, rest } => {
            elements
                .iter()
                .any(|p| pattern_mentions_local(p, target_slot))
                || rest
                    .as_deref()
                    .is_some_and(|r| pattern_mentions_local(r, target_slot))
        }
        CompiledPattern::ListCons { head, tail } => {
            pattern_mentions_local(head, target_slot) || pattern_mentions_local(tail, target_slot)
        }
        CompiledPattern::Dict { required, .. }
        | CompiledPattern::Struct {
            fields: required, ..
        } => required
            .iter()
            .any(|(_, p)| pattern_mentions_local(p, target_slot)),
        CompiledPattern::TypeAscription { inner, .. } => pattern_mentions_local(inner, target_slot),
        _ => false,
    }
}

/// Collect the global slots a compiled pattern binds.
pub(crate) fn collect_pattern_globals(pat: &CompiledPattern, out: &mut HashSet<GlobalSlot>) {
    match pat {
        CompiledPattern::Bind(PatternTarget::Global(g)) => {
            out.insert(*g);
        }
        CompiledPattern::Tuple(xs) | CompiledPattern::Variant { fields: xs, .. } => {
            for p in xs {
                collect_pattern_globals(p, out);
            }
        }
        CompiledPattern::ListVec { elements, rest } => {
            for p in elements {
                collect_pattern_globals(p, out);
            }
            if let Some(r) = rest {
                collect_pattern_globals(r, out);
            }
        }
        CompiledPattern::ListCons { head, tail } => {
            collect_pattern_globals(head, out);
            collect_pattern_globals(tail, out);
        }
        CompiledPattern::Dict { required, .. }
        | CompiledPattern::Struct {
            fields: required, ..
        } => {
            for (_, p) in required {
                collect_pattern_globals(p, out);
            }
        }
        CompiledPattern::TypeAscription { inner, .. } => collect_pattern_globals(inner, out),
        _ => {}
    }
}

/// True when `inst` may overwrite, clear, move out of, or rebind `slot`.
///
/// Single source of truth shared by `opt.rs` (`coalesce_struct_projection_loads` and
/// `promote_borrowed_call_loads`) and `ir.rs` (`verify_perceus_slot_moves`) so optimizer
/// and verifier agree on every instruction that invalidates a `BorrowLocal` loan or a
/// coalesced projection span.
pub(crate) fn inst_clobbers_local(inst: &ControlInst, slot: LocalSlot) -> bool {
    matches!(
        inst,
        ControlInst::StoreLocal(w, _)
            | ControlInst::MoveLocal(_, w)
            | ControlInst::FreshLocalCell(w)
            | ControlInst::DropLocal(w)
            if *w == slot
    ) || matches!(
        inst,
        ControlInst::MatchPattern { pattern, .. } if pattern_mentions_local(pattern, slot)
    )
}

/// Collect the parent local slots captured by a closure / handler / worker capture list.
pub(crate) fn collect_captured_locals(caps: &[CaptureSource], out: &mut Vec<LocalSlot>) {
    for c in caps {
        if let CaptureSource::ParentLocal(s) = *c {
            out.push(s);
        }
    }
}

/// Collect every local slot read by `inst`.
pub(crate) fn inst_local_uses(inst: &ControlInst) -> Vec<LocalSlot> {
    let mut out = Vec::new();
    match inst {
        ControlInst::LoadLocal(_, s)
        | ControlInst::BorrowLocal(_, s)
        | ControlInst::MoveLocal(_, s)
        | ControlInst::IsMissingArg(_, s) => out.push(*s),
        ControlInst::MakeClosure { captures, .. }
        | ControlInst::PushDefer { captures, .. }
        | ControlInst::NoGrad {
            body_captures: captures,
            ..
        } => collect_captured_locals(captures, &mut out),
        ControlInst::Handle {
            body_captures,
            handler_captures,
            ..
        } => {
            collect_captured_locals(body_captures, &mut out);
            collect_captured_locals(handler_captures, &mut out);
        }
        ControlInst::Transaction {
            body_captures,
            comp_captures,
            ..
        } => {
            collect_captured_locals(body_captures, &mut out);
            collect_captured_locals(comp_captures, &mut out);
        }
        ControlInst::Parallel { segments, .. } => {
            for seg in segments {
                match seg {
                    ParallelSegment::WorkerBatch(workers) => {
                        for (_, _, caps) in workers {
                            collect_captured_locals(caps, &mut out);
                        }
                    }
                    ParallelSegment::Sequential(_, caps) => {
                        collect_captured_locals(caps, &mut out);
                    }
                }
            }
        }
        ControlInst::Select { arms, .. } => {
            for arm in arms {
                let caps = match arm {
                    ControlSelectArm::Recv { captures, .. }
                    | ControlSelectArm::Send { captures, .. }
                    | ControlSelectArm::Timeout { captures, .. }
                    | ControlSelectArm::Default { captures, .. } => captures,
                };
                collect_captured_locals(caps, &mut out);
            }
        }
        // Every remaining instruction is classified explicitly (no catch-all): a future
        // `ControlInst` variant must decide here whether it touches local slots, otherwise the
        // compiler rejects this match instead of silently skipping borrow materialization.
        ControlInst::Const(..)
        | ControlInst::Copy(..)
        | ControlInst::Unary(..)
        | ControlInst::Binary(..)
        | ControlInst::ListLit(..)
        | ControlInst::DictLit(..)
        | ControlInst::Quasiquote(..)
        | ControlInst::MakeDynTrait { .. }
        | ControlInst::StoreLocal(..)
        | ControlInst::DropLocal(..)
        | ControlInst::DropReg(..)
        | ControlInst::FreshLocalCell(..)
        | ControlInst::LoadUpvalue(..)
        | ControlInst::StoreUpvalue(..)
        | ControlInst::LoadGlobal(..)
        | ControlInst::MoveGlobal(..)
        | ControlInst::StoreGlobal(..)
        | ControlInst::LoadDyn(..)
        | ControlInst::DefineDyn(..)
        | ControlInst::AssignDyn(..)
        | ControlInst::ExportGlobal(..)
        | ControlInst::ExportDyn(..)
        | ControlInst::Index(..)
        | ControlInst::IndexAssign(..)
        | ControlInst::TensorSlice { .. }
        | ControlInst::Call { .. }
        | ControlInst::CallGeneric { .. }
        | ControlInst::GetConstGeneric { .. }
        | ControlInst::ModuleCall { .. }
        | ControlInst::MethodCall { .. }
        | ControlInst::ProjectField { .. }
        | ControlInst::Pipe { .. }
        | ControlInst::MatchPattern { .. }
        | ControlInst::Spawn { .. }
        | ControlInst::TryProbe(..)
        | ControlInst::TryUnwrap(..)
        | ControlInst::Import { .. }
        | ControlInst::EvalAssert { .. }
        | ControlInst::Assert(..)
        | ControlInst::Commit
        | ControlInst::Rollback
        | ControlInst::RegisterTypeAlias { .. }
        | ControlInst::RegisterEnum { .. }
        | ControlInst::RegisterStruct { .. }
        | ControlInst::RegisterTrait { .. }
        | ControlInst::RegisterImpl { .. }
        | ControlInst::RegisterMacro { .. }
        | ControlInst::RegisterTask { .. }
        | ControlInst::RegisterKernel { .. }
        | ControlInst::KeepAlive(..) => {}
    }
    out
}

/// True when `prog` mentions the `eval` builtin, whose dynamic environment capture makes
/// storage-slot ownership transfer unsafe to infer (the evaluated code can observe any binding).
pub(crate) fn program_mentions_eval(prog: &super::ir::ControlProgram) -> bool {
    use super::ir::{BuiltinFnId, CalleeTarget, ControlConst};
    use crate::value::BuiltinKind;
    for func in &prog.functions {
        for b in &func.blocks {
            for inst in &b.insts {
                match inst {
                    ControlInst::Const(
                        _,
                        ControlConst::BuiltinFn(BuiltinFnId::Eval)
                        | ControlConst::BuiltinModule(BuiltinKind::Eval),
                    ) => return true,
                    ControlInst::Call {
                        target: CalleeTarget::Builtin(BuiltinFnId::Eval),
                        ..
                    }
                    | ControlInst::CallGeneric {
                        target: CalleeTarget::Builtin(BuiltinFnId::Eval),
                        ..
                    } => return true,
                    _ => {}
                }
            }
            if let ControlTerminator::TailCall {
                target: CalleeTarget::Builtin(BuiltinFnId::Eval),
                ..
            } = &b.terminator
            {
                return true;
            }
        }
    }
    false
}

/// Compute the `<main>`-private global slots of `prog`: plain (non-exported) variables that no
/// other function of the same program reads or writes.
///
/// Such a global is invisible outside the program, so the Perceus pass may transfer its
/// ownership out of its slot once it is dead — unless the embedding environment captures
/// program globals after `<main>` returns (see `ControlProgram::preserve_globals_at_exit`).
pub(crate) fn compute_private_main_globals(
    prog: &super::ir::ControlProgram,
) -> HashSet<GlobalSlot> {
    let mut shared_across_fns: HashSet<GlobalSlot> = HashSet::new();
    for func in &prog.functions {
        let is_main = func.id == prog.entry;
        for b in &func.blocks {
            for inst in &b.insts {
                match inst {
                    ControlInst::LoadGlobal(_, g)
                    | ControlInst::MoveGlobal(_, g)
                    | ControlInst::StoreGlobal(g, _) => {
                        if !is_main {
                            shared_across_fns.insert(*g);
                        }
                    }
                    ControlInst::ExportGlobal(g) => {
                        shared_across_fns.insert(*g);
                    }
                    ControlInst::MatchPattern { pattern, .. } if !is_main => {
                        collect_pattern_globals(pattern, &mut shared_across_fns);
                    }
                    ControlInst::RegisterTypeAlias { dst: Some(g), .. }
                    | ControlInst::RegisterEnum { dst: Some(g), .. }
                    | ControlInst::RegisterStruct { dst: Some(g), .. }
                    | ControlInst::RegisterTrait { dst: Some(g), .. }
                    | ControlInst::RegisterMacro { dst: Some(g), .. }
                    | ControlInst::RegisterTask { dst: Some(g), .. }
                    | ControlInst::RegisterKernel { dst: Some(g), .. }
                        if !is_main =>
                    {
                        shared_across_fns.insert(*g);
                    }
                    _ => {}
                }
            }
        }
    }

    let mut out = HashSet::new();
    for (idx, meta) in prog.globals.iter().enumerate() {
        let slot = GlobalSlot(idx as u16);
        if matches!(meta.kind, GlobalKind::Var)
            && !meta.exported
            && !shared_across_fns.contains(&slot)
        {
            out.insert(slot);
        }
    }
    out
}

/// Per-block backward liveness of storage slots.
#[derive(Debug, Clone)]
pub(crate) struct SlotLiveness {
    /// Live local slots at block entry.
    pub local_live_in: Vec<HashSet<LocalSlot>>,
    /// Live local slots at block exit.
    pub local_live_out: Vec<HashSet<LocalSlot>>,
    /// Live global slots at block exit.
    pub global_live_out: Vec<HashSet<GlobalSlot>>,
}

/// Compute the backward `LocalSlot` / `GlobalSlot` liveness of `func`.
///
/// - `eligible_local` selects the local slots eligible for ownership transfer (unshared slots);
/// - `tracked_globals` selects the global slots eligible for ownership transfer;
/// - `exit_globals` seeds the global liveness at function exit — the globals that must keep
///   their value after the function returns.
pub(crate) fn compute_slot_liveness(
    func: &ControlFunction,
    eligible_local: &dyn Fn(LocalSlot) -> bool,
    tracked_globals: &HashSet<GlobalSlot>,
    exit_globals: &HashSet<GlobalSlot>,
) -> SlotLiveness {
    let n_blocks = func.blocks.len();
    let rpo = func.rpo_order();
    let reachable: HashSet<BlockId> = rpo.iter().copied().collect();

    // ── LocalSlot liveness ──
    let mut local_live_in: Vec<HashSet<LocalSlot>> = vec![HashSet::new(); n_blocks];
    let mut local_live_out: Vec<HashSet<LocalSlot>> = vec![HashSet::new(); n_blocks];
    let mut changed = true;
    while changed {
        changed = false;
        for &bid in rpo.iter().rev() {
            let block = &func.blocks[bid.0 as usize];
            let mut out_set: HashSet<LocalSlot> = HashSet::new();
            for succ in block.terminator.successors() {
                if reachable.contains(&succ) {
                    out_set.extend(local_live_in[succ.0 as usize].iter().copied());
                }
            }
            let mut live = out_set.clone();
            for inst in block.insts.iter().rev() {
                // Note: `MatchPattern` only binds `PatternTarget::Local` when the pattern matches
                // (`dst_ok == true`), so it is a conditional may-def — NEVER an unconditional
                // must-kill in backward slot liveness. Only `StoreLocal` unconditionally overwrites.
                if let ControlInst::StoreLocal(s, _) = inst
                    && eligible_local(*s)
                {
                    live.remove(s);
                }
                for s in inst_local_uses(inst) {
                    if eligible_local(s) {
                        live.insert(s);
                    }
                }
            }
            if out_set != local_live_out[bid.0 as usize] || live != local_live_in[bid.0 as usize] {
                local_live_out[bid.0 as usize] = out_set;
                local_live_in[bid.0 as usize] = live;
                changed = true;
            }
        }
    }

    // ── GlobalSlot liveness ──
    let mut global_live_in: Vec<HashSet<GlobalSlot>> = vec![HashSet::new(); n_blocks];
    let mut global_live_out: Vec<HashSet<GlobalSlot>> = vec![HashSet::new(); n_blocks];
    let mut g_changed = true;
    while g_changed {
        g_changed = false;
        for &bid in rpo.iter().rev() {
            let block = &func.blocks[bid.0 as usize];
            let (mut live, out_set): (HashSet<GlobalSlot>, HashSet<GlobalSlot>) =
                match block.terminator {
                    ControlTerminator::Return(_)
                    | ControlTerminator::Halt(_)
                    | ControlTerminator::TailCall { .. } => {
                        (exit_globals.clone(), exit_globals.clone())
                    }
                    _ => {
                        let mut s = HashSet::new();
                        for succ in block.terminator.successors() {
                            if reachable.contains(&succ) {
                                s.extend(global_live_in[succ.0 as usize].iter().copied());
                            }
                        }
                        (s.clone(), s)
                    }
                };
            for inst in block.insts.iter().rev() {
                match inst {
                    ControlInst::StoreGlobal(g, _) if tracked_globals.contains(g) => {
                        live.remove(g);
                    }
                    ControlInst::LoadGlobal(_, g) | ControlInst::MoveGlobal(_, g)
                        if tracked_globals.contains(g) =>
                    {
                        live.insert(*g);
                    }
                    _ => {}
                }
            }
            if live != global_live_in[bid.0 as usize] || out_set != global_live_out[bid.0 as usize]
            {
                global_live_in[bid.0 as usize] = live;
                global_live_out[bid.0 as usize] = out_set;
                g_changed = true;
            }
        }
    }

    SlotLiveness {
        local_live_in,
        local_live_out,
        global_live_out,
    }
}

/// Live local slots immediately after instruction `pc` of `block`.
///
/// The walk starts at `pc + 1`: instruction `pc` itself is *before* the point of interest,
/// and counting it would fold its own effect (a read that makes the slot live again) into the
/// result.
pub(crate) fn local_live_after(
    block: &super::ir::BasicBlock,
    pc: usize,
    live_out: &HashSet<LocalSlot>,
    eligible_local: &dyn Fn(LocalSlot) -> bool,
) -> HashSet<LocalSlot> {
    let mut live = live_out.clone();
    for inst in block.insts[pc + 1..].iter().rev() {
        if let ControlInst::StoreLocal(s, _) = inst
            && eligible_local(*s)
        {
            live.remove(s);
        }
        for s in inst_local_uses(inst) {
            if eligible_local(s) {
                live.insert(s);
            }
        }
    }
    live
}

/// Live global slots immediately after instruction `pc` of `block`.
///
/// As with `local_live_after`, the walk starts at `pc + 1` so that instruction `pc`'s own
/// effect is excluded.
pub(crate) fn global_live_after(
    block: &super::ir::BasicBlock,
    pc: usize,
    live_out: &HashSet<GlobalSlot>,
    tracked_globals: &HashSet<GlobalSlot>,
) -> HashSet<GlobalSlot> {
    let mut live = live_out.clone();
    for inst in block.insts[pc + 1..].iter().rev() {
        match inst {
            ControlInst::StoreGlobal(g, _) if tracked_globals.contains(g) => {
                live.remove(g);
            }
            ControlInst::LoadGlobal(_, g) | ControlInst::MoveGlobal(_, g)
                if tracked_globals.contains(g) =>
            {
                live.insert(*g);
            }
            _ => {}
        }
    }
    live
}

/// Built-in functions whose arguments are strictly read-only and non-escaping.
#[inline]
pub(crate) fn is_borrowing_builtin(bid: BuiltinFnId) -> bool {
    matches!(
        bid,
        BuiltinFnId::Len
            | BuiltinFnId::TypeOf
            | BuiltinFnId::IsInstance
            | BuiltinFnId::MethodsOf
            | BuiltinFnId::Bool
            | BuiltinFnId::Int
            | BuiltinFnId::Float
            | BuiltinFnId::Str
            | BuiltinFnId::Assert
            | BuiltinFnId::AssertEq
    )
}

/// Built-in value/tensor/container methods whose receiver is strictly read-only and non-escaping.
#[inline]
pub(crate) fn is_borrowing_method_name(m: &str) -> bool {
    matches!(
        m,
        "len"
            | "ndim"
            | "rank"
            | "numel"
            | "nbytes"
            | "byte_len"
            | "dtype"
            | "device"
            | "shape"
            | "is_contiguous"
            | "is_vram_resident"
            | "requires_grad"
            | "storage_id"
            | "is_ok"
            | "is_err"
            | "starts_with"
            | "ends_with"
            | "contains"
            | "sum"
            | "mean"
            | "max"
            | "min"
            | "item"
            | "median"
            | "stddev"
            | "var"
            | "to_list"
            | "to_utf8"
            | "to_bytes"
            | "json"
            | "schema"
            | "tags"
    )
}

/// Built-in methods whose positional arguments are strictly read-only and non-escaping.
#[inline]
pub(crate) fn is_borrowing_method_arg(m: &str) -> bool {
    matches!(m, "starts_with" | "ends_with" | "contains" | "get")
}

/// Built-in mutating methods: even when a user `impl` of the same name borrows its receiver,
/// a call resolved to the built-in mutates/consumes it and must never borrow the receiver.
#[inline]
fn is_mutating_builtin_method(m: &str) -> bool {
    matches!(
        m,
        "set" | "remove" | "push" | "pop" | "fill_" | "add_" | "mul_"
    )
}

/// Methods that project a field out of their receiver without consuming it.
#[inline]
pub(crate) fn is_projecting_method(m: &str) -> bool {
    matches!(m, "get" | "first" | "last" | "keys" | "values")
}

/// Borrow summary of the matching `ImplMethod` / `TraitDefault` implementations for parameter
/// slot `slot_idx`.
///
/// `None` means no matching implementation exists, so the caller falls back to the built-in
/// name tables. `Some(all)` means every matching implementation borrows that slot; a single
/// non-borrowing implementation makes the slot non-borrowing (conservative across overloads).
///
/// `matrix` is the live fixed-point summary used while `infer_param_borrows` is still
/// iterating; `None` reads the finalized `ControlFunction::param_borrows` used by the verifier.
fn impl_slot_borrows(
    all_funcs: &[ControlFunction],
    symbols: &SymbolTable,
    method: SymId,
    arg_len: usize,
    slot_idx: usize,
    matrix: Option<&[Vec<bool>]>,
) -> Option<bool> {
    let suffix = format!("::{}", symbols.resolve(method));
    let mut found = false;
    for (idx, f) in all_funcs.iter().enumerate() {
        if !matches!(
            f.kind,
            FunctionKind::ImplMethod | FunctionKind::TraitDefault
        ) {
            continue;
        }
        let Some(nsym) = f.name else {
            continue;
        };
        if !symbols.resolve(nsym).ends_with(&suffix) {
            continue;
        }
        if 1 + arg_len > f.params.len() {
            continue;
        }
        found = true;
        let borrows = matrix
            .and_then(|m| m.get(idx))
            .and_then(|v| v.get(slot_idx))
            .copied()
            .unwrap_or_else(|| f.param_borrows.get(slot_idx).copied().unwrap_or(false));
        if !borrows {
            return Some(false);
        }
    }
    found.then_some(true)
}

/// Single source of truth: may a `MethodCall` receiver be passed as a borrowed slot?
///
/// Impl-method summaries take precedence over the built-in name tables, so a user
/// `impl Verdict { task is_ok(self): Verdict }` that returns `self` can never be mistaken for
/// the borrowing built-in `is_ok` just because the names collide.
pub(crate) fn method_receiver_borrows(
    all_funcs: &[ControlFunction],
    symbols: &SymbolTable,
    method: SymId,
    arg_len: usize,
    matrix: Option<&[Vec<bool>]>,
) -> bool {
    let mname = symbols.resolve(method);
    if is_mutating_builtin_method(mname) {
        return false;
    }
    match impl_slot_borrows(all_funcs, symbols, method, arg_len, 0, matrix) {
        Some(all) => all,
        None => is_borrowing_method_name(mname) || is_projecting_method(mname),
    }
}

/// Single source of truth: may positional argument `arg_index` of a `MethodCall` be borrowed?
pub(crate) fn method_arg_borrows(
    all_funcs: &[ControlFunction],
    symbols: &SymbolTable,
    method: SymId,
    arg_len: usize,
    arg_index: usize,
    matrix: Option<&[Vec<bool>]>,
) -> bool {
    match impl_slot_borrows(all_funcs, symbols, method, arg_len, 1 + arg_index, matrix) {
        Some(all) => all,
        None => is_borrowing_method_arg(symbols.resolve(method)),
    }
}

/// Shared single source of truth deciding whether instruction `uinst` may consume a borrowed
/// register `reg` produced by `ControlInst::BorrowLocal`.
pub(crate) fn is_valid_borrow_inst(
    uinst: &ControlInst,
    reg: VReg,
    all_funcs: &[ControlFunction],
    symbols: &SymbolTable,
) -> bool {
    match uinst {
        ControlInst::Call {
            target: CalleeTarget::Fn(cid),
            args,
            named_args,
            ..
        } => {
            let Some(callee) = all_funcs.get(cid.0 as usize) else {
                return false;
            };
            let pos_ok = args
                .iter()
                .enumerate()
                .all(|(ai, r)| *r != reg || callee.param_borrows.get(ai).copied().unwrap_or(false));
            let named_ok = named_args.iter().all(|(kw_sym, r)| {
                if *r != reg {
                    return true;
                }
                callee
                    .params
                    .iter()
                    .position(|&p| p == *kw_sym)
                    .and_then(|idx| callee.param_borrows.get(idx).copied())
                    .unwrap_or(false)
            });
            pos_ok && named_ok
        }
        ControlInst::Call {
            target: CalleeTarget::Builtin(bid),
            named_args,
            ..
        } => named_args.is_empty() && is_borrowing_builtin(*bid),
        ControlInst::MethodCall {
            receiver,
            method,
            args,
            named_args,
            ..
        } => {
            if !named_args.is_empty() {
                return false;
            }
            if *receiver == reg
                && !method_receiver_borrows(all_funcs, symbols, *method, args.len(), None)
            {
                return false;
            }
            for (ai, r) in args.iter().enumerate() {
                if *r == reg
                    && !method_arg_borrows(all_funcs, symbols, *method, args.len(), ai, None)
                {
                    return false;
                }
            }
            true
        }
        _ => false,
    }
}

/// Shared single source of truth deciding whether terminator `term` may consume a borrowed
/// register `reg` produced by `ControlInst::BorrowLocal`.
pub(crate) fn is_valid_borrow_terminator(
    term: &ControlTerminator,
    reg: VReg,
    all_funcs: &[ControlFunction],
) -> bool {
    match term {
        ControlTerminator::TailCall {
            target: CalleeTarget::Fn(cid),
            args,
        } => {
            let Some(callee) = all_funcs.get(cid.0 as usize) else {
                return false;
            };
            args.iter()
                .enumerate()
                .all(|(ai, r)| *r != reg || callee.param_borrows.get(ai).copied().unwrap_or(false))
        }
        _ => false,
    }
}

/// Single shared backward `VReg` liveness fixed-point analysis used by BOTH the Perceus pass
/// (`opt.rs`) and the IR soundness verifier (`ir.rs`).
pub(crate) fn compute_vreg_live_in(
    func: &ControlFunction,
    reachable: &HashSet<BlockId>,
) -> Vec<HashSet<VReg>> {
    let rpo = func.rpo_order();
    let mut live_in: Vec<HashSet<VReg>> = vec![HashSet::new(); func.blocks.len()];
    let mut changed = true;
    while changed {
        changed = false;
        for &bid in rpo.iter().rev() {
            let block = &func.blocks[bid.0 as usize];
            let mut live: HashSet<VReg> = HashSet::new();
            for succ in block.terminator.successors() {
                if reachable.contains(&succ) {
                    let sblk = &func.blocks[succ.0 as usize];
                    for &r in &live_in[succ.0 as usize] {
                        if !sblk.params.contains(&r) {
                            live.insert(r);
                        }
                    }
                }
            }
            for r in block.terminator.input_regs() {
                live.insert(r);
            }
            for inst in block.insts.iter().rev() {
                if let ControlInst::DropReg(r) = inst {
                    live.remove(r);
                    continue;
                }
                if let Some(d) = inst.dst() {
                    live.remove(&d);
                }
                for r in inst.input_regs() {
                    live.insert(r);
                }
            }
            if live != live_in[bid.0 as usize] {
                live_in[bid.0 as usize] = live;
                changed = true;
            }
        }
    }
    live_in
}

/// Compute the exact arbitrary-arity [`KillMask`] for `func.blocks[bid].terminator` from the
/// shared `live_in` sets and `live_after` (registers live across outgoing edges).
pub(crate) fn compute_terminator_kill_mask(
    func: &ControlFunction,
    bid: BlockId,
    reachable: &HashSet<BlockId>,
    live_in: &[HashSet<VReg>],
    live_after: &HashSet<VReg>,
) -> KillMask {
    let mut mask = KillMask::Empty;
    match &func.blocks[bid.0 as usize].terminator {
        ControlTerminator::Branch {
            cond,
            then_bb,
            then_args,
            else_bb,
            else_args,
        } => {
            let then_blk = &func.blocks[then_bb.0 as usize];
            let else_blk = &func.blocks[else_bb.0 as usize];
            let live_then = |r: VReg| {
                reachable.contains(then_bb)
                    && live_in[then_bb.0 as usize].contains(&r)
                    && !then_blk.params.contains(&r)
            };
            let live_else = |r: VReg| {
                reachable.contains(else_bb)
                    && live_in[else_bb.0 as usize].contains(&r)
                    && !else_blk.params.contains(&r)
            };
            if !then_args.contains(cond)
                && !else_args.contains(cond)
                && !live_then(*cond)
                && !live_else(*cond)
            {
                mask.set(0);
            }
            for (i, &r) in then_args.iter().enumerate() {
                if !then_args[i + 1..].contains(&r) && !live_then(r) {
                    mask.set(1 + i);
                }
            }
            for (i, &r) in else_args.iter().enumerate() {
                if !else_args[i + 1..].contains(&r) && !live_else(r) {
                    mask.set(1 + then_args.len() + i);
                }
            }
        }
        other => {
            let t_inputs = other.input_regs();
            for (i, &r) in t_inputs.iter().enumerate() {
                if !t_inputs[i + 1..].contains(&r) && !live_after.contains(&r) {
                    mask.set(i);
                }
            }
        }
    }
    mask
}
