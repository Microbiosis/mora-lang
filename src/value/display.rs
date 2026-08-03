//! v0.75.62: Value Display 实现 + fmt_inner 深度限制格式化 — 自 value.rs
//! 拆出（D6 单文件惯例）。纯格式化逻辑，零 BuiltinKind/Environment 依赖；
//! impl 在子模块内实现父模块类型（同 crate 允许）。

use super::Value;

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::String(s) => write!(f, "{}", s),
            Value::Char(c) => write!(f, "{}", c),
            Value::Int(i) => write!(f, "{}", i),
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
                    write!(f, "{}", n)
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
                write!(f, "{{")?;
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: ", k)?;
                    fmt_inner(f, v, 1)?;
                }
                write!(f, "}}")
            }
            Value::Task { name, .. } => write!(f, "<task {}>", name),
            Value::Tool { name, .. } => write!(f, "<tool {}>", name),
            Value::Closure { .. } => write!(f, "<closure>"),
            Value::Builtin(name) => write!(f, "<builtin {}>", name),
            Value::Conversation {
                model, messages, ..
            } => {
                write!(f, "<conversation {} ({} messages)>", model, messages.len())
            }
            Value::Stream { .. } => write!(f, "<stream>"),
            Value::Agent { name, .. } => write!(f, "<agent {}>", name),
            Value::AiConfig {
                model,
                temperature,
                max_tokens,
                system,
                budget,
            } => {
                write!(
                    f,
                    "AiConfig(model={:?}, temp={:?}, max_tokens={:?}, system={:?}, budget={:?})",
                    model, temperature, max_tokens, system, budget
                )
            }
            Value::Router { routes } => {
                // v0.35: Display must be infallible; parking_lot::Mutex does not poison.
                let route_count = routes.lock().len();
                write!(f, "<router ({} routes)>", route_count)
            }
            Value::HttpRequest { method, path, .. } => {
                write!(f, "<http_request {} {}>", method, path)
            }
            Value::McpServer { tools } => write!(f, "<mcp_server ({} tools)>", tools.len()),
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
                // v0.35: Display must be infallible; parking_lot::Mutex does not poison.
                let v = arc.lock();
                write!(f, "<atom {:?}>", v)
            }
            Value::Macro { name, .. } => {
                write!(f, "<macro {}>", name)
            }
            Value::PromptSection {
                name,
                role,
                budget_bytes,
                ..
            } => {
                write!(
                    f,
                    "<prompt_section name={} role={:?} budget={:?}>",
                    name, role, budget_bytes
                )
            }
            Value::Document { backend, .. } => {
                write!(f, "<document origin=\"{}\">", backend.origin())
            }
        }
    }
}

/// v0.36 (P2-3.14): depth-limited Display helper. Walks a Value recursively
/// but stops at MAX_DEPTH (default 16) to prevent stack overflow on
/// recursive/cyclic structures (e.g. Atom containing self).
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
            for (i, (k, child)) in map.iter().enumerate() {
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
