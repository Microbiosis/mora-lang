//! v0.59: DAG-aware MIR interpreter.
//!
//! Executes a `MirDag` using a BSP super-step model. All instruction
//! logic is delegated to `handlers::dispatch()`. The DAG layer only
//! controls execution ORDER: topological + BSP super-steps.
//!
//! With `dag.add_sequential_edges()`, this degenerates to linear
//! execution, making `run_mir ≡ run_dag`.

use std::collections::HashMap;

use crate::mir::dag::{EdgeKind, MirDag, MirDagNode};
use crate::mir::handlers::{self, Flow};
use crate::mir::interp as mir_interp;
use crate::mir::{MirFunction, MirInst};
use crate::value::Value;

use crate::interpreter::Interpreter;
use crate::interpreter::Environment;

pub fn run_mir_dag(
    func: &MirFunction,
    interp: &mut Interpreter,
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
    interp: &mut Interpreter,
    env: &mut Environment,
) -> Result<Value, String> {
    Ok(run_dag_with_signal(dag, func, interp, env)?.1)
}

/// v0.75: `run_dag` 的信号感知变体。
///
/// 返回 `(MirSignal, Value)`。此前 `run_mir_with_signal` 无条件包装成
/// `MirSignal::Return`，导致 `Flow::Halt`（vote_to_halt）信号被丢弃、
/// 引擎永远无法将顶点置为 Halted。此变体真正传播 Return/Halt 信号。
pub fn run_dag_with_signal(
    dag: &MirDag,
    func: &MirFunction,
    interp: &mut Interpreter,
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
                    if edge.from == n && is_control_edge(&edge.kind) { next.push(edge.to); }
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
                    let flow = handlers::dispatch(inst, &mut regs, interp, env, &task_registry)?;
                    if let Some(d) = inst.dst() { reg_ready[d] = true; result = regs[d].clone(); }
                    match flow {
                        Flow::Return(v) => { signal = MirSignal::Return(v.clone()); result = v; saw_return = true; }
                        Flow::Continue => {}
                        Flow::Jump(_) => {}
                        Flow::Halt(v) => { signal = MirSignal::Halt(v.clone()); result = v.unwrap_or(Value::Nil); saw_return = true; }
                    }
                }
                MirDagNode::Branch { cond, true_target, false_target } => {
                    if crate::flow::is_truthy(&regs[*cond]) {
                        if let Some(t) = true_target { next_active.push(*t); }
                    } else if let Some(f) = false_target {
                        next_active.push(*f);
                    }
                }
                MirDagNode::Jump { target } => {
                    if let Some(t) = target { next_active.push(*t); }
                }
                MirDagNode::Label { .. } | MirDagNode::Phi { .. } | MirDagNode::Removed => {}
            }
        }

        if saw_return { break; }

        let mut pushed: Vec<bool> = vec![false; dag.nodes.len()];
        for edge in &dag.edges {
            if ready.contains(&edge.from) {
                let should_push = match &edge.kind {
                    EdgeKind::Data { reg } => reg_ready[*reg],
                    _ => is_control_edge(&edge.kind) || matches!(edge.kind, EdgeKind::Sequence),
                };
                if should_push && !pushed[edge.to] {
                    next_active.push(edge.to); pushed[edge.to] = true;
                }
            }
        }
        active = next_active;
    }

    Ok((signal, result))
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
    matches!(kind, EdgeKind::Control | EdgeKind::ControlIfTrue | EdgeKind::ControlIfFalse | EdgeKind::BackEdge)
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::BinaryOp;
    use crate::interpreter::Interpreter;
    use crate::mir::{MirFunction, MirInst};
    use crate::value::Value;

    fn run(body: Vec<MirInst>) -> Result<Value, String> {
        let n_regs = body.iter().filter_map(|i| i.dst()).max().map(|r| r + 1).unwrap_or(1);
        let func = MirFunction { params: vec![], body, n_regs };
        let mut interp = Interpreter::new();
        let mut env = interp.take_env();
        run_mir_dag(&func, &mut interp, &mut env)
    }

    #[test]
    fn dag_exec_const() { assert_eq!(run(vec![MirInst::Const(0, Value::Int(42))]).unwrap(), Value::Int(42)); }

    #[test]
    fn dag_exec_binary_add() {
        assert_eq!(run(vec![MirInst::Const(0, Value::Int(10)), MirInst::Const(1, Value::Int(32)), MirInst::BinaryOp(2, 0, BinaryOp::Add, 1)]).unwrap(), Value::Int(42));
    }

    #[test]
    fn dag_exec_chain() {
        assert_eq!(run(vec![MirInst::Const(0, Value::Int(1)), MirInst::Const(1, Value::Int(2)), MirInst::BinaryOp(2, 0, BinaryOp::Add, 1), MirInst::Const(3, Value::Int(3)), MirInst::BinaryOp(4, 2, BinaryOp::Add, 3)]).unwrap(), Value::Int(6));
    }

    #[test]
    fn dag_exec_list() {
        assert_eq!(run(vec![MirInst::Const(0, Value::Int(1)), MirInst::Const(1, Value::Int(2)), MirInst::ListLit(2, vec![0, 1])]).unwrap(), Value::List(vec![Value::Int(1), Value::Int(2)]));
    }
}
