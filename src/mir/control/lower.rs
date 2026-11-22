//! v0.110.0: Direct `MirWitness[] -> ControlProgram` lowering.
//!
//! Compiles `ParserV3`'s syntax witness tree directly into explicit-CFG SSA
//! [`ControlProgram`] with:
//! - Structured `BasicBlock`s + `BlockParam`s + `ControlTerminator`s (zero jump
//!   patching, zero fake `Label`/`JumpExit` sentinel instructions)
//! - Pre-resolved lexical slots (`LocalSlot`), captured closure/handler upvalues
//!   (`UpvalueSlot` with automatic shared-cell promotion on cross-boundary mutation),
//!   module-level globals (`GlobalSlot`), and compile-time `BuiltinFnId`s
//! - Exhaustive lowering of every [`WitnessKind`] variant with compile-time
//!   structural verification via [`verify_program`].

use std::collections::{HashMap, HashSet};

use crate::common::Literal;
use crate::error::{MoraError, MoraResult};
use crate::mir::witness::{
    BuiltinOp, MirWitness, WitnessCallee, WitnessKind, WitnessParam, WitnessPattern,
    WitnessSelectArm, WitnessSliceAxis, WitnessSpawnTarget,
};
use crate::value::BuiltinKind;

use super::ir::{
    BasicBlock, BlockId, BuiltinFnId, CalleeTarget, CaptureSource, CompiledPattern, ControlConst,
    ControlFunction, ControlImplMethod, ControlInst, ControlProgram, ControlQqSegment,
    ControlSelectArm, ControlSliceAxis, ControlSpawnTarget, ControlStructField, ControlTerminator,
    ControlTraitMethod, FnId, FunctionKind, GlobalKind, GlobalMeta, GlobalSlot, LocalSlot,
    ParallelSegment, PatternTarget, SymId, SymbolTable, UnaryOp, UpvalueDesc, UpvalueSlot, VReg,
    verify_program,
};

/// Outcome of lowering a statement/expression inside the current basic block.
enum FlowState {
    /// Control continues linearly at `cur_bb` yielding SSA register `reg`.
    Active(VReg),
    /// Current basic block has already terminated (`Return`/`Break`/`Continue`/`TailCall`).
    Terminated,
}

/// Active loop targets stored on the per-function loop stack.
#[derive(Clone, Copy)]
struct LoopTargets {
    continue_bb: BlockId,
    break_bb: BlockId,
}

/// Module-wide lowering state shared across `<main>` and all nested functions.
struct ProgramLowerer {
    symbols: SymbolTable,
    functions: Vec<ControlFunction>,
    globals: Vec<GlobalMeta>,
    global_by_name: HashMap<String, GlobalSlot>,
    dyn_names: HashSet<String>,
    /// `(parent_fn, child_fn)` edges recorded to propagate `shared = true` flags
    /// uniformly after all functions finish lowering.
    parent_edges: Vec<(FnId, FnId)>,
    /// Stage 3C: Parameter signature `(name, has_default)` of pre-scanned top-level tasks
    /// for compile-time static keyword reordering and default argument slot filling.
    task_param_specs: HashMap<FnId, Vec<(String, bool)>>,
    /// P4-1A: Nominal struct generic metadata `(type_params, const_params)` for lowering
    /// explicit const-generic calls (`Linear<4, 3>(...)`) and `impl` const-param prologues.
    struct_const_meta: HashMap<String, (Vec<String>, Vec<String>)>,
}

impl ProgramLowerer {
    fn new(dyn_names: &[String]) -> Self {
        Self {
            symbols: SymbolTable::new(),
            functions: Vec::new(),
            globals: Vec::new(),
            global_by_name: HashMap::new(),
            dyn_names: dyn_names.iter().cloned().collect(),
            parent_edges: Vec::new(),
            task_param_specs: HashMap::new(),
            struct_const_meta: HashMap::new(),
        }
    }

    fn intern(&mut self, name: &str) -> SymId {
        self.symbols.intern(name)
    }

    fn reserve_fn(&mut self) -> FnId {
        let id = FnId(self.functions.len() as u32);
        self.functions.push(ControlFunction {
            id,
            name: None,
            kind: FunctionKind::Closure,
            params: Vec::new(),
            param_has_default: Vec::new(),
            upvalues: Vec::new(),
            shared_locals: Vec::new(),
            local_names: Vec::new(),
            n_locals: 0,
            n_regs: 0,
            blocks: Vec::new(),
            entry: BlockId(0),
            handler_k_slot: None,
            handler_arg_slots: Vec::new(),
        });
        id
    }

    fn alloc_global(&mut self, name: &str, kind: GlobalKind) -> GlobalSlot {
        if let Some(&slot) = self.global_by_name.get(name) {
            // Upgrade `Var` placeholder if declaration specifies a concrete kind.
            if !matches!(kind, GlobalKind::Var) {
                self.globals[slot.0 as usize].kind = kind;
            }
            return slot;
        }
        let sym = self.intern(name);
        let slot = GlobalSlot(self.globals.len() as u16);
        self.globals.push(GlobalMeta {
            sym,
            name: name.to_string(),
            exported: false,
            kind,
        });
        self.global_by_name.insert(name.to_string(), slot);
        slot
    }

    /// Pre-scan module top-level witnesses so forward task calls, mutual
    /// recursion, and cross-statement globals resolve to static `GlobalSlot`s.
    fn pre_scan_top_level(&mut self, witnesses: &[MirWitness]) {
        for w in witnesses {
            self.pre_scan_stmt(w);
        }
    }

    fn pre_scan_stmt(&mut self, w: &MirWitness) {
        match &w.kind {
            WitnessKind::FnDef { name, params, .. } => {
                if let Some(&existing) = self.global_by_name.get(name)
                    && let GlobalKind::Task(fn_id) = self.globals[existing.0 as usize].kind
                {
                    let specs = params
                        .iter()
                        .map(|p| (p.name.clone(), p.default.is_some()))
                        .collect();
                    self.task_param_specs.insert(fn_id, specs);
                    return;
                }
                let fn_id = self.reserve_fn();
                let specs = params
                    .iter()
                    .map(|p| (p.name.clone(), p.default.is_some()))
                    .collect();
                self.task_param_specs.insert(fn_id, specs);
                self.alloc_global(name, GlobalKind::Task(fn_id));
            }
            WitnessKind::MacroDef { name, .. } => {
                if let Some(&existing) = self.global_by_name.get(name)
                    && let GlobalKind::Macro(_) = self.globals[existing.0 as usize].kind
                {
                    return;
                }
                let fn_id = self.reserve_fn();
                self.alloc_global(name, GlobalKind::Macro(fn_id));
            }
            WitnessKind::EnumDef {
                name,
                type_params,
                const_params,
                ..
            } => {
                self.struct_const_meta
                    .insert(name.clone(), (type_params.clone(), const_params.clone()));
                self.alloc_global(name, GlobalKind::Enum);
            }
            WitnessKind::StructDef {
                name,
                type_params,
                const_params,
                ..
            } => {
                self.struct_const_meta
                    .insert(name.clone(), (type_params.clone(), const_params.clone()));
                self.alloc_global(name, GlobalKind::Struct);
            }
            WitnessKind::KernelDef { name, .. } => {
                self.alloc_global(name, GlobalKind::Kernel);
            }
            WitnessKind::TraitDef { name, .. } => {
                self.alloc_global(name, GlobalKind::Trait);
            }
            WitnessKind::TypeAlias { name, .. } => {
                self.alloc_global(name, GlobalKind::TypeAlias);
            }
            WitnessKind::LetBinding { name, .. } => {
                self.alloc_global(name, GlobalKind::Var);
            }
            WitnessKind::Export { names, decl } => {
                self.pre_scan_stmt(decl);
                for name in names {
                    let slot = self.alloc_global(name, GlobalKind::Var);
                    self.globals[slot.0 as usize].exported = true;
                }
            }
            WitnessKind::Sequence(items) => {
                for item in items {
                    self.pre_scan_stmt(item);
                }
            }
            WitnessKind::Parallel { body }
            | WitnessKind::Worker { body, .. }
            | WitnessKind::Transaction { body, .. } => {
                self.pre_scan_stmt(body);
            }
            WitnessKind::If { then, r#else, .. } => {
                self.pre_scan_stmt(then);
                if let Some(e) = r#else {
                    self.pre_scan_stmt(e);
                }
            }
            WitnessKind::While { body, .. } | WitnessKind::Loop { body, .. } => {
                self.pre_scan_stmt(body);
            }
            _ => {}
        }
    }

    /// Propagate shared-cell promotion bidirectionally across parent/child capture edges
    /// until reaching a fixed point.
    fn propagate_shared_upvalues(&mut self) {
        let mut changed = true;
        while changed {
            changed = false;
            for &(parent_id, child_id) in &self.parent_edges {
                let child_upvalues = self.functions[child_id.0 as usize].upvalues.clone();
                let mut parent_mutated = false;

                for desc in &child_upvalues {
                    if !desc.shared {
                        continue;
                    }
                    match desc.source {
                        CaptureSource::ParentLocal(slot) => {
                            let s = slot.0 as usize;
                            if !self.functions[parent_id.0 as usize].shared_locals[s] {
                                self.functions[parent_id.0 as usize].shared_locals[s] = true;
                                parent_mutated = true;
                            }
                        }
                        CaptureSource::ParentUpvalue(pu) => {
                            let idx = pu.0 as usize;
                            if !self.functions[parent_id.0 as usize].upvalues[idx].shared {
                                self.functions[parent_id.0 as usize].upvalues[idx].shared = true;
                                parent_mutated = true;
                            }
                        }
                    }
                }
                if parent_mutated {
                    changed = true;
                }

                // Downward pass: if parent slot/upvalue is shared, child upvalue must share too.
                let mut updated = false;
                let parent_shared_locals =
                    self.functions[parent_id.0 as usize].shared_locals.clone();
                let parent_upvalues = self.functions[parent_id.0 as usize].upvalues.clone();
                let child = &mut self.functions[child_id.0 as usize];
                for desc in &mut child.upvalues {
                    let is_shared = match desc.source {
                        CaptureSource::ParentLocal(slot) => parent_shared_locals[slot.0 as usize],
                        CaptureSource::ParentUpvalue(pu) => parent_upvalues[pu.0 as usize].shared,
                    };
                    if is_shared && !desc.shared {
                        desc.shared = true;
                        updated = true;
                    }
                }
                if updated {
                    changed = true;
                }
            }
        }
    }
}

/// Variable location resolved inside a single `FuncScope`.
#[derive(Clone, Copy)]
enum VarBinding {
    Local(LocalSlot),
    Upvalue(UpvalueSlot),
    Global(GlobalSlot),
    Dyn(SymId),
}

/// Per-function compilation state managing basic blocks, SSA VRegs, local slots, and upvalues.
struct FuncScope {
    fn_id: FnId,
    kind: FunctionKind,
    name_sym: Option<SymId>,
    param_syms: Vec<SymId>,
    param_has_default: Vec<bool>,
    blocks: Vec<BasicBlock>,
    cur_bb: BlockId,
    next_reg: u32,
    /// Lexical local symbol scopes: scope 0 holds function params and top/flat
    /// function `let` bindings; higher scopes hold block-local `for` / `match` bindings.
    local_scopes: Vec<HashMap<String, LocalSlot>>,
    local_names: Vec<SymId>,
    shared_locals: Vec<bool>,
    /// Tracks locals that have already been captured by an earlier closure, so
    /// a straight-line re-`let` creates a fresh shadowing slot without breaking
    /// the prior immutable capture.
    captured_local_slots: HashSet<LocalSlot>,
    upvalues: Vec<UpvalueDesc>,
    upvalue_by_name: HashMap<String, UpvalueSlot>,
    loop_stack: Vec<LoopTargets>,
    loop_depth: usize,
    branch_depth: usize,
    handler_k_slot: Option<LocalSlot>,
    handler_arg_slots: Vec<Option<LocalSlot>>,
}

impl FuncScope {
    fn new(
        fn_id: FnId,
        kind: FunctionKind,
        name_sym: Option<SymId>,
        params: &[String],
        prog: &mut ProgramLowerer,
    ) -> Self {
        let mut scope = Self {
            fn_id,
            kind,
            name_sym,
            param_syms: Vec::with_capacity(params.len()),
            param_has_default: vec![false; params.len()],
            blocks: vec![BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                insts: Vec::new(),
                terminator: ControlTerminator::Unreachable,
            }],
            cur_bb: BlockId(0),
            next_reg: 0,
            local_scopes: vec![HashMap::new()],
            local_names: Vec::new(),
            shared_locals: Vec::new(),
            captured_local_slots: HashSet::new(),
            upvalues: Vec::new(),
            upvalue_by_name: HashMap::new(),
            loop_stack: Vec::new(),
            loop_depth: 0,
            branch_depth: 0,
            handler_k_slot: None,
            handler_arg_slots: Vec::new(),
        };
        for p in params {
            let sym = prog.intern(p);
            scope.param_syms.push(sym);
            let slot = scope.alloc_local_slot(sym);
            scope.local_scopes[0].insert(p.clone(), slot);
        }
        scope
    }

    fn alloc_reg(&mut self) -> VReg {
        let r = VReg(self.next_reg);
        self.next_reg += 1;
        r
    }

    fn alloc_local_slot(&mut self, sym: SymId) -> LocalSlot {
        let slot = LocalSlot(self.local_names.len() as u16);
        self.local_names.push(sym);
        self.shared_locals.push(false);
        slot
    }

    fn new_block(&mut self, n_params: usize) -> (BlockId, Vec<VReg>) {
        let id = BlockId(self.blocks.len() as u32);
        let mut params = Vec::with_capacity(n_params);
        for _ in 0..n_params {
            params.push(self.alloc_reg());
        }
        self.blocks.push(BasicBlock {
            id,
            params: params.clone(),
            insts: Vec::new(),
            terminator: ControlTerminator::Unreachable,
        });
        (id, params)
    }

    fn switch_to(&mut self, bb: BlockId) {
        self.cur_bb = bb;
    }

    fn emit(&mut self, inst: ControlInst) {
        self.blocks[self.cur_bb.0 as usize].insts.push(inst);
    }

    fn terminate(&mut self, term: ControlTerminator) {
        self.blocks[self.cur_bb.0 as usize].terminator = term;
    }

    fn push_lexical_scope(&mut self) {
        self.local_scopes.push(HashMap::new());
    }

    fn pop_lexical_scope(&mut self) {
        if self.local_scopes.len() > 1 {
            self.local_scopes.pop();
        }
    }

    fn lookup_local(&self, name: &str) -> Option<LocalSlot> {
        for map in self.local_scopes.iter().rev() {
            if let Some(&slot) = map.get(name) {
                return Some(slot);
            }
        }
        None
    }

    /// Bind `name` for `let name = ...` in a non-`Main` function scope.
    fn bind_let_local(&mut self, name: &str, prog: &mut ProgramLowerer) -> LocalSlot {
        // Inside inner lexical scope (match arm / for body): if already declared in top
        // function scope, re-let in loops/branches rebinds that variable (preserving
        // `for x in ... { let total = total + x }`), unless a straight-line closure
        // captured it earlier.
        if let Some(&slot) = self.local_scopes[0].get(name) {
            if self.loop_depth == 0
                && self.branch_depth == 0
                && self.captured_local_slots.contains(&slot)
            {
                let sym = prog.intern(name);
                let fresh = self.alloc_local_slot(sym);
                self.local_scopes[0].insert(name.to_string(), fresh);
                return fresh;
            }
            return slot;
        }
        let sym = prog.intern(name);
        let slot = self.alloc_local_slot(sym);
        self.local_scopes[0].insert(name.to_string(), slot);
        slot
    }

    fn bind_block_local(&mut self, name: &str, prog: &mut ProgramLowerer) -> LocalSlot {
        let sym = prog.intern(name);
        let slot = self.alloc_local_slot(sym);
        if let Some(top) = self.local_scopes.last_mut() {
            top.insert(name.to_string(), slot);
        }
        slot
    }

    fn emit_const(&mut self, c: ControlConst) -> VReg {
        let dst = self.alloc_reg();
        self.emit(ControlInst::Const(dst, c));
        dst
    }

    fn emit_nil(&mut self) -> VReg {
        self.emit_const(ControlConst::Nil)
    }

    fn finish(self) -> ControlFunction {
        ControlFunction {
            id: self.fn_id,
            name: self.name_sym,
            kind: self.kind,
            params: self.param_syms,
            param_has_default: self.param_has_default,
            upvalues: self.upvalues,
            shared_locals: self.shared_locals,
            local_names: self.local_names.clone(),
            n_locals: self.local_names.len() as u16,
            n_regs: self.next_reg,
            blocks: self.blocks,
            entry: BlockId(0),
            handler_k_slot: self.handler_k_slot,
            handler_arg_slots: self.handler_arg_slots,
        }
    }
}

/// Lower a compiled program's syntax witnesses into verified canonical Control SSA IR.
pub fn lower_program(witnesses: &[MirWitness]) -> MoraResult<ControlProgram> {
    lower_program_with_dyn_globals(witnesses, &[])
}

/// Lower a compiled program with an explicit list of pre-existing dynamic/REPL global
/// symbol names so incremental chunks resolve them via `LoadDyn`/`AssignDyn`.
pub fn lower_program_with_dyn_globals(
    witnesses: &[MirWitness],
    dyn_names: &[String],
) -> MoraResult<ControlProgram> {
    let mut prog = ProgramLowerer::new(dyn_names);
    let main_id = prog.reserve_fn();
    let main_sym = prog.intern("<main>");
    prog.pre_scan_top_level(witnesses);

    let mut stack = ScopeStack::new(FuncScope::new(
        main_id,
        FunctionKind::Main,
        Some(main_sym),
        &[],
        &mut prog,
    ));

    match lower_sequence_slice(&mut stack, &mut prog, witnesses, true)? {
        FlowState::Active(r) => stack
            .current_mut()
            .terminate(ControlTerminator::Return(Some(r))),
        FlowState::Terminated => {}
    }

    let scope = stack.frames.pop().expect("main frame");
    prog.functions[main_id.0 as usize] = scope.finish();
    prog.propagate_shared_upvalues();

    let out = ControlProgram {
        symbols: prog.symbols,
        functions: prog.functions,
        entry: main_id,
        globals: prog.globals,
    };
    verify_program(&out)?;
    Ok(out)
}

/// Resolve a free variable `name` inside `scope` against `scope` locals, existing
/// upvalues, ancestor function frames, module static globals, or dynamic globals.
fn resolve_var_binding(
    prog: &mut ProgramLowerer,
    scopes_above: &mut [FuncScope],
    scope: &mut FuncScope,
    name: &str,
    mark_mutated: bool,
) -> Option<VarBinding> {
    if let Some(slot) = scope.lookup_local(name) {
        return Some(VarBinding::Local(slot));
    }
    if let Some(&u) = scope.upvalue_by_name.get(name) {
        if mark_mutated {
            scope.upvalues[u.0 as usize].shared = true;
        }
        return Some(VarBinding::Upvalue(u));
    }
    if let Some(u_slot) = try_capture_from_chain(prog, scopes_above, scope, name, mark_mutated) {
        return Some(VarBinding::Upvalue(u_slot));
    }
    if let Some(&g_slot) = prog.global_by_name.get(name) {
        return Some(VarBinding::Global(g_slot));
    }
    if prog.dyn_names.contains(name) {
        let sym = prog.intern(name);
        return Some(VarBinding::Dyn(sym));
    }
    None
}

/// Capture `name` from `scopes_above` into `scope`, threading intermediate closures.
fn try_capture_from_chain(
    prog: &mut ProgramLowerer,
    scopes_above: &mut [FuncScope],
    scope: &mut FuncScope,
    name: &str,
    mark_mutated: bool,
) -> Option<UpvalueSlot> {
    let mut origin_idx: Option<usize> = None;
    for (idx, s) in scopes_above.iter().enumerate().rev() {
        if s.lookup_local(name).is_some() || s.upvalue_by_name.contains_key(name) {
            origin_idx = Some(idx);
            break;
        }
    }
    let origin = origin_idx?;
    let sym = prog.intern(name);

    let (mut src_in_prev, mut shared) =
        if let Some(&u) = scopes_above[origin].upvalue_by_name.get(name) {
            if mark_mutated {
                scopes_above[origin].upvalues[u.0 as usize].shared = true;
            }
            let sh = scopes_above[origin].upvalues[u.0 as usize].shared;
            (CaptureSource::ParentUpvalue(u), sh)
        } else {
            let slot = scopes_above[origin].lookup_local(name)?;
            scopes_above[origin].captured_local_slots.insert(slot);
            if mark_mutated {
                scopes_above[origin].shared_locals[slot.0 as usize] = true;
            }
            let sh = scopes_above[origin].shared_locals[slot.0 as usize];
            (CaptureSource::ParentLocal(slot), sh)
        };

    if mark_mutated {
        shared = true;
    }

    for mid in &mut scopes_above[origin + 1..] {
        if let Some(&existing) = mid.upvalue_by_name.get(name) {
            if shared {
                mid.upvalues[existing.0 as usize].shared = true;
            }
            src_in_prev = CaptureSource::ParentUpvalue(existing);
            continue;
        }
        let u_slot = UpvalueSlot(mid.upvalues.len() as u16);
        mid.upvalues.push(UpvalueDesc {
            sym,
            source: src_in_prev,
            shared,
        });
        mid.upvalue_by_name.insert(name.to_string(), u_slot);
        src_in_prev = CaptureSource::ParentUpvalue(u_slot);
    }

    let u_slot = UpvalueSlot(scope.upvalues.len() as u16);
    scope.upvalues.push(UpvalueDesc {
        sym,
        source: src_in_prev,
        shared,
    });
    scope.upvalue_by_name.insert(name.to_string(), u_slot);
    Some(u_slot)
}

fn emit_load_binding(scope: &mut FuncScope, binding: VarBinding) -> VReg {
    let dst = scope.alloc_reg();
    match binding {
        VarBinding::Local(slot) => scope.emit(ControlInst::LoadLocal(dst, slot)),
        VarBinding::Upvalue(slot) => scope.emit(ControlInst::LoadUpvalue(dst, slot)),
        VarBinding::Global(slot) => scope.emit(ControlInst::LoadGlobal(dst, slot)),
        VarBinding::Dyn(sym) => scope.emit(ControlInst::LoadDyn(dst, sym)),
    }
    dst
}

fn emit_store_binding(scope: &mut FuncScope, binding: VarBinding, val: VReg) {
    match binding {
        VarBinding::Local(slot) => scope.emit(ControlInst::StoreLocal(slot, val)),
        VarBinding::Upvalue(slot) => scope.emit(ControlInst::StoreUpvalue(slot, val)),
        VarBinding::Global(slot) => scope.emit(ControlInst::StoreGlobal(slot, val)),
        VarBinding::Dyn(sym) => scope.emit(ControlInst::AssignDyn(sym, val)),
    }
}

/// Clean safe lexical stack for arbitrary nesting of `FuncScope`s during lowering.
struct ScopeStack {
    frames: Vec<FuncScope>,
}

impl ScopeStack {
    fn new(main: FuncScope) -> Self {
        Self { frames: vec![main] }
    }

    fn current_mut(&mut self) -> &mut FuncScope {
        self.frames.last_mut().expect("ScopeStack non-empty")
    }

    /// True when the active frame is `FunctionKind::Main` or a module-level transparent
    /// execution scope (`Worker`, `ParallelSeq`, `TransactionBody`) nested under `Main`
    /// with no enclosing user function/closure boundary.
    fn is_module_top_level(&self) -> bool {
        for frame in self.frames.iter().rev() {
            match frame.kind {
                FunctionKind::Main => return true,
                FunctionKind::Worker
                | FunctionKind::ParallelSeq
                | FunctionKind::TransactionBody => continue,
                _ => return false,
            }
        }
        false
    }

    fn resolve_binding(
        &mut self,
        prog: &mut ProgramLowerer,
        name: &str,
        mark_mutated: bool,
    ) -> Option<VarBinding> {
        let len = self.frames.len();
        debug_assert!(len >= 1);
        let (parents, cur_slice) = self.frames.split_at_mut(len - 1);
        resolve_var_binding(prog, parents, &mut cur_slice[0], name, mark_mutated)
    }

    /// Resolve a variable load, automatically marking mutated outer locals if `Assign` exists
    /// and falling back to built-in values (`print`, `str`, `math`, ...) or `DynGlobal`.
    fn emit_read_var(&mut self, prog: &mut ProgramLowerer, name: &str) -> VReg {
        if let Some(binding) = self.resolve_binding(prog, name, false) {
            return emit_load_binding(self.current_mut(), binding);
        }
        if let Some(bfn) = BuiltinFnId::from_name(name) {
            return self.current_mut().emit_const(ControlConst::BuiltinFn(bfn));
        }
        if let Some(kind) = module_object_by_name(name) {
            return self
                .current_mut()
                .emit_const(ControlConst::BuiltinModule(kind));
        }
        let sym = prog.intern(name);
        let dst = self.current_mut().alloc_reg();
        self.current_mut().emit(ControlInst::LoadDyn(dst, sym));
        dst
    }

    /// Lower a nested callable whose parameters may carry default initializer expressions.
    /// Emits `IsMissingArg` entry prologue branches so omitted/named-arg defaults evaluate
    /// lazily in the callee's own lexical scope.
    #[allow(clippy::too_many_arguments)]
    fn lower_child_fn_with_params(
        &mut self,
        prog: &mut ProgramLowerer,
        existing_fn_id: Option<FnId>,
        kind: FunctionKind,
        name: Option<&str>,
        params: &[WitnessParam],
        body: &MirWitness,
        handler_setup: Option<(&str, usize)>,
        capture_parent: bool,
    ) -> MoraResult<(FnId, Vec<CaptureSource>)> {
        self.lower_child_fn_with_params_and_consts(
            prog,
            existing_fn_id,
            kind,
            name,
            params,
            &[],
            false,
            body,
            handler_setup,
            capture_parent,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_child_fn_with_params_and_consts(
        &mut self,
        prog: &mut ProgramLowerer,
        existing_fn_id: Option<FnId>,
        kind: FunctionKind,
        name: Option<&str>,
        params: &[WitnessParam],
        const_params: &[String],
        has_self: bool,
        body: &MirWitness,
        handler_setup: Option<(&str, usize)>,
        capture_parent: bool,
    ) -> MoraResult<(FnId, Vec<CaptureSource>)> {
        let fn_id = match existing_fn_id {
            Some(id) => id,
            None => prog.reserve_fn(),
        };
        let parent_id = self.current_mut().fn_id;
        let name_sym = name.map(|n| prog.intern(n));
        let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
        let mut child = FuncScope::new(fn_id, kind, name_sym, &param_names, prog);
        child.param_has_default = params.iter().map(|p| p.default.is_some()).collect();

        if let Some((k_param, arg_count)) = handler_setup {
            let arg_slots: Vec<Option<LocalSlot>> = (0..arg_count)
                .map(|i| {
                    let arg_name = format!("__arg{i}");
                    let slot = child.bind_let_local(&arg_name, prog);
                    Some(slot)
                })
                .collect();
            child.handler_arg_slots = arg_slots;
            if !k_param.is_empty() && uses_identifier(body, k_param) {
                let k_slot = child.bind_let_local(k_param, prog);
                child.handler_k_slot = Some(k_slot);
            }
        }

        let emit_default_prologue = |stack: &mut ScopeStack,
                                     prog: &mut ProgramLowerer|
         -> MoraResult<bool> {
            // P4-1A: Bind const-generic parameters from `self` (`LocalSlot(0)`) into real locals.
            if has_self && !const_params.is_empty() {
                let r_self = stack.current_mut().alloc_reg();
                stack
                    .current_mut()
                    .emit(ControlInst::LoadLocal(r_self, LocalSlot(0)));
                for cname in const_params {
                    let cslot = stack.current_mut().bind_let_local(cname, prog);
                    let r_val = stack.current_mut().alloc_reg();
                    let csym = prog.intern(cname);
                    stack.current_mut().emit(ControlInst::GetConstGeneric {
                        dst: r_val,
                        src: r_self,
                        name: csym,
                    });
                    stack
                        .current_mut()
                        .emit(ControlInst::StoreLocal(cslot, r_val));
                }
            }
            for (idx, wp) in params.iter().enumerate() {
                if let Some(default_w) = &wp.default {
                    let slot = LocalSlot(idx as u16);
                    let is_missing = stack.current_mut().alloc_reg();
                    stack
                        .current_mut()
                        .emit(ControlInst::IsMissingArg(is_missing, slot));
                    let (eval_bb, _) = stack.current_mut().new_block(0);
                    let (cont_bb, _) = stack.current_mut().new_block(0);
                    stack.current_mut().terminate(ControlTerminator::Branch {
                        cond: is_missing,
                        then_bb: eval_bb,
                        then_args: Vec::new(),
                        else_bb: cont_bb,
                        else_args: Vec::new(),
                    });
                    stack.current_mut().switch_to(eval_bb);
                    if let FlowState::Active(def_reg) = lower_expr(stack, prog, default_w, false)? {
                        stack
                            .current_mut()
                            .emit(ControlInst::StoreLocal(slot, def_reg));
                        stack.current_mut().terminate(ControlTerminator::Jump {
                            target: cont_bb,
                            args: Vec::new(),
                        });
                    }
                    stack.current_mut().switch_to(cont_bb);
                }
            }
            Ok(true)
        };

        if !capture_parent {
            let saved = std::mem::take(&mut self.frames);
            self.frames.push(child);
            if let Err(e) = emit_default_prologue(self, prog) {
                self.frames = saved;
                return Err(e);
            }
            match lower_expr(self, prog, body, true) {
                Ok(FlowState::Active(r)) => self
                    .current_mut()
                    .terminate(ControlTerminator::Return(Some(r))),
                Ok(FlowState::Terminated) => {}
                Err(e) => {
                    self.frames = saved;
                    return Err(e);
                }
            }
            let finished = self.frames.pop().expect("child frame");
            self.frames = saved;
            prog.functions[fn_id.0 as usize] = finished.finish();
            return Ok((fn_id, Vec::new()));
        }

        self.frames.push(child);
        if let Err(e) = emit_default_prologue(self, prog) {
            self.frames.pop();
            return Err(e);
        }
        match lower_expr(self, prog, body, true) {
            Ok(FlowState::Active(r)) => self
                .current_mut()
                .terminate(ControlTerminator::Return(Some(r))),
            Ok(FlowState::Terminated) => {}
            Err(e) => {
                self.frames.pop();
                return Err(e);
            }
        }
        let finished = self.frames.pop().expect("child frame");
        let captures: Vec<CaptureSource> = finished.upvalues.iter().map(|u| u.source).collect();
        prog.functions[fn_id.0 as usize] = finished.finish();
        prog.parent_edges.push((parent_id, fn_id));
        Ok((fn_id, captures))
    }

    /// Lower a nested callable with plain parameter names (no defaults).
    #[allow(clippy::too_many_arguments)]
    fn lower_child_fn(
        &mut self,
        prog: &mut ProgramLowerer,
        existing_fn_id: Option<FnId>,
        kind: FunctionKind,
        name: Option<&str>,
        params: &[String],
        body: &MirWitness,
        handler_setup: Option<(&str, usize)>,
        capture_parent: bool,
    ) -> MoraResult<(FnId, Vec<CaptureSource>)> {
        let wparams: Vec<WitnessParam> = params
            .iter()
            .map(|n| WitnessParam {
                name: n.clone(),
                type_hint: None,
                default: None,
            })
            .collect();
        self.lower_child_fn_with_params(
            prog,
            existing_fn_id,
            kind,
            name,
            &wparams,
            body,
            handler_setup,
            capture_parent,
        )
    }
}

fn module_object_by_name(name: &str) -> Option<BuiltinKind> {
    crate::value::MODULE_OBJECTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, kind)| *kind)
}

fn literal_to_const(lit: &Literal) -> ControlConst {
    match lit {
        Literal::String(s, _) => ControlConst::String(s.clone()),
        Literal::Char(c, _) => ControlConst::Char(*c),
        Literal::Int(i, _) => ControlConst::Int(*i),
        Literal::TypedInt(i, dt, _) => {
            match crate::common::ScalarValue::from_i64_checked(*i, *dt) {
                Ok(s) => ControlConst::Scalar(s),
                Err(_) => ControlConst::Int(dt.quantize_i64(*i)),
            }
        }
        Literal::Float(f, _) => ControlConst::Float(*f),
        Literal::TypedFloat(f, dt, _) => {
            match crate::common::ScalarValue::from_f64_quantized(*f, *dt) {
                Some(s) => ControlConst::Scalar(s),
                None => ControlConst::Float(dt.quantize_f64(*f)),
            }
        }
        Literal::Bool(b, _) => ControlConst::Bool(*b),
        Literal::Nil(_) => ControlConst::Nil,
    }
}

fn sequence_has_worker(w: &MirWitness) -> bool {
    match &w.kind {
        WitnessKind::Worker { .. } => true,
        WitnessKind::Sequence(items) => items.iter().any(sequence_has_worker),
        _ => false,
    }
}

fn is_empty_or_nil(w: &MirWitness) -> bool {
    match &w.kind {
        WitnessKind::Literal(Literal::Nil(_)) => true,
        WitnessKind::Sequence(items) => items.is_empty(),
        _ => false,
    }
}

/// Scan a witness tree for identifier references to `target` without crossing re-binding
/// parameters/closures/nested handlers that shadow `target`.
fn uses_identifier(w: &MirWitness, target: &str) -> bool {
    match &w.kind {
        WitnessKind::Variable(name) => name == target,
        WitnessKind::Call {
            callee,
            args,
            named_args,
        } => {
            let hit = match callee {
                WitnessCallee::Name(n)
                | WitnessCallee::GenericName { name: n, .. }
                | WitnessCallee::Var(n) => n == target,
                WitnessCallee::Method(obj, _) => obj == target,
                WitnessCallee::Evaluated(e) => uses_identifier(e, target),
                WitnessCallee::Builtin(_) => false,
            };
            hit || args.iter().any(|a| uses_identifier(a, target))
                || named_args.iter().any(|(_, a)| uses_identifier(a, target))
        }
        WitnessKind::Assign { target: t, value } => t == target || uses_identifier(value, target),
        WitnessKind::Closure { params, body, .. } => {
            if params.iter().any(|p| p.name == target) {
                false
            } else {
                uses_identifier(body, target)
            }
        }
        WitnessKind::FnDef { params, body, .. } => {
            if params.iter().any(|p| p.name == target) {
                false
            } else {
                uses_identifier(body, target)
            }
        }
        WitnessKind::Handle {
            k_param,
            body,
            handler,
            ..
        } => {
            uses_identifier(body, target) || (k_param != target && uses_identifier(handler, target))
        }
        WitnessKind::MacroDef { params, body, .. } => {
            if params.iter().any(|p| p == target) {
                false
            } else {
                uses_identifier(body, target)
            }
        }
        WitnessKind::Loop {
            var,
            iterable,
            body,
        } => uses_identifier(iterable, target) || (var != target && uses_identifier(body, target)),
        _ => w
            .child_witnesses()
            .into_iter()
            .any(|c| uses_identifier(c, target)),
    }
}

/// Return `max_idx + 1` among all `__arg<N>` identifiers referenced inside `w`.
fn scan_max_handler_arg(w: &MirWitness) -> usize {
    let mut max_plus_one = 0usize;
    fn walk(w: &MirWitness, max_p1: &mut usize) {
        if let WitnessKind::Variable(name) = &w.kind {
            if let Some(suffix) = name.strip_prefix("__arg")
                && let Ok(idx) = suffix.parse::<usize>()
            {
                *max_p1 = (*max_p1).max(idx + 1);
            }
        } else if let WitnessKind::Assign { target, value } = &w.kind {
            if let Some(suffix) = target.strip_prefix("__arg")
                && let Ok(idx) = suffix.parse::<usize>()
            {
                *max_p1 = (*max_p1).max(idx + 1);
            }
            walk(value, max_p1);
            return;
        }
        if matches!(
            w.kind,
            WitnessKind::Closure { .. } | WitnessKind::FnDef { .. } | WitnessKind::Handle { .. }
        ) {
            return;
        }
        for child in w.child_witnesses() {
            walk(child, max_p1);
        }
    }
    walk(w, &mut max_plus_one);
    max_plus_one
}

/// Flatten nested `Parallel` sequences into sequential statements or worker groups.
fn collect_parallel_items<'a>(w: &'a MirWitness, out: &mut Vec<&'a MirWitness>) {
    match &w.kind {
        WitnessKind::Sequence(items) => {
            for item in items {
                collect_parallel_items(item, out);
            }
        }
        _ => out.push(w),
    }
}

/// Compile a `WitnessPattern` recursively into [`CompiledPattern`], binding pattern variables
/// into fresh lexical `LocalSlot`s of the active arm scope.
fn compile_pattern(
    stack: &mut ScopeStack,
    prog: &mut ProgramLowerer,
    pat: &WitnessPattern,
) -> CompiledPattern {
    match pat {
        WitnessPattern::Wildcard => CompiledPattern::Wildcard,
        WitnessPattern::Variable(name) => {
            let slot = if let Some(existing) = stack
                .current_mut()
                .local_scopes
                .last()
                .and_then(|top| top.get(name))
                .copied()
            {
                existing
            } else {
                stack.current_mut().bind_block_local(name, prog)
            };
            CompiledPattern::Bind(PatternTarget::Local(slot))
        }
        WitnessPattern::Literal(lit) => match lit {
            Literal::Nil(_) => CompiledPattern::Nil,
            Literal::Bool(b, _) => CompiledPattern::Bool(*b),
            Literal::Int(i, _) => CompiledPattern::Int(*i),
            Literal::TypedInt(i, dt, _) => {
                match crate::common::ScalarValue::from_i64_checked(*i, *dt) {
                    Ok(s) => CompiledPattern::Scalar(s),
                    Err(_) => CompiledPattern::Int(dt.quantize_i64(*i)),
                }
            }
            Literal::Float(f, _) => CompiledPattern::Float(*f),
            Literal::TypedFloat(f, dt, _) => {
                match crate::common::ScalarValue::from_f64_quantized(*f, *dt) {
                    Some(s) => CompiledPattern::Scalar(s),
                    None => CompiledPattern::Float(dt.quantize_f64(*f)),
                }
            }
            Literal::Char(c, _) => CompiledPattern::Char(*c),
            Literal::String(s, _) => CompiledPattern::String(s.clone()),
        },
        WitnessPattern::Tuple(items) => CompiledPattern::Tuple(
            items
                .iter()
                .map(|p| compile_pattern(stack, prog, p))
                .collect(),
        ),
        WitnessPattern::List { head, tail } => CompiledPattern::ListCons {
            head: Box::new(compile_pattern(stack, prog, head)),
            tail: Box::new(compile_pattern(stack, prog, tail)),
        },
        WitnessPattern::ListVec { elements, rest } => CompiledPattern::ListVec {
            elements: elements
                .iter()
                .map(|p| compile_pattern(stack, prog, p))
                .collect(),
            rest: rest
                .as_ref()
                .map(|r| Box::new(compile_pattern(stack, prog, r))),
        },
        WitnessPattern::Dict { required, rest } => CompiledPattern::Dict {
            required: required
                .iter()
                .map(|(k, p)| (k.clone(), compile_pattern(stack, prog, p)))
                .collect(),
            rest: *rest,
        },
        WitnessPattern::TypeAscription { name, pattern } => CompiledPattern::TypeAscription {
            ty_name: name.clone(),
            inner: Box::new(compile_pattern(stack, prog, pattern)),
        },
        WitnessPattern::Variant {
            enum_name,
            variant,
            fields,
        } => CompiledPattern::Variant {
            enum_name: enum_name.clone(),
            variant: variant.clone(),
            fields: fields
                .iter()
                .map(|p| compile_pattern(stack, prog, p))
                .collect(),
        },
        WitnessPattern::Struct { name, fields, rest } => CompiledPattern::Struct {
            name: name.clone(),
            fields: fields
                .iter()
                .map(|(k, p)| (k.clone(), compile_pattern(stack, prog, p)))
                .collect(),
            rest: *rest,
        },
    }
}

fn lower_sequence_slice(
    stack: &mut ScopeStack,
    prog: &mut ProgramLowerer,
    items: &[MirWitness],
    in_tail: bool,
) -> MoraResult<FlowState> {
    if items.is_empty() {
        return Ok(FlowState::Active(stack.current_mut().emit_nil()));
    }
    let last_idx = items.len() - 1;
    let mut last_reg = None;
    for (idx, item) in items.iter().enumerate() {
        let tail_here = in_tail && idx == last_idx;
        match lower_expr(stack, prog, item, tail_here)? {
            FlowState::Active(r) => last_reg = Some(r),
            FlowState::Terminated => return Ok(FlowState::Terminated),
        }
    }
    match last_reg {
        Some(r) => Ok(FlowState::Active(r)),
        None => Ok(FlowState::Active(stack.current_mut().emit_nil())),
    }
}

/// Iterative lowering of `Binary` / `Or` / `And` / `Pipe` / `MethodCall` chains using
/// an explicit heap stack so arbitrary-depth operator or unary chains never overflow the OS stack.
fn lower_expr_chain(
    stack: &mut ScopeStack,
    prog: &mut ProgramLowerer,
    root: &MirWitness,
) -> MoraResult<FlowState> {
    use crate::common::BinaryOp;

    enum ChainTask<'w> {
        Eval(&'w MirWitness),
        EmitUnary {
            op: UnaryOp,
        },
        EmitBinary {
            op: &'w BinaryOp,
        },
        EmitPipe,
        EmitModuleCall {
            kind: BuiltinKind,
            method: &'w str,
            arg_count: usize,
            named_keys: Vec<&'w str>,
        },
        EmitMethodCall {
            method: &'w str,
            arg_count: usize,
            named_keys: Vec<&'w str>,
        },
        StartShortCircuit {
            right: &'w MirWitness,
            is_or: bool,
            prev_vals_len: usize,
        },
        EndShortCircuit {
            merge_bb: BlockId,
            merge_reg: VReg,
            prev_vals_len: usize,
        },
    }

    fn unwind_terminated(
        tasks: &mut Vec<ChainTask<'_>>,
        vals: &mut Vec<VReg>,
        stack: &mut ScopeStack,
    ) -> bool {
        while let Some(task) = tasks.pop() {
            if let ChainTask::EndShortCircuit {
                merge_bb,
                merge_reg,
                prev_vals_len,
            } = task
            {
                vals.truncate(prev_vals_len);
                vals.push(merge_reg);
                stack.current_mut().switch_to(merge_bb);
                return true;
            }
        }
        false
    }

    let mut tasks = vec![ChainTask::Eval(root)];
    let mut vals: Vec<VReg> = Vec::new();

    while let Some(task) = tasks.pop() {
        match task {
            ChainTask::Eval(w) => match &w.kind {
                WitnessKind::Call {
                    callee: WitnessCallee::Builtin(BuiltinOp::Neg),
                    args,
                    named_args,
                } if args.len() == 1 && named_args.is_empty() => {
                    tasks.push(ChainTask::EmitUnary { op: UnaryOp::Neg });
                    tasks.push(ChainTask::Eval(&args[0]));
                }
                WitnessKind::Call {
                    callee: WitnessCallee::Builtin(BuiltinOp::Not),
                    args,
                    named_args,
                } if args.len() == 1 && named_args.is_empty() => {
                    tasks.push(ChainTask::EmitUnary { op: UnaryOp::Not });
                    tasks.push(ChainTask::Eval(&args[0]));
                }
                WitnessKind::Binary { left, op, right } => {
                    tasks.push(ChainTask::EmitBinary { op });
                    tasks.push(ChainTask::Eval(right));
                    tasks.push(ChainTask::Eval(left));
                }
                WitnessKind::Pipe { left, right } => {
                    tasks.push(ChainTask::EmitPipe);
                    tasks.push(ChainTask::Eval(right));
                    tasks.push(ChainTask::Eval(left));
                }
                WitnessKind::Or { left, right } => {
                    tasks.push(ChainTask::StartShortCircuit {
                        right,
                        is_or: true,
                        prev_vals_len: vals.len(),
                    });
                    tasks.push(ChainTask::Eval(left));
                }
                WitnessKind::And { left, right } => {
                    tasks.push(ChainTask::StartShortCircuit {
                        right,
                        is_or: false,
                        prev_vals_len: vals.len(),
                    });
                    tasks.push(ChainTask::Eval(left));
                }
                WitnessKind::MethodCall {
                    receiver,
                    method,
                    args,
                    named_args,
                } => {
                    let named_keys: Vec<&str> =
                        named_args.iter().map(|(k, _)| k.as_str()).collect();
                    if let WitnessKind::Variable(mod_name) = &receiver.kind
                        && stack.resolve_binding(prog, mod_name, false).is_none()
                        && let Some(kind) = module_object_by_name(mod_name)
                    {
                        tasks.push(ChainTask::EmitModuleCall {
                            kind,
                            method,
                            arg_count: args.len(),
                            named_keys,
                        });
                        for (_, v) in named_args.iter().rev() {
                            tasks.push(ChainTask::Eval(v));
                        }
                        for a in args.iter().rev() {
                            tasks.push(ChainTask::Eval(a));
                        }
                    } else {
                        tasks.push(ChainTask::EmitMethodCall {
                            method,
                            arg_count: args.len(),
                            named_keys,
                        });
                        for (_, v) in named_args.iter().rev() {
                            tasks.push(ChainTask::Eval(v));
                        }
                        for a in args.iter().rev() {
                            tasks.push(ChainTask::Eval(a));
                        }
                        tasks.push(ChainTask::Eval(receiver));
                    }
                }
                _ => match lower_expr_single(stack, prog, w, false)? {
                    FlowState::Active(r) => vals.push(r),
                    FlowState::Terminated => {
                        if !unwind_terminated(&mut tasks, &mut vals, stack) {
                            return Ok(FlowState::Terminated);
                        }
                    }
                },
            },
            ChainTask::EmitUnary { op } => {
                let src = vals.pop().expect("unary arg reg present");
                let dst = stack.current_mut().alloc_reg();
                stack.current_mut().emit(ControlInst::Unary(dst, op, src));
                vals.push(dst);
            }
            ChainTask::EmitBinary { op } => {
                let rhs = vals.pop().expect("rhs reg present");
                let lhs = vals.pop().expect("lhs reg present");
                let dst = stack.current_mut().alloc_reg();
                stack
                    .current_mut()
                    .emit(ControlInst::Binary(dst, lhs, *op, rhs));
                vals.push(dst);
            }
            ChainTask::EmitPipe => {
                let rhs = vals.pop().expect("pipe rhs reg present");
                let lhs = vals.pop().expect("pipe lhs reg present");
                let dst = stack.current_mut().alloc_reg();
                stack
                    .current_mut()
                    .emit(ControlInst::Pipe { dst, lhs, rhs });
                vals.push(dst);
            }
            ChainTask::EmitModuleCall {
                kind,
                method,
                arg_count,
                named_keys,
            } => {
                let named_start = vals.len() - named_keys.len();
                let named_vals: Vec<VReg> = vals.drain(named_start..).collect();
                let named_args: Vec<(SymId, VReg)> = named_keys
                    .into_iter()
                    .zip(named_vals)
                    .map(|(k, r)| (prog.intern(k), r))
                    .collect();
                let start = vals.len() - arg_count;
                let arg_regs: Vec<VReg> = vals.drain(start..).collect();
                let dst = stack.current_mut().alloc_reg();
                let method_sym = prog.intern(method);
                stack.current_mut().emit(ControlInst::ModuleCall {
                    dst,
                    module: kind,
                    method: method_sym,
                    args: arg_regs,
                    named_args,
                });
                vals.push(dst);
            }
            ChainTask::EmitMethodCall {
                method,
                arg_count,
                named_keys,
            } => {
                let named_start = vals.len() - named_keys.len();
                let named_vals: Vec<VReg> = vals.drain(named_start..).collect();
                let named_args: Vec<(SymId, VReg)> = named_keys
                    .into_iter()
                    .zip(named_vals)
                    .map(|(k, r)| (prog.intern(k), r))
                    .collect();
                let start = vals.len() - arg_count;
                let arg_regs: Vec<VReg> = vals.drain(start..).collect();
                let recv_reg = vals.pop().expect("method receiver reg present");
                let dst = stack.current_mut().alloc_reg();
                let method_sym = prog.intern(method);
                stack.current_mut().emit(ControlInst::MethodCall {
                    dst,
                    receiver: recv_reg,
                    method: method_sym,
                    args: arg_regs,
                    named_args,
                });
                vals.push(dst);
            }
            ChainTask::StartShortCircuit {
                right,
                is_or,
                prev_vals_len,
            } => {
                let lv = vals.pop().expect("short-circuit lhs reg present");
                let lb = stack.current_mut().alloc_reg();
                stack
                    .current_mut()
                    .emit(ControlInst::Unary(lb, UnaryOp::Truthy, lv));
                let (rhs_bb, _) = stack.current_mut().new_block(0);
                let (merge_bb, merge_params) = stack.current_mut().new_block(1);
                let merge_reg = merge_params[0];
                if is_or {
                    stack.current_mut().terminate(ControlTerminator::Branch {
                        cond: lb,
                        then_bb: merge_bb,
                        then_args: vec![lb],
                        else_bb: rhs_bb,
                        else_args: Vec::new(),
                    });
                } else {
                    stack.current_mut().terminate(ControlTerminator::Branch {
                        cond: lb,
                        then_bb: rhs_bb,
                        then_args: Vec::new(),
                        else_bb: merge_bb,
                        else_args: vec![lb],
                    });
                }
                stack.current_mut().switch_to(rhs_bb);
                tasks.push(ChainTask::EndShortCircuit {
                    merge_bb,
                    merge_reg,
                    prev_vals_len,
                });
                tasks.push(ChainTask::Eval(right));
            }
            ChainTask::EndShortCircuit {
                merge_bb,
                merge_reg,
                ..
            } => {
                let rv = vals.pop().expect("short-circuit rhs reg present");
                let rb = stack.current_mut().alloc_reg();
                stack
                    .current_mut()
                    .emit(ControlInst::Unary(rb, UnaryOp::Truthy, rv));
                stack.current_mut().terminate(ControlTerminator::Jump {
                    target: merge_bb,
                    args: vec![rb],
                });
                stack.current_mut().switch_to(merge_bb);
                vals.push(merge_reg);
            }
        }
    }

    Ok(vals
        .pop()
        .map(FlowState::Active)
        .unwrap_or(FlowState::Terminated))
}

/// Core exhaustive lowering of a single [`MirWitness`] into explicit-CFG instructions/terminators.
fn lower_expr(
    stack: &mut ScopeStack,
    prog: &mut ProgramLowerer,
    w: &MirWitness,
    in_tail: bool,
) -> MoraResult<FlowState> {
    if matches!(
        &w.kind,
        WitnessKind::Binary { .. }
            | WitnessKind::Or { .. }
            | WitnessKind::And { .. }
            | WitnessKind::Pipe { .. }
            | WitnessKind::MethodCall { .. }
    ) || matches!(
        &w.kind,
        WitnessKind::Call {
            callee: WitnessCallee::Builtin(BuiltinOp::Neg | BuiltinOp::Not),
            args,
            named_args,
        } if args.len() == 1 && named_args.is_empty()
    ) {
        return lower_expr_chain(stack, prog, w);
    }
    lower_expr_single(stack, prog, w, in_tail)
}

fn lower_expr_single(
    stack: &mut ScopeStack,
    prog: &mut ProgramLowerer,
    w: &MirWitness,
    in_tail: bool,
) -> MoraResult<FlowState> {
    match &w.kind {
        WitnessKind::Literal(lit) => Ok(FlowState::Active(
            stack.current_mut().emit_const(literal_to_const(lit)),
        )),

        WitnessKind::Variable(name) => Ok(FlowState::Active(stack.emit_read_var(prog, name))),

        WitnessKind::Binary { .. }
        | WitnessKind::Or { .. }
        | WitnessKind::And { .. }
        | WitnessKind::Pipe { .. }
        | WitnessKind::MethodCall { .. } => unreachable!("routed via lower_expr_chain"),

        WitnessKind::List(items) => {
            let mut regs = Vec::with_capacity(items.len());
            for item in items {
                match lower_expr(stack, prog, item, false)? {
                    FlowState::Active(r) => regs.push(r),
                    FlowState::Terminated => return Ok(FlowState::Terminated),
                }
            }
            let dst = stack.current_mut().alloc_reg();
            stack.current_mut().emit(ControlInst::ListLit(dst, regs));
            Ok(FlowState::Active(dst))
        }

        WitnessKind::Dict(entries) => {
            let mut pairs = Vec::with_capacity(entries.len());
            for (k, v) in entries {
                let vr = match lower_expr(stack, prog, v, false)? {
                    FlowState::Active(r) => r,
                    FlowState::Terminated => return Ok(FlowState::Terminated),
                };
                let key_sym = prog.intern(k);
                pairs.push((key_sym, vr));
            }
            let dst = stack.current_mut().alloc_reg();
            stack.current_mut().emit(ControlInst::DictLit(dst, pairs));
            Ok(FlowState::Active(dst))
        }

        WitnessKind::DynTrait {
            expr,
            trait_name,
            generics,
        } => {
            let src = match lower_expr(stack, prog, expr, false)? {
                FlowState::Active(r) => r,
                FlowState::Terminated => return Ok(FlowState::Terminated),
            };
            let dst = stack.current_mut().alloc_reg();
            let tsym = prog.intern(trait_name);
            let gen_strs = generics
                .iter()
                .map(|g| g.to_type().name().to_string())
                .collect();
            stack.current_mut().emit(ControlInst::MakeDynTrait {
                dst,
                src,
                trait_name: tsym,
                generics: gen_strs,
            });
            Ok(FlowState::Active(dst))
        }

        WitnessKind::Quasiquote { segments } => {
            let mut out_segs = Vec::new();
            let mut pending_splice = false;
            for seg in segments {
                if !pending_splice
                    && matches!(seg.kind, WitnessKind::Literal(Literal::Bool(true, _)))
                {
                    pending_splice = true;
                    continue;
                }
                if pending_splice {
                    pending_splice = false;
                    let r = match lower_expr(stack, prog, seg, false)? {
                        FlowState::Active(r) => r,
                        FlowState::Terminated => return Ok(FlowState::Terminated),
                    };
                    out_segs.push(ControlQqSegment::UnquoteSplice(r));
                } else if let WitnessKind::Literal(Literal::String(s, _)) = &seg.kind {
                    out_segs.push(ControlQqSegment::Quote(s.clone()));
                } else {
                    let r = match lower_expr(stack, prog, seg, false)? {
                        FlowState::Active(r) => r,
                        FlowState::Terminated => return Ok(FlowState::Terminated),
                    };
                    out_segs.push(ControlQqSegment::Unquote(r));
                }
            }
            let dst = stack.current_mut().alloc_reg();
            stack
                .current_mut()
                .emit(ControlInst::Quasiquote(dst, out_segs));
            Ok(FlowState::Active(dst))
        }

        WitnessKind::Call {
            callee,
            args,
            named_args,
        } => lower_call(stack, prog, callee, args, named_args, in_tail),

        WitnessKind::Closure { params, body, .. } => {
            let (fn_id, captures) = stack.lower_child_fn_with_params(
                prog,
                None,
                FunctionKind::Closure,
                None,
                params,
                body,
                None,
                true,
            )?;
            let dst = stack.current_mut().alloc_reg();
            stack.current_mut().emit(ControlInst::MakeClosure {
                dst,
                fn_id,
                captures,
            });
            Ok(FlowState::Active(dst))
        }

        WitnessKind::FnDef {
            name, params, body, ..
        } => {
            let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
            let is_top_level = stack.frames.len() == 1
                && stack.current_mut().kind == FunctionKind::Main
                && stack.current_mut().local_scopes.len() == 1;
            if is_top_level {
                let g_slot = prog.global_by_name.get(name).copied().unwrap_or_else(|| {
                    let id = prog.reserve_fn();
                    prog.alloc_global(name, GlobalKind::Task(id))
                });
                let existing_id = match prog.globals[g_slot.0 as usize].kind {
                    GlobalKind::Task(id) => Some(id),
                    _ => None,
                };
                if let Some(id) = existing_id {
                    let specs = params
                        .iter()
                        .map(|p| (p.name.clone(), p.default.is_some()))
                        .collect();
                    prog.task_param_specs.insert(id, specs);
                }
                let (fn_id, _) = stack.lower_child_fn_with_params(
                    prog,
                    existing_id,
                    FunctionKind::Task,
                    Some(name),
                    params,
                    body,
                    None,
                    false,
                )?;
                let name_sym = prog.intern(name);
                let param_syms = param_names.iter().map(|p| prog.intern(p)).collect();
                stack.current_mut().emit(ControlInst::RegisterTask {
                    name: name_sym,
                    params: param_syms,
                    fn_id,
                    dst: Some(g_slot),
                });
                Ok(FlowState::Active(stack.current_mut().emit_nil()))
            } else {
                // Nested `task` inside another function behaves like a captured local binding.
                let (fn_id, captures) = stack.lower_child_fn_with_params(
                    prog,
                    None,
                    FunctionKind::Closure,
                    Some(name),
                    params,
                    body,
                    None,
                    true,
                )?;
                let clos_reg = stack.current_mut().alloc_reg();
                stack.current_mut().emit(ControlInst::MakeClosure {
                    dst: clos_reg,
                    fn_id,
                    captures,
                });
                let slot = stack.current_mut().bind_let_local(name, prog);
                stack
                    .current_mut()
                    .emit(ControlInst::StoreLocal(slot, clos_reg));
                Ok(FlowState::Active(stack.current_mut().emit_nil()))
            }
        }

        WitnessKind::LetBinding {
            name,
            value,
            init_body,
            ..
        } => {
            let val_reg = match lower_expr(stack, prog, value, false)? {
                FlowState::Active(r) => r,
                FlowState::Terminated => return Ok(FlowState::Terminated),
            };
            if stack.is_module_top_level() {
                let g_slot = prog.alloc_global(name, GlobalKind::Var);
                stack
                    .current_mut()
                    .emit(ControlInst::StoreGlobal(g_slot, val_reg));
            } else {
                let slot = stack.current_mut().bind_let_local(name, prog);
                stack
                    .current_mut()
                    .emit(ControlInst::StoreLocal(slot, val_reg));
            }
            if is_empty_or_nil(init_body) {
                Ok(FlowState::Active(val_reg))
            } else {
                lower_expr(stack, prog, init_body, in_tail)
            }
        }

        WitnessKind::Assign { target, value } => {
            let val_reg = match lower_expr(stack, prog, value, false)? {
                FlowState::Active(r) => r,
                FlowState::Terminated => return Ok(FlowState::Terminated),
            };
            if let Some(binding) = stack.resolve_binding(prog, target, true) {
                emit_store_binding(stack.current_mut(), binding, val_reg);
            } else {
                let sym = prog.intern(target);
                stack
                    .current_mut()
                    .emit(ControlInst::AssignDyn(sym, val_reg));
            }
            Ok(FlowState::Active(val_reg))
        }

        WitnessKind::IndexAssign {
            object,
            index,
            value,
        } => {
            let obj = match lower_expr(stack, prog, object, false)? {
                FlowState::Active(r) => r,
                FlowState::Terminated => return Ok(FlowState::Terminated),
            };
            let idx = match lower_expr(stack, prog, index, false)? {
                FlowState::Active(r) => r,
                FlowState::Terminated => return Ok(FlowState::Terminated),
            };
            let val = match lower_expr(stack, prog, value, false)? {
                FlowState::Active(r) => r,
                FlowState::Terminated => return Ok(FlowState::Terminated),
            };
            let dst = stack.current_mut().alloc_reg();
            stack
                .current_mut()
                .emit(ControlInst::IndexAssign(dst, obj, idx, val));
            Ok(FlowState::Active(dst))
        }

        WitnessKind::If { cond, then, r#else } => {
            lower_if(stack, prog, cond, then, r#else.as_deref(), in_tail)
        }

        WitnessKind::While { cond, body } => lower_while(stack, prog, cond, body),

        WitnessKind::Loop {
            var,
            iterable,
            body,
        } => lower_for_loop(stack, prog, var, iterable, body),

        WitnessKind::Match { scrutinee, arms } => {
            lower_match(stack, prog, scrutinee, arms, in_tail)
        }

        WitnessKind::Return(expr) => {
            let ret_val = match expr {
                Some(e) => match lower_expr(stack, prog, e, true)? {
                    FlowState::Active(r) => Some(r),
                    FlowState::Terminated => return Ok(FlowState::Terminated),
                },
                None => None,
            };
            stack
                .current_mut()
                .terminate(ControlTerminator::Return(ret_val));
            Ok(FlowState::Terminated)
        }

        WitnessKind::Break(_) => {
            let target = stack
                .current_mut()
                .loop_stack
                .last()
                .map(|t| t.break_bb)
                .ok_or_else(|| {
                    MoraError::Parse("Compile error: `break` used outside of a loop".to_string())
                })?;
            stack.current_mut().terminate(ControlTerminator::Jump {
                target,
                args: Vec::new(),
            });
            Ok(FlowState::Terminated)
        }

        WitnessKind::Continue(_) => {
            let target = stack
                .current_mut()
                .loop_stack
                .last()
                .map(|t| t.continue_bb)
                .ok_or_else(|| {
                    MoraError::Parse("Compile error: `continue` used outside of a loop".to_string())
                })?;
            stack.current_mut().terminate(ControlTerminator::Jump {
                target,
                args: Vec::new(),
            });
            Ok(FlowState::Terminated)
        }

        WitnessKind::Perform { effect, args } => {
            let mut arg_regs = Vec::with_capacity(args.len());
            for a in args {
                match lower_expr(stack, prog, a, false)? {
                    FlowState::Active(r) => arg_regs.push(r),
                    FlowState::Terminated => return Ok(FlowState::Terminated),
                }
            }
            let esym = prog.intern(effect);
            let (resume_bb, params) = stack.current_mut().new_block(1);
            stack.current_mut().terminate(ControlTerminator::Perform {
                effect: esym,
                args: arg_regs,
                resume_bb,
            });
            stack.current_mut().switch_to(resume_bb);
            Ok(FlowState::Active(params[0]))
        }

        WitnessKind::Handle {
            effect,
            body,
            handler,
            k_param,
        } => {
            let (body_fn, body_captures) = stack.lower_child_fn(
                prog,
                None,
                FunctionKind::HandleBody,
                None,
                &[],
                body,
                None,
                true,
            )?;
            let arg_count = scan_max_handler_arg(handler);
            let auto_resume = !uses_identifier(handler, k_param);
            let (handler_fn, handler_captures) = stack.lower_child_fn(
                prog,
                None,
                FunctionKind::HandleHandler,
                None,
                &[],
                handler,
                Some((k_param, arg_count)),
                true,
            )?;
            let esym = prog.intern(effect);
            let dst = stack.current_mut().alloc_reg();
            stack.current_mut().emit(ControlInst::Handle {
                dst,
                effect: esym,
                body_fn,
                body_captures,
                handler_fn,
                handler_captures,
                auto_resume,
            });
            Ok(FlowState::Active(dst))
        }

        WitnessKind::TypeAlias { name, target } => {
            let sym = prog.intern(name);
            let slot = if stack.current_mut().kind == FunctionKind::Main {
                Some(prog.alloc_global(name, GlobalKind::TypeAlias))
            } else {
                None
            };
            let target_str = target.to_type().name().to_string();
            stack.current_mut().emit(ControlInst::RegisterTypeAlias {
                name: sym,
                target: target_str,
                dst: slot,
            });
            Ok(FlowState::Active(stack.current_mut().emit_nil()))
        }

        WitnessKind::EnumDef {
            name,
            type_params,
            const_params,
            variants,
        } => {
            prog.struct_const_meta
                .insert(name.clone(), (type_params.clone(), const_params.clone()));
            let sym = prog.intern(name);
            let tp_syms = type_params.iter().map(|t| prog.intern(t)).collect();
            let cp_syms = const_params.iter().map(|c| prog.intern(c)).collect();
            let mut const_shape_bindings = Vec::new();
            let mut vdescs = Vec::with_capacity(variants.len());
            for v in variants {
                let vsym = prog.intern(&v.name);
                let mut payload_names = Vec::with_capacity(v.payload_types.len());
                for (pidx, h) in v.payload_types.iter().enumerate() {
                    let ty = h.to_type();
                    if !const_params.is_empty()
                        && let crate::typeck::Type::Tensor {
                            shape: crate::typeck::ShapeType::Ranked(dims),
                            ..
                        } = ty
                    {
                        for (aidx, dim) in dims.iter().enumerate() {
                            if let crate::typeck::DimExpr::Symbolic(sym_name) = dim
                                && const_params.contains(sym_name)
                            {
                                const_shape_bindings.push((
                                    vsym,
                                    prog.intern(sym_name),
                                    pidx,
                                    aidx,
                                ));
                            }
                        }
                    }
                    payload_names.push(ty.name().to_string());
                }
                vdescs.push((vsym, payload_names));
            }
            let slot = if stack.current_mut().kind == FunctionKind::Main {
                Some(prog.alloc_global(name, GlobalKind::Enum))
            } else {
                None
            };
            stack.current_mut().emit(ControlInst::RegisterEnum {
                name: sym,
                type_params: tp_syms,
                const_params: cp_syms,
                const_shape_bindings,
                variants: vdescs,
                dst: slot,
            });
            Ok(FlowState::Active(stack.current_mut().emit_nil()))
        }

        WitnessKind::StructDef {
            name,
            type_params,
            const_params,
            fields,
        } => {
            prog.struct_const_meta
                .insert(name.clone(), (type_params.clone(), const_params.clone()));
            let sym = prog.intern(name);
            let tp_syms = type_params.iter().map(|t| prog.intern(t)).collect();
            let cp_syms = const_params.iter().map(|c| prog.intern(c)).collect();
            let mut const_shape_bindings = Vec::new();
            let mut field_descs = Vec::with_capacity(fields.len());
            for (fidx, f) in fields.iter().enumerate() {
                if !const_params.is_empty()
                    && let crate::typeck::Type::Tensor {
                        shape: crate::typeck::ShapeType::Ranked(dims),
                        ..
                    } = f.type_hint.to_type()
                {
                    for (aidx, d) in dims.iter().enumerate() {
                        if let crate::typeck::DimExpr::Symbolic(sym_name) = d
                            && const_params.contains(sym_name)
                        {
                            const_shape_bindings.push((prog.intern(sym_name), fidx, aidx));
                        }
                    }
                }
                let default_fn = if let Some(def_w) = &f.default {
                    let (df_id, _) = stack.lower_child_fn(
                        prog,
                        None,
                        FunctionKind::DefaultArg,
                        Some(&format!("{name}::default_{}", f.name)),
                        &[],
                        def_w,
                        None,
                        false,
                    )?;
                    Some(df_id)
                } else {
                    None
                };
                field_descs.push(ControlStructField {
                    name: prog.intern(&f.name),
                    type_name: f.type_hint.to_type().name().to_string(),
                    default_fn,
                    tags: f.tags.clone(),
                });
            }
            let slot = if stack.current_mut().kind == FunctionKind::Main {
                Some(prog.alloc_global(name, GlobalKind::Struct))
            } else {
                None
            };
            stack.current_mut().emit(ControlInst::RegisterStruct {
                name: sym,
                type_params: tp_syms,
                const_params: cp_syms,
                const_shape_bindings,
                fields: field_descs,
                dst: slot,
            });
            Ok(FlowState::Active(stack.current_mut().emit_nil()))
        }

        WitnessKind::TraitDef {
            name,
            type_params,
            parents,
            methods,
        } => {
            let sym = prog.intern(name);
            let tp_syms = type_params.iter().map(|t| prog.intern(t)).collect();
            let parent_syms = parents.iter().map(|p| prog.intern(p)).collect();
            let mut compiled_methods = Vec::with_capacity(methods.len());
            for m in methods {
                let default_fn = if let Some(body_w) = &m.default_body {
                    let (df_id, _) = stack.lower_child_fn_with_params(
                        prog,
                        None,
                        FunctionKind::TraitDefault,
                        Some(&format!("{name}::{}", m.name)),
                        &m.params,
                        body_w,
                        None,
                        false,
                    )?;
                    Some(df_id)
                } else {
                    None
                };
                let params = m
                    .params
                    .iter()
                    .map(|p| {
                        (
                            prog.intern(&p.name),
                            p.type_hint.as_ref().map(|h| h.to_type().name()),
                        )
                    })
                    .collect();
                compiled_methods.push(ControlTraitMethod {
                    name: prog.intern(&m.name),
                    params,
                    return_type: m.return_type.as_ref().map(|h| h.to_type().name()),
                    has_self: m.has_self,
                    default_fn,
                });
            }
            let slot = if stack.current_mut().kind == FunctionKind::Main {
                Some(prog.alloc_global(name, GlobalKind::Trait))
            } else {
                None
            };
            stack.current_mut().emit(ControlInst::RegisterTrait {
                name: sym,
                type_params: tp_syms,
                parents: parent_syms,
                methods: compiled_methods,
                dst: slot,
            });
            Ok(FlowState::Active(stack.current_mut().emit_nil()))
        }

        WitnessKind::ImplDef {
            const_params,
            trait_name,
            trait_generics,
            target_type,
            target_generics,
            methods,
            ..
        } => {
            let trait_sym = trait_name.as_ref().map(|t| prog.intern(t));
            let target_sym = prog.intern(target_type);
            let t_gens: Vec<String> = trait_generics.iter().map(|g| g.to_type().name()).collect();
            let tgt_gens: Vec<String> =
                target_generics.iter().map(|g| g.to_type().name()).collect();
            let mut active_consts = const_params.clone();
            if let Some((stp, scp)) = prog.struct_const_meta.get(target_type).cloned() {
                for (idx, tg) in target_generics.iter().enumerate() {
                    if let Some(tp_name) = stp.get(idx)
                        && scp.contains(tp_name)
                        && let crate::typeck::Type::TypeVar(crate::typeck::TypeVarId::Rigid(r)) =
                            tg.to_type()
                        && !active_consts.contains(r)
                    {
                        active_consts.push(r.clone());
                    }
                }
            }
            let mut compiled_methods = Vec::with_capacity(methods.len());
            for m in methods {
                let label = match trait_name {
                    Some(tr) => format!("impl<{tr} for {target_type}>::{}", m.name),
                    None => format!("impl<{target_type}>::{}", m.name),
                };
                let (fn_id, _) = stack.lower_child_fn_with_params_and_consts(
                    prog,
                    None,
                    FunctionKind::ImplMethod,
                    Some(&label),
                    &m.params,
                    &active_consts,
                    m.has_self,
                    &m.body,
                    None,
                    false,
                )?;
                compiled_methods.push(ControlImplMethod {
                    name: prog.intern(&m.name),
                    fn_id,
                    has_self: m.has_self,
                });
            }
            stack.current_mut().emit(ControlInst::RegisterImpl {
                trait_name: trait_sym,
                trait_generics: t_gens,
                target_type: target_sym,
                target_generics: tgt_gens,
                methods: compiled_methods,
            });
            Ok(FlowState::Active(stack.current_mut().emit_nil()))
        }

        WitnessKind::KernelDef { name, params, body } => {
            let kprog =
                crate::runtime::gpu::kernel_compiler::lower_witness_kernel(name, params, body)
                    .map_err(MoraError::Parse)?;
            let sym = prog.intern(name);
            let slot = if stack.current_mut().kind == FunctionKind::Main {
                Some(prog.alloc_global(name, GlobalKind::Kernel))
            } else {
                None
            };
            stack.current_mut().emit(ControlInst::RegisterKernel {
                name: sym,
                program: Box::new(kprog),
                dst: slot,
            });
            Ok(FlowState::Active(stack.current_mut().emit_nil()))
        }

        WitnessKind::TensorSlice { target, axes } => {
            let tgt_reg = match lower_expr(stack, prog, target, false)? {
                FlowState::Active(r) => r,
                FlowState::Terminated => return Ok(FlowState::Terminated),
            };
            let mut compiled_axes = Vec::with_capacity(axes.len());
            for ax in axes {
                let c_ax = match ax {
                    WitnessSliceAxis::Index(e) => match lower_expr(stack, prog, e, false)? {
                        FlowState::Active(r) => ControlSliceAxis::Index(r),
                        FlowState::Terminated => return Ok(FlowState::Terminated),
                    },
                    WitnessSliceAxis::Range { start, stop, step } => {
                        let s_r = if let Some(e) = start {
                            match lower_expr(stack, prog, e, false)? {
                                FlowState::Active(r) => Some(r),
                                FlowState::Terminated => return Ok(FlowState::Terminated),
                            }
                        } else {
                            None
                        };
                        let e_r = if let Some(e) = stop {
                            match lower_expr(stack, prog, e, false)? {
                                FlowState::Active(r) => Some(r),
                                FlowState::Terminated => return Ok(FlowState::Terminated),
                            }
                        } else {
                            None
                        };
                        let st_r = if let Some(e) = step {
                            match lower_expr(stack, prog, e, false)? {
                                FlowState::Active(r) => Some(r),
                                FlowState::Terminated => return Ok(FlowState::Terminated),
                            }
                        } else {
                            None
                        };
                        ControlSliceAxis::Range {
                            start: s_r,
                            stop: e_r,
                            step: st_r,
                        }
                    }
                    WitnessSliceAxis::Ellipsis => ControlSliceAxis::Ellipsis,
                    WitnessSliceAxis::NewAxis => ControlSliceAxis::NewAxis,
                };
                compiled_axes.push(c_ax);
            }
            let dst = stack.current_mut().alloc_reg();
            stack.current_mut().emit(ControlInst::TensorSlice {
                dst,
                target: tgt_reg,
                axes: compiled_axes,
            });
            Ok(FlowState::Active(dst))
        }

        WitnessKind::NoGrad { body } => {
            let (body_fn, body_captures) = stack.lower_child_fn(
                prog,
                None,
                FunctionKind::NoGradBody,
                None,
                &[],
                body,
                None,
                true,
            )?;
            let dst = stack.current_mut().alloc_reg();
            stack.current_mut().emit(ControlInst::NoGrad {
                dst,
                body_fn,
                body_captures,
            });
            Ok(FlowState::Active(dst))
        }

        WitnessKind::MacroDef { name, params, body } => {
            let is_top_level =
                stack.frames.len() == 1 && stack.current_mut().kind == FunctionKind::Main;
            let (existing_id, g_slot) = if is_top_level {
                let slot = prog.global_by_name.get(name).copied().unwrap_or_else(|| {
                    let id = prog.reserve_fn();
                    prog.alloc_global(name, GlobalKind::Macro(id))
                });
                let id = match prog.globals[slot.0 as usize].kind {
                    GlobalKind::Macro(id) => Some(id),
                    _ => None,
                };
                (id, Some(slot))
            } else {
                (None, None)
            };
            let (body_fn, _) = stack.lower_child_fn(
                prog,
                existing_id,
                FunctionKind::Macro,
                Some(name),
                params,
                body,
                None,
                false,
            )?;
            let sym = prog.intern(name);
            let psyms = params.iter().map(|p| prog.intern(p)).collect();
            stack.current_mut().emit(ControlInst::RegisterMacro {
                name: sym,
                params: psyms,
                body_fn,
                dst: g_slot,
            });
            Ok(FlowState::Active(stack.current_mut().emit_nil()))
        }

        WitnessKind::Import(path) => {
            stack
                .current_mut()
                .emit(ControlInst::Import { path: path.clone() });
            Ok(FlowState::Active(stack.current_mut().emit_nil()))
        }

        WitnessKind::Export { names, decl } => {
            let outcome = lower_expr(stack, prog, decl, false)?;
            for n in names {
                if let Some(&slot) = prog.global_by_name.get(n) {
                    prog.globals[slot.0 as usize].exported = true;
                    stack.current_mut().emit(ControlInst::ExportGlobal(slot));
                } else {
                    let sym = prog.intern(n);
                    stack.current_mut().emit(ControlInst::ExportDyn(sym));
                }
            }
            match outcome {
                FlowState::Active(_) => Ok(FlowState::Active(stack.current_mut().emit_nil())),
                FlowState::Terminated => Ok(FlowState::Terminated),
            }
        }

        WitnessKind::EffectSig { .. } => Ok(FlowState::Active(stack.current_mut().emit_nil())),

        WitnessKind::Worker { name, body } => {
            let (fn_id, captures) = stack.lower_child_fn(
                prog,
                None,
                FunctionKind::Worker,
                Some(name),
                &[],
                body,
                None,
                true,
            )?;
            let dst = stack.current_mut().alloc_reg();
            let wsym = prog.intern(name);
            stack.current_mut().emit(ControlInst::Parallel {
                dst,
                segments: vec![ParallelSegment::WorkerBatch(vec![(wsym, fn_id, captures)])],
            });
            Ok(FlowState::Active(dst))
        }

        WitnessKind::Parallel { body } => lower_parallel(stack, prog, body),

        WitnessKind::Transaction { body, compensation } => {
            let (body_fn, body_captures) = stack.lower_child_fn(
                prog,
                None,
                FunctionKind::TransactionBody,
                None,
                &[],
                body,
                None,
                true,
            )?;
            let (comp_fn, comp_captures) = stack.lower_child_fn(
                prog,
                None,
                FunctionKind::TransactionComp,
                None,
                &[],
                compensation,
                None,
                true,
            )?;
            let dst = stack.current_mut().alloc_reg();
            stack.current_mut().emit(ControlInst::Transaction {
                dst,
                body_fn,
                body_captures,
                comp_fn,
                comp_captures,
            });
            Ok(FlowState::Active(dst))
        }

        WitnessKind::Eval {
            name,
            given,
            expects,
            tolerance,
            replay_path,
        } => {
            let given_reg = match lower_expr(stack, prog, given, false)? {
                FlowState::Active(r) => r,
                FlowState::Terminated => return Ok(FlowState::Terminated),
            };
            let mut expect_regs = Vec::with_capacity(expects.len());
            for e in expects {
                match lower_expr(stack, prog, e, false)? {
                    FlowState::Active(r) => expect_regs.push(r),
                    FlowState::Terminated => return Ok(FlowState::Terminated),
                }
            }
            let sym = prog.intern(name);
            stack.current_mut().emit(ControlInst::EvalAssert {
                name: sym,
                given: given_reg,
                expects: expect_regs,
                tolerance: *tolerance,
                replay_path: replay_path.clone(),
            });
            // Statement value of `eval` matches spec §14.2: evaluates to `given`.
            Ok(FlowState::Active(given_reg))
        }

        WitnessKind::Aggregate { name: _, value } => {
            // Evaluate contributed expression cleanly; value equals contributed expression.
            lower_expr(stack, prog, value, false)
        }

        WitnessKind::Commit => {
            stack.current_mut().emit(ControlInst::Commit);
            Ok(FlowState::Active(stack.current_mut().emit_nil()))
        }

        WitnessKind::Rollback => {
            stack.current_mut().emit(ControlInst::Rollback);
            Ok(FlowState::Active(stack.current_mut().emit_nil()))
        }

        WitnessKind::Sequence(items) => lower_sequence_slice(stack, prog, items, in_tail),

        WitnessKind::Spawn { target } => lower_spawn(stack, prog, target),

        WitnessKind::Select { arms } => lower_select(stack, prog, arms),
    }
}

enum ResolvedCallee {
    Target(CalleeTarget),
    NotOp,
    NegOp,
    AssertOp,
}

fn resolve_callee_target(
    stack: &mut ScopeStack,
    prog: &mut ProgramLowerer,
    callee: &WitnessCallee,
) -> MoraResult<Option<ResolvedCallee>> {
    match callee {
        WitnessCallee::Evaluated(e) => {
            let creg = match lower_expr(stack, prog, e, false)? {
                FlowState::Active(r) => r,
                FlowState::Terminated => return Ok(None),
            };
            Ok(Some(ResolvedCallee::Target(CalleeTarget::Value(creg))))
        }
        WitnessCallee::Name(name)
        | WitnessCallee::GenericName { name, .. }
        | WitnessCallee::Var(name) => {
            if let Some(binding) = stack.resolve_binding(prog, name, false) {
                match binding {
                    VarBinding::Global(g) => match prog.globals[g.0 as usize].kind {
                        GlobalKind::Task(fn_id) => {
                            Ok(Some(ResolvedCallee::Target(CalleeTarget::Fn(fn_id))))
                        }
                        _ => {
                            let creg = emit_load_binding(stack.current_mut(), binding);
                            Ok(Some(ResolvedCallee::Target(CalleeTarget::Value(creg))))
                        }
                    },
                    _ => {
                        let creg = emit_load_binding(stack.current_mut(), binding);
                        Ok(Some(ResolvedCallee::Target(CalleeTarget::Value(creg))))
                    }
                }
            } else if let Some(bfn) = BuiltinFnId::from_name(name) {
                Ok(Some(ResolvedCallee::Target(CalleeTarget::Builtin(bfn))))
            } else {
                let sym = prog.intern(name);
                Ok(Some(ResolvedCallee::Target(CalleeTarget::DynGlobal(sym))))
            }
        }
        WitnessCallee::Builtin(op) => match op {
            BuiltinOp::Print => Ok(Some(ResolvedCallee::Target(CalleeTarget::Builtin(
                BuiltinFnId::Print,
            )))),
            BuiltinOp::Length => Ok(Some(ResolvedCallee::Target(CalleeTarget::Builtin(
                BuiltinFnId::Len,
            )))),
            BuiltinOp::Not => Ok(Some(ResolvedCallee::NotOp)),
            BuiltinOp::Neg => Ok(Some(ResolvedCallee::NegOp)),
            BuiltinOp::Assert => Ok(Some(ResolvedCallee::AssertOp)),
        },
        WitnessCallee::Method(..) => unreachable!("handled by caller"),
    }
}

fn lower_spawn(
    stack: &mut ScopeStack,
    prog: &mut ProgramLowerer,
    target: &WitnessSpawnTarget,
) -> MoraResult<FlowState> {
    let compiled_target = match target {
        WitnessSpawnTarget::MethodCall {
            receiver,
            method,
            args,
            named_args,
        } => {
            let recv_reg = match lower_expr(stack, prog, receiver, false)? {
                FlowState::Active(r) => r,
                FlowState::Terminated => return Ok(FlowState::Terminated),
            };
            let mut arg_regs = Vec::with_capacity(args.len());
            for a in args {
                match lower_expr(stack, prog, a, false)? {
                    FlowState::Active(r) => arg_regs.push(r),
                    FlowState::Terminated => return Ok(FlowState::Terminated),
                }
            }
            let mut named_regs = Vec::with_capacity(named_args.len());
            for (k, v) in named_args {
                match lower_expr(stack, prog, v, false)? {
                    FlowState::Active(r) => named_regs.push((prog.intern(k), r)),
                    FlowState::Terminated => return Ok(FlowState::Terminated),
                }
            }
            ControlSpawnTarget::MethodCall {
                receiver: recv_reg,
                method: prog.intern(method),
                args: arg_regs,
                named_args: named_regs,
            }
        }
        WitnessSpawnTarget::Call {
            callee,
            args,
            named_args,
        } => {
            if let WitnessCallee::Method(obj, method) = callee {
                let recv_reg = if args.is_empty() {
                    stack.emit_read_var(prog, obj)
                } else {
                    match lower_expr(stack, prog, &args[0], false)? {
                        FlowState::Active(r) => r,
                        FlowState::Terminated => return Ok(FlowState::Terminated),
                    }
                };
                let rest = if args.is_empty() { &[] } else { &args[1..] };
                let mut arg_regs = Vec::with_capacity(rest.len());
                for a in rest {
                    match lower_expr(stack, prog, a, false)? {
                        FlowState::Active(r) => arg_regs.push(r),
                        FlowState::Terminated => return Ok(FlowState::Terminated),
                    }
                }
                let mut named_regs = Vec::with_capacity(named_args.len());
                for (k, v) in named_args {
                    match lower_expr(stack, prog, v, false)? {
                        FlowState::Active(r) => named_regs.push((prog.intern(k), r)),
                        FlowState::Terminated => return Ok(FlowState::Terminated),
                    }
                }
                ControlSpawnTarget::MethodCall {
                    receiver: recv_reg,
                    method: prog.intern(method),
                    args: arg_regs,
                    named_args: named_regs,
                }
            } else {
                let Some(resolved) = resolve_callee_target(stack, prog, callee)? else {
                    return Ok(FlowState::Terminated);
                };
                let ResolvedCallee::Target(ct) = resolved else {
                    return Err(MoraError::Runtime(
                        "spawn target must be a callable function or task".to_string(),
                    ));
                };
                let mut arg_regs = Vec::with_capacity(args.len());
                for a in args {
                    match lower_expr(stack, prog, a, false)? {
                        FlowState::Active(r) => arg_regs.push(r),
                        FlowState::Terminated => return Ok(FlowState::Terminated),
                    }
                }
                let mut named_regs = Vec::with_capacity(named_args.len());
                for (k, v) in named_args {
                    match lower_expr(stack, prog, v, false)? {
                        FlowState::Active(r) => named_regs.push((prog.intern(k), r)),
                        FlowState::Terminated => return Ok(FlowState::Terminated),
                    }
                }
                ControlSpawnTarget::Call {
                    target: ct,
                    args: arg_regs,
                    named_args: named_regs,
                }
            }
        }
        WitnessSpawnTarget::Expr(expr) => {
            let reg = match lower_expr(stack, prog, expr, false)? {
                FlowState::Active(r) => r,
                FlowState::Terminated => return Ok(FlowState::Terminated),
            };
            ControlSpawnTarget::Callable(reg)
        }
    };
    let dst = stack.current_mut().alloc_reg();
    stack.current_mut().emit(ControlInst::Spawn {
        dst,
        target: compiled_target,
    });
    Ok(FlowState::Active(dst))
}

fn lower_select(
    stack: &mut ScopeStack,
    prog: &mut ProgramLowerer,
    arms: &[WitnessSelectArm],
) -> MoraResult<FlowState> {
    let mut compiled_arms = Vec::with_capacity(arms.len());
    for arm in arms {
        match arm {
            WitnessSelectArm::Recv {
                binding,
                channel,
                body,
            } => {
                let ch_reg = match lower_expr(stack, prog, channel, false)? {
                    FlowState::Active(r) => r,
                    FlowState::Terminated => return Ok(FlowState::Terminated),
                };
                let params: Vec<String> = binding.iter().cloned().collect();
                let (body_fn, captures) = stack.lower_child_fn(
                    prog,
                    None,
                    FunctionKind::SelectArm,
                    None,
                    &params,
                    body,
                    None,
                    true,
                )?;
                compiled_arms.push(ControlSelectArm::Recv {
                    channel: ch_reg,
                    has_binding: binding.is_some(),
                    body_fn,
                    captures,
                });
            }
            WitnessSelectArm::Send {
                channel,
                value,
                body,
            } => {
                let ch_reg = match lower_expr(stack, prog, channel, false)? {
                    FlowState::Active(r) => r,
                    FlowState::Terminated => return Ok(FlowState::Terminated),
                };
                let val_reg = match lower_expr(stack, prog, value, false)? {
                    FlowState::Active(r) => r,
                    FlowState::Terminated => return Ok(FlowState::Terminated),
                };
                let (body_fn, captures) = stack.lower_child_fn(
                    prog,
                    None,
                    FunctionKind::SelectArm,
                    None,
                    &[],
                    body,
                    None,
                    true,
                )?;
                compiled_arms.push(ControlSelectArm::Send {
                    channel: ch_reg,
                    value: val_reg,
                    body_fn,
                    captures,
                });
            }
            WitnessSelectArm::Timeout { ms, body } => {
                let ms_reg = match lower_expr(stack, prog, ms, false)? {
                    FlowState::Active(r) => r,
                    FlowState::Terminated => return Ok(FlowState::Terminated),
                };
                let (body_fn, captures) = stack.lower_child_fn(
                    prog,
                    None,
                    FunctionKind::SelectArm,
                    None,
                    &[],
                    body,
                    None,
                    true,
                )?;
                compiled_arms.push(ControlSelectArm::Timeout {
                    ms: ms_reg,
                    body_fn,
                    captures,
                });
            }
            WitnessSelectArm::Default { body } => {
                let (body_fn, captures) = stack.lower_child_fn(
                    prog,
                    None,
                    FunctionKind::SelectArm,
                    None,
                    &[],
                    body,
                    None,
                    true,
                )?;
                compiled_arms.push(ControlSelectArm::Default { body_fn, captures });
            }
        }
    }
    let dst = stack.current_mut().alloc_reg();
    stack.current_mut().emit(ControlInst::Select {
        dst,
        arms: compiled_arms,
    });
    Ok(FlowState::Active(dst))
}

fn lower_call(
    stack: &mut ScopeStack,
    prog: &mut ProgramLowerer,
    callee: &WitnessCallee,
    args: &[MirWitness],
    named_args: &[(String, MirWitness)],
    in_tail: bool,
) -> MoraResult<FlowState> {
    // Handle `[]` index expression encoded by parser.
    if matches!(callee, WitnessCallee::Name(n) | WitnessCallee::Var(n) if n == "[]")
        && args.len() == 2
        && named_args.is_empty()
    {
        let obj = match lower_expr(stack, prog, &args[0], false)? {
            FlowState::Active(r) => r,
            FlowState::Terminated => return Ok(FlowState::Terminated),
        };
        let idx = match lower_expr(stack, prog, &args[1], false)? {
            FlowState::Active(r) => r,
            FlowState::Terminated => return Ok(FlowState::Terminated),
        };
        let dst = stack.current_mut().alloc_reg();
        stack.current_mut().emit(ControlInst::Index(dst, obj, idx));
        return Ok(FlowState::Active(dst));
    }

    // `WitnessCallee::Method(obj, m)` uses `args[0]` as receiver if `args` carries receiver,
    // or fallback variable `obj` if zero args (mirroring the typeck side).
    if let WitnessCallee::Method(obj, method) = callee {
        let recv_reg = if args.is_empty() {
            stack.emit_read_var(prog, obj)
        } else {
            match lower_expr(stack, prog, &args[0], false)? {
                FlowState::Active(r) => r,
                FlowState::Terminated => return Ok(FlowState::Terminated),
            }
        };
        let rest_args = if args.is_empty() { &[] } else { &args[1..] };
        let mut arg_regs = Vec::with_capacity(rest_args.len());
        for a in rest_args {
            match lower_expr(stack, prog, a, false)? {
                FlowState::Active(r) => arg_regs.push(r),
                FlowState::Terminated => return Ok(FlowState::Terminated),
            }
        }
        let mut named_regs = Vec::with_capacity(named_args.len());
        for (k, v) in named_args {
            match lower_expr(stack, prog, v, false)? {
                FlowState::Active(r) => named_regs.push((prog.intern(k), r)),
                FlowState::Terminated => return Ok(FlowState::Terminated),
            }
        }
        let dst = stack.current_mut().alloc_reg();
        let method_sym = prog.intern(method);
        stack.current_mut().emit(ControlInst::MethodCall {
            dst,
            receiver: recv_reg,
            method: method_sym,
            args: arg_regs,
            named_args: named_regs,
        });
        return Ok(FlowState::Active(dst));
    }

    // P4-1A: Explicit generic/const constructor call `Linear<4, 3>(w, b)` or `Linear<4, 3>::new(w, b)`.
    if let WitnessCallee::GenericName { name, type_args } = callee {
        let base_type = name.split_once("::").map_or(name.as_str(), |(t, _)| t);
        let mut const_args: Vec<(SymId, i64)> = Vec::new();
        if let Some((type_params, const_params)) = prog.struct_const_meta.get(base_type).cloned() {
            for (tp, arg_hint) in type_params.iter().zip(type_args.iter()) {
                if const_params.contains(tp)
                    && let crate::typeck::Type::ConstInt(n) = arg_hint.to_type()
                {
                    const_args.push((prog.intern(tp), *n));
                }
            }
        }
        let Some(resolved) = resolve_callee_target(stack, prog, callee)? else {
            return Ok(FlowState::Terminated);
        };
        let mut arg_regs = Vec::with_capacity(args.len());
        for a in args {
            match lower_expr(stack, prog, a, false)? {
                FlowState::Active(r) => arg_regs.push(r),
                FlowState::Terminated => return Ok(FlowState::Terminated),
            }
        }
        let mut named_regs: Vec<(SymId, VReg)> = Vec::with_capacity(named_args.len());
        for (k, v) in named_args {
            match lower_expr(stack, prog, v, false)? {
                FlowState::Active(r) => named_regs.push((prog.intern(k), r)),
                FlowState::Terminated => return Ok(FlowState::Terminated),
            }
        }
        if let ResolvedCallee::Target(target) = resolved {
            let dst = stack.current_mut().alloc_reg();
            stack.current_mut().emit(ControlInst::CallGeneric {
                dst,
                target,
                const_args,
                args: arg_regs,
                named_args: named_regs,
            });
            return Ok(FlowState::Active(dst));
        }
    }

    let Some(resolved) = resolve_callee_target(stack, prog, callee)? else {
        return Ok(FlowState::Terminated);
    };

    let mut arg_regs = Vec::with_capacity(args.len());
    for a in args {
        match lower_expr(stack, prog, a, false)? {
            FlowState::Active(r) => arg_regs.push(r),
            FlowState::Terminated => return Ok(FlowState::Terminated),
        }
    }
    let mut named_regs: Vec<(String, VReg)> = Vec::with_capacity(named_args.len());
    for (k, v) in named_args {
        match lower_expr(stack, prog, v, false)? {
            FlowState::Active(r) => named_regs.push((k.clone(), r)),
            FlowState::Terminated => return Ok(FlowState::Terminated),
        }
    }

    match resolved {
        ResolvedCallee::NotOp => {
            let arg = arg_regs
                .into_iter()
                .next()
                .ok_or_else(|| MoraError::Parse("Unary `not` expects 1 argument".to_string()))?;
            let dst = stack.current_mut().alloc_reg();
            stack
                .current_mut()
                .emit(ControlInst::Unary(dst, UnaryOp::Not, arg));
            Ok(FlowState::Active(dst))
        }
        ResolvedCallee::NegOp => {
            let arg = arg_regs
                .into_iter()
                .next()
                .ok_or_else(|| MoraError::Parse("Unary `-` expects 1 argument".to_string()))?;
            let dst = stack.current_mut().alloc_reg();
            stack
                .current_mut()
                .emit(ControlInst::Unary(dst, UnaryOp::Neg, arg));
            Ok(FlowState::Active(dst))
        }
        ResolvedCallee::AssertOp => {
            let arg = arg_regs
                .into_iter()
                .next()
                .ok_or_else(|| MoraError::Parse("`assert` expects 1 argument".to_string()))?;
            let dst = stack.current_mut().alloc_reg();
            stack.current_mut().emit(ControlInst::Assert(dst, arg));
            Ok(FlowState::Active(dst))
        }
        ResolvedCallee::Target(target) => {
            // Stage 3C: Compile-time static keyword reordering & default slot filling
            // for statically resolved `CalleeTarget::Fn(fn_id)` calls.
            let (final_args, final_named) = if let CalleeTarget::Fn(fn_id) = target
                && let Some(param_specs) = prog.task_param_specs.get(&fn_id).cloned()
                && (!named_regs.is_empty()
                    || (arg_regs.len() < param_specs.len()
                        && param_specs[arg_regs.len()..]
                            .iter()
                            .all(|(_, has_def)| *has_def)))
            {
                let mut slots: Vec<Option<VReg>> = vec![None; param_specs.len()];
                if arg_regs.len() > param_specs.len() {
                    // Let runtime arity diagnostic handle too-many positional args consistently.
                    let sym_named = named_regs
                        .into_iter()
                        .map(|(k, r)| (prog.intern(&k), r))
                        .collect();
                    (arg_regs, sym_named)
                } else {
                    for (i, r) in arg_regs.into_iter().enumerate() {
                        slots[i] = Some(r);
                    }
                    for (kw, r) in named_regs {
                        let Some(idx) = param_specs.iter().position(|(pname, _)| pname == &kw)
                        else {
                            return Err(MoraError::Parse(format!(
                                "unknown keyword argument '{}' in call",
                                kw
                            )));
                        };
                        if slots[idx].is_some() {
                            return Err(MoraError::Parse(format!(
                                "multiple values provided for parameter '{}'",
                                kw
                            )));
                        }
                        slots[idx] = Some(r);
                    }
                    let mut out_args = Vec::with_capacity(param_specs.len());
                    for (idx, (pname, has_def)) in param_specs.iter().enumerate() {
                        if let Some(r) = slots[idx] {
                            out_args.push(r);
                        } else if *has_def {
                            let missing = stack.current_mut().emit_const(ControlConst::MissingArg);
                            out_args.push(missing);
                        } else {
                            return Err(MoraError::Parse(format!(
                                "missing required argument '{}'",
                                pname
                            )));
                        }
                    }
                    (out_args, Vec::new())
                }
            } else {
                let sym_named = named_regs
                    .into_iter()
                    .map(|(k, r)| (prog.intern(&k), r))
                    .collect();
                (arg_regs, sym_named)
            };

            let can_tail_call = in_tail
                && final_named.is_empty()
                && stack.current_mut().kind != FunctionKind::Main
                && !matches!(target, CalleeTarget::Builtin(_));
            if can_tail_call {
                stack.current_mut().terminate(ControlTerminator::TailCall {
                    target,
                    args: final_args,
                });
                return Ok(FlowState::Terminated);
            }
            let dst = stack.current_mut().alloc_reg();
            stack.current_mut().emit(ControlInst::Call {
                dst,
                target,
                args: final_args,
                named_args: final_named,
            });
            Ok(FlowState::Active(dst))
        }
    }
}

fn lower_if(
    stack: &mut ScopeStack,
    prog: &mut ProgramLowerer,
    cond: &MirWitness,
    then_w: &MirWitness,
    else_w: Option<&MirWitness>,
    in_tail: bool,
) -> MoraResult<FlowState> {
    let cond_reg = match lower_expr(stack, prog, cond, false)? {
        FlowState::Active(r) => r,
        FlowState::Terminated => return Ok(FlowState::Terminated),
    };
    let cond_bool = stack.current_mut().alloc_reg();
    stack
        .current_mut()
        .emit(ControlInst::Unary(cond_bool, UnaryOp::Truthy, cond_reg));

    let (then_bb, _) = stack.current_mut().new_block(0);
    let (else_bb, _) = stack.current_mut().new_block(0);
    let (merge_bb, merge_params) = stack.current_mut().new_block(1);
    let merge_reg = merge_params[0];

    stack.current_mut().terminate(ControlTerminator::Branch {
        cond: cond_bool,
        then_bb,
        then_args: Vec::new(),
        else_bb,
        else_args: Vec::new(),
    });

    stack.current_mut().branch_depth += 1;
    let mut reached_merge = false;

    stack.current_mut().switch_to(then_bb);
    if let FlowState::Active(tr) = lower_expr(stack, prog, then_w, in_tail)? {
        reached_merge = true;
        stack.current_mut().terminate(ControlTerminator::Jump {
            target: merge_bb,
            args: vec![tr],
        });
    }

    stack.current_mut().switch_to(else_bb);
    match else_w {
        Some(ew) => {
            if let FlowState::Active(er) = lower_expr(stack, prog, ew, in_tail)? {
                reached_merge = true;
                stack.current_mut().terminate(ControlTerminator::Jump {
                    target: merge_bb,
                    args: vec![er],
                });
            }
        }
        None => {
            let nil_r = stack.current_mut().emit_nil();
            reached_merge = true;
            stack.current_mut().terminate(ControlTerminator::Jump {
                target: merge_bb,
                args: vec![nil_r],
            });
        }
    }
    stack.current_mut().branch_depth -= 1;

    if !reached_merge {
        return Ok(FlowState::Terminated);
    }

    stack.current_mut().switch_to(merge_bb);
    Ok(FlowState::Active(merge_reg))
}

fn lower_while(
    stack: &mut ScopeStack,
    prog: &mut ProgramLowerer,
    cond: &MirWitness,
    body: &MirWitness,
) -> MoraResult<FlowState> {
    let (header_bb, _) = stack.current_mut().new_block(0);
    let (body_bb, _) = stack.current_mut().new_block(0);
    let (exit_bb, _) = stack.current_mut().new_block(0);

    stack.current_mut().terminate(ControlTerminator::Jump {
        target: header_bb,
        args: Vec::new(),
    });

    stack.current_mut().switch_to(header_bb);
    match lower_expr(stack, prog, cond, false)? {
        FlowState::Active(cond_reg) => {
            let cb = stack.current_mut().alloc_reg();
            stack
                .current_mut()
                .emit(ControlInst::Unary(cb, UnaryOp::Truthy, cond_reg));
            stack.current_mut().terminate(ControlTerminator::Branch {
                cond: cb,
                then_bb: body_bb,
                then_args: Vec::new(),
                else_bb: exit_bb,
                else_args: Vec::new(),
            });
        }
        FlowState::Terminated => {}
    }

    stack.current_mut().switch_to(body_bb);
    stack.current_mut().loop_stack.push(LoopTargets {
        continue_bb: header_bb,
        break_bb: exit_bb,
    });
    stack.current_mut().loop_depth += 1;
    let body_res = lower_expr(stack, prog, body, false);
    stack.current_mut().loop_depth -= 1;
    stack.current_mut().loop_stack.pop();

    if let FlowState::Active(_) = body_res? {
        stack.current_mut().terminate(ControlTerminator::Jump {
            target: header_bb,
            args: Vec::new(),
        });
    }

    stack.current_mut().switch_to(exit_bb);
    Ok(FlowState::Active(stack.current_mut().emit_nil()))
}

fn lower_for_loop(
    stack: &mut ScopeStack,
    prog: &mut ProgramLowerer,
    var: &str,
    iterable: &MirWitness,
    body: &MirWitness,
) -> MoraResult<FlowState> {
    let iter_reg = match lower_expr(stack, prog, iterable, false)? {
        FlowState::Active(r) => r,
        FlowState::Terminated => return Ok(FlowState::Terminated),
    };
    let len_reg = stack.current_mut().alloc_reg();
    stack.current_mut().emit(ControlInst::Call {
        dst: len_reg,
        target: CalleeTarget::Builtin(BuiltinFnId::Len),
        args: vec![iter_reg],
        named_args: Vec::new(),
    });
    let zero_reg = stack.current_mut().emit_const(ControlConst::Int(0));
    let one_reg = stack.current_mut().emit_const(ControlConst::Int(1));

    let (header_bb, header_params) = stack.current_mut().new_block(1);
    let idx_reg = header_params[0];
    let (body_bb, _) = stack.current_mut().new_block(0);
    let (step_bb, _) = stack.current_mut().new_block(0);
    let (exit_bb, _) = stack.current_mut().new_block(0);

    stack.current_mut().terminate(ControlTerminator::Jump {
        target: header_bb,
        args: vec![zero_reg],
    });

    // header(idx): cond = idx < len
    stack.current_mut().switch_to(header_bb);
    let cmp_reg = stack.current_mut().alloc_reg();
    stack.current_mut().emit(ControlInst::Binary(
        cmp_reg,
        idx_reg,
        crate::common::BinaryOp::Less,
        len_reg,
    ));
    stack.current_mut().terminate(ControlTerminator::Branch {
        cond: cmp_reg,
        then_bb: body_bb,
        then_args: Vec::new(),
        else_bb: exit_bb,
        else_args: Vec::new(),
    });

    // step: next_idx = idx + 1; jump header(next_idx)
    stack.current_mut().switch_to(step_bb);
    let next_idx = stack.current_mut().alloc_reg();
    stack.current_mut().emit(ControlInst::Binary(
        next_idx,
        idx_reg,
        crate::common::BinaryOp::Add,
        one_reg,
    ));
    stack.current_mut().terminate(ControlTerminator::Jump {
        target: header_bb,
        args: vec![next_idx],
    });

    // body: bind `var = iter[idx]` in lexical scope so outer variable with same name isn't clobbered.
    stack.current_mut().switch_to(body_bb);
    stack.current_mut().push_lexical_scope();
    let var_slot = stack.current_mut().bind_block_local(var, prog);
    let elem_reg = stack.current_mut().alloc_reg();
    stack
        .current_mut()
        .emit(ControlInst::Index(elem_reg, iter_reg, idx_reg));
    stack
        .current_mut()
        .emit(ControlInst::StoreLocal(var_slot, elem_reg));

    stack.current_mut().loop_stack.push(LoopTargets {
        continue_bb: step_bb,
        break_bb: exit_bb,
    });
    stack.current_mut().loop_depth += 1;
    let body_res = lower_expr(stack, prog, body, false);
    stack.current_mut().loop_depth -= 1;
    stack.current_mut().loop_stack.pop();
    stack.current_mut().pop_lexical_scope();

    if let FlowState::Active(_) = body_res? {
        stack.current_mut().terminate(ControlTerminator::Jump {
            target: step_bb,
            args: Vec::new(),
        });
    }

    stack.current_mut().switch_to(exit_bb);
    Ok(FlowState::Active(stack.current_mut().emit_nil()))
}

fn lower_match(
    stack: &mut ScopeStack,
    prog: &mut ProgramLowerer,
    scrutinee: &MirWitness,
    arms: &[crate::mir::witness::WitnessArm],
    in_tail: bool,
) -> MoraResult<FlowState> {
    let scrut_reg = match lower_expr(stack, prog, scrutinee, false)? {
        FlowState::Active(r) => r,
        FlowState::Terminated => return Ok(FlowState::Terminated),
    };

    let (merge_bb, merge_params) = stack.current_mut().new_block(1);
    let merge_reg = merge_params[0];

    stack.current_mut().branch_depth += 1;

    for arm in arms {
        stack.current_mut().push_lexical_scope();
        let compiled_pat = compile_pattern(stack, prog, &arm.pattern);
        let ok_reg = stack.current_mut().alloc_reg();
        stack.current_mut().emit(ControlInst::MatchPattern {
            dst_ok: ok_reg,
            val: scrut_reg,
            pattern: compiled_pat,
        });

        let (matched_bb, _) = stack.current_mut().new_block(0);
        let (next_arm_bb, _) = stack.current_mut().new_block(0);
        stack.current_mut().terminate(ControlTerminator::Branch {
            cond: ok_reg,
            then_bb: matched_bb,
            then_args: Vec::new(),
            else_bb: next_arm_bb,
            else_args: Vec::new(),
        });

        stack.current_mut().switch_to(matched_bb);
        if let Some(guard_w) = &arm.guard {
            match lower_expr(stack, prog, guard_w, false)? {
                FlowState::Active(greg) => {
                    let gbool = stack.current_mut().alloc_reg();
                    stack
                        .current_mut()
                        .emit(ControlInst::Unary(gbool, UnaryOp::Truthy, greg));
                    let (body_bb, _) = stack.current_mut().new_block(0);
                    stack.current_mut().terminate(ControlTerminator::Branch {
                        cond: gbool,
                        then_bb: body_bb,
                        then_args: Vec::new(),
                        else_bb: next_arm_bb,
                        else_args: Vec::new(),
                    });
                    stack.current_mut().switch_to(body_bb);
                }
                FlowState::Terminated => {
                    stack.current_mut().pop_lexical_scope();
                    stack.current_mut().switch_to(next_arm_bb);
                    continue;
                }
            }
        }

        if let FlowState::Active(arm_val) = lower_expr(stack, prog, &arm.body, in_tail)? {
            stack.current_mut().terminate(ControlTerminator::Jump {
                target: merge_bb,
                args: vec![arm_val],
            });
        }
        stack.current_mut().pop_lexical_scope();
        stack.current_mut().switch_to(next_arm_bb);
    }

    stack.current_mut().branch_depth -= 1;

    // Fallthrough when no arm matched yields Nil (spec / parser_recovery contract).
    let nil_r = stack.current_mut().emit_nil();
    stack.current_mut().terminate(ControlTerminator::Jump {
        target: merge_bb,
        args: vec![nil_r],
    });

    stack.current_mut().switch_to(merge_bb);
    Ok(FlowState::Active(merge_reg))
}

fn lower_parallel(
    stack: &mut ScopeStack,
    prog: &mut ProgramLowerer,
    body: &MirWitness,
) -> MoraResult<FlowState> {
    if !sequence_has_worker(body) {
        // Pure non-worker parallel block executes statements inline in enclosing scope,
        // preserving `let`/`assign` visibility outside `parallel` (D121, D166), evaluating to Nil.
        match lower_expr(stack, prog, body, false)? {
            FlowState::Active(_) => return Ok(FlowState::Active(stack.current_mut().emit_nil())),
            FlowState::Terminated => return Ok(FlowState::Terminated),
        }
    }

    let mut items = Vec::new();
    collect_parallel_items(body, &mut items);

    let mut segments: Vec<ParallelSegment> = Vec::new();
    let mut pending_workers: Vec<(SymId, FnId, Vec<CaptureSource>)> = Vec::new();
    let mut pending_seq: Vec<MirWitness> = Vec::new();

    for item in items {
        if let WitnessKind::Worker { name, body: wbody } = &item.kind {
            if !pending_seq.is_empty() {
                let seq_w = MirWitness {
                    kind: WitnessKind::Sequence(std::mem::take(&mut pending_seq)),
                    span: item.span,
                };
                let (fn_id, caps) = stack.lower_child_fn(
                    prog,
                    None,
                    FunctionKind::ParallelSeq,
                    None,
                    &[],
                    &seq_w,
                    None,
                    true,
                )?;
                segments.push(ParallelSegment::Sequential(fn_id, caps));
            }
            let (fn_id, caps) = stack.lower_child_fn(
                prog,
                None,
                FunctionKind::Worker,
                Some(name),
                &[],
                wbody,
                None,
                true,
            )?;
            let wsym = prog.intern(name);
            pending_workers.push((wsym, fn_id, caps));
        } else {
            if !pending_workers.is_empty() {
                segments.push(ParallelSegment::WorkerBatch(std::mem::take(
                    &mut pending_workers,
                )));
            }
            pending_seq.push(item.clone());
        }
    }

    if !pending_workers.is_empty() {
        segments.push(ParallelSegment::WorkerBatch(pending_workers));
    }
    if !pending_seq.is_empty() {
        let span = pending_seq[0].span;
        let seq_w = MirWitness {
            kind: WitnessKind::Sequence(pending_seq),
            span,
        };
        let (fn_id, caps) = stack.lower_child_fn(
            prog,
            None,
            FunctionKind::ParallelSeq,
            None,
            &[],
            &seq_w,
            None,
            true,
        )?;
        segments.push(ParallelSegment::Sequential(fn_id, caps));
    }

    let dst = stack.current_mut().alloc_reg();
    stack
        .current_mut()
        .emit(ControlInst::Parallel { dst, segments });
    Ok(FlowState::Active(dst))
}
