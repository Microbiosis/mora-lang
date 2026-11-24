//! v0.110.0: Heap-allocated stackful VM frames, upvalues, module globals, and delimited continuations.
//!
//! All types are `Send + Sync` so `Value` (including `Closure`, `Task`, `Macro`, and
//! `Continuation`) can safely cross thread boundaries for `parallel` worker scopes and the
//! shared zero-dependency [`crate::runtime::scheduler::TaskScheduler`].
//!
//! Invariants:
//! - A [`ScriptFrame`] owns its local slots and register file; a local captured by an inner
//!   closure becomes a heap [`UpvalueCell`] so the closure observes the defining frame's
//!   later mutations.
//! - A [`CapturedContinuation`] is multi-shot: it owns the frames stacked above the matching
//!   [`HandleBarrierState`], and resuming clones them back instead of consuming the capture.

use std::collections::HashMap;
use std::sync::Arc;

use crate::common::sync::Mutex;

use crate::error::{MoraError, MoraResult};
use crate::mir::control::{
    BlockId, CaptureSource, ControlFunction, ControlProgram, ControlSelectArm, FnId, FunctionKind,
    LocalSlot, ParallelSegment, UpvalueSlot, VReg,
};
use crate::value::{Environment, Value};

/// P4-2: Compact 16-byte hybrid unboxed VM register slot replacing full `Value` in
/// `ScriptFrame.regs` and unshared `LocalCell`s.
///
/// Primitives (`Nil`, `Bool`, `Int`, `Float`) live completely inline inside the 16-byte slot
/// with zero heap allocations on hot numeric, comparison, and control-flow paths; heap-backed
/// values (`String`, `List`, `Dict`, `Tensor`, `StructInstance`, closures, etc.) are stored
/// behind an 8-byte `Box<Value>` pointer.
#[derive(Debug, Clone)]
pub enum RegSlot {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Boxed(Box<Value>),
}

const _: () = assert!(std::mem::size_of::<RegSlot>() == 16);

impl RegSlot {
    /// Pack a full [`Value`] into a 16-byte [`RegSlot`], unboxing primitives inline.
    #[inline]
    pub fn from_value(val: Value) -> Self {
        match val {
            Value::Nil => Self::Nil,
            Value::Bool(b) => Self::Bool(b),
            Value::Int(i) => Self::Int(i),
            Value::Float(f) => Self::Float(f),
            other => Self::Boxed(Box::new(other)),
        }
    }

    /// Clone out a full [`Value`] from this register slot.
    #[inline]
    pub fn to_value(&self) -> Value {
        match self {
            Self::Nil => Value::Nil,
            Self::Bool(b) => Value::Bool(*b),
            Self::Int(i) => Value::Int(*i),
            Self::Float(f) => Value::Float(*f),
            Self::Boxed(v) => (**v).clone(),
        }
    }

    /// Consume this register slot into a full [`Value`].
    #[inline]
    pub(crate) fn take_value(self) -> Value {
        match self {
            Self::Nil => Value::Nil,
            Self::Bool(b) => Value::Bool(b),
            Self::Int(i) => Value::Int(i),
            Self::Float(f) => Value::Float(f),
            Self::Boxed(v) => *v,
        }
    }

    /// Zero-allocation truthiness test matching [`crate::flow::is_truthy`].
    #[inline]
    pub(crate) fn is_truthy(&self) -> bool {
        match self {
            Self::Nil => false,
            Self::Bool(b) => *b,
            Self::Int(i) => *i != 0,
            Self::Float(f) => *f != 0.0 && !f.is_nan(),
            Self::Boxed(v) => crate::flow::is_truthy(v),
        }
    }

    /// Borrow the inner [`Value`] when this slot is heap-boxed.
    #[inline]
    pub(crate) fn as_boxed(&self) -> Option<&Value> {
        match self {
            Self::Boxed(v) => Some(v),
            _ => None,
        }
    }
}

/// Shared stack-frame slice captured by both multi-shot delimited continuations
/// (`perform`/`handle`) and cooperative M:N VM fiber suspensions (`VmYield`).
pub type CapturedFrames = Vec<CallFrame>;

/// Shared cell holding a module-top static global binding `(value, is_exported)`.
pub type GlobalCell = Arc<Mutex<(Value, bool)>>;

/// Per-module global slot storage vector.
pub type ModuleGlobals = Arc<Vec<GlobalCell>>;

/// Interior-mutable heap cell backing locals captured by closures and mutated across boundaries.
pub type UpvalueCell = Arc<Mutex<Value>>;

/// Lexical upvalue entry stored in a `Closure` or `ScriptFrame`.
#[derive(Debug, Clone)]
pub enum UpvalueValue {
    /// Immutable capture-by-value copy.
    Const(Value),
    /// Shared mutable cell visible across enclosing frame and inner closures.
    Cell(UpvalueCell),
}

impl UpvalueValue {
    /// Read the current value stored in this upvalue.
    pub fn get(&self) -> Value {
        match self {
            Self::Const(v) => v.clone(),
            Self::Cell(cell) => cell.lock().clone(),
        }
    }

    /// Update a mutable shared upvalue cell.
    pub fn set(&self, val: Value) -> MoraResult<()> {
        match self {
            Self::Cell(cell) => {
                *cell.lock() = val;
                Ok(())
            }
            Self::Const(_) => Err(MoraError::Runtime(
                "cannot mutate immutable captured upvalue".to_string(),
            )),
        }
    }

    /// Stable pointer key for shared cell deduplication / worker isolation.
    pub fn cell_key(&self) -> Option<usize> {
        match self {
            Self::Cell(cell) => Some(Arc::as_ptr(cell) as usize),
            Self::Const(_) => None,
        }
    }
}

/// Per-frame local variable slot storage.
#[derive(Debug, Clone)]
pub enum LocalCell {
    /// Direct frame-local unboxed 16-byte register slot; cloned independently across multi-shot continuations.
    Val(RegSlot),
    /// Boxed heap cell shared with inner closures capturing this local.
    Shared(UpvalueCell),
}

impl LocalCell {
    /// Read the current value stored in this local slot as a [`Value`].
    #[inline]
    pub fn get(&self) -> Value {
        match self {
            Self::Val(s) => s.to_value(),
            Self::Shared(cell) => cell.lock().clone(),
        }
    }

    /// Read this local directly into a 16-byte [`RegSlot`] without boxing primitives.
    #[inline]
    pub(crate) fn get_slot(&self) -> RegSlot {
        match self {
            Self::Val(s) => s.clone(),
            Self::Shared(cell) => RegSlot::from_value(cell.lock().clone()),
        }
    }

    /// Write a [`Value`] into this local slot, preserving shared cell identity.
    #[inline]
    pub fn set(&mut self, val: Value) {
        match self {
            Self::Val(s) => *s = RegSlot::from_value(val),
            Self::Shared(cell) => *cell.lock() = val,
        }
    }

    /// Write a 16-byte [`RegSlot`] into this local slot without boxing unshared primitives.
    #[inline]
    pub(crate) fn set_slot(&mut self, slot: RegSlot) {
        match self {
            Self::Val(s) => *s = slot,
            Self::Shared(cell) => *cell.lock() = slot.take_value(),
        }
    }

    /// Capture this local slot for an inner closure according to `shared`.
    pub fn capture(&self, shared: bool) -> UpvalueValue {
        match (self, shared) {
            (Self::Shared(cell), true) => UpvalueValue::Cell(Arc::clone(cell)),
            (Self::Shared(cell), false) => UpvalueValue::Const(cell.lock().clone()),
            (Self::Val(s), false) => UpvalueValue::Const(s.to_value()),
            (Self::Val(s), true) => UpvalueValue::Cell(Arc::new(Mutex::new(s.to_value()))),
        }
    }
}

/// Heap-allocated call frame executing a compiled [`ControlFunction`].
#[derive(Debug, Clone)]
pub struct ScriptFrame {
    pub program: Arc<ControlProgram>,
    pub func_id: FnId,
    pub globals: ModuleGlobals,
    pub upvalues: Arc<Vec<UpvalueValue>>,
    pub locals: Vec<LocalCell>,
    /// P4-2: 16-byte hybrid unboxed register file.
    pub regs: Vec<RegSlot>,
    pub block_id: BlockId,
    pub pc: usize,
    pub resume_dst: Option<VReg>,
    /// G2: LIFO stack of deferred cleanup closures registered via `defer ...` in this frame.
    pub defers: Vec<(FnId, Arc<Vec<UpvalueValue>>)>,
}

impl ScriptFrame {
    /// Borrow the active [`ControlFunction`] descriptor from `self.program`.
    #[inline]
    pub fn func(&self) -> &ControlFunction {
        self.program.func(self.func_id)
    }

    /// Read a virtual register as a full [`Value`].
    #[inline]
    pub(crate) fn get_val(&self, r: VReg) -> Value {
        self.regs[r.0 as usize].to_value()
    }

    /// Write a full [`Value`] into a virtual register (unboxing primitives inline).
    #[inline]
    pub(crate) fn set_val(&mut self, r: VReg, val: Value) {
        self.regs[r.0 as usize] = RegSlot::from_value(val);
    }

    /// Write an already-unboxed [`RegSlot`] directly into a virtual register.
    #[inline]
    pub(crate) fn set_slot(&mut self, r: VReg, slot: RegSlot) {
        self.regs[r.0 as usize] = slot;
    }

    /// Deliver `reply` when control returns to this frame from a callee or resumed continuation.
    pub fn receive_reply(&mut self, reply: Value) {
        if let Some(dst) = self.resume_dst.take() {
            self.regs[dst.0 as usize] = RegSlot::from_value(reply);
            return;
        }
        let block = &self.func().blocks[self.block_id.0 as usize];
        if self.pc == 0
            && let Some(&param_reg) = block.params.first()
        {
            self.regs[param_reg.0 as usize] = RegSlot::from_value(reply);
        }
    }
}

/// Deep algebraic-effect barrier installed by `ControlInst::Handle`.
#[derive(Debug, Clone)]
pub struct HandleBarrierState {
    pub effect_name: String,
    pub handler_program: Arc<ControlProgram>,
    pub handler_fn: FnId,
    pub handler_globals: ModuleGlobals,
    pub handler_upvalues: Arc<Vec<UpvalueValue>>,
    pub auto_resume: bool,
}

/// Boxed state for resuming the winning arm of a suspended `select` instruction.
#[derive(Debug, Clone)]
pub struct SelectResumeState {
    pub program: Arc<ControlProgram>,
    pub globals: ModuleGlobals,
    pub parent_locals: Vec<LocalCell>,
    pub parent_upvalues: Arc<Vec<UpvalueValue>>,
    pub arms: Vec<ControlSelectArm>,
}

/// Boxed state for evaluating omitted `struct` field default initializers on the VM stack.
#[derive(Debug, Clone)]
pub struct StructInitState {
    pub def: Arc<crate::value::StructDefRuntime>,
    pub fields: HashMap<String, Value>,
    pub const_args: Vec<(String, i64)>,
    pub pending_defaults: Vec<(String, FnId)>,
    pub current_field: String,
}

/// Iterative state machine frames for higher-order builtins and collection methods.
#[derive(Debug, Clone)]
pub enum CombinatorFrame {
    /// Left-to-right `compose(f1, f2, ...)` invocation.
    Compose { funcs: Vec<Value>, next_idx: usize },
    /// `list.map(f)` accumulation state.
    ListMap {
        func: Value,
        items: Vec<Value>,
        idx: usize,
        results: Vec<Value>,
    },
    /// `list.filter(pred)` accumulation state.
    ListFilter {
        pred: Value,
        items: Vec<Value>,
        idx: usize,
        results: Vec<Value>,
    },
    /// `list.reduce(fn, init)` folding state.
    ListReduce {
        func: Value,
        items: Vec<Value>,
        idx: usize,
    },
    /// `into(list, transform)` flat-collect state.
    Into {
        transform: Value,
        items: Vec<Value>,
        idx: usize,
        results: Vec<Value>,
    },
    /// `swap(atom, f)` writeback awaiting `f(old)` return value.
    Swap { atom: Arc<Mutex<Value>> },
    /// Reverse-mode autograd wrapper around a differentiated function call.
    AutogradEval {
        tape_id: u64,
        leaf_nodes: Vec<usize>,
        /// `Some(dt)` if the original input was a scalar float/ScalarValue (unwrap 0-D grad tensor back to scalar);
        /// `None` if the original input was a `Value::Tensor`.
        scalar_flags: Vec<Option<crate::common::DType>>,
        return_value: bool,
    },
    /// Forward-mode JVP evaluation (`jvp(f, primals, tangents)`).
    JvpEval {
        tape_id: u64,
        leaf_nodes: Vec<usize>,
        leaf_tangents: Vec<Arc<crate::value::tensor::Tensor>>,
        scalar_out_dtype: Option<crate::common::DType>,
    },
    /// Compute graph tracing wrapper (`trace_dag(f, example_inputs...)`).
    TraceDagEval {
        tape_id: u64,
        leaf_nodes: Vec<usize>,
    },
    /// Evaluates omitted `struct` field default initializers on the VM stack.
    StructInit(Box<StructInitState>),
    /// P4-1A: Negates boolean result returned from `Eq::eq` when evaluating `!=` on overloaded types.
    BoolNot,
    /// P4-2: Resumes the winning arm body after an asynchronous `select` future resolves.
    SelectResume(Box<SelectResumeState>),
    /// Dispatches rising-edge memory watermark callbacks outside allocator locks
    /// on the stackful VM frame stack before yielding `final_result`.
    WatermarkCallbacks {
        final_result: Value,
        pending: Vec<(Value, HashMap<String, Value>)>,
    },
}

/// Snapshot & compensation state for `ControlInst::Transaction`.
#[derive(Debug, Clone)]
pub struct TransactionState {
    pub saved_env: Environment,
    pub saved_globals_vals: Vec<(Value, bool)>,
    pub target_globals: ModuleGlobals,
    pub comp_program: Arc<ControlProgram>,
    pub comp_fn: FnId,
    pub comp_upvalues: Arc<Vec<UpvalueValue>>,
    pub in_compensation: bool,
}

/// Continuation frame driving mixed `worker` batches and sequential statements in `parallel`.
#[derive(Debug, Clone)]
pub struct ParallelFrame {
    pub program: Arc<ControlProgram>,
    pub globals: ModuleGlobals,
    pub parent_upvalues: Arc<Vec<UpvalueValue>>,
    pub parent_locals: Vec<LocalCell>,
    pub segments: Vec<ParallelSegment>,
    pub next_idx: usize,
    pub last_val: Value,
}

/// Polymorphic execution frame on the VM's explicit heap call stack.
#[derive(Debug, Clone)]
pub enum CallFrame {
    /// Executing a compiled SSA `ControlFunction`.
    Script(Box<ScriptFrame>),
    /// Active algebraic effect handler boundary (`handle E { ... } { ... }`).
    HandleBarrier(HandleBarrierState),
    /// Automatic continuation resumption when a handler body omits `resume`.
    AutoResume(Arc<CapturedContinuation>),
    /// Higher-order combinator loop (`map`, `filter`, `reduce`, `compose`, `into`, `swap`).
    Combinator(CombinatorFrame),
    /// ACID transaction checkpoint wrapping body execution and compensation fallback.
    Transaction(TransactionState),
    /// Import boundary preserving caller environment while publishing only exports.
    ImportFinalize {
        saved_env: Environment,
        child_program: Arc<ControlProgram>,
        child_globals: ModuleGlobals,
    },
    /// Dynamic `eval(...)` boundary restoring outer `Environment` after sub-program exit.
    EvalRestore { saved_env: Environment },
    /// Parallel block driver executing worker batches and sequential slices.
    Parallel(ParallelFrame),
    /// Wraps propagated errors with a diagnostic prefix (e.g. macro invocation errors).
    ErrorContext { prefix: String },
    /// Restores `autograd.no_grad_depth` on normal return or error unwind.
    NoGradGuard,
    /// G2: Drains a frame's registered `defer` closures in LIFO order before finalizing
    /// a normal return, tail call, or error unwind.
    DeferDrain {
        program: Arc<ControlProgram>,
        globals: ModuleGlobals,
        remaining: Vec<(FnId, Arc<Vec<UpvalueValue>>)>,
        outcome: Option<Result<Value, MoraError>>,
    },
}

/// Multi-shot delimited continuation captured up to the matching [`HandleBarrierState`].
#[derive(Debug, Clone)]
pub struct CapturedContinuation {
    pub effect_name: String,
    pub barrier: HandleBarrierState,
    /// Captured frames ordered bottom-to-top above the barrier.
    pub frames: CapturedFrames,
}

/// Initialize module global cells from `prog.globals`.
pub fn init_globals(prog: &ControlProgram) -> ModuleGlobals {
    let cells = prog
        .globals
        .iter()
        .map(|meta| Arc::new(Mutex::new((Value::Nil, meta.exported))))
        .collect();
    Arc::new(cells)
}

/// Snapshot all module globals as `(value, is_exported)` tuples.
pub fn snapshot_globals(globals: &ModuleGlobals) -> Vec<(Value, bool)> {
    globals.iter().map(|cell| cell.lock().clone()).collect()
}

/// Restore module globals from a previous [`snapshot_globals`] slice.
pub fn restore_globals(globals: &ModuleGlobals, snap: &[(Value, bool)]) {
    for (cell, saved) in globals.iter().zip(snap.iter()) {
        *cell.lock() = saved.clone();
    }
}

/// Clone module globals into isolated mutex cells for concurrent worker execution.
pub fn clone_isolated_globals(snap: &[(Value, bool)]) -> ModuleGlobals {
    Arc::new(
        snap.iter()
            .map(|entry| Arc::new(Mutex::new(entry.clone())))
            .collect(),
    )
}

/// Resolve closure/sub-function `CaptureSource` descriptors from the active parent frame.
pub fn resolve_captures(
    captures: &[CaptureSource],
    parent_locals: &[LocalCell],
    parent_upvalues: &[UpvalueValue],
    child_func: &ControlFunction,
) -> Vec<UpvalueValue> {
    captures
        .iter()
        .enumerate()
        .map(|(idx, src)| {
            let want_shared = child_func
                .upvalues
                .get(idx)
                .map(|d| d.shared)
                .unwrap_or(false);
            match src {
                CaptureSource::ParentLocal(LocalSlot(s)) => {
                    parent_locals[*s as usize].capture(want_shared)
                }
                CaptureSource::ParentUpvalue(UpvalueSlot(u)) => match &parent_upvalues[*u as usize]
                {
                    UpvalueValue::Cell(c) if want_shared => UpvalueValue::Cell(Arc::clone(c)),
                    UpvalueValue::Cell(c) => UpvalueValue::Const(c.lock().clone()),
                    UpvalueValue::Const(v) if want_shared => {
                        UpvalueValue::Cell(Arc::new(Mutex::new(v.clone())))
                    }
                    UpvalueValue::Const(v) => UpvalueValue::Const(v.clone()),
                },
            }
        })
        .collect()
}

/// Clone worker upvalue slices while isolating shared mutable cells per concurrent worker.
pub fn clone_worker_upvalues(
    upvalues: &[UpvalueValue],
    child_func: &ControlFunction,
    cell_map: &mut HashMap<usize, UpvalueCell>,
) -> Vec<UpvalueValue> {
    upvalues
        .iter()
        .enumerate()
        .map(|(idx, uv)| match uv {
            UpvalueValue::Const(v) => UpvalueValue::Const(v.clone()),
            UpvalueValue::Cell(cell) => {
                let ptr = Arc::as_ptr(cell) as usize;
                let isolated = cell_map
                    .entry(ptr)
                    .or_insert_with(|| Arc::new(Mutex::new(cell.lock().clone())));
                let want_shared = child_func
                    .upvalues
                    .get(idx)
                    .map(|d| d.shared)
                    .unwrap_or(true);
                if want_shared {
                    UpvalueValue::Cell(Arc::clone(isolated))
                } else {
                    UpvalueValue::Const(isolated.lock().clone())
                }
            }
        })
        .collect()
}

/// Construct a new [`ScriptFrame`] for calling `func_id`, verifying arity for Tasks/Closures/Macros.
pub fn materialize_frame(
    program: Arc<ControlProgram>,
    func_id: FnId,
    globals: ModuleGlobals,
    upvalues: Arc<Vec<UpvalueValue>>,
    args: &[Value],
) -> MoraResult<ScriptFrame> {
    materialize_frame_named(program, func_id, globals, upvalues, args, &[])
}

/// Construct a new [`ScriptFrame`] with positional + optional named keyword arguments,
/// reordering named arguments into parameter slots and populating omitted defaulted parameters
/// with [`Value::MissingArg`] for the callee's entry prologue.
pub fn materialize_frame_named(
    program: Arc<ControlProgram>,
    func_id: FnId,
    globals: ModuleGlobals,
    upvalues: Arc<Vec<UpvalueValue>>,
    args: &[Value],
    named_args: &[(String, Value)],
) -> MoraResult<ScriptFrame> {
    let (locals, regs, block_id) = {
        let func = program.func(func_id);
        let needs_arity_check = matches!(
            func.kind,
            FunctionKind::Task
                | FunctionKind::Closure
                | FunctionKind::Macro
                | FunctionKind::ImplMethod
                | FunctionKind::TraitDefault
        );
        let kind_label = match func.kind {
            FunctionKind::Task => "task",
            FunctionKind::Closure => "closure",
            FunctionKind::Macro => "macro",
            FunctionKind::ImplMethod | FunctionKind::TraitDefault => "method",
            _ => "function",
        };

        let resolved_args: Vec<Value> = if needs_arity_check {
            let n_params = func.params.len();
            if args.len() > n_params {
                if func.kind == FunctionKind::Macro {
                    let mname = func
                        .name
                        .map(|s| program.symbols.resolve(s))
                        .unwrap_or("<macro>");
                    return Err(MoraError::Runtime(format!(
                        "macro '{}' expects {} args, got {}",
                        mname,
                        n_params,
                        args.len()
                    )));
                }
                return Err(MoraError::Runtime(format!(
                    "{kind_label} expects {} args, got {}",
                    n_params,
                    args.len()
                )));
            }
            if named_args.is_empty() && args.len() == n_params {
                args.to_vec()
            } else {
                let mut slots: Vec<Option<Value>> = vec![None; n_params];
                for (i, v) in args.iter().enumerate() {
                    slots[i] = Some(v.clone());
                }
                for (kw, val) in named_args {
                    let pos = func
                        .params
                        .iter()
                        .position(|&sym| program.symbols.resolve(sym) == kw.as_str());
                    let Some(idx) = pos else {
                        return Err(MoraError::Runtime(format!(
                            "{kind_label} got an unexpected keyword argument '{kw}'"
                        )));
                    };
                    if slots[idx].is_some() {
                        return Err(MoraError::Runtime(format!(
                            "{kind_label} got multiple values for argument '{kw}'"
                        )));
                    }
                    slots[idx] = Some(val.clone());
                }
                let mut out = Vec::with_capacity(n_params);
                for (idx, slot) in slots.into_iter().enumerate() {
                    if let Some(v) = slot {
                        out.push(v);
                    } else if func.param_has_default.get(idx).copied().unwrap_or(false) {
                        out.push(Value::MissingArg);
                    } else if named_args.is_empty() {
                        if func.kind == FunctionKind::Macro {
                            let mname = func
                                .name
                                .map(|s| program.symbols.resolve(s))
                                .unwrap_or("<macro>");
                            return Err(MoraError::Runtime(format!(
                                "macro '{}' expects {} args, got {}",
                                mname,
                                n_params,
                                args.len()
                            )));
                        }
                        return Err(MoraError::Runtime(format!(
                            "{kind_label} expects {} args, got {}",
                            n_params,
                            args.len()
                        )));
                    } else {
                        let pname = program.symbols.resolve(func.params[idx]);
                        return Err(MoraError::Runtime(format!(
                            "{kind_label} missing required argument '{pname}'"
                        )));
                    }
                }
                out
            }
        } else {
            args.to_vec()
        };

        let n_locals = func.n_locals as usize;
        let mut locals = Vec::with_capacity(n_locals);
        for slot_idx in 0..n_locals {
            if func.shared_locals.get(slot_idx).copied().unwrap_or(false) {
                locals.push(LocalCell::Shared(Arc::new(Mutex::new(Value::Nil))));
            } else {
                locals.push(LocalCell::Val(RegSlot::Nil));
            }
        }

        for (slot_idx, val) in resolved_args.iter().enumerate() {
            if slot_idx < locals.len() {
                locals[slot_idx].set(val.clone());
            }
        }

        let regs = vec![RegSlot::Nil; func.n_regs as usize];
        (locals, regs, func.entry)
    };

    Ok(ScriptFrame {
        program,
        func_id,
        globals,
        upvalues,
        locals,
        regs,
        block_id,
        pc: 0,
        resume_dst: None,
        defers: Vec::new(),
    })
}

/// Bind `perform` arguments and captured [`CapturedContinuation`] into a `HandleHandler` frame.
pub fn materialize_handler_frame(
    barrier: &HandleBarrierState,
    perform_args: &[Value],
    cont: Arc<CapturedContinuation>,
) -> MoraResult<ScriptFrame> {
    let mut frame = materialize_frame(
        Arc::clone(&barrier.handler_program),
        barrier.handler_fn,
        Arc::clone(&barrier.handler_globals),
        Arc::clone(&barrier.handler_upvalues),
        &[],
    )?;
    let func = frame.func().clone();
    for (idx, maybe_slot) in func.handler_arg_slots.iter().enumerate() {
        if let Some(LocalSlot(s)) = maybe_slot
            && (*s as usize) < frame.locals.len()
        {
            let val = perform_args.get(idx).cloned().unwrap_or(Value::Nil);
            frame.locals[*s as usize].set(val);
        }
    }
    if let Some(LocalSlot(k_slot)) = func.handler_k_slot
        && (k_slot as usize) < frame.locals.len()
    {
        frame.locals[k_slot as usize].set(Value::Continuation(cont));
    }
    Ok(frame)
}
