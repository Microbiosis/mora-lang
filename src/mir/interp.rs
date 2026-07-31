//! MIR 解释器（α.0）
//!
//! pc 循环执行 MirFunction。控制流用 Jump/Return/Break/Continue 直接改 pc，
//! 替代 AST 解释器的 FlowSignal 枚举层层传返。
//!
//! α.0 复用现有 Interpreter 的 call_function / eval_binary，不重写 builtins。
//! 这样 MIR 解释器只替代"执行引擎"，AI/transport/sandbox facade 不受影响。

use crate::flow::eval_binary;
use crate::interpreter::Interpreter;
use crate::interpreter::mir_pregel_engine::MirPregelEngine;
use crate::mir::expr::{MirOrchestrateKind, MirPregelConfig};
use crate::mir::handlers::Flow;
use crate::value::{Environment, Value};

use super::{MirFunction, MirInst};

use std::collections::HashMap;
use std::sync::Arc;

/// Build a task registry from a MirFunction body.
/// Maps task name → (parameter names, body function).
pub fn build_task_registry<'a>(
    body: &'a [MirInst],
) -> HashMap<&'a str, (&'a [String], &'a MirFunction)> {
    body.iter()
        .filter_map(|inst| {
            if let MirInst::TaskDef { name, params, body } = inst {
                Some((name.as_str(), (params.as_slice(), body.as_ref())))
            } else {
                None
            }
        })
        .collect()
}

/// MIR 解释器执行一个 MirFunction，返回最后的表达式值或 Return 值。
///
/// v0.59: 现在通过 DAG 分析 + 强制 Sequence 边退化为线性执行，
/// 与 `run_dag` 共享同一套 handler 函数。等价于:
///   dag_analyze(func) → add_sequential_edges() → run_dag()
pub fn run_mir(
    func: &MirFunction,
    interp: &mut Interpreter,
    env: &mut Environment,
) -> Result<Value, String> {
    Ok(run_mir_with_signal(func, interp, env)?.1)
}

/// α.1: 索引操作 List[i] / Dict[key] / String[i]
pub fn index_value(obj: &Value, idx: &Value) -> Result<Value, String> {
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
pub fn value_to_string(v: &Value) -> String {
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

pub fn is_truthy(v: &Value) -> bool {
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
pub fn index_assign_value(obj: &mut Value, idx: &Value, val: &Value) -> Result<(), String> {
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
pub fn self_match_pattern(
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

/// α.10: 控制流信号（与 AST `FlowSignal` 同形）。
/// REPL 与 differential test 用来观察 Return/Break/Continue 出口；
/// 主路径 `run_mir` 直接返回 `Result<Value, String>`，不暴露信号。
#[derive(Debug, Clone, PartialEq)]
pub enum MirSignal {
    /// 正常落到函数末尾，无显式 return。
    None,
    /// 显式 `return value`。
    Return(Value),
    /// `break label`。
    Break,
    /// `continue label`。
    Continue,
    /// v0.70: Vote to halt — agent signals completion. In BSP context
    /// marks the vertex as Halted. Equivalent to Return in linear context.
    Halt(Option<Value>),
}

/// α.10: 与 `run_mir` 等价，但返回 `(MirSignal, Value)`，保留信号。
/// REPL/差分测试用此观测控制流出口；生产路径仍走 `run_mir`。
///
/// v0.75: 修复 — 此前无条件返回 `MirSignal::Return`，丢弃 `Flow::Halt`
/// （vote_to_halt）信号，导致 Pregel 引擎的 vertex_state 永远无法置为
/// Halted。现在通过 `run_dag_with_signal` 真正传播 Return/Halt。
pub fn run_mir_with_signal(
    func: &MirFunction,
    interp: &mut Interpreter,
    env: &mut Environment,
) -> Result<(MirSignal, Value), String> {
    let mut dag = crate::mir::dag::dag_analyze(func);
    crate::mir::optimize::dag_optimize(&mut dag);
    dag.prune_sequence_edges();
    crate::mir::dag_interp::run_dag_with_signal(&dag, func, interp, env)
}

/// α.10: `run_main_task` 的信号感知变体。
/// main task 中允许出现显式 `return value`——返回它的值；否则返回 Value::Nil。
pub fn run_main_task_with_signal(
    func: &MirFunction,
    interp: &mut Interpreter,
    env: &mut Environment,
) -> Result<(MirSignal, Value), String> {
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
    let Some(main_func) = main_body else {
        return Ok((MirSignal::None, Value::Nil));
    };
    let mut main_env = env.clone();
    let value = run_mir(main_func, interp, &mut main_env)?;
    Ok((MirSignal::Return(value.clone()), value))
}
