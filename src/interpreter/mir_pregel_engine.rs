//! v0.57: MIR-native Pregel 引擎（Batch D2 — 与 orchestrate_v2.rs 并行存在）
//!
//! 与 `orchestrate_v2::PregelEngine` 功能等价，但：
//! - 直接消费 `Mir*` 类型（`MirAgentDef`/`MirEdgeDef`/...），无 ast_compat 桥接
//! - `task_body`/`verify_body`/`condition_body`/`merge_body`/`thread_id_body`
//!   均内嵌在对应 MIR-native 字段中，无需额外 HashMap
//! - 零 `AstArena`/`NodeId` 依赖
//!
//! Batch D3 将切换调用方；D4 删除旧 `orchestrate_v2::PregelEngine`。
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
//!
//! 当前为骨架（Batch D2）：定义结构 + 接口，内部逻辑先用 TODO 标记。

use std::collections::HashMap;
use std::sync::Arc;

use crate::checkpoint::{Checkpoint, CheckpointSaver, SendTask};
use crate::interpreter::Interpreter;
use crate::mir::expr::{
    MirAgentDef, MirEdgeDef, MirInterruptPoint, MirInterruptWhen, MirPregelConfig,
    MirReducerKind, MirStateChannel,
};
use crate::mir::expr::MirExpr;
use crate::mir::MirFunction;
use crate::value::{Conflict, MergeStrategy, Value};

/// Interrupt 回调签名
pub type MirInterruptCallback = Arc<dyn Fn(&str, MirInterruptWhen) -> bool>;

/// v0.62: Conflict callback — invoked for each detected write-write conflict.
/// Return `true` to continue the BSP run, `false` to abort.
pub type MirConflictCallback = Arc<dyn Fn(&Conflict) -> bool>;

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
fn accumulator_reduce(
    current: Option<Value>,
    incoming: Value,
    op: &str,
) -> Result<Value, String> {
    let identity = match op {
        "+" => Value::Int(0),
        "*" => Value::Int(1),
        _ => return Err(format!("Unknown accumulator op: {}", op)),
    };
    let cur = current.unwrap_or(identity);
    match op {
        "+" => crate::flow::eval_binary(cur, &crate::common::BinaryOp::Add, incoming),
        "*" => crate::flow::eval_binary(cur, &crate::common::BinaryOp::Mul, incoming),
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
            ty: None,
        }
    } else {
        MirExpr {
            kind: crate::mir::expr::MirExprKind::Variable(s.to_string()),
            span,
            ty: None,
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
        let aggregator_initial: HashMap<String, Value> = config
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
                a.combiner_body.as_ref().map(|b| (a.name.clone(), std::sync::Arc::new(b.clone())))
            })
            .collect();
        let master_compute = config.master_compute.as_ref()
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
            aggregator_reducer: HashMap::new(),
            combiner_bodies,
            master_compute,
            saver: None,
        }
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

    /// v0.69: Drain an external buffer of pending SendTasks into the engine.
    /// Called by `h_orchestrate` before each super-step to inject messages
    /// produced by `h_send` (which has no direct engine access).
    pub fn flush_pending_sends(&mut self, sends: Vec<SendTask>) {
        self.pending_sends.extend(sends);
    }

    /// v0.71: Snapshot of aggregator initial values (from config).
    fn aggregator_initial_snapshot(&self) -> Vec<(String, Value)> {
        self.config.aggregators.iter()
            .map(|a| (a.name.clone(), a.initial.clone()))
            .collect()
    }

    /// v0.71: Contribute a value to a per-super-step aggregator.
    /// Called by `h_aggregate`.
    pub fn aggregator_contribute(&mut self, name: &str, value: Value) -> Result<(), String> {
        let reducer = self.aggregator_reducer.get(name)
            .ok_or_else(|| format!("Unknown aggregator: {}", name))?
            .clone();
        let acc = self.aggregator_acc.entry(name.to_string())
            .or_insert_with(|| match reducer {
                crate::mir::expr::AggregatorKind::Add => Value::Int(0),
                crate::mir::expr::AggregatorKind::Max => value.clone(),
                crate::mir::expr::AggregatorKind::Min => value.clone(),
                crate::mir::expr::AggregatorKind::Last => value.clone(),
                crate::mir::expr::AggregatorKind::Concat => Value::String(String::new()),
            });
        *acc = match reducer {
            crate::mir::expr::AggregatorKind::Add => {
                crate::flow::eval_binary(std::mem::replace(acc, Value::Int(0)), &crate::common::BinaryOp::Add, value)?
            }
            crate::mir::expr::AggregatorKind::Max => {
                let cmp = crate::flow::eval_binary(value.clone(), &crate::common::BinaryOp::Greater, std::mem::replace(acc, Value::Int(0))).unwrap_or(Value::Bool(false));
                if matches!(cmp, Value::Bool(true)) { value } else { value }
            }
            crate::mir::expr::AggregatorKind::Min => {
                let cmp = crate::flow::eval_binary(value.clone(), &crate::common::BinaryOp::Less, std::mem::replace(acc, Value::Int(0))).unwrap_or(Value::Bool(false));
                if matches!(cmp, Value::Bool(true)) { value } else { value }
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
pub fn run(&mut self, interpreter: &mut Interpreter) -> Result<Value, String> {
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

            // ---------- 1. PLAN ----------
            let mut to_execute: Vec<String> = Vec::new();
            for node in &active_nodes {
                if node == "@start" {
                    continue;
                }
                // v0.70: Halted vertices are only rescheduled when targeted
                // by a Send — vote_to_halt semantics.
                let is_halted = matches!(
                    self.vertex_state.get(node),
                    Some(VertexState::Halted)
                );
                let targeted_by_send = self.pending_sends.iter()
                    .any(|s| s.target_node == *node);
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

            // ---------- 2. EXEC ----------
            // 记录激活节点的 snapshots
            for node_name in &to_execute {
                let snapshot = self
                    .versions_seen
                    .entry(node_name.clone())
                    .or_default();
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
                        // Default to active if no condition. Skip edge if
                        // condition returns falsy.
                        if let Some(cond_body) = &edge.condition_body {
                            let mut cond_env =
                                interpreter.core.environment.lock().clone();
                            let cond_val = crate::mir::interp::run_mir(
                                cond_body, interpreter, &mut cond_env,
                            ).unwrap_or(Value::Bool(false));
                            if !crate::flow::is_truthy(&cond_val) {
                                continue;
                            }
                        }
                        next_active.insert(edge.to.clone());
                    }
                }
            }

            let mut writes: Vec<(String, String, Value)> = Vec::new();

            for node_name in &to_execute {
                let agent_idx = *self.agents_by_name.get(node_name).ok_or_else(|| {
                    format!("Pregel: undefined agent '{}'", node_name)
                })?;
                let agent = &self.config.agents[agent_idx];

                // 构建输入（serialize channels）
                let input_val = self.build_node_input(node_name);

                // 设置 input 变量
                interpreter
                    .core
                    .environment
                    .lock()
                    .define("input".to_string(), Value::String(input_val.to_string()), false);

                // v0.57: 使用 pre-lowered task_body
                if agent.task_body.body.is_empty() && agent.task_body.n_regs == 0 {
                    return Err(format!(
                        "Pregel: agent '{}' has empty task_body (lowering missing)",
                        node_name
                    ));
                }

                let mut env = interpreter.core.environment.lock().clone();
                // v0.61: Tick vector clock for this agent
                env.clock.tick(node_name);
                let (signal, result) = crate::mir::interp::run_mir_with_signal(
                    &agent.task_body, interpreter, &mut env,
                ).map_err(|e| format!("Pregel node '{}': {}", node_name, e))?;

                // v0.70: vote_to_halt — mark vertex as Halted; will only
                // be rescheduled when a Send arrives.
                if matches!(signal, crate::mir::interp::MirSignal::Halt(_)) {
                    self.vertex_state.insert(node_name.clone(), VertexState::Halted);
                } else {
                    self.vertex_state.insert(node_name.clone(), VertexState::Active);
                }

                // v0.60: Merge agent environment back into shared environment.
                // Uses per-channel reducer strategies for conflict resolution.
                // v0.61: Collects write-write conflicts detected via vector clocks.
                let strategies = self.build_per_key_strategies();
                let conflicts = interpreter.core.environment.lock()
                    .merge_from_with_strategies(&env, &strategies, &MergeStrategy::LastWriteWins);
                #[allow(clippy::collapsible_if)]
                if !conflicts.is_empty() {
                    // v0.62: Invoke conflict callback (if set)
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

                // 简单解析：result 是值时写入 result 通道
                let result_str = result.to_string();
                writes.push((node_name.clone(), "result".to_string(), Value::String(result_str)));

                // 静态边 → 下一跳
                for edge in &self.config.edges {
                    if edge.from == *node_name && edge.to != "@exit" {
                        // v0.71: Edge condition evaluation (same as PLAN phase).
                        if let Some(cond_body) = &edge.condition_body {
                            let mut cond_env =
                                interpreter.core.environment.lock().clone();
                            let cond_val = crate::mir::interp::run_mir(
                                cond_body, interpreter, &mut cond_env,
                            ).unwrap_or(Value::Bool(false));
                            if !crate::flow::is_truthy(&cond_val) {
                                continue;
                            }
                        }
                        next_active.insert(edge.to.clone());
                    }
                }
            }

            // ---------- 3. UPDATE ----------
            for (_node, channel, value) in writes {
                self.apply_write(channel, value, interpreter)?;
            }

            // v0.71: Publish aggregator results as channels for next step.
            for (name, value) in &self.aggregator_acc {
                self.channels.insert(format!("aggregator_{}", name), value.clone());
                *self.channel_versions.entry(format!("aggregator_{}", name)).or_insert(0) += 1;
            }

            // v0.72: Master.compute — runs once per super-step after UPDATE.
            // Used for global coordination (e.g., dynamic topology changes,
            // aggregation-based decisions).
            if let Some(master) = self.master_compute.clone() {
                let mut master_env = interpreter.core.environment.lock().clone();
                let _ = crate::mir::interp::run_mir(&master, interpreter, &mut master_env);
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

            // ---------- 4. ADVANCE ----------
            // v0.69: Dynamic Send delivery — target nodes become active in
            // the next super-step and their `input` channel carries the payload.
            // v0.72: Combiners — multiple sends to the same target are folded
            // via the target's combiner_body (current, incoming) -> Value
            // before delivery. Default behavior (no combiner) = last-write-wins.
            let mut by_target: std::collections::HashMap<String, Vec<crate::value::Value>> =
                std::collections::HashMap::new();
            for send in self.pending_sends.drain(..) {
                by_target.entry(send.target_node).or_default().push(send.input);
            }
            for (target, messages) in by_target {
                let final_value = if let Some(combiner) = self.combiner_bodies.get(&target).cloned() {
                    let mut acc = messages[0].clone();
                    for incoming in &messages[1..] {
                        let mut env = interpreter.core.environment.lock().clone();
                        env.define("current".into(), acc.clone(), false);
                        env.define("incoming".into(), incoming.clone(), false);
                        match crate::mir::interp::run_mir(&combiner, interpreter, &mut env) {
                            Ok(v) => acc = v,
                            Err(_) => acc = incoming.clone(), // fallback: LWW
                        }
                    }
                    acc
                } else {
                    messages.last().cloned().unwrap_or(Value::Nil)
                };
                self.channels.insert("input".to_string(), final_value);
                *self.channel_versions.entry("input".to_string()).or_insert(0) += 1;
                next_active.insert(target);
            }
            active_nodes = next_active.into_iter().collect();
            self.current_step += 1;

            // v0.63: Auto-save checkpoint if configured
            if let Some(ref cp_cfg) = self.config.checkpoint {
                if let Some(interval) = cp_cfg.interval {
                    if self.current_step % interval as usize == 0 {
                        let cp = self.build_checkpoint();
                        if let Some(ref saver) = self.saver {
                            let thread_id = cp.thread_id.clone();
                            saver.save(&thread_id, &cp)?;
                        }
                    }
                }
            }
        }

        // 返回 result 通道
        Ok(self.channels.get("result").cloned().unwrap_or(Value::Nil))
    }

    /// 构建节点输入 — 序列化 channels
    fn build_node_input(&self, node_name: &str) -> String {
        let snapshot = self.versions_seen.get(node_name);
        let mut parts: Vec<String> = Vec::new();
        for (channel, version) in &self.channel_versions {
            let seen_version = snapshot
                .and_then(|s| s.get(channel))
                .copied()
                .unwrap_or(0);
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
        interpreter: &mut Interpreter,
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
                    let merge_fn = crate::mir::lower::lower_mir_exprs(&[merge_expr.clone()])
                        .map_err(|e| format!("Pregel merge body lowering failed: {}", e))?;
                    let mut merge_env = interpreter.core.environment.lock().clone();
                    merge_env.define("current".into(), current.unwrap_or(Value::Nil), false);
                    merge_env.define("incoming".into(), value, false);
                    crate::mir::interp::run_mir(&merge_fn, interpreter, &mut merge_env)
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
                    let mut merge_env = interpreter.core.environment.lock().clone();
                    merge_env.define("current".into(), current.unwrap_or(Value::Nil), false);
                    merge_env.define("incoming".into(), value, false);
                    crate::mir::interp::run_mir(&merge_fn, interpreter, &mut merge_env)
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
        let thread_id = self.config.checkpoint.as_ref()
            .and_then(|c| c.thread_id.as_ref())
            .map(|_| "pregel")  // MirExpr evaluation deferred; use config presence as signal
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
    use crate::mir::expr::{MirAgentDef, MirEdgeDef, MirExpr, MirStateChannel};

    fn empty_mir_function() -> MirFunction {
        MirFunction {
            params: Vec::new(),
            body: Vec::new(),
            n_regs: 0,
        }
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
            task_mir_expr: None,
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
            agents: vec![], edges: vec![],
            state_schema: vec![MirStateChannel {
                name: "total".into(), ty: "Int".into(),
                reducer: MirReducerKind::Sum,
            }],
            checkpoint: None, interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config);
        let mut interp = crate::interpreter::Interpreter::new();
        engine.apply_write("total".into(), Value::Int(10), &mut interp).unwrap();
        engine.apply_write("total".into(), Value::Int(32), &mut interp).unwrap();
        engine.apply_write("total".into(), Value::Int(8), &mut interp).unwrap();
        assert_eq!(engine.channels.get("total"), Some(&Value::Int(50)));
    }

    /// v0.67: Product reducer multiplies across writes
    #[test]
    fn mir_pregel_engine_apply_write_product() {
        let config = MirPregelConfig {
            agents: vec![], edges: vec![],
            state_schema: vec![MirStateChannel {
                name: "acc".into(), ty: "Int".into(),
                reducer: MirReducerKind::Product,
            }],
            checkpoint: None, interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config);
        let mut interp = crate::interpreter::Interpreter::new();
        engine.apply_write("acc".into(), Value::Int(2), &mut interp).unwrap();
        engine.apply_write("acc".into(), Value::Int(3), &mut interp).unwrap();
        engine.apply_write("acc".into(), Value::Int(4), &mut interp).unwrap();
        assert_eq!(engine.channels.get("acc"), Some(&Value::Int(24)));
    }

    /// v0.67: Concat reducer accumulates strings
    #[test]
    fn mir_pregel_engine_apply_write_concat() {
        let config = MirPregelConfig {
            agents: vec![], edges: vec![],
            state_schema: vec![MirStateChannel {
                name: "log".into(), ty: "String".into(),
                reducer: MirReducerKind::Concat,
            }],
            checkpoint: None, interrupt_points: vec![],
            adjacency: HashMap::new(),
            aggregators: Vec::new(),
            master_compute: None,
        };
        let mut engine = MirPregelEngine::new(config);
        let mut interp = crate::interpreter::Interpreter::new();
        engine.apply_write("log".into(), Value::String("hello".into()), &mut interp).unwrap();
        engine.apply_write("log".into(), Value::String(" world".into()), &mut interp).unwrap();
        assert_eq!(engine.channels.get("log"), Some(&Value::String("hello world".into())));
    }
}