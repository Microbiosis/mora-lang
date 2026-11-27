//! v0.110.0: 运行时核心值表示（`Value`）、词法环境（`Environment`）、一等张量（`tensor`）、
//! 分块持久化列表（`list`）与外部宿主对象契约（`HostObject`）。

use crate::common::sync::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

// P6-1: Perceus / FBIP `Arc`-backed copy-on-write dictionary —— `Value::Dict` 的实际表示。
pub mod dict;
// 不可变分块列表（块用 `Arc` 共享）—— `Value::List` 的实际表示。
pub mod list;
/// Wave 3 — Stage 3B: 一等连续对齐内存 `Tensor<DType, Shape, Device>` 与零拷贝步幅视图。
pub mod tensor;

// v1 Stmt 已移除 — Value::Task/Closure 不再持有 body

// ─── Value ───────────────────────────────────────────────

/// W2.5: 外部宿主对象契约 —— 为 `Value::External` 提供类型名、显示、动态方法分派、
/// 可选 JSON 投影与类型安全 downcast。
pub trait HostObject: Send + Sync + std::fmt::Debug {
    /// Returns the Mora runtime type name exposed by `type_of(val)`.
    fn type_name(&self) -> &'static str;

    /// Formats this host object for `Display`, `str(val)`, and `print(val)`.
    fn display(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<external:{}>", self.type_name())
    }

    /// Lists the method names advertised by `Value::methods()`.
    fn methods(&self) -> &[&'static str] {
        &[]
    }

    /// Dispatches a dynamic method call from the VM with mutable runtime access.
    fn call_method(
        &self,
        method: &str,
        args: &[Value],
        core: &mut crate::runtime::CoreRuntime,
    ) -> crate::error::MoraResult<Value>;

    /// Projects this host object into a JSON-compatible `Value` for `json.stringify`.
    /// Returning `None` serializes as `"\"<external:<type_name>>\""`.
    fn to_json(&self) -> Option<Value> {
        None
    }

    /// Value-level equality hook (`Arc::ptr_eq` short-circuits before calling this).
    fn dyn_eq(&self, _other: &dyn HostObject) -> bool {
        false
    }

    /// Downcast hook enabling host extraction and concrete equality in `dyn_eq`.
    fn as_any(&self) -> &dyn std::any::Any;
}

/// v0.37 (P1-3.6): Typed enum replacing stringly-typed builtin dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinKind {
    Print,
    Range,
    Len,
    Json,
    File,
    // v0.43.0: exec.* — parallel subprocess execution
    Exec,
    // v0.86: eval(code) — runtime eval, Mora 源码动态执行
    Eval,
    // v0.91: math.* — 标量数学
    Math,
    // v0.91: stats.* — 统计
    Stats,
    // v0.91: linalg.* — 线性代数
    Linalg,
    // v0.91: random.* — PRNG
    Random,
    // v0.26: tail(path, max) — 文件尾部行读取
    Tail,
    // Wave 2 (W2.2): Remaining first-class free built-in functions (int, str, cons, apply, ...).
    FreeFn(&'static str),
}

/// 全局**模块对象**（`name.method(...)` 形式）的单一事实源。
pub const MODULE_OBJECTS: &[(&str, BuiltinKind)] = &[
    ("json", BuiltinKind::Json),
    ("file", BuiltinKind::File),
    ("exec", BuiltinKind::Exec),
    ("math", BuiltinKind::Math),
    ("stats", BuiltinKind::Stats),
    ("linalg", BuiltinKind::Linalg),
    ("random", BuiltinKind::Random),
];

impl std::fmt::Display for BuiltinKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            BuiltinKind::Print => "print",
            BuiltinKind::Range => "range",
            BuiltinKind::Len => "len",
            BuiltinKind::Json => "json",
            BuiltinKind::File => "file",
            BuiltinKind::Exec => "Exec::new",
            BuiltinKind::Eval => "eval",
            BuiltinKind::Math => "math",
            BuiltinKind::Stats => "stats",
            BuiltinKind::Linalg => "linalg",
            BuiltinKind::Random => "random",
            BuiltinKind::Tail => "tail",
            BuiltinKind::FreeFn(name) => name,
        };
        f.write_str(s)
    }
}

impl BuiltinKind {
    /// v0.75.52: 静态查找表（P6）— 调用名 → BuiltinKind 的单一来源。
    pub fn from_name(name: &str) -> Option<BuiltinKind> {
        match name {
            "print" => return Some(BuiltinKind::Print),
            "range" => return Some(BuiltinKind::Range),
            "len" => return Some(BuiltinKind::Len),
            "eval" => return Some(BuiltinKind::Eval),
            "tail" => return Some(BuiltinKind::Tail),
            _ => {}
        }
        let prefix = name.split('.').next().unwrap_or(name);
        if let Some((_, kind)) = MODULE_OBJECTS.iter().find(|(n, _)| *n == prefix) {
            return Some(*kind);
        }
        None
    }
}

/// Wave 3 Stage 3C: Runtime descriptor for one field of a nominal `struct`.
#[derive(Debug, Clone)]
pub struct StructFieldRuntime {
    pub name: String,
    pub type_name: String,
    pub default_fn: Option<crate::mir::control::FnId>,
    pub tags: Vec<(String, String)>,
}

/// Wave 3 Stage 3C: Runtime metadata and default-initializer program handle for a `struct`.
#[derive(Debug, Clone)]
pub struct StructDefRuntime {
    pub name: String,
    pub type_params: Vec<String>,
    pub const_params: Vec<String>,
    pub const_shape_bindings: Vec<(String, usize, usize)>,
    pub fields: Vec<StructFieldRuntime>,
    pub program: std::sync::Arc<crate::mir::control::ControlProgram>,
    pub globals: crate::runtime::vm::ModuleGlobals,
}

impl StructDefRuntime {
    /// Build a structured Mora `Value::Dict` schema describing this struct and its compile-time `@tag` metadata.
    pub fn to_schema_value(&self) -> Value {
        let field_vals: Vec<Value> = self
            .fields
            .iter()
            .map(|f| {
                let mut fdict = HashMap::new();
                fdict.insert("name".to_string(), Value::String(f.name.clone()));
                fdict.insert("type".to_string(), Value::String(f.type_name.clone()));
                fdict.insert(
                    "has_default".to_string(),
                    Value::Bool(f.default_fn.is_some()),
                );
                let mut tag_map = HashMap::new();
                for (k, v) in &f.tags {
                    tag_map.insert(k.clone(), Value::String(v.clone()));
                }
                fdict.insert("tags".to_string(), Value::Dict(tag_map.into()));
                Value::Dict(fdict.into())
            })
            .collect();
        let mut out = HashMap::new();
        out.insert("name".to_string(), Value::String(self.name.clone()));
        out.insert(
            "type_params".to_string(),
            Value::List(
                self.type_params
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
        out.insert("fields".to_string(), Value::List(field_vals.into()));
        Value::Dict(out.into())
    }

    /// Build a Mora `Value::Dict` mapping `field_name -> { tag_key: tag_val, ... }`.
    pub fn to_tags_value(&self) -> Value {
        let mut out = HashMap::new();
        for f in &self.fields {
            let mut tag_map = HashMap::new();
            for (k, v) in &f.tags {
                tag_map.insert(k.clone(), Value::String(v.clone()));
            }
            out.insert(f.name.clone(), Value::Dict(tag_map.into()));
        }
        Value::Dict(out.into())
    }
}

/// Wave 3 Stage 3C + P4-1A: Runtime metadata for a nominal `enum` definition.
#[derive(Debug, Clone)]
pub struct EnumDefRuntime {
    pub name: String,
    pub type_params: Vec<String>,
    /// P4-1A: Subset of `type_params` that are compile-time integer const generics.
    pub const_params: Vec<String>,
    /// P4-1A: `(variant_name, const_name, payload_idx, tensor_axis_idx)` bindings enabling
    /// runtime const inference from tensor payload shapes when explicit `<...>` args are omitted.
    pub const_shape_bindings: Vec<(String, String, usize, usize)>,
    pub variants: Vec<(String, Vec<String>)>,
}

#[derive(Debug, Clone)]
pub enum Value {
    String(String),
    /// v0.x: 单字符（`string[number]` 索引结果）
    Char(char),
    // v0.38: Numeric tower — default 64-bit Int and Float variants.
    Int(i64),
    Float(f64),
    /// Wave 3 — Stage 3B: Non-default hardware-width scalar (`f32`, `f16`, `bf16`, `fp8`, `fp8_e5m2`, `i32`..`i4`, `u64`..`u8`).
    Scalar(crate::common::ScalarValue),
    Bool(bool),
    Nil,
    List(list::List),
    Dict(dict::Dict),
    Task {
        name: String,
        params: Vec<String>,
        program: std::sync::Arc<crate::mir::control::ControlProgram>,
        fn_id: crate::mir::control::FnId,
        globals: crate::runtime::vm::ModuleGlobals,
    },
    Closure {
        params: Vec<String>,
        program: std::sync::Arc<crate::mir::control::ControlProgram>,
        fn_id: crate::mir::control::FnId,
        globals: crate::runtime::vm::ModuleGlobals,
        upvalues: std::sync::Arc<Vec<crate::runtime::vm::UpvalueValue>>,
    },
    Builtin(BuiltinKind),
    TraitObject {
        for_generics: Vec<String>,
        trait_generics: Vec<String>,
        for_type: String,
        trait_name: String,
        data: Box<Value>,
    },
    // v0.17: Compose 组合函数
    Compose(Vec<Value>),
    // v0.18: Partial 部分应用
    Partial(Box<Value>, Vec<Value>),
    // v0.19: Atom 可变引用 (Clojure 启发)
    Atom(Arc<Mutex<Value>>),
    // v0.20 / v0.83: 宏定义
    Macro {
        name: String,
        params: Vec<String>,
        program: std::sync::Arc<crate::mir::control::ControlProgram>,
        fn_id: crate::mir::control::FnId,
        globals: crate::runtime::vm::ModuleGlobals,
    },
    // v0.86: Curry — 函数柯里化
    Curry {
        func: Box<Value>,
        arity: usize,
        bound_args: Vec<Value>,
    },
    // v0.86: Cons — Lisp 链式列表原语
    Cons {
        car: Box<Value>,
        cdr: Box<Value>,
    },
    /// v0.86: Lisp homoiconicity — `quote(expr)` captures source text.
    Code(String),
    /// W2.3: 一等定界续延（multi-shot handler resume）
    Continuation(std::sync::Arc<crate::runtime::vm::CapturedContinuation>),
    /// W2.5: 外部宿主不透明对象（通过 `HostObject` trait 暴露方法、显示与 JSON 投影）
    External(Arc<dyn HostObject>),
    /// Wave 2: Typed concurrent message channel (`channel([cap])`).
    Channel(Arc<crate::runtime::scheduler::MoraChannel>),
    /// Wave 2: Asynchronous task join handle (`spawn expr`).
    TaskHandle(Arc<crate::runtime::scheduler::MoraTaskHandle>),
    /// Wave 2: Cooperative cancellation & deadline context (`context()`).
    Context(crate::runtime::scheduler::TaskContext),
    /// Wave 2: Non-blocking async TCP listener (`tcp_listen(addr)`).
    TcpListener(Arc<crate::runtime::reactor::MoraTcpListener>),
    /// Wave 2: Non-blocking async TCP stream (`tcp_connect(addr)` / `listener.accept()`).
    TcpStream(Arc<crate::runtime::reactor::MoraTcpStream>),
    /// v0.110.1: Native async TLS 1.2/1.3 listener (`tls_listen(addr, config)`).
    TlsListener(Arc<crate::runtime::tls::MoraTlsListener>),
    /// v0.110.1: Native async TLS 1.2/1.3 stream (`tls_connect(...)` / `tls_listener.accept()`).
    TlsStream(Arc<crate::runtime::tls::MoraTlsStream>),
    /// Wave 3 — Stage 3B: First-class strided N-dimensional tensor (`tensor(...)` / `Tensor::*`).
    Tensor(Arc<tensor::Tensor>),
    /// Wave 4: Native fused tensor compute graph (`trace_dag(...)`).
    ComputeDag(Arc<crate::runtime::compute_dag::TensorComputeDag>),
    /// Wave 4: Paged KV-cache dynamic resource allocator (`kv_allocator(...)`).
    KVAllocator(Arc<crate::runtime::kv_cache::KVPageAllocator>),
    /// Wave 4 P1: General 64-byte aligned size-classed tensor memory pool (`memory_pool(...)`).
    MemoryPool(Arc<crate::runtime::memory_pool::MemoryPool>),
    /// P4-1B: First-class compiled source-level GPU compute kernel (`kernel name(...) ... end`).
    GpuKernel(Arc<crate::runtime::gpu::kernel_compiler::CompiledGpuKernel>),
    /// P4-4: Distributed tensor collective communicator (`process_group(...)` / `tcp_process_group(...)`).
    ProcessGroup(Arc<crate::runtime::distributed::ProcessGroup>),
    /// P4-4: Compute & service nanosecond trace profiler (`profiler()` / `std/trace`).
    TraceProfiler(Arc<crate::runtime::profiler::TraceProfilerHandle>),
    /// Wave 3 — Stage 3C: First-class nominal struct type constructor & metadata object.
    StructType(Arc<StructDefRuntime>),
    /// Wave 3 — Stage 3C: First-class nominal struct instance preserving field order, const args & `@tag` schema.
    /// P6-1: `fields` uses `Arc`-backed [`dict::Dict`], `const_args` uses `Arc<[(String, i64)]>`,
    /// and type/field metadata lives on `def: Arc<StructDefRuntime>`, so cloning a struct instance
    /// is pure $O(1)$ `Arc::clone` and uniquely-owned struct projections extract fields in place.
    StructInstance {
        fields: dict::Dict,
        const_args: Arc<[(String, i64)]>,
        def: Arc<StructDefRuntime>,
    },
    /// Wave 3 — Stage 3C: First-class nominal enum type descriptor.
    EnumType(Arc<EnumDefRuntime>),
    /// Wave 3 — Stage 3C + P4-1A: First-class nominal enum variant value with optional payload and const args.
    EnumVariant {
        enum_name: String,
        variant: String,
        payload: Vec<Value>,
        const_args: Vec<(String, i64)>,
    },
    /// Wave 3 — Stage 3C: Differentiated callable produced by `grad(fn)` or `value_and_grad(fn)`.
    GradFn {
        func: Box<Value>,
        argnums: Vec<usize>,
        return_value: bool,
    },
    /// Wave 3 — Stage 3C: Internal sentinel instructing callee prologue to evaluate a default param.
    MissingArg,
}

// 手动实现 PartialEq（Atom/Task/Closure/Macro/Continuation 等不自动比较内部载荷）
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::MissingArg, Value::MissingArg) => true,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Scalar(a), Value::Scalar(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Char(a), Value::Char(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Dict(a), Value::Dict(b)) => a == b,
            (Value::Builtin(a), Value::Builtin(b)) => a == b,
            (
                Value::Curry {
                    func: a,
                    arity: aa,
                    bound_args: ba,
                },
                Value::Curry {
                    func: b,
                    arity: ab,
                    bound_args: bb,
                },
            ) => a == b && aa == ab && ba == bb,
            (Value::Cons { car: a1, cdr: a2 }, Value::Cons { car: b1, cdr: b2 }) => {
                a1 == b1 && a2 == b2
            }
            (Value::Code(a), Value::Code(b)) => a == b,
            (Value::External(a), Value::External(b)) => Arc::ptr_eq(a, b) || a.dyn_eq(b.as_ref()),
            (Value::Channel(a), Value::Channel(b)) => Arc::ptr_eq(a, b),
            (Value::TaskHandle(a), Value::TaskHandle(b)) => Arc::ptr_eq(a, b),
            (Value::Context(a), Value::Context(b)) => a.same_handle(b),
            (Value::TcpListener(a), Value::TcpListener(b)) => Arc::ptr_eq(a, b),
            (Value::TcpStream(a), Value::TcpStream(b)) => Arc::ptr_eq(a, b),
            (Value::TlsListener(a), Value::TlsListener(b)) => Arc::ptr_eq(a, b),
            (Value::TlsStream(a), Value::TlsStream(b)) => Arc::ptr_eq(a, b),
            (Value::ComputeDag(a), Value::ComputeDag(b)) => Arc::ptr_eq(a, b) || a == b,
            (Value::KVAllocator(a), Value::KVAllocator(b)) => Arc::ptr_eq(a, b),
            (Value::MemoryPool(a), Value::MemoryPool(b)) => Arc::ptr_eq(a, b),
            (Value::GpuKernel(a), Value::GpuKernel(b)) => Arc::ptr_eq(a, b) || a.hash == b.hash,
            (Value::ProcessGroup(a), Value::ProcessGroup(b)) => Arc::ptr_eq(a, b),
            (Value::TraceProfiler(a), Value::TraceProfiler(b)) => Arc::ptr_eq(a, b),
            (Value::Tensor(a), Value::Tensor(b)) => Arc::ptr_eq(a, b) || a == b,
            (Value::StructType(a), Value::StructType(b)) => Arc::ptr_eq(a, b) || a.name == b.name,
            (
                Value::StructInstance {
                    fields: fa,
                    const_args: ca,
                    def: da,
                },
                Value::StructInstance {
                    fields: fb,
                    const_args: cb,
                    def: db,
                },
            ) => (Arc::ptr_eq(da, db) || da.name == db.name) && ca == cb && fa == fb,
            (Value::EnumType(a), Value::EnumType(b)) => Arc::ptr_eq(a, b) || a.name == b.name,
            (
                Value::EnumVariant {
                    enum_name: ea,
                    variant: va,
                    payload: pa,
                    const_args: ca,
                },
                Value::EnumVariant {
                    enum_name: eb,
                    variant: vb,
                    payload: pb,
                    const_args: cb,
                },
            ) => ea == eb && va == vb && ca == cb && pa == pb,
            _ => false,
        }
    }
}

mod display; // Value Display + fmt_inner（纯格式化，无 BuiltinKind/Environment 依赖）

// ─── Merge Strategy ─────────────────────────────────────────
/// v0.59: CRDT-inspired merge strategies for concurrent state.
///
/// When two environments (or state channels) write to the same key,
/// the merge strategy determines how the values are combined.
#[derive(Debug, Clone, PartialEq)]
pub enum MergeStrategy {
    /// Child overwrites parent (classic last-write-wins).
    LastWriteWins,
    /// List: concatenate. String: concatenate. Other: LWW.
    Append,
    /// Int/Float: numeric addition. Other: LWW.
    Add,
    /// Dict: key-level merge (child keys win on conflict).
    DictUnion,
    /// v0.75.5: G-Set（grow-only set）— List: 并集（只加新元素）；
    /// Dict: key 级并集（child 的 key 仅在 parent 缺失时插入）；其他 LWW。
    GrowOnlySet,
}

impl MergeStrategy {
    /// v0.75.24: 策略名 → 枚举的单一事实来源（替代运行时/typeck 各自的
    /// 硬编码字符串 match）。`merge_with(key, strategy)` 的运行时解析与
    /// typeck 字面量校验都走这里。
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "lww" | "last_write_wins" => Some(Self::LastWriteWins),
            "append" => Some(Self::Append),
            "add" => Some(Self::Add),
            "dict_union" => Some(Self::DictUnion),
            "grow_only_set" => Some(Self::GrowOnlySet),
            _ => None,
        }
    }
}

impl Value {
    /// 返回值可用的方法名列表（`methods_of(value)` builtin 用）。
    /// 内禀属性贴近数据定义（Lua 5.4 元表同源思想），dispatch 不再跨模块
    /// 查方法表。
    pub fn methods(&self) -> Vec<String> {
        let names: &[&str] = match self {
            Value::String(_) => &[
                "len",
                "upper",
                "lower",
                "trim",
                "starts_with",
                "ends_with",
                "contains",
                "split",
                "replace",
                "json",
            ],
            Value::List(_) => &[
                "push",
                "pop",
                "get",
                "len",
                "map",
                "filter",
                "reduce",
                "take",
                "drop",
                "window",
                "batch",
                "shape",
                "flatten",
                "transpose",
                "reshape",
                // v0.91: 列表统计方法（复用 stats.* 函数）
                "sum",
                "min",
                "max",
                "mean",
                "median",
                "stddev",
                "var",
                "sort",
            ],
            Value::Dict(_) => &["get", "set", "keys", "values", "len", "json"],
            Value::Int(_) => &[
                // v0.91: 整数数学方法
                "abs", "sign", "floor", "ceil", "round", "sqrt", "sin", "cos", "tan", "exp", "log",
                "log2", "log10", "to_float",
            ],
            Value::Float(_) => &[
                // v0.91: 浮点数学方法
                "abs",
                "sign",
                "floor",
                "ceil",
                "round",
                "trunc",
                "fract",
                "sqrt",
                "cbrt",
                "sin",
                "cos",
                "tan",
                "asin",
                "acos",
                "atan",
                "sinh",
                "cosh",
                "tanh",
                "exp",
                "log",
                "log2",
                "log10",
                "log1p",
                "is_nan",
                "is_inf",
                "is_finite",
                "to_int",
            ],
            Value::Builtin(kind) => {
                // `MODULE_OBJECTS` 是「注册名 ↔ BuiltinKind」的单一事实源，
                // 反向查回注册名（`Toolplane` 的注册名是 `tool`，D74 已记过
                // 「按枚举变体名建表会整段匹配不到」这个坑）。
                let name = MODULE_OBJECTS
                    .iter()
                    .find(|(_, k)| std::mem::discriminant(k) == std::mem::discriminant(kind))
                    .map(|(n, _)| *n);
                return match name {
                    Some(n) => crate::typeck::dispatch::module_method_names(n)
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                    // 裸函数（print/len/range）与未登记的 BuiltinKind：
                    // 无可枚举的签名表 → 空集是**诚实**的
                    None => Vec::new(),
                };
            }
            Value::External(obj) => {
                return obj.methods().iter().map(|s| (*s).to_string()).collect();
            }
            Value::Channel(_) => &[
                "send",
                "try_send",
                "send_ctx",
                "recv",
                "try_recv",
                "recv_ctx",
                "close",
                "is_closed",
                "len",
                "capacity",
            ],
            Value::TaskHandle(_) => &["join", "abort", "is_done", "context"],
            Value::Context(_) => &["child", "with_timeout", "cancel", "is_cancelled", "spawn"],
            Value::TcpListener(_) => &["accept", "local_addr", "close", "is_closed"],
            Value::TlsListener(_) => &["accept", "local_addr", "close", "is_closed"],
            Value::TlsStream(_) => &[
                "read",
                "read_bytes",
                "read_line",
                "write",
                "write_bytes",
                "shutdown",
                "close",
                "is_closed",
                "local_addr",
                "peer_addr",
                "tls_version",
                "cipher_suite",
                "alpn_protocol",
            ],
            Value::TcpStream(_) => &[
                "read",
                "read_bytes",
                "read_line",
                "write",
                "write_bytes",
                "shutdown",
                "close",
                "is_closed",
                "local_addr",
                "peer_addr",
            ],
            Value::ComputeDag(_) => &[
                "run",
                "run_multi",
                "run_profiled",
                "fuse",
                "compile",
                "autodiff",
                "autodiff_checkpointed",
                "checkpoint",
                "vjp",
                "grad",
                "jvp",
                "node_count",
                "fused_count",
                "output_count",
                "op_names",
                "to_dot",
                "is_fused",
                "memory_plan",
                "cost_report",
                "cache_stats",
                "cache_key",
                "backend_info",
            ],
            Value::KVAllocator(_) => &[
                "alloc_seq",
                "set_priority",
                "seq_priority",
                "set_quota",
                "quota_bytes",
                "on_watermark",
                "clear_watermarks",
                "append_tokens",
                "fork_seq",
                "write_kv",
                "read_kv",
                "free_seq",
                "swap_out",
                "swap_in",
                "is_swapped",
                "cache_prefix",
                "match_prefix",
                "attach_prefix",
                "evict_lru",
                "evict_preempt",
                "reclaim_for",
                "seq_pages",
                "seq_tokens",
                "free_pages",
                "used_pages",
                "total_pages",
                "utilization",
                "bytes_used",
                "stats",
            ],
            Value::MemoryPool(_) => &[
                "alloc",
                "zeros",
                "full",
                "release",
                "trim",
                "set_quota",
                "on_watermark",
                "clear_watermarks",
                "active_bytes",
                "cached_bytes",
                "max_bytes",
                "utilization",
                "stats",
            ],
            Value::GpuKernel(_) => &[
                "launch",
                "emit_ptx",
                "emit_spirv_words",
                "emit_msl",
                "name",
                "params",
            ],
            Value::ProcessGroup(_) => &[
                "rank",
                "world_size",
                "backend",
                "ranks",
                "all_reduce",
                "all_reduce_async",
                "all_gather",
                "broadcast",
                "reduce_scatter",
                "send",
                "recv",
                "split",
                "barrier",
                "stats",
            ],
            Value::TraceProfiler(_) => &[
                "start",
                "stop",
                "clear",
                "is_active",
                "now_ns",
                "record",
                "events",
                "summary",
                "to_chrome_json",
            ],
            Value::Tensor(_) => &[
                "shape",
                "ndim",
                "rank",
                "numel",
                "nbytes",
                "byte_len",
                "len",
                "dtype",
                "device",
                "is_contiguous",
                "is_vram_resident",
                "contiguous",
                "reshape",
                "transpose",
                "to",
                "to_dtype",
                "to_device",
                "requires_grad",
                "with_grad",
                "fill_",
                "add_",
                "mul_",
                "sum",
                "mean",
                "max",
                "min",
                "item",
                "abs",
                "sqrt",
                "exp",
                "ln",
                "log",
                "sin",
                "cos",
                "relu",
                "gelu",
                "silu",
                "tanh",
                "sigmoid",
                "softmax",
                "log_softmax",
                "rms_norm",
                "layer_norm",
                "to_list",
            ],
            Value::StructType(_) => &["schema", "tags", "fields"],
            Value::StructInstance { .. } => &[
                "to_dict",
                "schema",
                "tags",
                "json",
                "parameters",
                "named_parameters",
                "state_dict",
                "load_state_dict",
                "to_device",
            ],
            Value::EnumType(_) => &["variants"],
            _ => &[],
        };
        names.iter().map(|s| s.to_string()).collect()
    }

    /// Merge two values using the given strategy.
    /// Falls back to `LastWriteWins` if the strategy doesn't apply
    /// to the value types.
    pub fn merge(parent: Value, child: Value, strategy: &MergeStrategy) -> Value {
        match strategy {
            MergeStrategy::LastWriteWins => child,
            MergeStrategy::Append => match (parent, child) {
                // v0.104.6：List 不可变，拼接走「取回 Vec → extend → 装回」
                (Value::List(a), Value::List(b)) => {
                    let mut merged = a.to_vec();
                    merged.extend(b.iter().cloned());
                    Value::List(merged.into())
                }
                (Value::String(a), Value::String(b)) => Value::String(a + &b),
                (_, child) => child, // fallback: LWW
            },
            MergeStrategy::Add => match (parent, child) {
                (Value::Int(a), Value::Int(b)) => Value::Int(a + b),
                (Value::Float(a), Value::Float(b)) => Value::Float(a + b),
                (_, child) => child,
            },
            MergeStrategy::DictUnion => match (parent, child) {
                (Value::Dict(mut a), Value::Dict(b)) => {
                    let map = a.make_mut();
                    for (k, v) in b {
                        map.insert(k, v);
                    }
                    Value::Dict(a)
                }
                (_, child) => child,
            },
            MergeStrategy::GrowOnlySet => match (parent, child) {
                // v0.104.6：同上，List 不可变
                (Value::List(a), Value::List(b)) => {
                    let mut acc = a.to_vec();
                    for item in b.iter() {
                        if !acc.contains(item) {
                            acc.push(item.clone());
                        }
                    }
                    Value::List(acc.into())
                }
                (Value::Dict(mut a), Value::Dict(b)) => {
                    let map = a.make_mut();
                    for (k, v) in b {
                        map.entry(k).or_insert(v);
                    }
                    Value::Dict(a)
                }
                (_, child) => child, // fallback: LWW
            },
        }
    }
}

// ─── Environment ─────────────────────────────────────────
/// 词法环境 —— 扁平作用域哈希映射 + `Arc<Environment>` 不可变父链。
///
/// - 绑定值是纯 `Value`，无每绑定 `Mutex` —— 捕获的闭包无法再被外部改写。
/// - `parent` 是 `Arc<Environment>`（写时复制），父链共享无需加锁。
#[derive(Debug, Clone)]
pub struct Environment {
    pub values: HashMap<String, Value>,
    pub parent: Option<Arc<Environment>>,
    /// 本层对外公开的绑定名（模块导出面）。
    ///
    /// 语义（spec §10.2）：`export` 标记的符号对 import 者可见，未标记的
    /// 是模块私有。两条入口写同一存储：
    /// 1. [`Environment::define`] 的 `exported` 参数；
    /// 2. `ControlInst::ExportGlobal` / `ControlInst::ExportDyn` 指令
    ///    （`export <decl>` 的运行时落点，见 `mir::control::ir` 与 `runtime::vm::engine`）。
    pub exports: std::collections::HashSet<String>,
    /// Per-key CRDT merge strategies configured via `merge_with(key, strategy)` for parallel worker merges.
    pub(crate) merge_strategies: Option<HashMap<String, MergeStrategy>>,
}

impl Default for Environment {
    fn default() -> Self {
        Self::new()
    }
}

impl Environment {
    pub fn new() -> Self {
        Self {
            values: HashMap::new(),
            parent: None,
            exports: std::collections::HashSet::new(),
            merge_strategies: None,
        }
    }

    pub fn with_parent_of(parent: Arc<Environment>) -> Self {
        Self {
            values: HashMap::new(),
            parent: Some(parent),
            exports: std::collections::HashSet::new(),
            merge_strategies: None,
        }
    }

    /// Record a per-key parallel worker merge strategy in this lexical environment.
    pub(crate) fn set_merge_strategy(&mut self, key: String, strategy: MergeStrategy) {
        self.merge_strategies
            .get_or_insert_with(HashMap::new)
            .insert(key, strategy);
    }

    /// Read configured per-key parallel worker merge strategies, if any.
    pub(crate) fn merge_strategies(&self) -> Option<&HashMap<String, MergeStrategy>> {
        self.merge_strategies.as_ref()
    }

    /// 捕获当前作用域与共享父链的独立快照。
    pub fn snapshot(&self) -> Environment {
        self.clone()
    }

    /// 纯函数式定义：返回带新绑定的新环境，`self` 保持不变。
    pub fn assoc(&self, name: &str, value: Value) -> Environment {
        let mut next = self.clone();
        next.values.insert(name.to_string(), value);
        next
    }

    /// 定义绑定。`exported=true` 同时把名字记入本层的导出集（模块对外接口）。
    pub fn define(&mut self, name: String, value: Value, exported: bool) {
        if exported {
            self.exports.insert(name.clone());
        }
        self.values.insert(name, value);
    }

    /// 把名字标记为对外公开（`ControlInst::ExportGlobal` / `ControlInst::ExportDyn` 的落点）。
    pub fn mark_exported(&mut self, name: &str) {
        self.exports.insert(name.to_string());
    }

    /// 名字是否在本层导出（不看父链 —— 导出面属模块自身）。
    pub fn is_exported(&self, name: &str) -> bool {
        self.exports.contains(name)
    }

    /// 本层导出名（排序后稳定返回，供 import 合并与测试断言）。
    pub fn exported_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.exports.iter().cloned().collect();
        v.sort();
        v
    }

    pub fn get(&self, name: &str) -> Option<Value> {
        if let Some(v) = self.values.get(name) {
            Some(v.clone())
        } else if let Some(parent) = &self.parent {
            parent.get(name)
        } else {
            None
        }
    }

    pub fn assign(&mut self, name: &str, value: Value) -> bool {
        if self.values.contains_key(name) {
            self.values.insert(name.to_string(), value);
            true
        } else if let Some(parent) = &mut self.parent {
            // 父链是 Arc<Environment>：写入触发写时复制，共享的父版本不被就地改写。
            Arc::make_mut(parent).assign(name, value)
        } else {
            false
        }
    }

    /// 迭代环境中的所有绑定（仅当前层，不含 parent），供 import/子 env 合并用。
    pub fn iter(&self) -> Vec<(String, Value)> {
        self.values
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Merge bindings from a child environment into this one using `strategy`.
    pub fn merge_from(&mut self, child: &Environment, strategy: &MergeStrategy) {
        for (name, child_val) in &child.values {
            match self.values.get(name).cloned() {
                Some(parent_val) => {
                    let merged = Value::merge(parent_val, child_val.clone(), strategy);
                    self.values.insert(name.clone(), merged);
                }
                None => {
                    self.define(name.clone(), child_val.clone(), false);
                }
            }
        }
    }

    /// Merge bindings with per-key strategies, falling back to `default`.
    pub fn merge_from_with_strategies(
        &mut self,
        child: &Environment,
        strategies: &HashMap<String, MergeStrategy>,
        default: &MergeStrategy,
    ) {
        for (name, child_val) in &child.values {
            let strategy = strategies.get(name).unwrap_or(default);
            match self.values.get(name).cloned() {
                Some(parent_val) => {
                    let merged = Value::merge(parent_val, child_val.clone(), strategy);
                    self.values.insert(name.clone(), merged);
                }
                None => {
                    self.define(name.clone(), child_val.clone(), false);
                }
            }
        }
    }
}

// ─── Binding (v0.21: 所有权语义) ───────────────────────
/// 变量绑定状态，支持移动语义
#[derive(Debug, Clone)]
pub enum Binding {
    /// 正常值
    Value(Value),
    /// 已移动（所有权转移）
    Moved,
    /// 不可变借用
    Borrowed(Arc<Mutex<Value>>),
    /// 可变借用
    BorrowedMut(Arc<Mutex<Value>>),
}

impl Binding {
    pub fn is_moved(&self) -> bool {
        matches!(self, Binding::Moved)
    }

    pub fn is_borrowed(&self) -> bool {
        matches!(self, Binding::Borrowed(_) | Binding::BorrowedMut(_))
    }

    pub fn is_borrowed_mut(&self) -> bool {
        matches!(self, Binding::BorrowedMut(_))
    }

    pub fn get_value(&self) -> Option<&Value> {
        match self {
            Binding::Value(v) => Some(v),
            _ => None,
        }
    }

    pub fn into_value(self) -> crate::error::MoraResult<Value> {
        match self {
            Binding::Value(v) => Ok(v),
            Binding::Moved => Err(crate::error::MoraError::Runtime(
                "use of moved value".to_string(),
            )),
            Binding::Borrowed(_) | Binding::BorrowedMut(_) => Err(
                crate::error::MoraError::Runtime("cannot move out of borrowed value".to_string()),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v0.35 (P0-B2): Atom Display must not panic (smoke test).
    #[test]
    fn atom_display_does_not_panic_on_valid_value() {
        let v = Value::Atom(Arc::new(Mutex::new(Value::Float(42.0))));
        let s = format!("{}", v);
        assert!(s.contains("atom"), "got: {}", s);
        assert!(s.contains("42"), "got: {}", s);
    }

    /// v0.36 (P1-3.13): Number Display should render NaN/Inf without panicking.
    #[test]
    fn number_display_handles_nan() {
        let v = Value::Float(f64::NAN);
        let s = format!("{}", v);
        assert_eq!(s, "nan");
    }

    #[test]
    fn number_display_handles_pos_inf() {
        let v = Value::Float(f64::INFINITY);
        let s = format!("{}", v);
        assert_eq!(s, "inf");
    }

    #[test]
    fn number_display_handles_neg_inf() {
        let v = Value::Float(f64::NEG_INFINITY);
        let s = format!("{}", v);
        assert_eq!(s, "-inf");
    }

    #[test]
    fn number_display_normal_value() {
        let v = Value::Float(42.5);
        let s = format!("{}", v);
        assert_eq!(s, "42.5");
    }

    // ─── Merge tests ──────────────────────────────────────────

    #[test]
    fn merge_add_ints() {
        assert_eq!(
            Value::merge(Value::Int(5), Value::Int(3), &MergeStrategy::Add),
            Value::Int(8)
        );
    }

    #[test]
    fn merge_add_floats() {
        assert_eq!(
            Value::merge(Value::Float(1.5), Value::Float(2.5), &MergeStrategy::Add),
            Value::Float(4.0)
        );
    }

    #[test]
    fn merge_append_lists() {
        assert_eq!(
            Value::merge(
                Value::List(vec![Value::Int(1)].into()),
                Value::List(vec![Value::Int(2)].into()),
                &MergeStrategy::Append
            ),
            Value::List(vec![Value::Int(1), Value::Int(2)].into())
        );
    }

    #[test]
    fn merge_append_strings() {
        assert_eq!(
            Value::merge(
                Value::String("a".into()),
                Value::String("b".into()),
                &MergeStrategy::Append
            ),
            Value::String("ab".into())
        );
    }

    #[test]
    fn merge_dict_union() {
        let mut a = HashMap::new();
        a.insert("x".into(), Value::Int(1));
        let mut b = HashMap::new();
        b.insert("y".into(), Value::Int(2));
        let mut expected = HashMap::new();
        expected.insert("x".into(), Value::Int(1));
        expected.insert("y".into(), Value::Int(2));
        assert_eq!(
            Value::merge(
                Value::Dict(a.into()),
                Value::Dict(b.into()),
                &MergeStrategy::DictUnion
            ),
            Value::Dict(expected.into())
        );
    }

    #[test]
    fn merge_lww_is_child_wins() {
        assert_eq!(
            Value::merge(Value::Int(1), Value::Int(99), &MergeStrategy::LastWriteWins),
            Value::Int(99)
        );
    }

    #[test]
    fn merge_fallback_to_lww() {
        // String + Int with Add strategy — can't add, falls back to child
        assert_eq!(
            Value::merge(
                Value::String("x".into()),
                Value::Int(42),
                &MergeStrategy::Add
            ),
            Value::Int(42)
        );
    }

    // ─── v0.75.5: G-Set（grow-only set）───

    #[test]
    fn merge_grow_only_set_lists() {
        // 并集：parent ∪ child，只加新元素
        assert_eq!(
            Value::merge(
                Value::List(vec![Value::Int(1), Value::Int(2)].into()),
                Value::List(vec![Value::Int(2), Value::Int(3)].into()),
                &MergeStrategy::GrowOnlySet
            ),
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)].into())
        );
    }

    #[test]
    fn merge_grow_only_set_dicts() {
        // key 级并集：child 的 key 仅在 parent 缺失时插入，不覆盖已存在 key
        let parent = Value::Dict([("a".to_string(), Value::Int(1))].into_iter().collect());
        let child = Value::Dict(
            [
                ("a".to_string(), Value::Int(99)),
                ("b".to_string(), Value::Int(2)),
            ]
            .into_iter()
            .collect(),
        );
        let merged = Value::merge(parent, child, &MergeStrategy::GrowOnlySet);
        let map = match merged {
            Value::Dict(m) => m,
            _ => panic!("expected dict"),
        };
        assert_eq!(map.get("a"), Some(&Value::Int(1)), "已存在 key 不被覆盖");
        assert_eq!(map.get("b"), Some(&Value::Int(2)));
    }

    #[test]
    fn merge_grow_only_set_fallback_lww() {
        // 非 List/Dict 走 child（与 Append/Add 的 fallback 模式一致）
        assert_eq!(
            Value::merge(Value::Int(1), Value::Int(42), &MergeStrategy::GrowOnlySet),
            Value::Int(42)
        );
    }

    #[test]
    fn env_merge_with_grow_only_set_strategy() {
        let mut parent = Environment::new();
        parent.define(
            "tags".into(),
            Value::List(vec![Value::String("a".into())].into()),
            false,
        );
        let mut child = Environment::new();
        child.define(
            "tags".into(),
            Value::List(vec![Value::String("a".into()), Value::String("b".into())].into()),
            false,
        );
        let mut strategies = HashMap::new();
        strategies.insert("tags".to_string(), MergeStrategy::GrowOnlySet);
        parent.merge_from_with_strategies(&child, &strategies, &MergeStrategy::LastWriteWins);
        assert_eq!(
            parent.get("tags"),
            Some(Value::List(
                vec![Value::String("a".into()), Value::String("b".into())].into()
            ))
        );
    }

    #[test]
    fn env_merge_from_new_bindings() {
        let mut parent = Environment::new();
        parent.define("a".into(), Value::Int(1), false);
        let mut child = Environment::new();
        child.define("b".into(), Value::Int(2), false);
        parent.merge_from(&child, &MergeStrategy::LastWriteWins);
        assert_eq!(parent.get("a"), Some(Value::Int(1)));
        assert_eq!(parent.get("b"), Some(Value::Int(2)));
    }

    #[test]
    fn env_clone_is_isolated_not_aliased() {
        // 旧实现用 HashMap<String, Arc<Mutex<Value>>>：clone 共享绑定 cell，
        // 一处写会穿透另一处。持久化绑定后 clone 是独立版本。
        let mut base = Environment::new();
        base.define("x".into(), Value::Int(1), false);
        let mut copy = base.clone();
        copy.define("x".into(), Value::Int(2), false);
        assert_eq!(copy.get("x"), Some(Value::Int(2)));
        assert_eq!(base.get("x"), Some(Value::Int(1)), "clone 不得被别名写穿");
    }

    #[test]
    fn env_snapshot_isolated_from_further_writes() {
        let mut base = Environment::new();
        base.define("x".into(), Value::Int(1), false);
        let snap = base.snapshot();
        base.define("x".into(), Value::Int(9), false);
        base.define("y".into(), Value::Int(7), false);
        assert_eq!(snap.get("x"), Some(Value::Int(1)), "快照冻结在捕获时刻");
        assert_eq!(snap.get("y"), None, "后续定义不进入旧快照");
        assert_eq!(base.get("x"), Some(Value::Int(9)));
    }

    #[test]
    fn env_assoc_is_pure() {
        let base = Environment::new();
        let with_a = base.assoc("a", Value::Int(1));
        let with_b = with_a.assoc("b", Value::Int(2));
        assert_eq!(base.get("a"), None, "assoc 不改变原环境");
        assert_eq!(with_a.get("a"), Some(Value::Int(1)));
        assert_eq!(with_a.get("b"), None);
        assert_eq!(with_b.get("a"), Some(Value::Int(1)), "结构共享保留旧键");
        assert_eq!(with_b.get("b"), Some(Value::Int(2)));
    }

    #[test]
    fn env_parent_scope_assign_copies_on_write() {
        let mut p = Environment::new();
        p.define("a".into(), Value::Int(1), false);
        let parent = Arc::new(p);
        let mut child = Environment::with_parent_of(Arc::clone(&parent));
        assert!(child.assign("a", Value::Int(99)), "写入父作用域已有绑定");
        assert_eq!(child.get("a"), Some(Value::Int(99)));
        assert_eq!(
            parent.get("a"),
            Some(Value::Int(1)),
            "父链是不可变的：写时复制，共享的父版本不被就地改写"
        );
    }

    #[test]
    fn environment_is_send_sync() {
        // 纯函数式数据（无内部可变性）天然满足 Send + Sync —— 并发不再依赖锁。
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Environment>();
    }

    #[test]
    fn value_is_send_sync() {
        // v0.104.6 D484：`Value` 的若干变体带 **IR 层载荷**
        // （`Arc<ControlProgram>` / `Arc<Mutex<…>>` / 通道 / 任务句柄 …）
        // ⇒ `Value` 必须仍是 `Send + Sync`（运行时 VM 跨 task 共享 Value；
        // `Value::Closure` 的载荷用 `Arc` 而非 `Rc`，正是为了保住
        // `Value: Send + Sync`）。
        //
        // 此前这条**只写在注释里、没有任何断言** ⇒ 新增变体若引入不可 Send 的
        // 载荷（如 `Rc`），要等到跨线程用它的测试**炸在运行期**才暴露。
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Value>();
    }

    #[test]
    fn env_shared_across_threads_without_locks() {
        let mut base = Environment::new();
        base.define("answer".into(), Value::Int(42), false);
        let shared = Arc::new(base);
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let env = Arc::clone(&shared);
                std::thread::spawn(move || {
                    // 纯读：无锁、无 &mut，仅结构共享。
                    (i, env.get("answer").expect("binding present"))
                })
            })
            .collect();
        for h in handles {
            let (_, v) = h.join().expect("thread join");
            assert_eq!(v, Value::Int(42));
        }
    }

    #[test]
    fn env_merge_from_conflict_uses_strategy() {
        let mut parent = Environment::new();
        parent.define("x".into(), Value::Int(5), false);
        let mut child = Environment::new();
        child.define("x".into(), Value::Int(3), false);
        parent.merge_from(&child, &MergeStrategy::Add);
        assert_eq!(parent.get("x"), Some(Value::Int(8)));
    }

    #[test]
    fn env_merge_with_per_key_strategies() {
        let mut parent = Environment::new();
        parent.define("counter".into(), Value::Int(100), false);
        parent.define("log".into(), Value::List(vec![].into()), false);
        parent.define("name".into(), Value::String("alice".into()), false);

        let mut child = Environment::new();
        child.define("counter".into(), Value::Int(5), false);
        child.define(
            "log".into(),
            Value::List(vec![Value::String("msg1".into())].into()),
            false,
        );
        child.define("name".into(), Value::String("bob".into()), false);
        child.define("new_key".into(), Value::Int(42), false);

        let mut strategies: HashMap<String, MergeStrategy> = HashMap::new();
        strategies.insert("counter".into(), MergeStrategy::Add);
        strategies.insert("log".into(), MergeStrategy::Append);
        // "name" not in strategies → uses default (LWW)

        parent.merge_from_with_strategies(&child, &strategies, &MergeStrategy::LastWriteWins);

        // counter: 100 + 5 = 105 (Add strategy)
        assert_eq!(parent.get("counter"), Some(Value::Int(105)));
        // log: [] ++ ["msg1"] = ["msg1"] (Append strategy)
        assert_eq!(
            parent.get("log"),
            Some(Value::List(vec![Value::String("msg1".into())].into()))
        );
        // name: LWW → child wins (not in strategies map)
        assert_eq!(parent.get("name"), Some(Value::String("bob".into())));
        // new_key: new binding, defined directly
        assert_eq!(parent.get("new_key"), Some(Value::Int(42)));
    }
}
