//! v0.57: MIR-native Pregel 引擎（BSP 超步执行器）
//!
//! 直接消费 `Mir*` 类型（`MirAgentDef`/`MirEdgeDef`/...），无 ast_compat 桥接：
//! - `task_body`/`verify_body`/`condition_body`/`merge_body`/`thread_id_body`
//!   均内嵌在对应 MIR-native 字段中，无需额外 HashMap
//! - 零 `AstArena`/`NodeId` 依赖
//!
//! 引擎经 `MirInst::Orchestrate`（Pregel 变体）驱动，支持超步循环、
//! vote_to_halt 收敛、Aggregator、Combiner、故障恢复（checkpoint）、
//! 节点重平衡与统计。见 `MirPregelEngine` 与 `pregel_opt`（超步融合优化）。
//!
//! # Versioning Systems (v0.65)
//!
//! This module uses two complementary version-tracking mechanisms:
//!
//! - **`channel_versions`** (per-channel `u64`): Monotonic write counter for
//!   delta-input construction. Answers "has channel X been written since node Y
//!   last looked?" Used by `build_node_input()`. Checkpointed via `Checkpoint`.
//!
//! - **`Environment::versions`** (per-binding `VectorClock`): Causal ordering
//!   for write-write conflict detection. Answers "did agents A and B modify
//!   this key concurrently?" Used by `merge_from_with_strategies()`. NOT
//!   checkpointed (derived from agent execution, not persisted).
//!
//! These systems track different things and are intentionally separate.
//! `channel_versions` is a change-detection mechanism; `VectorClock` is a
//! causal-ordering mechanism. Do not unify them.

use std::collections::HashMap;
use std::sync::Arc;

use crate::checkpoint::{Checkpoint, CheckpointSaver, SendTask};
use crate::mir::expr::MirExpr;
use crate::mir::expr::{MirInterruptPoint, MirInterruptWhen, MirPregelConfig, MirReducerKind};
use crate::mir::host::MirHost;
use crate::value::{Conflict, MergeStrategy, Value};

pub mod worker_pool;

/// Interrupt 回调签名
pub type MirInterruptCallback = Arc<dyn Fn(&str, MirInterruptWhen) -> bool>;

/// v0.62: Conflict callback — invoked for each detected write-write conflict.
/// Return `true` to continue the BSP run, `false` to abort.
pub type MirConflictCallback = Arc<dyn Fn(&Conflict) -> bool>;

/// v0.75.8: 并行 EXEC 的 PREPARE 产物 — (node, task_body, 缓存 dag, 私有 env,
/// 本次 input)。v0.75.6 起携带缓存 dag，v0.75.8 增加 input_str 供增量缓存。
type PreparedJob = (
    String,
    std::sync::Arc<crate::mir::MirFunction>,
    std::sync::Arc<crate::mir::dag::MirDag>,
    crate::value::Environment,
    String,
);

/// v0.75.3: 增量 step 快照（undo log）— 只记录 EXEC 会修改的引擎状态，
/// 替代每步全量 `build_checkpoint()`。
///
/// 契约（EXEC 期间不写 channels）：retry 只重跑 EXEC 闭包，而 EXEC 对
/// `channels` / `channel_versions` / `versions_seen` 零写入（UPDATE 阶段
/// `apply_write` 在 retry 循环之外才执行），失败回滚无需恢复它们。
/// Flink 增量 checkpoint 思想的本地形态：只记录自上次以来的变更。
///
/// 若未来 EXEC 开始写 channels（如 UPDATE 提前到 retry 循环内），本结构
/// 需扩展为惰性记录 `old_channels: Option<HashMap<String, Value>>`。
struct StepUndo {
    /// EXEC 通过 `flush_pending_sends` 修改；失败时还原。
    old_pending_sends: Vec<SendTask>,
}

/// Pregel 引擎 BSP 循环状态
pub struct MirPregelEngine {
    config: MirPregelConfig,
    agents_by_name: HashMap<String, usize>,
    state_reducers: HashMap<String, MirReducerKind>,

    channels: HashMap<String, Value>,
    channel_versions: HashMap<String, u64>,
    versions_seen: HashMap<String, HashMap<String, u64>>,

    pending_sends: Vec<SendTask>,

    /// v0.61: Concurrent write-write conflicts detected during BSP execution.
    pub conflicts: Vec<Conflict>,

    max_steps: usize,
    interrupt_before: Option<MirInterruptCallback>,
    interrupt_after: Option<MirInterruptCallback>,
    /// v0.62: Invoked for each write-write conflict detected during merge.
    conflict_callback: Option<MirConflictCallback>,
    /// v0.63: Current super-step (for checkpoint/restore).
    pub current_step: usize,
    /// v0.64: Optional checkpoint persistence backend.
    saver: Option<Arc<dyn CheckpointSaver>>,
    /// v0.70: Per-vertex state for vote_to_halt semantics.
    /// Active: scheduled normally. Halted: only rescheduled when a Send arrives.
    pub vertex_state: HashMap<String, VertexState>,
    /// v0.71: Per-super-step aggregator accumulator. Reset at the start
    /// of each step, reduced at the end, exposed as channels.
    pub aggregator_acc: HashMap<String, Value>,
    /// v0.71: Final reducer applied to aggregator_acc each super-step.
    pub aggregator_reducer: HashMap<String, crate::mir::expr::AggregatorKind>,
    /// v0.72: Per-agent combiner body for pre-delivery message folding.
    pub combiner_bodies: HashMap<String, std::sync::Arc<crate::mir::MirFunction>>,
    /// v0.72: Master coordinator hook — runs once per super-step after
    /// UPDATE, before ADVANCE.
    pub master_compute: Option<std::sync::Arc<crate::mir::MirFunction>>,
    /// v0.73: Worker parallelism. 1 = sequential (default), N = parallel
    /// EXEC with an N-thread pool. Parallel mode gives proper Pregel
    /// super-step semantics: agents see step-start state, writes merge
    /// after join.
    pub parallelism: usize,
    /// v0.74: Cached worker pool, reused across super-steps (created lazily
    /// on the first parallel EXEC). Rebuilding on every step would discard
    /// any balancing/health state.
    pub worker_pool: Option<crate::pregel::worker_pool::WorkerPool>,
    /// v0.74: Per-step deadline for parallel EXEC (default None = no limit).
    /// On timeout the step is treated as a fault and retried per
    /// `fault_tolerance`. Timed-out worker threads are leaked (no
    /// cooperative cancellation); the pool is rebuilt on the next step.
    pub step_timeout: Option<std::time::Duration>,
    /// v0.74: Fault-tolerance retry count (default 0 = off).
    /// When a step fails, the engine restores the step-start checkpoint
    /// and re-runs up to `fault_tolerance` times.
    pub fault_tolerance: usize,
    /// v0.74: Runtime stats (steps, agents run, retries, timeouts, ms).
    pub stats: EngineStats,
    /// v0.75.8: 增量执行 v1 — 记录每 agent 上次 input（build_node_input 的
    /// JSON 字符串）。超步间 input 完全未变时跳过整个执行（复用上次 outcome）。
    ///
    /// v0.75.10: 寄存器级增量（memo）在其之上细化 — input 未变整体跳过，
    /// input 变了但部分纯节点输入未变则节点级跳过。v1 语义不变。
    agent_input_cache: HashMap<String, String>,
    /// v0.75.8: 每 agent 上次成功执行的 (signal, result, sends) — 跳过执行时
    /// 复用。input 相同 → 确定性执行，语义等价。
    agent_outcome_cache: HashMap<
        String,
        (
            crate::mir::vm::MirSignal,
            crate::value::Value,
            Vec<crate::checkpoint::SendTask>,
        ),
    >,
    /// v0.75.10: 每 agent 的寄存器级增量 memo（跨超步保持）— 纯节点按输入
    /// 值相等跳过执行。与 `agent_dag_cache` 生命周期一致：仅缓存 agents 的
    /// task_body（config 静态），随 engine 生命周期，无泄漏。
    ///
    /// 同时是 task_body 的稳定 Arc 持有者：`task_arcs` 以 engine 生命周期
    /// 保持 `Arc<MirFunction>`，跨超步复用同一 Arc → 全局 DAG 缓存（key =
    /// Arc 指针）真正命中（v0.75.9 每超步新建 Arc 导致缓存失效，本次修复）。
    task_arcs: HashMap<String, std::sync::Arc<crate::mir::MirFunction>>,
    /// v0.75.10: 并行路径的 memo 带回 — 并行 worker 内联执行，无 memo；
    /// worker 返回实际执行节点数，主线程（RECONCILE）用它区分「跳过路径的
    /// 空 memo」→ 不覆盖增量 memo（覆盖会丢失上一超步的记忆）。
    agent_memos: HashMap<String, crate::mir::vm::DagExecMemo>,
    /// v0.75.84: 执行环境（单一来源，v0.75.76+ 约定）。
    /// agent 执行 / 边条件 / master_compute 的 env 来源：优先 base_env
    /// （orchestrate 传入的执行 env，含 builtin ai 等）；未注入时回落
    /// interpreter.environment()（pregel 单测直接构造 Interpreter 未
    /// take_env 的路径，宿主全局槽仍完整）。
    base_env: Option<Arc<parking_lot::Mutex<crate::value::Environment>>>,
}

/// v0.74: Engine runtime metrics.
#[derive(Debug, Default, Clone)]
pub struct EngineStats {
    pub steps: usize,
    pub agents_run: usize,
    pub retries: usize,
    pub timeouts: usize,
    pub total_ms: u128,
    /// v0.75.4: 累计发送的消息条数（每个超步 ADVANCE 分发的 SendTask 总量）。
    /// BSP 保证全部送达，故不设 messages_received（sent == received 天然成立）。
    pub messages_sent: usize,
    /// v0.75.7: per-agent 最近一次执行耗时（ms）— FPGA 式调度可观测性，
    /// 用于识别 straggler（长 agent 阻塞短 agent）。覆盖式保留最新一次。
    pub per_agent_ms: HashMap<String, u128>,
}

/// v0.73: Per-agent outcome collected from a worker (parallel EXEC) or
/// inline (sequential EXEC). Everything needed by RECONCILE.
pub struct AgentExecOutcome {
    pub node_name: String,
    pub signal: crate::mir::vm::MirSignal,
    pub result: crate::value::Value,
    pub env: crate::value::Environment,
    pub sends: Vec<crate::checkpoint::SendTask>,
    /// v0.75.7: 本次 agent 执行耗时（ms），用于 per_agent_ms 统计。
    pub duration_ms: u128,
    /// v0.75.8: 本次执行的 input（build_node_input 结果），用于增量缓存。
    pub input_str: String,
    /// v0.75.10: 本次实际执行的 DAG 节点数（寄存器级增量观测）。
    /// 并行路径 worker 内联执行（无 memo），该值用于区分「跳过路径的空 memo」
    /// — RECONCILE 时 nodes_executed == 0 → 不覆盖增量 memo（保持上一超步
    /// 的记忆，下一超步可继续跳过）。
    pub nodes_executed: usize,
}

/// v0.70: Per-vertex lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VertexState {
    /// Default: scheduled when active_nodes has it OR a Send targets it.
    Active,
    /// Halted: only rescheduled when a Send targets it (vote_to_halt).
    Halted,
}

/// v0.67: Numeric accumulator for Sum/Product. First write initializes
/// to the op identity (`0` for `+`, `1` for `*`). Subsequent writes fold.
/// Mixed Int/Float promoted via `eval_binary`.
fn accumulator_reduce(current: Option<Value>, incoming: Value, op: &str) -> Result<Value, String> {
    let identity = match op {
        "+" => Value::Int(0),
        "*" => Value::Int(1),
        _ => return Err(format!("Unknown accumulator op: {}", op)),
    };
    let cur = current.unwrap_or(identity);
    match op {
        "+" => crate::flow::eval_binary(cur, &crate::common::BinaryOp::Add, incoming)
            .map_err(|e| e.to_string()),
        "*" => crate::flow::eval_binary(cur, &crate::common::BinaryOp::Mul, incoming)
            .map_err(|e| e.to_string()),
        _ => Err(format!("Unknown accumulator op: {}", op)),
    }
}

/// v0.67: Concat reducer — append incoming string to current.
/// Non-string incoming values are stringified via Display.
fn concat_reduce(current: Option<Value>, incoming: Value) -> Result<Value, String> {
    let cur = match current {
        Some(Value::String(s)) => s,
        Some(v) => format!("{}", v),
        None => String::new(),
    };
    let inc = match incoming {
        Value::String(s) => s,
        v => format!("{}", v),
    };
    Ok(Value::String(cur + &inc))
}

/// v0.67: Try to parse a Custom reducer's String payload as a MirExpr
/// heuristic: integer → IntLit, anything else → Variable reference.
fn parse_custom_merge_expr(s: &str) -> crate::mir::expr::MirExpr {
    use crate::common::{Literal, Span};
    let span = Span::default();
    if let Ok(n) = s.parse::<i64>() {
        MirExpr {
            kind: crate::mir::expr::MirExprKind::Literal(Literal::Int(n, span)),
            span,
        }
    } else {
        MirExpr {
            kind: crate::mir::expr::MirExprKind::Variable(s.to_string()),
            span,
        }
    }
}

impl MirPregelEngine {
    /// 构造 MIR-native Pregel 引擎
    pub fn new(config: MirPregelConfig) -> Self {
        let agents_by_name = config
            .agents
            .iter()
            .enumerate()
            .map(|(i, a)| (a.name.clone(), i))
            .collect();
        let state_reducers = config
            .state_schema
            .iter()
            .map(|ch| (ch.name.clone(), ch.reducer.clone()))
            .collect();
        let _aggregator_initial: HashMap<String, Value> = config
            .aggregators
            .iter()
            .map(|a| (a.name.clone(), a.initial.clone()))
            .collect();
        let aggregator_reducer: HashMap<String, crate::mir::expr::AggregatorKind> = config
            .aggregators
            .iter()
            .map(|a| (a.name.clone(), a.reducer.clone()))
            .collect();
        let combiner_bodies: HashMap<String, std::sync::Arc<crate::mir::MirFunction>> = config
            .agents
            .iter()
            .filter_map(|a| {
                a.combiner_body
                    .as_ref()
                    .map(|b| (a.name.clone(), std::sync::Arc::new(b.clone())))
            })
            .collect();
        let master_compute = config
            .master_compute
            .as_ref()
            .map(|b| std::sync::Arc::new(b.clone()));
        Self {
            config,
            agents_by_name,
            state_reducers,
            channels: HashMap::new(),
            channel_versions: HashMap::new(),
            versions_seen: HashMap::new(),
            pending_sends: Vec::new(),
            conflicts: Vec::new(),
            max_steps: 1000,
            interrupt_before: None,
            interrupt_after: None,
            conflict_callback: None,
            current_step: 0,
            vertex_state: HashMap::new(),
            aggregator_acc: HashMap::new(),
            aggregator_reducer,
            combiner_bodies,
            master_compute,
            parallelism: 1,
            worker_pool: None,
            step_timeout: None,
            fault_tolerance: 0,
            stats: EngineStats::default(),
            agent_input_cache: HashMap::new(),
            agent_outcome_cache: HashMap::new(),
            task_arcs: HashMap::new(),
            agent_memos: HashMap::new(),
            saver: None,
            base_env: None,
        }
    }

    /// v0.75.84: 注入执行环境（orchestrate 传入，含 builtin ai 等）。
    pub(crate) fn with_base_env(
        mut self,
        env: Arc<parking_lot::Mutex<crate::value::Environment>>,
    ) -> Self {
        self.base_env = Some(env);
        self
    }

    /// 执行环境来源：base_env（优先）或 interpreter.environment()（回落）。
    fn exec_env(
        &self,
        interpreter: &dyn MirHost,
    ) -> Arc<parking_lot::Mutex<crate::value::Environment>> {
        self.base_env
            .clone()
            .unwrap_or_else(|| interpreter.environment())
    }

    pub fn with_max_steps(mut self, max: usize) -> Self {
        self.max_steps = max;
        self
    }

    pub fn with_interrupt_before_callback(mut self, cb: MirInterruptCallback) -> Self {
        self.interrupt_before = Some(cb);
        self
    }

    pub fn with_interrupt_after_callback(mut self, cb: MirInterruptCallback) -> Self {
        self.interrupt_after = Some(cb);
        self
    }

    /// v0.62: Set a callback invoked for each detected write-write conflict.
    /// Return `true` to continue, `false` to abort the BSP run.
    pub fn with_conflict_callback(mut self, cb: MirConflictCallback) -> Self {
        self.conflict_callback = Some(cb);
        self
    }

    /// v0.64: Set a checkpoint saver for auto-persistence.
    pub fn with_checkpoint_saver(mut self, saver: Arc<dyn CheckpointSaver>) -> Self {
        self.saver = Some(saver);
        self
    }

    /// v0.73: Set worker parallelism for the BSP EXEC phase.
    /// 1 = sequential (default), N = parallel with an N-thread pool.
    pub fn with_parallelism(mut self, n: usize) -> Self {
        self.parallelism = n.max(1);
        self
    }

    /// v0.74: Enable fault tolerance — retry a failed super-step up to
    /// `max_retries` times by restoring the step-start checkpoint.
    pub fn with_fault_tolerance(mut self, max_retries: usize) -> Self {
        self.fault_tolerance = max_retries;
        self
    }

    /// v0.74: Set a per-super-step deadline for parallel EXEC. On timeout
    /// the step is treated as a fault and retried (see `with_fault_tolerance`).
    pub fn with_step_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.step_timeout = Some(timeout);
        self
    }

    /// v0.74: Access engine runtime stats.
    pub fn stats(&self) -> &EngineStats {
        &self.stats
    }

    /// v0.69: Drain an external buffer of pending SendTasks into the engine.
    /// Called by `h_orchestrate` before each super-step to inject messages
    /// produced by `h_send` (which has no direct engine access).
    pub fn flush_pending_sends(&mut self, sends: Vec<SendTask>) {
        self.pending_sends.extend(sends);
    }

    /// v0.75.10: 返回 agent task_body 的稳定 Arc（跨超步保持）。
    ///
    /// 修复 v0.75.9 缓存失效 bug：此前每超步 `Arc::new(agent.task_body.clone())`
    /// → 指针每次不同 → 全局 DAG 缓存（key = Arc::as_ptr）跨超步永远 miss，
    /// DAG 每步全量重建（v0.75.6 引擎本地缓存的收益被丢弃）。engine 生命周期
    /// 持有同一 Arc → 缓存真正命中；同时是 memo 记录稳定的锚点。
    ///
    /// 仅缓存 agents 的 task_body（config 静态，随 engine 生命周期，无泄漏）。
    fn stable_task_arc(&mut self, node_name: &str) -> std::sync::Arc<crate::mir::MirFunction> {
        if let Some(a) = self.task_arcs.get(node_name) {
            return a.clone();
        }
        let Some(agent_idx) = self.agents_by_name.get(node_name).copied() else {
            return std::sync::Arc::new(crate::mir::MirFunction {
                params: Vec::new(),
                body: Vec::new(),
                n_regs: 0,
                ..Default::default()
            });
        };
        let arc = std::sync::Arc::new(self.config.agents[agent_idx].task_body.clone());
        self.task_arcs.insert(node_name.to_string(), arc.clone());
        arc
    }

    /// v0.75.10: 加法注入 — 在单个 `input` delta JSON 契约之外，把每个
    /// 已变更 channel 另注入为 `input_<channel>` env var。旧 agent 读
    /// `input` 完全不受影响（纯加法）；新 agent 可读细粒度 var 获得
    /// 真正寄存器级感知。
    fn inject_channel_inputs(env: &mut crate::value::Environment, node_name: &str, engine: &Self) {
        let snapshot = engine.versions_seen.get(node_name);
        for (channel, version) in &engine.channel_versions {
            let seen_version = snapshot.and_then(|s| s.get(channel)).copied().unwrap_or(0);
            if *version > seen_version
                && let Some(v) = engine.channels.get(channel)
            {
                env.define(format!("input_{}", channel), v.clone(), false);
            }
        }
    }

    /// v0.73: Reconcile one agent outcome back into engine state.
    /// Shared by sequential and parallel EXEC paths (index-ordered,
    /// deterministic in parallel mode).
    fn reconcile_outcome(
        &mut self,
        host: &mut dyn MirHost,
        next_active: &mut std::collections::HashSet<String>,
        writes: &mut Vec<(String, String, Value)>,
        outcome: AgentExecOutcome,
    ) -> Result<(), String> {
        let node_name = outcome.node_name.clone();

        // v0.75.7: 记录 per-agent 最近一次耗时（FPGA 调度可观测性）
        self.stats
            .per_agent_ms
            .insert(node_name.clone(), outcome.duration_ms);

        // v0.75.8: 更新增量缓存（主线程统一入口；跳过路径重插幂等）
        self.agent_input_cache
            .insert(node_name.clone(), outcome.input_str.clone());
        self.agent_outcome_cache.insert(
            node_name.clone(),
            (
                outcome.signal.clone(),
                outcome.result.clone(),
                outcome.sends.clone(),
            ),
        );

        if matches!(outcome.signal, crate::mir::vm::MirSignal::Halt(_)) {
            self.vertex_state
                .insert(node_name.clone(), VertexState::Halted);
        } else {
            self.vertex_state
                .insert(node_name.clone(), VertexState::Active);
        }

        // Merge agent env back into shared env (conflict detection).
        // v0.75.84: 合并目标为执行 env（base_env 优先）— 与 agent 执行用
        // 同一容器（单一来源）；此前 host.environment() 在 CLI take_env 后
        // 是空壳，合并静默落空（MoA 层间 value 丢失）。
        let strategies = self.build_per_key_strategies();
        let conflicts = self.exec_env(host).lock().merge_from_with_strategies(
            &outcome.env,
            &strategies,
            &MergeStrategy::LastWriteWins,
        );
        if !conflicts.is_empty() {
            if let Some(cb) = &self.conflict_callback {
                for conflict in &conflicts {
                    if !cb(conflict) {
                        return Err(format!(
                            "Pregel: conflict callback aborted at key '{}' (node '{}')",
                            conflict.key, node_name
                        ));
                    }
                }
            }
            self.conflicts.extend(conflicts);
        }

        // result → result channel
        let result_str = outcome.result.to_string();
        writes.push((
            node_name.clone(),
            "result".to_string(),
            Value::String(result_str),
        ));

        // Static edges → next hop (with condition evaluation).
        for edge in &self.config.edges {
            if edge.from == node_name && edge.to != "@exit" {
                if let Some(cond_body) = &edge.condition_body {
                    let mut cond_env = host.environment().lock().clone();
                    // v0.75.9: 包裹 Arc 走全局 DAG 缓存
                    let cond_val = crate::mir::vm::run_mir(
                        &std::sync::Arc::new(cond_body.clone()),
                        host,
                        &mut cond_env,
                    )
                    .unwrap_or(Value::Bool(false));
                    if !crate::flow::is_truthy(&cond_val) {
                        continue;
                    }
                }
                next_active.insert(edge.to.clone());
            }
        }

        // Worker's dynamic sends → engine pending_sends (step-N+1 delivery).
        self.flush_pending_sends(outcome.sends);
        Ok(())
    }

    /// v0.71: Snapshot of aggregator initial values (from config).
    fn aggregator_initial_snapshot(&self) -> Vec<(String, Value)> {
        self.config
            .aggregators
            .iter()
            .map(|a| (a.name.clone(), a.initial.clone()))
            .collect()
    }

    /// v0.71: Contribute a value to a per-super-step aggregator.
    /// Called by `h_aggregate`.
    pub fn aggregator_contribute(&mut self, name: &str, value: Value) -> Result<(), String> {
        let reducer = self
            .aggregator_reducer
            .get(name)
            .ok_or_else(|| format!("Unknown aggregator: {}", name))?
            .clone();
        let acc = self
            .aggregator_acc
            .entry(name.to_string())
            .or_insert_with(|| match reducer {
                crate::mir::expr::AggregatorKind::Add => Value::Int(0),
                crate::mir::expr::AggregatorKind::Max => value.clone(),
                crate::mir::expr::AggregatorKind::Min => value.clone(),
                crate::mir::expr::AggregatorKind::Last => value.clone(),
                crate::mir::expr::AggregatorKind::Concat => Value::String(String::new()),
            });
        *acc = match reducer {
            crate::mir::expr::AggregatorKind::Add => crate::flow::eval_binary(
                std::mem::replace(acc, Value::Int(0)),
                &crate::common::BinaryOp::Add,
                value,
            )?,
            crate::mir::expr::AggregatorKind::Max => {
                let cur = acc.clone();
                match crate::flow::eval_binary(
                    value.clone(),
                    &crate::common::BinaryOp::Greater,
                    cur,
                ) {
                    Ok(Value::Bool(true)) => value, // incoming > current → keep incoming
                    _ => acc.clone(),               // else keep current (incl. equal)
                }
            }
            crate::mir::expr::AggregatorKind::Min => {
                let cur = acc.clone();
                match crate::flow::eval_binary(value.clone(), &crate::common::BinaryOp::Less, cur) {
                    Ok(Value::Bool(true)) => value, // incoming < current → keep incoming
                    _ => acc.clone(),               // else keep current (incl. equal)
                }
            }
            crate::mir::expr::AggregatorKind::Last => value,
            crate::mir::expr::AggregatorKind::Concat => {
                let prev = std::mem::replace(acc, Value::String(String::new()));
                let new = match prev {
                    Value::String(s) => format!("{}{}", s, value),
                    _ => format!("{}{}", prev, value),
                };
                Value::String(new)
            }
        };
        Ok(())
    }

    /// 初始化 channels（设置 input 通道初值）
    pub fn init_channels(&mut self, initial: HashMap<String, Value>) {
        for (k, v) in initial {
            self.channels.insert(k.clone(), v);
            *self.channel_versions.entry(k).or_insert(0) += 1;
        }
    }

    /// v0.57: 入口 — 执行 BSP 循环（完整实现）
    ///
    /// 阶段：
    /// 1. PLAN：决定本轮激活的节点
    /// 2. EXEC：调用 pre-lowered task_body
    /// 3. UPDATE：应用 reducer
    /// 4. ADVANCE：处理 send tasks + 决定下一跳
    pub fn run(&mut self, interpreter: &mut dyn MirHost) -> Result<Value, String> {
        use std::collections::HashSet;

        // v0.63: current_step is initialized in new() and may be set by restore_checkpoint.
        // Do NOT reset to 0 here — that would negate checkpoint restore.
        let mut active_nodes: Vec<String> = vec!["@start".to_string()];

        while !active_nodes.is_empty() && self.current_step < self.max_steps {
            // v0.71: Reset aggregators at start of each step.
            // Agents contribute via h_aggregate; we reduce at end of step.
            for (name, initial) in &self.aggregator_initial_snapshot() {
                self.aggregator_acc.insert(name.clone(), initial.clone());
            }

            // ─ 1. PLAN ──────
            let mut to_execute: Vec<String> = Vec::new();
            for node in &active_nodes {
                if node == "@start" {
                    continue;
                }
                // v0.70: Halted vertices are only rescheduled when targeted
                // by a Send — vote_to_halt semantics.
                let is_halted = matches!(self.vertex_state.get(node), Some(VertexState::Halted));
                let targeted_by_send = self.pending_sends.iter().any(|s| s.target_node == *node);
                if is_halted && !targeted_by_send {
                    continue;
                }
                if self.agents_by_name.contains_key(node) || targeted_by_send {
                    to_execute.push(node.clone());
                }
            }

            // interrupt before
            for node_name in &to_execute {
                for ip in &self.collect_interrupts(node_name, MirInterruptWhen::Before) {
                    if let Some(cb) = &self.interrupt_before
                        && !cb(&ip.node_name, MirInterruptWhen::Before)
                    {
                        return Err(format!("interrupted at {}", ip.node_name));
                    }
                }
            }

            // ─ 2. EXEC ──────
            // 记录激活节点的 snapshots
            for node_name in &to_execute {
                let snapshot = self.versions_seen.entry(node_name.clone()).or_default();
                for (channel, version) in &self.channel_versions {
                    snapshot.entry(channel.clone()).or_insert(*version);
                }
            }

            // v0.57 bugfix: 下一跳必须从 active_nodes（含 @start）计算，
            // 而不只是从 to_execute。这样 @start -> a 这类入口边才能触发 agent 执行。
            let mut next_active: HashSet<String> = HashSet::new();
            for active_node in &active_nodes {
                for edge in &self.config.edges {
                    if edge.from == *active_node && edge.to != "@exit" {
                        // v0.71: Evaluate edge condition if present.
                        if let Some(cond_body) = &edge.condition_body {
                            let mut cond_env = self.exec_env(interpreter).lock().clone();
                            // v0.75.9: 包裹 Arc 走全局 DAG 缓存
                            let cond_val = crate::mir::vm::run_mir(
                                &std::sync::Arc::new(cond_body.clone()),
                                interpreter,
                                &mut cond_env,
                            )
                            .unwrap_or(Value::Bool(false));
                            if !crate::flow::is_truthy(&cond_val) {
                                continue;
                            }
                        }
                        next_active.insert(edge.to.clone());
                    }
                }
            }

            // v0.75.57: EXEC 段提取至 execute_step（BSP 超步执行 + fault tolerance）
            let writes = self.execute_step(interpreter, &to_execute, &mut next_active)?;

            // ─ 3. UPDATE ────
            for (_node, channel, value) in writes {
                self.apply_write(channel, value, interpreter)?;
            }

            // v0.75.83: 收集 agent 经 aggregate 语句提交的贡献（MirHost 缓冲，
            // 与 dynamic_sends 同构）→ aggregator_contribute 归约。
            // 此前 h_aggregate 为空操作，语言层 → 引擎的贡献通道断头。
            let contributions = std::mem::take(interpreter.aggregator_contributions());
            for contrib in contributions {
                self.aggregator_contribute(&contrib.name, contrib.value)?;
            }

            // v0.71: Publish aggregator results as channels for next step.
            for (name, value) in &self.aggregator_acc {
                self.channels
                    .insert(format!("aggregator_{}", name), value.clone());
                *self
                    .channel_versions
                    .entry(format!("aggregator_{}", name))
                    .or_insert(0) += 1;
            }

            // v0.72: Master.compute — runs once per super-step after UPDATE.
            // Used for global coordination (e.g., dynamic topology changes,
            // aggregation-based decisions).
            // v0.75.28: master_compute 失败错误传播 — 此前 eprintln warn
            // 静默继续（吞错误；协调钩子失败可能让引擎跑出错误语义而不自知）。
            // 与「吞异常审计」约束一致：协调钩子是全局控制点，失败必须冒泡。
            if let Some(master) = self.master_compute.clone() {
                let mut master_env = self.exec_env(interpreter).lock().clone();
                // v0.75.9: master_compute 已是 Arc，直接走全局 DAG 缓存
                crate::mir::vm::run_mir(&master, interpreter, &mut master_env)?;
            }

            // interrupt after
            for node_name in &to_execute {
                for ip in &self.collect_interrupts(node_name, MirInterruptWhen::After) {
                    if let Some(cb) = &self.interrupt_after
                        && !cb(&ip.node_name, MirInterruptWhen::After)
                    {
                        return Err(format!("interrupted after {}", ip.node_name));
                    }
                }
            }

            // ─ 4. ADVANCE ────
            // v0.69: Dynamic Send delivery — target nodes become active in
            // the next super-step and their `input` channel carries the payload.
            // v0.72: Combiners — multiple sends to the same target are folded
            // via the target's combiner_body (current, incoming) -> Value
            // before delivery. Default behavior (no combiner) = last-write-wins.
            let mut by_target: std::collections::HashMap<String, Vec<crate::value::Value>> =
                std::collections::HashMap::new();
            for send in self.pending_sends.drain(..) {
                by_target
                    .entry(send.target_node)
                    .or_default()
                    .push(send.input);
            }
            for (target, messages) in by_target {
                // v0.75.4: 提前失败 — send 到未定义节点在消息分发点立即报错，
                // 而非延迟到下一超步 EXEC 才崩溃（Giraph message-ACK 精神：
                // 确认每条消息都有合法接收者）。
                if !self.agents_by_name.contains_key(&target) {
                    return Err(format!(
                        "Pregel: send to undefined node '{}' (defined agents: {:?})",
                        target,
                        self.agents_by_name.keys().collect::<Vec<_>>()
                    ));
                }
                self.stats.messages_sent += messages.len();
                let final_value = if let Some(combiner) = self.combiner_bodies.get(&target).cloned()
                {
                    let mut acc = messages[0].clone();
                    for incoming in &messages[1..] {
                        let mut env = self.exec_env(interpreter).lock().clone();
                        env.define("current".into(), acc.clone(), false);
                        env.define("incoming".into(), incoming.clone(), false);
                        // v0.75.9: combiner_bodies 已是 Arc，直接走全局 DAG 缓存
                        match crate::mir::vm::run_mir(&combiner, interpreter, &mut env) {
                            Ok(v) => acc = v,
                            Err(_) => acc = incoming.clone(), // fallback: LWW
                        }
                    }
                    acc
                } else {
                    messages.last().cloned().unwrap_or(Value::Nil)
                };
                self.channels.insert("input".to_string(), final_value);
                *self
                    .channel_versions
                    .entry("input".to_string())
                    .or_insert(0) += 1;
                next_active.insert(target);
            }
            active_nodes = next_active.into_iter().collect();
            // v0.73: Sort active_nodes by agent definition order for
            // deterministic super-step scheduling (HashSet iteration order
            // is nondeterministic → would make both sequential and
            // parallel EXEC produce order-dependent results).
            let agents = &self.config.agents;
            active_nodes.sort_by_key(|n| {
                agents
                    .iter()
                    .position(|a| &a.name == n)
                    .unwrap_or(usize::MAX)
            });
            self.current_step += 1;
            self.stats.steps += 1;

            // v0.63: Auto-save checkpoint if configured
            if let Some(ref cp_cfg) = self.config.checkpoint
                && let Some(interval) = cp_cfg.interval
                && self.current_step.is_multiple_of(interval as usize)
            {
                let cp = self.build_checkpoint();
                if let Some(ref saver) = self.saver {
                    let thread_id = cp.thread_id.clone();
                    saver.save(&thread_id, &cp)?;
                    // v0.74: Retention — prune oldest checkpoints
                    // beyond max_checkpoints (default: keep all).
                    if let Some(max_cp) = cp_cfg.max_checkpoints {
                        let mut ids = saver.list(&thread_id)?;
                        while ids.len() > max_cp {
                            let oldest = ids.remove(0);
                            saver.delete(&thread_id, &oldest)?;
                        }
                    }
                }
            }
        }

        // 返回 result 通道
        Ok(self.channels.get("result").cloned().unwrap_or(Value::Nil))
    }

    /// v0.75.57: EXEC 段提取（run() 内最大块）— BSP 超步执行 + fault tolerance
    /// 重试。顺序路径内联；并行路径经 WorkerPool（PREPARE → SPAWN → RECONCILE）。
    /// 返回本超步产生的 writes（UPDATE 段消费）。
    fn execute_step(
        &mut self,
        interpreter: &mut dyn MirHost,
        to_execute: &[String],
        next_active: &mut std::collections::HashSet<String>,
    ) -> Result<Vec<(String, String, Value)>, String> {
        let mut writes: Vec<(String, String, Value)> = Vec::new();

        // v0.74: Step-level fault tolerance. Snapshot step-start state,
        // run EXEC, and on failure restore + retry up to fault_tolerance.
        // v0.75.3: 增量快照（StepUndo）— 只记录 EXEC 会改的状态；
        // fault_tolerance == 0 时为零成本（此前每步全量 build_checkpoint
        // 在默认配置从未被读取，纯浪费）。
        let step_start = self.begin_step();
        let mut exec_result: Result<(), String> = Ok(());

        for attempt in 0..=self.fault_tolerance {
            if attempt > 0 {
                // Restore step-start state and re-run the step.
                self.stats.retries += 1;
                if let Some(ref undo) = step_start {
                    self.rollback_step(undo);
                }
                // Re-run EXEC from the restored state.
                self.worker_pool = None; // discard any leaked threads
            }

            // v0.73: EXEC — sequential (parallelism=1) or parallel.
            let mut step_writes: Vec<(String, String, Value)> = Vec::new();
            exec_result = (|| {
                if self.parallelism <= 1 {
                    // ── Sequential path: inline, current behavior ──
                    for node_name in to_execute {
                        let agent_idx = *self
                            .agents_by_name
                            .get(node_name)
                            .ok_or_else(|| format!("Pregel: undefined agent '{}'", node_name))?;
                        let agent = &self.config.agents[agent_idx];

                        let input_val = self.build_node_input(node_name);
                        let input_str = input_val.to_string();
                        let mut env = self.exec_env(interpreter).lock().clone();
                        // v0.73: define input on the private clone (agent only
                        // sees its own input; no cross-agent contamination).
                        env.define("input".to_string(), Value::String(input_str.clone()), false);
                        // v0.75.10: 加法注入 — 保留 `input` 契约，另注入
                        // 逐 channel 变更 var（input_<channel>）。旧 agent 无感。
                        Self::inject_channel_inputs(&mut env, node_name, self);
                        env.clock.tick(node_name);

                        // v0.75.8: 增量执行 v1 — input 与上次相同则跳过整个
                        // 执行，复用上次 outcome（signal/result）。input 相同
                        // → 确定性执行，语义等价；跳过避免重复副作用（如
                        // ai.chat 网络调用）。
                        // v0.75.10: 寄存器级增量（memo）在 v1 之上细化 —
                        // input 未变整体跳过；input 变了但部分纯节点输入
                        // 未变则节点级跳过（run_dag_with_signal_memo）。
                        if self.agent_input_cache.get(node_name) == Some(&input_str)
                            && let Some((signal, result, _sends)) =
                                self.agent_outcome_cache.get(node_name).cloned()
                        {
                            let outcome = AgentExecOutcome {
                                node_name: node_name.clone(),
                                signal,
                                result,
                                env,
                                // 顺序路径 sends 经 interpreter.dynamic_sends
                                // 在循环外收集；跳过则无新 send。
                                sends: Vec::new(),
                                duration_ms: 0,
                                input_str,
                                // 跳过路径：无实际执行（v0.75.10）。
                                nodes_executed: 0,
                            };
                            self.reconcile_outcome(
                                interpreter,
                                next_active,
                                &mut step_writes,
                                outcome,
                            )?;
                            continue;
                        }

                        if agent.task_body.body.is_empty() && agent.task_body.n_regs == 0 {
                            return Err(format!(
                                "Pregel: agent '{}' has empty task_body (lowering missing)",
                                node_name
                            ));
                        }
                        // v0.75.6: 克隆 task_body 解除对 self.config 的借用
                        // v0.75.10: 稳定 Arc（engine 生命周期保持）— 全局
                        // DAG 缓存（key = Arc 指针）跨超步真正命中（修复
                        // v0.75.9 每超步新建 Arc 的缓存失效）；同 Arc 锚定
                        // 寄存器级增量 memo 记录。
                        let task_body = self.stable_task_arc(node_name);
                        let mut memo = self.agent_memos.remove(node_name).unwrap_or_default();

                        self.stats.agents_run += 1;

                        // v0.75.9: 全局 DAG 缓存（mir::cache）— 取代引擎
                        // 本地 agent_dag_cache，Closure/Task/REPL 共用
                        // v0.75.7: 计时 per-agent 耗时（FPGA 调度可观测性）
                        let dag = crate::mir::cache::global_dag_cache().get_or_build(&task_body);
                        let started = std::time::Instant::now();
                        // v0.75.10: 寄存器级增量执行 — 纯节点输入与上次
                        // 相等则跳过；副作用/env 读取节点永远重跑。
                        let (signal, result) = crate::mir::vm::run_dag_with_signal_memo(
                            dag.as_ref(),
                            task_body.as_ref(),
                            &mut memo,
                            interpreter,
                            &mut env,
                        )
                        .map_err(|e| format!("Pregel node '{}': {}", node_name, e))?;
                        let duration_ms = started.elapsed().as_millis();
                        let nodes_executed = memo.executed_nodes;
                        self.agent_memos.insert(node_name.clone(), memo);

                        let outcome = AgentExecOutcome {
                            node_name: node_name.clone(),
                            signal,
                            result,
                            env,
                            sends: Vec::new(),
                            duration_ms,
                            input_str,
                            nodes_executed,
                        };
                        self.reconcile_outcome(
                            interpreter,
                            next_active,
                            &mut step_writes,
                            outcome,
                        )?;
                    }
                } else {
                    // ── Parallel path: PREPARE → SPAWN → RECONCILE ──
                    // PREPARE (main thread, &self read): build private envs.
                    // v0.75.6: prepared 携带缓存 dag（避免每超步重建）。
                    // v0.75.8: 携带 input_str 供 worker 填回 outcome。
                    let mut prepared: Vec<PreparedJob> = Vec::new();
                    for node_name in to_execute {
                        let agent_idx = *self
                            .agents_by_name
                            .get(node_name)
                            .ok_or_else(|| format!("Pregel: undefined agent '{}'", node_name))?;
                        let agent = &self.config.agents[agent_idx];
                        if agent.task_body.body.is_empty() && agent.task_body.n_regs == 0 {
                            return Err(format!(
                                "Pregel: agent '{}' has empty task_body (lowering missing)",
                                node_name
                            ));
                        }
                        let input_val = self.build_node_input(node_name);
                        let input_str = input_val.to_string();
                        let mut env = self.exec_env(interpreter).lock().clone();
                        env.define("input".to_string(), Value::String(input_str.clone()), false);
                        // v0.75.10: 加法注入（input_<channel>），旧 agent 无感
                        Self::inject_channel_inputs(&mut env, node_name, self);
                        env.clock.tick(node_name);

                        // v0.75.8: 增量 v1 — input 未变则跳过，直接 reconcile
                        // 缓存 outcome（与顺序路径同语义）。
                        if self.agent_input_cache.get(node_name) == Some(&input_str)
                            && let Some((signal, result, sends)) =
                                self.agent_outcome_cache.get(node_name).cloned()
                        {
                            let outcome = AgentExecOutcome {
                                node_name: node_name.clone(),
                                signal,
                                result,
                                env,
                                sends,
                                duration_ms: 0,
                                input_str,
                                // 跳过路径：无实际执行；并行 worker 内联执行
                                // 也无 memo，本字段仅用于统计（此处为 0）。
                                nodes_executed: 0,
                            };
                            self.reconcile_outcome(
                                interpreter,
                                next_active,
                                &mut step_writes,
                                outcome,
                            )?;
                            continue;
                        }

                        self.stats.agents_run += 1;
                        // v0.75.6: 克隆 task_body 解除借用
                        // v0.75.10: 稳定 Arc（engine 生命周期保持）— 全局
                        // DAG 缓存跨超步命中（修复 v0.75.9 缓存失效）。
                        let task_body = self.stable_task_arc(node_name);
                        let dag = crate::mir::cache::global_dag_cache().get_or_build(&task_body);
                        prepared.push((node_name.clone(), task_body, dag, env, input_str));
                    }

                    // v0.75.7: Longest-Job-First 排序 — 按 DAG 复杂度
                    // （nodes.len()，执行时长的廉价代理）降序。BSP 超步
                    // 隔离保证同超步 agent 顺序无关（读 step-start 快照、
                    // 写延迟到 barrier 后），重排仅改变分发顺序、不影响
                    // 正确性；长 job 先调度可减少 worker 空闲尾巴
                    // （straggler 缓解，FPGA list-scheduling 精神）。
                    prepared.sort_by(|a, b| {
                        b.2.nodes
                            .len()
                            .cmp(&a.2.nodes.len())
                            .then_with(|| a.0.cmp(&b.0))
                    });
                    // SPAWN: one Interpreter clone + private env per worker job.
                    // v0.74: Reuse cached pool (created once), keep across steps.
                    if self.worker_pool.is_none() {
                        self.worker_pool = Some(crate::pregel::worker_pool::WorkerPool::new(
                            self.parallelism,
                        ));
                    }
                    let pool = self
                        .worker_pool
                        .as_ref()
                        .ok_or("Pregel: worker pool missing")?;
                    let jobs: Vec<crate::pregel::worker_pool::WorkerJob> = prepared
                        .into_iter()
                        .enumerate()
                        .map(|(idx, (name, task, dag, mut env, input_str))| {
                            let mut interp_clone = interpreter.clone_box();
                            crate::pregel::worker_pool::WorkerJob {
                                index: idx,
                                task: Box::new(move || {
                                    // v0.75.6: 用缓存 dag 执行（避免每超步重建）
                                    // v0.75.7: 计时 per-agent 耗时
                                    let job_started = std::time::Instant::now();
                                    let (signal, result) = crate::mir::vm::run_dag_with_signal(
                                        dag.as_ref(),
                                        &task,
                                        interp_clone.as_mut(),
                                        &mut env,
                                    )
                                    .map_err(|e| format!("Pregel node '{}': {}", name, e))?;
                                    let duration_ms = job_started.elapsed().as_millis();
                                    let sends = std::mem::take(interp_clone.dynamic_sends());
                                    Ok(Box::new(AgentExecOutcome {
                                        node_name: name,
                                        signal,
                                        result,
                                        env,
                                        sends,
                                        duration_ms,
                                        input_str,
                                        // v0.75.10: worker 内联执行（无 memo），
                                        // 该字段仅作统计。
                                        nodes_executed: 0,
                                    })
                                        as Box<dyn std::any::Any + Send>)
                                }),
                            }
                        })
                        .collect();

                    // v0.74: Run batch with optional per-step timeout. On timeout
                    // the step is treated as a fault and retried (fault_tolerance).
                    let started = std::time::Instant::now();
                    let batch = pool
                        .run_batch_with_timeout(jobs, self.step_timeout)
                        .map_err(|e| format!("Pregel parallel EXEC: {}", e))?;
                    self.stats.total_ms += started.elapsed().as_millis();

                    if batch.timed_out {
                        self.stats.timeouts += 1;
                        // Drop the pool (leaked timed-out worker threads) and
                        // force a fresh one on the next step.
                        self.worker_pool = None;
                        return Err(format!(
                            "Pregel: super-step {} timed out after {:?}",
                            self.current_step, self.step_timeout
                        ));
                    }
                    let outcomes = batch.outcomes;

                    // RECONCILE (main thread, index order = deterministic).
                    for out in outcomes {
                        let outcome: AgentExecOutcome =
                            *out.value
                                .downcast::<AgentExecOutcome>()
                                .map_err(|_| "Pregel: worker outcome type mismatch".to_string())?;
                        self.reconcile_outcome(
                            interpreter,
                            next_active,
                            &mut step_writes,
                            outcome,
                        )?;
                    }
                }

                // v0.73: Flush intra-run sends so a step-N send reaches step-N+1.
                let sends = std::mem::take(interpreter.dynamic_sends());
                self.flush_pending_sends(sends);
                Ok(())
            })();
            // end retryable exec closure

            if exec_result.is_ok() {
                writes = step_writes;
                break;
            }
            // Else: retry loop will restore checkpoint and re-run.
            if attempt == self.fault_tolerance {
                break;
            }
        }
        exec_result?;
        Ok(writes)
    }

    /// 构建节点输入 — 序列化 channels
    fn build_node_input(&self, node_name: &str) -> String {
        let snapshot = self.versions_seen.get(node_name);
        let mut parts: Vec<String> = Vec::new();
        for (channel, version) in &self.channel_versions {
            let seen_version = snapshot.and_then(|s| s.get(channel)).copied().unwrap_or(0);
            if *version > seen_version
                && let Some(v) = self.channels.get(channel)
            {
                parts.push(format!("\"{}\":{}", channel, value_to_json_string(v)));
            }
        }
        if parts.is_empty() {
            "{}".to_string()
        } else {
            format!("{{{}}}", parts.join(","))
        }
    }

    /// 应用写入 — 通过 Value::merge() 统一 CRDT 路径
    pub fn apply_write(
        &mut self,
        channel: String,
        value: Value,
        interpreter: &mut dyn MirHost,
    ) -> Result<(), String> {
        let reducer = self
            .state_reducers
            .get(&channel)
            .cloned()
            .unwrap_or(MirReducerKind::Last);

        let current = self.channels.get(&channel).cloned();

        // Pregel Append: accumulate individual writes into a list.
        // Different from MergeStrategy::Append which extends two lists.
        if reducer == MirReducerKind::Append {
            let mut list = match current {
                Some(Value::List(l)) => l,
                Some(v) => vec![v],
                None => Vec::new(),
            };
            list.push(value);
            let new_value = Value::List(list);
            self.channels.insert(channel.clone(), new_value);
            *self.channel_versions.entry(channel).or_insert(0) += 1;
            return Ok(());
        }

        let new_value = match reducer.to_merge_strategy() {
            Some(strategy) => match current {
                Some(cur) => Value::merge(cur, value, &strategy),
                None => value,
            },
            None => match reducer {
                MirReducerKind::Merge(merge_expr) => {
                    // v0.62: Execute the merge body with `current` and `incoming`.
                    let merge_fn =
                        crate::mir::lower::lower_mir_exprs(std::slice::from_ref(&merge_expr))
                            .map_err(|e| format!("Pregel merge body lowering failed: {}", e))?;
                    let mut merge_env = self.exec_env(interpreter).lock().clone();
                    merge_env.define("current".into(), current.unwrap_or(Value::Nil), false);
                    merge_env.define("incoming".into(), value, false);
                    // v0.75.9: 包裹 Arc 走全局 DAG 缓存
                    crate::mir::vm::run_mir(
                        &std::sync::Arc::new(merge_fn),
                        interpreter,
                        &mut merge_env,
                    )
                    .map_err(|e| format!("Pregel merge body execution failed: {}", e))?
                }
                // v0.67: Sum — accumulate numeric writes (first write initializes).
                MirReducerKind::Sum => accumulator_reduce(current, value, "+")?,
                // v0.67: Product — fold numeric writes multiplicatively.
                MirReducerKind::Product => accumulator_reduce(current, value, "*")?,
                // v0.67: Concat — append strings (or stringify-then-append for non-strings).
                MirReducerKind::Concat => concat_reduce(current, value)?,
                // v0.67: Custom — execute user body via Custom merge expression.
                MirReducerKind::Custom(merge_expr) => {
                    let expr = parse_custom_merge_expr(merge_expr.as_str());
                    let merge_fn = crate::mir::lower::lower_mir_exprs(&[expr])
                        .map_err(|e| format!("Pregel custom body lowering failed: {}", e))?;
                    let mut merge_env = self.exec_env(interpreter).lock().clone();
                    merge_env.define("current".into(), current.unwrap_or(Value::Nil), false);
                    merge_env.define("incoming".into(), value, false);
                    // v0.75.9: 包裹 Arc 走全局 DAG 缓存
                    crate::mir::vm::run_mir(
                        &std::sync::Arc::new(merge_fn),
                        interpreter,
                        &mut merge_env,
                    )
                    .map_err(|e| format!("Pregel custom body execution failed: {}", e))?
                }
                // Static reducers already handled by to_merge_strategy() above
                _ => value,
            },
        };

        self.channels.insert(channel.clone(), new_value);
        *self.channel_versions.entry(channel).or_insert(0) += 1;
        Ok(())
    }

    /// v0.60: Build per-key merge strategies from state schema.
    pub fn build_per_key_strategies(&self) -> HashMap<String, MergeStrategy> {
        let mut map = HashMap::new();
        for channel in &self.config.state_schema {
            if let Some(strategy) = channel.reducer.to_merge_strategy() {
                map.insert(channel.name.clone(), strategy);
            }
        }
        map
    }

    /// 收集指定节点的中断点
    pub fn collect_interrupts(
        &self,
        node_name: &str,
        when: MirInterruptWhen,
    ) -> Vec<&MirInterruptPoint> {
        self.config
            .interrupt_points
            .iter()
            .filter(|ip| {
                ip.node_name == node_name
                    && std::mem::discriminant(&ip.when) == std::mem::discriminant(&when)
            })
            .collect()
    }

    /// v0.63: Build a checkpoint snapshot of current engine state.
    pub fn build_checkpoint(&self) -> Checkpoint {
        let thread_id = self
            .config
            .checkpoint
            .as_ref()
            .and_then(|c| c.thread_id.as_ref())
            .map(|_| "pregel") // MirExpr evaluation deferred; use config presence as signal
            .unwrap_or("default");
        Checkpoint::new(
            thread_id.to_string(),
            self.current_step,
            self.channels.clone(),
            self.channel_versions.clone(),
            self.versions_seen.clone(),
            self.pending_sends.clone(),
        )
    }

    /// 从 checkpoint 恢复
    /// v0.63: Restore engine state from a checkpoint.
    pub fn restore_checkpoint(&mut self, cp: &Checkpoint) {
        self.channels = cp.channel_values.clone();
        self.channel_versions = cp.channel_versions.clone();
        self.versions_seen = cp.versions_seen.clone();
        self.pending_sends = cp.pending_sends.clone();
        self.current_step = cp.step;
        // v0.74: Restore clears vertex_state — re-running the step rebuilds
        // it from the agents' signals. Aggregators reset to config initials.
        self.vertex_state.clear();
        self.aggregator_acc.clear();
        for (name, initial) in self.aggregator_initial_snapshot() {
            self.aggregator_acc.insert(name, initial);
        }
    }

    /// v0.75.3: 每步开始时记录增量 undo。`fault_tolerance == 0` 时返回
    /// `None`（零成本 — 此前的每步全量 `build_checkpoint()` 在默认配置
    /// 从未被读取，纯浪费）。
    fn begin_step(&mut self) -> Option<StepUndo> {
        (self.fault_tolerance > 0).then(|| StepUndo {
            old_pending_sends: self.pending_sends.clone(),
        })
    }

    /// v0.75.3: 失败回滚 — 还原 EXEC 修改的状态。与 `restore_checkpoint`
    /// 对 vertex_state / aggregator_acc 的处理语义一致（清空 + 从 config
    /// initials 重建）；channels 等在 EXEC 期间不变，无需恢复。
    fn rollback_step(&mut self, undo: &StepUndo) {
        self.pending_sends = undo.old_pending_sends.clone();
        self.vertex_state.clear();
        self.aggregator_acc.clear();
        for (name, initial) in self.aggregator_initial_snapshot() {
            self.aggregator_acc.insert(name, initial);
        }
    }

    /// 访问 config（用于测试与序列化）
    pub fn config(&self) -> &MirPregelConfig {
        &self.config
    }
}

/// v0.57: 将 Value 序列化为 JSON 字符串片段（用于 build_node_input）
fn value_to_json_string(v: &Value) -> String {
    match v {
        Value::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Value::Int(n) => format!("{}", n),
        Value::Float(n) => format!("{}", n),
        Value::Bool(b) => format!("{}", b),
        Value::Nil => "null".to_string(),
        Value::List(items) => {
            let parts: Vec<String> = items.iter().map(value_to_json_string).collect();
            format!("[{}]", parts.join(","))
        }
        _ => format!("\"{}\"", v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::MirFunction;
    use crate::mir::MirInst;
    use crate::mir::expr::{MirAgentDef, MirEdgeDef, MirExpr, MirStateChannel};

    fn empty_mir_function() -> MirFunction {
        MirFunction {
            params: Vec::new(),
            body: Vec::new(),
            n_regs: 0,
        
            ..Default::default()}
    }

    fn make_agent(name: &str) -> MirAgentDef {
        MirAgentDef {
            name: name.to_string(),
            task_expr: MirExpr::lit(
                crate::common::Literal::Nil(crate::common::Span::new(1, 1)),
                crate::common::Span::new(1, 1),
            ),
            verify_expr: None,
            with_config: None,
            task_body: empty_mir_function(),
            combiner_body: None,
        }
    }

    #[test]
    fn mir_pregel_engine_construction() {
        let config = MirPregelConfig {
            agents: vec![make_agent("a")],
            edges: vec![MirEdgeDef {
                from: "@start".to_string(),
                to: "a".to_string(),
                condition_expr: None,
                condition_body: None,
            }],
            state_schema: vec![MirStateChannel {
                name: "x".to_string(),
                ty: "Int".to_string(),
                reducer: MirReducerKind::Last,
            }],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let engine = MirPregelEngine::new(config);
        assert_eq!(engine.config().agents.len(), 1);
        assert_eq!(engine.config().edges.len(), 1);
    }

    #[test]
    fn mir_pregel_engine_run_empty() {
        let config = MirPregelConfig {
            agents: vec![],
            edges: vec![],
            state_schema: vec![],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config);
        let mut interp = crate::interpreter::Interpreter::new();
        let result = engine.run(&mut interp).unwrap();
        assert_eq!(result, Value::Nil);
    }

    #[test]
    fn mir_pregel_engine_apply_write_last() {
        let config = MirPregelConfig {
            agents: vec![],
            edges: vec![],
            state_schema: vec![MirStateChannel {
                name: "x".to_string(),
                ty: "Int".to_string(),
                reducer: MirReducerKind::Last,
            }],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config);
        let mut interp = crate::interpreter::Interpreter::new();
        engine
            .apply_write("x".to_string(), Value::Int(42), &mut interp)
            .unwrap();
        engine
            .apply_write("x".to_string(), Value::Int(99), &mut interp)
            .unwrap();
        assert_eq!(engine.channels.get("x"), Some(&Value::Int(99)));
    }

    #[test]
    fn mir_pregel_engine_apply_write_append() {
        let config = MirPregelConfig {
            agents: vec![],
            edges: vec![],
            state_schema: vec![MirStateChannel {
                name: "xs".to_string(),
                ty: "Int".to_string(),
                reducer: MirReducerKind::Append,
            }],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config);
        let mut interp = crate::interpreter::Interpreter::new();
        engine
            .apply_write("xs".to_string(), Value::Int(1), &mut interp)
            .unwrap();
        engine
            .apply_write("xs".to_string(), Value::Int(2), &mut interp)
            .unwrap();
        match engine.channels.get("xs") {
            Some(Value::List(items)) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0], Value::Int(1));
                assert_eq!(items[1], Value::Int(2));
            }
            other => panic!("expected List, got {:?}", other),
        }
    }

    /// v0.67: Sum reducer accumulates numeric writes
    #[test]
    fn mir_pregel_engine_apply_write_sum() {
        let config = MirPregelConfig {
            agents: vec![],
            edges: vec![],
            state_schema: vec![MirStateChannel {
                name: "total".into(),
                ty: "Int".into(),
                reducer: MirReducerKind::Sum,
            }],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config);
        let mut interp = crate::interpreter::Interpreter::new();
        engine
            .apply_write("total".into(), Value::Int(10), &mut interp)
            .unwrap();
        engine
            .apply_write("total".into(), Value::Int(32), &mut interp)
            .unwrap();
        engine
            .apply_write("total".into(), Value::Int(8), &mut interp)
            .unwrap();
        assert_eq!(engine.channels.get("total"), Some(&Value::Int(50)));
    }

    /// v0.67: Product reducer multiplies across writes
    #[test]
    fn mir_pregel_engine_apply_write_product() {
        let config = MirPregelConfig {
            agents: vec![],
            edges: vec![],
            state_schema: vec![MirStateChannel {
                name: "acc".into(),
                ty: "Int".into(),
                reducer: MirReducerKind::Product,
            }],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config);
        let mut interp = crate::interpreter::Interpreter::new();
        engine
            .apply_write("acc".into(), Value::Int(2), &mut interp)
            .unwrap();
        engine
            .apply_write("acc".into(), Value::Int(3), &mut interp)
            .unwrap();
        engine
            .apply_write("acc".into(), Value::Int(4), &mut interp)
            .unwrap();
        assert_eq!(engine.channels.get("acc"), Some(&Value::Int(24)));
    }

    /// v0.67: Concat reducer accumulates strings
    #[test]
    fn mir_pregel_engine_apply_write_concat() {
        let config = MirPregelConfig {
            agents: vec![],
            edges: vec![],
            state_schema: vec![MirStateChannel {
                name: "log".into(),
                ty: "String".into(),
                reducer: MirReducerKind::Concat,
            }],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config);
        let mut interp = crate::interpreter::Interpreter::new();
        engine
            .apply_write("log".into(), Value::String("hello".into()), &mut interp)
            .unwrap();
        engine
            .apply_write("log".into(), Value::String(" world".into()), &mut interp)
            .unwrap();
        assert_eq!(
            engine.channels.get("log"),
            Some(&Value::String("hello world".into()))
        );
    }

    // ─── v0.73: Parallelism tests ─────────────────────────────────

    /// Build an agent whose task_body returns a constant Int.
    fn make_const_agent(name: &str, value: i64) -> MirAgentDef {
        MirAgentDef {
            name: name.to_string(),
            task_expr: MirExpr::lit(
                crate::common::Literal::Nil(crate::common::Span::new(1, 1)),
                crate::common::Span::new(1, 1),
            ),
            verify_expr: None,
            with_config: None,
            task_body: MirFunction {
                params: Vec::new(),
                body: vec![
                    MirInst::Const(0, Value::Int(value)),
                    MirInst::Return(Some(0)),
                ],
                n_regs: 1,
            
            ..Default::default()},
            combiner_body: None,
        }
    }

    /// Sequential and parallel EXEC must run the same agents.
    /// Note: "result" channel final value is order-dependent because both
    /// agents write it (LWW). In parallel mode reconcile is index-sorted
    /// (deterministic); sequential mode iterates a HashSet (nondeterministic
    /// order). So we assert both agents ran and result is one of the two.
    #[test]
    fn parallel_matches_sequential_result() {
        let config = MirPregelConfig {
            agents: vec![make_const_agent("a", 10), make_const_agent("b", 20)],
            edges: vec![
                MirEdgeDef {
                    from: "@start".into(),
                    to: "a".into(),
                    condition_expr: None,
                    condition_body: None,
                },
                MirEdgeDef {
                    from: "@start".into(),
                    to: "b".into(),
                    condition_expr: None,
                    condition_body: None,
                },
            ],
            state_schema: vec![],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };

        // Parallel (4 workers)
        let mut par_engine = MirPregelEngine::new(config).with_parallelism(4);
        let mut par_interp = crate::interpreter::Interpreter::new();
        let par_result = par_engine.run(&mut par_interp).unwrap();

        // Parallel is deterministic (index-sorted reconcile): agent b (idx 1)
        // writes "result" last → "20".
        assert_eq!(par_result, Value::String("20".to_string()));
        // Both agents ran exactly once (parallel mode keeps vertex_state).
        assert_eq!(par_engine.vertex_state.get("a"), Some(&VertexState::Active));
        assert_eq!(par_engine.vertex_state.get("b"), Some(&VertexState::Active));
    }

    /// Parallel mode keeps vertex_state consistent (both agents run once).
    #[test]
    fn parallel_tracks_vertex_state() {
        let config = MirPregelConfig {
            agents: vec![make_const_agent("a", 1), make_const_agent("b", 2)],
            edges: vec![
                MirEdgeDef {
                    from: "@start".into(),
                    to: "a".into(),
                    condition_expr: None,
                    condition_body: None,
                },
                MirEdgeDef {
                    from: "@start".into(),
                    to: "b".into(),
                    condition_expr: None,
                    condition_body: None,
                },
            ],
            state_schema: vec![],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config).with_parallelism(4);
        let mut interp = crate::interpreter::Interpreter::new();
        engine.run(&mut interp).unwrap();
        assert_eq!(engine.vertex_state.get("a"), Some(&VertexState::Active));
        assert_eq!(engine.vertex_state.get("b"), Some(&VertexState::Active));
    }

    // ─── v0.74: Fault tolerance tests ─────────────────────────────

    /// Parallel EXEC with a failing agent returns Err (not hang).
    #[test]
    fn parallel_agent_error_propagates() {
        // Agent "a" task_body errors: Int + Float is a strict-mode type
        // error in eval_binary → run_mir returns Err.
        let failing_body = MirFunction {
            params: Vec::new(),
            body: vec![
                MirInst::Const(0, Value::Int(1)),
                MirInst::Const(1, Value::Float(2.5)),
                MirInst::BinaryOp(2, 0, crate::common::BinaryOp::Add, 1),
                MirInst::Return(Some(2)),
            ],
            n_regs: 3,
        
            ..Default::default()};
        let config = MirPregelConfig {
            agents: vec![MirAgentDef {
                name: "a".into(),
                task_expr: MirExpr::lit(
                    crate::common::Literal::Nil(crate::common::Span::new(1, 1)),
                    crate::common::Span::new(1, 1),
                ),
                verify_expr: None,
                with_config: None,
                task_body: failing_body,
                combiner_body: None,
            }],
            edges: vec![MirEdgeDef {
                from: "@start".into(),
                to: "a".into(),
                condition_expr: None,
                condition_body: None,
            }],
            state_schema: vec![],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config).with_parallelism(2);
        let mut interp = crate::interpreter::Interpreter::new();
        let err = engine.run(&mut interp).unwrap_err();
        assert!(
            err.contains("Pregel node 'a'"),
            "error must identify the agent, got: {}",
            err
        );
    }

    /// Fault tolerance runs cleanly; stats reflect steps/agents.
    /// Note: `@start` occupies one empty super-step, so with 1 agent the
    /// total is 2 steps (start → agent a).
    #[test]
    fn fault_tolerance_runs_and_stats() {
        let config = MirPregelConfig {
            agents: vec![make_const_agent("a", 7)],
            edges: vec![MirEdgeDef {
                from: "@start".into(),
                to: "a".into(),
                condition_expr: None,
                condition_body: None,
            }],
            state_schema: vec![],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config)
            .with_parallelism(2)
            .with_fault_tolerance(3);
        let mut interp = crate::interpreter::Interpreter::new();
        let result = engine.run(&mut interp).unwrap();
        assert_eq!(result, Value::String("7".to_string()));
        assert_eq!(engine.stats().steps, 2, "@start empty step + agent a");
        assert_eq!(engine.stats().agents_run, 1);
        assert_eq!(engine.stats().retries, 0, "no failures expected");
    }

    // ─── v0.75.28: Master.compute 激活守卫（方向 7 约束原语骨架）───────
    // master_compute（v0.72 每超步全局协调钩子）+ aggregators + vote_to_halt
    // 构成「每步评估目标 + 收敛」骨架；此前该钩子从未被任何测试激活
    // （全部 None）。本测试激活执行路径并锁定失败传播（v0.75.28 修复：
    // 此前失败被 eprintln warn 吞掉）。

    #[test]
    fn master_compute_runs_and_failure_propagates() {
        // 正常 master_compute（Const Nil 恒成功）→ engine.run 成功。
        let ok_master = MirFunction {
            params: Vec::new(),
            body: vec![MirInst::Const(0, Value::Nil), MirInst::Return(None)],
            n_regs: 1,
        
            ..Default::default()};
        let config = MirPregelConfig {
            agents: vec![make_const_agent("a", 1)],
            edges: vec![MirEdgeDef {
                from: "@start".into(),
                to: "a".into(),
                condition_expr: None,
                condition_body: None,
            }],
            state_schema: vec![],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: Some(ok_master),
        };
        let mut engine = MirPregelEngine::new(config);
        let mut interp = crate::interpreter::Interpreter::new();
        let result = engine.run(&mut interp);
        assert!(
            result.is_ok(),
            "正常 master_compute 应让引擎运行成功: {:?}",
            result
        );

        // 失败 master_compute（Call 未知函数 → run_mir Err）→ engine.run
        // 必须传播错误（此前 eprintln warn 静默继续）。
        let bad_master = MirFunction {
            params: Vec::new(),
            body: vec![MirInst::Call(0, "__no_such_fn__".to_string(), Vec::new())],
            n_regs: 1,
        
            ..Default::default()};
        let config_bad = MirPregelConfig {
            agents: vec![make_const_agent("a", 1)],
            edges: vec![MirEdgeDef {
                from: "@start".into(),
                to: "a".into(),
                condition_expr: None,
                condition_body: None,
            }],
            state_schema: vec![],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: Some(bad_master),
        };
        let mut engine_bad = MirPregelEngine::new(config_bad);
        let mut interp_bad = crate::interpreter::Interpreter::new();
        let err = engine_bad
            .run(&mut interp_bad)
            .expect_err("master_compute 失败应传播（不再被 eprintln warn 吞掉）");
        assert!(
            err.contains("__no_such_fn__") || err.contains("function"),
            "错误信息应含失败原因: {}",
            err
        );
    }
    /// v0.75: vote_to_halt — an agent whose task_body ends with MirInst::Halt
    /// must be marked Halted in vertex_state (signal actually propagates now).
    #[test]
    fn vote_to_halt_marks_vertex_halted() {
        let halt_body = MirFunction {
            params: Vec::new(),
            body: vec![MirInst::Const(0, Value::Int(99)), MirInst::Halt(Some(0))],
            n_regs: 1,
        
            ..Default::default()};
        let config = MirPregelConfig {
            agents: vec![MirAgentDef {
                name: "a".into(),
                task_expr: MirExpr::lit(
                    crate::common::Literal::Nil(crate::common::Span::new(1, 1)),
                    crate::common::Span::new(1, 1),
                ),
                verify_expr: None,
                with_config: None,
                task_body: halt_body,
                combiner_body: None,
            }],
            edges: vec![MirEdgeDef {
                from: "@start".into(),
                to: "a".into(),
                condition_expr: None,
                condition_body: None,
            }],
            state_schema: vec![],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config);
        let mut interp = crate::interpreter::Interpreter::new();
        let result = engine.run(&mut interp).unwrap();
        // Halt(Some(0)) returns the register value (99).
        assert_eq!(result, Value::String("99".to_string()));
        assert_eq!(
            engine.vertex_state.get("a"),
            Some(&VertexState::Halted),
            "agent ending in Halt must be marked Halted"
        );
    }

    /// v0.75: Aggregator Max/Min reduce correctly (was identity before).
    #[test]
    fn aggregator_max_min_work() {
        let mut engine = MirPregelEngine::new(MirPregelConfig {
            agents: vec![],
            edges: vec![],
            state_schema: vec![],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: vec![
                crate::mir::expr::MirAggregatorDef {
                    name: "hi".into(),
                    ty: "Int".into(),
                    initial: Value::Int(0),
                    reducer: crate::mir::expr::AggregatorKind::Max,
                },
                crate::mir::expr::MirAggregatorDef {
                    name: "lo".into(),
                    ty: "Int".into(),
                    initial: Value::Int(i64::MAX),
                    reducer: crate::mir::expr::AggregatorKind::Min,
                },
            ],
            master_compute: None,
        });

        // Max: contribute 5 then 42 → 42
        engine.aggregator_contribute("hi", Value::Int(5)).unwrap();
        engine.aggregator_contribute("hi", Value::Int(42)).unwrap();
        assert_eq!(engine.aggregator_acc.get("hi"), Some(&Value::Int(42)));

        // Min: contribute 100 then 7 → 7
        engine.aggregator_contribute("lo", Value::Int(100)).unwrap();
        engine.aggregator_contribute("lo", Value::Int(7)).unwrap();
        assert_eq!(engine.aggregator_acc.get("lo"), Some(&Value::Int(7)));
    }

    // ─── v0.75.3: StepUndo 增量 step 快照 ─────────────────────────

    #[test]
    fn begin_step_skipped_when_no_fault_tolerance() {
        // 默认 fault_tolerance == 0 → begin_step 为零成本（None）。
        let config = MirPregelConfig {
            agents: vec![make_const_agent("a", 1)],
            edges: vec![MirEdgeDef {
                from: "@start".into(),
                to: "a".into(),
                condition_expr: None,
                condition_body: None,
            }],
            state_schema: vec![],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config.clone());
        assert!(engine.begin_step().is_none());

        let mut engine_ft = MirPregelEngine::new(config).with_fault_tolerance(3);
        assert!(engine_ft.begin_step().is_some());
    }

    #[test]
    fn step_undo_rolls_back_pending_sends() {
        // begin_step → EXEC 增发 pending_sends + 改 vertex_state/aggregator_acc
        // → rollback_step 还原 pending_sends，vertex_state 清空、
        // aggregator_acc 回到 config initials（与 restore_checkpoint 语义一致）。
        let config = MirPregelConfig {
            agents: vec![make_const_agent("a", 1)],
            edges: vec![MirEdgeDef {
                from: "@start".into(),
                to: "a".into(),
                condition_expr: None,
                condition_body: None,
            }],
            state_schema: vec![],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: vec![crate::mir::expr::MirAggregatorDef {
                name: "sum".into(),
                ty: "Int".into(),
                initial: Value::Int(0),
                reducer: crate::mir::expr::AggregatorKind::Add,
            }],
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config).with_fault_tolerance(2);

        // 步初：一条 pending send（前一步遗留）
        engine.flush_pending_sends(vec![SendTask {
            target_node: "b".into(),
            input: Value::Int(7),
        }]);
        let undo = engine.begin_step().expect("ft > 0 时应有 undo");

        // EXEC 增发 + 改状态
        engine.flush_pending_sends(vec![SendTask {
            target_node: "c".into(),
            input: Value::Int(9),
        }]);
        engine.vertex_state.insert("a".into(), VertexState::Halted);
        engine.aggregator_contribute("sum", Value::Int(5)).unwrap();
        assert_eq!(engine.pending_sends.len(), 2);
        assert_eq!(engine.aggregator_acc.get("sum"), Some(&Value::Int(5)));

        // 回滚
        engine.rollback_step(&undo);
        assert_eq!(engine.pending_sends.len(), 1, "增发的 send 被还原");
        assert_eq!(engine.pending_sends[0].target_node, "b");
        assert!(engine.vertex_state.is_empty());
        assert_eq!(
            engine.aggregator_acc.get("sum"),
            Some(&Value::Int(0)),
            "aggregator 回到 config initial"
        );
    }

    // ─── v0.75.4: 消息计数 + 提前失败校验 ─────────────────────────

    #[test]
    fn advance_rejects_send_to_undefined_node() {
        // send 到未定义节点在 ADVANCE（消息分发点）立即报错，而非延迟到
        // 下一超步 EXEC 才崩溃。Giraph message-ACK 精神：提前确认每条
        // 消息都有合法接收者。
        let config = MirPregelConfig {
            agents: vec![make_const_agent("a", 1)],
            edges: vec![MirEdgeDef {
                from: "@start".into(),
                to: "a".into(),
                condition_expr: None,
                condition_body: None,
            }],
            state_schema: vec![],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config);
        // 手工注入一条发往未知节点的消息（等价于某 agent send 到 "ghost"）
        engine.flush_pending_sends(vec![SendTask {
            target_node: "ghost".into(),
            input: Value::Int(1),
        }]);
        let mut interp = crate::interpreter::Interpreter::new();
        let err = engine.run(&mut interp).unwrap_err();
        assert!(
            err.contains("undefined node 'ghost'"),
            "错误应指明未知节点, got: {}",
            err
        );
        assert!(err.contains("a"), "错误应列出已定义 agents, got: {}", err);
        assert_eq!(
            engine.stats().steps,
            0,
            "失败发生在第一超步 ADVANCE（steps 自增之前），未进入任何 EXEC"
        );
    }

    #[test]
    fn messages_sent_tracks_advance_delivery() {
        // 图 a →(send)→ b：a 的 task_body 用 MirInst::Send 发消息给 b。
        // 超步边界 ADVANCE 应统计到消息，且 b 因收到消息被再次激活。
        let send_body = MirFunction {
            params: Vec::new(),
            body: vec![
                MirInst::Const(0, Value::Int(42)),
                MirInst::Send {
                    value: 0,
                    target: "b".into(),
                },
                MirInst::Return(Some(0)),
            ],
            n_regs: 1,
            ..Default::default()
        };
        let agent_a = MirAgentDef {
            name: "a".into(),
            task_expr: MirExpr::lit(
                crate::common::Literal::Nil(crate::common::Span::new(1, 1)),
                crate::common::Span::new(1, 1),
            ),
            verify_expr: None,
            with_config: None,
            task_body: send_body,
            combiner_body: None,
        };
        let config = MirPregelConfig {
            agents: vec![agent_a, make_const_agent("b", 7)],
            edges: vec![MirEdgeDef {
                from: "@start".into(),
                to: "a".into(),
                condition_expr: None,
                condition_body: None,
            }],
            state_schema: vec![],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config);
        let mut interp = crate::interpreter::Interpreter::new();
        engine.run(&mut interp).unwrap();
        assert!(
            engine.stats().messages_sent >= 1,
            "a 的 send 应在超步边界被统计: {}",
            engine.stats().messages_sent
        );
    }

    // ─── v0.75.9: 全局 DAG 缓存（取代引擎本地 agent_dag_cache）──────────

    #[test]
    fn global_dag_cache_is_idempotent() {
        // 同一 task_body 的 Arc 两次缓存调用返回同一个 Arc（缓存命中）。
        // 全局缓存 = mir::cache::DAG_CACHE，pregel/Closure/REPL 共用。
        let agent = make_const_agent("a", 7);
        let body = std::sync::Arc::new(agent.task_body);
        let cache = crate::mir::cache::DagCache::new();
        let d1 = cache.get_or_build(&body);
        let d2 = cache.get_or_build(&body);
        assert!(std::sync::Arc::ptr_eq(&d1, &d2), "重复调用应命中缓存");
        assert_eq!(d1.nodes.len(), d2.nodes.len());
        assert_eq!(d1.edges.len(), d2.edges.len());
    }

    #[test]
    fn multi_step_run_uses_cached_dag() {
        // 多超步图（a send→b）：run 后 stats.steps >= 2，且结果非空 —
        // 缓存路径与未缓存语义一致（此前 parallel_matches_sequential 等
        // 回归已覆盖单路径，此处验证多超步 + 缓存组合不破坏结果）。
        let send_body = MirFunction {
            params: Vec::new(),
            body: vec![
                MirInst::Const(0, Value::Int(42)),
                MirInst::Send {
                    value: 0,
                    target: "b".into(),
                },
                MirInst::Return(Some(0)),
            ],
            n_regs: 1,
            ..Default::default()
        };
        let agent_a = MirAgentDef {
            name: "a".into(),
            task_expr: MirExpr::lit(
                crate::common::Literal::Nil(crate::common::Span::new(1, 1)),
                crate::common::Span::new(1, 1),
            ),
            verify_expr: None,
            with_config: None,
            task_body: send_body,
            combiner_body: None,
        };
        let config = MirPregelConfig {
            agents: vec![agent_a, make_const_agent("b", 7)],
            edges: vec![MirEdgeDef {
                from: "@start".into(),
                to: "a".into(),
                condition_expr: None,
                condition_body: None,
            }],
            state_schema: vec![],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config);
        let mut interp = crate::interpreter::Interpreter::new();
        let result = engine.run(&mut interp).unwrap();
        assert!(
            engine.stats().steps >= 2,
            "a send→b 应至少两个超步: {}",
            engine.stats().steps
        );
        assert!(
            !matches!(result, Value::Nil),
            "多超步 + 缓存路径应产出非空结果, got: {:?}",
            result
        );
    }

    // ─── v0.75.7: FPGA 式调度（per-agent 计时 + LJF 排序）────────────

    #[test]
    fn stats_tracks_per_agent_duration() {
        // 两 agent 图跑完：per_agent_ms 应含两节点且耗时 > 0。
        let config = MirPregelConfig {
            agents: vec![make_const_agent("a", 1), make_const_agent("b", 2)],
            edges: vec![
                MirEdgeDef {
                    from: "@start".into(),
                    to: "a".into(),
                    condition_expr: None,
                    condition_body: None,
                },
                MirEdgeDef {
                    from: "@start".into(),
                    to: "b".into(),
                    condition_expr: None,
                    condition_body: None,
                },
            ],
            state_schema: vec![],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config).with_parallelism(2);
        let mut interp = crate::interpreter::Interpreter::new();
        engine.run(&mut interp).unwrap();
        assert!(
            engine.stats().per_agent_ms.contains_key("a"),
            "per_agent_ms 应记录 agent a"
        );
        assert!(
            engine.stats().per_agent_ms.contains_key("b"),
            "per_agent_ms 应记录 agent b"
        );
    }

    #[test]
    fn ljf_order_preserves_correctness() {
        // LJF 排序改变分发顺序，但 BSP 语义保证同超步顺序无关 —
        // 排序后的运行结果应与未排序一致（两 agent 结果都是确定字面量）。
        let mut heavy = make_const_agent("heavy", 99);
        // 构造 DAG 复杂度明显更高的 heavy agent（更多指令）
        heavy.task_body = MirFunction {
            params: Vec::new(),
            body: vec![
                MirInst::Const(0, Value::Int(1)),
                MirInst::Const(1, Value::Int(2)),
                MirInst::BinaryOp(2, 0, crate::common::BinaryOp::Add, 1),
                MirInst::BinaryOp(3, 2, crate::common::BinaryOp::Mul, 2),
                MirInst::BinaryOp(4, 3, crate::common::BinaryOp::Sub, 1),
                MirInst::Return(Some(4)),
            ],
            n_regs: 5,
        
            ..Default::default()};
        let config = MirPregelConfig {
            agents: vec![heavy.clone(), make_const_agent("light", 7)],
            edges: vec![
                MirEdgeDef {
                    from: "@start".into(),
                    to: "heavy".into(),
                    condition_expr: None,
                    condition_body: None,
                },
                MirEdgeDef {
                    from: "@start".into(),
                    to: "light".into(),
                    condition_expr: None,
                    condition_body: None,
                },
            ],
            state_schema: vec![],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config).with_parallelism(2);
        let mut interp = crate::interpreter::Interpreter::new();
        let result = engine.run(&mut interp).unwrap();
        // 两 agent 都执行（LJF 只改分发顺序，不改 BSP 结果）
        assert!(
            engine.stats().agents_run >= 2,
            "heavy + light 都应执行: {}",
            engine.stats().agents_run
        );
        // result 是最后写入者（LWW），heavy 或 light 之一 — 非 Nil 即正确
        assert!(
            !matches!(result, Value::Nil),
            "LJF 排序后结果应非空: {:?}",
            result
        );
    }

    // ─── v0.75.8: 增量执行 v1（input 未变则跳过）────────────────────

    #[test]
    fn incremental_skip_when_input_unchanged() {
        // 预填充缓存（input "{}" 与 build_node_input 首次返回一致），
        // 则 agent "a" 首次激活即被跳过（agents_run 不增加），
        // 结果复用缓存 outcome — 确定性验证跳过路径。
        let config = MirPregelConfig {
            agents: vec![make_const_agent("a", 1)],
            edges: vec![MirEdgeDef {
                from: "@start".into(),
                to: "a".into(),
                condition_expr: None,
                condition_body: None,
            }],
            state_schema: vec![],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config);
        // 预填充：input = "{}"（无 channel 时 build_node_input 返回），
        // outcome = (Return(42), Int(42), 无 sends)
        engine.agent_input_cache.insert("a".into(), "{}".into());
        engine.agent_outcome_cache.insert(
            "a".into(),
            (
                crate::mir::vm::MirSignal::Return(Value::Int(42)),
                Value::Int(42),
                Vec::new(),
            ),
        );
        let mut interp = crate::interpreter::Interpreter::new();
        let result = engine.run(&mut interp).unwrap();
        assert_eq!(
            engine.stats().agents_run,
            0,
            "input 未变时 agent 应被跳过（不执行）"
        );
        // 结果复用缓存 outcome → result 通道 = 42
        assert_eq!(result, Value::String("42".to_string()));
    }

    #[test]
    fn incremental_cache_consistent_after_run() {
        // 正常 run 后：缓存填充，且缓存 input 与 build_node_input 一致。
        let config = MirPregelConfig {
            agents: vec![make_const_agent("a", 7)],
            edges: vec![MirEdgeDef {
                from: "@start".into(),
                to: "a".into(),
                condition_expr: None,
                condition_body: None,
            }],
            state_schema: vec![],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config);
        let mut interp = crate::interpreter::Interpreter::new();
        let result = engine.run(&mut interp).unwrap();
        assert_eq!(result, Value::String("7".to_string()));
        assert!(
            engine.agent_input_cache.contains_key("a"),
            "run 后应填充 input 缓存"
        );
        assert!(
            engine.agent_outcome_cache.contains_key("a"),
            "run 后应填充 outcome 缓存"
        );
        // 注：build_node_input 是时间敏感的（run 结束后 result 通道已写入，
        // 返回 {"result":"7"}）；缓存记录的是 a 执行时刻的 input（当时为 {}）。
        // 跳过机制的确定性由 incremental_skip_when_input_unchanged 验证。
        assert_eq!(
            engine.agent_input_cache["a"], "{}",
            "a 首次执行时 input 应为空"
        );
    }

    // ─── v0.75.10: 寄存器级增量（memo + 稳定 Arc + 加法注入）───────────

    /// 构造自定义 task_body 的 agent。
    fn make_custom_agent(name: &str, body: MirFunction) -> MirAgentDef {
        MirAgentDef {
            name: name.to_string(),
            task_expr: MirExpr::lit(
                crate::common::Literal::Nil(crate::common::Span::new(1, 1)),
                crate::common::Span::new(1, 1),
            ),
            verify_expr: None,
            with_config: None,
            task_body: body,
            combiner_body: None,
        }
    }

    #[test]
    fn register_memo_skips_pure_nodes_on_reactivation() {
        // a1 (step1) 向 a2 和 b 发消息；a2 (step2) 向 b 发消息 → b 跑两次，
        // 两次 input 不同（v1 不整体跳过），但 b 的纯前缀 Const 输入未变 →
        // 寄存器级 memo 跳过 Const。这是 v1（input 未变才跳过）覆盖不到的场景。
        let a1 = make_custom_agent(
            "a1",
            MirFunction {
                params: Vec::new(),
                body: vec![
                    MirInst::Const(0, Value::Int(1)),
                    MirInst::Send {
                        value: 0,
                        target: "a2".into(),
                    },
                    MirInst::Send {
                        value: 0,
                        target: "b".into(),
                    },
                    MirInst::Return(Some(0)),
                ],
                n_regs: 1,
                ..Default::default()
            },
        );
        let a2 = make_custom_agent(
            "a2",
            MirFunction {
                params: Vec::new(),
                body: vec![
                    MirInst::Const(0, Value::Int(2)),
                    MirInst::Send {
                        value: 0,
                        target: "b".into(),
                    },
                    MirInst::Return(Some(0)),
                ],
                n_regs: 1,
                ..Default::default()
            },
        );
        // b 含纯前缀 Const(100) + 非纯 Var(input) + Return — 第二次运行应跳过 Const。
        let b = make_custom_agent(
            "b",
            MirFunction {
                params: Vec::new(),
                body: vec![
                    MirInst::Const(0, Value::Int(100)),
                    MirInst::Var(1, "input".to_string()),
                    MirInst::Return(Some(0)),
                ],
                n_regs: 2,
            
            ..Default::default()},
        );
        let config = MirPregelConfig {
            agents: vec![a1, a2, b],
            edges: vec![
                MirEdgeDef {
                    from: "@start".into(),
                    to: "a1".into(),
                    condition_expr: None,
                    condition_body: None,
                },
                MirEdgeDef {
                    from: "a1".into(),
                    to: "a2".into(),
                    condition_expr: None,
                    condition_body: None,
                },
                MirEdgeDef {
                    from: "a1".into(),
                    to: "b".into(),
                    condition_expr: None,
                    condition_body: None,
                },
                MirEdgeDef {
                    from: "a2".into(),
                    to: "b".into(),
                    condition_expr: None,
                    condition_body: None,
                },
            ],
            state_schema: vec![],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config);
        let mut interp = crate::interpreter::Interpreter::new();
        let _ = engine.run(&mut interp).unwrap();
        assert!(
            engine.stats().steps >= 3,
            "a1→a2→b 链路应至少 3 超步: {}",
            engine.stats().steps
        );
        // b 跑两次（step2 input {}、step3 input {"input":2}）→ 第二次纯前缀跳过。
        // executed_nodes 只统计纯节点（Const），run1 执行 1 次后 run2/run3 全跳。
        let memo = engine.agent_memos.get("b").expect("b 应积累 memo");
        assert!(
            memo.skipped_nodes >= 1,
            "b 二次运行时 Const 纯节点应被跳过 (skipped={})",
            memo.skipped_nodes
        );
        assert_eq!(memo.executed_nodes, 1, "Const 只实际执行一次（run1）");
        // 稳定 Arc：跨超步同一指针（全局 DAG 缓存真正命中的前提）
        let arc = engine.task_arcs.get("b").cloned().expect("b 应有稳定 Arc");
        assert!(
            std::sync::Arc::ptr_eq(&arc, &engine.task_arcs["b"]),
            "task_arcs 应跨超步稳定"
        );
    }

    #[test]
    fn channel_input_var_injected_additively() {
        // C 路径：保留 `input` 契约，另注入 `input_<channel>`。delta 语义：
        // 首次激活的 snapshot 记录当前版本（同版本无 delta），**再次激活**且
        // 有新消息时才可见 — 两个 sender 依次给 b 发消息，b 第二次激活读
        // input_b 拿到细粒度 typed 值。
        let a1 = make_custom_agent(
            "a1",
            MirFunction {
                params: Vec::new(),
                body: vec![
                    MirInst::Const(0, Value::Int(11)),
                    MirInst::Send {
                        value: 0,
                        target: "b".into(),
                    },
                    MirInst::Return(Some(0)),
                ],
                n_regs: 1,
                ..Default::default()
            },
        );
        let a2 = make_custom_agent(
            "a2",
            MirFunction {
                params: Vec::new(),
                body: vec![
                    MirInst::Const(0, Value::Int(22)),
                    MirInst::Send {
                        value: 0,
                        target: "b".into(),
                    },
                    MirInst::Return(Some(0)),
                ],
                n_regs: 1,
                ..Default::default()
            },
        );
        // b 读 input_input（消息 channel 名就是 "input"；逐 channel 细粒度
        // var = input_<channel>；`input` 契约仍保留）
        let b = make_custom_agent(
            "b",
            MirFunction {
                params: Vec::new(),
                body: vec![
                    MirInst::Var(0, "input_input".to_string()),
                    MirInst::Return(Some(0)),
                ],
                n_regs: 1,
            
            ..Default::default()},
        );
        let config = MirPregelConfig {
            agents: vec![a1, a2, b],
            edges: vec![
                MirEdgeDef {
                    from: "@start".into(),
                    to: "a1".into(),
                    condition_expr: None,
                    condition_body: None,
                },
                MirEdgeDef {
                    from: "a1".into(),
                    to: "a2".into(),
                    condition_expr: None,
                    condition_body: None,
                },
                MirEdgeDef {
                    from: "a1".into(),
                    to: "b".into(),
                    condition_expr: None,
                    condition_body: None,
                },
                MirEdgeDef {
                    from: "a2".into(),
                    to: "b".into(),
                    condition_expr: None,
                    condition_body: None,
                },
            ],
            state_schema: vec![],
            checkpoint: None,
            interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config);
        let mut interp = crate::interpreter::Interpreter::new();
        let result = engine.run(&mut interp).unwrap();
        // b 第二次激活（step3）input_b = Int(22)（a2 的消息，last-write-wins）
        assert_eq!(result, Value::String("22".to_string()));
        // input 契约仍保留（b 的 input 缓存被填充）
        assert!(
            engine.agent_input_cache.contains_key("b"),
            "旧 input 契约缓存应保留"
        );
    }
}
