//! Pure value instructions — write to `regs[dst]`, return `Flow::Continue`.

use std::collections::HashMap;

use std::sync::Arc;

use crate::common::BinaryOp;

use crate::flow::eval_binary;

use crate::mir::host::MirHost;

use crate::mir::vm::{index_assign_value, index_value, run_mir, value_to_string};

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
        // v0.104.6 D47：arity 校验。此前本分支**完全没有**校验，缺参静默
        // 填 `Nil`（`arg_vals.get(i) … unw_or(Value::Nil)`）。
        //
        // 判决实验 —— 同一段源码，只改「调用点与定义点是否同一个函数体」：
        //
        // ```mora
        // task f(a, b)
        //   99
        // end
        // print(f(1))            ← 同体：走 registry，**99.0 静默通过**
        //
        // task f(a, b)
        //   99
        // end
        // task g()
        //   f(1)                 ← 跨体：走 env 里的 Value::Task
        // end
        // g()                    ← 正确报 `task expects 2 args, got 1`
        // ```
        //
        // 即「同体定义并调用」——**最常见的写法**——恰好绕过了
        // `interpreter/dispatch.rs` 里已有的那道检查。
        if arg_vals.len() < params.len() {
            return Err(format!(
                "task expects {} args, got {}",
                params.len(),
                arg_vals.len()
            ));
        }
        if arg_vals.len() > params.len() {
            return Err(format!(
                "task expects {} args, got {}",
                params.len(),
                arg_vals.len()
            ));
        }
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
    regs[dst] = Value::List(vals.into());
}

pub fn h_dict_lit(regs: &mut [Value], dst: Reg, entries: &[(String, Reg)]) {
    let mut map = HashMap::new();
    for (k, v) in entries {
        map.insert(k.clone(), regs[*v].clone());
    }
    regs[dst] = Value::Dict(map);
}

pub fn h_index(regs: &mut [Value], dst: Reg, obj: Reg, idx: Reg) -> Result<(), String> {
    // v0.104.6 性能修复：**不克隆被索引对象**。
    //
    // `Value::List(Vec<Value>)` 是**深值语义**，`regs[obj].clone()` 会把整个
    // n 元素列表逐个克隆一遍。而 `index_value(&Value, &Value)` 只**借用**
    // 它、内部 O(1) 取一个元素（`list.get(i).cloned()`）—— 这次克隆纯属浪费。
    //
    // 实测（`for i in range(0, n, 1) … end`，每轮一次 `Index(list, i)`）：
    // 这一行让每迭代多付一次 O(n) 拷贝，n 轮即 **O(n²)**：
    //     n= 5,000 → 74.6 µs/iter      n=20,000 → 722.5 µs/iter
    // 去掉它后应回到「无列表 while 循环」的水平（~4.3 µs/iter）。
    //
    // 借用的作用域要先于赋值结束，故先算进局部变量再写回 `regs[dst]`。
    let idx_val = regs[idx].clone();
    let out = index_value(&regs[obj], &idx_val)?;
    regs[dst] = out;
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
    // v0.104.6 D83：`for_type` 此前**硬编码为空串**。
    //
    // 后果有两层：
    //  1. `Value` 的 Display 输出 `<trait_object for= as Foo data=…>` —— 类型名
    //     丢失（`for_type` 位是空的）。
    //  2. **更严重**：`dispatch_trait_method` 用 `for_type` 拼 impl 查找键
    //     （`impl_method_key` → `__impl_<Trait>_<TGen>_<for_type>_<FGen>_<m>`），
    //     空类型名意味着查找键里**永远带不上被包值的具体类型** —— 任何按具体
    //     类型注册的 impl 都匹配不上。错误信息 `no impl for type ''` 就是它
    //     的直接暴露。
    //
    // 改用 `flow::type_name` 从**被包的值**算出类型名，与 `Value::TraitObject`
    // 其余构造点（`construct_trait_instance`）的命名约定一致。
    //
    // ⚠ 影响范围如实说明：当前 `impl` 定义前端**不存在**（lexer 里没有
    // `Impl` token），用户无法注册任何 impl，故这条缺陷今天**只影响错误信息
    // 的可读性**；功能层面的阻塞待前端落地时才会显现。修它是因为方向明确
    // 且代价极低，不是为了「证明它已修好一个能跑的功能」。
    let for_type = crate::flow::type_name(&data).to_string();
    regs[dst] = Value::TraitObject {
        for_generics: Vec::new(),
        trait_generics: trait_generics.to_vec(),
        for_type,
        trait_name: trait_name.to_string(),
        data: Box::new(data),
    };
}
