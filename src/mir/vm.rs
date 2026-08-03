//! v0.75.47: VM 执行内核 — 由 interp.rs + dag_interp.rs 合并而成
//! （SQLite VDBE 单文件惯例，P4）。
//!
//! - interp 部分：`run_mir` / `run_mir_with_signal` / `MirSignal` /
//!   `build_task_registry` / `run_main_task` / 索引与模式匹配辅助。
//! - dag_interp 部分：`run_dag_with_signal*` — BSP 超步模型，生产主路径。
//!   `run_mir ≡ run_dag`（dag.add_sequential_edges 后退化线性）。
//!
//! 模块引用：外部统一 `crate::mir::vm::`（旧 `mir::interp::` /
//! `mir::dag_interp::` 已合并，见 CHANGELOG v0.75.47）。

//! MIR 解释器（α.0）
//!
//! pc 循环执行 MirFunction。控制流用 Jump/Return/Break/Continue 直接改 pc，
//! 替代 AST 解释器的 FlowSignal 枚举层层传返。
//!
//! α.0 复用现有宿主（MirHost）的 call_function / eval_binary，不重写 builtins。
//! 这样 MIR 解释器只替代"执行引擎"，AI/transport/sandbox facade 不受影响。
//!
//! v0.75.x: 参数类型从 `&mut Interpreter` 改为 `&mut dyn MirHost`（mir/host.rs），
//! 解耦 mir ↔ interpreter 双向依赖。Interpreter 实现 MirHost。

use std::collections::HashMap;
use std::sync::Arc;

use super::{MirFunction, MirInst, cache};

use crate::mir::host::MirHost;
use crate::value::{Environment, Value};
/// float 模式匹配容差（`pat_str = "float:x"` 时 |实际 - x| < 此值判等）。
/// 与 eval 的 tolerance 语义一致：浮点比较用 epsilon 而非 ==。
const FLOAT_PATTERN_EPSILON: f64 = 1e-9;

/// Build a task registry from a MirFunction body.
/// Maps task name → (parameter names, body function).
pub fn build_task_registry(body: &[MirInst]) -> HashMap<&str, (&[String], &MirFunction)> {
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
///
/// v0.75.9: 接收 `&Arc<MirFunction>` — 优化后 DAG 走全局缓存
/// （`cache::DAG_CACHE`，key = Arc 指针），同 Arc 跨调用复用。
pub fn run_mir(
    func: &Arc<MirFunction>,
    interp: &mut dyn MirHost,
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

/// 值真值判定（MIR 线性执行：h_jump_if / h_jump_if_not）。
///
/// 注意与 `crate::flow::is_truthy` 的语义差异：本实现 List/Dict 恒真
/// （不判空），而 flow 版对 List/Dict 判非空（`!l.is_empty()`）——后者
/// 用于 DAG Branch 条件（vm/dag.rs）。两处语义分叉是历史残留，收敛前
/// 修改任一方必须同步另一方并更新对应测试。
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
        return (f - n).abs() < FLOAT_PATTERN_EPSILON;
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
///
/// v0.75.9: 接收 `&Arc<MirFunction>`；TaskDef body 克隆进 Arc 以便走
/// `run_mir` 的全局 DAG 缓存。
pub fn run_main_task(
    func: &Arc<MirFunction>,
    interp: &mut dyn MirHost,
    env: &mut Environment,
) -> Result<(), String> {
    let mut main_body: Option<Arc<MirFunction>> = None;
    for inst in &func.body {
        if let MirInst::TaskDef { name, params, body } = inst
            && name == "main"
            && params.is_empty()
        {
            main_body = Some(Arc::new((**body).clone()));
            break;
        }
    }
    if let Some(main_func) = main_body {
        let mut main_env = env.clone();
        let _ = run_mir(&main_func, interp, &mut main_env)?;
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
///
/// v0.75.9: 接收 `&Arc<MirFunction>`，优化后 DAG 走全局缓存
/// （`cache::global_dag_cache().get_or_build`），同一 Arc 跨调用复用，
/// 不再每次 `dag_analyze + dag_optimize + prune_sequence_edges` 全量重建。
///
/// v0.75.27: 委托给 `run_mir_with_signal_cached`（全局缓存为默认注入）。
pub fn run_mir_with_signal(
    func: &Arc<MirFunction>,
    interp: &mut dyn MirHost,
    env: &mut Environment,
) -> Result<(MirSignal, Value), String> {
    run_mir_with_signal_cached(func, interp, env, cache::global_dag_cache())
}

/// v0.75.27: 可注入缓存变体 — 测试/多租户可传独立 `DagCache` 实例隔离
/// 缓存状态（全局 OnceLock 解耦的注入点）。行为与 `run_mir_with_signal`
/// 完全一致，仅缓存来源不同。
pub fn run_mir_with_signal_cached(
    func: &Arc<MirFunction>,
    interp: &mut dyn MirHost,
    env: &mut Environment,
    dag_cache: &cache::DagCache,
) -> Result<(MirSignal, Value), String> {
    let dag = dag_cache.get_or_build(func);
    crate::mir::vm::run_dag_with_signal(&dag, func, interp, env)
}

/// α.10: `run_main_task` 的信号感知变体。
/// main task 中允许出现显式 `return value`——返回它的值；否则返回 Value::Nil。
pub fn run_main_task_with_signal(
    func: &Arc<MirFunction>,
    interp: &mut dyn MirHost,
    env: &mut Environment,
) -> Result<(MirSignal, Value), String> {
    let mut main_body: Option<Arc<MirFunction>> = None;
    for inst in &func.body {
        if let MirInst::TaskDef { name, params, body } = inst
            && name == "main"
            && params.is_empty()
        {
            main_body = Some(Arc::new((**body).clone()));
            break;
        }
    }
    let Some(main_func) = main_body else {
        return Ok((MirSignal::None, Value::Nil));
    };
    let mut main_env = env.clone();
    let value = run_mir(&main_func, interp, &mut main_env)?;
    Ok((MirSignal::Return(value.clone()), value))
}

// ===================================================================

mod dag; // v0.75.61: DAG 超步执行器（原 dag_interp 部分，自 vm.rs 拆出）
pub use dag::*; // 保持 vm::run_dag* / vm::DagExecMemo 路径（P4 契约不变）

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::BinaryOp;
    use crate::interpreter::Interpreter;
    use crate::mir::{MirFunction, MirInst};
    use crate::value::Value;

    fn run(body: Vec<MirInst>) -> Result<Value, String> {
        let n_regs = body
            .iter()
            .filter_map(|i| i.dst())
            .max()
            .map(|r| r + 1)
            .unwrap_or(1);
        let func = MirFunction {
            params: vec![],
            body,
            n_regs,
        };
        let mut interp = Interpreter::new();
        let mut env = interp.take_env();
        // v0.75.9: 包裹 Arc（run_mir_dag 签名变更）
        run_mir_dag(&Arc::new(func), &mut interp, &mut env)
    }

    #[test]
    fn dag_exec_const() {
        assert_eq!(
            run(vec![MirInst::Const(0, Value::Int(42))]).unwrap(),
            Value::Int(42)
        );
    }

    #[test]
    fn dag_exec_binary_add() {
        assert_eq!(
            run(vec![
                MirInst::Const(0, Value::Int(10)),
                MirInst::Const(1, Value::Int(32)),
                MirInst::BinaryOp(2, 0, BinaryOp::Add, 1)
            ])
            .unwrap(),
            Value::Int(42)
        );
    }

    #[test]
    fn dag_exec_chain() {
        assert_eq!(
            run(vec![
                MirInst::Const(0, Value::Int(1)),
                MirInst::Const(1, Value::Int(2)),
                MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
                MirInst::Const(3, Value::Int(3)),
                MirInst::BinaryOp(4, 2, BinaryOp::Add, 3)
            ])
            .unwrap(),
            Value::Int(6)
        );
    }

    #[test]
    fn dag_exec_list() {
        assert_eq!(
            run(vec![
                MirInst::Const(0, Value::Int(1)),
                MirInst::Const(1, Value::Int(2)),
                MirInst::ListLit(2, vec![0, 1])
            ])
            .unwrap(),
            Value::List(vec![Value::Int(1), Value::Int(2)])
        );
    }

    // ─── v0.75.10: 寄存器级增量（DagExecMemo）─────────────────────────

    /// 同一 memo 连跑两次同一 body：第一次全量，第二次复用记忆。
    /// 返回两次结果 + 最终 memo。
    fn run_memo_twice(body: Vec<MirInst>) -> (Value, Value, DagExecMemo) {
        let n_regs = body
            .iter()
            .filter_map(|i| i.dst())
            .max()
            .map(|r| r + 1)
            .unwrap_or(1);
        let func = MirFunction {
            params: vec![],
            body: body.clone(),
            n_regs,
        };
        let dag = crate::mir::dag::dag_analyze(&func);
        let mut memo = DagExecMemo::new();
        let run = |memo: &mut DagExecMemo| -> Value {
            let mut interp = Interpreter::new();
            let mut env = interp.take_env();
            run_dag_with_signal_memo(&dag, &func, memo, &mut interp, &mut env)
                .expect("memo run should succeed")
                .1
        };
        let v1 = run(&mut memo);
        let v2 = run(&mut memo);
        (v1, v2, memo)
    }

    /// 第二次跑（env 空、regs 重建）时，纯节点输入与上次相等 → 全部跳过。
    /// 结果不变（记忆化不改变语义），且第二次无实际执行。
    #[test]
    fn memo_second_run_skips_pure_nodes() {
        let (v1, v2, memo) = run_memo_twice(vec![
            MirInst::Const(0, Value::Int(1)),
            MirInst::Const(1, Value::Int(2)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
        ]);
        assert_eq!(v1, Value::Int(3));
        assert_eq!(v2, Value::Int(3), "记忆化不得改变结果");
        assert_eq!(memo.executed_nodes, 3, "第一次全量执行");
        assert_eq!(memo.skipped_nodes, 3, "第二次纯节点全部跳过");
    }

    /// 输入变化 → 不跳过（重执行），记忆仍正确。
    /// 用 Var 重建 regs（Var 非纯，永远重跑）驱动 BinaryOp 输入变化。
    #[test]
    fn memo_input_change_forces_recompute() {
        let n_regs = 3;
        let body = vec![
            MirInst::Var(0, "a".to_string()),
            MirInst::Const(1, Value::Int(10)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
        ];
        let dag = crate::mir::dag::dag_analyze(&MirFunction {
            params: vec![],
            body: body.clone(),
            n_regs,
        });
        let func = MirFunction {
            params: vec![],
            body,
            n_regs,
        };
        let mut memo = DagExecMemo::new();
        let run = |env_val: i64, memo: &mut DagExecMemo| -> Value {
            let mut interp = Interpreter::new();
            let mut env = interp.take_env();
            env.define("a".to_string(), Value::Int(env_val), false);
            run_dag_with_signal_memo(&dag, &func, memo, &mut interp, &mut env)
                .expect("memo run should succeed")
                .1
        };
        assert_eq!(run(1, &mut memo), Value::Int(11));
        assert_eq!(run(1, &mut memo), Value::Int(11), "a 未变 → BinaryOp 跳过");
        // run2：Var 重跑（非纯），Const（输入为空，未变）与 BinaryOp（输入
        // (1,10) 与记录相等）跳过 → skipped=2。
        assert_eq!(memo.skipped_nodes, 2, "第二次 Const + BinaryOp 跳过");
        assert_eq!(run(5, &mut memo), Value::Int(15), "a 变 → BinaryOp 重算");
        // run3：Var 重跑，Const 再跳过，BinaryOp 输入 (5,10) ≠ 记录 (1,10) → 重算。
        // skipped 累计 = 2 + 1（Const）= 3。
        assert_eq!(memo.skipped_nodes, 3);
    }

    /// 纯白名单不含 env 读取：含 Var 的程序第二次跑不被全跳（Var 重跑）。
    #[test]
    fn memo_var_not_skipped() {
        let n_regs = 2;
        let body = vec![MirInst::Var(0, "x".to_string())];
        let dag = crate::mir::dag::dag_analyze(&MirFunction {
            params: vec![],
            body: body.clone(),
            n_regs,
        });
        let func = MirFunction {
            params: vec![],
            body,
            n_regs,
        };
        let mut memo = DagExecMemo::new();
        let run = |memo: &mut DagExecMemo| -> Value {
            let mut interp = Interpreter::new();
            let mut env = interp.take_env();
            env.define("x".to_string(), Value::Int(7), false);
            run_dag_with_signal_memo(&dag, &func, memo, &mut interp, &mut env)
                .expect("memo run should succeed")
                .1
        };
        assert_eq!(run(&mut memo), Value::Int(7));
        assert_eq!(run(&mut memo), Value::Int(7));
        // Var 非纯（env 读取不可记忆）：永不 record（executed_nodes 只统计纯节点），
        // 也永不跳过。
        assert_eq!(memo.executed_nodes, 0, "Var 不在纯白名单，不记 memo");
        assert_eq!(memo.skipped_nodes, 0, "Var 每次重跑，无跳过");
    }
}
