//! v0.110.0: 统一静态类型系统与 AI-Native 编译期检查器（`src/typeck/`）。
//!
//! 架构原则（Wave 3 Stage 3A / 3B / 3C）：
//! - **单一求解入口**：[`check_mir::check_program_witnesses`] 是 CLI、运行期与 LSP 的唯一静态类型检查入口。
//! - **精确 `Span` 追踪与错误毒化恢复**：基于 `TypeVarId::{Meta, Rigid}`、`Type::Error` 毒化隔离与 fail-closed `Type::Unknown` 一次收集全部静态错误。
//! - **5 项 AI-Native 静态检查**：
//!   1. `DType`：硬件数值位宽严格匹配（`f64`/`f32`/`f16`/`bf16`/`fp8`/`i64`..`i4`/`u64`..`u8`/`bool`）
//!   2. `Shape`：一等 `Tensor<DType, Shape, Device>` 符号维度合一、广播与多维切片边界校验
//!   3. `Device`：静态追踪设备位置（`cpu`/`cuda:N`/`vulkan:N`/`metal:N`），禁止隐式跨设备混算
//!   4. `Resource`：作用域资源配额与自动微分磁带期间的原位修改（`tensor_mutate`）别名检查
//!   5. `Effect`：封闭可组合效果行（`EffectRow`）包含检查与自动微分纯度校验
//! - **名义与服务类型契约**：泛型 `struct`（含默认值与编译期 `@tag` 元数据）、带载荷 `enum`、源码级 `trait`/`impl`/`dyn Trait`、具名/默认参数、同构 `list<T>`/`dict<K, V>`、不变 `channel<T>` 与协变 `task_handle<T>`/`result<T, E>`。

pub mod check_mir;
pub mod dispatch;
pub mod hm;
/// 跨模块 import 符号表（typeck 阶段预扫描合并）
pub mod imports;

use std::collections::HashMap;

// 共享类型（Span 等）统一来自 crate::common；本模块直接消费 MirWitness。
use crate::common::Span;

// ===================================================================
// 公共类型
// ===================================================================

/// Type variable identity: inference meta-variable vs explicit rigid generic parameter.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TypeVarId {
    /// HM inference meta-variable allocated during inference.
    Meta(u32),
    /// User-declared generic type parameter (`task f<T>`, `fn<T>`).
    Rigid(String),
}

impl std::fmt::Display for TypeVarId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TypeVarId::Meta(id) => write!(f, "t{id}"),
            TypeVarId::Rigid(name) => write!(f, "{name}"),
        }
    }
}

/// Linear arithmetic operator inside a static tensor dimension expression (`DimExpr::BinOp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DimOp {
    Add,
    Sub,
    Mul,
    Div,
}

impl std::fmt::Display for DimOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DimOp::Add => write!(f, "+"),
            DimOp::Sub => write!(f, "-"),
            DimOp::Mul => write!(f, "*"),
            DimOp::Div => write!(f, "/"),
        }
    }
}

/// Stage 3B / G7: Static tensor dimension expression in `ShapeType::Ranked`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DimExpr {
    /// Statically known concrete dimension extent (e.g., `128`).
    Known(usize),
    /// Named symbolic dimension from user annotation (e.g., `B`, `Seq`, `Hidden`, or `"_"` wildcard).
    Symbolic(String),
    /// Unification meta-variable for dimension inference.
    Var(u32),
    /// Linear arithmetic expression over dimensions (e.g., `S + 1`, `D * 4`, `H / 2`).
    BinOp(DimOp, Box<DimExpr>, Box<DimExpr>),
}

impl DimExpr {
    /// Algebraically normalize and constant-fold linear dimension expressions.
    pub(crate) fn simplify(&self) -> DimExpr {
        match self {
            DimExpr::Known(_) | DimExpr::Symbolic(_) | DimExpr::Var(_) => self.clone(),
            DimExpr::BinOp(op, lhs, rhs) => {
                let l = lhs.simplify();
                let r = rhs.simplify();
                match (*op, l, r) {
                    // Constant folding
                    (DimOp::Add, DimExpr::Known(a), DimExpr::Known(b)) => {
                        DimExpr::Known(a.saturating_add(b))
                    }
                    (DimOp::Sub, DimExpr::Known(a), DimExpr::Known(b)) if a >= b => {
                        DimExpr::Known(a - b)
                    }
                    (DimOp::Mul, DimExpr::Known(a), DimExpr::Known(b)) => {
                        DimExpr::Known(a.saturating_mul(b))
                    }
                    (DimOp::Div, DimExpr::Known(a), DimExpr::Known(b)) if b > 0 && a % b == 0 => {
                        DimExpr::Known(a / b)
                    }
                    // Algebraic identities & canonical ordering (Known on right for commutative ops)
                    (DimOp::Add, x, DimExpr::Known(0)) | (DimOp::Add, DimExpr::Known(0), x) => x,
                    (DimOp::Sub, x, DimExpr::Known(0)) => x,
                    (DimOp::Mul, x, DimExpr::Known(1)) | (DimOp::Mul, DimExpr::Known(1), x) => x,
                    (DimOp::Mul, _, DimExpr::Known(0)) | (DimOp::Mul, DimExpr::Known(0), _) => {
                        DimExpr::Known(0)
                    }
                    (DimOp::Div, x, DimExpr::Known(1)) => x,
                    (DimOp::Add, DimExpr::Known(k), x) => {
                        DimExpr::BinOp(DimOp::Add, Box::new(x), Box::new(DimExpr::Known(k)))
                            .simplify()
                    }
                    (DimOp::Mul, DimExpr::Known(k), x) => {
                        DimExpr::BinOp(DimOp::Mul, Box::new(x), Box::new(DimExpr::Known(k)))
                            .simplify()
                    }
                    // Associativity & inverse cancellation
                    (DimOp::Add, DimExpr::BinOp(DimOp::Add, inner, c1), DimExpr::Known(b)) => {
                        if let DimExpr::Known(a) = *c1 {
                            DimExpr::BinOp(
                                DimOp::Add,
                                inner,
                                Box::new(DimExpr::Known(a.saturating_add(b))),
                            )
                            .simplify()
                        } else {
                            DimExpr::BinOp(
                                DimOp::Add,
                                Box::new(DimExpr::BinOp(DimOp::Add, inner, c1)),
                                Box::new(DimExpr::Known(b)),
                            )
                        }
                    }
                    (DimOp::Sub, DimExpr::BinOp(DimOp::Add, inner, c1), DimExpr::Known(b)) if matches!(*c1, DimExpr::Known(a) if a == b) => {
                        *inner
                    }
                    (DimOp::Add, DimExpr::BinOp(DimOp::Sub, inner, c1), DimExpr::Known(b)) if matches!(*c1, DimExpr::Known(a) if a == b) => {
                        *inner
                    }
                    (DimOp::Mul, DimExpr::BinOp(DimOp::Mul, inner, c1), DimExpr::Known(b)) => {
                        if let DimExpr::Known(a) = *c1 {
                            DimExpr::BinOp(
                                DimOp::Mul,
                                inner,
                                Box::new(DimExpr::Known(a.saturating_mul(b))),
                            )
                            .simplify()
                        } else {
                            DimExpr::BinOp(
                                DimOp::Mul,
                                Box::new(DimExpr::BinOp(DimOp::Mul, inner, c1)),
                                Box::new(DimExpr::Known(b)),
                            )
                        }
                    }
                    (DimOp::Div, DimExpr::BinOp(DimOp::Mul, inner, c1), DimExpr::Known(b))
                        if b > 0 && matches!(*c1, DimExpr::Known(a) if a == b) =>
                    {
                        *inner
                    }
                    (op, l, r) => DimExpr::BinOp(op, Box::new(l), Box::new(r)),
                }
            }
        }
    }

    /// Recursively transform every leaf (`Known` / `Symbolic` / `Var`) and simplify.
    pub(crate) fn map_leaves(&self, f: &mut impl FnMut(&DimExpr) -> DimExpr) -> DimExpr {
        match self {
            DimExpr::Known(_) | DimExpr::Symbolic(_) | DimExpr::Var(_) => f(self),
            DimExpr::BinOp(op, lhs, rhs) => DimExpr::BinOp(
                *op,
                Box::new(lhs.map_leaves(f)),
                Box::new(rhs.map_leaves(f)),
            )
            .simplify(),
        }
    }

    /// Collect all meta/symbolic dimension variables referenced inside this expression.
    pub(crate) fn collect_vars(&self, out: &mut Vec<TypeVarId>) {
        match self {
            DimExpr::Known(_) => {}
            DimExpr::Var(id) => out.push(TypeVarId::Meta(*id)),
            DimExpr::Symbolic(sym) if sym != "_" => out.push(TypeVarId::Rigid(sym.clone())),
            DimExpr::Symbolic(_) => {}
            DimExpr::BinOp(_, lhs, rhs) => {
                lhs.collect_vars(out);
                rhs.collect_vars(out);
            }
        }
    }

    /// Return true if meta variable `id` occurs anywhere inside this expression.
    pub(crate) fn contains_var(&self, id: u32) -> bool {
        match self {
            DimExpr::Var(v) => *v == id,
            DimExpr::Known(_) | DimExpr::Symbolic(_) => false,
            DimExpr::BinOp(_, l, r) => l.contains_var(id) || r.contains_var(id),
        }
    }

    pub fn compatible_with(&self, other: &DimExpr) -> bool {
        let a = self.simplify();
        let b = other.simplify();
        match (&a, &b) {
            (DimExpr::Known(x), DimExpr::Known(y)) => x == y,
            (DimExpr::Var(_), _) | (_, DimExpr::Var(_)) => true,
            (DimExpr::Symbolic(s), _) | (_, DimExpr::Symbolic(s)) if s == "_" => true,
            (DimExpr::Symbolic(x), DimExpr::Symbolic(y)) => x == y,
            (DimExpr::BinOp(op1, l1, r1), DimExpr::BinOp(op2, l2, r2)) => {
                op1 == op2 && l1.compatible_with(l2) && r1.compatible_with(r2)
            }
            _ => false,
        }
    }
}

impl std::fmt::Display for DimExpr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DimExpr::Known(n) => write!(f, "{n}"),
            DimExpr::Symbolic(s) => write!(f, "{s}"),
            DimExpr::Var(id) => write!(f, "_d{id}"),
            DimExpr::BinOp(op, l, r) => write!(f, "({l} {op} {r})"),
        }
    }
}

/// Stage 3B: Static tensor shape representation in `Type::Tensor`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ShapeType {
    /// Ranked shape with known rank and per-axis `DimExpr` (e.g., `[B, 128]`).
    Ranked(Vec<DimExpr>),
    /// Unranked / polymorphic shape (`None` = unconstrained wildcard shape, `Some(var)` = generic shape variable `S`).
    Unranked(Option<TypeVarId>),
}

impl ShapeType {
    pub fn compatible_with(&self, other: &ShapeType) -> bool {
        match (self, other) {
            (ShapeType::Unranked(None), _) | (_, ShapeType::Unranked(None)) => true,
            (ShapeType::Unranked(Some(TypeVarId::Meta(_))), _)
            | (_, ShapeType::Unranked(Some(TypeVarId::Meta(_)))) => true,
            (
                ShapeType::Unranked(Some(TypeVarId::Rigid(a))),
                ShapeType::Unranked(Some(TypeVarId::Rigid(b))),
            ) => a == b,
            (ShapeType::Ranked(a), ShapeType::Ranked(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.compatible_with(y))
            }
            _ => false,
        }
    }
}

impl std::fmt::Display for ShapeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShapeType::Ranked(dims) => {
                let parts: Vec<String> = dims.iter().map(|d| d.to_string()).collect();
                write!(f, "[{}]", parts.join(", "))
            }
            ShapeType::Unranked(Some(var)) => write!(f, "{var}"),
            ShapeType::Unranked(None) => write!(f, "_"),
        }
    }
}

/// Stage 3B: Static tensor device placement representation in `Type::Tensor`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DeviceType {
    /// Concrete placement (`cpu`, `cuda:0`, `metal:0`).
    Concrete(crate::common::Device),
    /// Polymorphic device parameter (`Dev` in `Tensor<D, S, Dev>` or meta inference var).
    Var(TypeVarId),
    /// Unconstrained device wildcard (`_` or bare `tensor`).
    Any,
}

impl DeviceType {
    pub fn compatible_with(&self, other: &DeviceType) -> bool {
        match (self, other) {
            (DeviceType::Any, _) | (_, DeviceType::Any) => true,
            (DeviceType::Var(TypeVarId::Meta(_)), _) | (_, DeviceType::Var(TypeVarId::Meta(_))) => {
                true
            }
            (DeviceType::Var(TypeVarId::Rigid(a)), DeviceType::Var(TypeVarId::Rigid(b))) => a == b,
            (DeviceType::Concrete(a), DeviceType::Concrete(b)) => a == b,
            _ => false,
        }
    }
}

impl std::fmt::Display for DeviceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceType::Concrete(d) => write!(f, "{d}"),
            DeviceType::Var(v) => write!(f, "{v}"),
            DeviceType::Any => write!(f, "_"),
        }
    }
}

/// Mora 类型系统：基础类型 + Any（推断不出时退路）
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    String,
    /// v0.x: 单字符类型（`string[number]` 索引结果）
    Char,
    // v0.38: numeric tower — Int(i64) and Float(f64) as distinct types.
    Int,
    Float,
    Bool,
    Nil,
    /// 任意类型（推断不出时的退路，或显式 `any` 标注）
    Any,
    /// Unknown type escape marker — fails closed in compatibility and subtyping.
    Unknown,
    /// Poison sentinel emitted after recording a type error to stop cascading errors.
    Error,
    /// Built-in stdlib module namespace (`json`, `file`, `exec`, `math`, `stats`, `linalg`).
    BuiltinModule(String),
    /// v0.x: 列表类型携带元素类型（`list<T>`）
    List(Box<Type>),
    /// v0.x: 字典类型携带键值类型（`dict<K, V>`）
    Dict(Box<Type>, Box<Type>),
    Task,
    Closure,
    Builtin,
    /// v0.99: random 模块类型（`random` 内建变量的接收者类型）。
    /// 模块方法调用 = ambient effect perform —— 每操作一个标签
    /// （`crate::mir::effect::ambient`）+ 预置签名，效果行随调用传播。
    RandomModule,
    /// v0.06.2: 类型化错误处理 Result<T, E>
    Result_(Box<Type>, Box<Type>),
    /// v0.83: 元组类型
    Tuple(Vec<Box<Type>>),
    /// v0.08: dyn trait 类型（名称）
    /// v0.09: 携带泛型参数列表（如 `dyn Container<number>`）
    Trait {
        name: String,
        generics: Vec<Type>,
    },
    /// v0.09: 具体类型（替代 v0.08.5 删的 Type::Struct）
    ///   携带泛型参数 + 实现的 trait 列表
    Concrete {
        name: String,
        generics: Vec<Type>,
        traits: Vec<Type>,
    },
    /// v0.13: Union 类型（多种类型的合集，e.g. `string | number | bool`）
    ///   用于 builtin 多类型签名（print 等）
    ///   兼容规则: A 兼容 B 当 A 是 B 的成员, 或 B 是 A 的成员, 或递归嵌套
    Union(Vec<Type>),
    /// v0.08.5: Trait object carrier (for_type + trait_name + generics + data)
    /// v0.75.86: 扩展 trait_name + generics 字段——subtype_of 升级为
    /// trait_name + generics 同构判断（之前 unit variant 返 false 是 stub）。
    /// 对应运行时 `Value::TraitObject` 包含 dyn dispatch 信息。
    TraitObject {
        trait_name: String,
        generics: Vec<Type>,
    },
    /// v0.17: Compose pipeline (arity = number of functions)
    Compose,
    /// v0.18: Partial application (boxed origin + how many args applied)
    Partial,
    /// v0.19: Atom (mutable reference cell)
    Atom,
    /// v0.20: Macro definition (name + params shape)
    Macro,
    /// HM type variable (meta-variable or rigid generic parameter).
    TypeVar(TypeVarId),
    /// Generic quantification ∀α₁...αₙ. τ — product of let-generalization / explicit generics.
    ForAll(Vec<TypeVarId>, Box<Type>),
    /// v0.80: 函数类型带 effect row（Stage 2/4 algebraic effects 的类型基础）。
    /// `fn(a) -> b ! {Ai, Fs}` — input type → output type with effect row。
    /// Koka 风格：`Arrow(input, output, EffectRow)`。
    /// 与 ForAll 的区别：ForAll 跨函数泛型量化；Arrow 标记具体函数类型的 effect。
    /// 老 `Closure` / `Task` 类型视为 `Arrow(_, _, Empty)`。
    Arrow(Box<Type>, Box<Type>, crate::mir::effect::EffectRow),
    /// cons 单元类型（`cons(h, t)` / `car` / `cdr`）
    Cons(Box<Type>, Box<Type>),
    /// Wave 2: Typed concurrent message channel (`channel<T>`).
    Channel(Box<Type>),
    /// Wave 2: Asynchronous task join handle (`task_handle<T>`).
    TaskHandle(Box<Type>),
    /// Wave 2: Cooperative cancellation & deadline context (`context`).
    Context,
    /// Wave 2: Non-blocking async TCP listener (`tcp_listener`).
    TcpListener,
    /// Wave 2: Non-blocking async TCP stream (`tcp_stream`).
    TcpStream,
    /// v0.110.1: Native async TLS 1.2/1.3 listener (`tls_listener`).
    TlsListener,
    /// v0.110.1: Native async TLS 1.2/1.3 stream (`tls_stream`).
    TlsStream,
    /// Wave 4: Native fused tensor compute graph (`compute_dag`).
    ComputeDag,
    /// Wave 4: Paged KV-cache dynamic resource allocator (`kv_allocator`).
    KVAllocator,
    /// Wave 4 P1: General 64-byte aligned size-classed tensor memory pool (`memory_pool`).
    MemoryPool,
    /// P4-4: Distributed tensor collective communicator (`process_group` / `tcp_process_group`).
    ProcessGroup,
    /// P4-4: Nanosecond compute & service trace profiler (`profiler()` / `std/trace`).
    TraceProfiler,
    /// Stage 3B: Distinct low-bitwidth / hardware-precision scalar type (`f32`, `f16`, `bf16`, `fp8`, `i32`, `i16`, `i8`, `i4`, `u64`, `u32`, `u16`, `u8`).
    /// Canonical 64-bit `i64` is `Type::Int` and `f64` is `Type::Float`.
    Scalar(crate::common::DType),
    /// Stage 3B: First-class multidimensional tensor type `Tensor<DType, Shape, Device>`.
    Tensor {
        dtype: Box<Type>,
        shape: ShapeType,
        device: DeviceType,
    },
    /// P4-1A: Compile-time integer const-generic argument (e.g., `4` in `Linear<4, 3>`).
    ConstInt(i64),
    /// P4-1B: First-class compiled GPU `kernel` block value with optional typed parameter signature.
    GpuKernel(Option<Vec<crate::common::kernel_ir::KernelParamType>>),
} // ← close pub enum Type

impl Type {
    /// Convert a `DType` into its canonical `Type` representation (`I64 -> Type::Int`, `F64 -> Type::Float`, `Bool -> Type::Bool`, others `Type::Scalar(dt)`).
    pub fn from_dtype(dt: crate::common::DType) -> Type {
        match dt {
            crate::common::DType::I64 => Type::Int,
            crate::common::DType::F64 => Type::Float,
            crate::common::DType::Bool => Type::Bool,
            other => Type::Scalar(other),
        }
    }

    /// Extract the concrete `DType` if this type is `Type::Int`, `Type::Float`, `Type::Bool`, or `Type::Scalar(dt)`.
    pub fn as_dtype(&self) -> Option<crate::common::DType> {
        match self {
            Type::Int => Some(crate::common::DType::I64),
            Type::Float => Some(crate::common::DType::F64),
            Type::Bool => Some(crate::common::DType::Bool),
            Type::Scalar(dt) => Some(*dt),
            _ => None,
        }
    }

    /// Construct an unconstrained `Type::Tensor` (used for bare `tensor` annotations and fallback signatures).
    pub fn any_tensor() -> Type {
        Type::Tensor {
            dtype: Box::new(Type::Any),
            shape: ShapeType::Unranked(None),
            device: DeviceType::Any,
        }
    }

    /// Build a curried `Arrow` type for a parameter slice, using empty `Tuple([])`
    /// as the unit domain when `param_types` is empty so 0-arg callables remain
    /// `Type::Arrow` values rather than bare return types.
    pub(crate) fn curried_arrow(
        param_types: &[Type],
        ret: Type,
        row: &crate::mir::effect::EffectRow,
    ) -> Type {
        if param_types.is_empty() {
            return Type::Arrow(
                Box::new(Type::Tuple(Vec::new())),
                Box::new(ret),
                row.clone(),
            );
        }
        let mut ty = ret;
        for param_ty in param_types.iter().rev() {
            ty = Type::Arrow(Box::new(param_ty.clone()), Box::new(ty), row.clone());
        }
        ty
    }

    /// 返回类型的字符串表示。v0.x 起支持泛型：`list<number>` / `dict<string, any>` / `result<T, E>`
    pub fn name(&self) -> String {
        match self {
            Type::String => "string".to_string(),
            Type::Char => "char".to_string(),
            // v0.38: Int and Float distinct.
            Type::Int => "int".to_string(),
            Type::Float => "float".to_string(),
            Type::Bool => "bool".to_string(),
            Type::Nil => "nil".to_string(),
            Type::Any => "any".to_string(),
            Type::Unknown => "unknown".to_string(),
            Type::Error => "<error>".to_string(),
            Type::BuiltinModule(name) => format!("module<{name}>"),
            Type::List(elem) => format!("list<{}>", elem.name()),
            Type::Dict(k, v) => format!("dict<{}, {}>", k.name(), v.name()),
            Type::Task => "task".to_string(),
            Type::Closure => "closure".to_string(),
            Type::Builtin => "builtin".to_string(),
            Type::RandomModule => "random".to_string(),
            Type::Result_(ok, err) => format!("result<{}, {}>", ok.name(), err.name()),
            // v0.83: Tuple 类型
            Type::Tuple(types) => {
                let parts: Vec<String> = types.iter().map(|t| t.name()).collect();
                format!("({})", parts.join(", "))
            }
            Type::Trait { name, generics } => {
                if generics.is_empty() {
                    name.clone()
                } else {
                    format!(
                        "{}<{}>",
                        name,
                        generics
                            .iter()
                            .map(|g| g.name())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            Type::Concrete { name, generics, .. } => {
                if generics.is_empty() {
                    name.clone()
                } else {
                    format!(
                        "{}<{}>",
                        name,
                        generics
                            .iter()
                            .map(|g| g.name())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            Type::TraitObject {
                trait_name,
                generics,
            } => {
                if generics.is_empty() {
                    format!("dyn {}", trait_name)
                } else {
                    format!(
                        "dyn {}<{}>",
                        trait_name,
                        generics
                            .iter()
                            .map(|t| t.name())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            Type::Compose => "compose".to_string(),
            Type::Partial => "partial".to_string(),
            Type::Atom => "atom".to_string(),
            Type::Macro => "macro".to_string(),
            Type::TypeVar(id) => format!("'{}", id),
            // v0.75.17: 泛型量化显示为 "forall<'a, 'b>. τ"（let-generalization 产物）
            Type::ForAll(vars, inner) => {
                let names: Vec<String> = vars.iter().map(|v| format!("'{}", v)).collect();
                format!("forall<{}>. {}", names.join(", "), inner.name())
            }
            // v0.80: 函数类型带 effect row —— `fn (T) -> U ! {Ai, Fs}` 形式。
            Type::Arrow(input, output, row) => {
                let row_str = row.to_string();
                let arg_str = if matches!(&**input, Type::Tuple(elems) if elems.is_empty()) {
                    String::new()
                } else {
                    input.name()
                };
                if row_str == "pure" {
                    format!("fn ({arg_str}) -> {}", output.name())
                } else {
                    format!("fn ({arg_str}) -> {} ! {{ {} }}", output.name(), row_str)
                }
            }
            // v0.13: Union 类型显示为 "T1 | T2 | T3"
            Type::Union(members) => {
                if members.is_empty() {
                    return "any".to_string();
                }
                let parts: Vec<String> = members.iter().map(|m| m.name()).collect();
                parts.join(" | ")
            }
            Type::Cons(h, t) => format!("cons<{}, {}>", h.name(), t.name()),
            Type::Channel(t) => format!("channel<{}>", t.name()),
            Type::TaskHandle(t) => format!("task_handle<{}>", t.name()),
            Type::Context => "context".to_string(),
            Type::TcpListener => "tcp_listener".to_string(),
            Type::TcpStream => "tcp_stream".to_string(),
            Type::TlsListener => "tls_listener".to_string(),
            Type::TlsStream => "tls_stream".to_string(),
            Type::ComputeDag => "compute_dag".to_string(),
            Type::KVAllocator => "kv_allocator".to_string(),
            Type::MemoryPool => "memory_pool".to_string(),
            Type::ProcessGroup => "process_group".to_string(),
            Type::TraceProfiler => "trace_profiler".to_string(),
            Type::ConstInt(n) => n.to_string(),
            Type::GpuKernel(params) => match params {
                Some(ps) => {
                    let parts: Vec<&str> = ps.iter().map(|p| p.canonical_name()).collect();
                    format!("gpu_kernel({})", parts.join(", "))
                }
                None => "gpu_kernel".to_string(),
            },
            Type::Scalar(dt) => dt.canonical_type_name().to_string(),
            Type::Tensor {
                dtype,
                shape,
                device,
            } => {
                if matches!(**dtype, Type::Any)
                    && matches!(shape, ShapeType::Unranked(None))
                    && matches!(device, DeviceType::Any)
                {
                    "tensor".to_string()
                } else {
                    format!("Tensor<{}, {}, {}>", dtype.name(), shape, device)
                }
            }
        }
    }

    /// 从用户写的类型 hint 字符串解析
    pub fn from_hint(hint: &str) -> Type {
        if let Some(dt) = crate::common::DType::from_name(hint) {
            return Type::from_dtype(dt);
        }
        match hint {
            "string" => Type::String,
            "char" => Type::Char,
            "int" => Type::Int,
            "float" => Type::Float,
            "number" => Type::Union(vec![Type::Int, Type::Float]),
            "bool" => Type::Bool,
            "nil" => Type::Nil,
            // v0.50: "any" 应解析为 Union(vec![])（兼容任何类型）
            "any" => Type::Union(vec![]),
            "list" => Type::List(Box::new(Type::Union(vec![]))),
            "dict" => Type::Dict(Box::new(Type::Union(vec![])), Box::new(Type::Union(vec![]))),
            "channel" => Type::Channel(Box::new(Type::Union(vec![]))),
            "task_handle" | "taskhandle" => Type::TaskHandle(Box::new(Type::Union(vec![]))),
            "context" => Type::Context,
            "tcp_listener" | "tcplistener" => Type::TcpListener,
            "tcp_stream" | "tcpstream" => Type::TcpStream,
            "tls_listener" | "tlslistener" => Type::TlsListener,
            "tls_stream" | "tlsstream" => Type::TlsStream,
            "compute_dag" | "computedag" => Type::ComputeDag,
            "kv_allocator" | "kvallocator" => Type::KVAllocator,
            "memory_pool" | "memorypool" => Type::MemoryPool,
            "process_group" | "processgroup" => Type::ProcessGroup,
            "trace_profiler" | "traceprofiler" => Type::TraceProfiler,
            "gpu_kernel" | "gpukernel" | "kernel" => Type::GpuKernel(None),
            "tensor" | "Tensor" => Type::any_tensor(),
            "task" => Type::Task,
            "closure" => Type::Closure,
            // v0.x: list<T> 泛型语法
            s if s.starts_with("list<") && s.ends_with('>') => {
                let inner = &s[5..s.len() - 1];
                Type::List(Box::new(Type::from_hint(inner.trim())))
            }
            s if s.starts_with("channel<") && s.ends_with('>') => {
                let inner = &s[8..s.len() - 1];
                Type::Channel(Box::new(Type::from_hint(inner.trim())))
            }
            s if s.starts_with("task_handle<") && s.ends_with('>') => {
                let inner = &s[12..s.len() - 1];
                Type::TaskHandle(Box::new(Type::from_hint(inner.trim())))
            }
            // v0.x: dict<K, V> 泛型语法（顶层 split，保留嵌套）
            s if s.starts_with("dict<") && s.ends_with('>') => {
                let inner = &s[5..s.len() - 1];
                match split_top_level_comma(inner) {
                    Some((k_str, v_str)) => Type::Dict(
                        Box::new(Type::from_hint(k_str.trim())),
                        Box::new(Type::from_hint(v_str.trim())),
                    ),
                    None => {
                        Type::Dict(Box::new(Type::Union(vec![])), Box::new(Type::Union(vec![])))
                    }
                }
            }
            // v0.x: string<char> 单字符
            "string<char>" => Type::Char,
            // v0.08: dyn: 前缀 → Trait 类型
            // v0.09: dyn:Foo<number> → Trait { name: "Foo", generics: [Number] }
            // v0.10: 泛型嵌套如 Boxed<number> → Type::Trait { name: "Boxed", generics: [Number] }
            s if s.starts_with("dyn:") => {
                let rest = &s[4..];
                if let Some(lt) = rest.find('<') {
                    let name = rest[..lt].to_string();
                    let generics_str = &rest[lt + 1..rest.len() - 1];
                    let generics: Vec<Type> = if generics_str.is_empty() {
                        vec![]
                    } else {
                        generics_str
                            .split(',')
                            .map(|s| Type::from_hint(s.trim()))
                            .collect()
                    };
                    Type::Trait { name, generics }
                } else {
                    Type::Trait {
                        name: rest.to_string(),
                        generics: vec![],
                    }
                }
            }
            // v0.10 修复: 嵌套泛型 `Foo<Bar<number>>` 解析为 Type::Trait
            s if s.contains('<') && s.ends_with('>') => {
                if let Some(lt) = s.find('<') {
                    let name = s[..lt].to_string();
                    let generics_str = &s[lt + 1..s.len() - 1];
                    let generics: Vec<Type> = if generics_str.is_empty() {
                        vec![]
                    } else {
                        generics_str
                            .split(',')
                            .map(|s| Type::from_hint(s.trim()))
                            .collect()
                    };
                    Type::Trait { name, generics }
                } else {
                    Type::Union(vec![])
                }
            }
            _ => Type::Trait {
                name: hint.to_string(),
                generics: vec![],
            },
        }
    }

    /// v0.12: 判断类型名是否是合法 builtin / 已知类型
    pub fn is_builtin_type_name(name: &str) -> bool {
        matches!(
            name,
            "string"
                | "char"
                | "int"
                | "i4"
                | "i8"
                | "i16"
                | "i32"
                | "i64"
                | "u8"
                | "u16"
                | "u32"
                | "u64"
                | "float"
                | "f16"
                | "bf16"
                | "fp8"
                | "f32"
                | "f64"
                | "number"
                | "bool"
                | "nil"
                | "list"
                | "dict"
                | "channel"
                | "task_handle"
                | "taskhandle"
                | "context"
                | "tensor"
                | "Tensor"
                | "task"
                | "closure"
                | "any"
        )
    }

    /// Type compatibility predicate (symmetric across compatible types; numeric coercion allowed).
    pub fn compatible_with(&self, expected: &Type) -> bool {
        if matches!(self, Type::Error) || matches!(expected, Type::Error) {
            return true;
        }
        // Fail-closed sentinel: Unknown must never silently pass type compatibility.
        if matches!(self, Type::Unknown) || matches!(expected, Type::Unknown) {
            return false;
        }
        if matches!(self, Type::Any) || matches!(expected, Type::Any) {
            return true;
        }
        if matches!(self, Type::TypeVar(TypeVarId::Meta(_)))
            || matches!(expected, Type::TypeVar(TypeVarId::Meta(_)))
        {
            return true;
        }
        // Empty Union represents top element placeholder (`any`).
        if let Type::Union(members) = expected {
            if members.is_empty() {
                return true;
            }
            if matches!(self, Type::Scalar(_))
                && members.contains(&Type::Int)
                && members.contains(&Type::Float)
            {
                return true;
            }
            return members.iter().any(|m| self.compatible_with(m));
        }
        if let Type::Union(members) = self {
            if members.is_empty() {
                return true;
            }
            if matches!(expected, Type::Scalar(_))
                && members.contains(&Type::Int)
                && members.contains(&Type::Float)
            {
                return true;
            }
            return members.iter().any(|m| m.compatible_with(expected));
        }
        if let Type::ForAll(_, inner) = self {
            return inner.compatible_with(expected);
        }
        if let Type::ForAll(_, inner) = expected {
            return self.compatible_with(inner);
        }
        if let (Type::TypeVar(TypeVarId::Rigid(a)), Type::TypeVar(TypeVarId::Rigid(b))) =
            (self, expected)
        {
            return a == b;
        }
        if matches!(self, Type::TypeVar(TypeVarId::Rigid(_)))
            || matches!(expected, Type::TypeVar(TypeVarId::Rigid(_)))
        {
            return false;
        }
        if let (
            Type::Tensor {
                dtype: d1,
                shape: s1,
                device: dev1,
            },
            Type::Tensor {
                dtype: d2,
                shape: s2,
                device: dev2,
            },
        ) = (self, expected)
        {
            let dtype_ok = (d1.subtype_of(d2) && d2.subtype_of(d1))
                || matches!(**d1, Type::Any | Type::TypeVar(TypeVarId::Meta(_)))
                || matches!(**d2, Type::Any | Type::TypeVar(TypeVarId::Meta(_)));
            return dtype_ok && s1.compatible_with(s2) && dev1.compatible_with(dev2);
        }
        // v0.13: Result<T1, E1> 兼容 Result<T2, E2> 当 T1 兼容 T2 且 E1 兼容 E2
        if let (Type::Result_(t1, e1), Type::Result_(t2, e2)) = (self, expected) {
            return t1.compatible_with(t2) && e1.compatible_with(e2);
        }
        // v0.83: Tuple 元素逐一兼容
        if let (Type::Tuple(a), Type::Tuple(b)) = (self, expected) {
            if a.len() != b.len() {
                return false;
            }
            for (ta, tb) in a.iter().zip(b.iter()) {
                if !ta.compatible_with(tb) {
                    return false;
                }
            }
            return true;
        }
        // v0.x: List<T1> 兼容 List<T2> 当 T1 兼容 T2
        if let (Type::List(a), Type::List(b)) = (self, expected) {
            return a.compatible_with(b);
        }
        if let (Type::Channel(a), Type::Channel(b)) = (self, expected) {
            return a.compatible_with(b);
        }
        if let (Type::TaskHandle(a), Type::TaskHandle(b)) = (self, expected) {
            return a.compatible_with(b);
        }
        // v0.x: Dict<K1, V1> 兼容 Dict<K2, V2> 当 K 兼容且 V 兼容
        if let (Type::Dict(k1, v1), Type::Dict(k2, v2)) = (self, expected) {
            return k1.compatible_with(k2) && v1.compatible_with(v2);
        }
        if matches!(self, Type::Nil) && matches!(expected, Type::Nil) {
            return true;
        }
        if matches!(self, Type::Nil) || matches!(expected, Type::Nil) {
            return false;
        }
        if let (
            Type::Concrete {
                name: a,
                generics: ga,
                ..
            },
            Type::Concrete {
                name: b,
                generics: gb,
                ..
            },
        ) = (self, expected)
        {
            if a != b || ga.len() != gb.len() {
                return false;
            }
            return ga.iter().zip(gb.iter()).all(|(x, y)| x.compatible_with(y));
        }
        if let (
            Type::Trait {
                name: a,
                generics: ga,
            },
            Type::Trait {
                name: b,
                generics: gb,
            },
        ) = (self, expected)
        {
            if a != b || ga.len() != gb.len() {
                return false;
            }
            for (x, y) in ga.iter().zip(gb.iter()) {
                if !x.compatible_with(y) {
                    return false;
                }
            }
            return true;
        }
        if let (Type::Arrow(i1, o1, r1), Type::Arrow(i2, o2, r2)) = (self, expected) {
            return i1.compatible_with(i2)
                && o1.compatible_with(o2)
                && (effect_row_subsumes(r1, r2) || effect_row_subsumes(r2, r1));
        }
        if (matches!(self, Type::Int) && matches!(expected, Type::Float))
            || (matches!(self, Type::Float) && matches!(expected, Type::Int))
        {
            return true;
        }
        if let (Type::Cons(h1, t1), Type::Cons(h2, t2)) = (self, expected) {
            return h1.compatible_with(h2) && t1.compatible_with(t2);
        }
        if let (Type::TraitObject { .. }, Type::TraitObject { .. }) = (self, expected) {
            return true;
        }
        if let (Type::GpuKernel(a), Type::GpuKernel(b)) = (self, expected) {
            return match (a, b) {
                (None, _) | (_, None) => true,
                (Some(pa), Some(pb)) => pa == pb,
            };
        }
        self == expected
    }

    /// Asymmetric subtyping `self <: super_ty` (strict: `Int <: Float`, not `Float <: Int`).
    pub fn subtype_of(&self, super_ty: &Type) -> bool {
        if matches!(self, Type::Error) || matches!(super_ty, Type::Error) {
            return true;
        }
        // Fail-closed sentinel: Unknown never participates in subtyping.
        if matches!(self, Type::Unknown) || matches!(super_ty, Type::Unknown) {
            return false;
        }
        if matches!(self, Type::Any) || matches!(super_ty, Type::Any) {
            return true;
        }
        if matches!(self, Type::TypeVar(TypeVarId::Meta(_)))
            || matches!(super_ty, Type::TypeVar(TypeVarId::Meta(_)))
        {
            return true;
        }
        if let Type::ForAll(_, inner) = self {
            return inner.subtype_of(super_ty);
        }
        if let Type::ForAll(_, inner) = super_ty {
            return self.subtype_of(inner);
        }
        if let Type::Union(members) = self {
            if members.is_empty() {
                return true;
            }
            return members.iter().any(|m| m.subtype_of(super_ty));
        }
        if let Type::Union(members) = super_ty {
            if members.is_empty() {
                return true;
            }
            if matches!(self, Type::Scalar(_))
                && members.contains(&Type::Int)
                && members.contains(&Type::Float)
            {
                return true;
            }
            return members.iter().any(|m| self.subtype_of(m));
        }
        if let (Type::TypeVar(TypeVarId::Rigid(a)), Type::TypeVar(TypeVarId::Rigid(b))) =
            (self, super_ty)
        {
            return a == b;
        }
        if matches!(self, Type::TypeVar(TypeVarId::Rigid(_)))
            || matches!(super_ty, Type::TypeVar(TypeVarId::Rigid(_)))
        {
            return false;
        }
        if let (
            Type::Tensor {
                dtype: d1,
                shape: s1,
                device: dev1,
            },
            Type::Tensor {
                dtype: d2,
                shape: s2,
                device: dev2,
            },
        ) = (self, super_ty)
        {
            let dtype_ok = (d1.subtype_of(d2) && d2.subtype_of(d1))
                || matches!(**d1, Type::Any | Type::TypeVar(TypeVarId::Meta(_)))
                || matches!(**d2, Type::Any | Type::TypeVar(TypeVarId::Meta(_)));
            return dtype_ok && s1.compatible_with(s2) && dev1.compatible_with(dev2);
        }
        // Result<T, E> is covariant in both ok (T) and err (E) variants.
        if let (Type::Result_(t1, e1), Type::Result_(t2, e2)) = (self, super_ty) {
            return t1.subtype_of(t2) && e1.subtype_of(e2);
        }
        if let (Type::Tuple(a), Type::Tuple(b)) = (self, super_ty) {
            if a.len() != b.len() {
                return false;
            }
            for (ta, tb) in a.iter().zip(b.iter()) {
                if !ta.subtype_of(tb) {
                    return false;
                }
            }
            return true;
        }
        if let (Type::List(a), Type::List(b)) = (self, super_ty) {
            return a.subtype_of(b);
        }
        // Channel<T> is INVARIANT in T because channels both receive and send values.
        if let (Type::Channel(a), Type::Channel(b)) = (self, super_ty) {
            return a.subtype_of(b) && b.subtype_of(a);
        }
        // TaskHandle<T> is COVARIANT in T because join() only reads the produced result.
        if let (Type::TaskHandle(a), Type::TaskHandle(b)) = (self, super_ty) {
            return a.subtype_of(b);
        }
        if let (Type::Dict(k1, v1), Type::Dict(k2, v2)) = (self, super_ty) {
            return k1.subtype_of(k2) && v1.subtype_of(v2);
        }
        match (self, super_ty) {
            (Type::Nil, Type::Nil) => return true,
            (Type::Nil, _) | (_, Type::Nil) => return false,
            _ => {}
        }
        if let (
            Type::Concrete {
                name: a,
                generics: ga,
                ..
            },
            Type::Concrete {
                name: b,
                generics: gb,
                ..
            },
        ) = (self, super_ty)
        {
            if a != b || ga.len() != gb.len() {
                return false;
            }
            return ga.iter().zip(gb.iter()).all(|(x, y)| x.subtype_of(y));
        }
        if let (Type::Concrete { traits, .. }, Type::Trait { .. }) = (self, super_ty) {
            return traits.iter().any(|t| t.subtype_of(super_ty));
        }
        if let (
            Type::TraitObject {
                trait_name: tn1,
                generics: g1,
            },
            Type::Trait {
                name: n2,
                generics: g2,
            },
        ) = (self, super_ty)
        {
            if tn1 != n2 || g1.len() != g2.len() {
                return false;
            }
            return g1.iter().zip(g2.iter()).all(|(a, b)| a.subtype_of(b));
        }
        if let (
            Type::Trait {
                name: a,
                generics: ga,
            },
            Type::Trait {
                name: b,
                generics: gb,
            },
        ) = (self, super_ty)
        {
            if a != b || ga.len() != gb.len() {
                return false;
            }
            return ga.iter().zip(gb.iter()).all(|(x, y)| x.subtype_of(y));
        }
        // Function subtyping: contravariant parameter, covariant return, subsumed effect row.
        if let (Type::Arrow(i1, o1, r1), Type::Arrow(i2, o2, r2)) = (self, super_ty) {
            return i2.subtype_of(i1) && o1.subtype_of(o2) && effect_row_subsumes(r1, r2);
        }
        // Int widens cleanly to Float; Float does not narrow to Int.
        if matches!(self, Type::Int) && matches!(super_ty, Type::Float) {
            return true;
        }
        if let (Type::Cons(h1, t1), Type::Cons(h2, t2)) = (self, super_ty) {
            return h1.subtype_of(h2) && t1.subtype_of(t2);
        }
        if let (Type::GpuKernel(a), Type::GpuKernel(b)) = (self, super_ty) {
            return match (a, b) {
                (_, None) | (None, _) => true,
                (Some(pa), Some(pb)) => pa == pb,
            };
        }
        self == super_ty
    }
}

fn effect_row_tail(row: &crate::mir::effect::EffectRow) -> Option<&str> {
    let mut cur = row;
    loop {
        match cur {
            crate::mir::effect::EffectRow::Empty => return None,
            crate::mir::effect::EffectRow::Var(v) => return Some(v.as_str()),
            crate::mir::effect::EffectRow::Cons(_, rest) => cur = rest,
        }
    }
}

/// True iff every effect possibly performed by `sub` is permitted by `sup`.
pub fn effect_row_subsumes(
    sub: &crate::mir::effect::EffectRow,
    sup: &crate::mir::effect::EffectRow,
) -> bool {
    let sup_tail = effect_row_tail(sup);
    if let Some(sub_var) = effect_row_tail(sub)
        && sup_tail != Some(sub_var)
    {
        return false;
    }
    let sup_labels = sup.labels();
    sub.labels()
        .iter()
        .all(|lbl| sup_labels.contains(lbl) || sup_tail.is_some())
}

/// v0.13: 判断类型是否是空 Union (即原 Any 占位)
pub fn is_empty_union(ty: &Type) -> bool {
    matches!(ty, Type::Union(m) if m.is_empty())
}

/// 检查是否是已知的内置类型名（大小写不敏感）
pub fn is_known_type(name: &str) -> bool {
    let lower = name.to_lowercase();
    matches!(
        lower.as_str(),
        "string"
            | "char"
            | "int"
            | "i4"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "float"
            | "f16"
            | "bf16"
            | "fp8"
            | "f32"
            | "f64"
            | "number"
            | "bool"
            | "nil"
            | "list"
            | "dict"
            | "tensor"
            | "task"
            | "closure"
            | "result"
            | "atom"
            | "compose"
            | "partial"
            | "macro"
            | "any"
    ) || lower.starts_with("list<")
        || lower.starts_with("dict<")
        || lower.starts_with("tensor<")
        || lower.starts_with("result<")
        || lower.starts_with("dyn ")
}

/// v0.x: 在顶层（不进入嵌套 `<...>`）按 ',' 分割字符串。
/// 返回 `Some((head, tail))`；若找不到顶层 ',' 则 `None`。
/// 例：`"string, list<int>"` → `Some(("string", " list<int>"))`
fn split_top_level_comma(s: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => return Some((&s[..i], &s[i + 1..])),
            _ => {}
        }
    }
    None
}

/// 类型错误 + 位置 + 修复建议（v0.05）
#[derive(Debug, Clone, PartialEq)]
pub struct TypeError {
    pub line: usize,
    pub column: usize,
    pub message: String,
    /// 期望的类型（可选）
    pub expected: Option<String>,
    /// 实际的类型（可选）
    pub actual: Option<String>,
    /// 修复建议（可选）
    pub hint: Option<String>,
}

impl TypeError {
    pub fn new(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            column: 0,
            message: message.into(),
            expected: None,
            actual: None,
            hint: None,
        }
    }

    /// v0.05: 从 Span 构造 (line + column)
    pub fn from_span(span: &Span, message: impl Into<String>) -> Self {
        Self {
            line: span.line,
            column: span.column,
            message: message.into(),
            expected: None,
            actual: None,
            hint: None,
        }
    }

    /// v0.05: 从 Span + 详情构造
    pub fn from_span_with_detail(
        span: &Span,
        message: impl Into<String>,
        expected: impl Into<String>,
        actual: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            line: span.line,
            column: span.column,
            message: message.into(),
            expected: Some(expected.into()),
            actual: Some(actual.into()),
            hint: Some(hint.into()),
        }
    }

    /// 完整构造：定位 + 期望 + 实际 + 修复建议
    pub fn with_detail(
        line: usize,
        message: impl Into<String>,
        expected: impl Into<String>,
        actual: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            line,
            column: 0,
            message: message.into(),
            expected: Some(expected.into()),
            actual: Some(actual.into()),
            hint: Some(hint.into()),
        }
    }

    /// 加修复建议
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// 格式化错误信息（含修复建议）
pub fn format_error(err: &TypeError) -> String {
    // v0.104.6 D126：`line == 0` 表示这条诊断**没有可用的位置**（`Span::default()`），
    // 典型来源是约束求解阶段 —— `Constraint` 不携带 span，于是
    // `unify` 失败时只能报 `span: None`，一路渲染成「line 0」。
    // 「第 0 行」是误导性的（源码没有第 0 行），**宁可诚实说位置未知**。
    //
    // 根治方案（让 `Constraint` 携带 span、从构造点一路带到 unify）触及
    // 泛化/量化逻辑，风险高于本轮的其它改动，已记入 CHANGELOG D126 待办。
    let mut s = if err.line == 0 {
        "Type error (位置未跟踪)".to_string()
    } else if err.column > 0 {
        format!("Type error at line {}:{}", err.line, err.column)
    } else {
        format!("Type error at line {}", err.line)
    };
    s.push_str(&format!(": {}", err.message));
    if let (Some(exp), Some(act)) = (&err.expected, &err.actual) {
        s.push_str(&format!("\n  expected: {}", exp));
        s.push_str(&format!("\n  actual:   {}", act));
    }
    if let Some(hint) = &err.hint {
        s.push_str(&format!("\n  hint:     {}", hint));
    }
    s
}

// ===================================================================
// 符号表
// ===================================================================

/// 多 scope 嵌套的变量类型表
#[derive(Debug, Clone, Default)]
pub struct SymbolTable {
    scopes: Vec<HashMap<String, Type>>,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
        }
    }

    pub fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    pub fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// 当前 scope 定义变量
    pub fn define(&mut self, name: String, ty: Type) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, ty);
        }
    }

    /// 沿作用域链查找；找不到返回空 Union（`any` 占位）
    pub fn lookup(&self, name: &str) -> Type {
        for scope in self.scopes.iter().rev() {
            if let Some(t) = scope.get(name) {
                return t.clone();
            }
        }
        Type::Union(vec![])
    }
}

// ===================================================================
// 类型检查入口
// ===================================================================

// 唯一入口：check_mir::check_program_witnesses（CLI / 运行期 / LSP 共用），
// 跨模块版本为 check_program_witnesses_with_modules。
