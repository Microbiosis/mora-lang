//! Stackful builtin, method, callable-value, and dynamic-lookup resolution (Wave 2 — Stage W2.3).
//!
//! Pure builtins and value methods delegate directly to the canonical shared helpers in
//! `crate::runtime::builtins::{impls, method, traits}`. Higher-order combinators, closures,
//! continuations, ambient random effects, `eval`, and `macroexpand` yield [`CallAction`] descriptors
//! executed iteratively by [`super::engine::VmEngine`] on its explicit heap call stack.

use std::sync::Arc;

use crate::common::Span;
use crate::error::{MoraError, MoraResult};
use crate::mir::control::BuiltinFnId;
use crate::mir::effect::ambient;
use crate::runtime::CoreRuntime;
use crate::runtime::builtins::impls::eval_pure_builtin;
use crate::runtime::builtins::method::eval_pure_method;
use crate::runtime::builtins::traits::{
    TraitMethodTarget, construct_trait_instance_core, resolve_trait_method_call,
};
use crate::value::{BuiltinKind, MODULE_OBJECTS, Value};

use super::continuation::{CombinatorFrame, ScriptFrame, materialize_frame};

/// Asynchronous or stackful call dispatch action returned by builtin/callee resolution.
pub enum CallAction {
    /// Immediate synchronous return value.
    Done(Value),
    /// Push a compiled `ScriptFrame` (optionally wrapping propagated errors with `error_prefix`).
    PushFrame {
        frame: ScriptFrame,
        error_prefix: Option<String>,
    },
    /// Push a [`CombinatorFrame`] and invoke its initial callback target + args.
    PushCombinator {
        combinator: CombinatorFrame,
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
                        combinator: CombinatorFrame::Swap {
                            atom: Arc::clone(arc),
                        },
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
                        combinator: CombinatorFrame::Into {
                            transform: transform.clone(),
                            items,
                            idx: 0,
                            results: Vec::new(),
                        },
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
    if let Some(res) = eval_pure_method(&receiver, method, &args) {
        return res.map(CallAction::Done);
    }

    match receiver {
        Value::TraitObject { .. } => {
            match resolve_trait_method_call(core, &receiver, method, args, call_site)? {
                TraitMethodTarget::Immediate(v) => Ok(CallAction::Done(v)),
                TraitMethodTarget::Call(task, all_args) => resolve_value_call(task, all_args, core),
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
                    combinator: CombinatorFrame::ListMap {
                        func: mapper.clone(),
                        results: Vec::with_capacity(items.len()),
                        items,
                        idx: 0,
                    },
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
                    combinator: CombinatorFrame::ListFilter {
                        pred: predicate.clone(),
                        items,
                        idx: 0,
                        results: Vec::new(),
                    },
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
                    combinator: CombinatorFrame::ListReduce {
                        func: reducer.clone(),
                        items,
                        idx: 0,
                    },
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
            core.scheduler()
                .block_on(ch.send_async(val, core.task_context.as_ref()))?;
            Ok(CallAction::Done(Value::Nil))
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
            core.scheduler().block_on(async {
                match core.task_context.as_ref() {
                    Some(outer) => {
                        outer.status()?;
                        tokio::select! {
                            biased;
                            err = outer.cancelled() => Err(err),
                            res = ch.send_async(val, Some(&explicit_ctx)) => res,
                        }
                    }
                    None => ch.send_async(val, Some(&explicit_ctx)).await,
                }
            })?;
            Ok(CallAction::Done(Value::Nil))
        }
        "recv" => {
            let val = core
                .scheduler()
                .block_on(ch.recv_async(core.task_context.as_ref()))?;
            Ok(CallAction::Done(val))
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
            let val = core.scheduler().block_on(async {
                match core.task_context.as_ref() {
                    Some(outer) => {
                        outer.status()?;
                        tokio::select! {
                            biased;
                            err = outer.cancelled() => Err(err),
                            res = ch.recv_async(Some(&explicit_ctx)) => res,
                        }
                    }
                    None => ch.recv_async(Some(&explicit_ctx)).await,
                }
            })?;
            Ok(CallAction::Done(val))
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
            let val = core
                .scheduler()
                .block_on(handle.join_async(core.task_context.as_ref()))?;
            Ok(CallAction::Done(val))
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

/// Resolve an arbitrary runtime callable [`Value`] iteratively across Partial/Curry/Compose wrappers.
pub fn resolve_value_call(
    mut callee: Value,
    mut args: Vec<Value>,
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
                let frame =
                    materialize_frame(program, fn_id, globals, Arc::new(Vec::new()), &args)?;
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
                let frame = materialize_frame(program, fn_id, globals, upvalues, &args)?;
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
                let frame =
                    materialize_frame(program, fn_id, globals, Arc::new(Vec::new()), &args)?;
                return Ok(CallAction::PushFrame {
                    frame,
                    error_prefix: Some(format!("macro '{name}' execution error: ")),
                });
            }
            Value::Continuation(cont) => {
                if args.len() > 1 {
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
                    combinator: CombinatorFrame::Compose { funcs, next_idx: 1 },
                    callee: first,
                    args,
                });
            }
            Value::Builtin(kind) => {
                if let Some(id) = builtin_kind_to_fn_id(kind) {
                    return resolve_builtin_call(id, core, args);
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
                    crate::flow::type_name(&other)
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
    if let Some(id) = BuiltinFnId::from_name(name) {
        return resolve_builtin_call(id, core, args);
    }

    if let Some(assoc) = name
        .strip_prefix("Tensor::")
        .or_else(|| name.strip_prefix("tensor::"))
    {
        return crate::value::tensor::call_tensor_associated(assoc, &args, core)
            .map(CallAction::Done);
    }

    if let Some(val) = core.environment.get(name) {
        return dispatch_named_callable(name, val, args, core);
    }

    if let Some((prog, globals)) = active_globals {
        for (idx, meta) in prog.globals.iter().enumerate() {
            if meta.name == name {
                let val = globals[idx].lock().0.clone();
                return dispatch_named_callable(name, val, args, core);
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
    core: &mut CoreRuntime,
) -> MoraResult<CallAction> {
    match val {
        Value::Task { .. }
        | Value::Closure { .. }
        | Value::Continuation(_)
        | Value::Macro { .. }
        | Value::Compose(_)
        | Value::Partial(_, _)
        | Value::Curry { .. } => resolve_value_call(val, args, core),
        Value::Builtin(kind) => resolve_value_call(Value::Builtin(kind), args, core),
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
