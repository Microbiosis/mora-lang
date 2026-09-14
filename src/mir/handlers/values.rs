//! Pure value instructions — write to `regs[dst]`, return `Flow::Continue`.

use std::collections::HashMap;

use std::sync::Arc;

use crate::common::BinaryOp;

use crate::flow::eval_binary;

use crate::mir::host::MirHost;

use crate::mir::vm::{index_value, index_assign_value, run_mir, value_to_string};

use crate::mir::{MirFunction, Reg};

use crate::value::{Environment, Value};

// ============================================================
// Pure value instructions (write to regs[dst])
// ============================================================

pub fn h_const(regs: &mut [Value], dst: Reg, value: &Value) {
    regs[dst] = value.clone();
}

pub fn h_var(regs: &mut [Value], dst: Reg, name: &str, env: &Environment) {
    regs[dst] = env.get(name).unwrap_or(Value::Nil);
}

pub fn h_binary_op(
    regs: &mut [Value],
    dst: Reg,
    lhs: Reg,
    op: &BinaryOp,
    rhs: Reg,
) -> Result<(), String> {
    let lv = regs[lhs].clone();
    let rv = regs[rhs].clone();
    regs[dst] = eval_binary(lv, op, rv)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)] // effect-as-data 线穿：regs+5 语义参数+effects
pub fn h_call(
    regs: &mut [Value],
    dst: Reg,
    name: &str,
    args: &[Reg],
    task_registry: &HashMap<&str, (&[String], &MirFunction)>,
    interp: &mut dyn MirHost,
    env: &mut Environment,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    let arg_vals: Vec<Value> = args.iter().map(|r| regs[*r].clone()).collect();
    let result = if let Some((params, body)) = task_registry.get(name) {
        let mut child_env = env.clone();
        for (i, param) in params.iter().enumerate() {
            let val = arg_vals.get(i).cloned().unwrap_or(Value::Nil);
            child_env.define(param.clone(), val, false);
        }
        // v0.75.9: 包裹 Arc 走全局 DAG 缓存（task body 借自指令表）
        run_mir(&Arc::new((*body).clone()), interp, &mut child_env, effects)?
    } else if let Some(callable) = env.get(name) {
        // v0.75.76: 用户自定义 callable（Closure/Task/Compose/Partial）在
        // 执行 env 中直调（与 h_define 同一容器，无回落）；其余名（builtin、
        // 未定义等）统一经 mir_call_function —— 单一 env 传递，无回退分支。
        match callable {
            Value::Task { .. }
            | Value::Closure { .. }
            | Value::Compose(_)
            | Value::Partial(_, _)
            // v0.102: 关系值可调用 —— 调用构造 Goal::Invoke（目标构建期）
            | Value::Relation { .. } => interp.call_value(&callable, arg_vals, effects)?,
            _ => interp.mir_call_function(name, arg_vals, env, effects)?,
        }
    } else {
        interp.mir_call_function(name, arg_vals, env, effects)?
    };
    regs[dst] = result;
    Ok(())
}

pub fn h_list_lit(regs: &mut [Value], dst: Reg, items: &[Reg]) {
    let vals: Vec<Value> = items.iter().map(|r| regs[*r].clone()).collect();
    regs[dst] = Value::List(vals);
}

pub fn h_dict_lit(regs: &mut [Value], dst: Reg, entries: &[(String, Reg)]) {
    let mut map = HashMap::new();
    for (k, v) in entries {
        map.insert(k.clone(), regs[*v].clone());
    }
    regs[dst] = Value::Dict(map);
}

pub fn h_index(regs: &mut [Value], dst: Reg, obj: Reg, idx: Reg) -> Result<(), String> {
    let obj_val = regs[obj].clone();
    let idx_val = regs[idx].clone();
    regs[dst] = index_value(&obj_val, &idx_val)?;
    Ok(())
}

pub fn h_index_assign(regs: &mut [Value], obj: Reg, idx: Reg, val: Reg) -> Result<(), String> {
    let mut obj_val = regs[obj].clone();
    let idx_val = regs[idx].clone();
    let val_val = regs[val].clone();
    index_assign_value(&mut obj_val, &idx_val, &val_val)?;
    regs[obj] = obj_val;
    Ok(())
}

pub fn h_method_call(
    regs: &mut [Value],
    dst: Reg,
    receiver: Reg,
    method: &str,
    args: &[Reg],
    interp: &mut dyn MirHost,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    let recv_val = regs[receiver].clone();
    let arg_vals: Vec<Value> = args.iter().map(|r| regs[*r].clone()).collect();
    regs[dst] = interp.mir_call_method(recv_val, method, arg_vals, effects)?;
    Ok(())
}

pub fn h_pipe(
    regs: &mut [Value],
    dst: Reg,
    lhs: Reg,
    rhs: Reg,
    interp: &mut dyn MirHost,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    let lhs_val = regs[lhs].clone();
    let rhs_val = regs[rhs].clone();
    regs[dst] = interp.call_value(&rhs_val, vec![lhs_val], effects)?;
    Ok(())
}

pub fn h_prompt(regs: &mut [Value], dst: Reg, parts: &[Reg]) {
    let mut s = String::new();
    for r in parts {
        s.push_str(&value_to_string(&regs[*r]));
    }
    regs[dst] = Value::String(s);
}

pub fn h_closure(
    regs: &mut [Value],
    dst: Reg,
    params: &[String],
    body: &MirFunction,
    env: &Environment,
) {
    // v0.75.77: 闭包捕获执行 env（与 h_define 写入同一容器，单一来源）——
    // 不再读 interp.environment() 宿主全局槽（take_env 移空后捕获到空壳，
    // 顶层绑定 base 对闭包不可见：`let base=10; let f=fn(x) x+base end`）。
    let closure = Value::Closure {
        params: params.to_vec(),
        env: crate::value::EnvRef(Box::new(env.clone())),
        mir_body: Arc::new(body.clone()),
    };
    regs[dst] = closure;
}

pub fn h_dyn_trait(
    regs: &mut [Value],
    dst: Reg,
    src: Reg,
    trait_name: &str,
    trait_generics: &[String],
) {
    let data = regs[src].clone();
    regs[dst] = Value::TraitObject {
        for_generics: Vec::new(),
        trait_generics: trait_generics.to_vec(),
        for_type: String::new(),
        trait_name: trait_name.to_string(),
        data: Box::new(data),
    };
}
