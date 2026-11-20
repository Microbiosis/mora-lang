//! v0.92 P1.2: `Interpreter::call_builtin_*` — builtin 函数实现（自 dispatch.rs 迁出）。
//!
//! 按 concern 拆分：dispatch.rs 只保留路由（`call_function` 的 name→handler
//! 分派 + `call_value` 的 Value→调用），本模块承载 30 个 `call_builtin_*`
//! 具体实现 + 兜底查找。`testcase!` 宏定义在 `interpreter` 模块根。

use std::sync::Arc;

use parking_lot::Mutex;

use super::*;
use crate::error::{MoraError, MoraResult};
use crate::value::Value;

/// D327：取一元内建的**唯一**实参入口。
///
/// 修前 `str` / `int` / `float` / `bool` / `atom` 五处都是
/// `args.first().cloned().unwrap_or(Value::Nil)` —— 少传参数时**静默**
/// 当成 `nil` 继续算：`str()` → `"nil"`、`bool()` → `false`、
/// `atom()` → `Atom(Nil)`，而 `int()` / `float()` 因为随后撞上
/// 「nil 不可转换」才**碰巧**报错（且文案误导，说「does not accept nil」，
/// 真实原因是「没给参数」）。
///
/// 方向由既有证据钉死，不需要另作选择：`typeck/hm/builtin.rs` 把这五个
/// 全部登记成**一元**箭头（`str: α → String` 等），同族的
/// `type_of()` / `deref()` / `methods_of()` / `len()` / `compose()`
/// 也全都对缺参报错。所以「缺参报错」是既有契约，本条只是让实现回到契约上。
fn require_1(name: &str, args: &[Value]) -> MoraResult<Value> {
    match args.first() {
        Some(v) => Ok(v.clone()),
        None => Err(MoraError::Runtime(format!("{name}() requires 1 argument"))),
    }
}

use crate::mir::control::BuiltinFnId;
use crate::runtime::CoreRuntime;

/// Dispatch stateless/core-only built-in functions; returns `None` for VM callback/eval builtins.
pub(crate) fn eval_pure_builtin(
    id: BuiltinFnId,
    core: &mut CoreRuntime,
    args: &[Value],
) -> Option<MoraResult<Value>> {
    match id {
        BuiltinFnId::Print => Some(call_builtin_print(args)),
        BuiltinFnId::Range => Some(call_builtin_range(args)),
        BuiltinFnId::Len => Some(call_builtin_len(args)),
        BuiltinFnId::Str => Some(call_builtin_str(args)),
        BuiltinFnId::Int => Some(call_builtin_int(args)),
        BuiltinFnId::Float => Some(call_builtin_float(args)),
        BuiltinFnId::Bool => Some(call_builtin_bool(args)),
        BuiltinFnId::Compose => Some(call_builtin_compose(args)),
        BuiltinFnId::Partial => Some(call_builtin_partial(args)),
        BuiltinFnId::Atom => Some(call_builtin_atom(args)),
        BuiltinFnId::Deref => Some(call_builtin_deref(args)),
        BuiltinFnId::TypeOf => Some(call_builtin_type_of(args)),
        BuiltinFnId::IsInstance => Some(call_builtin_is_instance(args)),
        BuiltinFnId::MethodsOf => Some(call_builtin_methods_of(args)),
        BuiltinFnId::Tail => Some(call_builtin_tail(args)),
        BuiltinFnId::Curry => Some(call_builtin_curry(args)),
        BuiltinFnId::Uncurry => Some(call_builtin_uncurry(args)),
        BuiltinFnId::Cons => Some(call_builtin_cons(args)),
        BuiltinFnId::Car => Some(call_builtin_car(args)),
        BuiltinFnId::Cdr => Some(call_builtin_cdr(args)),
        BuiltinFnId::Quote => Some(call_builtin_quote(args)),
        BuiltinFnId::Read => Some(call_builtin_read(args)),
        BuiltinFnId::Channel => Some(call_builtin_channel(args)),
        BuiltinFnId::Context => Some(call_builtin_context(args)),
        BuiltinFnId::Sleep => Some(call_builtin_sleep_core(core, args)),
        BuiltinFnId::MergeWith => Some(call_builtin_merge_with_core(core, args)),
        BuiltinFnId::Gensym => Some(call_builtin_gensym_core(core, args)),
        BuiltinFnId::Tensor => Some(crate::value::tensor::call_builtin_tensor(args)),
        BuiltinFnId::Grad => Some(call_builtin_grad(args, false)),
        BuiltinFnId::ValueAndGrad => Some(call_builtin_grad(args, true)),
        BuiltinFnId::Swap
        | BuiltinFnId::Into
        | BuiltinFnId::Eval
        | BuiltinFnId::Apply
        | BuiltinFnId::Macroexpand
        | BuiltinFnId::TcpConnect
        | BuiltinFnId::TcpListen
        | BuiltinFnId::Jvp
        | BuiltinFnId::TraceDag
        | BuiltinFnId::KvAllocator
        | BuiltinFnId::MemoryPool => None,
    }
}

pub(crate) fn call_builtin_merge_with_core(
    core: &mut CoreRuntime,
    args: &[Value],
) -> MoraResult<Value> {
    let key = match args.first() {
        Some(Value::String(s)) => {
            testcase!(true, "merge_with: string key");
            s.clone()
        }
        _ => {
            return Err(MoraError::Runtime(
                "merge_with(key, strategy) expects string key".to_string(),
            ));
        }
    };
    let strat = match args.get(1) {
        Some(Value::String(s)) => {
            testcase!(true, "merge_with: string strategy");
            s.as_str()
        }
        _ => {
            return Err(MoraError::Runtime(
                "merge_with(key, strategy) expects string strategy".to_string(),
            ));
        }
    };
    let ms = match crate::value::MergeStrategy::from_name(strat) {
        Some(s) => s,
        None => {
            return Err(MoraError::Runtime(format!(
                "merge_with: unknown strategy '{}' (append/add/dict_union/grow_only_set/lww)",
                strat
            )));
        }
    };
    let mut strategies = core.current_merge_strategies.clone().unwrap_or_default();
    strategies.insert(key, ms);
    core.current_merge_strategies = Some(strategies);
    Ok(Value::Nil)
}

pub(crate) fn call_builtin_print(args: &[Value]) -> MoraResult<Value> {
    let msg = args
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join("\t");
    println!("{}", msg);
    Ok(Value::Nil)
}

pub(crate) fn call_builtin_range(args: &[Value]) -> MoraResult<Value> {
    fn as_i64(v: &Value, which: &str) -> MoraResult<Option<i64>> {
        match v {
            Value::Float(n) => {
                if !n.is_finite() {
                    return Err(MoraError::Runtime(format!(
                        "range() {which} must be finite, got {n}"
                    )));
                }
                Ok(Some(*n as i64))
            }
            Value::Int(n) => Ok(Some(*n)),
            other => Err(MoraError::Runtime(format!(
                "range() {which} must be a number, got {}",
                crate::flow::type_name(other)
            ))),
        }
    }

    let (start, end) = match args.len() {
        0 => (0, 0),
        1 => (0, as_i64(&args[0], "end")?.unwrap_or(0)),
        _ => (
            as_i64(&args[0], "start")?.unwrap_or(0),
            as_i64(&args[1], "end")?.unwrap_or(0),
        ),
    };
    let step = match args.get(2) {
        Some(v) => as_i64(v, "step")?.unwrap_or(1),
        None => 1,
    };
    if step == 0 {
        return Err(MoraError::Runtime(
            "range() step must not be 0 (would never terminate)".to_string(),
        ));
    }
    let mut items = Vec::new();
    if step > 0 {
        let mut i = start;
        while i < end {
            items.push(Value::Int(i));
            i += step;
        }
    } else {
        let mut i = start;
        while i > end {
            items.push(Value::Int(i));
            i += step;
        }
    }
    Ok(Value::List(items.into()))
}

pub(crate) fn call_builtin_len(args: &[Value]) -> MoraResult<Value> {
    let len = match args.first() {
        Some(Value::List(list)) => {
            testcase!(true, "len: list");
            list.len()
        }
        Some(Value::String(s)) => {
            testcase!(true, "len: string");
            s.chars().count()
        }
        Some(Value::Dict(map)) => {
            testcase!(true, "len: dict");
            map.len()
        }
        Some(Value::Channel(ch)) => {
            testcase!(true, "len: channel");
            ch.len()
        }
        Some(Value::Tensor(t)) => {
            testcase!(true, "len: tensor");
            t.shape.first().copied().unwrap_or(0)
        }
        _ => {
            return Err(MoraError::Runtime(
                "len() expects a list, string, dict, channel, or tensor".to_string(),
            ));
        }
    };
    Ok(Value::Int(len as i64))
}

pub(crate) fn call_builtin_str(args: &[Value]) -> MoraResult<Value> {
    let v = require_1("str", args)?;
    testcase!(true, "str: any");
    Ok(Value::String(v.to_string()))
}

pub(crate) fn call_builtin_int(args: &[Value]) -> MoraResult<Value> {
    let v = require_1("int", args)?;
    let out = match v {
        Value::String(s) => {
            let t = s.trim();
            if let Ok(n) = t.parse::<i64>() {
                n
            } else {
                t.parse::<f64>()
                    .map(|f| f as i64)
                    .map_err(|_| MoraError::Runtime(format!("int() cannot parse string: {s:?}")))?
            }
        }
        Value::Float(f) => {
            if !f.is_finite() {
                return Err(MoraError::Runtime(format!(
                    "int() cannot convert non-finite float: {f}"
                )));
            }
            f as i64
        }
        Value::Int(n) => n,
        Value::Scalar(s) => {
            if let Some(n) = s.as_i64() {
                n
            } else {
                let f = s.as_f64();
                if !f.is_finite() {
                    return Err(MoraError::Runtime(format!(
                        "int() cannot convert non-finite scalar: {f}"
                    )));
                }
                f as i64
            }
        }
        Value::Bool(b) => {
            if b {
                1
            } else {
                0
            }
        }
        other => {
            return Err(MoraError::Runtime(format!(
                "int() does not accept {}",
                crate::flow::type_name(&other)
            )));
        }
    };
    testcase!(true, "int: coercion");
    Ok(Value::Int(out))
}

pub(crate) fn call_builtin_float(args: &[Value]) -> MoraResult<Value> {
    let v = require_1("float", args)?;
    let out = match v {
        Value::String(s) => s
            .trim()
            .parse::<f64>()
            .map_err(|_| MoraError::Runtime(format!("float() cannot parse string: {s:?}")))?,
        Value::Float(f) => f,
        Value::Int(n) => n as f64,
        Value::Scalar(s) => s.as_f64(),
        Value::Bool(b) => {
            if b {
                1.0
            } else {
                0.0
            }
        }
        other => {
            return Err(MoraError::Runtime(format!(
                "float() does not accept {}",
                crate::flow::type_name(&other)
            )));
        }
    };
    testcase!(true, "float: coercion");
    Ok(Value::Float(out))
}

pub(crate) fn call_builtin_bool(args: &[Value]) -> MoraResult<Value> {
    let v = require_1("bool", args)?;
    testcase!(true, "bool: coercion");
    Ok(Value::Bool(crate::flow::is_truthy(&v)))
}

pub(crate) fn call_builtin_compose(args: &[Value]) -> MoraResult<Value> {
    if args.is_empty() {
        return Err(MoraError::Runtime(
            "compose() requires at least 1 argument".to_string(),
        ));
    }
    Ok(Value::Compose(args.to_vec()))
}

pub(crate) fn call_builtin_partial(args: &[Value]) -> MoraResult<Value> {
    if args.is_empty() {
        return Err(MoraError::Runtime(
            "partial() requires at least 1 argument (the function)".to_string(),
        ));
    }
    let func = args[0].clone();
    let partial_args: Vec<Value> = args[1..].to_vec();
    Ok(Value::Partial(Box::new(func), partial_args))
}

pub(crate) fn call_builtin_atom(args: &[Value]) -> MoraResult<Value> {
    let value = require_1("atom", args)?;
    Ok(Value::Atom(Arc::new(Mutex::new(value))))
}

pub(crate) fn call_builtin_deref(args: &[Value]) -> MoraResult<Value> {
    let value = args
        .first()
        .ok_or_else(|| MoraError::Runtime("deref() requires 1 argument".to_string()))?;
    match value {
        Value::Atom(arc) => Ok(arc.lock().clone()),
        _ => Err(MoraError::Runtime(
            "deref() argument must be an atom".to_string(),
        )),
    }
}

pub(crate) fn call_builtin_grad(args: &[Value], return_value: bool) -> MoraResult<Value> {
    let fname = if return_value {
        "value_and_grad"
    } else {
        "grad"
    };
    let func = args.first().cloned().ok_or_else(|| {
        MoraError::Runtime(format!(
            "{fname}(fn, argnums?) requires a callable function as first argument"
        ))
    })?;
    let is_callable = matches!(
        func,
        Value::Task { .. }
            | Value::Closure { .. }
            | Value::Compose(_)
            | Value::Partial(_, _)
            | Value::Curry { .. }
            | Value::Builtin(_)
            | Value::GradFn { .. }
    );
    if !is_callable {
        return Err(MoraError::Runtime(format!(
            "{fname}() expects a callable function, got {}",
            crate::flow::dynamic_type_name(&func)
        )));
    }
    let argnums = match args.get(1) {
        None | Some(Value::Nil) => vec![0usize],
        Some(Value::Int(i)) if *i >= 0 => vec![*i as usize],
        Some(Value::Float(f)) if f.is_finite() && *f >= 0.0 && f.fract() == 0.0 => {
            vec![*f as usize]
        }
        Some(Value::List(items)) => {
            if items.is_empty() {
                return Err(MoraError::Runtime(format!(
                    "{fname}() argnums list cannot be empty"
                )));
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items.iter() {
                let idx = match item {
                    Value::Int(i) if *i >= 0 => *i as usize,
                    Value::Float(f) if f.is_finite() && *f >= 0.0 && f.fract() == 0.0 => {
                        *f as usize
                    }
                    other => {
                        return Err(MoraError::Runtime(format!(
                            "{fname}() argnums entries must be non-negative integers, got {}",
                            crate::flow::dynamic_type_name(other)
                        )));
                    }
                };
                if out.contains(&idx) {
                    return Err(MoraError::Runtime(format!(
                        "{fname}() duplicate argnum index {idx}"
                    )));
                }
                out.push(idx);
            }
            out
        }
        Some(other) => {
            return Err(MoraError::Runtime(format!(
                "{fname}() second argument `argnums` must be a non-negative integer or list of integers, got {}",
                crate::flow::dynamic_type_name(other)
            )));
        }
    };
    Ok(Value::GradFn {
        func: Box::new(func),
        argnums,
        return_value,
    })
}

pub(crate) fn call_builtin_type_of(args: &[Value]) -> MoraResult<Value> {
    let value = args
        .first()
        .ok_or_else(|| MoraError::Runtime("type_of() requires 1 argument".to_string()))?;
    Ok(Value::String(crate::flow::dynamic_type_name(value)))
}

pub(crate) fn call_builtin_is_instance(args: &[Value]) -> MoraResult<Value> {
    if args.len() < 2 {
        return Err(MoraError::Runtime(
            "is_instance() requires 2 arguments".to_string(),
        ));
    }
    let value = &args[0];
    let target = match &args[1] {
        Value::String(s) => s.as_str(),
        _ => {
            return Err(MoraError::Runtime(
                "is_instance() second argument must be a string".to_string(),
            ));
        }
    };
    Ok(Value::Bool(
        crate::flow::dynamic_type_name(value) == target || value_type_name(value) == target,
    ))
}

pub(crate) fn call_builtin_methods_of(args: &[Value]) -> MoraResult<Value> {
    let value = args
        .first()
        .ok_or_else(|| MoraError::Runtime("methods_of() requires 1 argument".to_string()))?;
    let methods = value.methods();
    Ok(Value::List(
        methods.into_iter().map(Value::String).collect(),
    ))
}

pub(crate) fn call_builtin_tail(args: &[Value]) -> MoraResult<Value> {
    if args.len() < 2 {
        return Err(MoraError::Runtime(
            "tail() requires 2 arguments: path and max".to_string(),
        ));
    }
    let path = match &args[0] {
        Value::String(s) => s.clone(),
        other => {
            return Err(MoraError::Runtime(format!(
                "tail() first argument must be a string path, got {:?}",
                other
            )));
        }
    };
    let max =
        crate::runtime::builtins::required_num_arg(args, 1, "tail()", "second argument 'max'")?;
    if max < 0.0 {
        return Err(MoraError::Runtime(
            "tail() max must be non-negative".to_string(),
        ));
    }
    let max = max as usize;
    let content = std::fs::read_to_string(&path)
        .map_err(|e| MoraError::Io(format!("tail() cannot read '{}': {}", path, e)))?;
    let lines: Vec<&str> = content.lines().collect();
    let start = if lines.len() > max {
        lines.len() - max
    } else {
        0
    };
    Ok(Value::String(lines[start..].join("\n")))
}

pub(crate) fn call_builtin_curry(args: &[Value]) -> MoraResult<Value> {
    if args.len() < 2 {
        return Err(MoraError::Runtime(
            "curry(fn, arity) expects fn and arity".to_string(),
        ));
    }
    let fn_val = args[0].clone();
    let arity = match &args[1] {
        Value::Int(n) if *n < 0 => {
            return Err(MoraError::Runtime(format!(
                "curry: arity 不能为负数（得到 {n}）"
            )));
        }
        Value::Float(n) if *n < 0.0 => {
            return Err(MoraError::Runtime(format!(
                "curry: arity 不能为负数（得到 {n}）"
            )));
        }
        Value::Int(n) => *n as usize,
        Value::Float(n) => *n as usize,
        _ => {
            return Err(MoraError::Runtime(
                "curry: arity must be an integer".to_string(),
            ));
        }
    };
    if arity == 0 {
        return Err(MoraError::Runtime("curry: arity must be > 0".to_string()));
    }
    Ok(Value::Curry {
        func: Box::new(fn_val),
        arity,
        bound_args: Vec::new(),
    })
}

pub(crate) fn call_builtin_uncurry(args: &[Value]) -> MoraResult<Value> {
    if args.is_empty() {
        return Err(MoraError::Runtime(
            "uncurry(fn) expects a function argument".to_string(),
        ));
    }
    match &args[0] {
        Value::Curry { func, .. } => Ok((**func).clone()),
        _ => Ok(args[0].clone()),
    }
}

pub(crate) fn call_builtin_cons(args: &[Value]) -> MoraResult<Value> {
    if args.len() < 2 {
        return Err(MoraError::Runtime(
            "cons(car, cdr) expects 2 arguments".to_string(),
        ));
    }
    Ok(Value::Cons {
        car: Box::new(args[0].clone()),
        cdr: Box::new(args[1].clone()),
    })
}

pub(crate) fn call_builtin_car(args: &[Value]) -> MoraResult<Value> {
    if args.is_empty() {
        return Err(MoraError::Runtime(
            "car(cell) expects a cons cell or list".to_string(),
        ));
    }
    match &args[0] {
        Value::Cons { car, .. } => Ok((**car).clone()),
        Value::List(items) => items
            .first()
            .cloned()
            .ok_or_else(|| MoraError::Runtime("car: empty list has no first element".to_string())),
        Value::Nil => Err(MoraError::Runtime(
            "car: cannot take car of Nil".to_string(),
        )),
        _ => Err(MoraError::Runtime(format!(
            "car: expected cons cell or list, got {}",
            args[0]
        ))),
    }
}

pub(crate) fn call_builtin_cdr(args: &[Value]) -> MoraResult<Value> {
    if args.is_empty() {
        return Err(MoraError::Runtime(
            "cdr(cell) expects a cons cell or list".to_string(),
        ));
    }
    match &args[0] {
        Value::Cons { cdr, .. } => Ok((**cdr).clone()),
        Value::List(items) => Ok(Value::List(items.slice(1, items.len()))),
        Value::Nil => Err(MoraError::Runtime(
            "cdr: cannot take cdr of Nil".to_string(),
        )),
        _ => Err(MoraError::Runtime(format!(
            "cdr: expected cons cell or list, got {}",
            args[0]
        ))),
    }
}

pub(crate) fn call_builtin_quote(args: &[Value]) -> MoraResult<Value> {
    let source = match args.first() {
        Some(Value::String(s)) => s.clone(),
        _ => {
            return Err(MoraError::Runtime(
                "quote(source: string) expects a string argument".to_string(),
            ));
        }
    };
    Ok(Value::Code(source))
}

pub(crate) fn call_builtin_gensym_core(
    core: &mut CoreRuntime,
    args: &[Value],
) -> MoraResult<Value> {
    if !args.is_empty() {
        return Err(MoraError::Runtime(
            "gensym() expects no arguments".to_string(),
        ));
    }
    let n = core.gensym_counter;
    core.gensym_counter += 1;
    Ok(Value::String(format!("g{n}")))
}

pub(crate) fn call_builtin_read(args: &[Value]) -> MoraResult<Value> {
    let source = match args.first() {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Code(s)) => s.clone(),
        _ => {
            return Err(MoraError::Runtime(
                "read(source: string|code) expects a string or code argument".to_string(),
            ));
        }
    };
    Ok(Value::Code(source))
}

pub(crate) fn call_builtin_channel(args: &[Value]) -> MoraResult<Value> {
    let cap = match args.first() {
        None | Some(Value::Nil) => 0usize,
        Some(v) => crate::flow::value_as_usize(v).ok_or_else(|| {
            MoraError::Runtime(format!(
                "channel(): capacity must be a non-negative integer, got {}",
                v
            ))
        })?,
    };
    Ok(Value::Channel(Arc::new(
        crate::runtime::scheduler::MoraChannel::new(cap),
    )))
}

pub(crate) fn call_builtin_context(args: &[Value]) -> MoraResult<Value> {
    if !args.is_empty() {
        return Err(MoraError::Runtime(
            "context() expects no arguments".to_string(),
        ));
    }
    Ok(Value::Context(
        crate::runtime::scheduler::TaskContext::root(),
    ))
}

pub(crate) fn call_builtin_sleep_core(core: &mut CoreRuntime, args: &[Value]) -> MoraResult<Value> {
    let Some(ms_val) = args.first() else {
        return Err(MoraError::Runtime(
            "sleep(ms) requires 1 numeric argument".to_string(),
        ));
    };
    let duration = crate::runtime::builtins::duration_from_ms(ms_val, "sleep")?;
    core.scheduler()
        .block_on(core.reactor().sleep(duration, core.task_context.as_ref()))?;
    Ok(Value::Nil)
}
