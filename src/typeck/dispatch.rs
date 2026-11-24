//! v0.110.0: Builtin dispatch table — the canonical registry of builtin,
//! operator and method signatures.
//!
//! This module is the single source of truth for built-in functions,
//! type-level operators (binary, comparison), and method-dispatch
//! tables. The Hindley-Milner engine in [`crate::typeck::hm`]
//! consumes the `Signatures` so registered builtins and types are
//! recognized by the checker.
//!
//! It is also where `method_return_type` lives, which used to live in
//! `typeck::mod` and was the source of a back-reference from
//! `typeck::hm`. Moving it here keeps the dependency direction clean:
//! `typeck` depends on `typeck::dispatch`, never the other way around.

use crate::typeck::Type;

///  Function signature used by the builtin registry.
#[derive(Debug, Clone, PartialEq)]
pub struct Signature {
    /// v0.x parameter list as `(name, type)` pairs.
    pub params: Vec<(String, Type)>,
    /// v0.10 raw hint strings (e.g. "T" → number).
    pub raw_params: Vec<Option<String>>,
    pub return_type: Type,
    /// v0.10 raw return hint.
    pub raw_return_type: Option<String>,
    /// 可变参数标志（至少提供 `min_arity` 个参数，余下复用末位参数类型）。
    pub variadic: bool,
    /// 可选最小元数（用于 `range(end)` / `range(start, end, step)` 等可选参数函数）。
    pub min_arity: Option<usize>,
}

impl Signature {
    /// Build a signature with no raw-hint metadata (used by HM).
    pub fn new(params: Vec<(String, Type)>, return_type: Type) -> Self {
        let raw_params = params.iter().map(|_| None).collect();
        Self {
            params,
            raw_params,
            return_type,
            raw_return_type: None,
            variadic: false,
            min_arity: None,
        }
    }

    /// Fixed maximum arity with optional trailing parameters (`min_arity..=params.len()`).
    pub fn with_min_arity(
        params: Vec<(String, Type)>,
        min_arity: usize,
        return_type: Type,
    ) -> Self {
        let mut sig = Self::new(params, return_type);
        sig.min_arity = Some(min_arity);
        sig
    }

    /// v0.103: 变参签名 —— 末位参数类型可重复（`print(...)`）。
    pub fn variadic(params: Vec<(String, Type)>, return_type: Type) -> Self {
        let mut sig = Self::new(params, return_type);
        sig.variadic = true;
        sig
    }
}

///  Return the canonical builtin registry. This is a flat snapshot;
///  the underlying collection is intentionally immutable from the
///  outside so the checker can read it without contention.
pub fn builtin_signatures() -> Vec<(String, Signature)> {
    vec![
        (
            "compose".to_string(),
            Signature {
                params: vec![("f".to_string(), Type::Any)],
                raw_params: vec![None],
                return_type: Type::Compose,
                raw_return_type: None,
                variadic: true,
                min_arity: None,
            },
        ),
        (
            "partial".to_string(),
            Signature {
                params: vec![("f".to_string(), Type::Any)],
                raw_params: vec![None],
                return_type: Type::Partial,
                raw_return_type: None,
                variadic: true,
                min_arity: None,
            },
        ),
        (
            "merge_with".to_string(),
            Signature::new(
                vec![
                    ("key".to_string(), Type::String),
                    ("strategy".to_string(), Type::String),
                ],
                Type::Nil,
            ),
        ),
        // v0.110.1: First-class runtime assertions for `mora test` and scripts.
        (
            "assert".to_string(),
            Signature::with_min_arity(
                vec![
                    ("cond".to_string(), Type::Bool),
                    ("msg".to_string(), Type::String),
                ],
                1,
                Type::Nil,
            ),
        ),
        (
            "assert_eq".to_string(),
            Signature::with_min_arity(
                vec![
                    ("left".to_string(), Type::Any),
                    ("right".to_string(), Type::Any),
                    ("msg".to_string(), Type::String),
                ],
                2,
                Type::Nil,
            ),
        ),
        // v0.13: print(x...) accepts any printable value and returns nil.
        (
            "print".to_string(),
            Signature::variadic(vec![("x".to_string(), Type::Any)], Type::Nil),
        ),
        (
            "macroexpand".to_string(),
            Signature {
                params: vec![("macro".to_string(), Type::Any)],
                raw_params: vec![None],
                return_type: Type::Any,
                raw_return_type: None,
                variadic: true,
                min_arity: Some(0),
            },
        ),
        (
            "range".to_string(),
            Signature::with_min_arity(
                vec![
                    (
                        "start".to_string(),
                        Type::Union(vec![Type::Int, Type::Float]),
                    ),
                    ("end".to_string(), Type::Union(vec![Type::Int, Type::Float])),
                    (
                        "step".to_string(),
                        Type::Union(vec![Type::Int, Type::Float]),
                    ),
                ],
                1,
                Type::List(Box::new(Type::Int)),
            ),
        ),
        // len(x) for string / list / dict
        //
        // 返回 `Type::Int` —— 与运行期的 `Value::Int` 一致。两侧必须同侧，
        // 否则 typeck 会**拒绝正确标注、接受错误标注**：
        //     let n: Int   = len(xs)   → Type error: expected int, got float
        //     let n: Float = len(xs)   → 通过，但运行期拿到的是 Int
        //
        // builtin `len` 的函数签名只登记一份（本文件的 builtin 表）：HM 侧经
        // `dispatch::lookup_builtin` 取的是同一份（`hm::builtin`），不存在
        // 第二处 `len` 声明互相覆盖。
        (
            "len".to_string(),
            Signature::new(
                vec![(
                    "x".to_string(),
                    Type::Union(vec![
                        Type::String,
                        Type::List(Box::new(Type::Union(vec![]))),
                        Type::Dict(Box::new(Type::Union(vec![])), Box::new(Type::Union(vec![]))),
                        Type::Channel(Box::new(Type::Union(vec![]))),
                        Type::any_tensor(),
                    ]),
                )],
                Type::Int,
            ),
        ),
        // str(x) -> string
        (
            "str".to_string(),
            Signature::new(vec![("x".to_string(), Type::Any)], Type::String),
        ),
        // int(x) -> int (accepts string, float, int, bool)
        (
            "int".to_string(),
            Signature::new(
                vec![(
                    "s".to_string(),
                    Type::Union(vec![Type::String, Type::Float, Type::Int, Type::Bool]),
                )],
                Type::Int,
            ),
        ),
        // float(x) -> float (accepts string, float, int, bool)
        (
            "float".to_string(),
            Signature::new(
                vec![(
                    "x".to_string(),
                    Type::Union(vec![Type::String, Type::Float, Type::Int, Type::Bool]),
                )],
                Type::Float,
            ),
        ),
        // bool(x) -> bool
        (
            "bool".to_string(),
            Signature::new(vec![("x".to_string(), Type::Any)], Type::Bool),
        ),
        (
            "channel".to_string(),
            Signature::with_min_arity(
                vec![(
                    "capacity".to_string(),
                    Type::Union(vec![Type::Int, Type::Float, Type::Nil]),
                )],
                0,
                Type::Channel(Box::new(Type::Any)),
            ),
        ),
        ("context".to_string(), Signature::new(vec![], Type::Context)),
        (
            "sleep".to_string(),
            Signature::new(
                vec![("ms".to_string(), Type::Union(vec![Type::Int, Type::Float]))],
                Type::Nil,
            ),
        ),
        (
            "tcp_listen".to_string(),
            Signature::new(vec![("addr".to_string(), Type::String)], Type::TcpListener),
        ),
        (
            "tcp_connect".to_string(),
            Signature::with_min_arity(
                vec![
                    ("addr".to_string(), Type::String),
                    (
                        "timeout_ms".to_string(),
                        Type::Union(vec![Type::Int, Type::Float, Type::Nil]),
                    ),
                    (
                        "ctx".to_string(),
                        Type::Union(vec![Type::Context, Type::Nil]),
                    ),
                ],
                1,
                Type::TcpStream,
            ),
        ),
        (
            "tls_listen".to_string(),
            Signature::new(
                vec![
                    ("addr".to_string(), Type::String),
                    (
                        "config".to_string(),
                        Type::Dict(Box::new(Type::String), Box::new(Type::Any)),
                    ),
                ],
                Type::TlsListener,
            ),
        ),
        (
            "tls_connect".to_string(),
            Signature::with_min_arity(
                vec![
                    ("addr".to_string(), Type::String),
                    (
                        "server_name".to_string(),
                        Type::Union(vec![Type::String, Type::Nil]),
                    ),
                    (
                        "config".to_string(),
                        Type::Union(vec![
                            Type::Dict(Box::new(Type::String), Box::new(Type::Any)),
                            Type::Nil,
                        ]),
                    ),
                    (
                        "timeout_ms".to_string(),
                        Type::Union(vec![Type::Int, Type::Float, Type::Nil]),
                    ),
                    (
                        "ctx".to_string(),
                        Type::Union(vec![Type::Context, Type::Nil]),
                    ),
                ],
                1,
                Type::TlsStream,
            ),
        ),
        (
            "tensor".to_string(),
            Signature::with_min_arity(
                vec![
                    ("data".to_string(), Type::Any),
                    (
                        "dtype".to_string(),
                        Type::Union(vec![Type::String, Type::Nil]),
                    ),
                    (
                        "device".to_string(),
                        Type::Union(vec![Type::String, Type::Nil]),
                    ),
                    (
                        "requires_grad".to_string(),
                        Type::Union(vec![Type::Bool, Type::Nil]),
                    ),
                ],
                1,
                Type::any_tensor(),
            ),
        ),
        (
            "grad".to_string(),
            Signature::with_min_arity(
                vec![
                    ("f".to_string(), Type::Any),
                    (
                        "argnums".to_string(),
                        Type::Union(vec![Type::Int, Type::List(Box::new(Type::Int)), Type::Nil]),
                    ),
                ],
                1,
                Type::Closure,
            ),
        ),
        (
            "value_and_grad".to_string(),
            Signature::with_min_arity(
                vec![
                    ("f".to_string(), Type::Any),
                    (
                        "argnums".to_string(),
                        Type::Union(vec![Type::Int, Type::List(Box::new(Type::Int)), Type::Nil]),
                    ),
                ],
                1,
                Type::Closure,
            ),
        ),
        (
            "jvp".to_string(),
            Signature::new(
                vec![
                    ("f".to_string(), Type::Any),
                    ("primals".to_string(), Type::Any),
                    ("tangents".to_string(), Type::Any),
                ],
                Type::List(Box::new(Type::Any)),
            ),
        ),
        (
            "trace_dag".to_string(),
            Signature::variadic(
                vec![
                    ("f".to_string(), Type::Any),
                    ("input".to_string(), Type::Any),
                ],
                Type::ComputeDag,
            ),
        ),
        (
            "kv_allocator".to_string(),
            Signature::with_min_arity(
                vec![
                    ("total_pages".to_string(), Type::Int),
                    ("page_tokens".to_string(), Type::Int),
                    ("num_heads".to_string(), Type::Int),
                    ("head_dim".to_string(), Type::Int),
                    (
                        "dtype".to_string(),
                        Type::Union(vec![Type::String, Type::Nil]),
                    ),
                    (
                        "device".to_string(),
                        Type::Union(vec![Type::String, Type::Nil]),
                    ),
                    (
                        "max_bytes".to_string(),
                        Type::Union(vec![Type::Int, Type::Nil]),
                    ),
                ],
                4,
                Type::KVAllocator,
            ),
        ),
        (
            "memory_pool".to_string(),
            Signature::with_min_arity(
                vec![
                    ("max_bytes".to_string(), Type::Int),
                    (
                        "device".to_string(),
                        Type::Union(vec![Type::String, Type::Nil]),
                    ),
                ],
                1,
                Type::MemoryPool,
            ),
        ),
        (
            "process_group".to_string(),
            Signature::new(
                vec![("world_size".to_string(), Type::Int)],
                Type::ProcessGroup,
            ),
        ),
        (
            "tcp_process_group".to_string(),
            Signature::new(
                vec![
                    ("rank".to_string(), Type::Int),
                    ("world_size".to_string(), Type::Int),
                    ("addrs".to_string(), Type::List(Box::new(Type::String))),
                ],
                Type::ProcessGroup,
            ),
        ),
        (
            "profiler".to_string(),
            Signature::new(vec![], Type::TraceProfiler),
        ),
        // v0.91: 数学 builtin — math/stats/linalg/random
        // 这些是 domain prefix 模块，typeck 注册为 marker signature（任意参数→Any）
        // 实际方法分派由 dispatch.rs::call_method_builtin 处理
        ("math".to_string(), Signature::new(vec![], Type::Any)),
        ("stats".to_string(), Signature::new(vec![], Type::Any)),
        ("linalg".to_string(), Signature::new(vec![], Type::Any)),
        // v0.99: "random" 不在此表 —— 模块对象不可直接调用（`random(...)`
        // 应报非函数），方法调用经 infer_var → Type::RandomModule 分型。
    ]
}

// ════════════════════════════════════════════════════════════════════════════
// 模块方法表 —— 名字与签名同一张表（Wave 1 保留 7 个核心模块）
// ════════════════════════════════════════════════════════════════════════════

/// `Signature` 的 **const 可构造投影**。
#[derive(Clone, Copy, Debug)]
enum Ret {
    Any,
    Int,
    Float,
    Bool,
    String,
    Nil,
    /// `Tensor<u8, [_], cpu>` —— 紧凑 64B 对齐字节缓冲区（`file.read_binary` 等）。
    ByteTensor,
    /// `checked_path` —— 不可绕过的沙箱校验路径凭证（`file.check_path` / `std/os` / `std/io`）。
    CheckedPath,
    /// `Type::List` —— 元素类型唯一，故是 1 元数组而非切片。
    List(&'static [Ret; 1]),
    /// `Type::Dict` —— `[键, 值]`。
    Dict(&'static [Ret; 2]),
    /// `Type::Union` —— 顺序即声明顺序，参与兼容判定，不可重排。
    Union(&'static [Ret]),
}

impl Ret {
    fn materialize(self) -> Type {
        match self {
            Ret::Any => Type::Any,
            Ret::Int => Type::Int,
            Ret::Float => Type::Float,
            Ret::Bool => Type::Bool,
            Ret::String => Type::String,
            Ret::Nil => Type::Nil,
            Ret::ByteTensor => Type::Tensor {
                dtype: Box::new(Type::Scalar(crate::common::DType::U8)),
                shape: crate::typeck::ShapeType::Ranked(vec![crate::typeck::DimExpr::Symbolic(
                    "_".to_string(),
                )]),
                device: crate::typeck::DeviceType::Concrete(crate::common::Device::Cpu),
            },
            Ret::CheckedPath => Type::CheckedPath,
            Ret::List([elem]) => Type::List(Box::new(elem.materialize())),
            Ret::Dict([k, v]) => Type::Dict(Box::new(k.materialize()), Box::new(v.materialize())),
            Ret::Union(xs) => Type::Union(xs.iter().map(|r| r.materialize()).collect()),
        }
    }
}

/// 一个方法组：**同签名**的一组模块方法名 + 该签名。
#[derive(Clone, Copy)]
struct MethodGroup {
    names: &'static [&'static str],
    /// 最小元数（**不含** `self`）。
    min_arity: usize,
    ret: Ret,
}

impl MethodGroup {
    const fn new(names: &'static [&'static str], min_arity: usize, ret: Ret) -> Self {
        Self {
            names,
            min_arity,
            ret,
        }
    }

    /// 物化成 `Signature` —— 与 D171 的 `params_variadic` 逐字段等价：
    /// `self: Unknown` 打头（`infer_method_call` 用 `params.len() - 1`
    /// 算 user arity），其余形参一律 `Any`（保守过近似：能拒掉
    /// `String` 之类的错标注，又不会误拒合法实参），
    /// `raw_params` 与 `params` 等长且全 `None`，一律 `variadic`。
    fn signature(self) -> Signature {
        let n = self.min_arity;
        let mut params = Vec::with_capacity(n + 1);
        params.push(("self".to_string(), Type::Unknown));
        for i in 0..n {
            params.push((format!("a{i}"), Type::Any));
        }
        Signature {
            params,
            raw_params: vec![None; n + 1],
            return_type: self.ret.materialize(),
            raw_return_type: None,
            variadic: true,
            min_arity: None,
        }
    }
}

// ── math ── 返回类型逐条核对自 `builtins/math.rs::call_math_method` ──────
const MATH_METHODS: &[MethodGroup] = &[
    // 零参模块常量
    MethodGroup::new(&["PI", "E", "TAU", "INF", "NAN"], 0, Ret::Float),
    // unary_float —— 运行期一律 Ok(Value::Float)
    MethodGroup::new(
        &[
            "sin", "cos", "tan", "asin", "acos", "atan", "sinh", "cosh", "tanh", "exp", "log",
            "log2", "log10", "log1p", "sqrt", "cbrt", "fract",
        ],
        1,
        Ret::Float,
    ),
    // binary_float —— pow / hypot / atan2 是**双参**，写成 1 参会
    // 让 `math.pow(2.0, 3.0)` 报 "Expected 1 arguments, got 2"
    MethodGroup::new(&["pow", "hypot", "atan2"], 2, Ret::Float),
    // unary_preserve —— `Int` 进 `Int` 出、`Float` 进 `Float` 出，
    // 声明为数值塔（Union[Int, Float]）是**可靠的过近似**：既能拒掉
    // String 之类的错标注，又不会误拒 `Int` / `Float` 任一。
    MethodGroup::new(
        &["abs", "sign", "floor", "ceil", "round", "trunc"],
        1,
        Ret::Union(&[Ret::Int, Ret::Float]),
    ),
    // 类型谓词 —— 运行期一律 Ok(Value::Bool)
    MethodGroup::new(&["is_nan", "is_inf", "is_finite"], 1, Ret::Bool),
];

// ── json ── `call_builtin` 里 (BuiltinKind::Json, …) 两个 arm ────────────
const JSON_METHODS: &[MethodGroup] = &[
    MethodGroup::new(&["stringify"], 1, Ret::String),
    // parse 的结果由文本决定（对象/数组/标量），如实声明只能给 Any
    MethodGroup::new(&["parse"], 1, Ret::Any),
];

// ── file ── 返回类型与元数逐条核对自 `builtins/file.rs::call_file_method`
// 元数由各 arm 里 `expect_str(N, …)` / `args.get(N)` 的最大下标 +1 得出。 ──
const FILE_METHODS: &[MethodGroup] = &[
    // String —— 路径派生与读取
    MethodGroup::new(
        &[
            "read_text",
            "read_bytes",
            "abs",
            "basename",
            "dirname",
            "extname",
        ],
        1,
        Ret::String,
    ),
    // 零参
    MethodGroup::new(&["cwd", "home_dir", "os_name", "arch"], 0, Ret::String),
    MethodGroup::new(&["is_confined"], 0, Ret::Bool),
    // 沙箱路径凭证构造 & 环境变量读取
    MethodGroup::new(&["check_path"], 1, Ret::CheckedPath),
    MethodGroup::new(&["env"], 1, Ret::Union(&[Ret::String, Ret::Nil])),
    // `join` 是**变参**（运行期 `for arg in args`），最小 1 元
    MethodGroup::new(&["join"], 1, Ret::String),
    // Float —— `Ok(Value::Float(meta.len() as f64))`
    MethodGroup::new(&["size"], 1, Ret::Float),
    // List[String] —— 目录列举
    MethodGroup::new(&["list"], 1, Ret::List(&[Ret::String])),
    // Bool —— 三个路径谓词
    MethodGroup::new(&["exists", "is_file", "is_dir"], 1, Ret::Bool),
    // Nil —— 写操作一族（1 元）
    MethodGroup::new(
        &[
            "mkdir",
            "mkdir_all",
            "remove",
            "remove_all",
            "touch",
            "chdir",
        ],
        1,
        Ret::Nil,
    ),
    // Nil —— 写操作一族（2 元）
    MethodGroup::new(
        &[
            "write_text",
            "append_text",
            "write_bytes",
            "write_binary",
            "rename",
            "copy",
        ],
        2,
        Ret::Nil,
    ),
    // ByteTensor —— 零装箱二进制字节流读写（供 std/nn/safetensors、std/nn/gguf 等复用）
    MethodGroup::new(&["read_binary"], 1, Ret::ByteTensor),
    MethodGroup::new(&["read_binary_range"], 3, Ret::ByteTensor),
];

// ── exec ── `exec_parallel` 的成功路径返回 `Ok(Value::List[Dict])` ────────
//
// v0.104.6 D337：`min_arity` 由 **2 改为 1**。
//
// 修前 `exec.parallel(["echo a"])` 被 typeck 拒（"Expected 2 arguments, got 1"），
// 而**运行期明确支持**单参形式：
// - `exec.rs:143-145` 的错误消息自称「requires **at least 1 arg** (cmds list)」；
// - `exec.rs:176` / `:197` 用 `args.len() >= 2` / `>= 3` 把 `max_concurrent` /
//   `timeout_ms` 当**可选**处理；
// - `exec.rs:143` 的 `args.is_empty()` 守卫正是为「1 个参数」这条路径写的；
// - `exec.rs` 的运行期单测（`exec_parallel_runs_all_commands` 等）**全部用 1 个实参**。
//
// ⇒ 运行时契约 =「至少 1 个」，typeck 契约 =「至少 2 个」⇒ **契约分叉**。
// 这正是 `Signature` 文档里那句「运行期支持、类型系统拒绝 = 契约分叉」所指的情况；
// 而 D81 的 `signature_no_over_tightening` 只固化了**上限**那一侧
// （「不得拒绝多余实参」），**没管下限** ⇒ 双参的 `exec.parallel`
// 让这个下限过紧一直没人看见。
//
// 注：`min_arity` 降到 1 **不会**让 `exec.parallel([], -1)` 变成报错 ——
// `exec.rs:164` 的空列表早返回发生在可选参数校验**之前**，
// 那条不一致属另一层问题（D337 已实测并记档），本条不夹带修。
const EXEC_METHODS: &[MethodGroup] = &[MethodGroup::new(
    &["parallel"],
    1,
    Ret::List(&[Ret::Dict(&[Ret::String, Ret::Any])]),
)];

// ── stats ── 逐条核对自 `builtins/stats.rs::call_stats_method` ───────────
// 一元统计量（`expect_number_list(args.first(), …)`，arity 1）返 Float；
// `sum` 走 `sum_value` → `Value::Float(xs.iter().sum())`。 ────────────────
const STATS_METHODS: &[MethodGroup] = &[
    MethodGroup::new(
        &[
            "sum", "mean", "median", "var", "stddev", "min", "max", "quantile",
        ],
        1,
        Ret::Float,
    ),
    // `histogram` 的每个 bin 是 `Dict{lo, hi, count}`，外层是 List
    MethodGroup::new(
        &["histogram"],
        2,
        Ret::List(&[Ret::Dict(&[Ret::String, Ret::Float])]),
    ),
    // 双列相关 / 协方差
    MethodGroup::new(&["corr", "cov"], 2, Ret::Float),
];

// ── linalg ── 逐条核对自 `builtins/linalg.rs::call_linalg_method` ────────
const LINALG_METHODS: &[MethodGroup] = &[
    // 矩阵结果：`Vec<Vec<f64>>` → `List[List[Float]]`
    MethodGroup::new(&["transpose"], 1, Ret::List(&[Ret::List(&[Ret::Float])])),
    MethodGroup::new(&["matmul"], 2, Ret::List(&[Ret::List(&[Ret::Float])])),
    // `cross` 收两个向量 `Vec<f64>` → `List[Float]`
    MethodGroup::new(&["cross"], 2, Ret::List(&[Ret::Float])),
    MethodGroup::new(&["dot"], 2, Ret::Float),
    // `norm(vec, p?)` 的**阶数是可选的**（`args.get(1) … .unwrap_or(2.0)`）。
    // 按固定元数登记会拒掉其中一种合法写法 —— 1 参登记则
    // `linalg.norm(v, 3)` 被拒，2 参登记则 `linalg.norm(v)` 被拒。
    // 用变参（最小 1）同时放行两种；代价是第 3 个实参要到运行期才报
    // （`linalg.norm` 只读 `args.get(1)`），比现状略松但严格优于
    // 「两种写法都不检查、返回类型也丢失」。
    MethodGroup::new(&["norm"], 1, Ret::Float),
];

/// 模块名 → 方法组表。**这是模块名唯一的查表入口**（Wave 1 保留 7 个核心模块；`random` 走 `Type::RandomModule`）。
fn module_groups(module: &str) -> Option<&'static [MethodGroup]> {
    Some(match module {
        "math" => MATH_METHODS,
        "json" => JSON_METHODS,
        "file" => FILE_METHODS,
        "exec" => EXEC_METHODS,
        "stats" => STATS_METHODS,
        "linalg" => LINALG_METHODS,
        _ => return None,
    })
}

pub const RANDOM_METHODS: &[&str] = &["rand_int", "rand_float", "rand_choice", "seed", "shuffle"];

/// v0.104.6 D171：模块对象（`Value::Builtin`）的**全部方法名**。
pub fn module_method_names(module: &str) -> Vec<&'static str> {
    if module == "random" {
        return RANDOM_METHODS.to_vec();
    }
    module_groups(module)
        .map(|gs| gs.iter().flat_map(|g| g.names.iter().copied()).collect())
        .unwrap_or_default()
}

/// v0.104.6 D69：**模块对象**的方法签名，按**模块名**索引。
///
/// 与 [`method_signature`] 的区别：后者按 `Type` 索引（`Type::BuiltinModule`
/// 分支转调本表），本表按**模块名**索引 —— `infer_var` 把模块裸名解析成
/// `Type::BuiltinModule(name)`（`random` 为 `Type::RandomModule`），
/// 模块方法调用的返回类型即由此表给出。
///
/// 形参一律写 `Any`：**本表只负责给出结果类型**，不收紧实参（收紧实参需要
/// 与运行期 `call_*_method` 的逐分支校验逐条对齐，收益与风险不成比例）。
/// `self` 保留在首位，与 `method_signature` 的调用约定一致
/// （`infer_method_call` 用 `params.len() - 1` 算 user arity）。
///
/// 覆盖范围刻意保守：只登记**运行期返回类型确定且已被逐条核对**的模块。
/// 其余模块宁可继续返回 TypeVar，也不要写一个没核对过的签名 —— 那会把
/// 「不检查」换成「检查错」。
///
/// 接收者是**模块裸名**（如 `math`），不是 `Value` —— 本函数同时驱动
/// arity 与返回类型**两条路**。
///
/// D293：实现从「`match module` → `match group_of(GROUPS, method)` →
/// `Some(N) => 签名`」压平成一次线性查找，返回值逐字段与 D172 等价
/// （等价性判据见 `tests/module_method_groups_single_table.rs`）。
///
/// 找不到时返回 `None`：调用方据此把结果类型留给 `TypeVar`（不检查），
/// **而不是**报「未知方法」—— 后者由运行期的 `unknown method` 负责。
pub fn module_method_signature(module: &str, method: &str) -> Option<Signature> {
    module_groups(module)?
        .iter()
        .find(|g| g.names.contains(&method))
        .map(|g| g.signature())
}

pub fn lookup_builtin(name: &str) -> Option<Signature> {
    builtin_signatures()
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, s)| s)
}

///  Number of declared parameters for a known receiver method, used by
///  `infer_method_call` to enforce arity.
///  Returns `None` if the method is unknown to the dispatch table.
pub fn method_arity(receiver: &Type, method: &str) -> Option<usize> {
    if let Some(sig) = method_signature(receiver, method) {
        Some(sig.params.len())
    } else {
        None
    }
}

///  Look up a method's `Signature` (parameter list + return type) for
///  `receiver`. Returns `None` for unknown `(receiver, method)` pairs.
pub fn method_signature(receiver: &Type, method: &str) -> Option<Signature> {
    if let Type::BuiltinModule(module) = receiver {
        return module_method_signature(module, method);
    }
    if let Type::Union(members) = receiver
        && !members.is_empty()
    {
        let mut member_sigs: Vec<Signature> = Vec::with_capacity(members.len());
        for m in members {
            let sig = method_signature(m, method)?;
            member_sigs.push(sig);
        }
        let first = member_sigs[0].clone();
        if member_sigs.iter().all(|s| {
            s.params.len() == first.params.len()
                && s.min_arity == first.min_arity
                && s.variadic == first.variadic
        }) {
            let mut merged_params = first.params.clone();
            if let Some(p0) = merged_params.first_mut() {
                p0.1 = receiver.clone();
            }
            let mut ret_types: Vec<Type> = Vec::new();
            for s in &member_sigs {
                if !ret_types.contains(&s.return_type) {
                    ret_types.push(s.return_type.clone());
                }
            }
            let merged_ret = if ret_types.len() == 1 {
                ret_types.remove(0)
            } else {
                Type::Union(ret_types)
            };
            let mut sig = match first.min_arity {
                Some(min_a) => Signature::with_min_arity(merged_params, min_a, merged_ret),
                None => Signature::new(merged_params, merged_ret),
            };
            sig.variadic = first.variadic;
            return Some(sig);
        }
    }
    if let Some(sig) = method_signature_builtin(receiver, method) {
        return Some(sig);
    }
    if let Some(sig) = method_signature_via_type(receiver, method) {
        return Some(sig);
    }
    if method == "len" {
        // v0.104.6：`Float` → `Int`，与运行期及本文件上方的 builtin 签名一致。
        return Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::Int,
        ));
    }
    None
}

fn method_signature_builtin(receiver: &Type, method: &str) -> Option<Signature> {
    match (receiver, method) {
        // v0.75.16: 保留 List 元素类型（此前 map/filter/push 返回 List<Any>、
        // reduce/pop/get 返回 Any — 元素类型信息丢失）。
        // v0.75.16: 保留 List 元素类型。map/filter 接收闭包参数（M2 前
        // 用 Any 宽松约束）；push 接收元素参数（elem 类型）。
        (Type::List(elem), "map" | "filter") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("f".to_string(), Type::Any),
            ],
            Type::List(elem.clone()),
        )),
        (Type::List(elem), "push") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("value".to_string(), elem.as_ref().clone()),
            ],
            Type::List(elem.clone()),
        )),
        (Type::List(elem), "pop") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            (**elem).clone(),
        )),
        // ── v0.104.6 D130：补齐 spec §12 承诺、运行期已实现、但 typeck 无签名的
        // 9 个 List 方法 ──────────────────────────────────────────────
        //
        // 缺陷形态是**假阴性**（与 D54 的 `let n: String = d.len()` 同型）：
        // 无签名 → 返回未解算的 TypeVar → 与**任何**标注都「相容」→
        // 类型标注形同虚设。实测：
        //   let xs = [1, 2, 3]
        //   let bad: string = xs.take(2)     → exit 0，**零诊断**，而运行时
        //                                          `bad` 是个 list
        //
        // 运行期全部可用（已实测）：`reduce`=6.0、`take`=[1,2]、`drop`=[2,3]、
        // `window`=[[1,2],[2,3]]、`batch`=[[1,2],[3]]、`flatten`=[1,2,3]、
        // `transpose`（要求矩形）、`reshape`、`shape`。
        //
        // 返回类型尽量保留元素类型 `elem`（与上方 map/filter/push 同做法）；
        // 元素类型本身不可知的（flatten 的内层可异质）用 `Any`。
        (Type::List(elem), "take" | "drop") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("n".to_string(), Type::Union(vec![Type::Int, Type::Float])),
            ],
            Type::List(elem.clone()),
        )),
        (Type::List(elem), "window" | "batch") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                (
                    "size".to_string(),
                    Type::Union(vec![Type::Int, Type::Float]),
                ),
            ],
            Type::List(Box::new(Type::List(elem.clone()))),
        )),
        (Type::List(elem), "reduce") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("f".to_string(), Type::Any),
                // 累加初值必须与元素同类型（`[1,2,3].reduce(f, "")` 是错的）
                ("init".to_string(), elem.as_ref().clone()),
            ],
            (**elem).clone(),
        )),
        (Type::List(elem), "reshape") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                (
                    "rows".to_string(),
                    Type::Union(vec![Type::Int, Type::Float]),
                ),
                (
                    "cols".to_string(),
                    Type::Union(vec![Type::Int, Type::Float]),
                ),
            ],
            Type::List(elem.clone()),
        )),
        (Type::List(elem), "transpose") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::List(Box::new(Type::List(elem.clone()))),
        )),
        (Type::List(_), "flatten") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            // 内层元素类型不可知（`[[1],["x"]]` 是合法的），故用 Any
            Type::List(Box::new(Type::Any)),
        )),
        (Type::List(_), "shape") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::List(Box::new(Type::Int)),
        )),
        // v0.104.6 D52：下标收 **Int | Float**，与运行期 `index_value` 的
        // 两个分支（`(Value::List, Value::Int)` / `(Value::List, Value::Float)`，
        // 两者都走 `checked_index`）对齐。
        //
        // 收窄成单边会让同一操作的两种写法出现分歧（`xs.get(1)` 被拒，而
        // 等价的 `xs[1]` 落 `Any` 放行），故收 Int | Float。
        (Type::List(elem), "get") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                (
                    "index".to_string(),
                    Type::Union(vec![Type::Int, Type::Float]),
                ),
            ],
            (**elem).clone(),
        )),
        // v0.104.6：以下三条 `len` 方法签名此前声明 `Type::Float`，与运行期的
        // `Value::Int` 不符（详见上方 builtin `len` 签名的说明）。
        (Type::List(_), "len") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::Int,
        )),
        // get 接收 key（String）；返回 Union<V, Nil>（v0.75.16 unify 成员合一）
        (Type::Dict(_, v), "get") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("key".to_string(), Type::String),
            ],
            Type::Union(vec![v.as_ref().clone(), Type::Nil]),
        )),
        // set 接收 key + value
        //
        // v0.104.6 D127：形参 `value` 此前绑成 **dict 自身的 `V`**，与 spec §12
        // 方法表 `.set(key, val) | string, any -> dict` 明确写的 **`any`** 相悖。
        // 后果是一条**假阳性**：
        //   let d = {a: 1}
        //   let e = d.set("b", "text")     → 报 expected float, got string
        // 而运行期 `Value::Dict(HashMap<String, Value>)` **完全支持**异质，
        // `json.parse` 也能产出异质 dict（它的 `V` 未受约束，故那条路径可用）——
        // 于是「从字面量 dict 出发就不能再加异质键」，同一门语言里两套行为。
        //
        // `keys` / `values` / `len` / `get` 四条签名都正确，`set` 是**孤例**，
        // 故按疏漏修正而非设计决定。
        //
        // 返回类型同步放宽为 `Dict(k, Any)`：形参放宽后若仍返回 `Dict(k, v)`，
        // 存进去的 `Any` 值与声明的 `v` 会对不上（`e["b"]` 又会被按 `v` 检查而
        // 再次报错）。spec 的 `-> dict` 未指定 `V`，且运行时确实可异质。
        (Type::Dict(k, _), "set") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("key".to_string(), k.as_ref().clone()),
                ("value".to_string(), Type::Any),
            ],
            Type::Dict(k.clone(), Box::new(Type::Any)),
        )),
        (Type::Dict(k, _), "keys") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::List(Box::new(k.as_ref().clone())),
        )),
        (Type::Dict(_, v), "values") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::List(Box::new(v.as_ref().clone())),
        )),
        (Type::Dict(_, _), "len") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::Int,
        )),
        (Type::String, "len") => Some(Signature::new(
            vec![("self".to_string(), Type::String)],
            Type::Int,
        )),
        // v0.104.2: String 方法签名补全**实参**。此前 `upper/lower/trim/replace`
        // 与 `starts_with/ends_with/contains/split` 各自共用一条只声明 `self`
        // 的签名，而运行期 `call_method_string` 对这些名字都要读实参：
        //   replace(from, to) / starts_with(p) / ends_with(p) / contains(n) / split(sep)
        // 于是 `"a,b" |> split(",")` 报 "Expected 0 arguments, got 1"
        //（spec §7.6 的管道示例正是这个形状）。按运行期真实 arity 拆开。
        (Type::String, "upper" | "lower" | "trim") => Some(Signature::new(
            vec![("self".to_string(), Type::String)],
            Type::String,
        )),
        (Type::String, "replace") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::String),
                ("from".to_string(), Type::String),
                ("to".to_string(), Type::String),
            ],
            Type::String,
        )),
        (Type::String, "starts_with" | "ends_with" | "contains") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::String),
                ("needle".to_string(), Type::String),
            ],
            Type::Bool,
        )),
        (Type::String, "split") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::String),
                ("sep".to_string(), Type::String),
            ],
            Type::List(Box::new(Type::String)),
        )),
        // ── Go-style Channel<T>, TaskHandle<T>, and Context methods ──
        (Type::Channel(elem), "send") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("value".to_string(), (**elem).clone()),
            ],
            Type::Nil,
        )),
        (Type::Channel(elem), "try_send") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("value".to_string(), (**elem).clone()),
            ],
            Type::Bool,
        )),
        (Type::Channel(elem), "send_ctx") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("value".to_string(), (**elem).clone()),
                ("ctx".to_string(), Type::Context),
            ],
            Type::Nil,
        )),
        (Type::Channel(elem), "recv") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            (**elem).clone(),
        )),
        (Type::Channel(elem), "try_recv") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::Union(vec![(**elem).clone(), Type::Nil]),
        )),
        (Type::Channel(elem), "recv_ctx") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("ctx".to_string(), Type::Context),
            ],
            (**elem).clone(),
        )),
        (Type::Channel(_), "close") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::Nil,
        )),
        (Type::Channel(_), "is_closed") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::Bool,
        )),
        (Type::Channel(_), "len" | "capacity") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::Int,
        )),
        (Type::TaskHandle(res), "join") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            (**res).clone(),
        )),
        (Type::TaskHandle(_), "abort") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::Nil,
        )),
        (Type::TaskHandle(_), "is_done") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::Bool,
        )),
        (Type::TaskHandle(_), "context") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::Context,
        )),
        (Type::Context, "child") => Some(Signature::new(
            vec![("self".to_string(), Type::Context)],
            Type::Context,
        )),
        (Type::Context, "with_timeout") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::Context),
                ("ms".to_string(), Type::Union(vec![Type::Int, Type::Float])),
            ],
            Type::Context,
        )),
        (Type::Context, "cancel") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::Context),
                (
                    "reason".to_string(),
                    Type::Union(vec![Type::String, Type::Nil]),
                ),
            ],
            Type::Nil,
        )),
        (Type::Context, "is_cancelled") => Some(Signature::new(
            vec![("self".to_string(), Type::Context)],
            Type::Bool,
        )),
        (Type::Context, "spawn") => Some(Signature::variadic(
            vec![
                ("self".to_string(), Type::Context),
                ("fn".to_string(), Type::Any),
            ],
            Type::TaskHandle(Box::new(Type::Any)),
        )),
        // ── Wave 2: AsyncTcpListener / AsyncTcpStream methods ──
        (Type::TcpListener, "accept") => Some(Signature::with_min_arity(
            vec![
                ("self".to_string(), Type::TcpListener),
                (
                    "ctx".to_string(),
                    Type::Union(vec![Type::Context, Type::Nil]),
                ),
            ],
            1,
            Type::TcpStream,
        )),
        (Type::TcpListener, "local_addr") => Some(Signature::new(
            vec![("self".to_string(), Type::TcpListener)],
            Type::String,
        )),
        (Type::TcpListener, "close") => Some(Signature::new(
            vec![("self".to_string(), Type::TcpListener)],
            Type::Nil,
        )),
        (Type::TcpListener, "is_closed") => Some(Signature::new(
            vec![("self".to_string(), Type::TcpListener)],
            Type::Bool,
        )),
        (Type::TcpStream, "read" | "read_line") => Some(Signature::with_min_arity(
            vec![
                ("self".to_string(), Type::TcpStream),
                (
                    "max_bytes".to_string(),
                    Type::Union(vec![Type::Int, Type::Nil]),
                ),
                (
                    "ctx".to_string(),
                    Type::Union(vec![Type::Context, Type::Nil]),
                ),
            ],
            1,
            Type::String,
        )),
        (Type::TcpStream, "read_bytes") => Some(Signature::with_min_arity(
            vec![
                ("self".to_string(), Type::TcpStream),
                (
                    "max_bytes".to_string(),
                    Type::Union(vec![Type::Int, Type::Nil]),
                ),
                (
                    "ctx".to_string(),
                    Type::Union(vec![Type::Context, Type::Nil]),
                ),
            ],
            1,
            Type::List(Box::new(Type::Int)),
        )),
        (Type::TcpStream, "write") => Some(Signature::with_min_arity(
            vec![
                ("self".to_string(), Type::TcpStream),
                ("data".to_string(), Type::String),
                (
                    "ctx".to_string(),
                    Type::Union(vec![Type::Context, Type::Nil]),
                ),
            ],
            2,
            Type::Int,
        )),
        (Type::TcpStream, "write_bytes") => Some(Signature::with_min_arity(
            vec![
                ("self".to_string(), Type::TcpStream),
                ("bytes".to_string(), Type::List(Box::new(Type::Int))),
                (
                    "ctx".to_string(),
                    Type::Union(vec![Type::Context, Type::Nil]),
                ),
            ],
            2,
            Type::Int,
        )),
        (Type::TcpStream, "shutdown" | "close") => Some(Signature::new(
            vec![("self".to_string(), Type::TcpStream)],
            Type::Nil,
        )),
        (Type::TcpStream, "is_closed") => Some(Signature::new(
            vec![("self".to_string(), Type::TcpStream)],
            Type::Bool,
        )),
        (Type::TcpStream, "local_addr" | "peer_addr") => Some(Signature::new(
            vec![("self".to_string(), Type::TcpStream)],
            Type::String,
        )),
        // ── v0.110.1: Native Async TLS 1.2/1.3 Listener / Stream methods ──
        (Type::TlsListener, "accept") => Some(Signature::with_min_arity(
            vec![
                ("self".to_string(), Type::TlsListener),
                (
                    "ctx".to_string(),
                    Type::Union(vec![Type::Context, Type::Nil]),
                ),
            ],
            1,
            Type::TlsStream,
        )),
        (Type::TlsListener, "local_addr") => Some(Signature::new(
            vec![("self".to_string(), Type::TlsListener)],
            Type::String,
        )),
        (Type::TlsListener, "close") => Some(Signature::new(
            vec![("self".to_string(), Type::TlsListener)],
            Type::Nil,
        )),
        (Type::TlsListener, "is_closed") => Some(Signature::new(
            vec![("self".to_string(), Type::TlsListener)],
            Type::Bool,
        )),
        (Type::TlsStream, "read" | "read_line") => Some(Signature::with_min_arity(
            vec![
                ("self".to_string(), Type::TlsStream),
                (
                    "max_bytes".to_string(),
                    Type::Union(vec![Type::Int, Type::Nil]),
                ),
                (
                    "ctx".to_string(),
                    Type::Union(vec![Type::Context, Type::Nil]),
                ),
            ],
            1,
            Type::String,
        )),
        (Type::TlsStream, "read_bytes") => Some(Signature::with_min_arity(
            vec![
                ("self".to_string(), Type::TlsStream),
                (
                    "max_bytes".to_string(),
                    Type::Union(vec![Type::Int, Type::Nil]),
                ),
                (
                    "ctx".to_string(),
                    Type::Union(vec![Type::Context, Type::Nil]),
                ),
            ],
            1,
            Type::List(Box::new(Type::Int)),
        )),
        (Type::TlsStream, "write") => Some(Signature::with_min_arity(
            vec![
                ("self".to_string(), Type::TlsStream),
                ("data".to_string(), Type::String),
                (
                    "ctx".to_string(),
                    Type::Union(vec![Type::Context, Type::Nil]),
                ),
            ],
            2,
            Type::Int,
        )),
        (Type::TlsStream, "write_bytes") => Some(Signature::with_min_arity(
            vec![
                ("self".to_string(), Type::TlsStream),
                ("bytes".to_string(), Type::List(Box::new(Type::Int))),
                (
                    "ctx".to_string(),
                    Type::Union(vec![Type::Context, Type::Nil]),
                ),
            ],
            2,
            Type::Int,
        )),
        (Type::TlsStream, "shutdown" | "close") => Some(Signature::new(
            vec![("self".to_string(), Type::TlsStream)],
            Type::Nil,
        )),
        (Type::TlsStream, "is_closed") => Some(Signature::new(
            vec![("self".to_string(), Type::TlsStream)],
            Type::Bool,
        )),
        (
            Type::TlsStream,
            "local_addr" | "peer_addr" | "tls_version" | "cipher_suite" | "alpn_protocol",
        ) => Some(Signature::new(
            vec![("self".to_string(), Type::TlsStream)],
            Type::String,
        )),
        // ── Wave 4: TensorComputeDag & KVPageAllocator methods ──
        (Type::ComputeDag, "fuse" | "compile") => Some(Signature::new(
            vec![("self".to_string(), Type::ComputeDag)],
            Type::ComputeDag,
        )),
        (Type::ComputeDag, "vjp" | "grad" | "jvp") => Some(Signature::with_min_arity(
            vec![
                ("self".to_string(), Type::ComputeDag),
                ("input_idx".to_string(), Type::Int),
            ],
            1,
            Type::ComputeDag,
        )),
        (Type::ComputeDag, "run") => Some(Signature::variadic(
            vec![
                ("self".to_string(), Type::ComputeDag),
                ("input".to_string(), Type::Any),
            ],
            Type::any_tensor(),
        )),
        (Type::ComputeDag, "node_count" | "fused_count") => Some(Signature::new(
            vec![("self".to_string(), Type::ComputeDag)],
            Type::Int,
        )),
        (Type::ComputeDag, "is_fused") => Some(Signature::new(
            vec![("self".to_string(), Type::ComputeDag)],
            Type::Bool,
        )),
        (Type::ComputeDag, "op_names") => Some(Signature::new(
            vec![("self".to_string(), Type::ComputeDag)],
            Type::List(Box::new(Type::String)),
        )),
        (Type::ComputeDag, "to_dot" | "cache_key") => Some(Signature::new(
            vec![("self".to_string(), Type::ComputeDag)],
            Type::String,
        )),
        (Type::ComputeDag, "memory_plan" | "cache_stats") => Some(Signature::new(
            vec![("self".to_string(), Type::ComputeDag)],
            Type::Dict(Box::new(Type::String), Box::new(Type::Int)),
        )),
        (Type::ComputeDag, "cost_report") => Some(Signature::new(
            vec![("self".to_string(), Type::ComputeDag)],
            Type::Dict(Box::new(Type::String), Box::new(Type::Any)),
        )),
        (Type::ComputeDag, "run_profiled") => Some(Signature::variadic(
            vec![
                ("self".to_string(), Type::ComputeDag),
                ("input".to_string(), Type::Any),
            ],
            Type::Dict(Box::new(Type::String), Box::new(Type::Any)),
        )),
        (Type::ComputeDag, "backend_info") => Some(Signature::new(
            vec![("self".to_string(), Type::ComputeDag)],
            Type::Dict(Box::new(Type::String), Box::new(Type::String)),
        )),
        (Type::KVAllocator, "alloc_seq") => Some(Signature::with_min_arity(
            vec![
                ("self".to_string(), Type::KVAllocator),
                ("seq_id".to_string(), Type::Int),
                (
                    "priority".to_string(),
                    Type::Union(vec![Type::Int, Type::Nil]),
                ),
            ],
            2,
            Type::Bool,
        )),
        (Type::KVAllocator, "set_priority") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::KVAllocator),
                ("seq_id".to_string(), Type::Int),
                ("priority".to_string(), Type::Int),
            ],
            Type::Nil,
        )),
        (Type::KVAllocator, "set_quota") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::KVAllocator),
                ("max_bytes".to_string(), Type::Int),
            ],
            Type::Nil,
        )),
        (Type::KVAllocator, "on_watermark") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::KVAllocator),
                (
                    "threshold".to_string(),
                    Type::Union(vec![Type::Float, Type::Int]),
                ),
                ("callback".to_string(), Type::Any),
            ],
            Type::Nil,
        )),
        (Type::KVAllocator, "clear_watermarks") => Some(Signature::new(
            vec![("self".to_string(), Type::KVAllocator)],
            Type::Nil,
        )),
        (Type::KVAllocator, "append_tokens") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::KVAllocator),
                ("seq_id".to_string(), Type::Int),
                ("count".to_string(), Type::Int),
            ],
            Type::List(Box::new(Type::Int)),
        )),
        (Type::KVAllocator, "fork_seq") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::KVAllocator),
                ("src_seq_id".to_string(), Type::Int),
                ("dst_seq_id".to_string(), Type::Int),
            ],
            Type::List(Box::new(Type::Int)),
        )),
        (Type::KVAllocator, "write_kv") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::KVAllocator),
                ("seq_id".to_string(), Type::Int),
                ("token_pos".to_string(), Type::Int),
                ("k".to_string(), Type::any_tensor()),
                ("v".to_string(), Type::any_tensor()),
            ],
            Type::Nil,
        )),
        (Type::KVAllocator, "read_kv") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::KVAllocator),
                ("seq_id".to_string(), Type::Int),
            ],
            Type::List(Box::new(Type::any_tensor())),
        )),
        (Type::KVAllocator, "free_seq" | "seq_tokens" | "seq_priority") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::KVAllocator),
                ("seq_id".to_string(), Type::Int),
            ],
            Type::Int,
        )),
        (Type::KVAllocator, "seq_pages") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::KVAllocator),
                ("seq_id".to_string(), Type::Int),
            ],
            Type::List(Box::new(Type::Int)),
        )),
        (Type::KVAllocator, "evict_preempt") => Some(Signature::with_min_arity(
            vec![
                ("self".to_string(), Type::KVAllocator),
                (
                    "max_victim_priority".to_string(),
                    Type::Union(vec![Type::Int, Type::Nil]),
                ),
            ],
            1,
            Type::Int,
        )),
        (Type::KVAllocator, "reclaim_for") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::KVAllocator),
                ("pages_needed".to_string(), Type::Int),
                ("requester_priority".to_string(), Type::Int),
            ],
            Type::List(Box::new(Type::Int)),
        )),
        (Type::KVAllocator, "swap_out") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::KVAllocator),
                ("seq_id".to_string(), Type::Int),
            ],
            Type::Int,
        )),
        (Type::KVAllocator, "swap_in") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::KVAllocator),
                ("seq_id".to_string(), Type::Int),
            ],
            Type::List(Box::new(Type::Int)),
        )),
        (Type::KVAllocator, "is_swapped") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::KVAllocator),
                ("seq_id".to_string(), Type::Int),
            ],
            Type::Bool,
        )),
        (Type::KVAllocator, "cache_prefix") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::KVAllocator),
                ("seq_id".to_string(), Type::Int),
                ("token_ids".to_string(), Type::List(Box::new(Type::Int))),
            ],
            Type::Int,
        )),
        (Type::KVAllocator, "match_prefix") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::KVAllocator),
                ("token_ids".to_string(), Type::List(Box::new(Type::Int))),
            ],
            Type::Int,
        )),
        (Type::KVAllocator, "attach_prefix") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::KVAllocator),
                ("seq_id".to_string(), Type::Int),
                ("token_ids".to_string(), Type::List(Box::new(Type::Int))),
            ],
            Type::Int,
        )),
        (
            Type::KVAllocator,
            "evict_lru" | "free_pages" | "used_pages" | "total_pages" | "bytes_used"
            | "quota_bytes",
        ) => Some(Signature::new(
            vec![("self".to_string(), Type::KVAllocator)],
            Type::Int,
        )),
        (Type::KVAllocator, "utilization") => Some(Signature::new(
            vec![("self".to_string(), Type::KVAllocator)],
            Type::Float,
        )),
        (Type::KVAllocator, "stats") => Some(Signature::new(
            vec![("self".to_string(), Type::KVAllocator)],
            Type::Dict(Box::new(Type::String), Box::new(Type::Int)),
        )),
        // ── Wave 4 P1: General Tensor MemoryPool methods ──
        (Type::MemoryPool, "alloc" | "zeros") => Some(Signature::with_min_arity(
            vec![
                ("self".to_string(), Type::MemoryPool),
                ("shape".to_string(), Type::List(Box::new(Type::Int))),
                (
                    "dtype".to_string(),
                    Type::Union(vec![Type::String, Type::Nil]),
                ),
            ],
            2,
            Type::any_tensor(),
        )),
        (Type::MemoryPool, "full") => Some(Signature::with_min_arity(
            vec![
                ("self".to_string(), Type::MemoryPool),
                ("shape".to_string(), Type::List(Box::new(Type::Int))),
                (
                    "value".to_string(),
                    Type::Union(vec![Type::Float, Type::Int]),
                ),
                (
                    "dtype".to_string(),
                    Type::Union(vec![Type::String, Type::Nil]),
                ),
            ],
            3,
            Type::any_tensor(),
        )),
        (Type::MemoryPool, "release") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::MemoryPool),
                ("tensor".to_string(), Type::any_tensor()),
            ],
            Type::Int,
        )),
        (Type::MemoryPool, "set_quota") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::MemoryPool),
                ("max_bytes".to_string(), Type::Int),
            ],
            Type::Nil,
        )),
        (Type::MemoryPool, "on_watermark") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::MemoryPool),
                (
                    "threshold".to_string(),
                    Type::Union(vec![Type::Float, Type::Int]),
                ),
                ("callback".to_string(), Type::Any),
            ],
            Type::Nil,
        )),
        (Type::MemoryPool, "clear_watermarks") => Some(Signature::new(
            vec![("self".to_string(), Type::MemoryPool)],
            Type::Nil,
        )),
        (Type::MemoryPool, "trim" | "active_bytes" | "cached_bytes" | "max_bytes") => Some(
            Signature::new(vec![("self".to_string(), Type::MemoryPool)], Type::Int),
        ),
        (Type::MemoryPool, "utilization") => Some(Signature::new(
            vec![("self".to_string(), Type::MemoryPool)],
            Type::Float,
        )),
        (Type::MemoryPool, "stats") => Some(Signature::new(
            vec![("self".to_string(), Type::MemoryPool)],
            Type::Dict(Box::new(Type::String), Box::new(Type::Int)),
        )),
        // ── P4-4: Distributed ProcessGroup collective methods ──
        (Type::ProcessGroup, "rank" | "world_size") => Some(Signature::new(
            vec![("self".to_string(), Type::ProcessGroup)],
            Type::Int,
        )),
        (Type::ProcessGroup, "backend") => Some(Signature::new(
            vec![("self".to_string(), Type::ProcessGroup)],
            Type::String,
        )),
        (Type::ProcessGroup, "ranks") => Some(Signature::new(
            vec![("self".to_string(), Type::ProcessGroup)],
            Type::List(Box::new(Type::ProcessGroup)),
        )),
        (Type::ProcessGroup, "all_reduce" | "reduce_scatter") => Some(Signature::with_min_arity(
            vec![
                ("self".to_string(), Type::ProcessGroup),
                ("tensor".to_string(), Type::any_tensor()),
                ("op".to_string(), Type::Union(vec![Type::String, Type::Nil])),
            ],
            2,
            Type::any_tensor(),
        )),
        (Type::ProcessGroup, "all_gather") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::ProcessGroup),
                ("tensor".to_string(), Type::any_tensor()),
            ],
            Type::any_tensor(),
        )),
        (Type::ProcessGroup, "broadcast") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::ProcessGroup),
                ("tensor".to_string(), Type::any_tensor()),
                ("root".to_string(), Type::Int),
            ],
            Type::any_tensor(),
        )),
        (Type::ProcessGroup, "barrier") => Some(Signature::new(
            vec![("self".to_string(), Type::ProcessGroup)],
            Type::Nil,
        )),
        (Type::ProcessGroup, "stats") => Some(Signature::new(
            vec![("self".to_string(), Type::ProcessGroup)],
            Type::Dict(Box::new(Type::String), Box::new(Type::Int)),
        )),
        // ── P4-4: TraceProfiler nanosecond span recorder methods ──
        (Type::TraceProfiler, "start" | "stop" | "clear") => Some(Signature::new(
            vec![("self".to_string(), Type::TraceProfiler)],
            Type::Nil,
        )),
        (Type::TraceProfiler, "is_active") => Some(Signature::new(
            vec![("self".to_string(), Type::TraceProfiler)],
            Type::Bool,
        )),
        (Type::TraceProfiler, "now_ns") => Some(Signature::new(
            vec![("self".to_string(), Type::TraceProfiler)],
            Type::Int,
        )),
        (Type::TraceProfiler, "record") => Some(Signature::with_min_arity(
            vec![
                ("self".to_string(), Type::TraceProfiler),
                ("cat".to_string(), Type::String),
                ("name".to_string(), Type::String),
                ("start_ns".to_string(), Type::Int),
                ("dur_ns".to_string(), Type::Int),
                (
                    "args".to_string(),
                    Type::Union(vec![
                        Type::Dict(Box::new(Type::String), Box::new(Type::Any)),
                        Type::Nil,
                    ]),
                ),
            ],
            5,
            Type::Nil,
        )),
        (Type::TraceProfiler, "events") => Some(Signature::new(
            vec![("self".to_string(), Type::TraceProfiler)],
            Type::List(Box::new(Type::Dict(
                Box::new(Type::String),
                Box::new(Type::Any),
            ))),
        )),
        (Type::TraceProfiler, "summary") => Some(Signature::new(
            vec![("self".to_string(), Type::TraceProfiler)],
            Type::Dict(Box::new(Type::String), Box::new(Type::Any)),
        )),
        (Type::TraceProfiler, "to_chrome_json") => Some(Signature::new(
            vec![("self".to_string(), Type::TraceProfiler)],
            Type::String,
        )),
        // ── v0.112.0 (G6): First-class sandboxed CheckedPath capability methods ──
        (
            Type::CheckedPath,
            "as_str" | "to_str" | "basename" | "dirname" | "extname" | "read_text" | "read_bytes",
        ) => Some(Signature::new(
            vec![("self".to_string(), Type::CheckedPath)],
            Type::String,
        )),
        (Type::CheckedPath, "join") => Some(Signature::variadic(
            vec![
                ("self".to_string(), Type::CheckedPath),
                ("segment".to_string(), Type::String),
            ],
            Type::CheckedPath,
        )),
        (Type::CheckedPath, "exists" | "is_file" | "is_dir") => Some(Signature::new(
            vec![("self".to_string(), Type::CheckedPath)],
            Type::Bool,
        )),
        (Type::CheckedPath, "size") => Some(Signature::new(
            vec![("self".to_string(), Type::CheckedPath)],
            Type::Float,
        )),
        (Type::CheckedPath, "list") => Some(Signature::new(
            vec![("self".to_string(), Type::CheckedPath)],
            Type::List(Box::new(Type::String)),
        )),
        (Type::CheckedPath, "mkdir" | "mkdir_all" | "remove" | "remove_all" | "touch") => Some(
            Signature::new(vec![("self".to_string(), Type::CheckedPath)], Type::Nil),
        ),
        (Type::CheckedPath, "write_text" | "append_text" | "write_bytes") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::CheckedPath),
                ("content".to_string(), Type::String),
            ],
            Type::Nil,
        )),
        (Type::CheckedPath, "read_binary") => Some(Signature::new(
            vec![("self".to_string(), Type::CheckedPath)],
            Ret::ByteTensor.materialize(),
        )),
        (Type::CheckedPath, "read_binary_range") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::CheckedPath),
                ("offset".to_string(), Type::Int),
                ("len".to_string(), Type::Int),
            ],
            Ret::ByteTensor.materialize(),
        )),
        (Type::CheckedPath, "write_binary") => Some(Signature::new(
            vec![
                ("self".to_string(), Type::CheckedPath),
                ("data".to_string(), Type::any_tensor()),
            ],
            Type::Nil,
        )),
        (Type::Tensor { .. }, "shape") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::List(Box::new(Type::Int)),
        )),
        (Type::Tensor { .. }, "ndim" | "rank" | "numel" | "len") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::Int,
        )),
        (Type::Tensor { .. }, "dtype" | "device") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::String,
        )),
        (Type::Tensor { .. }, "is_contiguous" | "requires_grad" | "is_vram_resident") => Some(
            Signature::new(vec![("self".to_string(), receiver.clone())], Type::Bool),
        ),
        (
            Type::Tensor { .. },
            "contiguous" | "abs" | "sqrt" | "exp" | "ln" | "relu" | "gelu" | "silu" | "tanh"
            | "sigmoid" | "softmax",
        ) => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            receiver.clone(),
        )),
        (Type::Tensor { .. }, "with_grad") => Some(Signature::with_min_arity(
            vec![
                ("self".to_string(), receiver.clone()),
                ("enabled".to_string(), Type::Bool),
            ],
            1,
            receiver.clone(),
        )),
        (Type::Tensor { .. }, "rms_norm" | "layer_norm") => Some(Signature::with_min_arity(
            vec![
                ("self".to_string(), receiver.clone()),
                ("eps".to_string(), Type::Union(vec![Type::Int, Type::Float])),
            ],
            1,
            receiver.clone(),
        )),
        (Type::Tensor { dtype, .. }, "sum" | "max" | "min" | "item") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            (**dtype).clone(),
        )),
        (Type::Tensor { dtype, .. }, "mean") => {
            let ret = match (**dtype).as_dtype() {
                Some(dt) if dt.is_float() => Type::from_dtype(dt),
                _ => Type::Float,
            };
            Some(Signature::new(
                vec![("self".to_string(), receiver.clone())],
                ret,
            ))
        }
        (Type::Tensor { .. }, "to_list") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::List(Box::new(Type::Any)),
        )),
        (Type::Tensor { .. }, "to_bytes") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Ret::ByteTensor.materialize(),
        )),
        (Type::Tensor { .. }, "to_utf8") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::String,
        )),
        (Type::Tensor { .. }, "reshape") => Some(Signature::variadic(
            vec![
                ("self".to_string(), receiver.clone()),
                ("shape".to_string(), Type::Any),
            ],
            Type::any_tensor(),
        )),
        (Type::Tensor { .. }, "transpose") => Some(Signature::with_min_arity(
            vec![
                ("self".to_string(), receiver.clone()),
                (
                    "dim0".to_string(),
                    Type::Union(vec![Type::Int, Type::Float]),
                ),
                (
                    "dim1".to_string(),
                    Type::Union(vec![Type::Int, Type::Float]),
                ),
            ],
            1,
            Type::any_tensor(),
        )),
        (Type::Tensor { .. }, "to" | "to_dtype" | "to_device") => Some(Signature::with_min_arity(
            vec![
                ("self".to_string(), receiver.clone()),
                ("target".to_string(), Type::String),
                ("second".to_string(), Type::String),
            ],
            2,
            Type::any_tensor(),
        )),
        (Type::Tensor { .. }, "fill_" | "add_" | "mul_") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("rhs".to_string(), Type::Any),
            ],
            receiver.clone(),
        )),
        _ => None,
    }
}

fn method_signature_via_type(receiver: &Type, method: &str) -> Option<Signature> {
    let ret = method_return_type(receiver, method);
    // v0.75.91: top type (Any) 与 escape hatch (Unknown) 都不算「已知签名」，
    // 都应 None —— 调用方走 fallback 路径（unknown_method_returns_none 测试）
    if matches!(ret, Type::Any) || matches!(ret, Type::Unknown) {
        None
    } else {
        Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            ret,
        ))
    }
}

///  Return the result type of a method call. Mirrors the dispatch
///  tables above. Returns `Type::Unknown` for unknown combinations so
///  callers can fall back gracefully.
pub fn method_return_type(receiver: &Type, method: &str) -> Type {
    if let Some(sig) = method_signature_builtin(receiver, method) {
        return sig.return_type;
    }
    method_return_type_fallback(receiver, method)
}

fn method_return_type_fallback(receiver: &Type, method: &str) -> Type {
    if let Type::Union(_) = receiver {
        return Type::Unknown;
    }
    if method == "len" {
        return Type::Int;
    }
    Type::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_signature_is_known() {
        let sig = lookup_builtin("print").expect("print should be registered");
        assert_eq!(sig.params.len(), 1);
        assert!(matches!(sig.return_type, Type::Nil));
    }

    #[test]
    fn list_map_arity_is_two() {
        // v0.75.16: map 接收闭包参数（receiver + f = 2 参）。
        assert_eq!(
            method_arity(&Type::List(Box::new(Type::Unknown)), "map"),
            Some(2)
        );
    }

    #[test]
    fn unknown_method_returns_none() {
        assert!(method_signature(&Type::String, "no_such_method").is_none());
    }
}
