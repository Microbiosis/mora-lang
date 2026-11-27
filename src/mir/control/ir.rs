//! v0.110.0: Canonical explicit-CFG Control SSA IR (`ControlProgram` / `ControlFunction`).
//!
//! Design principles:
//! - Explicit `BasicBlock`s + `BlockParam`s + structured `ControlTerminator`s:
//!   no linear pc jump patching, no fragile Phi edge reconstruction, no fake
//!   `Label` / `JumpExit` workarounds.
//! - Compile-time symbol & storage resolution:
//!   - Local variables & pattern/loop bindings -> [`LocalSlot`] (`Vec<Value>` indexing)
//!   - Closed-over variables -> [`UpvalueSlot`] (shared cell when mutated across boundary,
//!     immutable value capture otherwise)
//!   - Module-top declarations -> [`GlobalSlot`] (`Vec<Value>` indexing across top-level & tasks)
//!   - Free builtins & module prefixes -> [`BuiltinFnId`] / [`BuiltinKind`]
//!   - Dynamically imported / REPL bindings -> [`SymId`] interned dynamic global lookup
//! - Explicit [`Purity`] classification so optimization passes (DCE / GVN / LICM)
//!   never reorder or hoist trapping, effectful, or mutable-storage operations.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::common::{BinaryOp, ScalarValue};
use crate::error::{MoraError, MoraResult};
use crate::value::{BuiltinKind, Value};

/// Interned identifier index within a [`ControlProgram`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SymId(pub u32);

/// Index of a [`BasicBlock`] inside a [`ControlFunction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);

/// Index of a [`ControlFunction`] inside a [`ControlProgram`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FnId(pub u32);

/// SSA virtual value register inside a [`ControlFunction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VReg(pub u32);

/// Per-frame local variable slot index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalSlot(pub u16);

/// Per-closure captured upvalue slot index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UpvalueSlot(pub u16);

/// Module-level statically declared global slot index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GlobalSlot(pub u16);

/// String interner mapping symbol names to compact copyable [`SymId`]s.
#[derive(Debug, Clone, Default)]
pub struct SymbolTable {
    names: Vec<String>,
    index: HashMap<String, SymId>,
}

impl SymbolTable {
    /// Create an empty symbol table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Intern `name` and return its canonical [`SymId`].
    pub fn intern(&mut self, name: &str) -> SymId {
        if let Some(&id) = self.index.get(name) {
            return id;
        }
        let id = SymId(self.names.len() as u32);
        let owned = name.to_string();
        self.names.push(owned.clone());
        self.index.insert(owned, id);
        id
    }

    /// Resolve a [`SymId`] back to its string slice.
    pub fn resolve(&self, id: SymId) -> &str {
        self.names
            .get(id.0 as usize)
            .map(String::as_str)
            .unwrap_or("<unknown>")
    }

    /// Number of interned symbols.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Return whether no symbols have been interned.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Borrow the ordered slice of interned symbol names for deterministic binary serialization.
    pub(crate) fn as_slice(&self) -> &[String] {
        &self.names
    }

    /// Reconstruct a [`SymbolTable`] preserving exact [`SymId`] ordering from serialized names.
    pub(crate) fn from_names(names: Vec<String>) -> Self {
        let mut index = HashMap::with_capacity(names.len());
        for (i, name) in names.iter().enumerate() {
            index.insert(name.clone(), SymId(i as u32));
        }
        Self { names, index }
    }
}

/// Exhaustive compile-time IDs for Mora's built-in free functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BuiltinFnId {
    Apply,
    Assert,
    AssertEq,
    Atom,
    Bool,
    Car,
    Cdr,
    Channel,
    Compose,
    Cons,
    Context,
    Curry,
    Deref,
    Err,
    Eval,
    FbipResetStats,
    FbipStats,
    Float,
    Gensym,
    Grad,
    Int,
    Into,
    IsInstance,
    Jvp,
    KvAllocator,
    Len,
    Macroexpand,
    MemoryPool,
    MergeWith,
    MethodsOf,
    Ok,
    Partial,
    Print,
    ProcessGroup,
    Profiler,
    Quote,
    Range,
    Read,
    Sleep,
    Str,
    Swap,
    Tail,
    TcpConnect,
    TcpListen,
    TcpProcessGroup,
    Tensor,
    TlsConnect,
    TlsListen,
    TraceDag,
    TraceValueAndGradDag,
    TryCall,
    TypeOf,
    Uncurry,
    ValueAndGrad,
}

impl BuiltinFnId {
    /// Resolve a built-in free function name at compile time.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "apply" => Some(Self::Apply),
            "assert" => Some(Self::Assert),
            "assert_eq" => Some(Self::AssertEq),
            "atom" => Some(Self::Atom),
            "bool" => Some(Self::Bool),
            "car" => Some(Self::Car),
            "cdr" => Some(Self::Cdr),
            "channel" => Some(Self::Channel),
            "compose" => Some(Self::Compose),
            "cons" => Some(Self::Cons),
            "context" => Some(Self::Context),
            "curry" => Some(Self::Curry),
            "deref" => Some(Self::Deref),
            "Err" => Some(Self::Err),
            "eval" => Some(Self::Eval),
            "fbip_reset_stats" => Some(Self::FbipResetStats),
            "fbip_stats" => Some(Self::FbipStats),
            "float" => Some(Self::Float),
            "gensym" => Some(Self::Gensym),
            "grad" => Some(Self::Grad),
            "int" => Some(Self::Int),
            "into" => Some(Self::Into),
            "is_instance" => Some(Self::IsInstance),
            "jvp" => Some(Self::Jvp),
            "kv_allocator" => Some(Self::KvAllocator),
            "len" => Some(Self::Len),
            "macroexpand" => Some(Self::Macroexpand),
            "memory_pool" => Some(Self::MemoryPool),
            "merge_with" => Some(Self::MergeWith),
            "methods_of" => Some(Self::MethodsOf),
            "Ok" => Some(Self::Ok),
            "partial" => Some(Self::Partial),
            "print" => Some(Self::Print),
            "process_group" => Some(Self::ProcessGroup),
            "profiler" => Some(Self::Profiler),
            "quote" => Some(Self::Quote),
            "range" => Some(Self::Range),
            "read" => Some(Self::Read),
            "sleep" => Some(Self::Sleep),
            "str" => Some(Self::Str),
            "swap" => Some(Self::Swap),
            "tail" => Some(Self::Tail),
            "tcp_connect" => Some(Self::TcpConnect),
            "tcp_listen" => Some(Self::TcpListen),
            "tcp_process_group" => Some(Self::TcpProcessGroup),
            "tensor" => Some(Self::Tensor),
            "tls_connect" => Some(Self::TlsConnect),
            "tls_listen" => Some(Self::TlsListen),
            "trace_dag" => Some(Self::TraceDag),
            "trace_value_and_grad_dag" => Some(Self::TraceValueAndGradDag),
            "try_call" => Some(Self::TryCall),
            "type_of" => Some(Self::TypeOf),
            "uncurry" => Some(Self::Uncurry),
            "value_and_grad" => Some(Self::ValueAndGrad),
            _ => None,
        }
    }

    /// Return the canonical source name for this built-in function.
    pub fn name(self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::Assert => "assert",
            Self::AssertEq => "assert_eq",
            Self::Atom => "atom",
            Self::Bool => "bool",
            Self::Car => "car",
            Self::Cdr => "cdr",
            Self::Channel => "channel",
            Self::Compose => "compose",
            Self::Cons => "cons",
            Self::Context => "context",
            Self::Curry => "curry",
            Self::Deref => "deref",
            Self::Err => "Err",
            Self::Eval => "eval",
            Self::FbipResetStats => "fbip_reset_stats",
            Self::FbipStats => "fbip_stats",
            Self::Float => "float",
            Self::Gensym => "gensym",
            Self::Grad => "grad",
            Self::Int => "int",
            Self::Into => "into",
            Self::IsInstance => "is_instance",
            Self::Jvp => "jvp",
            Self::KvAllocator => "kv_allocator",
            Self::Len => "len",
            Self::Macroexpand => "macroexpand",
            Self::MemoryPool => "memory_pool",
            Self::MergeWith => "merge_with",
            Self::MethodsOf => "methods_of",
            Self::Ok => "Ok",
            Self::Partial => "partial",
            Self::Print => "print",
            Self::ProcessGroup => "process_group",
            Self::Profiler => "profiler",
            Self::Quote => "quote",
            Self::Range => "range",
            Self::Read => "read",
            Self::Sleep => "sleep",
            Self::Str => "str",
            Self::Swap => "swap",
            Self::Tail => "tail",
            Self::TcpConnect => "tcp_connect",
            Self::TcpListen => "tcp_listen",
            Self::TcpProcessGroup => "tcp_process_group",
            Self::Tensor => "tensor",
            Self::TlsConnect => "tls_connect",
            Self::TlsListen => "tls_listen",
            Self::TraceDag => "trace_dag",
            Self::TraceValueAndGradDag => "trace_value_and_grad_dag",
            Self::TryCall => "try_call",
            Self::TypeOf => "type_of",
            Self::Uncurry => "uncurry",
            Self::ValueAndGrad => "value_and_grad",
        }
    }
}

/// Side-effect classification used by optimization passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purity {
    /// Deterministic value computation depending only on SSA operands. Safe for DCE, CSE, LICM.
    Pure,
    /// Value computation that can trap on bad inputs (div-by-zero, OOB index, type error).
    /// Must NOT be speculated out of loops or across guards, and dead unused traps cannot be deleted.
    Fallible,
    /// Reads/writes mutable storage, invokes calls/effects/handlers, or performs I/O.
    Effectful,
}

/// Literal scalar constant embedded in IR instructions.
#[derive(Debug, Clone, PartialEq)]
pub enum ControlConst {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// Wave 3 — Stage 3B: Explicit hardware-width scalar (`f32`, `f16`, `bf16`, `fp8`, `i4`..`i32`, `u8`..`u64`).
    Scalar(ScalarValue),
    String(String),
    Char(char),
    BuiltinFn(BuiltinFnId),
    BuiltinModule(BuiltinKind),
    /// Wave 3 Stage 3C: Internal sentinel instructing callee prologue to evaluate its default arg.
    MissingArg,
}

impl ControlConst {
    /// Materialize this constant into a runtime [`Value`].
    pub fn to_value(&self) -> Value {
        match self {
            Self::Nil => Value::Nil,
            Self::Bool(b) => Value::Bool(*b),
            Self::Int(i) => Value::Int(*i),
            Self::Float(f) => Value::Float(*f),
            Self::Scalar(s) => Value::Scalar(*s),
            Self::String(s) => Value::String(s.clone()),
            Self::Char(c) => Value::Char(*c),
            Self::BuiltinFn(id) => Value::Builtin(match id {
                BuiltinFnId::Print => BuiltinKind::Print,
                BuiltinFnId::Range => BuiltinKind::Range,
                BuiltinFnId::Len => BuiltinKind::Len,
                BuiltinFnId::Eval => BuiltinKind::Eval,
                BuiltinFnId::Tail => BuiltinKind::Tail,
                _ => BuiltinKind::FreeFn(id.name()),
            }),
            Self::BuiltinModule(kind) => Value::Builtin(*kind),
            Self::MissingArg => Value::MissingArg,
        }
    }
}

/// Unary register operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Truthy,
    Not,
    Neg,
}

/// Kind of a compiled [`ControlFunction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionKind {
    Main,
    Task,
    Closure,
    Macro,
    HandleBody,
    HandleHandler,
    TransactionBody,
    TransactionComp,
    Worker,
    ParallelSeq,
    SelectArm,
    NoGradBody,
    TraitDefault,
    ImplMethod,
    DefaultArg,
}

/// How a closure/sub-function captures a variable from its enclosing lexical frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureSource {
    ParentLocal(LocalSlot),
    ParentUpvalue(UpvalueSlot),
}

/// Descriptor for one captured upvalue slot in a [`ControlFunction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpvalueDesc {
    pub sym: SymId,
    pub source: CaptureSource,
    /// When true, outer local uses an interior-mutable `UpvalueCell` shared across capture boundaries.
    pub shared: bool,
}

/// Kind metadata for a statically declared module global.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalKind {
    Var,
    Task(FnId),
    Macro(FnId),
    Enum,
    Struct,
    Trait,
    TypeAlias,
    Kernel,
}

/// Metadata entry for one module-level [`GlobalSlot`].
#[derive(Debug, Clone)]
pub struct GlobalMeta {
    pub sym: SymId,
    pub name: String,
    pub exported: bool,
    pub kind: GlobalKind,
}

/// Pre-resolved callee descriptor on `ControlInst::Call` / `ControlTerminator::TailCall`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalleeTarget {
    /// Free built-in function (print, len, range, int, str, ...).
    Builtin(BuiltinFnId),
    /// Direct static call to a top-level `task` within this program.
    Fn(FnId),
    /// Indirect call through a value register (closure, continuation, task value, compose/curry).
    Value(VReg),
    /// Dynamic symbol fallback for imported names or `Trait::new("...")`.
    DynGlobal(SymId),
}

/// Segments of a compiled quasiquote expression.
#[derive(Debug, Clone)]
pub enum ControlQqSegment {
    Quote(String),
    Unquote(VReg),
    UnquoteSplice(VReg),
}

/// Structured binding destination for a compiled pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternTarget {
    Local(LocalSlot),
    Global(GlobalSlot),
    Dyn(SymId),
    Discard,
}

/// Compiled structural match pattern (eliminating runtime string-split parsing).
#[derive(Debug, Clone)]
pub enum CompiledPattern {
    Wildcard,
    Bind(PatternTarget),
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Scalar(ScalarValue),
    Char(char),
    String(String),
    Tuple(Vec<CompiledPattern>),
    ListVec {
        elements: Vec<CompiledPattern>,
        rest: Option<Box<CompiledPattern>>,
    },
    ListCons {
        head: Box<CompiledPattern>,
        tail: Box<CompiledPattern>,
    },
    Dict {
        required: Vec<(String, CompiledPattern)>,
        rest: bool,
    },
    TypeAscription {
        ty_name: String,
        inner: Box<CompiledPattern>,
    },
    /// Wave 3 Stage 3C: Enum variant pattern (`Enum::Variant(p1, p2)`).
    Variant {
        enum_name: Option<String>,
        variant: String,
        fields: Vec<CompiledPattern>,
    },
    /// Wave 3 Stage 3C: Nominal struct destructuring pattern (`Point { x: a, .. }`).
    Struct {
        name: String,
        fields: Vec<(String, CompiledPattern)>,
        rest: bool,
    },
}

/// Wave 3 Stage 3C: Compiled slice axis for `ControlInst::TensorSlice`.
#[derive(Debug, Clone)]
pub enum ControlSliceAxis {
    Index(VReg),
    Range {
        start: Option<VReg>,
        stop: Option<VReg>,
        step: Option<VReg>,
    },
    Ellipsis,
    NewAxis,
}

/// Wave 3 Stage 3C: Compiled struct field descriptor in `ControlInst::RegisterStruct`.
#[derive(Debug, Clone)]
pub struct ControlStructField {
    pub name: SymId,
    pub type_name: String,
    pub default_fn: Option<FnId>,
    pub tags: Vec<(String, String)>,
}

/// Wave 3 Stage 3C: Compiled trait method descriptor in `ControlInst::RegisterTrait`.
#[derive(Debug, Clone)]
pub struct ControlTraitMethod {
    pub name: SymId,
    pub params: Vec<(SymId, Option<String>)>,
    pub return_type: Option<String>,
    pub has_self: bool,
    pub default_fn: Option<FnId>,
}

/// Wave 3 Stage 3C: Compiled impl method descriptor in `ControlInst::RegisterImpl`.
#[derive(Debug, Clone)]
pub struct ControlImplMethod {
    pub name: SymId,
    pub fn_id: FnId,
    pub has_self: bool,
}

/// One segment inside `ControlInst::Parallel`.
#[derive(Debug, Clone)]
pub enum ParallelSegment {
    /// Batch of concurrent `worker` bodies executed in declaration order for deterministic merge.
    WorkerBatch(Vec<(SymId, FnId, Vec<CaptureSource>)>),
    /// Non-worker statement segment run sequentially on the caller scope.
    Sequential(FnId, Vec<CaptureSource>),
}

/// Target inside `ControlInst::Spawn`.
#[derive(Debug, Clone)]
pub enum ControlSpawnTarget {
    Call {
        target: CalleeTarget,
        args: Vec<VReg>,
        named_args: Vec<(SymId, VReg)>,
    },
    MethodCall {
        receiver: VReg,
        method: SymId,
        args: Vec<VReg>,
        named_args: Vec<(SymId, VReg)>,
    },
    Callable(VReg),
}

/// One branch inside `ControlInst::Select`.
#[derive(Debug, Clone)]
pub enum ControlSelectArm {
    Recv {
        channel: VReg,
        has_binding: bool,
        body_fn: FnId,
        captures: Vec<CaptureSource>,
    },
    Send {
        channel: VReg,
        value: VReg,
        body_fn: FnId,
        captures: Vec<CaptureSource>,
    },
    Timeout {
        ms: VReg,
        body_fn: FnId,
        captures: Vec<CaptureSource>,
    },
    Default {
        body_fn: FnId,
        captures: Vec<CaptureSource>,
    },
}

/// One explicit-CFG instruction in a [`BasicBlock`].
#[derive(Debug, Clone)]
pub enum ControlInst {
    Const(VReg, ControlConst),
    Copy(VReg, VReg),
    Unary(VReg, UnaryOp, VReg),
    Binary(VReg, VReg, BinaryOp, VReg),
    ListLit(VReg, Vec<VReg>),
    DictLit(VReg, Vec<(SymId, VReg)>),
    Quasiquote(VReg, Vec<ControlQqSegment>),
    MakeClosure {
        dst: VReg,
        fn_id: FnId,
        captures: Vec<CaptureSource>,
    },
    MakeDynTrait {
        dst: VReg,
        src: VReg,
        trait_name: SymId,
        generics: Vec<String>,
    },

    // Slot / global storage operations
    LoadLocal(VReg, LocalSlot),
    /// P6-1 Perceus/FBIP: Zero-clone borrow of a still-live unshared `LocalSlot` feeding a
    /// verified `param_borrows[i] == true` `Call` site in the same basic block.
    BorrowLocal(VReg, LocalSlot),
    /// P6-1 Perceus/FBIP: Final live read of an unshared `LocalSlot` — transfers ownership
    /// directly into `dst` (clearing the local slot to `Nil` when no effect `HandleBarrier` is active).
    MoveLocal(VReg, LocalSlot),
    StoreLocal(LocalSlot, VReg),
    /// P6-1 Perceus/FBIP: Drop a dead unshared `LocalSlot` on asymmetric branch entry.
    DropLocal(LocalSlot),
    /// P6-1 Perceus/FBIP: Drop a dead SSA `VReg` on asymmetric branch entry or unused effectful `dst`.
    DropReg(VReg),
    /// P5-1A: Allocate a fresh `UpvalueCell` for `LocalSlot` if it is shared with closures,
    /// enforcing Go-1.22-style per-iteration loop variable capture semantics.
    FreshLocalCell(LocalSlot),
    /// Wave 3 Stage 3C: Sets `dst` to `Bool(true)` iff `local` holds `Value::MissingArg`.
    IsMissingArg(VReg, LocalSlot),
    LoadUpvalue(VReg, UpvalueSlot),
    StoreUpvalue(UpvalueSlot, VReg),
    LoadGlobal(VReg, GlobalSlot),
    /// P6-1 Perceus/FBIP: Final live read of a private `<main>` `GlobalSlot` prior to a
    /// post-dominating `StoreGlobal`, transferring ownership into `dst`.
    MoveGlobal(VReg, GlobalSlot),
    StoreGlobal(GlobalSlot, VReg),
    LoadDyn(VReg, SymId),
    DefineDyn(SymId, VReg, bool),
    AssignDyn(SymId, VReg),
    ExportGlobal(GlobalSlot),
    ExportDyn(SymId),

    // Fallible / call / runtime operations
    Index(VReg, VReg, VReg),
    IndexAssign(VReg, VReg, VReg, VReg),
    /// Wave 3 Stage 3C: Multi-dimensional zero-copy tensor slice view.
    TensorSlice {
        dst: VReg,
        target: VReg,
        axes: Vec<ControlSliceAxis>,
    },
    Call {
        dst: VReg,
        target: CalleeTarget,
        args: Vec<VReg>,
        named_args: Vec<(SymId, VReg)>,
    },
    /// P4-1A: Nominal generic constructor/associated call with explicit const-generic integer args (`Linear<4, 3>(w, b)`).
    CallGeneric {
        dst: VReg,
        target: CalleeTarget,
        const_args: Vec<(SymId, i64)>,
        args: Vec<VReg>,
        named_args: Vec<(SymId, VReg)>,
    },
    /// P4-1A: Read a compile-time const-generic integer bound on a `StructInstance` receiver (`self`).
    GetConstGeneric {
        dst: VReg,
        src: VReg,
        name: SymId,
    },
    ModuleCall {
        dst: VReg,
        module: BuiltinKind,
        method: SymId,
        args: Vec<VReg>,
        named_args: Vec<(SymId, VReg)>,
    },
    MethodCall {
        dst: VReg,
        receiver: VReg,
        method: SymId,
        args: Vec<VReg>,
        named_args: Vec<(SymId, VReg)>,
    },
    /// P6-1 Perceus/FBIP: Compile-time statically resolved nominal struct field projection.
    /// Extracts `field` in place when `can_move` holds and `receiver` uniquely owns its CoW
    /// `StructInstance` + `fields` dictionary, or clones in $O(1)$ when shared.
    ProjectField {
        dst: VReg,
        receiver: VReg,
        field: SymId,
    },
    Pipe {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
    },
    MatchPattern {
        dst_ok: VReg,
        val: VReg,
        pattern: CompiledPattern,
    },

    // Structured runtime constructs & top-level declarations
    Handle {
        dst: VReg,
        effect: SymId,
        body_fn: FnId,
        body_captures: Vec<CaptureSource>,
        handler_fn: FnId,
        handler_captures: Vec<CaptureSource>,
        auto_resume: bool,
    },
    Transaction {
        dst: VReg,
        body_fn: FnId,
        body_captures: Vec<CaptureSource>,
        comp_fn: FnId,
        comp_captures: Vec<CaptureSource>,
    },
    Parallel {
        dst: VReg,
        segments: Vec<ParallelSegment>,
    },
    Spawn {
        dst: VReg,
        target: ControlSpawnTarget,
    },
    Select {
        dst: VReg,
        arms: Vec<ControlSelectArm>,
    },
    /// Wave 3 Stage 3C: Execute `body_fn` with autograd tape recording suppressed.
    NoGrad {
        dst: VReg,
        body_fn: FnId,
        body_captures: Vec<CaptureSource>,
    },
    /// G2: Probe whether `src` is an ok/present value (`Result::Ok`, `Option::Some`, or non-nil)
    /// vs an early-return propagation value (`Result::Err`, `Option::None`, or `Nil`).
    TryProbe(VReg, VReg),
    /// G2: Unwrap payload from `Ok(v)` / `Some(v)` or pass through a verified non-nil value.
    TryUnwrap(VReg, VReg),
    /// G2: Register a zero-arg closure on the active frame's LIFO defer stack.
    PushDefer {
        fn_id: FnId,
        captures: Vec<CaptureSource>,
    },
    Import {
        path: String,
    },
    EvalAssert {
        name: SymId,
        given: VReg,
        expects: Vec<VReg>,
        tolerance: Option<f64>,
        replay_path: Option<String>,
    },
    Assert(VReg, VReg),
    Commit,
    Rollback,
    RegisterTypeAlias {
        name: SymId,
        target: String,
        dst: Option<GlobalSlot>,
    },
    RegisterEnum {
        name: SymId,
        type_params: Vec<SymId>,
        const_params: Vec<SymId>,
        const_shape_bindings: Vec<(SymId, SymId, usize, usize)>,
        variants: Vec<(SymId, Vec<String>)>,
        dst: Option<GlobalSlot>,
    },
    RegisterStruct {
        name: SymId,
        type_params: Vec<SymId>,
        const_params: Vec<SymId>,
        const_shape_bindings: Vec<(SymId, usize, usize)>,
        fields: Vec<ControlStructField>,
        dst: Option<GlobalSlot>,
    },
    RegisterTrait {
        name: SymId,
        type_params: Vec<SymId>,
        parents: Vec<SymId>,
        methods: Vec<ControlTraitMethod>,
        dst: Option<GlobalSlot>,
    },
    RegisterImpl {
        trait_name: Option<SymId>,
        trait_generics: Vec<String>,
        target_type: SymId,
        target_generics: Vec<String>,
        methods: Vec<ControlImplMethod>,
    },
    RegisterMacro {
        name: SymId,
        params: Vec<SymId>,
        body_fn: FnId,
        dst: Option<GlobalSlot>,
    },
    RegisterTask {
        name: SymId,
        params: Vec<SymId>,
        fn_id: FnId,
        dst: Option<GlobalSlot>,
    },
    /// P4-1B: Register a compiled first-class GPU `kernel` value in global/lexical scope.
    RegisterKernel {
        name: SymId,
        program: Box<crate::common::kernel_ir::KernelProgram>,
        dst: Option<GlobalSlot>,
    },
    /// Optional statement expression marker keeping last statement value live.
    KeepAlive(VReg),
}

impl ControlInst {
    /// Return the destination [`VReg`] written by this instruction, if any.
    pub fn dst(&self) -> Option<VReg> {
        match self {
            Self::Const(d, _)
            | Self::Copy(d, _)
            | Self::Unary(d, _, _)
            | Self::Binary(d, _, _, _)
            | Self::ListLit(d, _)
            | Self::DictLit(d, _)
            | Self::Quasiquote(d, _)
            | Self::MakeClosure { dst: d, .. }
            | Self::MakeDynTrait { dst: d, .. }
            | Self::LoadLocal(d, _)
            | Self::BorrowLocal(d, _)
            | Self::MoveLocal(d, _)
            | Self::IsMissingArg(d, _)
            | Self::LoadUpvalue(d, _)
            | Self::LoadGlobal(d, _)
            | Self::MoveGlobal(d, _)
            | Self::LoadDyn(d, _)
            | Self::Index(d, _, _)
            | Self::IndexAssign(d, _, _, _)
            | Self::TensorSlice { dst: d, .. }
            | Self::Call { dst: d, .. }
            | Self::CallGeneric { dst: d, .. }
            | Self::GetConstGeneric { dst: d, .. }
            | Self::ModuleCall { dst: d, .. }
            | Self::MethodCall { dst: d, .. }
            | Self::ProjectField { dst: d, .. }
            | Self::Pipe { dst: d, .. }
            | Self::MatchPattern { dst_ok: d, .. }
            | Self::Handle { dst: d, .. }
            | Self::Transaction { dst: d, .. }
            | Self::Parallel { dst: d, .. }
            | Self::Spawn { dst: d, .. }
            | Self::Select { dst: d, .. }
            | Self::NoGrad { dst: d, .. }
            | Self::TryProbe(d, _)
            | Self::TryUnwrap(d, _)
            | Self::Assert(d, _) => Some(*d),
            Self::StoreLocal(..)
            | Self::DropLocal(..)
            | Self::DropReg(..)
            | Self::FreshLocalCell(..)
            | Self::PushDefer { .. }
            | Self::StoreUpvalue(..)
            | Self::StoreGlobal(..)
            | Self::DefineDyn(..)
            | Self::AssignDyn(..)
            | Self::ExportGlobal(..)
            | Self::ExportDyn(..)
            | Self::Import { .. }
            | Self::EvalAssert { .. }
            | Self::Commit
            | Self::Rollback
            | Self::RegisterTypeAlias { .. }
            | Self::RegisterEnum { .. }
            | Self::RegisterStruct { .. }
            | Self::RegisterTrait { .. }
            | Self::RegisterImpl { .. }
            | Self::RegisterMacro { .. }
            | Self::RegisterTask { .. }
            | Self::RegisterKernel { .. }
            | Self::KeepAlive(..) => None,
        }
    }

    /// Collect every input [`VReg`] read by this instruction.
    pub fn input_regs(&self) -> Vec<VReg> {
        match self {
            Self::Const(..)
            | Self::MakeClosure { .. }
            | Self::LoadLocal(..)
            | Self::BorrowLocal(..)
            | Self::MoveLocal(..)
            | Self::DropLocal(..)
            | Self::DropReg(..)
            | Self::FreshLocalCell(..)
            | Self::IsMissingArg(..)
            | Self::LoadUpvalue(..)
            | Self::LoadGlobal(..)
            | Self::MoveGlobal(..)
            | Self::LoadDyn(..)
            | Self::ExportGlobal(..)
            | Self::ExportDyn(..)
            | Self::Import { .. }
            | Self::Commit
            | Self::Rollback
            | Self::RegisterTypeAlias { .. }
            | Self::RegisterEnum { .. }
            | Self::RegisterStruct { .. }
            | Self::RegisterTrait { .. }
            | Self::RegisterImpl { .. }
            | Self::RegisterMacro { .. }
            | Self::RegisterTask { .. }
            | Self::RegisterKernel { .. }
            | Self::Handle { .. }
            | Self::Transaction { .. }
            | Self::Parallel { .. }
            | Self::NoGrad { .. }
            | Self::PushDefer { .. } => Vec::new(),
            Self::Copy(_, s)
            | Self::Unary(_, _, s)
            | Self::TryProbe(_, s)
            | Self::TryUnwrap(_, s)
            | Self::MakeDynTrait { src: s, .. }
            | Self::GetConstGeneric { src: s, .. }
            | Self::ProjectField { receiver: s, .. }
            | Self::StoreLocal(_, s)
            | Self::StoreUpvalue(_, s)
            | Self::StoreGlobal(_, s)
            | Self::DefineDyn(_, s, _)
            | Self::AssignDyn(_, s)
            | Self::MatchPattern { val: s, .. }
            | Self::Assert(_, s)
            | Self::KeepAlive(s) => vec![*s],
            Self::Binary(_, l, _, r) | Self::Index(_, l, r) | Self::Pipe { lhs: l, rhs: r, .. } => {
                vec![*l, *r]
            }
            Self::IndexAssign(_, obj, idx, val) => vec![*obj, *idx, *val],
            Self::TensorSlice { target, axes, .. } => {
                let mut out = vec![*target];
                for ax in axes {
                    match ax {
                        ControlSliceAxis::Index(r) => out.push(*r),
                        ControlSliceAxis::Range { start, stop, step } => {
                            if let Some(r) = start {
                                out.push(*r);
                            }
                            if let Some(r) = stop {
                                out.push(*r);
                            }
                            if let Some(r) = step {
                                out.push(*r);
                            }
                        }
                        ControlSliceAxis::Ellipsis | ControlSliceAxis::NewAxis => {}
                    }
                }
                out
            }
            Self::ListLit(_, items) => items.clone(),
            Self::DictLit(_, pairs) => pairs.iter().map(|(_, r)| *r).collect(),
            Self::Quasiquote(_, segs) => segs
                .iter()
                .filter_map(|s| match s {
                    ControlQqSegment::Quote(_) => None,
                    ControlQqSegment::Unquote(r) | ControlQqSegment::UnquoteSplice(r) => Some(*r),
                })
                .collect(),
            Self::Call {
                target,
                args,
                named_args,
                ..
            }
            | Self::CallGeneric {
                target,
                args,
                named_args,
                ..
            } => {
                let mut out = Vec::new();
                if let CalleeTarget::Value(v) = target {
                    out.push(*v);
                }
                out.extend_from_slice(args);
                out.extend(named_args.iter().map(|(_, r)| *r));
                out
            }
            Self::ModuleCall {
                args, named_args, ..
            } => {
                let mut out = args.clone();
                out.extend(named_args.iter().map(|(_, r)| *r));
                out
            }
            Self::MethodCall {
                receiver,
                args,
                named_args,
                ..
            } => {
                let mut out = vec![*receiver];
                out.extend_from_slice(args);
                out.extend(named_args.iter().map(|(_, r)| *r));
                out
            }
            Self::EvalAssert { given, expects, .. } => {
                let mut out = vec![*given];
                out.extend_from_slice(expects);
                out
            }
            Self::Spawn { target, .. } => {
                let mut out = Vec::new();
                match target {
                    ControlSpawnTarget::Call {
                        target,
                        args,
                        named_args,
                    } => {
                        if let CalleeTarget::Value(v) = target {
                            out.push(*v);
                        }
                        out.extend_from_slice(args);
                        out.extend(named_args.iter().map(|(_, r)| *r));
                    }
                    ControlSpawnTarget::MethodCall {
                        receiver,
                        args,
                        named_args,
                        ..
                    } => {
                        out.push(*receiver);
                        out.extend_from_slice(args);
                        out.extend(named_args.iter().map(|(_, r)| *r));
                    }
                    ControlSpawnTarget::Callable(v) => out.push(*v),
                }
                out
            }
            Self::Select { arms, .. } => {
                let mut out = Vec::new();
                for arm in arms {
                    match arm {
                        ControlSelectArm::Recv { channel, .. } => out.push(*channel),
                        ControlSelectArm::Send { channel, value, .. } => {
                            out.push(*channel);
                            out.push(*value);
                        }
                        ControlSelectArm::Timeout { ms, .. } => out.push(*ms),
                        ControlSelectArm::Default { .. } => {}
                    }
                }
                out
            }
        }
    }

    /// Rewrite every [`VReg`] operand read or written by this instruction using `f`.
    pub fn remap_regs(&mut self, mut f: impl FnMut(VReg) -> VReg) {
        match self {
            Self::Const(d, _)
            | Self::LoadLocal(d, _)
            | Self::BorrowLocal(d, _)
            | Self::MoveLocal(d, _)
            | Self::IsMissingArg(d, _)
            | Self::LoadUpvalue(d, _)
            | Self::LoadGlobal(d, _)
            | Self::MoveGlobal(d, _)
            | Self::LoadDyn(d, _)
            | Self::MakeClosure { dst: d, .. }
            | Self::Handle { dst: d, .. }
            | Self::Transaction { dst: d, .. }
            | Self::Parallel { dst: d, .. }
            | Self::NoGrad { dst: d, .. } => {
                *d = f(*d);
            }
            Self::Copy(d, s)
            | Self::Unary(d, _, s)
            | Self::Assert(d, s)
            | Self::MakeDynTrait { dst: d, src: s, .. }
            | Self::GetConstGeneric { dst: d, src: s, .. }
            | Self::ProjectField {
                dst: d,
                receiver: s,
                ..
            } => {
                *d = f(*d);
                *s = f(*s);
            }
            Self::Binary(d, l, _, r)
            | Self::Pipe {
                dst: d,
                lhs: l,
                rhs: r,
            } => {
                *d = f(*d);
                *l = f(*l);
                *r = f(*r);
            }
            Self::Index(d, o, i) => {
                *d = f(*d);
                *o = f(*o);
                *i = f(*i);
            }
            Self::IndexAssign(d, o, i, v) => {
                *d = f(*d);
                *o = f(*o);
                *i = f(*i);
                *v = f(*v);
            }
            Self::TensorSlice { dst, target, axes } => {
                *dst = f(*dst);
                *target = f(*target);
                for ax in axes {
                    match ax {
                        ControlSliceAxis::Index(r) => *r = f(*r),
                        ControlSliceAxis::Range { start, stop, step } => {
                            if let Some(r) = start {
                                *r = f(*r);
                            }
                            if let Some(r) = stop {
                                *r = f(*r);
                            }
                            if let Some(r) = step {
                                *r = f(*r);
                            }
                        }
                        ControlSliceAxis::Ellipsis | ControlSliceAxis::NewAxis => {}
                    }
                }
            }
            Self::ListLit(d, items) => {
                *d = f(*d);
                for item in items {
                    *item = f(*item);
                }
            }
            Self::DictLit(d, pairs) => {
                *d = f(*d);
                for (_, r) in pairs {
                    *r = f(*r);
                }
            }
            Self::Quasiquote(d, segs) => {
                *d = f(*d);
                for s in segs {
                    match s {
                        ControlQqSegment::Quote(_) => {}
                        ControlQqSegment::Unquote(r) | ControlQqSegment::UnquoteSplice(r) => {
                            *r = f(*r);
                        }
                    }
                }
            }
            Self::StoreLocal(_, s)
            | Self::DropReg(s)
            | Self::StoreUpvalue(_, s)
            | Self::StoreGlobal(_, s)
            | Self::DefineDyn(_, s, _)
            | Self::AssignDyn(_, s)
            | Self::KeepAlive(s) => {
                *s = f(*s);
            }
            Self::MatchPattern { dst_ok, val, .. } => {
                *dst_ok = f(*dst_ok);
                *val = f(*val);
            }
            Self::TryProbe(d, s) | Self::TryUnwrap(d, s) => {
                *d = f(*d);
                *s = f(*s);
            }
            Self::Call {
                dst,
                target,
                args,
                named_args,
            }
            | Self::CallGeneric {
                dst,
                target,
                args,
                named_args,
                ..
            } => {
                *dst = f(*dst);
                if let CalleeTarget::Value(v) = target {
                    *v = f(*v);
                }
                for a in args {
                    *a = f(*a);
                }
                for (_, a) in named_args {
                    *a = f(*a);
                }
            }
            Self::ModuleCall {
                dst,
                args,
                named_args,
                ..
            } => {
                *dst = f(*dst);
                for a in args {
                    *a = f(*a);
                }
                for (_, a) in named_args {
                    *a = f(*a);
                }
            }
            Self::MethodCall {
                dst,
                receiver,
                args,
                named_args,
                ..
            } => {
                *dst = f(*dst);
                *receiver = f(*receiver);
                for a in args {
                    *a = f(*a);
                }
                for (_, a) in named_args {
                    *a = f(*a);
                }
            }
            Self::EvalAssert { given, expects, .. } => {
                *given = f(*given);
                for e in expects {
                    *e = f(*e);
                }
            }
            Self::Spawn { dst, target } => {
                *dst = f(*dst);
                match target {
                    ControlSpawnTarget::Call {
                        target,
                        args,
                        named_args,
                    } => {
                        if let CalleeTarget::Value(v) = target {
                            *v = f(*v);
                        }
                        for a in args {
                            *a = f(*a);
                        }
                        for (_, a) in named_args {
                            *a = f(*a);
                        }
                    }
                    ControlSpawnTarget::MethodCall {
                        receiver,
                        args,
                        named_args,
                        ..
                    } => {
                        *receiver = f(*receiver);
                        for a in args {
                            *a = f(*a);
                        }
                        for (_, a) in named_args {
                            *a = f(*a);
                        }
                    }
                    ControlSpawnTarget::Callable(v) => *v = f(*v),
                }
            }
            Self::Select { dst, arms } => {
                *dst = f(*dst);
                for arm in arms {
                    match arm {
                        ControlSelectArm::Recv { channel, .. } => *channel = f(*channel),
                        ControlSelectArm::Send { channel, value, .. } => {
                            *channel = f(*channel);
                            *value = f(*value);
                        }
                        ControlSelectArm::Timeout { ms, .. } => *ms = f(*ms),
                        ControlSelectArm::Default { .. } => {}
                    }
                }
            }
            Self::ExportGlobal(..)
            | Self::DropLocal(..)
            | Self::FreshLocalCell(..)
            | Self::PushDefer { .. }
            | Self::ExportDyn(..)
            | Self::Import { .. }
            | Self::Commit
            | Self::Rollback
            | Self::RegisterTypeAlias { .. }
            | Self::RegisterEnum { .. }
            | Self::RegisterStruct { .. }
            | Self::RegisterTrait { .. }
            | Self::RegisterImpl { .. }
            | Self::RegisterMacro { .. }
            | Self::RegisterTask { .. }
            | Self::RegisterKernel { .. } => {}
        }
    }

    /// Classify side-effect behavior conservatively for optimization soundness.
    pub fn purity(&self) -> Purity {
        match self {
            Self::Const(..)
            | Self::Copy(..)
            | Self::Unary(_, UnaryOp::Truthy | UnaryOp::Not, _)
            | Self::ListLit(..)
            | Self::DictLit(..)
            | Self::MakeDynTrait { .. } => Purity::Pure,
            Self::Unary(_, UnaryOp::Neg, _) => Purity::Fallible,
            Self::Binary(_, _, op, _) => match op {
                BinaryOp::Div | BinaryOp::Mod => Purity::Fallible,
                _ => Purity::Fallible,
            },
            Self::Index(..)
            | Self::IndexAssign(..)
            | Self::TensorSlice { .. }
            | Self::GetConstGeneric { .. }
            | Self::Quasiquote(..)
            | Self::TryProbe(..)
            | Self::TryUnwrap(..)
            | Self::Assert(..) => Purity::Fallible,
            Self::MatchPattern { .. }
            | Self::MakeClosure { .. }
            | Self::LoadLocal(..)
            | Self::BorrowLocal(..)
            | Self::MoveLocal(..)
            | Self::DropLocal(..)
            | Self::DropReg(..)
            | Self::FreshLocalCell(..)
            | Self::IsMissingArg(..)
            | Self::StoreLocal(..)
            | Self::LoadUpvalue(..)
            | Self::StoreUpvalue(..)
            | Self::LoadGlobal(..)
            | Self::MoveGlobal(..)
            | Self::StoreGlobal(..)
            | Self::LoadDyn(..)
            | Self::DefineDyn(..)
            | Self::AssignDyn(..)
            | Self::ExportGlobal(..)
            | Self::ExportDyn(..)
            | Self::Call { .. }
            | Self::ModuleCall { .. }
            | Self::MethodCall { .. }
            | Self::ProjectField { .. }
            | Self::Pipe { .. }
            | Self::Handle { .. }
            | Self::Transaction { .. }
            | Self::Parallel { .. }
            | Self::Spawn { .. }
            | Self::Select { .. }
            | Self::NoGrad { .. }
            | Self::PushDefer { .. }
            | Self::Import { .. }
            | Self::EvalAssert { .. }
            | Self::Commit
            | Self::Rollback
            | Self::RegisterTypeAlias { .. }
            | Self::RegisterEnum { .. }
            | Self::RegisterStruct { .. }
            | Self::RegisterTrait { .. }
            | Self::RegisterImpl { .. }
            | Self::RegisterMacro { .. }
            | Self::RegisterTask { .. }
            | Self::RegisterKernel { .. }
            | Self::CallGeneric { .. }
            | Self::KeepAlive(..) => Purity::Effectful,
        }
    }
}

/// Terminator of a [`BasicBlock`] in explicit CFG form.
#[derive(Debug, Clone)]
pub enum ControlTerminator {
    /// Unconditional branch to `target`, passing `args` into `target.params`.
    Jump { target: BlockId, args: Vec<VReg> },
    /// Conditional branch based on `cond` (already evaluated bool/value via truthiness).
    Branch {
        cond: VReg,
        then_bb: BlockId,
        then_args: Vec<VReg>,
        else_bb: BlockId,
        else_args: Vec<VReg>,
    },
    /// Return optional value from the current [`ControlFunction`].
    Return(Option<VReg>),
    /// Delimited algebraic effect yield: unwinds frames to nearest matching handler, captures
    /// continuation resuming at `resume_bb(reply)`, and invokes handler.
    Perform {
        effect: SymId,
        args: Vec<VReg>,
        resume_bb: BlockId,
    },
    /// Explicit tail-call terminator replacing the current frame on the heap frame stack.
    TailCall {
        target: CalleeTarget,
        args: Vec<VReg>,
    },
    /// VM execution halt signal.
    Halt(Option<VReg>),
    /// Dead/unreachable terminator following an unconditional return/break/continue.
    Unreachable,
}

impl ControlTerminator {
    /// List outgoing successor block IDs of this terminator.
    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            Self::Jump { target, .. } => vec![*target],
            Self::Branch {
                then_bb, else_bb, ..
            } => vec![*then_bb, *else_bb],
            Self::Perform { resume_bb, .. } => vec![*resume_bb],
            Self::Return(_) | Self::TailCall { .. } | Self::Halt(_) | Self::Unreachable => {
                Vec::new()
            }
        }
    }

    /// Collect input [`VReg`]s read by this terminator.
    pub fn input_regs(&self) -> Vec<VReg> {
        match self {
            Self::Jump { args, .. } => args.clone(),
            Self::Branch {
                cond,
                then_args,
                else_args,
                ..
            } => {
                let mut out = vec![*cond];
                out.extend_from_slice(then_args);
                out.extend_from_slice(else_args);
                out
            }
            Self::Return(r) | Self::Halt(r) => r.iter().copied().collect(),
            Self::Perform { args, .. } => args.clone(),
            Self::TailCall { target, args } => {
                let mut out = Vec::new();
                if let CalleeTarget::Value(v) = target {
                    out.push(*v);
                }
                out.extend_from_slice(args);
                out
            }
            Self::Unreachable => Vec::new(),
        }
    }

    /// Rewrite input [`VReg`]s of this terminator in place.
    pub fn remap_regs(&mut self, mut f: impl FnMut(VReg) -> VReg) {
        match self {
            Self::Jump { args, .. } => {
                for a in args {
                    *a = f(*a);
                }
            }
            Self::Branch {
                cond,
                then_args,
                else_args,
                ..
            } => {
                *cond = f(*cond);
                for a in then_args {
                    *a = f(*a);
                }
                for a in else_args {
                    *a = f(*a);
                }
            }
            Self::Return(Some(r)) | Self::Halt(Some(r)) => {
                *r = f(*r);
            }
            Self::Return(None) | Self::Halt(None) | Self::Unreachable => {}
            Self::Perform { args, .. } => {
                for a in args {
                    *a = f(*a);
                }
            }
            Self::TailCall { target, args } => {
                if let CalleeTarget::Value(v) = target {
                    *v = f(*v);
                }
                for a in args {
                    *a = f(*a);
                }
            }
        }
    }
}

/// Exact arbitrary-arity operand last-use bitset for Perceus / FBIP ownership transfer.
///
/// Stores `<= 64` operand bits inline in a single `u64` (zero heap allocation on virtually all
/// instructions and terminators) and transparently spills to `Box<[u64]>` when an instruction or
/// terminator has `> 64` input registers — guaranteeing uniform in-place ownership transfer at
/// every operand index with no 64-bit truncation or post-instruction fallback.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum KillMask {
    #[default]
    Empty,
    Inline(u64),
    Words(Box<[u64]>),
}

impl KillMask {
    #[inline]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Empty => true,
            Self::Inline(w) => *w == 0,
            Self::Words(ws) => ws.iter().all(|&w| w == 0),
        }
    }

    #[inline]
    pub fn test(&self, bit: usize) -> bool {
        match self {
            Self::Empty => false,
            Self::Inline(w) => bit < 64 && ((*w >> bit) & 1) != 0,
            Self::Words(ws) => ws
                .get(bit / 64)
                .is_some_and(|w| ((*w >> (bit % 64)) & 1) != 0),
        }
    }

    #[inline]
    pub fn set(&mut self, bit: usize) {
        if bit < 64 {
            match self {
                Self::Empty => *self = Self::Inline(1u64 << bit),
                Self::Inline(w) => *w |= 1u64 << bit,
                Self::Words(ws) => {
                    if let Some(w) = ws.get_mut(0) {
                        *w |= 1u64 << bit;
                    } else {
                        *self = Self::Inline(1u64 << bit);
                    }
                }
            }
            return;
        }
        let word_idx = bit / 64;
        let bit_off = bit % 64;
        match self {
            Self::Empty => {
                let mut v = vec![0u64; word_idx + 1];
                v[word_idx] = 1u64 << bit_off;
                *self = Self::Words(v.into_boxed_slice());
            }
            Self::Inline(w0) => {
                let mut v = vec![0u64; word_idx + 1];
                v[0] = *w0;
                v[word_idx] = 1u64 << bit_off;
                *self = Self::Words(v.into_boxed_slice());
            }
            Self::Words(ws) => {
                if word_idx >= ws.len() {
                    let mut v = ws.to_vec();
                    v.resize(word_idx + 1, 0);
                    v[word_idx] |= 1u64 << bit_off;
                    *self = Self::Words(v.into_boxed_slice());
                } else {
                    ws[word_idx] |= 1u64 << bit_off;
                }
            }
        }
    }

    /// True if any set bit lies at index `>= bound`.
    pub fn has_bits_at_or_above(&self, bound: usize) -> bool {
        match self {
            Self::Empty => false,
            Self::Inline(w) => bound < 64 && (*w >> bound) != 0,
            Self::Words(ws) => {
                let first_word = bound / 64;
                let rem = bound % 64;
                if let Some(&w) = ws.get(first_word)
                    && (w >> rem) != 0
                {
                    return true;
                }
                let start = if rem == 0 { first_word } else { first_word + 1 };
                ws.get(start..)
                    .is_some_and(|tail| tail.iter().any(|&w| w != 0))
            }
        }
    }

    #[inline]
    pub fn words(&self) -> &[u64] {
        match self {
            Self::Empty => &[],
            Self::Inline(0) => &[],
            Self::Inline(w) => std::slice::from_ref(w),
            Self::Words(ws) => ws,
        }
    }

    pub fn from_words(mut words: Vec<u64>) -> Self {
        while words.last() == Some(&0) {
            words.pop();
        }
        match words.len() {
            0 => Self::Empty,
            1 => Self::Inline(words[0]),
            _ => Self::Words(words.into_boxed_slice()),
        }
    }
}

/// One basic block in a [`ControlFunction`].
#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub id: BlockId,
    pub params: Vec<VReg>,
    pub insts: Vec<ControlInst>,
    /// P6-1 Perceus/FBIP: either empty (unannotated) or 1:1 with `insts`. Bit `k` of
    /// `inst_kill_masks[pc]` is 1 iff the `k`-th register in `insts[pc].input_regs()` is the final
    /// live read of that `VReg` on all reachable paths and its final occurrence in `insts[pc]`.
    pub inst_kill_masks: Vec<KillMask>,
    /// P6-1 Perceus/FBIP: exact last-use ownership bitset for `terminator` register operands.
    pub term_kill_mask: KillMask,
    pub terminator: ControlTerminator,
}

/// One compiled callable unit (main script, task, closure, macro, handler, worker, etc.).
#[derive(Debug, Clone)]
pub struct ControlFunction {
    pub id: FnId,
    pub name: Option<SymId>,
    pub kind: FunctionKind,
    pub params: Vec<SymId>,
    /// Wave 3 Stage 3C: `param_has_default[i] == true` when parameter `i` has a default initializer
    /// lowered into the entry prologue via `IsMissingArg`.
    pub param_has_default: Vec<bool>,
    /// P6-1 Perceus/FBIP: `param_borrows[i] == true` when parameter `i` is statically inferred as
    /// read-only & non-escaping (borrowed rather than owned).
    pub param_borrows: Vec<bool>,
    pub upvalues: Vec<UpvalueDesc>,
    /// For `LocalSlot(i)`: true if an inner closure/handler captures it as a shared mutable cell.
    pub shared_locals: Vec<bool>,
    /// Mapping `LocalSlot(i) -> SymId` for runtime `eval` scope materialization and diagnostics.
    pub local_names: Vec<SymId>,
    pub n_locals: u16,
    pub n_regs: u32,
    pub blocks: Vec<BasicBlock>,
    pub entry: BlockId,
    /// For `HandleHandler`: optional slot where `k_param` (`"resume"`) is bound, plus positional
    /// slots for `__arg0..__argN` referenced inside this handler.
    pub handler_k_slot: Option<LocalSlot>,
    pub handler_arg_slots: Vec<Option<LocalSlot>>,
}

impl ControlFunction {
    /// Compute predecessor adjacency lists indexed by `BlockId`.
    pub fn predecessors(&self) -> Vec<Vec<BlockId>> {
        let mut preds = vec![Vec::new(); self.blocks.len()];
        for b in &self.blocks {
            for succ in b.terminator.successors() {
                if let Some(p) = preds.get_mut(succ.0 as usize)
                    && !p.contains(&b.id)
                {
                    p.push(b.id);
                }
            }
        }
        preds
    }

    /// Return reachable blocks in Reverse Post-Order starting at `self.entry`.
    pub fn rpo_order(&self) -> Vec<BlockId> {
        let mut visited = vec![false; self.blocks.len()];
        let mut post = Vec::new();
        // Iterative DFS to remain stack-safe even on large CFGs.
        let mut stack: Vec<(BlockId, usize)> = vec![(self.entry, 0)];
        if (self.entry.0 as usize) < visited.len() {
            visited[self.entry.0 as usize] = true;
        }
        while let Some(&(bid, next_idx)) = stack.last() {
            let succs = self.blocks[bid.0 as usize].terminator.successors();
            if next_idx < succs.len() {
                if let Some(top) = stack.last_mut() {
                    top.1 += 1;
                }
                let s = succs[next_idx];
                if let Some(v) = visited.get_mut(s.0 as usize)
                    && !*v
                {
                    *v = true;
                    stack.push((s, 0));
                }
            } else {
                stack.pop();
                post.push(bid);
            }
        }
        post.reverse();
        post
    }

    /// Compute immediate dominator (`idom[b] = Some(dom)` or `None` for entry/unreachable)
    /// using the iterative Cooper-Harvey-Kennedy algorithm on RPO.
    pub fn compute_idom(&self) -> Vec<Option<BlockId>> {
        let n = self.blocks.len();
        let preds = self.predecessors();
        let rpo = self.rpo_order();
        let mut rpo_num = vec![usize::MAX; n];
        for (idx, b) in rpo.iter().enumerate() {
            rpo_num[b.0 as usize] = idx;
        }
        let mut idom: Vec<Option<BlockId>> = vec![None; n];
        idom[self.entry.0 as usize] = Some(self.entry);

        let intersect = |mut b1: BlockId, mut b2: BlockId, idom: &[Option<BlockId>]| -> BlockId {
            while b1 != b2 {
                while rpo_num[b1.0 as usize] > rpo_num[b2.0 as usize] {
                    b1 = idom[b1.0 as usize].expect("reachable predecessor must have idom");
                }
                while rpo_num[b2.0 as usize] > rpo_num[b1.0 as usize] {
                    b2 = idom[b2.0 as usize].expect("reachable predecessor must have idom");
                }
            }
            b1
        };

        let mut changed = true;
        while changed {
            changed = false;
            for &b in &rpo {
                if b == self.entry {
                    continue;
                }
                let mut new_idom: Option<BlockId> = None;
                for &p in &preds[b.0 as usize] {
                    if idom[p.0 as usize].is_some() {
                        new_idom = Some(match new_idom {
                            None => p,
                            Some(cur) => intersect(p, cur, &idom),
                        });
                    }
                }
                if new_idom.is_some() && idom[b.0 as usize] != new_idom {
                    idom[b.0 as usize] = new_idom;
                    changed = true;
                }
            }
        }
        idom[self.entry.0 as usize] = None;
        idom
    }

    /// Return true if block `a` dominates block `b` using precomputed `idom`.
    pub fn dominates(&self, idom: &[Option<BlockId>], a: BlockId, mut b: BlockId) -> bool {
        if a == b {
            return true;
        }
        let mut steps = 0usize;
        while let Some(d) = idom[b.0 as usize] {
            if d == a {
                return true;
            }
            if d == b || steps > idom.len() {
                break;
            }
            b = d;
            steps += 1;
        }
        false
    }
}

/// Complete compiled program in canonical Control SSA IR.
#[derive(Debug, Clone)]
pub struct ControlProgram {
    pub symbols: SymbolTable,
    pub functions: Vec<ControlFunction>,
    pub entry: FnId,
    pub globals: Vec<GlobalMeta>,
    /// P6-1 Perceus/FBIP: whether `<main>`-private global slots must keep their values after
    /// `<main>` returns.
    ///
    /// `true` when an embedding environment captures the program globals after execution (the
    /// REPL copies every global of a chunk into the session environment; `eval` restores the
    /// caller's environment). `false` for scripts, AOT binaries and module initializers, whose
    /// globals are dead once `<main>` returns.
    ///
    /// This is exactly what decides whether the Perceus pass may transfer a private global's
    /// ownership out of its slot (`MoveGlobal`) at its last use — the difference between
    /// `let x = …; f(x)` reusing `x`'s buffer in place and cloning it.
    pub preserve_globals_at_exit: bool,
}

impl ControlProgram {
    /// Fetch function by [`FnId`].
    pub fn func(&self, id: FnId) -> &ControlFunction {
        &self.functions[id.0 as usize]
    }
}

/// Structurally verify CFG well-formedness, register bounds, block-argument arity,
/// and SSA dominance invariants across an entire [`ControlProgram`].
pub fn verify_program(prog: &ControlProgram) -> MoraResult<()> {
    if (prog.entry.0 as usize) >= prog.functions.len() {
        return Err(MoraError::Parse(format!(
            "ControlProgram entry {:?} out of range (len {})",
            prog.entry,
            prog.functions.len()
        )));
    }
    // P6-1 Perceus/FBIP: the `<main>`-private globals whose ownership may be transferred out
    // of their slot, and the subset that must survive `<main>` because the embedding
    // environment captures program globals after execution.
    let private_main_globals = crate::mir::control::liveness::compute_private_main_globals(prog);
    let exit_globals: HashSet<GlobalSlot> = if prog.preserve_globals_at_exit {
        private_main_globals.clone()
    } else {
        HashSet::new()
    };

    for (idx, func) in prog.functions.iter().enumerate() {
        if func.id.0 as usize != idx {
            return Err(MoraError::Parse(format!(
                "Function slot {idx} has mismatched id {:?}",
                func.id
            )));
        }
        verify_function(
            func,
            &prog.functions,
            &prog.symbols,
            prog.globals.len(),
            func.id == prog.entry,
            &private_main_globals,
            &exit_globals,
        )?;
    }
    Ok(())
}

fn verify_function(
    func: &ControlFunction,
    all_funcs: &[ControlFunction],
    symbols: &SymbolTable,
    n_globals: usize,
    is_main: bool,
    private_main_globals: &HashSet<GlobalSlot>,
    exit_globals: &HashSet<GlobalSlot>,
) -> MoraResult<()> {
    let n_funcs = all_funcs.len();
    let n_blocks = func.blocks.len();
    if (func.entry.0 as usize) >= n_blocks {
        return Err(MoraError::Parse(format!(
            "fn {:?}: entry block {:?} >= n_blocks {n_blocks}",
            func.id, func.entry
        )));
    }

    // Map each VReg definition to (BlockId, inst_position: Option<usize>) where None = BlockParam.
    let mut defs: HashMap<VReg, (BlockId, Option<usize>)> = HashMap::new();
    for (bidx, block) in func.blocks.iter().enumerate() {
        if block.id.0 as usize != bidx {
            return Err(MoraError::Parse(format!(
                "fn {:?}: block index {bidx} has id {:?}",
                func.id, block.id
            )));
        }
        for &p in &block.params {
            if p.0 >= func.n_regs {
                return Err(MoraError::Parse(format!(
                    "fn {:?} {:?}: param reg {:?} >= n_regs {}",
                    func.id, block.id, p, func.n_regs
                )));
            }
            if defs.insert(p, (block.id, None)).is_some() {
                return Err(MoraError::Parse(format!(
                    "fn {:?} {:?}: duplicate SSA definition of {:?}",
                    func.id, block.id, p
                )));
            }
        }
        for (iidx, inst) in block.insts.iter().enumerate() {
            if let Some(d) = inst.dst() {
                if d.0 >= func.n_regs {
                    return Err(MoraError::Parse(format!(
                        "fn {:?} {:?}: dst reg {:?} >= n_regs {}",
                        func.id, block.id, d, func.n_regs
                    )));
                }
                if defs.insert(d, (block.id, Some(iidx))).is_some() {
                    return Err(MoraError::Parse(format!(
                        "fn {:?} {:?}: duplicate SSA definition of {:?}",
                        func.id, block.id, d
                    )));
                }
            }
            verify_inst_slots(inst, func, n_funcs, n_globals)?;
        }
        // Check terminator block targets and block-argument arity
        match &block.terminator {
            ControlTerminator::Jump { target, args } => {
                check_edge_args(func, block.id, *target, args)?;
            }
            ControlTerminator::Branch {
                then_bb,
                then_args,
                else_bb,
                else_args,
                ..
            } => {
                check_edge_args(func, block.id, *then_bb, then_args)?;
                check_edge_args(func, block.id, *else_bb, else_args)?;
            }
            ControlTerminator::Perform { resume_bb, .. } => {
                let target_bb = func.blocks.get(resume_bb.0 as usize).ok_or_else(|| {
                    MoraError::Parse(format!(
                        "fn {:?} {:?}: perform target {:?} OOB",
                        func.id, block.id, resume_bb
                    ))
                })?;
                if target_bb.params.len() != 1 {
                    return Err(MoraError::Parse(format!(
                        "fn {:?} {:?}: perform resume_bb {:?} must accept 1 reply param, got {}",
                        func.id,
                        block.id,
                        resume_bb,
                        target_bb.params.len()
                    )));
                }
            }
            ControlTerminator::Return(_)
            | ControlTerminator::TailCall { .. }
            | ControlTerminator::Halt(_)
            | ControlTerminator::Unreachable => {}
        }
    }

    // Verify dominance over reachable blocks.
    let idom = func.compute_idom();
    let rpo = func.rpo_order();
    let reachable: HashSet<BlockId> = rpo.into_iter().collect();

    // Queue reachability double check vs rpo
    let mut seen = HashSet::new();
    let mut q = VecDeque::from([func.entry]);
    seen.insert(func.entry);
    while let Some(b) = q.pop_front() {
        for s in func.blocks[b.0 as usize].terminator.successors() {
            if seen.insert(s) {
                q.push_back(s);
            }
        }
    }
    if seen.len() != reachable.len() {
        return Err(MoraError::Parse(format!(
            "fn {:?}: RPO/BFS reachability mismatch",
            func.id
        )));
    }

    for &bid in &reachable {
        let block = &func.blocks[bid.0 as usize];
        if !block.inst_kill_masks.is_empty() && block.inst_kill_masks.len() != block.insts.len() {
            return Err(MoraError::Parse(format!(
                "fn {:?} {:?}: inst_kill_masks len {} != insts len {}",
                func.id,
                bid,
                block.inst_kill_masks.len(),
                block.insts.len()
            )));
        }
        for (iidx, inst) in block.insts.iter().enumerate() {
            if let ControlInst::DropReg(r) = inst
                && r.0 >= func.n_regs
            {
                return Err(MoraError::Parse(format!(
                    "fn {:?} {:?}: DropReg {:?} >= n_regs {}",
                    func.id, bid, r, func.n_regs
                )));
            }
            for used in inst.input_regs() {
                check_dominance(func, &idom, &defs, used, bid, Some(iidx))?;
            }
        }
        for used in block.terminator.input_regs() {
            check_dominance(func, &idom, &defs, used, bid, None)?;
        }
    }

    verify_perceus_kills(func, &reachable)?;
    verify_perceus_project_fields(func, &reachable)?;
    verify_perceus_slot_moves(
        func,
        all_funcs,
        symbols,
        &reachable,
        is_main,
        private_main_globals,
        exit_globals,
    )?;
    Ok(())
}

/// P6-1 Perceus/FBIP soundness verifier for destructive in-place `ProjectField` reads.
///
/// `VmEngine` extracts nominal fields out of a uniquely owned `StructInstance` in place even
/// before the receiver's last use so coalesced sibling projections (`p.a`, `p.b`) remain $O(1)$
/// in any field order. That mutation is sound iff a `ProjectField` whose `receiver` stays live
/// after `pc` never escapes the basic block and is only followed in the same block by sibling
/// `ProjectField`s projecting strictly distinct fields.
fn verify_perceus_project_fields(
    func: &ControlFunction,
    reachable: &HashSet<BlockId>,
) -> MoraResult<()> {
    use crate::mir::control::liveness;

    let has_project = func.blocks.iter().any(|b| {
        b.insts
            .iter()
            .any(|i| matches!(i, ControlInst::ProjectField { .. }))
    });
    if !has_project {
        return Ok(());
    }

    let live_in = liveness::compute_vreg_live_in(func, reachable);
    for &bid in reachable {
        let block = &func.blocks[bid.0 as usize];
        let mut live_out: HashSet<VReg> = HashSet::new();
        for succ in block.terminator.successors() {
            if reachable.contains(&succ) {
                let sblk = &func.blocks[succ.0 as usize];
                for &r in &live_in[succ.0 as usize] {
                    if !sblk.params.contains(&r) {
                        live_out.insert(r);
                    }
                }
            }
        }
        let term_inputs = block.terminator.input_regs();
        let mut live = live_out.clone();
        for &r in &term_inputs {
            live.insert(r);
        }

        for (pc, inst) in block.insts.iter().enumerate().rev() {
            if let ControlInst::DropReg(r) = inst {
                live.remove(r);
                continue;
            }
            if let Some(d) = inst.dst() {
                live.remove(&d);
            }
            if let ControlInst::ProjectField {
                receiver, field, ..
            } = inst
                && live.contains(receiver)
            {
                if live_out.contains(receiver) || term_inputs.contains(receiver) {
                    return Err(MoraError::Parse(format!(
                        "fn {:?} {:?} pc {pc}: ProjectField receiver {:?} escapes block after destructive projection",
                        func.id, bid, receiver
                    )));
                }
                for (next_pc, later) in block.insts.iter().enumerate().skip(pc + 1) {
                    if matches!(later, ControlInst::DropReg(r) if r == receiver) {
                        break;
                    }
                    if !later.input_regs().contains(receiver) {
                        continue;
                    }
                    match later {
                        ControlInst::ProjectField {
                            receiver: r2,
                            field: f2,
                            ..
                        } if r2 == receiver && f2 != field => {}
                        ControlInst::ProjectField { field: f2, .. } if f2 == field => {
                            return Err(MoraError::Parse(format!(
                                "fn {:?} {:?} pc {pc}: duplicate ProjectField of field {:?} on receiver {:?} at pc {next_pc}",
                                func.id, bid, field, receiver
                            )));
                        }
                        _ => {
                            return Err(MoraError::Parse(format!(
                                "fn {:?} {:?} pc {pc}: ProjectField receiver {:?} used by non-projection instruction at pc {next_pc}",
                                func.id, bid, receiver
                            )));
                        }
                    }
                }
            }
            for r in inst.input_regs() {
                live.insert(r);
            }
        }
    }
    Ok(())
}

/// P6-1 Perceus/FBIP soundness verifier for storage-slot ownership transfers.
///
/// A `MoveLocal` / `MoveGlobal` destructively clears the slot it reads, and a `DropLocal`
/// clears it outright — so each of them may only fire on a slot that is dead on every reachable
/// path. The liveness is re-derived here with the very same analysis the optimizer used
/// (`liveness::compute_slot_liveness`), which makes an unsound transfer a compile-time error
/// instead of a silently dropped value at runtime.
fn verify_perceus_slot_moves(
    func: &ControlFunction,
    all_funcs: &[ControlFunction],
    symbols: &SymbolTable,
    reachable: &HashSet<BlockId>,
    is_main: bool,
    private_main_globals: &HashSet<GlobalSlot>,
    exit_globals: &HashSet<GlobalSlot>,
) -> MoraResult<()> {
    use crate::mir::control::liveness;

    let has_slot_move = func.blocks.iter().any(|b| {
        b.insts.iter().any(|i| {
            matches!(
                i,
                ControlInst::MoveLocal(..)
                    | ControlInst::BorrowLocal(..)
                    | ControlInst::MoveGlobal(..)
                    | ControlInst::DropLocal(_)
            )
        })
    });
    if !has_slot_move {
        return Ok(());
    }

    let eligible_local = |s: LocalSlot| -> bool {
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
    // Same exit seed as the optimizer: only `<main>` can own a `<main>`-private global.
    let no_exit_globals: HashSet<GlobalSlot> = HashSet::new();
    let exit_seed: &HashSet<GlobalSlot> = if is_main {
        exit_globals
    } else {
        &no_exit_globals
    };
    let live =
        liveness::compute_slot_liveness(func, &eligible_local, private_main_globals, exit_seed);

    for &bid in reachable {
        let block = &func.blocks[bid.0 as usize];
        let local_out = &live.local_live_out[bid.0 as usize];
        let global_out = &live.global_live_out[bid.0 as usize];
        for (pc, inst) in block.insts.iter().enumerate() {
            match inst {
                ControlInst::BorrowLocal(dst, s) => {
                    // Find every use of `dst` across the function: all must live at a single
                    // borrowing consumer (`Call`, `MethodCall`, or `TailCall` terminator) strictly
                    // after `pc` in `block`, validated by `liveness::is_valid_borrow_*`.
                    let mut consumer_pc: Option<usize> = None;
                    for &ubid in reachable {
                        let ub = &func.blocks[ubid.0 as usize];
                        if ub.terminator.input_regs().contains(dst) {
                            if ubid != bid
                                || !liveness::is_valid_borrow_terminator(
                                    &ub.terminator,
                                    *dst,
                                    all_funcs,
                                )
                            {
                                return Err(MoraError::Parse(format!(
                                    "fn {:?} {:?} pc {pc}: BorrowLocal reg {:?} escapes into non-borrowing terminator",
                                    func.id, bid, dst
                                )));
                            }
                            if consumer_pc.replace(ub.insts.len()).is_some() {
                                return Err(MoraError::Parse(format!(
                                    "fn {:?} {:?} pc {pc}: BorrowLocal reg {:?} used by multiple consumers",
                                    func.id, bid, dst
                                )));
                            }
                        }
                        for (upc, uinst) in ub.insts.iter().enumerate() {
                            if !uinst.input_regs().contains(dst) {
                                continue;
                            }
                            if ubid != bid || upc <= pc {
                                return Err(MoraError::Parse(format!(
                                    "fn {:?} {:?} pc {pc}: BorrowLocal reg {:?} used outside following block consumer",
                                    func.id, bid, dst
                                )));
                            }
                            if consumer_pc.replace(upc).is_some_and(|prev| prev != upc) {
                                return Err(MoraError::Parse(format!(
                                    "fn {:?} {:?} pc {pc}: BorrowLocal reg {:?} used by multiple instructions",
                                    func.id, bid, dst
                                )));
                            }
                            if !liveness::is_valid_borrow_inst(uinst, *dst, all_funcs, symbols) {
                                return Err(MoraError::Parse(format!(
                                    "fn {:?} {:?} pc {pc}: BorrowLocal reg {:?} passed to non-borrowing consumer",
                                    func.id, bid, dst
                                )));
                            }
                        }
                    }
                    let Some(upc) = consumer_pc else {
                        return Err(MoraError::Parse(format!(
                            "fn {:?} {:?} pc {pc}: unused BorrowLocal reg {:?}",
                            func.id, bid, dst
                        )));
                    };
                    let end_excl = upc.saturating_add(1).min(block.insts.len());
                    for inner in &block.insts[pc + 1..end_excl] {
                        if liveness::inst_clobbers_local(inner, *s) {
                            return Err(MoraError::Parse(format!(
                                "fn {:?} {:?} pc {pc}: slot {:?} clobbered before BorrowLocal consumer at pc {upc}",
                                func.id, bid, s
                            )));
                        }
                    }
                }
                ControlInst::MoveLocal(_, s) => {
                    let after = liveness::local_live_after(block, pc, local_out, &eligible_local);
                    if after.contains(s) {
                        return Err(MoraError::Parse(format!(
                            "fn {:?} {:?} pc {pc}: MoveLocal on still-live local slot {:?}",
                            func.id, bid, s
                        )));
                    }
                }
                ControlInst::MoveGlobal(_, g) => {
                    if !is_main {
                        return Err(MoraError::Parse(format!(
                            "fn {:?} {:?} pc {pc}: MoveGlobal is only allowed in <main>",
                            func.id, bid
                        )));
                    }
                    if !private_main_globals.contains(g) {
                        return Err(MoraError::Parse(format!(
                            "fn {:?} {:?} pc {pc}: MoveGlobal on non-private global {:?}",
                            func.id, bid, g
                        )));
                    }
                    let after =
                        liveness::global_live_after(block, pc, global_out, private_main_globals);
                    if after.contains(g) {
                        return Err(MoraError::Parse(format!(
                            "fn {:?} {:?} pc {pc}: MoveGlobal on still-live global {:?}",
                            func.id, bid, g
                        )));
                    }
                }
                ControlInst::DropLocal(s)
                    if liveness::local_live_after(block, pc, local_out, &eligible_local)
                        .contains(s) =>
                {
                    return Err(MoraError::Parse(format!(
                        "fn {:?} {:?} pc {pc}: DropLocal on still-live local slot {:?}",
                        func.id, bid, s
                    )));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// P6-1 Perceus/FBIP soundness verifier: ensures no `inst_kill_masks` / `term_kill_mask` bit kills
/// a `VReg` that is still live on any reachable path, and no `DropReg` drops a live `VReg`.
fn verify_perceus_kills(func: &ControlFunction, reachable: &HashSet<BlockId>) -> MoraResult<()> {
    use crate::mir::control::liveness;

    let any_annotated = func.blocks.iter().any(|b| {
        !b.inst_kill_masks.is_empty()
            || !b.term_kill_mask.is_empty()
            || b.insts
                .iter()
                .any(|inst| matches!(inst, ControlInst::DropReg(_)))
    });
    if !any_annotated {
        return Ok(());
    }

    let live_in = liveness::compute_vreg_live_in(func, reachable);

    for &bid in reachable {
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

        // Verify terminator kill mask against the exact shared `compute_terminator_kill_mask` oracle.
        let tmask = &block.term_kill_mask;
        if !tmask.is_empty() {
            let max_valid =
                liveness::compute_terminator_kill_mask(func, bid, reachable, &live_in, &live);
            let t_inputs = block.terminator.input_regs();
            if tmask.has_bits_at_or_above(t_inputs.len()) {
                return Err(MoraError::Parse(format!(
                    "fn {:?} {:?}: term_kill_mask has out-of-range bits (len {})",
                    func.id,
                    bid,
                    t_inputs.len()
                )));
            }
            for (i, &r) in t_inputs.iter().enumerate() {
                if tmask.test(i) && !max_valid.test(i) {
                    return Err(MoraError::Parse(format!(
                        "fn {:?} {:?}: terminator reg {:?} (operand {i}) killed while still live",
                        func.id, bid, r
                    )));
                }
            }
        }

        for r in block.terminator.input_regs() {
            live.insert(r);
        }

        for (pc, inst) in block.insts.iter().enumerate().rev() {
            let imask = block.inst_kill_masks.get(pc);
            if let ControlInst::DropReg(r) = inst {
                if imask.is_some_and(|m| !m.is_empty()) {
                    return Err(MoraError::Parse(format!(
                        "fn {:?} {:?} pc {pc}: DropReg must have empty kill_mask",
                        func.id, bid
                    )));
                }
                if live.contains(r) {
                    return Err(MoraError::Parse(format!(
                        "fn {:?} {:?} pc {pc}: DropReg on live reg {:?}",
                        func.id, bid, r
                    )));
                }
                live.remove(r);
                continue;
            }
            if let Some(d) = inst.dst() {
                live.remove(&d);
            }
            let inputs = inst.input_regs();
            if let Some(m) = imask
                && m.has_bits_at_or_above(inputs.len())
            {
                return Err(MoraError::Parse(format!(
                    "fn {:?} {:?} pc {pc}: inst_kill_mask has out-of-range bits",
                    func.id, bid
                )));
            }
            for (k, &r) in inputs.iter().enumerate().rev() {
                if imask.is_some_and(|m| m.test(k)) && live.contains(&r) {
                    return Err(MoraError::Parse(format!(
                        "fn {:?} {:?} pc {pc}: Perceus use-after-move on reg {:?}",
                        func.id, bid, r
                    )));
                }
                live.insert(r);
            }
        }
    }
    Ok(())
}

fn check_edge_args(
    func: &ControlFunction,
    from: BlockId,
    target: BlockId,
    args: &[VReg],
) -> MoraResult<()> {
    let target_bb = func.blocks.get(target.0 as usize).ok_or_else(|| {
        MoraError::Parse(format!(
            "fn {:?} {:?}: jump target {:?} OOB",
            func.id, from, target
        ))
    })?;
    if args.len() != target_bb.params.len() {
        return Err(MoraError::Parse(format!(
            "fn {:?} {:?} -> {:?}: edge passes {} args, block expects {}",
            func.id,
            from,
            target,
            args.len(),
            target_bb.params.len()
        )));
    }
    Ok(())
}

fn check_dominance(
    func: &ControlFunction,
    idom: &[Option<BlockId>],
    defs: &HashMap<VReg, (BlockId, Option<usize>)>,
    reg: VReg,
    use_bb: BlockId,
    use_inst: Option<usize>,
) -> MoraResult<()> {
    let &(def_bb, def_inst) = defs.get(&reg).ok_or_else(|| {
        MoraError::Parse(format!(
            "fn {:?} {:?}: use of undefined reg {:?}",
            func.id, use_bb, reg
        ))
    })?;
    if def_bb == use_bb {
        match (def_inst, use_inst) {
            (None, _) => Ok(()),
            (Some(di), Some(ui)) if di < ui => Ok(()),
            (Some(_), None) => Ok(()),
            (Some(di), Some(ui)) => Err(MoraError::Parse(format!(
                "fn {:?} {:?}: reg {:?} used at inst {ui} before definition at inst {di}",
                func.id, use_bb, reg
            ))),
        }
    } else if func.dominates(idom, def_bb, use_bb) {
        Ok(())
    } else {
        Err(MoraError::Parse(format!(
            "fn {:?}: def of {:?} in {:?} does not dominate use in {:?}",
            func.id, reg, def_bb, use_bb
        )))
    }
}

fn verify_inst_slots(
    inst: &ControlInst,
    func: &ControlFunction,
    n_funcs: usize,
    n_globals: usize,
) -> MoraResult<()> {
    match inst {
        ControlInst::LoadLocal(_, l)
        | ControlInst::BorrowLocal(_, l)
        | ControlInst::MoveLocal(_, l)
        | ControlInst::DropLocal(l)
        | ControlInst::FreshLocalCell(l)
        | ControlInst::IsMissingArg(_, l)
        | ControlInst::StoreLocal(l, _) => {
            if l.0 >= func.n_locals {
                return Err(MoraError::Parse(format!(
                    "fn {:?}: local slot {:?} >= n_locals {}",
                    func.id, l, func.n_locals
                )));
            }
            if matches!(
                inst,
                ControlInst::MoveLocal(_, _)
                    | ControlInst::BorrowLocal(_, _)
                    | ControlInst::DropLocal(_)
            ) && func
                .shared_locals
                .get(l.0 as usize)
                .copied()
                .unwrap_or(false)
            {
                return Err(MoraError::Parse(format!(
                    "fn {:?}: MoveLocal/BorrowLocal/DropLocal forbidden on shared local {:?}",
                    func.id, l
                )));
            }
            if matches!(
                inst,
                ControlInst::MoveLocal(_, _)
                    | ControlInst::DropLocal(_)
                    | ControlInst::StoreLocal(_, _)
                    | ControlInst::FreshLocalCell(_)
            ) && func
                .param_borrows
                .get(l.0 as usize)
                .copied()
                .unwrap_or(false)
            {
                return Err(MoraError::Parse(format!(
                    "fn {:?}: MoveLocal/DropLocal/StoreLocal/FreshLocalCell forbidden on borrowed parameter {:?}",
                    func.id, l
                )));
            }
        }
        ControlInst::MatchPattern { pattern, .. } => {
            for (idx, &borrowed) in func.param_borrows.iter().enumerate() {
                if borrowed
                    && crate::mir::control::liveness::pattern_mentions_local(
                        pattern,
                        LocalSlot(idx as u16),
                    )
                {
                    return Err(MoraError::Parse(format!(
                        "fn {:?}: MatchPattern binding forbidden on borrowed parameter LocalSlot({idx})",
                        func.id
                    )));
                }
            }
        }
        ControlInst::LoadUpvalue(_, u) | ControlInst::StoreUpvalue(u, _) => {
            if (u.0 as usize) >= func.upvalues.len() {
                return Err(MoraError::Parse(format!(
                    "fn {:?}: upvalue {:?} >= upvalues {}",
                    func.id,
                    u,
                    func.upvalues.len()
                )));
            }
        }
        ControlInst::LoadGlobal(_, g)
        | ControlInst::MoveGlobal(_, g)
        | ControlInst::StoreGlobal(g, _)
        | ControlInst::ExportGlobal(g) => {
            if (g.0 as usize) >= n_globals {
                return Err(MoraError::Parse(format!(
                    "fn {:?}: global {:?} >= globals {n_globals}",
                    func.id, g
                )));
            }
        }
        ControlInst::MakeClosure { fn_id, .. } => {
            if (fn_id.0 as usize) >= n_funcs {
                return Err(MoraError::Parse(format!(
                    "fn {:?}: closure fn {:?} OOB",
                    func.id, fn_id
                )));
            }
        }
        ControlInst::Handle {
            body_fn,
            handler_fn,
            ..
        } => {
            if (body_fn.0 as usize) >= n_funcs || (handler_fn.0 as usize) >= n_funcs {
                return Err(MoraError::Parse(format!(
                    "fn {:?}: handle sub-function OOB",
                    func.id
                )));
            }
        }
        ControlInst::Transaction {
            body_fn, comp_fn, ..
        } => {
            if (body_fn.0 as usize) >= n_funcs || (comp_fn.0 as usize) >= n_funcs {
                return Err(MoraError::Parse(format!(
                    "fn {:?}: transaction sub-function OOB",
                    func.id
                )));
            }
        }
        ControlInst::RegisterTask { fn_id, .. } if (fn_id.0 as usize) >= n_funcs => {
            return Err(MoraError::Parse(format!(
                "fn {:?}: task fn {:?} OOB",
                func.id, fn_id
            )));
        }
        ControlInst::NoGrad { body_fn, .. } => {
            if (body_fn.0 as usize) >= n_funcs {
                return Err(MoraError::Parse(format!(
                    "fn {:?}: no_grad body fn {:?} OOB",
                    func.id, body_fn
                )));
            }
        }
        ControlInst::PushDefer { fn_id, .. } => {
            if (fn_id.0 as usize) >= n_funcs {
                return Err(MoraError::Parse(format!(
                    "fn {:?}: defer fn {:?} OOB",
                    func.id, fn_id
                )));
            }
        }
        ControlInst::RegisterStruct { fields, .. } => {
            for f in fields {
                if let Some(df) = f.default_fn
                    && (df.0 as usize) >= n_funcs
                {
                    return Err(MoraError::Parse(format!(
                        "fn {:?}: struct field default fn {:?} OOB",
                        func.id, df
                    )));
                }
            }
        }
        ControlInst::RegisterTrait { methods, .. } => {
            for m in methods {
                if let Some(df) = m.default_fn
                    && (df.0 as usize) >= n_funcs
                {
                    return Err(MoraError::Parse(format!(
                        "fn {:?}: trait default fn {:?} OOB",
                        func.id, df
                    )));
                }
            }
        }
        ControlInst::RegisterImpl { methods, .. } => {
            for m in methods {
                if (m.fn_id.0 as usize) >= n_funcs {
                    return Err(MoraError::Parse(format!(
                        "fn {:?}: impl method fn {:?} OOB",
                        func.id, m.fn_id
                    )));
                }
            }
        }
        ControlInst::Select { arms, .. } => {
            for arm in arms {
                let body_fn = match arm {
                    ControlSelectArm::Recv { body_fn, .. }
                    | ControlSelectArm::Send { body_fn, .. }
                    | ControlSelectArm::Timeout { body_fn, .. }
                    | ControlSelectArm::Default { body_fn, .. } => *body_fn,
                };
                if (body_fn.0 as usize) >= n_funcs {
                    return Err(MoraError::Parse(format!(
                        "fn {:?}: select arm fn {:?} OOB",
                        func.id, body_fn
                    )));
                }
            }
        }
        _ => {}
    }
    Ok(())
}
