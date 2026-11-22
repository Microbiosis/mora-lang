//! v0.110.0: `Value` 的 `Display` 实现 + 深度限制格式化（`fmt_inner`）。
//!
//! 纯格式化逻辑，零 `BuiltinKind` / `Environment` 依赖；`impl` 在子模块内
//! 实现父模块类型（同 crate 允许）。深度上限与自引用环的断开机制见
//! [`DISPLAY_MAX_DEPTH`] 的说明。

use super::Value;

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::String(s) => write!(f, "{}", s),
            Value::Char(c) => write!(f, "{}", c),
            Value::Int(i) => write!(f, "{}", i),
            Value::Scalar(s) => write!(f, "{}", s),
            Value::Float(n) => {
                // v0.36 (P1-3.13): never panic on NaN/Inf — Display must be infallible.
                if n.is_nan() {
                    f.write_str("nan")
                } else if n.is_infinite() {
                    if *n > 0.0 {
                        f.write_str("inf")
                    } else {
                        f.write_str("-inf")
                    }
                } else {
                    // v0.87: 区分 Int/Float — 整数 Float（5.0）显示 "5.0" 而非 "5"，
                    // 与 Language Display 语义一致；带小数的（3.14）原样输出。
                    if n.fract() == 0.0 {
                        write!(f, "{:.1}", n)
                    } else {
                        write!(f, "{}", n)
                    }
                }
            }
            Value::Bool(b) => write!(f, "{}", b),
            Value::Nil => write!(f, "nil"),
            Value::List(items) => {
                // v0.36 (P1-2.7 + P2-3.14): streaming write, no Vec<String> build.
                // Depth-limited via fmt_inner helper below to guard against
                // recursive Value::List / Value::Atom cycles.
                write!(f, "[")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    fmt_inner(f, v, 1)?;
                }
                write!(f, "]")
            }
            Value::Dict(map) => {
                // v0.104.6 可复现性修复：按 key 排序（`Value::Dict` 是
                // `HashMap`，迭代序每进程随机）。与 `flow::json::value_to_json`
                // 及 http/mcp 服务器的 `BTreeMap` 做法对齐。
                //
                // ⚠ v0.104.6 D291：这句话**只对 dict 的键序成立** ——
                // 键序由本实现自己排序保证；**值**的序列化不在这里做。
                // 整条 JSON 路径只有一套内核：`flow::json::value_to_json`
                // 写值（整数写**裸数字**，不加引号），读方向
                // `flow::json::json_to_value` 区分 Int/Float。
                // （LSP 侧复用同一套解析状态机，见 `lsp::json`。）
                write!(f, "{{")?;
                let mut entries: Vec<(&String, &Value)> = map.iter().collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                for (i, (k, v)) in entries.into_iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: ", k)?;
                    fmt_inner(f, v, 1)?;
                }
                write!(f, "}}")
            }
            Value::Task { name, .. } => write!(f, "<task {}>", name),
            Value::Closure { .. } => write!(f, "<closure>"),
            Value::Builtin(name) => write!(f, "<builtin {}>", name),
            Value::TraitObject {
                for_type,
                trait_name,
                for_generics: _,
                trait_generics: _,
                data,
            } => {
                write!(
                    f,
                    "<trait_object for={} as {} data={:?}>",
                    for_type, trait_name, data
                )
            }
            Value::Compose(funcs) => {
                write!(f, "<compose({} funcs)>", funcs.len())
            }
            Value::Partial(_, _) => {
                write!(f, "<partial>")
            }
            Value::Atom(arc) => {
                let v = arc.lock();
                write!(f, "<atom {:?}>", v)
            }
            Value::Macro { name, .. } => {
                write!(f, "<macro {}>", name)
            }
            // v0.86: Curry — 显示为可调用值，包含已绑定参数数量。
            Value::Curry {
                arity, bound_args, ..
            } => {
                write!(f, "<curry arity={} bound={}>", arity, bound_args.len())
            }
            // v0.86: Cons — 显示为 Lisp 风格的 (car . cdr)。
            Value::Cons { car, cdr } => match cdr.as_ref() {
                Value::Nil => write!(f, "({})", car),
                _ => write!(f, "({} . {})", car, cdr),
            },
            // v0.86: Code — 显示为 "code:source_text"，与 eval/quote 配对可读。
            Value::Code(s) => write!(f, "code:{s}"),
            Value::Continuation(_) => write!(f, "<continuation>"),
            Value::External(obj) => obj.display(f),
            Value::Channel(ch) => {
                write!(
                    f,
                    "<channel cap={} len={} closed={}>",
                    ch.capacity(),
                    ch.len(),
                    ch.is_closed()
                )
            }
            Value::TaskHandle(h) => write!(f, "<task_handle done={}>", h.is_done()),
            Value::Context(ctx) => {
                write!(f, "<context cancelled={}>", ctx.status().is_err())
            }
            Value::TcpListener(l) => {
                write!(
                    f,
                    "<tcp_listener addr={} closed={}>",
                    l.local_addr(),
                    l.is_closed()
                )
            }
            Value::TcpStream(s) => {
                write!(
                    f,
                    "<tcp_stream peer={} closed={}>",
                    s.peer_addr(),
                    s.is_closed()
                )
            }
            Value::TlsListener(l) => {
                write!(
                    f,
                    "<tls_listener addr={} closed={}>",
                    l.local_addr(),
                    l.is_closed()
                )
            }
            Value::TlsStream(s) => {
                write!(
                    f,
                    "<tls_stream peer={} proto={} closed={}>",
                    s.peer_addr(),
                    s.tls_version(),
                    s.is_closed()
                )
            }
            Value::ComputeDag(dag) => {
                write!(
                    f,
                    "<compute_dag nodes={} fused={} opt={}>",
                    dag.node_count(),
                    dag.fused_count(),
                    dag.fused
                )
            }
            Value::KVAllocator(kv) => {
                write!(
                    f,
                    "<kv_allocator pages={}/{} dtype={} device={}>",
                    kv.used_pages(),
                    kv.total_pages,
                    kv.dtype.short_name(),
                    kv.device
                )
            }
            Value::MemoryPool(pool) => {
                write!(
                    f,
                    "<memory_pool active={}/{} cached={} device={}>",
                    pool.active_bytes(),
                    pool.max_bytes(),
                    pool.cached_bytes(),
                    pool.device
                )
            }
            Value::GpuKernel(k) => {
                let pstr: Vec<String> = k
                    .params()
                    .iter()
                    .map(|p| format!("{}: {}", p.name, p.ty.canonical_name()))
                    .collect();
                write!(f, "<gpu_kernel {}({})>", k.name(), pstr.join(", "))
            }
            Value::ProcessGroup(pg) => {
                write!(
                    f,
                    "<process_group rank={}/{} backend={}>",
                    pg.rank(),
                    pg.world_size(),
                    pg.backend_name()
                )
            }
            Value::TraceProfiler(p) => {
                write!(
                    f,
                    "<trace_profiler active={} events={}>",
                    p.is_active(),
                    p.event_count()
                )
            }
            Value::Tensor(t) => write!(f, "{t}"),
            Value::StructType(def) => write!(f, "<struct {}>", def.name),
            Value::StructInstance {
                type_name,
                fields,
                field_order,
                ..
            } => {
                write!(f, "{type_name} {{ ")?;
                for (i, fname) in field_order.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{fname}: ")?;
                    if let Some(v) = fields.get(fname) {
                        fmt_inner(f, v, 1)?;
                    } else {
                        write!(f, "nil")?;
                    }
                }
                write!(f, " }}")
            }
            Value::EnumType(def) => write!(f, "<enum {}>", def.name),
            Value::EnumVariant {
                enum_name,
                variant,
                payload,
                ..
            } => {
                if payload.is_empty() {
                    write!(f, "{enum_name}::{variant}")
                } else {
                    write!(f, "{enum_name}::{variant}(")?;
                    for (i, v) in payload.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        fmt_inner(f, v, 1)?;
                    }
                    write!(f, ")")
                }
            }
            Value::GradFn { return_value, .. } => {
                if *return_value {
                    write!(f, "<value_and_grad>")
                } else {
                    write!(f, "<grad>")
                }
            }
            Value::MissingArg => write!(f, "<missing_arg>"),
        }
    }
}

/// v0.36 (P2-3.14): depth-limited Display helper. Walks a Value recursively
/// but stops at `DISPLAY_MAX_DEPTH` (16) to prevent stack overflow on
/// **deeply nested** `List` / `Dict`。
///
/// ⚠ v0.104.6 D393 更正：原文写「prevent stack overflow on
/// recursive/cyclic structures (**e.g. Atom containing self**)」——
/// **该括号内的说法不成立**。
///
/// `Atom` 分支走 `{:?}`，递归由 `crate::common::sync::Mutex` 的 `Debug` 实现完成：
/// 当 `Mutex::Debug` 调用 `self.try_lock()` 发现同一线程已持有锁（如自引用 `Atom`）时，
/// 直接打印 `<locked>` 占位而不再下钻，从而在自引用环上保证确定性终止。
///
/// ⇒ **深度限制（`DISPLAY_MAX_DEPTH`）覆盖 `List`/`Dict`，`try_lock()` 环断开器覆盖 `Atom`**，
/// 判据 `tests/display_atom_and_depth.rs` 持续验证自引用 `Atom` 的终止性与 `<locked>` 输出。
const DISPLAY_MAX_DEPTH: usize = 16;

fn fmt_inner(f: &mut std::fmt::Formatter<'_>, v: &Value, depth: usize) -> std::fmt::Result {
    if depth > DISPLAY_MAX_DEPTH {
        return f.write_str("…");
    }
    match v {
        Value::Atom(arc) => {
            let inner = arc.lock();
            write!(f, "<atom {:?}>", inner)
        }
        Value::List(items) => {
            write!(f, "[")?;
            for (i, child) in items.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                fmt_inner(f, child, depth + 1)?;
            }
            write!(f, "]")
        }
        Value::Dict(map) => {
            write!(f, "{{")?;
            // v0.104.6 可复现性修复：同上方浅层分支，按 key 排序。
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            for (i, (k, child)) in entries.into_iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{}: ", k)?;
                fmt_inner(f, child, depth + 1)?;
            }
            write!(f, "}}")
        }
        _ => write!(f, "{}", v),
    }
}
