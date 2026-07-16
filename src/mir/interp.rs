//! MIR 解释器（α.0）
//!
//! pc 循环执行 MirFunction。控制流用 Jump/Return/Break/Continue 直接改 pc，
//! 替代 AST 解释器的 FlowSignal 枚举层层传返。
//!
//! α.0 复用现有 Interpreter 的 call_function / eval_binary，不重写 builtins。
//! 这样 MIR 解释器只替代"执行引擎"，AI/transport/sandbox facade 不受影响。

use crate::flow::eval_binary;
use crate::interpreter::Interpreter;
use crate::value::{Environment, Value};

use super::{MirFunction, MirInst};

/// MIR 解释器执行一个 MirFunction，返回最后的表达式值或 Return 值
pub fn run_mir(
    func: &MirFunction,
    interp: &mut Interpreter,
    env: &mut Environment,
) -> Result<Value, String> {
    // α.2: 扫描收集 task 定义，建立注册表（存整个 TaskDef 指令引用，含 params + body）
    let task_registry: std::collections::HashMap<&str, (&[String], &MirFunction)> = func
        .body
        .iter()
        .filter_map(|inst| {
            if let MirInst::TaskDef { name, params, body } = inst {
                Some((name.as_str(), (params.as_slice(), body.as_ref())))
            } else {
                None
            }
        })
        .collect();

    let mut regs: Vec<Value> = vec![Value::Nil; func.n_regs];
    let mut pc: usize = 0;

    while pc < func.body.len() {
        match &func.body[pc] {
            MirInst::Const(dst, v) => {
                regs[*dst] = v.clone();
                pc += 1;
            }
            MirInst::Var(dst, name) => {
                regs[*dst] = env.get(name).unwrap_or(Value::Nil);
                pc += 1;
            }
            MirInst::BinaryOp(dst, l, op, r) => {
                let lv = regs[*l].clone();
                let rv = regs[*r].clone();
                regs[*dst] = eval_binary(lv, op, rv)?;
                pc += 1;
            }
            MirInst::Call(dst, callee, args) => {
                let arg_vals: Vec<Value> = args.iter().map(|r| regs[*r].clone()).collect();
                // α.2: 先查 task 注册表，找到则递归 run_mir；否则走内置函数
                let result =
                    if let Some((task_params, task_func)) = task_registry.get(callee.as_str()) {
                        let mut child_env = env.clone();
                        for (i, param) in task_params.iter().enumerate() {
                            let val = arg_vals.get(i).cloned().unwrap_or(Value::Nil);
                            child_env.define(param.clone(), val, false);
                        }
                        run_mir(task_func, interp, &mut child_env)
                    } else {
                        interp.mir_call_function(callee, arg_vals)
                    };
                let result = result?;
                regs[*dst] = result;
                pc += 1;
            }
            // α.1: List/Dict/Index/MethodCall/Pipe/Prompt
            MirInst::ListLit(dst, items) => {
                let vals: Vec<Value> = items.iter().map(|r| regs[*r].clone()).collect();
                regs[*dst] = Value::List(vals);
                pc += 1;
            }
            MirInst::DictLit(dst, pairs) => {
                let mut map = std::collections::HashMap::new();
                for (k, v) in pairs {
                    map.insert(k.clone(), regs[*v].clone());
                }
                regs[*dst] = Value::Dict(map);
                pc += 1;
            }
            MirInst::Index(dst, obj, idx) => {
                let obj_val = regs[*obj].clone();
                let idx_val = regs[*idx].clone();
                regs[*dst] = index_value(&obj_val, &idx_val)?;
                pc += 1;
            }
            MirInst::MethodCall(dst, recv, method, args) => {
                let recv_val = regs[*recv].clone();
                let arg_vals: Vec<Value> = args.iter().map(|r| regs[*r].clone()).collect();
                let result = interp.mir_call_method(recv_val, method, arg_vals)?;
                regs[*dst] = result;
                pc += 1;
            }
            MirInst::Pipe(dst, lhs, rhs) => {
                let lhs_val = regs[*lhs].clone();
                let rhs_val = regs[*rhs].clone();
                // lhs |> rhs = call_value(rhs, [lhs])
                let result = interp.call_value(&rhs_val, vec![lhs_val])?;
                regs[*dst] = result;
                pc += 1;
            }
            MirInst::Prompt(dst, parts) => {
                // p"..." 不触发 AI，只拼接字符串
                let mut s = String::new();
                for r in parts {
                    s.push_str(&value_to_string(&regs[*r]));
                }
                regs[*dst] = Value::String(s);
                pc += 1;
            }
            MirInst::Define(name, src) => {
                env.define(name.clone(), regs[*src].clone(), false);
                pc += 1;
            }
            MirInst::Assign(name, src) => {
                env.assign(name, regs[*src].clone());
                pc += 1;
            }
            MirInst::Expr(src) => {
                let _ = &regs[*src];
                pc += 1;
            }
            MirInst::TaskDef { .. } => {
                // task 定义已在 run_mir 入口扫描注册，此处跳过
                pc += 1;
            }
            MirInst::Import(path) => {
                // 委托 AST 解释器的 import 路径
                interp.mir_import(path)?;
                pc += 1;
            }
            MirInst::WithConfig { bindings, body } => {
                // 保存/恢复 AI config，执行 body MirFunction
                let binding_vals: Vec<(String, Value)> = bindings
                    .iter()
                    .map(|(k, r)| (k.clone(), regs[*r].clone()))
                    .collect();
                interp.mir_with_config(&binding_vals)?;
                let mut child_env = env.clone();
                let result = run_mir(body, interp, &mut child_env)?;
                interp.mir_restore_config();
                let _ = result; // with 块的返回值丢弃（语句语义）
                pc += 1;
            }
            MirInst::Label(_) => {
                pc += 1;
            }
            MirInst::Jump(lbl) => {
                pc = *lbl;
            }
            MirInst::JumpIf(cond, lbl) => {
                if is_truthy(&regs[*cond]) {
                    pc = *lbl;
                } else {
                    pc += 1;
                }
            }
            MirInst::JumpIfNot(cond, lbl) => {
                if !is_truthy(&regs[*cond]) {
                    pc = *lbl;
                } else {
                    pc += 1;
                }
            }
            MirInst::Return(r) => {
                return Ok(r.map_or(Value::Nil, |r| regs[r].clone()));
            }
            MirInst::Break(lbl) => {
                pc = *lbl;
            }
            MirInst::Continue(lbl) => {
                pc = *lbl;
            }
        }
    }
    Ok(Value::Nil)
}

/// α.1: 索引操作 List[i] / Dict[key] / String[i]
fn index_value(obj: &Value, idx: &Value) -> Result<Value, String> {
    match (obj, idx) {
        (Value::List(list), Value::Int(i)) => {
            let i = *i as usize;
            list.get(i)
                .cloned()
                .ok_or_else(|| format!("index {} out of bounds (len {})", i, list.len()))
        }
        (Value::List(list), Value::Number(n)) => {
            let i = *n as usize;
            list.get(i)
                .cloned()
                .ok_or_else(|| format!("index {} out of bounds (len {})", i, list.len()))
        }
        (Value::Dict(map), Value::String(key)) => Ok(map.get(key).cloned().unwrap_or(Value::Nil)),
        (Value::String(s), Value::Int(i)) => {
            let i = *i as usize;
            s.chars().nth(i).map(Value::Char).ok_or_else(|| {
                format!(
                    "string index {} out of bounds (len {})",
                    i,
                    s.chars().count()
                )
            })
        }
        _ => Err(format!("cannot index {:?} with {:?}", obj, idx)),
    }
}

/// α.1: Value 转字符串（p"..." 拼接用，与 AST 解释器 evaluate_prompt 语义一致）
fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Nil => "nil".to_string(),
        Value::Char(c) => c.to_string(),
        other => format!("{:?}", other),
    }
}

fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Nil => false,
        Value::Number(n) => *n != 0.0,
        Value::Int(i) => *i != 0,
        Value::Float(f) => *f != 0.0,
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}
