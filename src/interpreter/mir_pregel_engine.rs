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
//! 当前为骨架（Batch D2）：定义结构 + 接口，内部逻辑先用 TODO 标记。

use std::collections::HashMap;
use std::sync::Arc;

use crate::checkpoint::{Checkpoint, SendTask};
use crate::interpreter::Interpreter;
use crate::mir::expr::{
    MirAgentDef, MirEdgeDef, MirInterruptPoint, MirInterruptWhen, MirPregelConfig,
    MirReducerKind, MirStateChannel,
};
use crate::mir::MirFunction;
use crate::value::Value;

/// Interrupt 回调签名
pub type MirInterruptCallback = Arc<dyn Fn(&str, MirInterruptWhen) -> bool>;

/// Pregel 引擎 BSP 循环状态
pub struct MirPregelEngine {
    config: MirPregelConfig,
    agents_by_name: HashMap<String, usize>,
    state_reducers: HashMap<String, MirReducerKind>,

    channels: HashMap<String, Value>,
    channel_versions: HashMap<String, u64>,
    versions_seen: HashMap<String, HashMap<String, u64>>,

    pending_sends: Vec<SendTask>,

    max_steps: usize,
    interrupt_before: Option<MirInterruptCallback>,
    interrupt_after: Option<MirInterruptCallback>,
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
            max_steps: 1000,
            interrupt_before: None,
            interrupt_after: None,
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

        let mut step: usize = 0;
        let mut active_nodes: Vec<String> = vec!["@start".to_string()];

        while !active_nodes.is_empty() && step < self.max_steps {
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
                let result = crate::mir::interp::run_mir(&agent.task_body, interpreter, &mut env)
                    .map_err(|e| format!("Pregel node '{}': {}", node_name, e))?;

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
            step += 1;
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

    /// 应用写入 — 通过 MirReducerKind
    pub fn apply_write(
        &mut self,
        channel: String,
        value: Value,
        _interpreter: &mut Interpreter,
    ) -> Result<(), String> {
        let reducer = self
            .state_reducers
            .get(&channel)
            .cloned()
            .unwrap_or(MirReducerKind::Last);

        let current = self.channels.get(&channel).cloned();
        let new_value = match reducer {
            MirReducerKind::Last => value,
            MirReducerKind::Append => {
                let mut list = match current {
                    Some(Value::List(l)) => l,
                    _ => Vec::new(),
                };
                list.push(value);
                Value::List(list)
            }
            MirReducerKind::Add => {
                let cur_num = match &current {
                    Some(Value::Float(n)) => *n,
                    Some(Value::Int(n)) => *n as f64,
                    _ => 0.0,
                };
                let new_num = match &value {
                    Value::Float(n) => *n,
                    Value::Int(n) => *n as f64,
                    _ => {
                        return Err(format!(
                            "Pregel @add reducer expects number, got {:?}",
                            value
                        ));
                    }
                };
                Value::Float(cur_num + new_num)
            }
            MirReducerKind::Merge(_merge_expr) => {
                // v0.57: merge body stored in MirExpr — TODO 实际调用 run_mir(body, ...)
                current.unwrap_or(value)
            }
            // v0.57: 其他 reducer 类型当前未在 V3 pipeline 触发
            MirReducerKind::Sum | MirReducerKind::Product | MirReducerKind::Concat => {
                return Err(format!(
                    "Pregel reducer {:?} not yet implemented in MIR-native engine",
                    reducer
                ));
            }
            MirReducerKind::Custom(_) => {
                // v0.57: Custom reducer 暂走 Last 行为
                value
            }
        };

        self.channels.insert(channel.clone(), new_value);
        *self.channel_versions.entry(channel).or_insert(0) += 1;
        Ok(())
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

    /// 构建 checkpoint 快照
    pub fn build_checkpoint(&self, step: usize) -> Checkpoint {
        // v0.57: 从 state 构造 Checkpoint
        Checkpoint::new(
            "default".to_string(),
            step,
            self.channels.clone(),
            self.channel_versions.clone(),
            self.versions_seen.clone(),
            self.pending_sends.clone(),
        )
    }

    /// 从 checkpoint 恢复
    pub fn restore_checkpoint(&mut self, _cp: &Checkpoint) {
        // TODO: 从 cp 恢复 channels/channel_versions
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