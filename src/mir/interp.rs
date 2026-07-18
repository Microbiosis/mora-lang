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
            MirInst::IndexAssign(obj, idx, val) => {
                let mut obj_val = regs[*obj].clone();
                let idx_val = regs[*idx].clone();
                let val_val = regs[*val].clone();
                index_assign_value(&mut obj_val, &idx_val, &val_val)?;
                regs[*obj] = obj_val;
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
            // α.3: 类型别名 — env 中定义 name → String(target)
            MirInst::TypeAlias { name, target } => {
                env.define(name.clone(), Value::String(target.clone()), false);
                pc += 1;
            }
            // α.3: 枚举定义 — env 中定义 name → Dict(variant → String)
            MirInst::EnumDef { name, variants } => {
                let mut enum_map = std::collections::HashMap::new();
                for v in variants {
                    enum_map.insert(v.name.clone(), Value::String(v.name.clone()));
                }
                env.define(name.clone(), Value::Dict(enum_map), false);
                pc += 1;
            }
            // α.3: 结构体定义 — env 中定义 name → Closure(构造器)
            MirInst::StructDef { name, fields } => {
                let field_names: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();
                env.define(
                    name.clone(),
                    Value::Closure {
                        params: field_names,
                        env: crate::value::EnvRef::from_arc_mutex(interp.core.environment.clone()),
                        v2_node_id: None,
                    },
                    false,
                );
                pc += 1;
            }
            MirInst::TaskDef { .. } => {
                // task 定义已在 run_mir 入口扫描注册，此处跳过
                pc += 1;
            }
            MirInst::Import(path) => {
                // α.3: 走 MIR 路径 — 解析 → lowering → run_mir
                interp.mir_import(path, env)?;
                pc += 1;
            }
            MirInst::WithConfig {
                bindings,
                body,
                jit,
            } => {
                // 保存/恢复 AI config，执行 body MirFunction
                let binding_vals: Vec<(String, Value)> = bindings
                    .iter()
                    .map(|(k, r)| (k.clone(), regs[*r].clone()))
                    .collect();
                interp.mir_with_config(&binding_vals)?;
                let mut child_env = env.clone();

                let result = if *jit {
                    // α.8: with jit → SSA → LLVM → JIT
                    // 先尝试 JIT，失败时 fallback 到 MIR 解释器
                    let mut ssa = crate::mir::ssa::construct(body);
                    crate::mir::typeinfer::infer_types(&mut ssa);

                    match crate::mir::jit::run_jit(&ssa, interp, &mut child_env) {
                        Ok(v) => v,
                        Err(e) => {
                            eprintln!(
                                "JIT compilation failed ({}), falling back to MIR interpreter",
                                e
                            );
                            run_mir(body, interp, &mut child_env)?
                        }
                    }
                } else {
                    run_mir(body, interp, &mut child_env)?
                };
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
            MirInst::MatchExpr { val, arms } => {
                let val_val = regs[*val].clone();
                // 依次尝试每个 arm，找到匹配的第一个
                let mut matched = false;
                for (pat_str, cond_reg, arm_func, output_reg) in arms {
                    if self_match_pattern(
                        &val_val,
                        pat_str,
                        cond_reg.as_ref().map(|r| &regs[*r]),
                        env,
                    ) {
                        let result = run_mir(arm_func, interp, env)?;
                        regs[*output_reg] = result;
                        matched = true;
                        break;
                    }
                }
                if !matched {
                    // 没有匹配任何 arm，返回 Nil
                    if let Some((_pat, _cond, _func, output_reg)) = arms.first() {
                        regs[*output_reg] = Value::Nil;
                    }
                }
                pc += 1;
            }
            MirInst::ToolDef { .. } => {
                // tool 定义由 AST 解释器 execute_tool_def 处理，
                // MIR 解释器不独立处理 tool 注册（委托给 AST 路径）
                pc += 1;
            }
            MirInst::StreamFor {
                prompt_reg,
                var,
                body,
            } => {
                // stream_for: 委托 AST 解释器的 stream_for 语义
                // 简化实现：顺序执行 body（与 parallel 相同）
                let _prompt = regs[*prompt_reg].clone();
                let _ = var;
                // 占位：实际 stream_for 需要 AI 集成
                let mut child_env = env.clone();
                let result = run_mir(body, interp, &mut child_env)?;
                let _ = result;
                pc += 1;
            }
            MirInst::MatchArm { .. } => {
                // MatchArm 是 MatchExpr 的内部 arm，不直接出现在 body 顶层
                // 若出现则跳过（应已由 MatchExpr lowering 纳入嵌套 MirFunction）
                pc += 1;
            }
            // α.4: 事务 — body 执行，失败则执行 compensation 后返回错误
            MirInst::Transaction { body, compensation } => {
                let mut child_env = env.clone();
                let result = run_mir(body, interp, &mut child_env);
                match result {
                    Ok(_) => {
                        // 执行成功，child_env 合并回父 env
                        for (name, val) in child_env.iter() {
                            env.define(name, val, false);
                        }
                        pc += 1;
                    }
                    Err(_) => {
                        // body 执行失败 → 执行 compensation
                        let mut comp_env = env.clone();
                        let _ = run_mir(compensation, interp, &mut comp_env);
                        // compensation 执行完毕后返回事务回滚错误
                        return Err("Transaction rolled back".to_string());
                    }
                }
            }
            // α.4: send — 发送值到 worker channel
            MirInst::Send { value, target } => {
                let val = regs[*value].clone();
                if let Some(tx) = interp.core.worker_channels.get(target.as_str()) {
                    tx.send(val).map_err(|e| format!("Send error: {}", e))?;
                }
                pc += 1;
            }
            // α.4: receive — 从 worker channel 接收值
            MirInst::Receive { var, source } => {
                if let Some(rx) = interp.core.worker_receivers.get(source.as_str()) {
                    let val = rx.recv().map_err(|e| format!("Receive error: {}", e))?;
                    env.define(var.clone(), val, false);
                }
                pc += 1;
            }
            // α.4: rollback — 触发事务回滚
            MirInst::Rollback => {
                return Err("Transaction rolled back".to_string());
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
        (Value::List(list), Value::Float(n)) => {
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
        Value::Float(n) => *n != 0.0,
        Value::Int(i) => *i != 0,
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// α.2: 索引赋值 obj[idx] = val（就地修改）
fn index_assign_value(obj: &mut Value, idx: &Value, val: &Value) -> Result<(), String> {
    match (obj, idx) {
        (Value::List(list), Value::Int(i)) => {
            let i = *i as usize;
            if i >= list.len() {
                Err(format!("index {} out of bounds (len {})", i, list.len()))
            } else {
                list[i] = val.clone();
                Ok(())
            }
        }
        (Value::List(list), Value::Float(n)) => {
            let i = *n as usize;
            if i >= list.len() {
                Err(format!("index {} out of bounds (len {})", i, list.len()))
            } else {
                list[i] = val.clone();
                Ok(())
            }
        }
        (Value::Dict(map), Value::String(key)) => {
            map.insert(key.clone(), val.clone());
            Ok(())
        }
        _ => Err("cannot index assign with given object and index".to_string()),
    }
}

/// α.2: MIR 模式匹配（简化版）
/// pat_str 来自 pattern_to_string 的序列化结果
fn self_match_pattern(
    val: &Value,
    pat_str: &str,
    _cond_reg: Option<&Value>,
    env: &mut Environment,
) -> bool {
    // 通配符匹配任意值
    if pat_str == "_" {
        return true;
    }
    // 变量模式：纯标识符（无 ":" 前缀）匹配任意值，绑定到 env
    if !pat_str.contains(':') {
        env.define(pat_str.to_string(), val.clone(), false);
        return true;
    }
    // nil 模式
    if pat_str == "nil" {
        return matches!(val, Value::Nil);
    }
    // bool 模式
    if pat_str == "bool:true" {
        return matches!(val, Value::Bool(true));
    }
    if pat_str == "bool:false" {
        return matches!(val, Value::Bool(false));
    }
    // int 模式: int:42
    if let Some(suffix) = pat_str.strip_prefix("int:")
        && let Value::Int(i) = val
        && let Ok(n) = suffix.parse::<i64>()
    {
        return i == &n;
    }
    // float 模式: float:3.14
    if let Some(suffix) = pat_str.strip_prefix("float:")
        && let Value::Float(f) = val
        && let Ok(n) = suffix.parse::<f64>()
    {
        return (f - n).abs() < 1e-9;
    }
    // str 模式: str:hello
    if let Some(suffix) = pat_str.strip_prefix("str:")
        && let Value::String(s) = val
    {
        return s == suffix;
    }
    // 列表模式: list:[p1,p2,...]
    if pat_str == "list:[]" {
        return matches!(val, Value::List(items) if items.is_empty());
    }
    if let Some(inner) = pat_str
        .strip_prefix("list:[")
        .and_then(|s| s.strip_suffix(']'))
    {
        if let Value::List(items) = val {
            let pats: Vec<&str> = if inner.is_empty() {
                Vec::new()
            } else {
                inner.split(',').collect()
            };
            if items.len() != pats.len() {
                return false;
            }
            return items
                .iter()
                .zip(pats.iter())
                .all(|(item, pat)| self_match_pattern(item, pat, None, env));
        }
        return false;
    }
    // 字典模式: dict:{k1:v1,k2:v2,...}
    if pat_str == "dict:{}" {
        return matches!(val, Value::Dict(map) if map.is_empty());
    }
    if let Some(inner) = pat_str
        .strip_prefix("dict:{")
        .and_then(|s| s.strip_suffix('}'))
    {
        if let Value::Dict(map) = val {
            let pairs: Vec<&str> = if inner.is_empty() {
                Vec::new()
            } else {
                inner.split(',').collect()
            };
            return pairs.iter().all(|pair| {
                if let Some((k, v)) = pair.split_once(':') {
                    map.get(k)
                        .map(|item| self_match_pattern(item, v, None, env))
                        .unwrap_or(false)
                } else {
                    false
                }
            });
        }
        return false;
    }
    // 守卫模式: guard:inner — 只检查内部模式匹配
    if let Some(inner) = pat_str.strip_prefix("guard:") {
        return self_match_pattern(val, inner, _cond_reg, env);
    }
    // 字符模式: char:c
    if let Some(suffix) = pat_str.strip_prefix("char:")
        && let Value::Char(c) = val
    {
        return *c == suffix.chars().next().unwrap_or('\0');
    }
    // 未匹配的模式
    false
}

/// α.3: 查找并执行 main task（与 AST 路径的 interpret() 末尾逻辑一致）。
/// 扫描 func.body 中的 TaskDef，找到 name="main" 且无参的，执行其 body。
/// 若不存在或非无参 main 则静默返回 Ok。
pub fn run_main_task(
    func: &MirFunction,
    interp: &mut Interpreter,
    env: &mut Environment,
) -> Result<(), String> {
    let mut main_body: Option<&MirFunction> = None;
    for inst in &func.body {
        if let MirInst::TaskDef { name, params, body } = inst
            && name == "main"
            && params.is_empty()
        {
            main_body = Some(body);
            break;
        }
    }
    if let Some(main_func) = main_body {
        let mut main_env = env.clone();
        let _ = run_mir(main_func, interp, &mut main_env)?;
    }
    Ok(())
}
