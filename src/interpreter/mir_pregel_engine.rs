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
            // ---------- 1. PLAN ----------
            let mut to_execute: Vec<String> = Vec::new();
            for node in &active_nodes {
                if node == "@start" {
                    continue;
                }
                if self.agents_by_name.contains_key(node)
                    || self.pending_sends.iter().any(|s| s.target_node == *node)
                {
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
                let result = crate::mir::interp::run_mir(&agent.task_body, interpreter, &mut env)
                    .map_err(|e| format!("Pregel node '{}': {}", node_name, e))?;

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
                        next_active.insert(edge.to.clone());
                    }
                }
            }

            // ---------- 3. UPDATE ----------
            for (_node, channel, value) in writes {
                self.apply_write(channel, value, interpreter)?;
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
                MirReducerKind::Sum | MirReducerKind::Product | MirReducerKind::Concat => {
                    return Err(format!(
                        "Pregel reducer {:?} not yet implemented in MIR-native engine",
                        reducer
                    ));
                }
                MirReducerKind::Custom(_) => value,
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
}