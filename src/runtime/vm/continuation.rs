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

use super::loans::{CallArg, FrameLoans};
use std::collections::HashMap;
use std::sync::Arc;

use crate::common::sync::Mutex;

use crate::error::{MoraError, MoraResult};
use crate::mir::control::{
    BlockId, CaptureSource, ControlFunction, ControlProgram, ControlSelectArm, FnId, FunctionKind,
    LocalSlot, ParallelSegment, UpvalueSlot, VReg,
};
use crate::value::{Environment, Value};

/// P4-2 + P6-1: Safe 16-byte hybrid unboxed VM register slot replacing full `Value` in
/// `ScriptFrame.regs` and unshared `LocalCell`s.
///
/// Primitives (`Nil`, `Bool`, `Int`, `Float`) live completely inline inside the 16-byte slot
/// with zero heap allocations on hot numeric, comparison, and control-flow paths; heap-backed
/// values (`String`, `List`, `Dict`, `Tensor`, `StructInstance`, closures, etc.) are stored
/// behind an 8-byte `Arc<Value>` pointer:
/// - `BorrowLocal` 指向帧内本地槽，读取期间不增加强引用。
/// - `LoadLocal` / `Copy` clone only the outer `Arc<Value>` (**0 heap allocations**,
///   **0 inner `Tensor` / `Dict` / `List` refcount bumps**, **0 `unsafe`**, automatic `Send + Sync`
///   across M:N fibers, `parallel` workers, and multi-shot `CapturedContinuation`s).
/// - At last-use consumption (`take_value`), `Arc::unwrap_or_clone` unwraps `Value` in $O(1)$
///   when uniquely owned (`Arc::strong_count == 1`), preserving `Arc::strong_count == 1` on the
///   inner `Tensor` / `Dict` / `List` / `String` for FBIP in-place mutation.
#[derive(Debug)]
pub enum RegSlot {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Heap(Arc<Value>),
}

impl Clone for RegSlot {
    #[inline]
    fn clone(&self) -> Self {
        match self {
            Self::Nil => Self::Nil,
            Self::Bool(value) => Self::Bool(*value),
            Self::Int(value) => Self::Int(*value),
            Self::Float(value) => Self::Float(*value),
            Self::Heap(value) => {
                // Debug-build-only trace: a borrowed call must pass the caller's slot by address,
                // so any outer `Arc<Value>` clone here means the borrow path degraded to a copy.
                crate::runtime::fbip::note_regslot_heap_clone();
                Self::Heap(Arc::clone(value))
            }
        }
    }
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
            other => Self::Heap(Arc::new(other)),
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
            Self::Heap(v) => (**v).clone(),
        }
    }

    /// Consume this register slot into a full [`Value`], unwrapping in $O(1)$ without touching
    /// inner `Arc` reference counts when `Arc::strong_count == 1`.
    #[inline]
    pub(crate) fn take_value(self) -> Value {
        match self {
            Self::Nil => Value::Nil,
            Self::Bool(b) => Value::Bool(b),
            Self::Int(i) => Value::Int(i),
            Self::Float(f) => Value::Float(f),
            Self::Heap(v) => Arc::unwrap_or_clone(v),
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
            Self::Heap(v) => crate::flow::is_truthy(v),
        }
    }

    /// Borrow the inner [`Value`] when this slot holds a heap-allocated value.
    #[inline]
    pub(crate) fn as_heap(&self) -> Option<&Value> {
        match self {
            Self::Heap(v) => Some(v),
            _ => None,
        }
    }
}

/// Shared stack-frame slice captured by both multi-shot delimited continuations
/// (`perform`/`handle`) and cooperative M:N VM fiber suspensions (`VmYield`).
pub type CapturedFrames = Vec<CallFrame>;

/// Shared cell holding a module-top static global binding `(value, is_exported)`.
pub type GlobalCell = Arc<Mutex<(Value, bool)>>;

/// Per-module runtime global state: static global slots plus lexically scoped module imports.
///
/// Storing imported bindings alongside each module's global cells guarantees true lexical
/// module scoping: tasks, closures, `impl` methods, and `spawn`ed fibers defined in module `M`
/// always resolve `M`'s transitive `import`s even after `M`'s `ImportFinalize` returns to callers.
#[derive(Debug)]
pub struct ModuleGlobalsInner {
    pub slots: Vec<GlobalCell>,
    pub imports: Mutex<HashMap<String, Value>>,
}

impl ModuleGlobalsInner {
    pub(crate) fn get_import(&self, name: &str) -> Option<Value> {
        self.imports.lock().get(name).cloned()
    }

    pub(crate) fn insert_import(&self, name: String, val: Value) {
        self.imports.lock().insert(name, val);
    }

    pub(crate) fn snapshot_imports(&self) -> HashMap<String, Value> {
        self.imports.lock().clone()
    }
}

impl std::ops::Deref for ModuleGlobalsInner {
    type Target = [GlobalCell];

    #[inline]
    fn deref(&self) -> &[GlobalCell] {
        &self.slots
    }
}

/// Per-module global slot & lexical import storage handle.
pub type ModuleGlobals = Arc<ModuleGlobalsInner>;

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

    /// Read this local directly into a 16-byte [`RegSlot`] without boxing primitives
    /// (cloning only the outer `Arc<Value>` for heap values).
    #[inline]
    pub(crate) fn get_slot(&self) -> RegSlot {
        match self {
            Self::Val(s) => s.clone(),
            Self::Shared(cell) => RegSlot::from_value(cell.lock().clone()),
        }
    }

    /// P6-1 Perceus/FBIP: Transfer ownership out of an unshared `LocalCell::Val` (clearing to `Nil`),
    /// or clone out of `LocalCell::Shared`.
    #[inline]
    pub(crate) fn take_slot(&mut self) -> RegSlot {
        match self {
            Self::Val(s) => std::mem::replace(s, RegSlot::Nil),
            Self::Shared(cell) => RegSlot::from_value(cell.lock().clone()),
        }
    }

    /// P6-1 Perceus/FBIP: Drop the value stored in an unshared `LocalCell::Val`.
    #[inline]
    pub(crate) fn drop_slot(&mut self) {
        if let Self::Val(s) = self {
            *s = RegSlot::Nil;
        }
    }

    /// Return true when this local holds a heap container/tensor/string/ADT eligible for FBIP reuse.
    #[inline]
    pub(crate) fn holds_fbip_candidate(&self) -> bool {
        matches!(
            self,
            Self::Val(RegSlot::Heap(v))
                if matches!(
                    **v,
                    Value::Dict(_)
                        | Value::List(_)
                        | Value::Tensor(_)
                        | Value::String(_)
                        | Value::StructInstance { .. }
                        | Value::EnumVariant { .. }
                )
        )
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
    pub(crate) loans: FrameLoans,
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

    /// P6-1 Perceus/FBIP: Project a nominal struct field **in place** out of register `r`
    /// when `r` uniquely owns both its outer `Arc<Value>` and `StructInstance.fields`
    /// (`Arc::strong_count == 1`), keeping remaining fields in `r` for sibling projections.
    #[inline]
    pub(crate) fn take_struct_field(&mut self, r: VReg, name: &str) -> Option<Value> {
        let RegSlot::Heap(arc_v) = &mut self.regs[r.0 as usize] else {
            return None;
        };
        let Value::StructInstance { fields, .. } = Arc::get_mut(arc_v)? else {
            return None;
        };
        fields.remove_if_unique(name)
    }

    /// Drop the value held by virtual register `r`.
    #[inline]
    pub(crate) fn clear_reg(&mut self, r: VReg) {
        self.regs[r.0 as usize] = RegSlot::Nil;
    }

    /// P6-1 Perceus/FBIP: Read or destructively take virtual register `r` as a full [`Value`].
    #[inline]
    pub(crate) fn read_or_take_val(
        &mut self,
        r: VReg,
        kill: bool,
        can_move: bool,
        fbip_stats: &crate::runtime::fbip::FbipCounters,
    ) -> Value {
        if kill {
            if can_move {
                return std::mem::replace(&mut self.regs[r.0 as usize], RegSlot::Nil).take_value();
            }
            if matches!(
                &self.regs[r.0 as usize],
                RegSlot::Heap(v)
                    if matches!(**v, Value::Dict(_) | Value::List(_) | Value::Tensor(_))
            ) {
                fbip_stats.on_continuation_copy_fallback();
            }
        }
        self.regs[r.0 as usize].to_value()
    }

    /// P6-1 Perceus/FBIP: Read or destructively take virtual register `r` as a 16-byte [`RegSlot`].
    #[inline]
    pub(crate) fn read_or_take_slot(
        &mut self,
        r: VReg,
        kill: bool,
        can_move: bool,
        fbip_stats: &crate::runtime::fbip::FbipCounters,
    ) -> RegSlot {
        if kill {
            if can_move {
                return std::mem::replace(&mut self.regs[r.0 as usize], RegSlot::Nil);
            }
            if matches!(
                &self.regs[r.0 as usize],
                RegSlot::Heap(v)
                    if matches!(**v, Value::Dict(_) | Value::List(_) | Value::Tensor(_))
            ) {
                fbip_stats.on_continuation_copy_fallback();
            }
        }
        self.regs[r.0 as usize].clone()
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

/// P5-4: Structural PyTree template recorded when registering differentiated inputs for
/// `grad` / `value_and_grad`, allowing gradients to be reconstructed with the exact original
/// container shape (`Tensor`, scalar float, `list<...>`, or `dict<string, ...>`).
#[derive(Debug, Clone)]
pub enum GradTreeSpec {
    TensorLeaf,
    ScalarLeaf(crate::common::DType),
    List(Vec<GradTreeSpec>),
    Dict(Vec<(String, GradTreeSpec)>),
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
    /// `try_call(f, args...)` boundary catching runtime/IO/timeout errors into `result<T, string>`.
    TryCatch,
    /// Reverse-mode autograd wrapper around a differentiated function call.
    AutogradEval {
        tape_id: u64,
        leaf_nodes: Vec<usize>,
        /// PyTree structure for every differentiated argument in `argnums`, preserving
        /// scalars, tensors, nested lists, and string-keyed dicts on gradient reconstruction.
        tree_specs: Vec<GradTreeSpec>,
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
    let slots = prog
        .globals
        .iter()
        .map(|meta| Arc::new(Mutex::new((Value::Nil, meta.exported))))
        .collect();
    Arc::new(ModuleGlobalsInner {
        slots,
        imports: Mutex::new(HashMap::new()),
    })
}

/// Snapshot all module globals as `(value, is_exported)` tuples.
pub(crate) fn snapshot_globals(globals: &ModuleGlobals) -> Vec<(Value, bool)> {
    globals.iter().map(|cell| cell.lock().clone()).collect()
}

/// Restore module globals from a previous [`snapshot_globals`] slice.
pub(crate) fn restore_globals(globals: &ModuleGlobals, snap: &[(Value, bool)]) {
    for (cell, saved) in globals.iter().zip(snap.iter()) {
        *cell.lock() = saved.clone();
    }
}

/// Clone module globals and lexical imports into isolated mutex cells for parallel workers.
pub(crate) fn clone_isolated_globals_with_imports(
    snap: &[(Value, bool)],
    imports: HashMap<String, Value>,
) -> ModuleGlobals {
    Arc::new(ModuleGlobalsInner {
        slots: snap
            .iter()
            .map(|entry| Arc::new(Mutex::new(entry.clone())))
            .collect(),
        imports: Mutex::new(imports),
    })
}

/// Resolve closure/sub-function `CaptureSource` descriptors from the active parent frame.
pub(crate) fn resolve_captures(
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
pub(crate) fn clone_worker_upvalues(
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

/// Construct a new [`ScriptFrame`] directly from positional + optional named [`RegSlot`]s,
/// preserving `RegSlot::Heap(Arc<Value>)` identity for borrowed and moved arguments without
/// intermediate `Vec<Value>` materialization.
pub(crate) fn materialize_frame_slots_named(
    program: Arc<ControlProgram>,
    func_id: FnId,
    globals: ModuleGlobals,
    upvalues: Arc<Vec<UpvalueValue>>,
    args: Vec<CallArg<'_>>,
    named_args: Vec<(String, CallArg<'_>)>,
) -> MoraResult<ScriptFrame> {
    let (locals, regs, block_id, loans) = {
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

        let resolved_slots: Vec<CallArg<'_>> = if needs_arity_check {
            let n_params = func.params.len();
            let pos_len = args.len();
            if pos_len > n_params {
                if func.kind == FunctionKind::Macro {
                    let mname = func
                        .name
                        .map(|s| program.symbols.resolve(s))
                        .unwrap_or("<macro>");
                    return Err(MoraError::Runtime(format!(
                        "macro '{}' expects {} args, got {}",
                        mname, n_params, pos_len
                    )));
                }
                return Err(MoraError::Runtime(format!(
                    "{kind_label} expects {} args, got {}",
                    n_params, pos_len
                )));
            }
            if named_args.is_empty() && pos_len == n_params {
                args
            } else {
                let mut slots: Vec<_> = (0..n_params).map(|_| None).collect();
                for (i, slot) in args.into_iter().enumerate() {
                    slots[i] = Some(slot);
                }
                let had_named = !named_args.is_empty();
                for (kw, slot) in named_args {
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
                    slots[idx] = Some(slot);
                }
                let mut out = Vec::with_capacity(n_params);
                for (idx, maybe_slot) in slots.into_iter().enumerate() {
                    if let Some(s) = maybe_slot {
                        out.push(s);
                    } else if func.param_has_default.get(idx).copied().unwrap_or(false) {
                        out.push(CallArg::from_value(Value::MissingArg));
                    } else if !had_named {
                        if func.kind == FunctionKind::Macro {
                            let mname = func
                                .name
                                .map(|s| program.symbols.resolve(s))
                                .unwrap_or("<macro>");
                            return Err(MoraError::Runtime(format!(
                                "macro '{}' expects {} args, got {}",
                                mname, n_params, pos_len
                            )));
                        }
                        return Err(MoraError::Runtime(format!(
                            "{kind_label} expects {} args, got {}",
                            n_params, pos_len
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
            args
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

        let mut loans = FrameLoans::default();
        for (slot_idx, slot) in resolved_slots.into_iter().enumerate() {
            if slot_idx < locals.len() {
                let borrowed = func.param_borrows.get(slot_idx) == Some(&true)
                    && matches!(locals[slot_idx], LocalCell::Val(_));
                locals[slot_idx].set_slot(slot.bind_parameter(
                    LocalSlot(slot_idx as u16),
                    borrowed,
                    &mut loans,
                ));
            }
        }

        let regs = vec![RegSlot::Nil; func.n_regs as usize];
        (locals, regs, func.entry, loans)
    };

    Ok(ScriptFrame {
        program,
        func_id,
        globals,
        upvalues,
        locals,
        loans,
        regs,
        block_id,
        pc: 0,
        resume_dst: None,
        defers: Vec::new(),
    })
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

/// Construct a new [`ScriptFrame`] from `&[Value]` + `&[(String, Value)]` by delegating to
/// [`materialize_frame_slots_named`].
pub(crate) fn materialize_frame_named(
    program: Arc<ControlProgram>,
    func_id: FnId,
    globals: ModuleGlobals,
    upvalues: Arc<Vec<UpvalueValue>>,
    args: &[Value],
    named_args: &[(String, Value)],
) -> MoraResult<ScriptFrame> {
    let slot_args = args.iter().cloned().map(CallArg::from_value).collect();
    let slot_named = named_args
        .iter()
        .map(|(k, v)| (k.clone(), CallArg::from_value(v.clone())))
        .collect();
    materialize_frame_slots_named(program, func_id, globals, upvalues, slot_args, slot_named)
}

/// Bind `perform` arguments and captured [`CapturedContinuation`] into a `HandleHandler` frame.
pub(crate) fn materialize_handler_frame(
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
