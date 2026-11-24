//! v0.110.0: Stackful builtin, method, callable-value, and dynamic-lookup resolution.
//!
//! Pure builtins and value methods delegate directly to the canonical shared helpers in
//! `crate::runtime::builtins::{impls, method, traits}`. Higher-order combinators, closures,
//! continuations, ambient random effects, `eval`, and `macroexpand` yield [`CallAction`] descriptors
//! executed iteratively by [`super::engine::VmEngine`] on its explicit heap call stack.
//!
//! Every stackful call is expressed as a [`CallAction`] descriptor rather than as a nested
//! Rust call, so resolution itself never grows the C stack.

use std::collections::HashMap;
use std::sync::Arc;

use crate::common::Span;
use crate::error::{MoraError, MoraResult};
use crate::mir::control::BuiltinFnId;
use crate::mir::effect::ambient;
use crate::runtime::CoreRuntime;
use crate::runtime::builtins::impls::eval_pure_builtin;
use crate::runtime::builtins::method::eval_pure_method;
use crate::runtime::builtins::traits::{
    TraitMethodTarget, construct_trait_instance_core, resolve_concrete_impl_method,
    resolve_trait_method_call,
};
use crate::value::{BuiltinKind, MODULE_OBJECTS, StructDefRuntime, Value};

use super::continuation::{CombinatorFrame, ScriptFrame, materialize_frame_named};
use std::future::Future;
use std::pin::Pin;

/// P4-2: Boxed `Send` future awaited cooperatively by `VmEngine` without blocking OS worker threads.
pub type VmFuture = Pin<Box<dyn Future<Output = MoraResult<Value>> + Send>>;

/// Deferred or stackful call dispatch action returned by builtin/callee resolution.
pub enum CallAction {
    /// Immediate synchronous return value.
    Done(Value),
    /// P4-2: Suspend the VM call stack cooperatively until the async operation resolves.
    AwaitFuture(VmFuture),
    /// Push a compiled `ScriptFrame` (optionally wrapping propagated errors with `error_prefix`).
    PushFrame {
        frame: ScriptFrame,
        error_prefix: Option<String>,
    },
    /// Push a [`CombinatorFrame`] and invoke its initial callback target + args.
    PushCombinator {
        combinator: Box<CombinatorFrame>,
        callee: Value,
        args: Vec<Value>,
    },
    /// Dynamically compile and evaluate Mora source inside an isolated child `Environment`.
    EvalSource { source: String },
    /// Expand and evaluate a named macro against argument list.
    MacroExpand { name: String, args: Vec<Value> },
    /// Perform an ambient `random.*` effect (intercepted by active `HandleBarrier` or `Xoshiro256`).
    AmbientRandom {
        label: &'static str,
        method: String,
        args: Vec<Value>,
    },
    /// Resume a captured delimited [`CapturedContinuation`] with a single reply value.
    ResumeContinuation {
        cont: Arc<super::continuation::CapturedContinuation>,
        val: Value,
    },
}

/// Map a runtime [`BuiltinKind`] value back to its [`BuiltinFnId`] when it names a free function.
pub fn builtin_kind_to_fn_id(kind: BuiltinKind) -> Option<BuiltinFnId> {
    match kind {
        BuiltinKind::Print => Some(BuiltinFnId::Print),
        BuiltinKind::Range => Some(BuiltinFnId::Range),
        BuiltinKind::Len => Some(BuiltinFnId::Len),
        BuiltinKind::Eval => Some(BuiltinFnId::Eval),
        BuiltinKind::Tail => Some(BuiltinFnId::Tail),
        BuiltinKind::FreeFn(name) => BuiltinFnId::from_name(name),
        BuiltinKind::Json
        | BuiltinKind::File
        | BuiltinKind::Exec
        | BuiltinKind::Math
        | BuiltinKind::Stats
        | BuiltinKind::Linalg
        | BuiltinKind::Random => None,
    }
}

/// Resolve a built-in free function call by [`BuiltinFnId`].
pub fn resolve_builtin_call(
    id: BuiltinFnId,
    core: &mut CoreRuntime,
    args: Vec<Value>,
) -> MoraResult<CallAction> {
    resolve_builtin_call_named(id, core, args, Vec::new())
}

/// Resolve a built-in free function call with optional named keyword arguments.
pub fn resolve_builtin_call_named(
    id: BuiltinFnId,
    core: &mut CoreRuntime,
    mut args: Vec<Value>,
    named_args: Vec<(String, Value)>,
) -> MoraResult<CallAction> {
    if !named_args.is_empty() {
        match id {
            BuiltinFnId::Tensor => {
                return crate::value::tensor::call_builtin_tensor_named(&args, &named_args)
                    .map(CallAction::Done);
            }
            BuiltinFnId::Grad | BuiltinFnId::ValueAndGrad => {
                let mut fn_slot = args.first().cloned();
                let mut argnums_slot = args.get(1).cloned();
                for (k, v) in named_args {
                    match k.as_str() {
                        "fn" | "f" | "func" => {
                            if fn_slot.is_some() {
                                return Err(MoraError::Runtime(format!(
                                    "{}(): duplicate argument '{k}'",
                                    id.name()
                                )));
                            }
                            fn_slot = Some(v);
                        }
                        "argnums" => {
                            if argnums_slot.is_some() {
                                return Err(MoraError::Runtime(format!(
                                    "{}(): duplicate argument 'argnums'",
                                    id.name()
                                )));
                            }
                            argnums_slot = Some(v);
                        }
                        other => {
                            return Err(MoraError::Runtime(format!(
                                "{}(): unexpected keyword argument '{other}'",
                                id.name()
                            )));
                        }
                    }
                }
                args.clear();
                if let Some(f) = fn_slot {
                    args.push(f);
                }
                if let Some(an) = argnums_slot {
                    args.push(an);
                }
            }
            BuiltinFnId::TcpListen => {
                let mut addr_slot = args.first().cloned();
                for (k, v) in named_args {
                    if k == "addr" {
                        if addr_slot.is_some() {
                            return Err(MoraError::Runtime(
                                "tcp_listen(): duplicate argument 'addr'".to_string(),
                            ));
                        }
                        addr_slot = Some(v);
                    } else {
                        return Err(MoraError::Runtime(format!(
                            "tcp_listen(): unexpected keyword argument '{k}'"
                        )));
                    }
                }
                args.clear();
                if let Some(a) = addr_slot {
                    args.push(a);
                }
            }
            BuiltinFnId::TcpConnect => {
                let mut addr_slot = args.first().cloned();
                let mut timeout_slot = args.get(1).cloned();
                let mut ctx_slot = args.get(2).cloned();
                for (k, v) in named_args {
                    match k.as_str() {
                        "addr" => {
                            if addr_slot.is_some() {
                                return Err(MoraError::Runtime(
                                    "tcp_connect(): duplicate argument 'addr'".to_string(),
                                ));
                            }
                            addr_slot = Some(v);
                        }
                        "timeout_ms" | "timeout" => {
                            if timeout_slot.is_some() {
                                return Err(MoraError::Runtime(
                                    "tcp_connect(): duplicate argument 'timeout_ms'".to_string(),
                                ));
                            }
                            timeout_slot = Some(v);
                        }
                        "ctx" | "context" => {
                            if ctx_slot.is_some() {
                                return Err(MoraError::Runtime(
                                    "tcp_connect(): duplicate argument 'ctx'".to_string(),
                                ));
                            }
                            ctx_slot = Some(v);
                        }
                        other => {
                            return Err(MoraError::Runtime(format!(
                                "tcp_connect(): unexpected keyword argument '{other}'"
                            )));
                        }
                    }
                }
                args.clear();
                if let Some(a) = addr_slot {
                    args.push(a);
                }
                if timeout_slot.is_some() || ctx_slot.is_some() {
                    args.push(timeout_slot.unwrap_or(Value::Nil));
                }
                if let Some(c) = ctx_slot {
                    args.push(c);
                }
            }
            BuiltinFnId::TlsListen => {
                let mut addr_slot = args.first().cloned();
                let mut cfg_slot = args.get(1).cloned();
                for (k, v) in named_args {
                    match k.as_str() {
                        "addr" => {
                            if addr_slot.is_some() {
                                return Err(MoraError::Runtime(
                                    "tls_listen(): duplicate argument 'addr'".to_string(),
                                ));
                            }
                            addr_slot = Some(v);
                        }
                        "config" => {
                            if cfg_slot.is_some() {
                                return Err(MoraError::Runtime(
                                    "tls_listen(): duplicate argument 'config'".to_string(),
                                ));
                            }
                            cfg_slot = Some(v);
                        }
                        other => {
                            return Err(MoraError::Runtime(format!(
                                "tls_listen(): unexpected keyword argument '{other}'"
                            )));
                        }
                    }
                }
                args.clear();
                if let Some(a) = addr_slot {
                    args.push(a);
                }
                if let Some(c) = cfg_slot {
                    args.push(c);
                }
            }
            BuiltinFnId::TlsConnect => {
                let mut addr_slot = args.first().cloned();
                let mut sni_slot = args.get(1).cloned();
                let mut cfg_slot = args.get(2).cloned();
                let mut timeout_slot = args.get(3).cloned();
                let mut ctx_slot = args.get(4).cloned();
                for (k, v) in named_args {
                    match k.as_str() {
                        "addr" => {
                            if addr_slot.is_some() {
                                return Err(MoraError::Runtime(
                                    "tls_connect(): duplicate argument 'addr'".to_string(),
                                ));
                            }
                            addr_slot = Some(v);
                        }
                        "server_name" | "sni" => {
                            if sni_slot.is_some() {
                                return Err(MoraError::Runtime(
                                    "tls_connect(): duplicate argument 'server_name'".to_string(),
                                ));
                            }
                            sni_slot = Some(v);
                        }
                        "config" => {
                            if cfg_slot.is_some() {
                                return Err(MoraError::Runtime(
                                    "tls_connect(): duplicate argument 'config'".to_string(),
                                ));
                            }
                            cfg_slot = Some(v);
                        }
                        "timeout_ms" | "timeout" => {
                            if timeout_slot.is_some() {
                                return Err(MoraError::Runtime(
                                    "tls_connect(): duplicate argument 'timeout_ms'".to_string(),
                                ));
                            }
                            timeout_slot = Some(v);
                        }
                        "ctx" | "context" => {
                            if ctx_slot.is_some() {
                                return Err(MoraError::Runtime(
                                    "tls_connect(): duplicate argument 'ctx'".to_string(),
                                ));
                            }
                            ctx_slot = Some(v);
                        }
                        other => {
                            return Err(MoraError::Runtime(format!(
                                "tls_connect(): unexpected keyword argument '{other}'"
                            )));
                        }
                    }
                }
                args.clear();
                if let Some(a) = addr_slot {
                    args.push(a);
                }
                if sni_slot.is_some()
                    || cfg_slot.is_some()
                    || timeout_slot.is_some()
                    || ctx_slot.is_some()
                {
                    args.push(sni_slot.unwrap_or(Value::Nil));
                }
                if cfg_slot.is_some() || timeout_slot.is_some() || ctx_slot.is_some() {
                    args.push(cfg_slot.unwrap_or(Value::Nil));
                }
                if timeout_slot.is_some() || ctx_slot.is_some() {
                    args.push(timeout_slot.unwrap_or(Value::Nil));
                }
                if let Some(c) = ctx_slot {
                    args.push(c);
                }
            }
            BuiltinFnId::KvAllocator => {
                let names = [
                    "total_pages",
                    "page_tokens",
                    "num_heads",
                    "head_dim",
                    "dtype",
                    "device",
                    "max_bytes",
                ];
                let mut slots: Vec<Option<Value>> = vec![None; names.len()];
                if args.len() > names.len() {
                    return Err(MoraError::Runtime(format!(
                        "kv_allocator(): expected at most {} positional args, got {}",
                        names.len(),
                        args.len()
                    )));
                }
                for (i, v) in args.into_iter().enumerate() {
                    slots[i] = Some(v);
                }
                for (k, v) in named_args {
                    let Some(pos) = names.iter().position(|&n| n == k.as_str()) else {
                        return Err(MoraError::Runtime(format!(
                            "kv_allocator(): unexpected keyword argument '{k}'"
                        )));
                    };
                    if slots[pos].is_some() {
                        return Err(MoraError::Runtime(format!(
                            "kv_allocator(): duplicate argument '{k}'"
                        )));
                    }
                    slots[pos] = Some(v);
                }
                while matches!(slots.last(), Some(None)) {
                    slots.pop();
                }
                args = slots.into_iter().map(|s| s.unwrap_or(Value::Nil)).collect();
            }
            BuiltinFnId::MemoryPool => {
                let names = ["max_bytes", "device"];
                let mut slots: Vec<Option<Value>> = vec![None; names.len()];
                if args.len() > names.len() {
                    return Err(MoraError::Runtime(format!(
                        "memory_pool(): expected at most {} positional args, got {}",
                        names.len(),
                        args.len()
                    )));
                }
                for (i, v) in args.into_iter().enumerate() {
                    slots[i] = Some(v);
                }
                for (k, v) in named_args {
                    let Some(pos) = names.iter().position(|&n| n == k.as_str()) else {
                        return Err(MoraError::Runtime(format!(
                            "memory_pool(): unexpected keyword argument '{k}'"
                        )));
                    };
                    if slots[pos].is_some() {
                        return Err(MoraError::Runtime(format!(
                            "memory_pool(): duplicate argument '{k}'"
                        )));
                    }
                    slots[pos] = Some(v);
                }
                while matches!(slots.last(), Some(None)) {
                    slots.pop();
                }
                args = slots.into_iter().map(|s| s.unwrap_or(Value::Nil)).collect();
            }
            BuiltinFnId::Jvp => {
                let mut slots: Vec<Option<Value>> = vec![None; 3];
                if args.len() > 3 {
                    return Err(MoraError::Runtime(format!(
                        "jvp(): expected 3 arguments, got {}",
                        args.len()
                    )));
                }
                for (i, v) in args.into_iter().enumerate() {
                    slots[i] = Some(v);
                }
                for (k, v) in named_args {
                    let pos = match k.as_str() {
                        "f" | "fn" | "func" => 0,
                        "primals" | "primal" | "x" => 1,
                        "tangents" | "tangent" | "v" => 2,
                        _ => {
                            return Err(MoraError::Runtime(format!(
                                "jvp(): unexpected keyword argument '{k}'"
                            )));
                        }
                    };
                    if slots[pos].is_some() {
                        return Err(MoraError::Runtime(format!(
                            "jvp(): duplicate argument '{k}'"
                        )));
                    }
                    slots[pos] = Some(v);
                }
                args = slots.into_iter().map(|s| s.unwrap_or(Value::Nil)).collect();
            }
            _ => {
                return Err(MoraError::Runtime(format!(
                    "{}(): builtin does not accept keyword arguments (got '{}')",
                    id.name(),
                    named_args[0].0
                )));
            }
        }
    }
    if let Some(res) = eval_pure_builtin(id, core, &args) {
        return res.map(CallAction::Done);
    }

    match id {
        BuiltinFnId::Swap => {
            if args.len() < 2 {
                return Err(MoraError::Runtime(
                    "swap() requires 2 arguments: atom and function".to_string(),
                ));
            }
            match &args[0] {
                Value::Atom(arc) => {
                    let old = arc.lock().clone();
                    Ok(CallAction::PushCombinator {
                        combinator: Box::new(CombinatorFrame::Swap {
                            atom: Arc::clone(arc),
                        }),
                        callee: args[1].clone(),
                        args: vec![old],
                    })
                }
                _ => Err(MoraError::Runtime(
                    "swap() first argument must be an atom".to_string(),
                )),
            }
        }
        BuiltinFnId::Into => {
            if args.len() < 2 {
                return Err(MoraError::Runtime(
                    "into() requires 2 arguments: collection and function".to_string(),
                ));
            }
            let transform = args[1].clone();
            match &args[0] {
                Value::List(list) => {
                    if list.is_empty() {
                        return Ok(CallAction::Done(Value::List(Vec::new().into())));
                    }
                    let items: Vec<Value> = list.iter().cloned().collect();
                    let first = items[0].clone();
                    Ok(CallAction::PushCombinator {
                        combinator: Box::new(CombinatorFrame::Into {
                            transform: transform.clone(),
                            items,
                            idx: 0,
                            results: Vec::new(),
                        }),
                        callee: transform,
                        args: vec![first],
                    })
                }
                _ => Err(MoraError::Runtime(
                    "into() first argument must be a list".to_string(),
                )),
            }
        }
        BuiltinFnId::Apply => {
            if args.len() < 2 {
                return Err(MoraError::Runtime(
                    "apply(fn, [args...]) expects at least 2 arguments".to_string(),
                ));
            }
            let fn_val = args[0].clone();
            let expanded = match &args[1] {
                Value::List(items) => items.iter().cloned().collect(),
                other => vec![other.clone()],
            };
            resolve_value_call(fn_val, expanded, core)
        }
        BuiltinFnId::Eval => {
            let source = match args.first() {
                Some(Value::String(s)) | Some(Value::Code(s)) => s.clone(),
                _ => {
                    return Err(MoraError::Runtime(
                        "eval(source: string|code) expects a string or code argument".to_string(),
                    ));
                }
            };
            Ok(CallAction::EvalSource { source })
        }
        BuiltinFnId::Macroexpand => {
            let name = match args.first() {
                Some(Value::String(s)) => s.clone(),
                _ => {
                    return Err(MoraError::Runtime(
                        "macroexpand(name: string, args...) expects a string name".to_string(),
                    ));
                }
            };
            let expr_args = if args.len() > 1 {
                match &args[1] {
                    Value::List(items) => items.iter().cloned().collect(),
                    other => vec![other.clone()],
                }
            } else {
                Vec::new()
            };
            Ok(CallAction::MacroExpand {
                name,
                args: expr_args,
            })
        }
        BuiltinFnId::Sleep => {
            let ms_val = args.first().ok_or_else(|| {
                MoraError::Runtime("sleep(ms) requires 1 numeric argument".into())
            })?;
            let duration = crate::runtime::builtins::duration_from_ms(ms_val, "sleep")?;
            let reactor = Arc::clone(core.reactor());
            let ctx = core.task_context.clone();
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                reactor.sleep(duration, ctx.as_ref()).await?;
                Ok(Value::Nil)
            })))
        }
        BuiltinFnId::TcpListen => {
            if args.len() != 1 {
                return Err(MoraError::Runtime(format!(
                    "tcp_listen(addr) expects 1 argument, got {}",
                    args.len()
                )));
            }
            let Value::String(addr) = &args[0] else {
                return Err(MoraError::Runtime(format!(
                    "tcp_listen(addr): expected string address, got {}",
                    crate::flow::type_name(&args[0])
                )));
            };
            if let Some(c) = core.task_context.as_ref() {
                c.status()?;
            }
            let addr = addr.clone();
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                let listener = crate::runtime::reactor::MoraTcpListener::bind(&addr).await?;
                Ok(Value::TcpListener(listener))
            })))
        }
        BuiltinFnId::TcpConnect => {
            if args.is_empty() || args.len() > 3 {
                return Err(MoraError::Runtime(format!(
                    "tcp_connect(addr, timeout_ms?, ctx?) expects 1..=3 arguments, got {}",
                    args.len()
                )));
            }
            let Value::String(addr) = &args[0] else {
                return Err(MoraError::Runtime(format!(
                    "tcp_connect(addr, ...): expected string address, got {}",
                    crate::flow::type_name(&args[0])
                )));
            };
            let timeout = match args.get(1) {
                None | Some(Value::Nil) => None,
                Some(v) => Some(crate::runtime::builtins::duration_from_ms(
                    v,
                    "tcp_connect",
                )?),
            };
            let explicit_ctx = match args.get(2) {
                None | Some(Value::Nil) => None,
                Some(Value::Context(c)) => Some(c.clone()),
                Some(other) => {
                    return Err(MoraError::Runtime(format!(
                        "tcp_connect(..., ctx): expected context, got {}",
                        crate::flow::type_name(other)
                    )));
                }
            };
            let addr = addr.clone();
            let eff_ctx = explicit_ctx.or_else(|| core.task_context.clone());
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                let stream = crate::runtime::reactor::MoraTcpStream::connect(
                    &addr,
                    timeout,
                    eff_ctx.as_ref(),
                )
                .await?;
                Ok(Value::TcpStream(stream))
            })))
        }
        BuiltinFnId::TlsListen => {
            if args.len() != 2 {
                return Err(MoraError::Runtime(format!(
                    "tls_listen(addr, config) expects 2 arguments, got {}",
                    args.len()
                )));
            }
            let Value::String(addr) = &args[0] else {
                return Err(MoraError::Runtime(format!(
                    "tls_listen(addr, config): expected string address, got {}",
                    crate::flow::type_name(&args[0])
                )));
            };
            let Value::Dict(cfg_map) = &args[1] else {
                return Err(MoraError::Runtime(format!(
                    "tls_listen(addr, config): expected config dict, got {}",
                    crate::flow::type_name(&args[1])
                )));
            };
            let cert_pem = match cfg_map.get("cert_pem") {
                Some(Value::String(s)) => s.clone(),
                _ => {
                    return Err(MoraError::Runtime(
                        "tls_listen(addr, config): config dict requires string 'cert_pem'"
                            .to_string(),
                    ));
                }
            };
            let key_pem = match cfg_map.get("key_pem") {
                Some(Value::String(s)) => s.clone(),
                _ => {
                    return Err(MoraError::Runtime(
                        "tls_listen(addr, config): config dict requires string 'key_pem'"
                            .to_string(),
                    ));
                }
            };
            let alpn_protocols = parse_tls_alpn_opt(cfg_map.get("alpn"), "tls_listen")?;
            if let Some(c) = core.task_context.as_ref() {
                c.status()?;
            }
            let cfg = crate::runtime::tls::TlsServerConfig {
                cert_pem,
                key_pem,
                alpn_protocols,
            };
            let addr = addr.clone();
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                let listener = crate::runtime::tls::MoraTlsListener::bind(&addr, &cfg).await?;
                Ok(Value::TlsListener(listener))
            })))
        }
        BuiltinFnId::TlsConnect => {
            if args.is_empty() || args.len() > 5 {
                return Err(MoraError::Runtime(format!(
                    "tls_connect(addr, server_name?, config?, timeout_ms?, ctx?) expects 1..=5 arguments, got {}",
                    args.len()
                )));
            }
            let Value::String(addr) = &args[0] else {
                return Err(MoraError::Runtime(format!(
                    "tls_connect(addr, ...): expected string address, got {}",
                    crate::flow::type_name(&args[0])
                )));
            };
            let mut server_name = match args.get(1) {
                None | Some(Value::Nil) => String::new(),
                Some(Value::String(s)) => s.clone(),
                Some(other) => {
                    return Err(MoraError::Runtime(format!(
                        "tls_connect(..., server_name): expected string or nil, got {}",
                        crate::flow::type_name(other)
                    )));
                }
            };
            let mut ca_pem: Option<String> = None;
            let mut alpn_protocols: Vec<String> = Vec::new();
            let mut insecure_skip_verify = false;
            match args.get(2) {
                None | Some(Value::Nil) => {}
                Some(Value::Dict(cfg_map)) => {
                    if let Some(v) = cfg_map.get("ca_pem") {
                        match v {
                            Value::Nil => {}
                            Value::String(s) if !s.is_empty() => ca_pem = Some(s.clone()),
                            Value::String(_) => {}
                            other => {
                                return Err(MoraError::Runtime(format!(
                                    "tls_connect: config.ca_pem expected string or nil, got {}",
                                    crate::flow::type_name(other)
                                )));
                            }
                        }
                    }
                    if let Some(v) = cfg_map.get("server_name")
                        && server_name.is_empty()
                    {
                        if let Value::String(s) = v {
                            server_name = s.clone();
                        } else if !matches!(v, Value::Nil) {
                            return Err(MoraError::Runtime(format!(
                                "tls_connect: config.server_name expected string, got {}",
                                crate::flow::type_name(v)
                            )));
                        }
                    }
                    alpn_protocols = parse_tls_alpn_opt(cfg_map.get("alpn"), "tls_connect")?;
                    if let Some(v) = cfg_map.get("insecure_skip_verify") {
                        match v {
                            Value::Bool(b) => insecure_skip_verify = *b,
                            Value::Nil => {}
                            other => {
                                return Err(MoraError::Runtime(format!(
                                    "tls_connect: config.insecure_skip_verify expected bool, got {}",
                                    crate::flow::type_name(other)
                                )));
                            }
                        }
                    }
                }
                Some(other) => {
                    return Err(MoraError::Runtime(format!(
                        "tls_connect(..., config): expected dict or nil, got {}",
                        crate::flow::type_name(other)
                    )));
                }
            }
            if server_name.is_empty() {
                server_name = extract_host_from_addr(addr);
            }
            let timeout = match args.get(3) {
                None | Some(Value::Nil) => None,
                Some(v) => Some(crate::runtime::builtins::duration_from_ms(
                    v,
                    "tls_connect",
                )?),
            };
            let explicit_ctx = match args.get(4) {
                None | Some(Value::Nil) => None,
                Some(Value::Context(c)) => Some(c.clone()),
                Some(other) => {
                    return Err(MoraError::Runtime(format!(
                        "tls_connect(..., ctx): expected context, got {}",
                        crate::flow::type_name(other)
                    )));
                }
            };
            let addr = addr.clone();
            let eff_ctx = explicit_ctx.or_else(|| core.task_context.clone());
            let cfg = crate::runtime::tls::TlsClientConfig {
                server_name,
                ca_pem,
                alpn_protocols,
                insecure_skip_verify,
            };
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                let stream = crate::runtime::tls::MoraTlsStream::connect(
                    &addr,
                    &cfg,
                    timeout,
                    eff_ctx.as_ref(),
                )
                .await?;
                Ok(Value::TlsStream(stream))
            })))
        }
        BuiltinFnId::Jvp => {
            if args.len() != 3 {
                return Err(MoraError::Runtime(format!(
                    "jvp(f, primals, tangents) expects 3 arguments, got {}",
                    args.len()
                )));
            }
            let func = args[0].clone();
            let primals: Vec<Value> = match &args[1] {
                Value::List(xs) => xs.iter().cloned().collect(),
                other => vec![other.clone()],
            };
            let tangents_raw: Vec<Value> = match &args[2] {
                Value::List(xs) => xs.iter().cloned().collect(),
                other => vec![other.clone()],
            };
            if primals.len() != tangents_raw.len() || primals.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "jvp: primals ({}) and tangents ({}) must have matching non-zero length",
                    primals.len(),
                    tangents_raw.len()
                )));
            }
            let tape_id = core.autograd.push_tape();
            let mut leaf_nodes = Vec::with_capacity(primals.len());
            let mut leaf_tangents = Vec::with_capacity(primals.len());
            let mut tracked_args = Vec::with_capacity(primals.len());
            let mut scalar_out_dtype: Option<crate::common::DType> = None;

            let promote_scalar = |v: &Value| -> MoraResult<(
                Vec<usize>,
                crate::common::DType,
                crate::common::Device,
                f64,
            )> {
                match v {
                    Value::Float(f) => Ok((
                        Vec::new(),
                        crate::common::DType::F64,
                        crate::common::Device::Cpu,
                        *f,
                    )),
                    Value::Scalar(s) if s.dtype().is_float() => Ok((
                        Vec::new(),
                        s.dtype(),
                        crate::common::Device::Cpu,
                        s.as_f64(),
                    )),
                    other => Err(MoraError::Runtime(format!(
                        "jvp: expected float or floating-point Tensor, got {}",
                        crate::flow::dynamic_type_name(other)
                    ))),
                }
            };

            for (i, (p, t)) in primals.into_iter().zip(tangents_raw).enumerate() {
                match (p, t) {
                    (Value::Tensor(pt), Value::Tensor(tt)) => {
                        if !pt.dtype.is_float() {
                            core.autograd.pop_tape(tape_id);
                            return Err(MoraError::Runtime(format!(
                                "jvp: cannot differentiate non-floating-point Tensor<{}>",
                                pt.dtype
                            )));
                        }
                        if pt.shape != tt.shape {
                            core.autograd.pop_tape(tape_id);
                            return Err(MoraError::Runtime(format!(
                                "jvp: primal shape {:?} != tangent shape {:?} at input #{i}",
                                pt.shape, tt.shape
                            )));
                        }
                        let Some((_, leaf_idx)) = core.autograd.register_leaf(
                            tape_id,
                            pt.shape.clone(),
                            pt.dtype,
                            pt.device.clone(),
                        ) else {
                            core.autograd.pop_tape(tape_id);
                            return Err(MoraError::Runtime(
                                "jvp: failed to allocate autograd leaf".to_string(),
                            ));
                        };
                        leaf_nodes.push(leaf_idx);
                        leaf_tangents.push(tt);
                        tracked_args
                            .push(Value::Tensor(pt.with_pushed_grad_node((tape_id, leaf_idx))));
                    }
                    (Value::Tensor(pt), tv) if pt.shape.is_empty() && pt.dtype.is_float() => {
                        let (_, _, _, t_val) = match promote_scalar(&tv) {
                            Ok(x) => x,
                            Err(e) => {
                                core.autograd.pop_tape(tape_id);
                                return Err(e);
                            }
                        };
                        if i == 0 {
                            scalar_out_dtype = Some(pt.dtype);
                        }
                        let Some((_, leaf_idx)) = core.autograd.register_leaf(
                            tape_id,
                            Vec::new(),
                            pt.dtype,
                            pt.device.clone(),
                        ) else {
                            core.autograd.pop_tape(tape_id);
                            return Err(MoraError::Runtime(
                                "jvp: failed to allocate scalar autograd leaf".to_string(),
                            ));
                        };
                        let t_tensor = crate::value::tensor::Tensor::from_raw(
                            vec![t_val],
                            Vec::new(),
                            pt.dtype,
                            pt.device.clone(),
                            false,
                        )?;
                        leaf_nodes.push(leaf_idx);
                        leaf_tangents.push(t_tensor);
                        tracked_args
                            .push(Value::Tensor(pt.with_pushed_grad_node((tape_id, leaf_idx))));
                    }
                    (pv, Value::Tensor(tt)) if tt.shape.is_empty() && tt.dtype.is_float() => {
                        let (_, _, _, p_val) = match promote_scalar(&pv) {
                            Ok(x) => x,
                            Err(e) => {
                                core.autograd.pop_tape(tape_id);
                                return Err(e);
                            }
                        };
                        if i == 0 {
                            scalar_out_dtype = Some(tt.dtype);
                        }
                        let Some((_, leaf_idx)) = core.autograd.register_leaf(
                            tape_id,
                            Vec::new(),
                            tt.dtype,
                            tt.device.clone(),
                        ) else {
                            core.autograd.pop_tape(tape_id);
                            return Err(MoraError::Runtime(
                                "jvp: failed to allocate scalar autograd leaf".to_string(),
                            ));
                        };
                        let p_tensor = crate::value::tensor::Tensor::from_raw(
                            vec![p_val],
                            Vec::new(),
                            tt.dtype,
                            tt.device.clone(),
                            true,
                        )?;
                        leaf_nodes.push(leaf_idx);
                        leaf_tangents.push(tt);
                        tracked_args.push(Value::Tensor(
                            p_tensor.with_pushed_grad_node((tape_id, leaf_idx)),
                        ));
                    }
                    (pv, tv) => {
                        let (p_shape, p_dt, p_dev, p_val) = match promote_scalar(&pv) {
                            Ok(x) => x,
                            Err(e) => {
                                core.autograd.pop_tape(tape_id);
                                return Err(e);
                            }
                        };
                        let (_, _, _, t_val) = match promote_scalar(&tv) {
                            Ok(x) => x,
                            Err(e) => {
                                core.autograd.pop_tape(tape_id);
                                return Err(e);
                            }
                        };
                        if i == 0 {
                            scalar_out_dtype = Some(p_dt);
                        }
                        let Some((_, leaf_idx)) = core.autograd.register_leaf(
                            tape_id,
                            p_shape.clone(),
                            p_dt,
                            p_dev.clone(),
                        ) else {
                            core.autograd.pop_tape(tape_id);
                            return Err(MoraError::Runtime(
                                "jvp: failed to allocate scalar autograd leaf".to_string(),
                            ));
                        };
                        let p_tensor = crate::value::tensor::Tensor::from_raw(
                            vec![p_val],
                            p_shape.clone(),
                            p_dt,
                            p_dev.clone(),
                            true,
                        )?;
                        let t_tensor = crate::value::tensor::Tensor::from_raw(
                            vec![t_val],
                            p_shape,
                            p_dt,
                            p_dev,
                            false,
                        )?;
                        leaf_nodes.push(leaf_idx);
                        leaf_tangents.push(t_tensor);
                        tracked_args.push(Value::Tensor(
                            p_tensor.with_pushed_grad_node((tape_id, leaf_idx)),
                        ));
                    }
                }
            }

            Ok(CallAction::PushCombinator {
                combinator: Box::new(CombinatorFrame::JvpEval {
                    tape_id,
                    leaf_nodes,
                    leaf_tangents,
                    scalar_out_dtype,
                }),
                callee: func,
                args: tracked_args,
            })
        }
        BuiltinFnId::TraceDag => {
            if args.len() < 2 {
                return Err(MoraError::Runtime(format!(
                    "trace_dag(f, example_inputs...) expects at least 2 arguments, got {}",
                    args.len()
                )));
            }
            let func = args[0].clone();
            let raw_inputs: Vec<Value> = if args.len() == 2
                && let Value::List(xs) = &args[1]
            {
                xs.iter().cloned().collect()
            } else {
                args[1..].to_vec()
            };
            if raw_inputs.is_empty() {
                return Err(MoraError::Runtime(
                    "trace_dag: requires at least 1 input tensor".to_string(),
                ));
            }
            let tape_id = core.autograd.push_tape();
            let mut leaf_nodes = Vec::with_capacity(raw_inputs.len());
            let mut tracked_args = Vec::with_capacity(raw_inputs.len());
            for (i, inp) in raw_inputs.into_iter().enumerate() {
                let t = match inp {
                    Value::Tensor(t) => t,
                    Value::Float(f) => crate::value::tensor::Tensor::from_raw(
                        vec![f],
                        Vec::new(),
                        crate::common::DType::F64,
                        crate::common::Device::Cpu,
                        true,
                    )?,
                    Value::Scalar(s) => crate::value::tensor::Tensor::from_raw(
                        vec![s.as_f64()],
                        Vec::new(),
                        s.dtype(),
                        crate::common::Device::Cpu,
                        true,
                    )?,
                    other => {
                        core.autograd.pop_tape(tape_id);
                        return Err(MoraError::Runtime(format!(
                            "trace_dag: example input #{i} must be Tensor or numeric scalar, got {}",
                            crate::flow::dynamic_type_name(&other)
                        )));
                    }
                };
                let Some((_, leaf_idx)) = core.autograd.register_leaf(
                    tape_id,
                    t.shape.clone(),
                    t.dtype,
                    t.device.clone(),
                ) else {
                    core.autograd.pop_tape(tape_id);
                    return Err(MoraError::Runtime(
                        "trace_dag: failed to allocate input leaf".to_string(),
                    ));
                };
                leaf_nodes.push(leaf_idx);
                tracked_args.push(Value::Tensor(t.with_pushed_grad_node((tape_id, leaf_idx))));
            }

            Ok(CallAction::PushCombinator {
                combinator: Box::new(CombinatorFrame::TraceDagEval {
                    tape_id,
                    leaf_nodes,
                }),
                callee: func,
                args: tracked_args,
            })
        }
        BuiltinFnId::KvAllocator => {
            if args.len() < 4 || args.len() > 7 {
                return Err(MoraError::Runtime(format!(
                    "kv_allocator(total_pages, page_tokens, num_heads, head_dim, dtype?, device?, max_bytes?) expects 4..=7 arguments, got {}",
                    args.len()
                )));
            }
            let pos_usize = |v: &Value, label: &str| -> MoraResult<usize> {
                match v {
                    Value::Int(n) if *n > 0 => Ok(*n as usize),
                    _ => Err(MoraError::Runtime(format!(
                        "kv_allocator: '{label}' must be a positive integer, got {v}"
                    ))),
                }
            };
            let total_pages = pos_usize(&args[0], "total_pages")?;
            let page_tokens = pos_usize(&args[1], "page_tokens")?;
            let num_heads = pos_usize(&args[2], "num_heads")?;
            let head_dim = pos_usize(&args[3], "head_dim")?;
            let dtype = match args.get(4) {
                None | Some(Value::Nil) => crate::common::DType::F64,
                Some(Value::String(s)) => crate::common::DType::from_name(s).ok_or_else(|| {
                    MoraError::Runtime(format!("kv_allocator: unknown dtype '{s}'"))
                })?,
                Some(other) => {
                    return Err(MoraError::Runtime(format!(
                        "kv_allocator: dtype must be string, got {}",
                        crate::flow::type_name(other)
                    )));
                }
            };
            let device = match args.get(5) {
                None | Some(Value::Nil) => crate::common::Device::Cpu,
                Some(Value::String(s)) => crate::common::Device::parse(s)?,
                Some(other) => {
                    return Err(MoraError::Runtime(format!(
                        "kv_allocator: device must be string, got {}",
                        crate::flow::type_name(other)
                    )));
                }
            };
            let max_bytes = match args.get(6) {
                None | Some(Value::Nil) => None,
                Some(v) => Some(pos_usize(v, "max_bytes")?),
            };
            let alloc = crate::runtime::kv_cache::KVPageAllocator::new_with_quota(
                total_pages,
                page_tokens,
                num_heads,
                head_dim,
                dtype,
                device,
                max_bytes,
            )?;
            Ok(CallAction::Done(Value::KVAllocator(alloc)))
        }
        BuiltinFnId::MemoryPool => {
            if args.is_empty() || args.len() > 2 {
                return Err(MoraError::Runtime(format!(
                    "memory_pool(max_bytes, device?) expects 1..=2 arguments, got {}",
                    args.len()
                )));
            }
            let max_bytes = match &args[0] {
                Value::Int(n) if *n > 0 => *n as usize,
                other => {
                    return Err(MoraError::Runtime(format!(
                        "memory_pool: 'max_bytes' must be a positive integer, got {other}"
                    )));
                }
            };
            let device = match args.get(1) {
                None | Some(Value::Nil) => crate::common::Device::Cpu,
                Some(Value::String(s)) => crate::common::Device::parse(s)?,
                Some(other) => {
                    return Err(MoraError::Runtime(format!(
                        "memory_pool: device must be string, got {}",
                        crate::flow::type_name(other)
                    )));
                }
            };
            let pool = crate::runtime::memory_pool::MemoryPool::new(max_bytes, device)?;
            Ok(CallAction::Done(Value::MemoryPool(pool)))
        }
        BuiltinFnId::ProcessGroup => {
            if args.len() != 1 {
                return Err(MoraError::Runtime(format!(
                    "process_group(world_size) expects 1 argument, got {}",
                    args.len()
                )));
            }
            let world_size = match &args[0] {
                Value::Int(n) if *n > 0 => *n as usize,
                other => {
                    return Err(MoraError::Runtime(format!(
                        "process_group: 'world_size' must be a positive integer, got {other}"
                    )));
                }
            };
            let pg = crate::runtime::distributed::ProcessGroup::new_inproc(world_size)?;
            Ok(CallAction::Done(Value::ProcessGroup(pg)))
        }
        BuiltinFnId::TcpProcessGroup => {
            if args.len() != 3 {
                return Err(MoraError::Runtime(format!(
                    "tcp_process_group(rank, world_size, addrs) expects 3 arguments, got {}",
                    args.len()
                )));
            }
            let rank = match &args[0] {
                Value::Int(n) if *n >= 0 => *n as usize,
                other => {
                    return Err(MoraError::Runtime(format!(
                        "tcp_process_group: 'rank' must be a non-negative integer, got {other}"
                    )));
                }
            };
            let world_size = match &args[1] {
                Value::Int(n) if *n > 0 => *n as usize,
                other => {
                    return Err(MoraError::Runtime(format!(
                        "tcp_process_group: 'world_size' must be a positive integer, got {other}"
                    )));
                }
            };
            let addrs = match &args[2] {
                Value::List(items) => {
                    let mut out = Vec::with_capacity(items.len());
                    for item in items.iter() {
                        match item {
                            Value::String(s) => out.push(s.clone()),
                            other => {
                                return Err(MoraError::Runtime(format!(
                                    "tcp_process_group: peer address must be string, got {}",
                                    crate::flow::type_name(other)
                                )));
                            }
                        }
                    }
                    out
                }
                other => {
                    return Err(MoraError::Runtime(format!(
                        "tcp_process_group: 'addrs' must be list<string>, got {}",
                        crate::flow::type_name(other)
                    )));
                }
            };
            let eff_ctx = core.task_context.clone();
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                let pg = crate::runtime::distributed::ProcessGroup::connect_tcp_ring(
                    rank,
                    world_size,
                    addrs,
                    eff_ctx.as_ref(),
                )
                .await?;
                Ok(Value::ProcessGroup(pg))
            })))
        }
        BuiltinFnId::Profiler => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "profiler() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            Ok(CallAction::Done(Value::TraceProfiler(
                crate::runtime::profiler::global_handle(),
            )))
        }
        _ => unreachable!("all BuiltinFnId handled cleanly"),
    }
}

/// Resolve a method invocation `receiver.method(args...)` onto pure value result or stackful action.
pub fn resolve_method_call(
    core: &mut CoreRuntime,
    receiver: Value,
    method: &str,
    args: Vec<Value>,
    call_site: Span,
) -> MoraResult<CallAction> {
    resolve_method_call_named(core, receiver, method, args, Vec::new(), call_site)
}

/// Construct a nominal `Value::StructInstance` from positional + keyword arguments,
/// evaluating any omitted field default thunks iteratively on the VM stack.
pub fn construct_struct_instance(
    def: Arc<StructDefRuntime>,
    args: Vec<Value>,
    named_args: Vec<(String, Value)>,
) -> MoraResult<CallAction> {
    construct_struct_instance_with_consts(def, Vec::new(), args, named_args)
}

/// P4-1A: Construct a nominal `Value::StructInstance` with explicit and/or tensor-shape-inferred
/// compile-time const-generic integer bindings (`Linear<4, 3>(weight=w, bias=b)`).
pub(crate) fn construct_struct_instance_with_consts(
    def: Arc<StructDefRuntime>,
    explicit_const_args: Vec<(String, i64)>,
    args: Vec<Value>,
    named_args: Vec<(String, Value)>,
) -> MoraResult<CallAction> {
    if args.len() > def.fields.len() {
        return Err(MoraError::Runtime(format!(
            "struct '{}' expects at most {} fields, got {} positional args",
            def.name,
            def.fields.len(),
            args.len()
        )));
    }
    let mut fields: std::collections::HashMap<String, Value> =
        std::collections::HashMap::with_capacity(def.fields.len());
    for (idx, val) in args.into_iter().enumerate() {
        fields.insert(def.fields[idx].name.clone(), val);
    }
    for (kw, val) in named_args {
        if !def.fields.iter().any(|f| f.name == kw) {
            return Err(MoraError::Runtime(format!(
                "struct '{}' has no field '{}'",
                def.name, kw
            )));
        }
        if fields.contains_key(&kw) {
            return Err(MoraError::Runtime(format!(
                "struct '{}': duplicate value for field '{}'",
                def.name, kw
            )));
        }
        fields.insert(kw, val);
    }

    let mut pending_defaults: Vec<(String, crate::mir::control::FnId)> = Vec::new();
    for f in &def.fields {
        if !fields.contains_key(&f.name) {
            if let Some(df_id) = f.default_fn {
                pending_defaults.push((f.name.clone(), df_id));
            } else {
                return Err(MoraError::Runtime(format!(
                    "struct '{}' missing required field '{}'",
                    def.name, f.name
                )));
            }
        }
    }

    if pending_defaults.is_empty() {
        let const_args = finalize_struct_const_args(&def, &explicit_const_args, &fields)?;
        let field_order = def.fields.iter().map(|f| f.name.clone()).collect();
        return Ok(CallAction::Done(Value::StructInstance {
            type_name: def.name.clone(),
            fields,
            field_order,
            const_args,
            def,
        }));
    }

    let (first_field, first_fn) = pending_defaults.remove(0);
    let thunk = Value::Task {
        name: format!("{}::default_{}", def.name, first_field),
        params: Vec::new(),
        program: Arc::clone(&def.program),
        fn_id: first_fn,
        globals: Arc::clone(&def.globals),
    };
    Ok(CallAction::PushCombinator {
        combinator: Box::new(CombinatorFrame::StructInit(Box::new(
            super::continuation::StructInitState {
                def,
                fields,
                const_args: explicit_const_args,
                pending_defaults,
                current_field: first_field,
            },
        ))),
        callee: thunk,
        args: Vec::new(),
    })
}

/// P4-1A: Construct a nominal `Value::EnumVariant`, propagating explicit const-generic
/// arguments and inferring any omitted const generics from tensor payload shapes.
pub(crate) fn construct_enum_variant_with_consts(
    def: &crate::value::EnumDefRuntime,
    variant: &str,
    explicit_const_args: Vec<(String, i64)>,
    args: Vec<Value>,
    named_args: Vec<(String, Value)>,
) -> MoraResult<CallAction> {
    let Some((vname, payload_tys)) = def.variants.iter().find(|(v, _)| v == variant) else {
        return Err(MoraError::Runtime(format!(
            "Enum '{}' has no variant '{}'",
            def.name, variant
        )));
    };
    if !named_args.is_empty() {
        return Err(MoraError::Runtime(format!(
            "{}::{} does not accept keyword arguments",
            def.name, vname
        )));
    }
    if args.len() != payload_tys.len() {
        return Err(MoraError::Runtime(format!(
            "{}::{} expects {} payload arg(s), got {}",
            def.name,
            vname,
            payload_tys.len(),
            args.len()
        )));
    }
    let mut out = explicit_const_args;
    for (bind_v, cname, pidx, axis_idx) in &def.const_shape_bindings {
        if bind_v != vname {
            continue;
        }
        if let Some(Value::Tensor(t)) = args.get(*pidx)
            && let Some(&dim) = t.shape.get(*axis_idx)
        {
            let dim_i64 = dim as i64;
            if let Some((_, existing)) = out.iter().find(|(k, _)| k == cname) {
                if *existing != dim_i64 {
                    return Err(MoraError::Runtime(format!(
                        "{}::{}: const generic '{}' mismatch (expected {}, got tensor dim {})",
                        def.name, vname, cname, existing, dim_i64
                    )));
                }
            } else {
                out.push((cname.clone(), dim_i64));
            }
        }
    }
    Ok(CallAction::Done(Value::EnumVariant {
        enum_name: def.name.clone(),
        variant: vname.clone(),
        payload: args,
        const_args: out,
    }))
}

/// Populate missing const-generic values from tensor field shapes (and validate explicit consts).
pub(crate) fn finalize_struct_const_args(
    def: &StructDefRuntime,
    explicit: &[(String, i64)],
    fields: &std::collections::HashMap<String, Value>,
) -> MoraResult<Vec<(String, i64)>> {
    let mut out: Vec<(String, i64)> = explicit.to_vec();
    for (cname, fidx, aidx) in &def.const_shape_bindings {
        if let Some(fdef) = def.fields.get(*fidx)
            && let Some(Value::Tensor(t)) = fields.get(&fdef.name)
            && let Some(&actual_dim) = t.shape.get(*aidx)
        {
            let actual_i64 = actual_dim as i64;
            if let Some((_, expected_i64)) = out.iter().find(|(n, _)| n == cname) {
                if *expected_i64 != actual_i64 {
                    return Err(MoraError::Runtime(format!(
                        "struct '{}' const generic '{}' mismatch: expected {}, field '{}' axis {} has {}",
                        def.name, cname, expected_i64, fdef.name, aidx, actual_i64
                    )));
                }
            } else {
                out.push((cname.clone(), actual_i64));
            }
        }
    }
    Ok(out)
}

/// P4-1A: Resolve a trait-driven overloaded operator (`Add::add`, `Sub::sub`, ..., `Eq::eq`, `Neg::neg`)
/// on a nominal `StructInstance`, `EnumVariant`, or `TraitObject`.
pub(crate) fn resolve_operator_impl(
    core: &CoreRuntime,
    receiver: &Value,
    op_trait: &str,
    op_method: &str,
    mut rhs_args: Vec<Value>,
) -> Option<(Value, Vec<Value>)> {
    let target_name = match receiver {
        Value::StructInstance { type_name, .. } => type_name.as_str(),
        Value::EnumVariant { enum_name, .. } => enum_name.as_str(),
        Value::TraitObject { for_type, .. } => for_type.as_str(),
        _ => return None,
    };
    let (func, has_self) = core
        .trait_impls
        .iter()
        .find(|((tr, tgt, m), (_, hs))| {
            *hs && tr == op_trait
                && m == op_method
                && (tgt == target_name || tgt.eq_ignore_ascii_case(target_name))
        })
        .map(|(_, v)| v.clone())?;
    let mut all_args = Vec::with_capacity(1 + rhs_args.len());
    if has_self {
        all_args.push(receiver.clone());
    }
    all_args.append(&mut rhs_args);
    Some((func, all_args))
}

/// Resolve a method invocation `receiver.method(args..., kw=val...)` onto pure result or stackful action.
pub fn resolve_method_call_named(
    core: &mut CoreRuntime,
    receiver: Value,
    method: &str,
    args: Vec<Value>,
    named_args: Vec<(String, Value)>,
    call_site: Span,
) -> MoraResult<CallAction> {
    // Tensor methods run with live autograd tape tracking first, then fall back to user impls.
    if let Value::Tensor(ref t) = receiver {
        if named_args.is_empty() {
            match crate::value::tensor::call_tensor_method_tracked(
                t,
                method,
                &args,
                Some(&mut core.autograd),
            ) {
                Ok(v) => return Ok(CallAction::Done(v)),
                Err(e) if e.message().contains("Tensor has no method") => {
                    if let Some((func, all_args)) =
                        resolve_concrete_impl_method(core, &receiver, method, &args)
                    {
                        return resolve_value_call_named(func, all_args, named_args, core);
                    }
                    return Err(e);
                }
                Err(e) => return Err(e),
            }
        } else if let Some((func, all_args)) =
            resolve_concrete_impl_method(core, &receiver, method, &args)
        {
            return resolve_value_call_named(func, all_args, named_args, core);
        }
    }

    // Nominal struct type & instance dispatch (field access, inherent/trait impls, schema/tags).
    match &receiver {
        Value::StructType(def) => {
            let def = Arc::clone(def);
            return match method {
                "schema" => Ok(CallAction::Done(def.to_schema_value())),
                "tags" => Ok(CallAction::Done(def.to_tags_value())),
                "fields" => Ok(CallAction::Done(Value::List(
                    def.fields
                        .iter()
                        .map(|f| Value::String(f.name.clone()))
                        .collect(),
                ))),
                "new" => construct_struct_instance(def, args, named_args),
                "from_dict" => {
                    let Some(Value::Dict(map)) = args.into_iter().next() else {
                        return Err(MoraError::Runtime(format!(
                            "{}::from_dict(dict) expects a dict argument",
                            def.name
                        )));
                    };
                    let entries: Vec<(String, Value)> = map.into_iter().collect();
                    construct_struct_instance(def, Vec::new(), entries)
                }
                _ => {
                    if let Some((func, has_self)) = core
                        .inherent_methods
                        .get(&(def.name.clone(), method.to_string()))
                        .cloned()
                        && !has_self
                    {
                        return resolve_value_call_named(func, args, named_args, core);
                    }
                    Err(MoraError::Runtime(format!(
                        "Struct type '{}' has no method '{}'",
                        def.name, method
                    )))
                }
            };
        }
        Value::StructInstance {
            type_name,
            fields,
            def,
            ..
        } => {
            // 1. Direct field read when invoked with 0 args (`obj.field`)
            if args.is_empty()
                && named_args.is_empty()
                && let Some(val) = fields.get(method)
            {
                return Ok(CallAction::Done(val.clone()));
            }
            // 2. Inherent `impl Struct` or trait `impl Trait for Struct` method
            if let Some((func, all_args)) =
                resolve_concrete_impl_method(core, &receiver, method, &args)
            {
                return resolve_value_call_named(func, all_args, named_args, core);
            }
            // 3. Built-in struct instance reflection / projection & Layer parameter-state methods
            match method {
                "to_dict" => return Ok(CallAction::Done(Value::Dict(fields.clone()))),
                "schema" => return Ok(CallAction::Done(def.to_schema_value())),
                "tags" => return Ok(CallAction::Done(def.to_tags_value())),
                "json" => {
                    return Ok(CallAction::Done(Value::String(crate::flow::value_to_json(
                        &receiver,
                    ))));
                }
                "parameters" => {
                    if !args.is_empty() || !named_args.is_empty() {
                        return Err(MoraError::Runtime(format!(
                            "{}.parameters() expects 0 arguments",
                            type_name
                        )));
                    }
                    let mut out = Vec::new();
                    collect_struct_tensors(&receiver, "", false, &mut |_, t| {
                        out.push(Value::Tensor(t));
                    });
                    return Ok(CallAction::Done(Value::List(out.into())));
                }
                "named_parameters" | "state_dict" => {
                    let include_buffers = method == "state_dict";
                    let prefix = extract_optional_prefix_arg(method, &args, &named_args)?;
                    let mut out = HashMap::new();
                    collect_struct_tensors(&receiver, &prefix, include_buffers, &mut |k, t| {
                        out.insert(k, Value::Tensor(t));
                    });
                    return Ok(CallAction::Done(Value::Dict(out)));
                }
                "load_state_dict" => {
                    let (state_map, strict) =
                        extract_load_state_dict_args(type_name, &args, &named_args)?;
                    let updated = load_struct_state_dict(&receiver, state_map, strict)?;
                    return Ok(CallAction::Done(updated));
                }
                "to_device" => {
                    if args.len() != 1 || !named_args.is_empty() {
                        return Err(MoraError::Runtime(format!(
                            "{}.to_device(device) expects 1 string argument",
                            type_name
                        )));
                    }
                    let Value::String(dev_str) = &args[0] else {
                        return Err(MoraError::Runtime(format!(
                            "{}.to_device(device) expects a device string, got {}",
                            type_name,
                            crate::flow::type_name(&args[0])
                        )));
                    };
                    let dev = crate::common::Device::parse(dev_str)?;
                    let moved = move_struct_to_device(&receiver, &dev)?;
                    return Ok(CallAction::Done(moved));
                }
                _ => {}
            }
            // 4. Callable field invocation (`obj.callback(x)`)
            if let Some(callable) = fields.get(method).cloned() {
                return resolve_value_call_named(callable, args, named_args, core);
            }
            return Err(MoraError::Runtime(format!(
                "Struct '{}' has no field or method '{}'",
                type_name, method
            )));
        }
        Value::EnumType(def) => {
            let def = Arc::clone(def);
            if method == "variants" {
                return Ok(CallAction::Done(Value::List(
                    def.variants
                        .iter()
                        .map(|(v, _)| Value::String(v.clone()))
                        .collect(),
                )));
            }
            if def.variants.iter().any(|(v, _)| v == method) {
                return construct_enum_variant_with_consts(
                    &def,
                    method,
                    Vec::new(),
                    args,
                    named_args,
                );
            }
            return Err(MoraError::Runtime(format!(
                "Enum '{}' has no variant or method '{}'",
                def.name, method
            )));
        }
        Value::EnumVariant { enum_name, .. } => {
            if let Some((func, all_args)) =
                resolve_concrete_impl_method(core, &receiver, method, &args)
            {
                return resolve_value_call_named(func, all_args, named_args, core);
            }
            return Err(MoraError::Runtime(format!(
                "Enum '{}' has no method '{}'",
                enum_name, method
            )));
        }
        _ => {}
    }

    if let Value::Builtin(BuiltinKind::Math) = &receiver {
        return crate::runtime::builtins::math::call_math_method_tracked(
            method,
            &args,
            Some(&mut core.autograd),
        )
        .map(CallAction::Done);
    }

    if named_args.is_empty()
        && let Some(res) = eval_pure_method(&receiver, method, &args)
    {
        match res {
            Ok(v) => return Ok(CallAction::Done(v)),
            Err(e) => {
                if let Some((func, all_args)) =
                    resolve_concrete_impl_method(core, &receiver, method, &args)
                {
                    return resolve_value_call_named(func, all_args, named_args, core);
                }
                return Err(e);
            }
        }
    }

    // If keyword args were passed to a user impl method on a built-in type, dispatch it now.
    if !named_args.is_empty()
        && let Some((func, all_args)) = resolve_concrete_impl_method(core, &receiver, method, &args)
    {
        return resolve_value_call_named(func, all_args, named_args, core);
    }

    match receiver {
        Value::TraitObject { ref data, .. } => {
            let underlying = (**data).clone();
            match resolve_trait_method_call(core, &receiver, method, args.clone(), call_site) {
                Ok(TraitMethodTarget::Immediate(v)) => Ok(CallAction::Done(v)),
                Ok(TraitMethodTarget::Call(task, all_args)) => {
                    resolve_value_call_named(task, all_args, named_args, core)
                }
                Err(trait_err) => {
                    // Allow impl methods (e.g. `impl Person for dict` calling `self.get("name")`
                    // or `impl Greeter for User` reading `self.name`) to access underlying receiver.
                    if let Ok(action) = resolve_method_call_named(
                        core, underlying, method, args, named_args, call_site,
                    ) {
                        return Ok(action);
                    }
                    Err(trait_err)
                }
            }
        }
        Value::External(obj) => obj.call_method(method, &args, core).map(CallAction::Done),
        Value::Builtin(BuiltinKind::File) => {
            crate::runtime::builtins::call_file_method(core, method, &args).map(CallAction::Done)
        }
        Value::Builtin(BuiltinKind::Exec) => {
            crate::runtime::builtins::call_exec_method(core, method, &args).map(CallAction::Done)
        }
        Value::Builtin(BuiltinKind::Random) => {
            let Some(label) = ambient::random_label_for_method(method) else {
                return Err(MoraError::Runtime(format!(
                    "random.{method}: unknown method"
                )));
            };
            Ok(CallAction::AmbientRandom {
                label,
                method: method.to_string(),
                args,
            })
        }
        Value::Channel(ch) => resolve_channel_method(&ch, method, args, core),
        Value::TaskHandle(h) => resolve_task_handle_method(&h, method, core),
        Value::Context(ctx) => resolve_context_method(&ctx, method, args, core),
        Value::TcpListener(l) => resolve_tcp_listener_method(&l, method, args, core),
        Value::TcpStream(s) => resolve_tcp_stream_method(&s, method, args, core),
        Value::TlsListener(l) => resolve_tls_listener_method(&l, method, args, core),
        Value::TlsStream(s) => resolve_tls_stream_method(&s, method, args, core),
        Value::ComputeDag(dag) => resolve_compute_dag_method(&dag, method, args, core),
        Value::KVAllocator(kv) => resolve_kv_allocator_method(&kv, method, args),
        Value::MemoryPool(pool) => resolve_memory_pool_method(&pool, method, args),
        Value::GpuKernel(kernel) => resolve_gpu_kernel_method(&kernel, method, args),
        Value::ProcessGroup(pg) => resolve_process_group_method(&pg, method, args, core),
        Value::TraceProfiler(prof) => resolve_trace_profiler_method(&prof, method, args),
        Value::List(list) => match method {
            "map" => {
                let mapper = args
                    .first()
                    .cloned()
                    .ok_or_else(|| MoraError::Runtime("map() requires a function".to_string()))?;
                if list.is_empty() {
                    return Ok(CallAction::Done(Value::List(Vec::new().into())));
                }
                let items: Vec<Value> = list.iter().cloned().collect();
                let first = items[0].clone();
                Ok(CallAction::PushCombinator {
                    combinator: Box::new(CombinatorFrame::ListMap {
                        func: mapper.clone(),
                        results: Vec::with_capacity(items.len()),
                        items,
                        idx: 0,
                    }),
                    callee: mapper,
                    args: vec![first],
                })
            }
            "filter" => {
                let predicate = args.first().cloned().ok_or_else(|| {
                    MoraError::Runtime("filter() requires a function".to_string())
                })?;
                if list.is_empty() {
                    return Ok(CallAction::Done(Value::List(Vec::new().into())));
                }
                let items: Vec<Value> = list.iter().cloned().collect();
                let first = items[0].clone();
                Ok(CallAction::PushCombinator {
                    combinator: Box::new(CombinatorFrame::ListFilter {
                        pred: predicate.clone(),
                        items,
                        idx: 0,
                        results: Vec::new(),
                    }),
                    callee: predicate,
                    args: vec![first],
                })
            }
            "reduce" => {
                let reducer = args.first().cloned().ok_or_else(|| {
                    MoraError::Runtime("reduce() requires a function".to_string())
                })?;
                let init = args.get(1).cloned().ok_or_else(|| {
                    MoraError::Runtime(
                        "reduce() requires an initial value as the second argument".to_string(),
                    )
                })?;
                if list.is_empty() {
                    return Ok(CallAction::Done(init));
                }
                let items: Vec<Value> = list.iter().cloned().collect();
                let first = items[0].clone();
                Ok(CallAction::PushCombinator {
                    combinator: Box::new(CombinatorFrame::ListReduce {
                        func: reducer.clone(),
                        items,
                        idx: 0,
                    }),
                    callee: reducer,
                    args: vec![init, first],
                })
            }
            _ => unreachable!("eval_pure_method covered remaining list methods"),
        },
        Value::Dict(map) => {
            if let Some(callable) = map.get(method) {
                return resolve_value_call(callable.clone(), args, core);
            }
            Err(MoraError::Runtime(format!("Dict has no method: {method}")))
        }
        other => Err(MoraError::Runtime(format!(
            "cannot call method '{method}' on {}",
            crate::flow::type_name(&other)
        ))),
    }
}

fn resolve_channel_method(
    ch: &Arc<crate::runtime::scheduler::MoraChannel>,
    method: &str,
    args: Vec<Value>,
    core: &CoreRuntime,
) -> MoraResult<CallAction> {
    match method {
        "send" => {
            let val = args.into_iter().next().ok_or_else(|| {
                MoraError::Runtime("channel.send() requires 1 value argument".to_string())
            })?;
            let ch = Arc::clone(ch);
            let ctx = core.task_context.clone();
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                ch.send_async(val, ctx.as_ref()).await?;
                Ok(Value::Nil)
            })))
        }
        "try_send" => {
            let val = args.into_iter().next().ok_or_else(|| {
                MoraError::Runtime("channel.try_send() requires 1 value argument".to_string())
            })?;
            let ok = ch.try_send(val)?;
            Ok(CallAction::Done(Value::Bool(ok)))
        }
        "send_ctx" => {
            let mut it = args.into_iter();
            let val = it.next().ok_or_else(|| {
                MoraError::Runtime("channel.send_ctx(val, ctx) requires 2 arguments".to_string())
            })?;
            let Some(Value::Context(explicit_ctx)) = it.next() else {
                return Err(MoraError::Runtime(
                    "channel.send_ctx(val, ctx): second argument must be a Context".to_string(),
                ));
            };
            let ch = Arc::clone(ch);
            let outer_ctx = core.task_context.clone();
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                match outer_ctx.as_ref() {
                    Some(outer) => {
                        outer.status()?;
                        match crate::runtime::scheduler::race2(
                            outer.cancelled(),
                            ch.send_async(val, Some(&explicit_ctx)),
                        )
                        .await
                        {
                            crate::runtime::scheduler::Select2::First(err) => Err(err),
                            crate::runtime::scheduler::Select2::Second(res) => {
                                res?;
                                Ok(Value::Nil)
                            }
                        }
                    }
                    None => {
                        ch.send_async(val, Some(&explicit_ctx)).await?;
                        Ok(Value::Nil)
                    }
                }
            })))
        }
        "recv" => {
            let ch = Arc::clone(ch);
            let ctx = core.task_context.clone();
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                ch.recv_async(ctx.as_ref()).await
            })))
        }
        "try_recv" => {
            let opt = ch.try_recv()?;
            Ok(CallAction::Done(opt.unwrap_or(Value::Nil)))
        }
        "recv_ctx" => {
            let Some(Value::Context(explicit_ctx)) = args.into_iter().next() else {
                return Err(MoraError::Runtime(
                    "channel.recv_ctx(ctx) requires 1 Context argument".to_string(),
                ));
            };
            let ch = Arc::clone(ch);
            let outer_ctx = core.task_context.clone();
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                match outer_ctx.as_ref() {
                    Some(outer) => {
                        outer.status()?;
                        match crate::runtime::scheduler::race2(
                            outer.cancelled(),
                            ch.recv_async(Some(&explicit_ctx)),
                        )
                        .await
                        {
                            crate::runtime::scheduler::Select2::First(err) => Err(err),
                            crate::runtime::scheduler::Select2::Second(res) => res,
                        }
                    }
                    None => ch.recv_async(Some(&explicit_ctx)).await,
                }
            })))
        }
        "close" => {
            ch.close();
            Ok(CallAction::Done(Value::Nil))
        }
        "is_closed" => Ok(CallAction::Done(Value::Bool(ch.is_closed()))),
        "len" => Ok(CallAction::Done(Value::Int(ch.len() as i64))),
        "capacity" => Ok(CallAction::Done(Value::Int(ch.capacity() as i64))),
        _ => Err(MoraError::Runtime(format!(
            "Channel has no method: {}",
            method
        ))),
    }
}

fn resolve_task_handle_method(
    handle: &Arc<crate::runtime::scheduler::MoraTaskHandle>,
    method: &str,
    core: &CoreRuntime,
) -> MoraResult<CallAction> {
    match method {
        "join" => {
            let handle = Arc::clone(handle);
            let ctx = core.task_context.clone();
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                handle.join_async(ctx.as_ref()).await
            })))
        }
        "abort" => {
            handle.abort();
            Ok(CallAction::Done(Value::Nil))
        }
        "is_done" => Ok(CallAction::Done(Value::Bool(handle.is_done()))),
        "context" => Ok(CallAction::Done(Value::Context(handle.context()))),
        _ => Err(MoraError::Runtime(format!(
            "TaskHandle has no method: {}",
            method
        ))),
    }
}

fn resolve_context_method(
    ctx: &crate::runtime::scheduler::TaskContext,
    method: &str,
    args: Vec<Value>,
    core: &CoreRuntime,
) -> MoraResult<CallAction> {
    match method {
        "child" => Ok(CallAction::Done(Value::Context(ctx.child()))),
        "with_timeout" => {
            let Some(ms_val) = args.first() else {
                return Err(MoraError::Runtime(
                    "context.with_timeout(ms) requires 1 numeric argument".to_string(),
                ));
            };
            let dur = crate::runtime::builtins::duration_from_ms(ms_val, "context.with_timeout")?;
            Ok(CallAction::Done(Value::Context(ctx.with_timeout(dur))))
        }
        "cancel" => {
            let reason = match args.first() {
                None | Some(Value::Nil) => "cancelled".to_string(),
                Some(v) => v.to_string(),
            };
            ctx.cancel(reason);
            Ok(CallAction::Done(Value::Nil))
        }
        "is_cancelled" => Ok(CallAction::Done(Value::Bool(ctx.status().is_err()))),
        "spawn" => {
            let mut it = args.into_iter();
            let callee = it.next().ok_or_else(|| {
                MoraError::Runtime("context.spawn(fn, args...) requires a callable".to_string())
            })?;
            let spawn_args: Vec<Value> = it.collect();
            let handle = core
                .scheduler()
                .spawn_mora_value(callee, spawn_args, core, ctx);
            Ok(CallAction::Done(Value::TaskHandle(handle)))
        }
        _ => Err(MoraError::Runtime(format!(
            "Context has no method: {}",
            method
        ))),
    }
}

fn parse_tcp_max_bytes(val: Option<&Value>, default_bytes: usize, op: &str) -> MoraResult<usize> {
    match val {
        None | Some(Value::Nil) => Ok(default_bytes),
        Some(Value::Int(n)) if *n >= 0 => Ok(*n as usize),
        Some(Value::Float(f)) if f.is_finite() && *f >= 0.0 && f.fract() == 0.0 => Ok(*f as usize),
        Some(other) => Err(MoraError::Runtime(format!(
            "{op}: max_bytes must be a non-negative integer, got {}",
            crate::flow::type_name(other)
        ))),
    }
}

fn parse_optional_tcp_ctx(
    val: Option<&Value>,
    op: &str,
) -> MoraResult<Option<crate::runtime::scheduler::TaskContext>> {
    match val {
        None | Some(Value::Nil) => Ok(None),
        Some(Value::Context(c)) => Ok(Some(c.clone())),
        Some(other) => Err(MoraError::Runtime(format!(
            "{op}: ctx must be a context, got {}",
            crate::flow::type_name(other)
        ))),
    }
}

fn resolve_tcp_listener_method(
    listener: &Arc<crate::runtime::reactor::MoraTcpListener>,
    method: &str,
    args: Vec<Value>,
    core: &CoreRuntime,
) -> MoraResult<CallAction> {
    match method {
        "accept" => {
            if args.len() > 1 {
                return Err(MoraError::Runtime(format!(
                    "tcp_listener.accept(ctx?) expects 0..=1 arguments, got {}",
                    args.len()
                )));
            }
            let explicit_ctx = parse_optional_tcp_ctx(args.first(), "tcp_listener.accept")?;
            let eff_ctx = explicit_ctx.or_else(|| core.task_context.clone());
            let listener = Arc::clone(listener);
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                let stream = listener.accept(eff_ctx.as_ref()).await?;
                Ok(Value::TcpStream(stream))
            })))
        }
        "local_addr" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "tcp_listener.local_addr() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            Ok(CallAction::Done(Value::String(
                listener.local_addr().to_string(),
            )))
        }
        "close" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "tcp_listener.close() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            listener.close();
            Ok(CallAction::Done(Value::Nil))
        }
        "is_closed" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "tcp_listener.is_closed() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            Ok(CallAction::Done(Value::Bool(listener.is_closed())))
        }
        _ => Err(MoraError::Runtime(format!(
            "TcpListener has no method: {method}"
        ))),
    }
}

fn resolve_tcp_stream_method(
    stream: &Arc<crate::runtime::reactor::MoraTcpStream>,
    method: &str,
    args: Vec<Value>,
    core: &CoreRuntime,
) -> MoraResult<CallAction> {
    match method {
        "read" => {
            if args.len() > 2 {
                return Err(MoraError::Runtime(format!(
                    "tcp_stream.read(max_bytes?, ctx?) expects 0..=2 arguments, got {}",
                    args.len()
                )));
            }
            let max_bytes = parse_tcp_max_bytes(args.first(), 65536, "tcp_stream.read")?;
            let explicit_ctx = parse_optional_tcp_ctx(args.get(1), "tcp_stream.read")?;
            let eff_ctx = explicit_ctx.or_else(|| core.task_context.clone());
            let stream = Arc::clone(stream);
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                let bytes = stream.read_bytes(max_bytes, eff_ctx.as_ref()).await?;
                let text = String::from_utf8(bytes).map_err(|e| {
                    MoraError::Io(format!("tcp_stream.read: invalid UTF-8 payload: {e}"))
                })?;
                Ok(Value::String(text))
            })))
        }
        "read_bytes" => {
            if args.len() > 2 {
                return Err(MoraError::Runtime(format!(
                    "tcp_stream.read_bytes(max_bytes?, ctx?) expects 0..=2 arguments, got {}",
                    args.len()
                )));
            }
            let max_bytes = parse_tcp_max_bytes(args.first(), 65536, "tcp_stream.read_bytes")?;
            let explicit_ctx = parse_optional_tcp_ctx(args.get(1), "tcp_stream.read_bytes")?;
            let eff_ctx = explicit_ctx.or_else(|| core.task_context.clone());
            let stream = Arc::clone(stream);
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                let bytes = stream.read_bytes(max_bytes, eff_ctx.as_ref()).await?;
                let vals: Vec<Value> = bytes.into_iter().map(|b| Value::Int(b as i64)).collect();
                Ok(Value::List(vals.into()))
            })))
        }
        "read_line" => {
            if args.len() > 2 {
                return Err(MoraError::Runtime(format!(
                    "tcp_stream.read_line(max_bytes?, ctx?) expects 0..=2 arguments, got {}",
                    args.len()
                )));
            }
            let max_bytes = parse_tcp_max_bytes(args.first(), 65536, "tcp_stream.read_line")?;
            let explicit_ctx = parse_optional_tcp_ctx(args.get(1), "tcp_stream.read_line")?;
            let eff_ctx = explicit_ctx.or_else(|| core.task_context.clone());
            let stream = Arc::clone(stream);
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                let bytes = stream.read_line_bytes(max_bytes, eff_ctx.as_ref()).await?;
                let text = String::from_utf8(bytes).map_err(|e| {
                    MoraError::Io(format!("tcp_stream.read_line: invalid UTF-8 line: {e}"))
                })?;
                Ok(Value::String(text))
            })))
        }
        "write" => {
            if args.is_empty() || args.len() > 2 {
                return Err(MoraError::Runtime(format!(
                    "tcp_stream.write(data, ctx?) expects 1..=2 arguments, got {}",
                    args.len()
                )));
            }
            let Value::String(data) = &args[0] else {
                return Err(MoraError::Runtime(format!(
                    "tcp_stream.write(data, ctx?): expected string, got {}",
                    crate::flow::type_name(&args[0])
                )));
            };
            let explicit_ctx = parse_optional_tcp_ctx(args.get(1), "tcp_stream.write")?;
            let eff_ctx = explicit_ctx.or_else(|| core.task_context.clone());
            let stream = Arc::clone(stream);
            let payload = data.as_bytes().to_vec();
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                let n = stream.write_bytes(&payload, eff_ctx.as_ref()).await?;
                Ok(Value::Int(n as i64))
            })))
        }
        "write_bytes" => {
            if args.is_empty() || args.len() > 2 {
                return Err(MoraError::Runtime(format!(
                    "tcp_stream.write_bytes(bytes, ctx?) expects 1..=2 arguments, got {}",
                    args.len()
                )));
            }
            let Value::List(items) = &args[0] else {
                return Err(MoraError::Runtime(format!(
                    "tcp_stream.write_bytes(bytes, ctx?): expected list<int>, got {}",
                    crate::flow::type_name(&args[0])
                )));
            };
            let mut raw = Vec::with_capacity(items.len());
            for item in items.iter() {
                match item {
                    Value::Int(n) if (0..=255).contains(n) => raw.push(*n as u8),
                    Value::Int(n) => {
                        return Err(MoraError::Runtime(format!(
                            "tcp_stream.write_bytes: byte value {n} out of range 0..=255"
                        )));
                    }
                    other => {
                        return Err(MoraError::Runtime(format!(
                            "tcp_stream.write_bytes: expected int byte 0..=255, got {}",
                            crate::flow::type_name(other)
                        )));
                    }
                }
            }
            let explicit_ctx = parse_optional_tcp_ctx(args.get(1), "tcp_stream.write_bytes")?;
            let eff_ctx = explicit_ctx.or_else(|| core.task_context.clone());
            let stream = Arc::clone(stream);
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                let n = stream.write_bytes(&raw, eff_ctx.as_ref()).await?;
                Ok(Value::Int(n as i64))
            })))
        }
        "shutdown" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "tcp_stream.shutdown() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            let stream = Arc::clone(stream);
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                stream.shutdown().await?;
                Ok(Value::Nil)
            })))
        }
        "close" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "tcp_stream.close() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            let stream = Arc::clone(stream);
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                stream.close().await;
                Ok(Value::Nil)
            })))
        }
        "is_closed" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "tcp_stream.is_closed() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            Ok(CallAction::Done(Value::Bool(stream.is_closed())))
        }
        "local_addr" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "tcp_stream.local_addr() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            Ok(CallAction::Done(Value::String(
                stream.local_addr().to_string(),
            )))
        }
        "peer_addr" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "tcp_stream.peer_addr() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            Ok(CallAction::Done(Value::String(
                stream.peer_addr().to_string(),
            )))
        }
        _ => Err(MoraError::Runtime(format!(
            "TcpStream has no method: {method}"
        ))),
    }
}

fn parse_tls_alpn_opt(val: Option<&Value>, fn_name: &str) -> MoraResult<Vec<String>> {
    match val {
        None | Some(Value::Nil) => Ok(Vec::new()),
        Some(Value::List(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items.iter() {
                let Value::String(s) = item else {
                    return Err(MoraError::Runtime(format!(
                        "{fn_name}: config.alpn elements must be strings, got {}",
                        crate::flow::type_name(item)
                    )));
                };
                out.push(s.clone());
            }
            Ok(out)
        }
        Some(other) => Err(MoraError::Runtime(format!(
            "{fn_name}: config.alpn expected list<string> or nil, got {}",
            crate::flow::type_name(other)
        ))),
    }
}

fn extract_host_from_addr(addr: &str) -> String {
    let trimmed = addr.trim();
    if let Some(rest) = trimmed.strip_prefix('[')
        && let Some(end) = rest.find(']')
    {
        return rest[..end].to_string();
    }
    if let Some((host, _port)) = trimmed.rsplit_once(':') {
        host.to_string()
    } else {
        trimmed.to_string()
    }
}

fn resolve_tls_listener_method(
    listener: &Arc<crate::runtime::tls::MoraTlsListener>,
    method: &str,
    args: Vec<Value>,
    core: &CoreRuntime,
) -> MoraResult<CallAction> {
    match method {
        "accept" => {
            if args.len() > 1 {
                return Err(MoraError::Runtime(format!(
                    "tls_listener.accept(ctx?) expects 0..=1 arguments, got {}",
                    args.len()
                )));
            }
            let explicit_ctx = parse_optional_tcp_ctx(args.first(), "tls_listener.accept")?;
            let eff_ctx = explicit_ctx.or_else(|| core.task_context.clone());
            let listener = Arc::clone(listener);
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                let stream = listener.accept(eff_ctx.as_ref()).await?;
                Ok(Value::TlsStream(stream))
            })))
        }
        "local_addr" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "tls_listener.local_addr() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            Ok(CallAction::Done(Value::String(
                listener.local_addr().to_string(),
            )))
        }
        "close" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "tls_listener.close() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            listener.close();
            Ok(CallAction::Done(Value::Nil))
        }
        "is_closed" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "tls_listener.is_closed() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            Ok(CallAction::Done(Value::Bool(listener.is_closed())))
        }
        _ => Err(MoraError::Runtime(format!(
            "TlsListener has no method: {method}"
        ))),
    }
}

fn resolve_tls_stream_method(
    stream: &Arc<crate::runtime::tls::MoraTlsStream>,
    method: &str,
    args: Vec<Value>,
    core: &CoreRuntime,
) -> MoraResult<CallAction> {
    match method {
        "read" => {
            if args.len() > 2 {
                return Err(MoraError::Runtime(format!(
                    "tls_stream.read(max_bytes?, ctx?) expects 0..=2 arguments, got {}",
                    args.len()
                )));
            }
            let max_bytes = parse_tcp_max_bytes(args.first(), 65536, "tls_stream.read")?;
            let explicit_ctx = parse_optional_tcp_ctx(args.get(1), "tls_stream.read")?;
            let eff_ctx = explicit_ctx.or_else(|| core.task_context.clone());
            let stream = Arc::clone(stream);
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                let bytes = stream.read_bytes(max_bytes, eff_ctx.as_ref()).await?;
                let text = String::from_utf8(bytes).map_err(|e| {
                    MoraError::Io(format!("tls_stream.read: invalid UTF-8 payload: {e}"))
                })?;
                Ok(Value::String(text))
            })))
        }
        "read_bytes" => {
            if args.len() > 2 {
                return Err(MoraError::Runtime(format!(
                    "tls_stream.read_bytes(max_bytes?, ctx?) expects 0..=2 arguments, got {}",
                    args.len()
                )));
            }
            let max_bytes = parse_tcp_max_bytes(args.first(), 65536, "tls_stream.read_bytes")?;
            let explicit_ctx = parse_optional_tcp_ctx(args.get(1), "tls_stream.read_bytes")?;
            let eff_ctx = explicit_ctx.or_else(|| core.task_context.clone());
            let stream = Arc::clone(stream);
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                let bytes = stream.read_bytes(max_bytes, eff_ctx.as_ref()).await?;
                let vals: Vec<Value> = bytes.into_iter().map(|b| Value::Int(b as i64)).collect();
                Ok(Value::List(vals.into()))
            })))
        }
        "read_line" => {
            if args.len() > 2 {
                return Err(MoraError::Runtime(format!(
                    "tls_stream.read_line(max_bytes?, ctx?) expects 0..=2 arguments, got {}",
                    args.len()
                )));
            }
            let max_bytes = parse_tcp_max_bytes(args.first(), 65536, "tls_stream.read_line")?;
            let explicit_ctx = parse_optional_tcp_ctx(args.get(1), "tls_stream.read_line")?;
            let eff_ctx = explicit_ctx.or_else(|| core.task_context.clone());
            let stream = Arc::clone(stream);
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                let bytes = stream.read_line_bytes(max_bytes, eff_ctx.as_ref()).await?;
                let text = String::from_utf8(bytes).map_err(|e| {
                    MoraError::Io(format!("tls_stream.read_line: invalid UTF-8 line: {e}"))
                })?;
                Ok(Value::String(text))
            })))
        }
        "write" => {
            if args.is_empty() || args.len() > 2 {
                return Err(MoraError::Runtime(format!(
                    "tls_stream.write(data, ctx?) expects 1..=2 arguments, got {}",
                    args.len()
                )));
            }
            let Value::String(data) = &args[0] else {
                return Err(MoraError::Runtime(format!(
                    "tls_stream.write(data, ctx?): expected string, got {}",
                    crate::flow::type_name(&args[0])
                )));
            };
            let explicit_ctx = parse_optional_tcp_ctx(args.get(1), "tls_stream.write")?;
            let eff_ctx = explicit_ctx.or_else(|| core.task_context.clone());
            let stream = Arc::clone(stream);
            let payload = data.as_bytes().to_vec();
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                let n = stream.write_bytes(&payload, eff_ctx.as_ref()).await?;
                Ok(Value::Int(n as i64))
            })))
        }
        "write_bytes" => {
            if args.is_empty() || args.len() > 2 {
                return Err(MoraError::Runtime(format!(
                    "tls_stream.write_bytes(bytes, ctx?) expects 1..=2 arguments, got {}",
                    args.len()
                )));
            }
            let Value::List(items) = &args[0] else {
                return Err(MoraError::Runtime(format!(
                    "tls_stream.write_bytes(bytes, ctx?): expected list<int>, got {}",
                    crate::flow::type_name(&args[0])
                )));
            };
            let mut raw = Vec::with_capacity(items.len());
            for item in items.iter() {
                match item {
                    Value::Int(n) if (0..=255).contains(n) => raw.push(*n as u8),
                    Value::Int(n) => {
                        return Err(MoraError::Runtime(format!(
                            "tls_stream.write_bytes: byte value {n} out of range 0..=255"
                        )));
                    }
                    other => {
                        return Err(MoraError::Runtime(format!(
                            "tls_stream.write_bytes: expected int byte 0..=255, got {}",
                            crate::flow::type_name(other)
                        )));
                    }
                }
            }
            let explicit_ctx = parse_optional_tcp_ctx(args.get(1), "tls_stream.write_bytes")?;
            let eff_ctx = explicit_ctx.or_else(|| core.task_context.clone());
            let stream = Arc::clone(stream);
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                let n = stream.write_bytes(&raw, eff_ctx.as_ref()).await?;
                Ok(Value::Int(n as i64))
            })))
        }
        "shutdown" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "tls_stream.shutdown() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            let stream = Arc::clone(stream);
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                stream.shutdown().await?;
                Ok(Value::Nil)
            })))
        }
        "close" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "tls_stream.close() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            let stream = Arc::clone(stream);
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                stream.close().await;
                Ok(Value::Nil)
            })))
        }
        "is_closed" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "tls_stream.is_closed() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            Ok(CallAction::Done(Value::Bool(stream.is_closed())))
        }
        "local_addr" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "tls_stream.local_addr() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            Ok(CallAction::Done(Value::String(
                stream.local_addr().to_string(),
            )))
        }
        "peer_addr" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "tls_stream.peer_addr() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            Ok(CallAction::Done(Value::String(
                stream.peer_addr().to_string(),
            )))
        }
        "tls_version" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "tls_stream.tls_version() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            Ok(CallAction::Done(Value::String(
                stream.tls_version().to_string(),
            )))
        }
        "cipher_suite" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "tls_stream.cipher_suite() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            Ok(CallAction::Done(Value::String(
                stream.cipher_suite().to_string(),
            )))
        }
        "alpn_protocol" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "tls_stream.alpn_protocol() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            Ok(CallAction::Done(Value::String(
                stream.alpn_protocol().unwrap_or("").to_string(),
            )))
        }
        _ => Err(MoraError::Runtime(format!(
            "TlsStream has no method: {method}"
        ))),
    }
}

fn resolve_compute_dag_method(
    dag: &Arc<crate::runtime::compute_dag::TensorComputeDag>,
    method: &str,
    args: Vec<Value>,
    core: &CoreRuntime,
) -> MoraResult<CallAction> {
    match method {
        "fuse" | "compile" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "ComputeDag.{method}() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            let compiled = dag.compile_with_cache(core.shared().dag_cache());
            Ok(CallAction::Done(Value::ComputeDag(Arc::new(compiled))))
        }
        "run" => {
            let raw_inputs: Vec<Value> = if args.len() == 1
                && let Value::List(xs) = &args[0]
            {
                xs.iter().cloned().collect()
            } else {
                args
            };
            let mut tensors = Vec::with_capacity(raw_inputs.len());
            for (i, v) in raw_inputs.into_iter().enumerate() {
                match v {
                    Value::Tensor(t) => tensors.push(t),
                    Value::Float(f) => tensors.push(crate::value::tensor::Tensor::from_raw(
                        vec![f],
                        Vec::new(),
                        crate::common::DType::F64,
                        crate::common::Device::Cpu,
                        false,
                    )?),
                    Value::Scalar(s) => tensors.push(crate::value::tensor::Tensor::from_raw(
                        vec![s.as_f64()],
                        Vec::new(),
                        s.dtype(),
                        crate::common::Device::Cpu,
                        false,
                    )?),
                    other => {
                        return Err(MoraError::Runtime(format!(
                            "ComputeDag.run: argument #{i} must be Tensor or numeric scalar, got {}",
                            crate::flow::dynamic_type_name(&other)
                        )));
                    }
                }
            }
            let out = dag.run_with_cache(core.shared().dag_cache(), &tensors)?;
            Ok(CallAction::Done(Value::Tensor(out)))
        }
        "run_profiled" => {
            let raw_inputs: Vec<Value> = if args.len() == 1
                && let Value::List(xs) = &args[0]
            {
                xs.iter().cloned().collect()
            } else {
                args
            };
            let mut tensors = Vec::with_capacity(raw_inputs.len());
            for (i, v) in raw_inputs.into_iter().enumerate() {
                match v {
                    Value::Tensor(t) => tensors.push(t),
                    Value::Float(f) => tensors.push(crate::value::tensor::Tensor::from_raw(
                        vec![f],
                        Vec::new(),
                        crate::common::DType::F64,
                        crate::common::Device::Cpu,
                        false,
                    )?),
                    Value::Scalar(s) => tensors.push(crate::value::tensor::Tensor::from_raw(
                        vec![s.as_f64()],
                        Vec::new(),
                        s.dtype(),
                        crate::common::Device::Cpu,
                        false,
                    )?),
                    other => {
                        return Err(MoraError::Runtime(format!(
                            "ComputeDag.run_profiled: argument #{i} must be Tensor or numeric scalar, got {}",
                            crate::flow::dynamic_type_name(&other)
                        )));
                    }
                }
            }
            let (out, dur_ns, events) = dag.run_profiled(core.shared().dag_cache(), &tensors)?;
            let mut m = std::collections::HashMap::new();
            m.insert("output".to_string(), Value::Tensor(out));
            m.insert("duration_ns".to_string(), Value::Int(dur_ns as i64));
            m.insert(
                "duration_us".to_string(),
                Value::Float(dur_ns as f64 / 1000.0),
            );
            m.insert(
                "events".to_string(),
                crate::runtime::profiler::events_to_mora_value(&events),
            );
            m.insert(
                "chrome_json".to_string(),
                Value::String(crate::runtime::profiler::format_chrome_trace_json(&events)),
            );
            Ok(CallAction::Done(Value::Dict(m)))
        }
        "node_count" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "ComputeDag.node_count() expects 0 arguments".to_string(),
                ));
            }
            Ok(CallAction::Done(Value::Int(dag.node_count() as i64)))
        }
        "fused_count" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "ComputeDag.fused_count() expects 0 arguments".to_string(),
                ));
            }
            Ok(CallAction::Done(Value::Int(dag.fused_count() as i64)))
        }
        "is_fused" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "ComputeDag.is_fused() expects 0 arguments".to_string(),
                ));
            }
            Ok(CallAction::Done(Value::Bool(dag.fused)))
        }
        "op_names" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "ComputeDag.op_names() expects 0 arguments".to_string(),
                ));
            }
            let names: Vec<Value> = dag
                .op_names()
                .into_iter()
                .map(|s| Value::String(s.to_string()))
                .collect();
            Ok(CallAction::Done(Value::List(names.into())))
        }
        "to_dot" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "ComputeDag.to_dot() expects 0 arguments".to_string(),
                ));
            }
            Ok(CallAction::Done(Value::String(dag.to_dot())))
        }
        "cache_key" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "ComputeDag.cache_key() expects 0 arguments".to_string(),
                ));
            }
            Ok(CallAction::Done(Value::String(format!(
                "{:016x}",
                dag.fingerprint()
            ))))
        }
        "memory_plan" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "ComputeDag.memory_plan() expects 0 arguments".to_string(),
                ));
            }
            let mem = dag.memory_plan();
            let mut m = std::collections::HashMap::new();
            m.insert(
                "total_intermediates".to_string(),
                Value::Int(mem.total_intermediates as i64),
            );
            m.insert(
                "buffer_slots".to_string(),
                Value::Int(mem.buffer_slots as i64),
            );
            m.insert(
                "naive_peak_bytes".to_string(),
                Value::Int(mem.naive_peak_bytes as i64),
            );
            m.insert(
                "planned_peak_bytes".to_string(),
                Value::Int(mem.planned_peak_bytes as i64),
            );
            m.insert(
                "reuse_count".to_string(),
                Value::Int(mem.reuse_count as i64),
            );
            Ok(CallAction::Done(Value::Dict(m)))
        }
        "cost_report" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "ComputeDag.cost_report() expects 0 arguments".to_string(),
                ));
            }
            let rep = dag.cost_report();
            let mut m = std::collections::HashMap::new();
            m.insert(
                "total_flops".to_string(),
                Value::Int(rep.total_flops as i64),
            );
            m.insert(
                "total_bytes_read".to_string(),
                Value::Int(rep.total_bytes_read as i64),
            );
            m.insert(
                "total_bytes_written".to_string(),
                Value::Int(rep.total_bytes_written as i64),
            );
            m.insert(
                "total_memory_bytes".to_string(),
                Value::Int(rep.total_memory_bytes as i64),
            );
            m.insert(
                "arithmetic_intensity".to_string(),
                Value::Float(rep.arithmetic_intensity),
            );
            m.insert(
                "bottleneck".to_string(),
                Value::String(rep.bottleneck.to_string()),
            );
            m.insert(
                "fusion_bytes_saved".to_string(),
                Value::Int(rep.fusion_bytes_saved as i64),
            );
            m.insert(
                "fused_nodes".to_string(),
                Value::Int(rep.fused_nodes as i64),
            );
            m.insert(
                "total_nodes".to_string(),
                Value::Int(rep.total_nodes as i64),
            );
            m.insert(
                "algebraic_rewrites".to_string(),
                Value::Int(rep.algebraic_rewrites as i64),
            );
            m.insert(
                "native_jit_eligible_nodes".to_string(),
                Value::Int(rep.native_jit_eligible_nodes as i64),
            );
            m.insert(
                "native_jit_isa".to_string(),
                Value::String(rep.native_jit_isa.to_string()),
            );
            m.insert(
                "native_jit_compiled".to_string(),
                Value::Int(rep.native_jit_compiled as i64),
            );
            m.insert(
                "native_jit_cache_hits".to_string(),
                Value::Int(rep.native_jit_cache_hits as i64),
            );
            m.insert(
                "native_jit_dispatches".to_string(),
                Value::Int(rep.native_jit_dispatches as i64),
            );
            let node_list: Vec<Value> = rep
                .node_costs
                .into_iter()
                .map(|nc| {
                    let mut nm = std::collections::HashMap::new();
                    nm.insert("node_id".to_string(), Value::Int(nc.node_id as i64));
                    nm.insert("op".to_string(), Value::String(nc.op.to_string()));
                    nm.insert("flops".to_string(), Value::Int(nc.flops as i64));
                    nm.insert("bytes_read".to_string(), Value::Int(nc.bytes_read as i64));
                    nm.insert(
                        "bytes_written".to_string(),
                        Value::Int(nc.bytes_written as i64),
                    );
                    nm.insert(
                        "arithmetic_intensity".to_string(),
                        Value::Float(nc.arithmetic_intensity),
                    );
                    nm.insert("bound".to_string(), Value::String(nc.bound.to_string()));
                    Value::Dict(nm)
                })
                .collect();
            m.insert("nodes".to_string(), Value::List(node_list.into()));
            Ok(CallAction::Done(Value::Dict(m)))
        }
        "cache_stats" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "ComputeDag.cache_stats() expects 0 arguments".to_string(),
                ));
            }
            let stats = core.shared().dag_cache().stats();
            let mut m = std::collections::HashMap::new();
            m.insert("hits".to_string(), Value::Int(stats.hits as i64));
            m.insert("misses".to_string(), Value::Int(stats.misses as i64));
            m.insert("entries".to_string(), Value::Int(stats.entries as i64));
            Ok(CallAction::Done(Value::Dict(m)))
        }
        "backend_info" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "ComputeDag.backend_info() expects 0 arguments".to_string(),
                ));
            }
            let dev = dag.output_device();
            let backend = crate::runtime::compute_dag::backend_for_device(&dev);
            let mut m = std::collections::HashMap::new();
            for (k, v) in crate::runtime::gpu::hardware_device_info_map(&dev) {
                m.insert(k, Value::String(v));
            }
            m.insert("device".to_string(), Value::String(dev.to_string()));
            m.insert(
                "backend".to_string(),
                Value::String(backend.name().to_string()),
            );
            m.insert(
                "simd_isa".to_string(),
                Value::String(backend.simd_isa().to_string()),
            );
            m.insert(
                "worker_threads".to_string(),
                Value::String(backend.worker_threads().to_string()),
            );
            Ok(CallAction::Done(Value::Dict(m)))
        }
        _ => Err(MoraError::Runtime(format!(
            "ComputeDag has no method: {method}"
        ))),
    }
}

fn resolve_gpu_kernel_method(
    kernel: &Arc<crate::runtime::gpu::kernel_compiler::CompiledGpuKernel>,
    method: &str,
    args: Vec<Value>,
) -> MoraResult<CallAction> {
    match method {
        "name" => Ok(CallAction::Done(Value::String(kernel.name().to_string()))),
        "params" => Ok(CallAction::Done(Value::List(
            kernel
                .params()
                .iter()
                .map(|p| Value::String(format!("{}: {}", p.name, p.ty.canonical_name())))
                .collect(),
        ))),
        "emit_ptx" => Ok(CallAction::Done(Value::String(kernel.emit_ptx()))),
        "emit_msl" => Ok(CallAction::Done(Value::String(kernel.emit_msl()))),
        "emit_spirv_words" => {
            let local_size_x = match args.first() {
                None => 64u32,
                Some(Value::Int(n)) if *n > 0 => *n as u32,
                Some(other) => {
                    return Err(MoraError::Runtime(format!(
                        "gpu_kernel.emit_spirv_words: local_size_x must be positive int, got {other}"
                    )));
                }
            };
            let words = kernel.emit_spirv_words(local_size_x)?;
            Ok(CallAction::Done(Value::List(
                words
                    .into_iter()
                    .map(|w| Value::Int(i64::from(w)))
                    .collect(),
            )))
        }
        "launch" => {
            if args.len() < 2 {
                return Err(MoraError::Runtime(format!(
                    "gpu_kernel '{}'.launch(grid, block, args...) expects at least 2 arguments, got {}",
                    kernel.name(),
                    args.len()
                )));
            }
            let grid = match &args[0] {
                Value::Int(n) if *n > 0 => *n as u32,
                other => {
                    return Err(MoraError::Runtime(format!(
                        "gpu_kernel '{}'.launch: grid must be a positive int, got {other}",
                        kernel.name()
                    )));
                }
            };
            let block = match &args[1] {
                Value::Int(n) if *n > 0 => *n as u32,
                other => {
                    return Err(MoraError::Runtime(format!(
                        "gpu_kernel '{}'.launch: block must be a positive int, got {other}",
                        kernel.name()
                    )));
                }
            };
            crate::runtime::gpu::kernel_compiler::launch_compiled_kernel(
                kernel,
                grid,
                block,
                &args[2..],
            )?;
            Ok(CallAction::Done(Value::Nil))
        }
        other => Err(MoraError::Runtime(format!(
            "GpuKernel '{}' has no method '{other}'",
            kernel.name()
        ))),
    }
}

fn is_callable_value(v: &Value) -> bool {
    matches!(
        v,
        Value::Closure { .. }
            | Value::Task { .. }
            | Value::Builtin(_)
            | Value::Compose(_)
            | Value::Partial(_, _)
            | Value::Curry { .. }
            | Value::GradFn { .. }
            | Value::Continuation(_)
    )
}

fn dispatch_with_watermarks(
    final_result: Value,
    mut fired: Vec<crate::runtime::kv_cache::TriggeredWatermark>,
) -> CallAction {
    if fired.is_empty() {
        return CallAction::Done(final_result);
    }
    let (first_cb, first_event) = fired.remove(0);
    CallAction::PushCombinator {
        combinator: Box::new(CombinatorFrame::WatermarkCallbacks {
            final_result,
            pending: fired,
        }),
        callee: first_cb,
        args: vec![Value::Dict(first_event)],
    }
}

fn parse_shape_arg(val: &Value, ctx: &str) -> MoraResult<Vec<usize>> {
    let Value::List(items) = val else {
        return Err(MoraError::Runtime(format!(
            "{ctx}: shape must be list<int>, got {}",
            crate::flow::type_name(val)
        )));
    };
    let mut dims = Vec::with_capacity(items.len());
    for item in items.iter() {
        match item {
            Value::Int(n) if *n >= 0 => dims.push(*n as usize),
            other => {
                return Err(MoraError::Runtime(format!(
                    "{ctx}: shape dimension must be non-negative int, got {other}"
                )));
            }
        }
    }
    Ok(dims)
}

fn parse_f64_arg(val: &Value, ctx: &str, label: &str) -> MoraResult<f64> {
    match val {
        Value::Float(f) => Ok(*f),
        Value::Int(n) => Ok(*n as f64),
        Value::Scalar(s) => Ok(s.as_f64()),
        other => Err(MoraError::Runtime(format!(
            "{ctx}: '{label}' must be a number, got {other}"
        ))),
    }
}

fn resolve_kv_allocator_method(
    kv: &Arc<crate::runtime::kv_cache::KVPageAllocator>,
    method: &str,
    args: Vec<Value>,
) -> MoraResult<CallAction> {
    let as_u64 = |v: &Value, label: &str| -> MoraResult<u64> {
        match v {
            Value::Int(n) if *n >= 0 => Ok(*n as u64),
            _ => Err(MoraError::Runtime(format!(
                "kv_allocator.{method}: '{label}' must be a non-negative integer, got {v}"
            ))),
        }
    };
    let as_i64 = |v: &Value, label: &str| -> MoraResult<i64> {
        match v {
            Value::Int(n) => Ok(*n),
            _ => Err(MoraError::Runtime(format!(
                "kv_allocator.{method}: '{label}' must be an integer, got {v}"
            ))),
        }
    };
    let as_usize = |v: &Value, label: &str| -> MoraResult<usize> {
        match v {
            Value::Int(n) if *n >= 0 => Ok(*n as usize),
            _ => Err(MoraError::Runtime(format!(
                "kv_allocator.{method}: '{label}' must be a non-negative integer, got {v}"
            ))),
        }
    };

    match method {
        "alloc_seq" => {
            if args.is_empty() || args.len() > 2 {
                return Err(MoraError::Runtime(
                    "kv_allocator.alloc_seq(seq_id, priority?) expects 1..=2 arguments".to_string(),
                ));
            }
            let seq_id = as_u64(&args[0], "seq_id")?;
            let priority = match args.get(1) {
                None | Some(Value::Nil) => 0,
                Some(v) => as_i64(v, "priority")?,
            };
            Ok(CallAction::Done(Value::Bool(
                kv.alloc_seq_with_priority(seq_id, priority)?,
            )))
        }
        "set_priority" => {
            if args.len() != 2 {
                return Err(MoraError::Runtime(
                    "kv_allocator.set_priority(seq_id, priority) expects 2 arguments".to_string(),
                ));
            }
            let seq_id = as_u64(&args[0], "seq_id")?;
            let priority = as_i64(&args[1], "priority")?;
            kv.set_priority(seq_id, priority)?;
            Ok(CallAction::Done(Value::Nil))
        }
        "seq_priority" => {
            if args.len() != 1 {
                return Err(MoraError::Runtime(
                    "kv_allocator.seq_priority(seq_id) expects 1 argument".to_string(),
                ));
            }
            let seq_id = as_u64(&args[0], "seq_id")?;
            Ok(CallAction::Done(Value::Int(kv.seq_priority(seq_id)?)))
        }
        "set_quota" => {
            if args.len() != 1 {
                return Err(MoraError::Runtime(
                    "kv_allocator.set_quota(max_bytes) expects 1 argument".to_string(),
                ));
            }
            let max_bytes = as_usize(&args[0], "max_bytes")?;
            kv.set_quota(max_bytes)?;
            Ok(CallAction::Done(Value::Nil))
        }
        "quota_bytes" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "kv_allocator.quota_bytes() expects 0 arguments".to_string(),
                ));
            }
            Ok(CallAction::Done(Value::Int(kv.quota_bytes() as i64)))
        }
        "on_watermark" => {
            if args.len() != 2 {
                return Err(MoraError::Runtime(
                    "kv_allocator.on_watermark(threshold, callback) expects 2 arguments"
                        .to_string(),
                ));
            }
            let threshold = parse_f64_arg(&args[0], "kv_allocator.on_watermark", "threshold")?;
            if !is_callable_value(&args[1]) {
                return Err(MoraError::Runtime(format!(
                    "kv_allocator.on_watermark: 'callback' must be callable, got {}",
                    crate::flow::type_name(&args[1])
                )));
            }
            kv.register_watermark(threshold, args[1].clone())?;
            Ok(CallAction::Done(Value::Nil))
        }
        "clear_watermarks" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "kv_allocator.clear_watermarks() expects 0 arguments".to_string(),
                ));
            }
            kv.clear_watermarks();
            Ok(CallAction::Done(Value::Nil))
        }
        "append_tokens" => {
            if args.len() != 2 {
                return Err(MoraError::Runtime(
                    "kv_allocator.append_tokens(seq_id, count) expects 2 arguments".to_string(),
                ));
            }
            let seq_id = as_u64(&args[0], "seq_id")?;
            let count = as_usize(&args[1], "count")?;
            let (raw_pages, fired) = kv.append_tokens_with_watermarks(seq_id, count)?;
            let pages: Vec<Value> = raw_pages
                .into_iter()
                .map(|p| Value::Int(p as i64))
                .collect();
            Ok(dispatch_with_watermarks(Value::List(pages.into()), fired))
        }
        "fork_seq" => {
            if args.len() != 2 {
                return Err(MoraError::Runtime(
                    "kv_allocator.fork_seq(src_seq_id, dst_seq_id) expects 2 arguments".to_string(),
                ));
            }
            let src = as_u64(&args[0], "src_seq_id")?;
            let dst = as_u64(&args[1], "dst_seq_id")?;
            let pages: Vec<Value> = kv
                .fork_seq(src, dst)?
                .into_iter()
                .map(|p| Value::Int(p as i64))
                .collect();
            Ok(CallAction::Done(Value::List(pages.into())))
        }
        "write_kv" => {
            if args.len() != 4 {
                return Err(MoraError::Runtime(
                    "kv_allocator.write_kv(seq_id, token_pos, k, v) expects 4 arguments"
                        .to_string(),
                ));
            }
            let seq_id = as_u64(&args[0], "seq_id")?;
            let token_pos = as_usize(&args[1], "token_pos")?;
            let Value::Tensor(k) = &args[2] else {
                return Err(MoraError::Runtime(
                    "kv_allocator.write_kv: 'k' must be a Tensor".to_string(),
                ));
            };
            let Value::Tensor(v) = &args[3] else {
                return Err(MoraError::Runtime(
                    "kv_allocator.write_kv: 'v' must be a Tensor".to_string(),
                ));
            };
            let fired = kv.write_kv_with_watermarks(seq_id, token_pos, k, v)?;
            Ok(dispatch_with_watermarks(Value::Nil, fired))
        }
        "read_kv" => {
            if args.len() != 1 {
                return Err(MoraError::Runtime(
                    "kv_allocator.read_kv(seq_id) expects 1 argument".to_string(),
                ));
            }
            let seq_id = as_u64(&args[0], "seq_id")?;
            let (kt, vt) = kv.read_kv(seq_id)?;
            Ok(CallAction::Done(Value::List(
                vec![Value::Tensor(kt), Value::Tensor(vt)].into(),
            )))
        }
        "free_seq" => {
            if args.len() != 1 {
                return Err(MoraError::Runtime(
                    "kv_allocator.free_seq(seq_id) expects 1 argument".to_string(),
                ));
            }
            let seq_id = as_u64(&args[0], "seq_id")?;
            Ok(CallAction::Done(Value::Int(kv.free_seq(seq_id)? as i64)))
        }
        "evict_lru" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "kv_allocator.evict_lru() expects 0 arguments".to_string(),
                ));
            }
            Ok(CallAction::Done(Value::Int(kv.evict_lru())))
        }
        "evict_preempt" => {
            if args.len() > 1 {
                return Err(MoraError::Runtime(
                    "kv_allocator.evict_preempt(max_victim_priority?) expects 0..=1 arguments"
                        .to_string(),
                ));
            }
            let limit = match args.first() {
                None | Some(Value::Nil) => None,
                Some(v) => Some(as_i64(v, "max_victim_priority")?),
            };
            Ok(CallAction::Done(Value::Int(kv.evict_preempt(limit))))
        }
        "reclaim_for" => {
            if args.len() != 2 {
                return Err(MoraError::Runtime(
                    "kv_allocator.reclaim_for(pages_needed, requester_priority) expects 2 arguments"
                        .to_string(),
                ));
            }
            let pages_needed = as_usize(&args[0], "pages_needed")?;
            let req_prio = as_i64(&args[1], "requester_priority")?;
            let evicted: Vec<Value> = kv
                .reclaim_for(pages_needed, req_prio)
                .into_iter()
                .map(|id| Value::Int(id as i64))
                .collect();
            Ok(CallAction::Done(Value::List(evicted.into())))
        }
        "seq_pages" => {
            if args.len() != 1 {
                return Err(MoraError::Runtime(
                    "kv_allocator.seq_pages(seq_id) expects 1 argument".to_string(),
                ));
            }
            let seq_id = as_u64(&args[0], "seq_id")?;
            let pages: Vec<Value> = kv
                .seq_pages(seq_id)?
                .into_iter()
                .map(|p| Value::Int(p as i64))
                .collect();
            Ok(CallAction::Done(Value::List(pages.into())))
        }
        "seq_tokens" => {
            if args.len() != 1 {
                return Err(MoraError::Runtime(
                    "kv_allocator.seq_tokens(seq_id) expects 1 argument".to_string(),
                ));
            }
            let seq_id = as_u64(&args[0], "seq_id")?;
            Ok(CallAction::Done(Value::Int(kv.seq_tokens(seq_id)? as i64)))
        }
        "free_pages" => Ok(CallAction::Done(Value::Int(kv.free_pages() as i64))),
        "used_pages" => Ok(CallAction::Done(Value::Int(kv.used_pages() as i64))),
        "total_pages" => Ok(CallAction::Done(Value::Int(kv.total_pages as i64))),
        "utilization" => Ok(CallAction::Done(Value::Float(kv.utilization()))),
        "bytes_used" => Ok(CallAction::Done(Value::Int(kv.bytes_used() as i64))),
        "stats" => Ok(CallAction::Done(Value::Dict(kv.stats_dict()))),
        _ => Err(MoraError::Runtime(format!(
            "KVAllocator has no method: {method}"
        ))),
    }
}

fn resolve_memory_pool_method(
    pool: &Arc<crate::runtime::memory_pool::MemoryPool>,
    method: &str,
    args: Vec<Value>,
) -> MoraResult<CallAction> {
    let parse_dtype_opt = |opt: Option<&Value>, ctx: &str| -> MoraResult<crate::common::DType> {
        match opt {
            None | Some(Value::Nil) => Ok(crate::common::DType::F64),
            Some(Value::String(s)) => crate::common::DType::from_name(s)
                .ok_or_else(|| MoraError::Runtime(format!("{ctx}: unknown dtype '{s}'"))),
            Some(other) => Err(MoraError::Runtime(format!(
                "{ctx}: dtype must be string, got {}",
                crate::flow::type_name(other)
            ))),
        }
    };

    match method {
        "alloc" | "zeros" => {
            if args.is_empty() || args.len() > 2 {
                return Err(MoraError::Runtime(format!(
                    "memory_pool.{method}(shape, dtype?) expects 1..=2 arguments"
                )));
            }
            let ctx = format!("memory_pool.{method}");
            let shape = parse_shape_arg(&args[0], &ctx)?;
            let dtype = parse_dtype_opt(args.get(1), &ctx)?;
            let (tensor, fired) = pool.alloc_tensor_with_watermarks(shape, dtype, 0.0)?;
            Ok(dispatch_with_watermarks(Value::Tensor(tensor), fired))
        }
        "full" => {
            if args.len() < 2 || args.len() > 3 {
                return Err(MoraError::Runtime(
                    "memory_pool.full(shape, value, dtype?) expects 2..=3 arguments".to_string(),
                ));
            }
            let shape = parse_shape_arg(&args[0], "memory_pool.full")?;
            let fill = parse_f64_arg(&args[1], "memory_pool.full", "value")?;
            let dtype = parse_dtype_opt(args.get(2), "memory_pool.full")?;
            let (tensor, fired) = pool.alloc_tensor_with_watermarks(shape, dtype, fill)?;
            Ok(dispatch_with_watermarks(Value::Tensor(tensor), fired))
        }
        "release" => {
            if args.len() != 1 {
                return Err(MoraError::Runtime(
                    "memory_pool.release(tensor) expects 1 argument".to_string(),
                ));
            }
            let Value::Tensor(t) = &args[0] else {
                return Err(MoraError::Runtime(format!(
                    "memory_pool.release: expected Tensor, got {}",
                    crate::flow::type_name(&args[0])
                )));
            };
            Ok(CallAction::Done(Value::Int(pool.release_tensor(t)? as i64)))
        }
        "trim" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "memory_pool.trim() expects 0 arguments".to_string(),
                ));
            }
            Ok(CallAction::Done(Value::Int(pool.trim() as i64)))
        }
        "set_quota" => {
            if args.len() != 1 {
                return Err(MoraError::Runtime(
                    "memory_pool.set_quota(max_bytes) expects 1 argument".to_string(),
                ));
            }
            let max_bytes = match &args[0] {
                Value::Int(n) if *n > 0 => *n as usize,
                other => {
                    return Err(MoraError::Runtime(format!(
                        "memory_pool.set_quota: 'max_bytes' must be positive int, got {other}"
                    )));
                }
            };
            pool.set_quota(max_bytes)?;
            Ok(CallAction::Done(Value::Nil))
        }
        "on_watermark" => {
            if args.len() != 2 {
                return Err(MoraError::Runtime(
                    "memory_pool.on_watermark(threshold, callback) expects 2 arguments".to_string(),
                ));
            }
            let threshold = parse_f64_arg(&args[0], "memory_pool.on_watermark", "threshold")?;
            if !is_callable_value(&args[1]) {
                return Err(MoraError::Runtime(format!(
                    "memory_pool.on_watermark: 'callback' must be callable, got {}",
                    crate::flow::type_name(&args[1])
                )));
            }
            pool.register_watermark(threshold, args[1].clone())?;
            Ok(CallAction::Done(Value::Nil))
        }
        "clear_watermarks" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "memory_pool.clear_watermarks() expects 0 arguments".to_string(),
                ));
            }
            pool.clear_watermarks();
            Ok(CallAction::Done(Value::Nil))
        }
        "active_bytes" => Ok(CallAction::Done(Value::Int(pool.active_bytes() as i64))),
        "cached_bytes" => Ok(CallAction::Done(Value::Int(pool.cached_bytes() as i64))),
        "max_bytes" => Ok(CallAction::Done(Value::Int(pool.max_bytes() as i64))),
        "utilization" => Ok(CallAction::Done(Value::Float(pool.utilization()))),
        "stats" => Ok(CallAction::Done(Value::Dict(pool.stats_dict()))),
        _ => Err(MoraError::Runtime(format!(
            "MemoryPool has no method: {method}"
        ))),
    }
}

fn resolve_process_group_method(
    pg: &Arc<crate::runtime::distributed::ProcessGroup>,
    method: &str,
    args: Vec<Value>,
    core: &CoreRuntime,
) -> MoraResult<CallAction> {
    match method {
        "rank" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "process_group.rank() expects 0 arguments".to_string(),
                ));
            }
            Ok(CallAction::Done(Value::Int(pg.rank() as i64)))
        }
        "world_size" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "process_group.world_size() expects 0 arguments".to_string(),
                ));
            }
            Ok(CallAction::Done(Value::Int(pg.world_size() as i64)))
        }
        "backend" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "process_group.backend() expects 0 arguments".to_string(),
                ));
            }
            Ok(CallAction::Done(Value::String(
                pg.backend_name().to_string(),
            )))
        }
        "ranks" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "process_group.ranks() expects 0 arguments".to_string(),
                ));
            }
            let handles: Vec<Value> = pg
                .inproc_ranks()?
                .into_iter()
                .map(Value::ProcessGroup)
                .collect();
            Ok(CallAction::Done(Value::List(handles.into())))
        }
        "stats" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "process_group.stats() expects 0 arguments".to_string(),
                ));
            }
            let map: std::collections::HashMap<String, Value> = pg
                .stats_map()
                .into_iter()
                .map(|(k, v)| (k, Value::Int(v)))
                .collect();
            Ok(CallAction::Done(Value::Dict(map)))
        }
        "all_reduce" => {
            if args.is_empty() || args.len() > 2 {
                return Err(MoraError::Runtime(format!(
                    "process_group.all_reduce(tensor, op?) expects 1..=2 arguments, got {}",
                    args.len()
                )));
            }
            let Value::Tensor(t) = args[0].clone() else {
                return Err(MoraError::Runtime(format!(
                    "process_group.all_reduce: first argument must be Tensor, got {}",
                    crate::flow::type_name(&args[0])
                )));
            };
            let op_name = match args.get(1) {
                None | Some(Value::Nil) => "sum",
                Some(Value::String(s)) => s.as_str(),
                Some(other) => {
                    return Err(MoraError::Runtime(format!(
                        "process_group.all_reduce: 'op' must be string, got {}",
                        crate::flow::type_name(other)
                    )));
                }
            };
            let op = crate::runtime::distributed::ReduceOp::parse(op_name)?;
            let pg = Arc::clone(pg);
            let eff_ctx = core.task_context.clone();
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                let out = pg.all_reduce(t, op, eff_ctx.as_ref()).await?;
                Ok(Value::Tensor(out))
            })))
        }
        "reduce_scatter" => {
            if args.is_empty() || args.len() > 2 {
                return Err(MoraError::Runtime(format!(
                    "process_group.reduce_scatter(tensor, op?) expects 1..=2 arguments, got {}",
                    args.len()
                )));
            }
            let Value::Tensor(t) = args[0].clone() else {
                return Err(MoraError::Runtime(format!(
                    "process_group.reduce_scatter: first argument must be Tensor, got {}",
                    crate::flow::type_name(&args[0])
                )));
            };
            let op_name = match args.get(1) {
                None | Some(Value::Nil) => "sum",
                Some(Value::String(s)) => s.as_str(),
                Some(other) => {
                    return Err(MoraError::Runtime(format!(
                        "process_group.reduce_scatter: 'op' must be string, got {}",
                        crate::flow::type_name(other)
                    )));
                }
            };
            let op = crate::runtime::distributed::ReduceOp::parse(op_name)?;
            let pg = Arc::clone(pg);
            let eff_ctx = core.task_context.clone();
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                let out = pg.reduce_scatter(t, op, eff_ctx.as_ref()).await?;
                Ok(Value::Tensor(out))
            })))
        }
        "all_gather" => {
            if args.len() != 1 {
                return Err(MoraError::Runtime(format!(
                    "process_group.all_gather(tensor) expects 1 argument, got {}",
                    args.len()
                )));
            }
            let Value::Tensor(t) = args[0].clone() else {
                return Err(MoraError::Runtime(format!(
                    "process_group.all_gather: argument must be Tensor, got {}",
                    crate::flow::type_name(&args[0])
                )));
            };
            let pg = Arc::clone(pg);
            let eff_ctx = core.task_context.clone();
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                let out = pg.all_gather(t, eff_ctx.as_ref()).await?;
                Ok(Value::Tensor(out))
            })))
        }
        "broadcast" => {
            if args.len() != 2 {
                return Err(MoraError::Runtime(format!(
                    "process_group.broadcast(tensor, root) expects 2 arguments, got {}",
                    args.len()
                )));
            }
            let Value::Tensor(t) = args[0].clone() else {
                return Err(MoraError::Runtime(format!(
                    "process_group.broadcast: first argument must be Tensor, got {}",
                    crate::flow::type_name(&args[0])
                )));
            };
            let root = match &args[1] {
                Value::Int(n) if *n >= 0 => *n as usize,
                other => {
                    return Err(MoraError::Runtime(format!(
                        "process_group.broadcast: 'root' must be a non-negative integer, got {other}"
                    )));
                }
            };
            let pg = Arc::clone(pg);
            let eff_ctx = core.task_context.clone();
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                let out = pg.broadcast(t, root, eff_ctx.as_ref()).await?;
                Ok(Value::Tensor(out))
            })))
        }
        "barrier" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "process_group.barrier() expects 0 arguments".to_string(),
                ));
            }
            let pg = Arc::clone(pg);
            let eff_ctx = core.task_context.clone();
            Ok(CallAction::AwaitFuture(Box::pin(async move {
                pg.barrier(eff_ctx.as_ref()).await?;
                Ok(Value::Nil)
            })))
        }
        _ => Err(MoraError::Runtime(format!(
            "ProcessGroup has no method: {method}"
        ))),
    }
}

fn resolve_trace_profiler_method(
    prof: &Arc<crate::runtime::profiler::TraceProfilerHandle>,
    method: &str,
    args: Vec<Value>,
) -> MoraResult<CallAction> {
    match method {
        "start" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "trace_profiler.start() expects 0 arguments".to_string(),
                ));
            }
            prof.start();
            Ok(CallAction::Done(Value::Nil))
        }
        "stop" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "trace_profiler.stop() expects 0 arguments".to_string(),
                ));
            }
            prof.stop();
            Ok(CallAction::Done(Value::Nil))
        }
        "clear" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "trace_profiler.clear() expects 0 arguments".to_string(),
                ));
            }
            prof.clear();
            Ok(CallAction::Done(Value::Nil))
        }
        "is_active" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "trace_profiler.is_active() expects 0 arguments".to_string(),
                ));
            }
            Ok(CallAction::Done(Value::Bool(prof.is_active())))
        }
        "now_ns" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "trace_profiler.now_ns() expects 0 arguments".to_string(),
                ));
            }
            Ok(CallAction::Done(Value::Int(prof.now_ns() as i64)))
        }
        "record" => {
            if args.len() < 4 || args.len() > 5 {
                return Err(MoraError::Runtime(format!(
                    "trace_profiler.record(cat, name, start_ns, dur_ns, args?) expects 4..=5 arguments, got {}",
                    args.len()
                )));
            }
            let Value::String(cat) = args[0].clone() else {
                return Err(MoraError::Runtime(format!(
                    "trace_profiler.record: 'cat' must be string, got {}",
                    crate::flow::type_name(&args[0])
                )));
            };
            let Value::String(name) = args[1].clone() else {
                return Err(MoraError::Runtime(format!(
                    "trace_profiler.record: 'name' must be string, got {}",
                    crate::flow::type_name(&args[1])
                )));
            };
            let start_ns = match &args[2] {
                Value::Int(n) if *n >= 0 => *n as u64,
                other => {
                    return Err(MoraError::Runtime(format!(
                        "trace_profiler.record: 'start_ns' must be non-negative int, got {other}"
                    )));
                }
            };
            let dur_ns = match &args[3] {
                Value::Int(n) if *n >= 0 => *n as u64,
                other => {
                    return Err(MoraError::Runtime(format!(
                        "trace_profiler.record: 'dur_ns' must be non-negative int, got {other}"
                    )));
                }
            };
            let mut extra = Vec::new();
            if let Some(arg_dict) = args.get(4) {
                match arg_dict {
                    Value::Dict(map) => {
                        let mut pairs: Vec<(String, String)> = map
                            .iter()
                            .map(|(k, v)| match v {
                                Value::String(s) => (k.clone(), s.clone()),
                                other => (k.clone(), other.to_string()),
                            })
                            .collect();
                        pairs.sort_by(|a, b| a.0.cmp(&b.0));
                        extra = pairs;
                    }
                    Value::Nil => {}
                    other => {
                        return Err(MoraError::Runtime(format!(
                            "trace_profiler.record: 'args' must be dict, got {}",
                            crate::flow::type_name(other)
                        )));
                    }
                }
            }
            prof.record(cat, name, start_ns, dur_ns, extra);
            Ok(CallAction::Done(Value::Nil))
        }
        "events" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "trace_profiler.events() expects 0 arguments".to_string(),
                ));
            }
            let evs = prof.snapshot();
            Ok(CallAction::Done(
                crate::runtime::profiler::events_to_mora_value(&evs),
            ))
        }
        "summary" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "trace_profiler.summary() expects 0 arguments".to_string(),
                ));
            }
            let evs = prof.snapshot();
            Ok(CallAction::Done(
                crate::runtime::profiler::summary_to_mora_value(&evs),
            ))
        }
        "to_chrome_json" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(
                    "trace_profiler.to_chrome_json() expects 0 arguments".to_string(),
                ));
            }
            let evs = prof.snapshot();
            Ok(CallAction::Done(Value::String(
                crate::runtime::profiler::format_chrome_trace_json(&evs),
            )))
        }
        _ => Err(MoraError::Runtime(format!(
            "TraceProfiler has no method: {method}"
        ))),
    }
}

/// Resolve an arbitrary runtime callable [`Value`] iteratively across Partial/Curry/Compose wrappers.
pub fn resolve_value_call(
    callee: Value,
    args: Vec<Value>,
    core: &mut CoreRuntime,
) -> MoraResult<CallAction> {
    resolve_value_call_named(callee, args, Vec::new(), core)
}

/// Resolve an arbitrary runtime callable [`Value`] with optional named keyword arguments.
pub fn resolve_value_call_named(
    mut callee: Value,
    mut args: Vec<Value>,
    mut named_args: Vec<(String, Value)>,
    core: &mut CoreRuntime,
) -> MoraResult<CallAction> {
    loop {
        match callee {
            Value::Task {
                program,
                fn_id,
                globals,
                ..
            } => {
                let frame = materialize_frame_named(
                    program,
                    fn_id,
                    globals,
                    Arc::new(Vec::new()),
                    &args,
                    &named_args,
                )?;
                return Ok(CallAction::PushFrame {
                    frame,
                    error_prefix: None,
                });
            }
            Value::Closure {
                program,
                fn_id,
                globals,
                upvalues,
                ..
            } => {
                let frame =
                    materialize_frame_named(program, fn_id, globals, upvalues, &args, &named_args)?;
                return Ok(CallAction::PushFrame {
                    frame,
                    error_prefix: None,
                });
            }
            Value::Macro {
                name,
                program,
                fn_id,
                globals,
                ..
            } => {
                let frame = materialize_frame_named(
                    program,
                    fn_id,
                    globals,
                    Arc::new(Vec::new()),
                    &args,
                    &named_args,
                )?;
                return Ok(CallAction::PushFrame {
                    frame,
                    error_prefix: Some(format!("macro '{name}' execution error: ")),
                });
            }
            Value::StructType(def) => {
                return construct_struct_instance(def, args, named_args);
            }
            Value::GradFn {
                func,
                argnums,
                return_value,
            } => {
                if !named_args.is_empty() {
                    return Err(MoraError::Runtime(
                        "grad() callable expects positional arguments when differentiating"
                            .to_string(),
                    ));
                }
                for &idx in &argnums {
                    if idx >= args.len() {
                        return Err(MoraError::Runtime(format!(
                            "grad: argnum {idx} out of bounds for call with {} argument(s)",
                            args.len()
                        )));
                    }
                }
                let tape_id = core.autograd.push_tape();
                let mut leaf_nodes: Vec<usize> = Vec::with_capacity(argnums.len());
                let mut scalar_flags: Vec<Option<crate::common::DType>> =
                    Vec::with_capacity(argnums.len());
                let mut tracked_args = args;

                for &idx in &argnums {
                    match tracked_args[idx].clone() {
                        Value::Tensor(t) => {
                            if !t.dtype.is_float() {
                                core.autograd.pop_tape(tape_id);
                                return Err(MoraError::Runtime(format!(
                                    "grad: cannot differentiate with respect to non-floating-point Tensor<{}>",
                                    t.dtype
                                )));
                            }
                            let Some((_, leaf_idx)) = core.autograd.register_leaf(
                                tape_id,
                                t.shape.clone(),
                                t.dtype,
                                t.device.clone(),
                            ) else {
                                core.autograd.pop_tape(tape_id);
                                return Err(MoraError::Runtime(
                                    "grad: failed to allocate autograd leaf".to_string(),
                                ));
                            };
                            leaf_nodes.push(leaf_idx);
                            scalar_flags.push(None);
                            tracked_args[idx] =
                                Value::Tensor(t.with_pushed_grad_node((tape_id, leaf_idx)));
                        }
                        Value::Float(f) => {
                            let Some((_, leaf_idx)) = core.autograd.register_leaf(
                                tape_id,
                                Vec::new(),
                                crate::common::DType::F64,
                                crate::common::Device::Cpu,
                            ) else {
                                core.autograd.pop_tape(tape_id);
                                return Err(MoraError::Runtime(
                                    "grad: failed to allocate scalar autograd leaf".to_string(),
                                ));
                            };
                            leaf_nodes.push(leaf_idx);
                            scalar_flags.push(Some(crate::common::DType::F64));
                            let t = crate::value::tensor::Tensor::from_raw(
                                vec![f],
                                Vec::new(),
                                crate::common::DType::F64,
                                crate::common::Device::Cpu,
                                true,
                            )?;
                            tracked_args[idx] =
                                Value::Tensor(t.with_pushed_grad_node((tape_id, leaf_idx)));
                        }
                        Value::Scalar(s) if s.dtype().is_float() => {
                            let dt = s.dtype();
                            let Some((_, leaf_idx)) = core.autograd.register_leaf(
                                tape_id,
                                Vec::new(),
                                dt,
                                crate::common::Device::Cpu,
                            ) else {
                                core.autograd.pop_tape(tape_id);
                                return Err(MoraError::Runtime(
                                    "grad: failed to allocate scalar autograd leaf".to_string(),
                                ));
                            };
                            leaf_nodes.push(leaf_idx);
                            scalar_flags.push(Some(dt));
                            let t = crate::value::tensor::Tensor::from_raw(
                                vec![s.as_f64()],
                                Vec::new(),
                                dt,
                                crate::common::Device::Cpu,
                                true,
                            )?;
                            tracked_args[idx] =
                                Value::Tensor(t.with_pushed_grad_node((tape_id, leaf_idx)));
                        }
                        other => {
                            core.autograd.pop_tape(tape_id);
                            return Err(MoraError::Runtime(format!(
                                "grad: argument {idx} must be a floating-point tensor or float scalar, got {}",
                                crate::flow::dynamic_type_name(&other)
                            )));
                        }
                    }
                }

                return Ok(CallAction::PushCombinator {
                    combinator: Box::new(CombinatorFrame::AutogradEval {
                        tape_id,
                        leaf_nodes,
                        scalar_flags,
                        return_value,
                    }),
                    callee: *func,
                    args: tracked_args,
                });
            }
            Value::Continuation(cont) => {
                if args.len() > 1 || !named_args.is_empty() {
                    return Err(MoraError::Runtime(format!(
                        "continuation expects 1 arg, got {}",
                        args.len()
                    )));
                }
                let val = args.into_iter().next().unwrap_or(Value::Nil);
                return Ok(CallAction::ResumeContinuation { cont, val });
            }
            Value::Partial(inner, bound) => {
                let mut combined = bound;
                combined.extend(args);
                callee = *inner;
                args = combined;
            }
            Value::Curry {
                func,
                arity,
                mut bound_args,
            } => {
                bound_args.extend(args);
                if bound_args.len() < arity {
                    return Ok(CallAction::Done(Value::Curry {
                        func,
                        arity,
                        bound_args,
                    }));
                }
                callee = *func;
                args = bound_args;
            }
            Value::Compose(funcs) => {
                if funcs.is_empty() {
                    return Ok(CallAction::Done(
                        args.into_iter().next().unwrap_or(Value::Nil),
                    ));
                }
                if funcs.len() == 1 {
                    callee = funcs.into_iter().next().expect("non-empty compose vec");
                    continue;
                }
                let first = funcs[0].clone();
                return Ok(CallAction::PushCombinator {
                    combinator: Box::new(CombinatorFrame::Compose { funcs, next_idx: 1 }),
                    callee: first,
                    args,
                });
            }
            Value::Builtin(kind) => {
                if let Some(id) = builtin_kind_to_fn_id(kind) {
                    return resolve_builtin_call_named(
                        id,
                        core,
                        args,
                        std::mem::take(&mut named_args),
                    );
                }
                let prefix = kind.to_string();
                return Err(MoraError::Runtime(format!(
                    "'{prefix}' is a module, not a function — call a method on it \
                     (e.g. {prefix}.<method>(...)), not {prefix}(...)"
                )));
            }
            other => {
                return Err(MoraError::Runtime(format!(
                    "Cannot call value of type {}",
                    crate::flow::dynamic_type_name(&other)
                )));
            }
        }
    }
}

/// Resolve a dynamic global symbol call (imported functions, `Trait::new(...)`, REPL vars, macros).
pub fn resolve_dyn_call(
    name: &str,
    args: Vec<Value>,
    core: &mut CoreRuntime,
    active_globals: Option<&(
        Arc<crate::mir::control::ControlProgram>,
        super::continuation::ModuleGlobals,
    )>,
) -> MoraResult<CallAction> {
    resolve_dyn_call_named(name, args, Vec::new(), core, active_globals)
}

/// Resolve a dynamic global symbol call with optional named keyword arguments.
pub fn resolve_dyn_call_named(
    name: &str,
    args: Vec<Value>,
    named_args: Vec<(String, Value)>,
    core: &mut CoreRuntime,
    active_globals: Option<&(
        Arc<crate::mir::control::ControlProgram>,
        super::continuation::ModuleGlobals,
    )>,
) -> MoraResult<CallAction> {
    if let Some(id) = BuiltinFnId::from_name(name) {
        return resolve_builtin_call_named(id, core, args, named_args);
    }

    if let Some(assoc) = name
        .strip_prefix("Tensor::")
        .or_else(|| name.strip_prefix("tensor::"))
    {
        return crate::value::tensor::call_tensor_associated_named(assoc, &args, &named_args, core)
            .map(CallAction::Done);
    }

    // Associated calls `Type::member(...)` for nominal structs, enums, and inherent impls.
    if let Some((type_name, member)) = name.split_once("::") {
        if let Some(sdef) = core.struct_registry.get(type_name).cloned() {
            match member {
                "new" => return construct_struct_instance(sdef, args, named_args),
                "from_dict" => {
                    let Some(Value::Dict(map)) = args.into_iter().next() else {
                        return Err(MoraError::Runtime(format!(
                            "{type_name}::from_dict(dict) expects a dict argument"
                        )));
                    };
                    let entries: Vec<(String, Value)> = map.into_iter().collect();
                    return construct_struct_instance(sdef, Vec::new(), entries);
                }
                "schema" => return Ok(CallAction::Done(sdef.to_schema_value())),
                "tags" => return Ok(CallAction::Done(sdef.to_tags_value())),
                "fields" => {
                    return Ok(CallAction::Done(Value::List(
                        sdef.fields
                            .iter()
                            .map(|f| Value::String(f.name.clone()))
                            .collect(),
                    )));
                }
                _ => {}
            }
        }
        if let Some(edef) = core.enum_registry.get(type_name).cloned() {
            if member == "variants" {
                if !args.is_empty() || !named_args.is_empty() {
                    return Err(MoraError::Runtime(format!(
                        "{type_name}::variants() expects 0 arguments, got {}",
                        args.len() + named_args.len()
                    )));
                }
                return Ok(CallAction::Done(Value::List(
                    edef.variants
                        .iter()
                        .map(|(v, _)| Value::String(v.clone()))
                        .collect(),
                )));
            }
            if edef.variants.iter().any(|(v, _)| v == member) {
                return construct_enum_variant_with_consts(
                    &edef,
                    member,
                    Vec::new(),
                    args,
                    named_args,
                );
            }
        }
        if let Some((func, has_self)) = core
            .inherent_methods
            .get(&(type_name.to_string(), member.to_string()))
            .cloned()
            && !has_self
        {
            return resolve_value_call_named(func, args, named_args, core);
        }
    }

    if let Some(val) = core.environment.get(name) {
        return dispatch_named_callable(name, val, args, named_args, core);
    }

    if let Some((prog, globals)) = active_globals {
        for (idx, meta) in prog.globals.iter().enumerate() {
            if meta.name == name {
                let val = globals[idx].lock().0.clone();
                return dispatch_named_callable(name, val, args, named_args, core);
            }
        }
    }

    if let Some(trait_name) = name.strip_suffix("::new")
        && core.trait_registry.contains_key(trait_name)
    {
        let for_type = match args.first() {
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => {
                return Err(MoraError::Runtime(format!(
                    "{name}() requires a target type name argument"
                )));
            }
        };
        let instance =
            construct_trait_instance_core(core, trait_name, &[], &for_type, &[], Span::default())?;
        return Ok(CallAction::Done(instance));
    }

    let prefix = name.split('.').next().unwrap_or(name);
    if MODULE_OBJECTS.iter().any(|(m, _)| *m == prefix) {
        return Err(MoraError::Runtime(format!(
            "'{name}' is a module, not a function — call a method on it \
             (e.g. {name}.<method>(...)), not {name}(...)"
        )));
    }

    Err(MoraError::Runtime(format!(
        "Undefined function or task: {name}"
    )))
}

fn dispatch_named_callable(
    name: &str,
    val: Value,
    args: Vec<Value>,
    named_args: Vec<(String, Value)>,
    core: &mut CoreRuntime,
) -> MoraResult<CallAction> {
    match val {
        Value::Task { .. }
        | Value::Closure { .. }
        | Value::Continuation(_)
        | Value::Macro { .. }
        | Value::Compose(_)
        | Value::Partial(_, _)
        | Value::Curry { .. }
        | Value::StructType(_)
        | Value::GradFn { .. } => resolve_value_call_named(val, args, named_args, core),
        Value::Builtin(kind) => {
            resolve_value_call_named(Value::Builtin(kind), args, named_args, core)
        }
        _ => {
            let prefix = name.split('.').next().unwrap_or(name);
            if MODULE_OBJECTS.iter().any(|(m, _)| *m == prefix) {
                Err(MoraError::Runtime(format!(
                    "'{name}' is a module, not a function — call a method on it \
                     (e.g. {name}.<method>(...)), not {name}(...)"
                )))
            } else {
                Err(MoraError::Runtime(format!("'{name}' is not callable")))
            }
        }
    }
}

/// G2: Field role classification derived from compile-time `@tag(...)` metadata on a `struct` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayerFieldRole {
    /// Trainable parameter or container traversed for both `parameters()` and `state_dict()`.
    Param,
    /// Persistent non-trainable buffer (`@tag(buffer="true")`): excluded from `parameters()`, included in `state_dict()`.
    Buffer,
    /// Ephemeral scratch (`@tag(transient="true")`): excluded from both `parameters()` and `state_dict()`.
    Transient,
    /// Explicitly non-parameter (`@tag(param="false")` without `buffer="true"`): excluded from `parameters()`, included in `state_dict()` if persistent tensor.
    NonParam,
}

fn classify_layer_field(def: &StructDefRuntime, field_name: &str) -> LayerFieldRole {
    let Some(fmeta) = def.fields.iter().find(|f| f.name == field_name) else {
        return LayerFieldRole::Param;
    };
    let tag_true = |key: &str| {
        fmeta
            .tags
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case(key) && v.eq_ignore_ascii_case("true"))
    };
    let tag_false = |key: &str| {
        fmeta
            .tags
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case(key) && v.eq_ignore_ascii_case("false"))
    };
    if tag_true("transient") {
        LayerFieldRole::Transient
    } else if tag_true("buffer") {
        LayerFieldRole::Buffer
    } else if tag_false("param") {
        LayerFieldRole::NonParam
    } else {
        LayerFieldRole::Param
    }
}

fn join_state_key(prefix: &str, segment: &str) -> String {
    if prefix.is_empty() {
        segment.to_string()
    } else {
        format!("{prefix}.{segment}")
    }
}

fn collect_struct_tensors(
    val: &Value,
    prefix: &str,
    include_buffers: bool,
    visit: &mut impl FnMut(String, Arc<crate::value::tensor::Tensor>),
) {
    match val {
        Value::Tensor(t) => {
            visit(prefix.to_string(), Arc::clone(t));
        }
        Value::StructInstance {
            fields,
            field_order,
            def,
            ..
        } => {
            for fname in field_order {
                let Some(fval) = fields.get(fname) else {
                    continue;
                };
                let role = classify_layer_field(def, fname);
                match role {
                    LayerFieldRole::Transient => continue,
                    LayerFieldRole::Buffer | LayerFieldRole::NonParam if !include_buffers => {
                        continue;
                    }
                    _ => {}
                }
                let child_key = join_state_key(prefix, fname);
                collect_struct_tensors(fval, &child_key, include_buffers, visit);
            }
        }
        Value::List(items) => {
            for (idx, item) in items.iter().enumerate() {
                let child_key = join_state_key(prefix, &idx.to_string());
                collect_struct_tensors(item, &child_key, include_buffers, visit);
            }
        }
        Value::Dict(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for k in keys {
                let child_key = join_state_key(prefix, k);
                if let Some(v) = map.get(k) {
                    collect_struct_tensors(v, &child_key, include_buffers, visit);
                }
            }
        }
        _ => {}
    }
}

fn extract_optional_prefix_arg(
    method: &str,
    args: &[Value],
    named_args: &[(String, Value)],
) -> MoraResult<String> {
    if args.len() + named_args.len() > 1 {
        return Err(MoraError::Runtime(format!(
            "{method}(prefix?) expects at most 1 argument"
        )));
    }
    let val = if let Some(v) = args.first() {
        Some(v)
    } else if let Some((k, v)) = named_args.first() {
        if k != "prefix" {
            return Err(MoraError::Runtime(format!(
                "{method}() got unexpected keyword argument '{k}'"
            )));
        }
        Some(v)
    } else {
        None
    };
    match val {
        None | Some(Value::Nil) => Ok(String::new()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(other) => Err(MoraError::Runtime(format!(
            "{method}(prefix) expects a string prefix, got {}",
            crate::flow::type_name(other)
        ))),
    }
}

fn extract_load_state_dict_args<'a>(
    type_name: &str,
    args: &'a [Value],
    named_args: &'a [(String, Value)],
) -> MoraResult<(&'a HashMap<String, Value>, bool)> {
    let mut state_val: Option<&Value> = args.first();
    let mut strict_val: Option<&Value> = args.get(1);
    if args.len() > 2 {
        return Err(MoraError::Runtime(format!(
            "{type_name}.load_state_dict(state, strict?) expects 1 or 2 arguments"
        )));
    }
    for (k, v) in named_args {
        match k.as_str() {
            "state" | "state_dict" => {
                if state_val.is_some() {
                    return Err(MoraError::Runtime(format!(
                        "{type_name}.load_state_dict() received multiple values for 'state'"
                    )));
                }
                state_val = Some(v);
            }
            "strict" => {
                if strict_val.is_some() {
                    return Err(MoraError::Runtime(format!(
                        "{type_name}.load_state_dict() received multiple values for 'strict'"
                    )));
                }
                strict_val = Some(v);
            }
            _ => {
                return Err(MoraError::Runtime(format!(
                    "{type_name}.load_state_dict() got unexpected keyword argument '{k}'"
                )));
            }
        }
    }
    let Some(Value::Dict(map)) = state_val else {
        return Err(MoraError::Runtime(format!(
            "{type_name}.load_state_dict(state, strict?) expects a dict as first argument"
        )));
    };
    let strict = match strict_val {
        None | Some(Value::Nil) => true,
        Some(Value::Bool(b)) => *b,
        Some(other) => {
            return Err(MoraError::Runtime(format!(
                "{type_name}.load_state_dict() expects bool for 'strict', got {}",
                crate::flow::type_name(other)
            )));
        }
    };
    Ok((map, strict))
}

fn load_struct_state_dict(
    root: &Value,
    state: &HashMap<String, Value>,
    strict: bool,
) -> MoraResult<Value> {
    let mut matched_keys = std::collections::HashSet::<String>::new();
    let mut missing_keys = Vec::<String>::new();
    let updated = rebuild_with_state_dict(root, "", state, &mut matched_keys, &mut missing_keys)?;
    if strict {
        let mut unexpected: Vec<String> = state
            .keys()
            .filter(|k| !matched_keys.contains(*k))
            .cloned()
            .collect();
        unexpected.sort();
        if !missing_keys.is_empty() || !unexpected.is_empty() {
            let mut parts = Vec::new();
            if !missing_keys.is_empty() {
                parts.push(format!("missing keys: [{}]", missing_keys.join(", ")));
            }
            if !unexpected.is_empty() {
                parts.push(format!("unexpected keys: [{}]", unexpected.join(", ")));
            }
            return Err(MoraError::Runtime(format!(
                "load_state_dict(strict=true) mismatch: {}",
                parts.join("; ")
            )));
        }
    }
    Ok(updated)
}

fn rebuild_with_state_dict(
    val: &Value,
    prefix: &str,
    state: &HashMap<String, Value>,
    matched_keys: &mut std::collections::HashSet<String>,
    missing_keys: &mut Vec<String>,
) -> MoraResult<Value> {
    match val {
        Value::Tensor(existing) => {
            if let Some(new_val) = state.get(prefix) {
                let Value::Tensor(new_t) = new_val else {
                    return Err(MoraError::Runtime(format!(
                        "load_state_dict: key '{prefix}' expected Tensor, got {}",
                        crate::flow::type_name(new_val)
                    )));
                };
                if new_t.shape != existing.shape {
                    return Err(MoraError::Runtime(format!(
                        "load_state_dict: shape mismatch for key '{prefix}': expected {:?}, got {:?}",
                        existing.shape, new_t.shape
                    )));
                }
                matched_keys.insert(prefix.to_string());
                Ok(Value::Tensor(Arc::clone(new_t)))
            } else {
                missing_keys.push(prefix.to_string());
                Ok(Value::Tensor(Arc::clone(existing)))
            }
        }
        Value::StructInstance {
            type_name,
            fields,
            field_order,
            const_args,
            def,
        } => {
            let mut new_fields = fields.clone();
            for fname in field_order {
                let Some(fval) = fields.get(fname) else {
                    continue;
                };
                if classify_layer_field(def, fname) == LayerFieldRole::Transient {
                    continue;
                }
                let child_key = join_state_key(prefix, fname);
                let updated =
                    rebuild_with_state_dict(fval, &child_key, state, matched_keys, missing_keys)?;
                new_fields.insert(fname.clone(), updated);
            }
            Ok(Value::StructInstance {
                type_name: type_name.clone(),
                fields: new_fields,
                field_order: field_order.clone(),
                const_args: const_args.clone(),
                def: Arc::clone(def),
            })
        }
        Value::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (idx, item) in items.iter().enumerate() {
                let child_key = join_state_key(prefix, &idx.to_string());
                out.push(rebuild_with_state_dict(
                    item,
                    &child_key,
                    state,
                    matched_keys,
                    missing_keys,
                )?);
            }
            Ok(Value::List(out.into()))
        }
        Value::Dict(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = HashMap::with_capacity(map.len());
            for k in keys {
                let child_key = join_state_key(prefix, k);
                if let Some(v) = map.get(k) {
                    out.insert(
                        k.clone(),
                        rebuild_with_state_dict(v, &child_key, state, matched_keys, missing_keys)?,
                    );
                }
            }
            Ok(Value::Dict(out))
        }
        other => Ok(other.clone()),
    }
}

fn move_struct_to_device(val: &Value, dev: &crate::common::Device) -> MoraResult<Value> {
    match val {
        Value::Tensor(t) => Ok(Value::Tensor(t.to_device(dev.clone())?)),
        Value::StructInstance {
            type_name,
            fields,
            field_order,
            const_args,
            def,
        } => {
            let mut new_fields = HashMap::with_capacity(fields.len());
            for (k, v) in fields {
                new_fields.insert(k.clone(), move_struct_to_device(v, dev)?);
            }
            Ok(Value::StructInstance {
                type_name: type_name.clone(),
                fields: new_fields,
                field_order: field_order.clone(),
                const_args: const_args.clone(),
                def: Arc::clone(def),
            })
        }
        Value::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items.iter() {
                out.push(move_struct_to_device(item, dev)?);
            }
            Ok(Value::List(out.into()))
        }
        Value::Dict(map) => {
            let mut out = HashMap::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), move_struct_to_device(v, dev)?);
            }
            Ok(Value::Dict(out))
        }
        other => Ok(other.clone()),
    }
}
