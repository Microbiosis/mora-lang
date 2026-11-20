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
    TraitMethodTarget, construct_trait_instance_core, resolve_concrete_impl_method,
    resolve_trait_method_call,
};
use crate::value::{BuiltinKind, MODULE_OBJECTS, StructDefRuntime, Value};

use super::continuation::{CombinatorFrame, ScriptFrame, materialize_frame_named};

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
            let listener = core
                .reactor()
                .scheduler()
                .block_on(crate::runtime::reactor::MoraTcpListener::bind(addr))?;
            Ok(CallAction::Done(Value::TcpListener(listener)))
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
            let eff_ctx = explicit_ctx.as_ref().or(core.task_context.as_ref());
            let stream = core.reactor().scheduler().block_on(
                crate::runtime::reactor::MoraTcpStream::connect(addr, timeout, eff_ctx),
            )?;
            Ok(CallAction::Done(Value::TcpStream(stream)))
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
                combinator: CombinatorFrame::JvpEval {
                    tape_id,
                    leaf_nodes,
                    leaf_tangents,
                    scalar_out_dtype,
                },
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
                combinator: CombinatorFrame::TraceDagEval {
                    tape_id,
                    leaf_nodes,
                },
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
                Some(Value::String(s)) => {
                    crate::common::Device::parse(s).map_err(MoraError::Runtime)?
                }
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
                Some(Value::String(s)) => {
                    crate::common::Device::parse(s).map_err(MoraError::Runtime)?
                }
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
        let field_order = def.fields.iter().map(|f| f.name.clone()).collect();
        return Ok(CallAction::Done(Value::StructInstance {
            type_name: def.name.clone(),
            fields,
            field_order,
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
        combinator: CombinatorFrame::StructInit {
            def,
            fields,
            pending_defaults,
            current_field: first_field,
        },
        callee: thunk,
        args: Vec::new(),
    })
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
    // Stage 3C: Tensor methods run with live autograd tape tracking first, then fall back to user impls.
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

    // Stage 3C: Nominal struct type & instance dispatch (field access, inherent/trait impls, schema/tags).
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
            // 3. Built-in struct instance reflection / projection methods
            match method {
                "to_dict" => return Ok(CallAction::Done(Value::Dict(fields.clone()))),
                "schema" => return Ok(CallAction::Done(def.to_schema_value())),
                "tags" => return Ok(CallAction::Done(def.to_tags_value())),
                "json" => {
                    return Ok(CallAction::Done(Value::String(crate::flow::value_to_json(
                        &receiver,
                    ))));
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
            if let Some((vname, payload_tys)) = def.variants.iter().find(|(v, _)| v == method) {
                if args.len() != payload_tys.len() {
                    return Err(MoraError::Runtime(format!(
                        "{}::{} expects {} payload arg(s), got {}",
                        def.name,
                        vname,
                        payload_tys.len(),
                        args.len()
                    )));
                }
                return Ok(CallAction::Done(Value::EnumVariant {
                    enum_name: def.name.clone(),
                    variant: vname.clone(),
                    payload: args,
                }));
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
        Value::ComputeDag(dag) => resolve_compute_dag_method(&dag, method, args, core),
        Value::KVAllocator(kv) => resolve_kv_allocator_method(&kv, method, args),
        Value::MemoryPool(pool) => resolve_memory_pool_method(&pool, method, args),
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
            let eff_ctx = explicit_ctx.as_ref().or(core.task_context.as_ref());
            let stream = core
                .reactor()
                .scheduler()
                .block_on(listener.accept(eff_ctx))?;
            Ok(CallAction::Done(Value::TcpStream(stream)))
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
            let eff_ctx = explicit_ctx.as_ref().or(core.task_context.as_ref());
            let bytes = core
                .reactor()
                .scheduler()
                .block_on(stream.read_bytes(max_bytes, eff_ctx))?;
            let text = String::from_utf8(bytes).map_err(|e| {
                MoraError::Io(format!("tcp_stream.read: invalid UTF-8 payload: {e}"))
            })?;
            Ok(CallAction::Done(Value::String(text)))
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
            let eff_ctx = explicit_ctx.as_ref().or(core.task_context.as_ref());
            let bytes = core
                .reactor()
                .scheduler()
                .block_on(stream.read_bytes(max_bytes, eff_ctx))?;
            let vals: Vec<Value> = bytes.into_iter().map(|b| Value::Int(b as i64)).collect();
            Ok(CallAction::Done(Value::List(vals.into())))
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
            let eff_ctx = explicit_ctx.as_ref().or(core.task_context.as_ref());
            let bytes = core
                .reactor()
                .scheduler()
                .block_on(stream.read_line_bytes(max_bytes, eff_ctx))?;
            let text = String::from_utf8(bytes).map_err(|e| {
                MoraError::Io(format!("tcp_stream.read_line: invalid UTF-8 line: {e}"))
            })?;
            Ok(CallAction::Done(Value::String(text)))
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
            let eff_ctx = explicit_ctx.as_ref().or(core.task_context.as_ref());
            let n = core
                .reactor()
                .scheduler()
                .block_on(stream.write_bytes(data.as_bytes(), eff_ctx))?;
            Ok(CallAction::Done(Value::Int(n as i64)))
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
            let eff_ctx = explicit_ctx.as_ref().or(core.task_context.as_ref());
            let n = core
                .reactor()
                .scheduler()
                .block_on(stream.write_bytes(&raw, eff_ctx))?;
            Ok(CallAction::Done(Value::Int(n as i64)))
        }
        "shutdown" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "tcp_stream.shutdown() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            core.reactor().scheduler().block_on(stream.shutdown())?;
            Ok(CallAction::Done(Value::Nil))
        }
        "close" => {
            if !args.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "tcp_stream.close() expects 0 arguments, got {}",
                    args.len()
                )));
            }
            core.reactor().scheduler().block_on(stream.close());
            Ok(CallAction::Done(Value::Nil))
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
            let hw = crate::runtime::gpu::hardware_device_info(&dev);
            let mut m = std::collections::HashMap::new();
            for (k, v) in hw.to_string_map() {
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
        combinator: CombinatorFrame::WatermarkCallbacks {
            final_result,
            pending: fired,
        },
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
                    combinator: CombinatorFrame::AutogradEval {
                        tape_id,
                        leaf_nodes,
                        scalar_flags,
                        return_value,
                    },
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
                    combinator: CombinatorFrame::Compose { funcs, next_idx: 1 },
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

    // Stage 3C: Associated calls `Type::member(...)` for nominal structs, enums, and inherent impls.
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
            if let Some((vname, payload_tys)) = edef.variants.iter().find(|(v, _)| v == member) {
                if !named_args.is_empty() {
                    return Err(MoraError::Runtime(format!(
                        "{type_name}::{vname} does not accept keyword arguments"
                    )));
                }
                if args.len() != payload_tys.len() {
                    return Err(MoraError::Runtime(format!(
                        "{type_name}::{vname} expects {} payload arg(s), got {}",
                        payload_tys.len(),
                        args.len()
                    )));
                }
                return Ok(CallAction::Done(Value::EnumVariant {
                    enum_name: edef.name.clone(),
                    variant: vname.clone(),
                    payload: args,
                }));
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
