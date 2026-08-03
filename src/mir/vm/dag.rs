//! v0.75.61: DAG 超步执行器 — 原 vm.rs 的 DAG 区（BSP 超步模型，生产主路径）。
//! 自 vm.rs 拆出（D6 单文件惯例）。所有指令逻辑委托 handlers::dispatch；
//! 本层只控制执行顺序（拓扑 + BSP 超步）。线性区（run_mir）仍在 vm.rs。

use std::collections::HashMap;
use std::sync::Arc;

use super::{MirSignal, build_task_registry, run_main_task};

use crate::mir::host::MirHost;
use crate::mir::{MirFunction, MirInst};
use crate::value::{Environment, Value};

// v0.59: DAG-aware MIR interpreter（原 dag_interp.rs）
// ===================================================================
// v0.59 部分（原 dag_interp.rs 模块文档，已并入 vm.rs）：
// v0.59: DAG-aware MIR interpreter.
//
// Executes a `MirDag` using a BSP super-step model. All instruction
// logic is delegated to `handlers::dispatch()`. The DAG layer only
// controls execution ORDER: topological + BSP super-steps.
//
// With `dag.add_sequential_edges()`, this degenerates to linear
// execution, making `run_mir ≡ run_dag`.
//
// # 执行边界（v0.75.33，v0.75.36 修正）
//
// 本解释器为 **pregel 顶点执行 + 生产主路径** 双用途：pregel BSP 引擎
// 逐超步调用，同时 `run_mir`（main.rs/REPL/import）经 `run_dag_with_signal`
// 也走本解释器——**生产路径全部经过 DAG 解释器，不存在「循环走线性
// fallback」**。
// - 无循环的直线/分支程序：正确（Sequence 前驱判定保证 Define/Var 顺序）。
// - 含 `MirInst` 循环（for/while 降级到 JumpIf 回边）的程序：v0.75.34 起
//   正确（块内全序 + 控制转移 handler 决定 + wave 去重），循环累加验证
//   输出 6/45。回归保护：`tests/tier0_replacement.rs`、`orchestrate_v3_pipeline.rs`。
// - 优化器（CSE/DeadNode/ConstFolding）删除/合并节点时不得破坏控制目标
//   与寄存器消费者（dag_rule/dag_search 的 guard + reg_rename 负责）。

use crate::mir::dag::{EdgeKind, MirDag, MirDagNode};
use crate::mir::handlers::{self, Flow};

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
) -> Result<(MirSignal, Value), String> {
    use MirSignal;
    let task_registry = build_task_registry(&func.body);
    let mut regs: Vec<Value> = vec![Value::Nil; dag.n_regs];
    let mut reg_ready: Vec<bool> = vec![false; dag.n_regs];
    let mut active: Vec<usize> = dag.entry.clone();
    let mut exec_count: Vec<usize> = vec![0; dag.nodes.len()];
    // v0.75.33: 每节点是否已执行 — Sequence 前驱就绪判定用（见 ready 过滤）。
    let mut executed: Vec<bool> = vec![false; dag.nodes.len()];

    const MAX_EXECUTIONS: usize = 500;
    const MAX_STEPS: u32 = 10000;
    let mut step = 0;
    let mut result: Value = Value::Nil;
    let mut signal: MirSignal = MirSignal::None;

    while !active.is_empty() && step < MAX_STEPS {
        step += 1;

        let ready: Vec<usize> = active
            .iter()
            .filter(|&&n| {
                exec_count[n] < MAX_EXECUTIONS
                    && node_ready(&dag.nodes[n], &reg_ready)
                    // v0.75.33: Sequence 前驱必须已执行 — 仅 data-ready 不够：
                    // 无输入寄存器的节点（Var/Define 等）一激活即可执行，若其
                    // Sequence 前驱（如 Define 语句）仍在本波未执行，会提前
                    // 读脏值。示例：`let c = 5` 的 Define(c) 与下一句
                    // `let d = c + 1` 的 Var(c) 同波就绪 → Var(c) 先跑读 Nil。
                    && dag.edges.iter().all(|e| {
                        e.to != n
                            || !matches!(e.kind, crate::mir::dag::EdgeKind::Sequence)
                            || executed[e.from]
                    })
            })
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

        // v0.75.33: 统一去重 — 本 wave 已执行的节点（ready）不再被重调度；
        // Branch/Jump handler 的 push 与 scan 的 push 共用同一 pushed 标记，
        // 防止同 wave 重复执行（此前 scan 会把 25→26 的 Sequence 边把已执行的
        // n26 重新推入 → body 链每轮重复激活、归纳变量漂移 → 越界）。
        let mut pushed: Vec<bool> = vec![false; dag.nodes.len()];
        for &n in &ready {
            pushed[n] = true;
        }

        for &node_id in &ready {
            exec_count[node_id] += 1;
            if exec_count[node_id] > MAX_EXECUTIONS {
                return Err(format!("DAG node {} loop", node_id));
            }
            executed[node_id] = true;

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
                    let chosen = if crate::flow::is_truthy(&regs[*cond]) {
                        true_target
                    } else {
                        false_target
                    };
                    if let Some(t) = chosen
                        && !pushed[*t]
                    {
                        pushed[*t] = true;
                        next_active.push(*t);
                    }
                }
                MirDagNode::Jump { target } => {
                    if let Some(t) = target
                        && !pushed[*t]
                    {
                        pushed[*t] = true;
                        next_active.push(*t);
                    }
                }
                MirDagNode::Label { .. } | MirDagNode::Phi { .. } | MirDagNode::Removed => {}
            }
        }

        if saw_return {
            break;
        }

        // 边传播：只调度本 wave 已执行节点的消费者（Branch/Jump 的转移
        // 已由 handler 决定，见下）。`pushed` 在 wave 开头创建并标记了
        // ready 节点，scan 不会把已执行/已调度的节点重复推入。
        for edge in &dag.edges {
            if ready.contains(&edge.from) {
                // v0.75.33: 分支/Jump 节点的控制转移完全由 handler 决定
                // （Branch 只推选中的 target、Jump 只推 target）。此处若再
                // 无条件推送其出边，会把两个分支目标都激活 — exit 与 body
                // 同 wave 竞态执行（after-loop 读脏值、body 用越界 i 再跑）。
                // 示例：for 循环 i==len 时 exit 被推 27、body 同时被
                // ControlIfFalse/Sequence 推 19 → Index 越界 OOB。
                if matches!(
                    dag.nodes[edge.from],
                    MirDagNode::Branch { .. } | MirDagNode::Jump { .. }
                ) {
                    continue;
                }
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
        run_main_task(func, interp, env)?;
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
) -> Result<(MirSignal, Value), String> {
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
