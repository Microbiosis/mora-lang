//! v0.59: DAG-aware MIR interpreter.
//!
//! Executes a `MirDag` using a BSP super-step model. All instruction
//! logic is delegated to `handlers::dispatch()`. The DAG layer only
//! controls execution ORDER: topological + BSP super-steps.
//!
//! With `dag.add_sequential_edges()`, this degenerates to linear
//! execution, making `run_mir ≡ run_dag`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::mir::dag::{EdgeKind, MirDag, MirDagNode};
use crate::mir::handlers::{self, Flow};
use crate::mir::host::MirHost;
use crate::mir::interp as mir_interp;
use crate::mir::{MirFunction, MirInst};
use crate::value::{Environment, Value};

/// v0.75.10: 寄存器级增量执行器状态（跨调用/超步记忆化）。
///
/// 只对「可证明纯计算」节点记忆化（白名单，见 [`is_memoizable_pure`]）：
/// 当节点的输入寄存器值与上次执行相等时跳过执行、复用上次输出。纯节点的
/// 输出完全由输入决定（零 env 读取、零副作用），因此跳过不改变任何可观察
/// 语义。副作用 / env 读取节点（Var/Call/Prompt/Send/...）永远重跑 —
/// 保守白名单保证增量安全。
///
/// 正确性关键：记忆按「输入值」判断，而非按超步号 — 即使 fault-retry
/// 回滚了引擎状态，被记录的输入由重跑的 Var 节点重建，与记录时相等 →
/// 跳过仍然正确（输入决定输出）。
pub struct DagExecMemo {
    /// node_id → 上次执行的输入寄存器值（相等性判断依据）
    last_inputs: HashMap<usize, Vec<Value>>,
    /// node_id → 上次输出（跳过时复用）
    last_outputs: HashMap<usize, Value>,
    /// 记忆化跳过的节点执行次数（stats 可观测性）
    pub skipped_nodes: usize,
    /// 实际执行的节点次数（stats 可观测性）
    pub executed_nodes: usize,
}

impl DagExecMemo {
    pub fn new() -> Self {
        Self {
            last_inputs: HashMap::new(),
            last_outputs: HashMap::new(),
            skipped_nodes: 0,
            executed_nodes: 0,
        }
    }

    /// 输入与上次相等则返回缓存输出（并计入 skipped），否则 None。
    fn reuse(&mut self, node_id: usize, inputs: &Vec<Value>) -> Option<Value> {
        if self.last_inputs.get(&node_id) == Some(inputs) {
            self.skipped_nodes += 1;
            self.last_outputs.get(&node_id).cloned()
        } else {
            None
        }
    }

    /// 记录本次执行（输入 + 输出），供下次比较/复用。
    fn record(&mut self, node_id: usize, inputs: Vec<Value>, output: Value) {
        self.last_inputs.insert(node_id, inputs);
        self.last_outputs.insert(node_id, output);
        self.executed_nodes += 1;
    }

    /// 是否积累了任何记忆（并行 RECONCILE 用于区分「跳过路径的空 memo」）。
    pub fn is_empty(&self) -> bool {
        self.last_inputs.is_empty()
    }
}

impl Default for DagExecMemo {
    fn default() -> Self {
        Self::new()
    }
}

/// 白名单：可证明纯计算的 MIR 指令（零 env 读取、零副作用、输出 = 输入函数）。
/// 对应 handlers::dispatch 中恒返回 `Flow::Continue` 的值指令。
/// 保守原则：不确定即排除（Var 读 env、Call/Pipe 可能副作用、Prompt 副作用、
/// MatchExpr 执行 arm body、Define/Assign 写 env、IndexAssign 就地修改 regs）。
fn is_memoizable_pure(inst: &MirInst) -> bool {
    matches!(
        inst,
        MirInst::Const(..)
            | MirInst::BinaryOp(..)
            | MirInst::ListLit(..)
            | MirInst::DictLit(..)
            | MirInst::Index(..)
            | MirInst::Expr(..)
    )
}

/// 带记忆化的 `run_dag_with_signal` 变体。`memo` 跨调用保持（pregel 每超步
/// 传同一 agent 的 memo），输入未变的纯节点被跳过。传 `&mut DagExecMemo::new()`
/// 即退化为普通执行（零增量）。
pub fn run_dag_with_signal_memo(
    dag: &MirDag,
    func: &MirFunction,
    memo: &mut DagExecMemo,
    interp: &mut dyn MirHost,
    env: &mut Environment,
) -> Result<(mir_interp::MirSignal, Value), String> {
    use mir_interp::MirSignal;
    let task_registry = mir_interp::build_task_registry(&func.body);
    let mut regs: Vec<Value> = vec![Value::Nil; dag.n_regs];
    let mut reg_ready: Vec<bool> = vec![false; dag.n_regs];
    let mut active: Vec<usize> = dag.entry.clone();
    let mut exec_count: Vec<usize> = vec![0; dag.nodes.len()];

    const MAX_EXECUTIONS: usize = 500;
    const MAX_STEPS: u32 = 10000;
    let mut step = 0;
    let mut result: Value = Value::Nil;
    let mut signal: MirSignal = MirSignal::None;

    while !active.is_empty() && step < MAX_STEPS {
        step += 1;

        let ready: Vec<usize> = active
            .iter()
            .filter(|&&n| exec_count[n] < MAX_EXECUTIONS && node_ready(&dag.nodes[n], &reg_ready))
            .copied()
            .collect();

        if ready.is_empty() {
            let mut next: Vec<usize> = Vec::new();
            for &n in &active {
                for edge in &dag.edges {
                    if edge.from == n && is_control_edge(&edge.kind) {
                        next.push(edge.to);
                    }
                }
            }
            active = next;
            continue;
        }

        let mut next_active: Vec<usize> = Vec::new();
        let mut saw_return = false;

        for &node_id in &ready {
            exec_count[node_id] += 1;
            if exec_count[node_id] > MAX_EXECUTIONS {
                return Err(format!("DAG node {} loop", node_id));
            }

            match &dag.nodes[node_id] {
                MirDagNode::Compute { inst, .. } | MirDagNode::Effect { inst } => {
                    // v0.75.10: 纯节点输入与上次相等 → 跳过执行，复用输出。
                    // Compute 的 input_regs 存于节点；Effect 无该字段，
                    // 从 inst.input_regs() 推导（同一输入集合）。
                    let pure = is_memoizable_pure(inst);
                    let inputs: Vec<Value> = if pure {
                        match &dag.nodes[node_id] {
                            MirDagNode::Compute { input_regs, .. } => {
                                input_regs.iter().map(|r| regs[*r].clone()).collect()
                            }
                            MirDagNode::Effect { inst } => {
                                inst.input_regs().iter().map(|r| regs[*r].clone()).collect()
                            }
                            _ => Vec::new(),
                        }
                    } else {
                        Vec::new()
                    };
                    if pure && let Some(cached) = memo.reuse(node_id, &inputs) {
                        if let Some(d) = inst.dst() {
                            regs[d] = cached;
                            reg_ready[d] = true;
                            result = regs[d].clone();
                        }
                        // 纯节点恒 Flow::Continue — 无控制流副作用可跳过。
                        continue;
                    }

                    let flow = handlers::dispatch(inst, &mut regs, interp, env, &task_registry)?;
                    if pure {
                        if let Some(d) = inst.dst() {
                            memo.record(node_id, inputs, regs[d].clone());
                        } else {
                            memo.record(node_id, inputs, Value::Nil);
                        }
                    }
                    if let Some(d) = inst.dst() {
                        reg_ready[d] = true;
                        result = regs[d].clone();
                    }
                    match flow {
                        Flow::Return(v) => {
                            signal = MirSignal::Return(v.clone());
                            result = v;
                            saw_return = true;
                        }
                        Flow::Continue => {}
                        Flow::Jump(_) => {}
                        Flow::Halt(v) => {
                            signal = MirSignal::Halt(v.clone());
                            result = v.unwrap_or(Value::Nil);
                            saw_return = true;
                        }
                    }
                }
                MirDagNode::Branch {
                    cond,
                    true_target,
                    false_target,
                } => {
                    if crate::flow::is_truthy(&regs[*cond]) {
                        if let Some(t) = true_target {
                            next_active.push(*t);
                        }
                    } else if let Some(f) = false_target {
                        next_active.push(*f);
                    }
                }
                MirDagNode::Jump { target } => {
                    if let Some(t) = target {
                        next_active.push(*t);
                    }
                }
                MirDagNode::Label { .. } | MirDagNode::Phi { .. } | MirDagNode::Removed => {}
            }
        }

        if saw_return {
            break;
        }

        let mut pushed: Vec<bool> = vec![false; dag.nodes.len()];
        for edge in &dag.edges {
            if ready.contains(&edge.from) {
                let should_push = match &edge.kind {
                    EdgeKind::Data { reg } => reg_ready[*reg],
                    _ => is_control_edge(&edge.kind) || matches!(edge.kind, EdgeKind::Sequence),
                };
                if should_push && !pushed[edge.to] {
                    next_active.push(edge.to);
                    pushed[edge.to] = true;
                }
            }
        }
        active = next_active;
    }

    Ok((signal, result))
}

pub fn run_mir_dag(
    func: &Arc<MirFunction>,
    interp: &mut dyn MirHost,
    env: &mut Environment,
) -> Result<Value, String> {
    let dag = crate::mir::dag::dag_analyze(func);
    let val = run_dag(&dag, func, interp, env)?;
    if func.body.iter().any(|i| matches!(i, MirInst::TaskDef { name, params, .. } if name == "main" && params.is_empty())) {
        mir_interp::run_main_task(func, interp, env)?;
    }
    Ok(val)
}

pub fn run_dag(
    dag: &MirDag,
    func: &MirFunction,
    interp: &mut dyn MirHost,
    env: &mut Environment,
) -> Result<Value, String> {
    Ok(run_dag_with_signal(dag, func, interp, env)?.1)
}

/// v0.75: `run_dag` 的信号感知变体。
///
/// 返回 `(MirSignal, Value)`。此前 `run_mir_with_signal` 无条件包装成
/// `MirSignal::Return`，导致 `Flow::Halt`（vote_to_halt）信号被丢弃、
/// 引擎永远无法将顶点置为 Halted。此变体真正传播 Return/Halt 信号。
///
/// v0.75.10: 委托给 [`run_dag_with_signal_memo`]（每次新 memo = 无增量，
/// 语义与旧实现完全一致）。需要跨调用增量的调用方（pregel）用 memo 变体。
pub fn run_dag_with_signal(
    dag: &MirDag,
    func: &MirFunction,
    interp: &mut dyn MirHost,
    env: &mut Environment,
) -> Result<(mir_interp::MirSignal, Value), String> {
    run_dag_with_signal_memo(dag, func, &mut DagExecMemo::new(), interp, env)
}

fn node_ready(node: &MirDagNode, reg_ready: &[bool]) -> bool {
    match node {
        MirDagNode::Compute { input_regs, .. } => input_regs.iter().all(|r| reg_ready[*r]),
        MirDagNode::Branch { cond, .. } => reg_ready[*cond],
        MirDagNode::Effect { inst } => inst.input_regs().iter().all(|r| reg_ready[*r]),
        _ => true,
    }
}

fn is_control_edge(kind: &EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Control | EdgeKind::ControlIfTrue | EdgeKind::ControlIfFalse | EdgeKind::BackEdge
    )
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::BinaryOp;
    use crate::interpreter::Interpreter;
    use crate::mir::host::MirHost;
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
