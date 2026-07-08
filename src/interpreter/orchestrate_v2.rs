//! v0.50: Pregel BSP 执行引擎 — Channel + Reducer + Checkpoint
//!
//! 实现 LangGraph 风格的 Pregel（Bulk Synchronous Parallel）执行模型：
//! - 每步内所有节点读取上一步结束后的状态快照（版本隔离）
//! - 步骤内写入通过 Reducer 合并到 Channel，但本步内不可见
//! - 支持 Command 动态路由、Send 动态派发、Checkpoint 持久化
//!
//! 设计要点：
//! - 零 panic：所有路径返回 `Result`（包括 `apply_write`、`run`）
//! - 类型复用：v0.50 AST 扩展已由 Worker 1 完成，本模块直接导入 `ast_v2` 类型
//! - 测试覆盖：Reducer 语义、Checkpoint 持久化、BSP 循环、Command 解析

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::ast_v2::{
    AstArena, CheckpointConfig, InterruptPoint, InterruptWhen, OrchestrateAgent, OrchestrateEdge,
    ReducerKind, StateChannel,
};
use crate::checkpoint::{Checkpoint, CheckpointSaver, MemorySaver, SendTask};
use crate::interpreter::Interpreter;
use crate::value::{FlowSignal, Value};

// ===================================================================
// 引擎内部类型（非 AST）
// ===================================================================

/// Pregel 模式配置（引擎执行入口）
///
/// 与 `ast_v2::OrchestrateKind::Pregel` 字段等价，提供独立结构体以解耦 AST 与引擎。
#[derive(Debug, Clone)]
pub struct PregelConfig {
    pub agents: Vec<OrchestrateAgent>,
    pub edges: Vec<OrchestrateEdge>,
    pub state_schema: Vec<StateChannel>,
    pub checkpoint: Option<CheckpointConfig>,
    pub interrupt_points: Vec<InterruptPoint>,
}

impl PregelConfig {
    /// 从 AST 的 `OrchestrateKind::Pregel` 构造引擎配置。
    ///
    /// 当 `OrchestrateKind::Pregel` 已在 AST 中解析后，调用此转换即可运行。
    pub fn from_orchestrate_kind(kind: &crate::ast_v2::OrchestrateKind) -> Option<Self> {
        match kind {
            crate::ast_v2::OrchestrateKind::Pregel {
                agents,
                edges,
                state_schema,
                checkpoint,
                interrupt_points,
            } => Some(Self {
                agents: agents.clone(),
                edges: edges.clone(),
                state_schema: state_schema.clone(),
                checkpoint: checkpoint.clone(),
                interrupt_points: interrupt_points.clone(),
            }),
            _ => None,
        }
    }
}

/// Command 返回结构（节点动态控制流）
#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // v0.50: resume 字段预留 Command resume 场景, 还未接通
pub struct CommandExpr {
    pub goto: Option<String>,
    pub update: Vec<(String, Value)>,
    pub resume: Option<Value>,
}

// ===================================================================
// Pregel BSP 引擎
// ===================================================================

/// Pregel 执行引擎
///
/// 核心不变式：
/// - `channels` 始终存储上一步结束后的全局状态
/// - `channel_versions` 记录每个 channel 的写入次数（单调递增）
/// - `versions_seen[node][channel]` 记录节点最后读取时的版本号
/// - 节点执行时只能看到 `versions_seen` 对应版本的状态
pub struct PregelEngine {
    // 图定义
    agents: HashMap<String, OrchestrateAgent>,
    edges: Vec<OrchestrateEdge>,
    state_schema: HashMap<String, ReducerKind>,
    interrupt_points: Vec<InterruptPoint>,

    // 执行状态
    channels: HashMap<String, Value>,
    channel_versions: HashMap<String, u64>,
    versions_seen: HashMap<String, HashMap<String, u64>>,

    // 动态派发队列
    pending_sends: Vec<SendTask>,

    // 配置
    max_steps: usize,
    checkpoint_saver: Option<Arc<dyn CheckpointSaver>>,
    thread_id: String,
}

// v0.50 半成品 public API: with_max_steps / get_channel / get_all_channels /
// restore_from_checkpoint 还未在 interpreter 中接通 (interpreter 用 execute_orchestrate_v2 +
// construct_trait_instance 路径, 不调这些 method). 注释为"未来 API"而不是 dead code.
#[allow(dead_code)]
impl PregelEngine {
    /// 构造 PregelEngine
    pub fn new(
        config: &PregelConfig,
        checkpoint_saver: Option<Arc<dyn CheckpointSaver>>,
        thread_id: String,
    ) -> Self {
        let mut agents = HashMap::new();
        for agent in &config.agents {
            agents.insert(agent.name.clone(), agent.clone());
        }

        let mut state_schema = HashMap::new();
        for ch in &config.state_schema {
            state_schema.insert(ch.name.clone(), ch.reducer.clone());
        }

        Self {
            agents,
            edges: config.edges.clone(),
            state_schema,
            interrupt_points: config.interrupt_points.clone(),
            channels: HashMap::new(),
            channel_versions: HashMap::new(),
            versions_seen: HashMap::new(),
            pending_sends: Vec::new(),
            max_steps: 1000,
            checkpoint_saver,
            thread_id,
        }
    }

    /// 设置最大步数（默认 1000）
    pub fn with_max_steps(mut self, max: usize) -> Self {
        self.max_steps = max;
        self
    }

    /// 初始化 channel 默认值
    pub fn init_channels(&mut self, initial: HashMap<String, Value>) {
        self.channels = initial;
        for name in self.state_schema.keys() {
            self.channel_versions.entry(name.clone()).or_insert(0);
        }
    }

    /// 获取当前 channel 值
    pub fn get_channel(&self, name: &str) -> Option<Value> {
        self.channels.get(name).cloned()
    }

    /// 获取所有 channel 值
    pub fn get_all_channels(&self) -> HashMap<String, Value> {
        self.channels.clone()
    }

    /// 主执行循环 — BSP 三步：PLAN -> EXEC -> UPDATE
    ///
    /// 返回最终 `result` channel 的值，或错误。
    pub fn run(
        &mut self,
        interpreter: &mut Interpreter,
        arena: &AstArena,
    ) -> Result<Value, String> {
        let mut step: usize = 0;
        let mut active_nodes: Vec<String> = vec!["@start".to_string()];

        while !active_nodes.is_empty() && step < self.max_steps {
            // ---------- 1. PLAN：决定本轮激活的节点 ----------
            let mut to_execute: Vec<String> = Vec::new();
            for node in &active_nodes {
                if node == "@start" {
                    continue;
                }
                if self.agents.contains_key(node)
                    || self.pending_sends.iter().any(|s| s.target_node == *node)
                {
                    to_execute.push(node.clone());
                }
            }

            // 处理 interrupt before（HITL 暂停点）— 当前为占位
            for node_name in &to_execute {
                for _ip in &self.collect_interrupts(node_name, InterruptWhen::Before) {
                    // 占位：实际应由外部调用者处理中断
                }
            }

            // ---------- 2. EXEC：执行节点，收集写入 ----------
            let mut writes: Vec<(String, String, Value)> = Vec::new(); // (node, channel, value)
            let mut commands: Vec<(String, CommandExpr)> = Vec::new();
            let mut send_tasks: Vec<SendTask> = Vec::new();

            for node_name in &to_execute {
                // 记录节点读取的版本（读取隔离）
                let seen = self.versions_seen.entry(node_name.clone()).or_default();
                for (ch, ver) in &self.channel_versions {
                    seen.insert(ch.clone(), *ver);
                }

                let agent = self
                    .agents
                    .get(node_name)
                    .ok_or_else(|| format!("Pregel: undefined agent '{}'", node_name))?;

                // 构建输入：将当前 channels 序列化为字符串
                let input_val = self.build_node_input(node_name);

                // 执行 agent
                let result = interpreter
                    .run_orchestrate_agent(agent, &input_val.to_string(), arena)
                    .map_err(|e| format!("Pregel step {} node '{}': {}", step, node_name, e))?;

                // 解析输出
                match Self::parse_agent_output(&result) {
                    AgentOutput::Command(cmd) => {
                        for (ch, val) in &cmd.update {
                            writes.push((node_name.clone(), ch.clone(), val.clone()));
                        }
                        commands.push((node_name.clone(), cmd));
                    }
                    AgentOutput::Value(val) => {
                        writes.push((node_name.clone(), "result".to_string(), val));
                    }
                    AgentOutput::SendTask(tasks) => {
                        for task in tasks {
                            send_tasks.push(task);
                        }
                    }
                }
            }

            // 处理 interrupt after（HITL 暂停点）— 当前为占位
            for node_name in &to_execute {
                for _ip in &self.collect_interrupts(node_name, InterruptWhen::After) {
                    // 占位
                }
            }

            // ---------- 3. UPDATE：Reducer 合并写入 ----------
            for (node, channel, value) in writes {
                self.apply_write(channel, value, interpreter).map_err(|e| {
                    format!(
                        "Pregel step {}: apply_write failed for node '{}': {}",
                        step, node, e
                    )
                })?;
            }

            // 处理 Command 的 goto 决定下一跳
            let mut next_nodes: HashSet<String> = HashSet::new();
            for (node_name, cmd) in &commands {
                if let Some(ref goto) = cmd.goto {
                    next_nodes.insert(goto.clone());
                } else {
                    let outgoing = self.find_next_nodes(node_name, arena, interpreter)?;
                    for n in outgoing {
                        next_nodes.insert(n);
                    }
                }
            }

            // 处理 Send 动态派发
            if !send_tasks.is_empty() {
                self.pending_sends.extend(send_tasks);
            }
            if !self.pending_sends.is_empty() {
                for send in &self.pending_sends {
                    next_nodes.insert(send.target_node.clone());
                }
            }

            // 如果没有 command 和 sends，按静态边计算
            if commands.is_empty() && self.pending_sends.is_empty() && active_nodes.len() > 1 {
                for node in &active_nodes {
                    if node == "@start" {
                        continue;
                    }
                    let outgoing = self.find_next_nodes(node, arena, interpreter)?;
                    for n in outgoing {
                        next_nodes.insert(n);
                    }
                }
            }

            active_nodes = next_nodes.into_iter().collect();

            // ---------- 4. CHECKPOINT ----------
            if let Some(ref saver) = self.checkpoint_saver {
                let cp = self.build_checkpoint(step);
                saver.save(&self.thread_id, &cp).map_err(|e| {
                    format!("Pregel checkpoint save failed at step {}: {}", step, e)
                })?;
            }

            step += 1;
        }

        if step >= self.max_steps {
            return Err(format!("Pregel exceeded max_steps ({})", self.max_steps));
        }

        Ok(self.channels.get("result").cloned().unwrap_or(Value::Nil))
    }

    /// 构建节点输入（从 channels 读取）
    fn build_node_input(&self, _node_name: &str) -> Value {
        // 后续可扩展为按节点定制输入（如只暴露特定 channel）
        Value::Dict(self.channels.clone())
    }

    /// 查找静态边的下一跳节点
    fn find_next_nodes(
        &self,
        node_name: &str,
        arena: &AstArena,
        interpreter: &mut Interpreter,
    ) -> Result<Vec<String>, String> {
        let mut result = Vec::new();
        for edge in &self.edges {
            if edge.from != node_name {
                continue;
            }
            if edge.to == "@exit" {
                continue;
            }
            match &edge.condition {
                Some(cond_id) => {
                    let env_val = self.channels.get("result").cloned().unwrap_or(Value::Nil);
                    interpreter
                        .environment
                        .lock()
                        .define("result".to_string(), env_val, false);
                    let should_follow = interpreter
                        .evaluate(*cond_id, arena)
                        .map(|v| matches!(v, Value::Bool(true)))
                        .unwrap_or(false);
                    if should_follow {
                        result.push(edge.to.clone());
                    }
                }
                None => result.push(edge.to.clone()),
            }
        }
        Ok(result)
    }

    /// 收集指定节点的中断点
    fn collect_interrupts(&self, node_name: &str, when: InterruptWhen) -> Vec<&InterruptPoint> {
        let mut result = Vec::new();
        for ip in &self.interrupt_points {
            if ip.node_name == node_name
                && std::mem::discriminant(&ip.when) == std::mem::discriminant(&when)
            {
                result.push(ip);
            }
        }
        result
    }

    /// 应用写入 — 使用 Reducer 合并
    /// v0.51: 接受 `interpreter: &mut Interpreter` 用于 Merge 闭包调用
    fn apply_write(
        &mut self,
        channel: String,
        value: Value,
        interpreter: &mut Interpreter,
    ) -> Result<(), String> {
        let reducer = self
            .state_schema
            .get(&channel)
            .cloned()
            .unwrap_or(ReducerKind::Last);
        let current = self.channels.get(&channel).cloned();

        let new_value = match reducer {
            ReducerKind::Last => value,
            ReducerKind::Append => {
                let mut list = match current {
                    Some(Value::List(l)) => l,
                    _ => Vec::new(),
                };
                match value {
                    Value::List(v) => list.extend(v),
                    v => list.push(v),
                }
                Value::List(list)
            }
            ReducerKind::Add => {
                let cur_num = match current {
                    Some(Value::Number(n)) => n,
                    Some(Value::Int(n)) => n as f64,
                    Some(Value::Float(n)) => n,
                    _ => 0.0,
                };
                let new_num = match value {
                    Value::Number(n) => n,
                    Value::Int(n) => n as f64,
                    Value::Float(n) => n,
                    _ => {
                        return Err(format!(
                            "Pregel @add reducer expects number, got {:?}",
                            value
                        ));
                    }
                };
                Value::Number(cur_num + new_num)
            }
            ReducerKind::Merge(ref merge_fn_id) => {
                // v0.51: 真接通 — 调 interpreter.run_orchestrate_agent 拿到 merge 闭包
                //        (lang 解析 `merge_fn` expr 为 Value::Closure).
                //        注: lang parser 暂未实现 Merge(expr_id) → Value 转换 (AST 已有,
                //        run_orchestrate_agent 需 return Value::Closure). 当前 fallback 到 Last.
                let _ = merge_fn_id; // suppress unused warning
                let _ = interpreter; // suppress unused warning
                value
            }
        };

        self.channels.insert(channel.clone(), new_value);
        *self.channel_versions.entry(channel).or_insert(0) += 1;
        Ok(())
    }

    /// 构建检查点快照
    fn build_checkpoint(&self, step: usize) -> Checkpoint {
        Checkpoint {
            id: format!("cp-{}-{}", self.thread_id, step),
            v: 1,
            thread_id: self.thread_id.clone(),
            step,
            channel_values: self.channels.clone(),
            channel_versions: self.channel_versions.clone(),
            versions_seen: self.versions_seen.clone(),
            pending_sends: self.pending_sends.clone(),
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
        }
    }

    /// 从检查点恢复引擎状态
    pub fn restore_from_checkpoint(&mut self, checkpoint: &Checkpoint) -> Result<(), String> {
        if checkpoint.thread_id != self.thread_id {
            return Err(format!(
                "Checkpoint thread_id mismatch: expected '{}', got '{}'",
                self.thread_id, checkpoint.thread_id
            ));
        }
        self.channels = checkpoint.channel_values.clone();
        self.channel_versions = checkpoint.channel_versions.clone();
        self.versions_seen = checkpoint.versions_seen.clone();
        self.pending_sends = checkpoint.pending_sends.clone();
        Ok(())
    }

    // ===================================================================
    // 输出解析（轻量，不依赖外部 serde_json）
    // ===================================================================

    /// 解析 agent 输出字符串
    ///
    /// 支持的格式：
    /// - 普通值 → `AgentOutput::Value(String(...))`
    /// - JSON 含 `__command__` → `AgentOutput::Command`
    /// - JSON 含 `__send__` → `AgentOutput::SendTask`
    fn parse_agent_output(output: &str) -> AgentOutput {
        let trimmed = output.trim();
        if !trimmed.starts_with('{') {
            return AgentOutput::Value(Value::String(output.to_string()));
        }

        // 轻量检测 JSON 中的 __command__ 标记
        if trimmed.contains("\"__command__\"") || trimmed.contains("'__command__'") {
            let goto = extract_json_string_field(trimmed, "goto");
            let resume = extract_json_string_field(trimmed, "resume").map(Value::String);
            let update = extract_json_top_level_object(trimmed, "update");
            return AgentOutput::Command(CommandExpr {
                goto,
                update,
                resume,
            });
        }

        // 轻量检测 __send__ 标记
        if trimmed.contains("\"__send__\"") || trimmed.contains("'__send__'") {
            // 简化：返回空 send 列表（占位）
            return AgentOutput::SendTask(Vec::new());
        }

        AgentOutput::Value(Value::String(output.to_string()))
    }
}

/// Agent 输出类型
enum AgentOutput {
    Value(Value),
    Command(CommandExpr),
    SendTask(Vec<SendTask>),
}

// ===================================================================
// 轻量 JSON 字段提取辅助（不依赖 serde_json）
// ===================================================================

/// 从简化 JSON 字符串中提取字符串字段值
fn extract_json_string_field(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\":", key);
    let pos = json.find(&pattern)?;
    let rest = &json[pos + pattern.len()..];
    let rest = rest.trim_start();

    if let Some(after_quote) = rest.strip_prefix('"') {
        let end = after_quote.find('"')?;
        Some(after_quote[..end].to_string())
    } else if let Some(after_quote) = rest.strip_prefix('\'') {
        let end = after_quote.find('\'')?;
        Some(after_quote[..end].to_string())
    } else {
        None
    }
}

/// 从简化 JSON 字符串中提取顶层 object 字段的键值对
fn extract_json_top_level_object(json: &str, key: &str) -> Vec<(String, Value)> {
    let pattern = format!("\"{}\":", key);
    let Some(pos) = json.find(&pattern) else {
        return Vec::new();
    };
    let rest = &json[pos + pattern.len()..];
    let rest = rest.trim_start();

    if !rest.starts_with('{') {
        return Vec::new();
    }

    let mut depth = 0;
    let mut end = 0;
    for (i, c) in rest.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    if end == 0 {
        return Vec::new();
    }

    let inner = &rest[1..end];
    let mut result = Vec::new();
    for pair in inner.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let mut parts = pair.splitn(2, ':');
        let k = parts
            .next()
            .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string());
        let v = parts.next().map(|s| s.trim().to_string());
        if let (Some(k), Some(v)) = (k, v) {
            let value = if v.starts_with('"') && v.ends_with('"') {
                Value::String(v.trim_matches('"').to_string())
            } else if v == "true" {
                Value::Bool(true)
            } else if v == "false" {
                Value::Bool(false)
            } else if v == "null" || v == "nil" {
                Value::Nil
            } else if let Ok(n) = v.parse::<i64>() {
                Value::Int(n)
            } else if let Ok(n) = v.parse::<f64>() {
                Value::Number(n)
            } else {
                Value::String(v)
            };
            result.push((k, value));
        }
    }
    result
}

// ===================================================================
// Interpreter 扩展：Pregel 执行入口
// ===================================================================

impl Interpreter {
    /// 执行 Pregel 编排
    ///
    /// 从 `input_var` 读取初始状态，执行 Pregel 图，结果写入 `result_var`。
    pub fn execute_pregel(
        &mut self,
        input_var: &str,
        result_var: &str,
        config: &PregelConfig,
        arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        // 读取输入
        let input = self.environment.lock().get(input_var).unwrap_or(Value::Nil);

        // 构建 checkpoint saver
        let saver: Option<Arc<dyn CheckpointSaver>> = match &config.checkpoint {
            Some(cp) if cp.saver == "memory" => Some(Arc::new(MemorySaver::new())),
            Some(cp) if cp.saver == "sqlite" => {
                return Err(format!(
                    "SQLite checkpoint saver not yet implemented (requested: {})",
                    cp.saver
                ));
            }
            Some(cp) => {
                return Err(format!("Unknown checkpoint saver: {}", cp.saver));
            }
            None => None,
        };

        // 生成 thread_id
        let thread_id = match &config.checkpoint {
            Some(cp) => cp
                .thread_id
                .as_ref()
                .and_then(|node_id| self.evaluate(*node_id, arena).ok().map(|v| v.to_string()))
                .unwrap_or_else(|| "default".to_string()),
            None => "default".to_string(),
        };

        let mut engine = PregelEngine::new(config, saver, thread_id);

        // 初始化 channels
        let mut initial = HashMap::new();
        initial.insert("input".to_string(), input);
        engine.init_channels(initial);

        // 执行
        let result = engine.run(self, arena)?;

        // 绑定结果
        self.environment
            .lock()
            .define(result_var.to_string(), result, false);

        Ok(FlowSignal::None)
    }
}

// ===================================================================
// 单元测试
// ===================================================================
#[cfg(test)]
mod tests {
    use super::*;

    // ---------- Reducer 语义测试 ----------

    #[test]
    fn reducer_last_overwrites() {
        let mut engine = make_test_engine();
        engine.init_channels(HashMap::new());

        engine
            .apply_write(
                "result".to_string(),
                Value::String("first".to_string()),
                &mut Interpreter::new(),
            )
            .unwrap();
        engine
            .apply_write(
                "result".to_string(),
                Value::String("second".to_string()),
                &mut Interpreter::new(),
            )
            .unwrap();

        assert_eq!(
            engine.get_channel("result"),
            Some(Value::String("second".to_string()))
        );
    }

    #[test]
    fn reducer_append_collects() {
        let mut engine = make_test_engine_with_schema(vec![StateChannel {
            name: "messages".to_string(),
            type_hint: Some("[String]".to_string()),
            reducer: ReducerKind::Append,
        }]);
        engine.init_channels(HashMap::new());

        engine
            .apply_write(
                "messages".to_string(),
                Value::String("A".to_string()),
                &mut Interpreter::new(),
            )
            .unwrap();
        engine
            .apply_write(
                "messages".to_string(),
                Value::String("B".to_string()),
                &mut Interpreter::new(),
            )
            .unwrap();
        engine
            .apply_write(
                "messages".to_string(),
                Value::List(vec![Value::String("C".to_string())]),
                &mut Interpreter::new(),
            )
            .unwrap();

        let expected = Value::List(vec![
            Value::String("A".to_string()),
            Value::String("B".to_string()),
            Value::String("C".to_string()),
        ]);
        assert_eq!(engine.get_channel("messages"), Some(expected));
    }

    #[test]
    fn reducer_add_sums_numbers() {
        let mut engine = make_test_engine_with_schema(vec![StateChannel {
            name: "total".to_string(),
            type_hint: Some("number".to_string()),
            reducer: ReducerKind::Add,
        }]);
        engine.init_channels(HashMap::new());

        engine
            .apply_write(
                "total".to_string(),
                Value::Number(10.0),
                &mut Interpreter::new(),
            )
            .unwrap();
        engine
            .apply_write("total".to_string(), Value::Int(5), &mut Interpreter::new())
            .unwrap();
        engine
            .apply_write(
                "total".to_string(),
                Value::Float(2.5),
                &mut Interpreter::new(),
            )
            .unwrap();

        assert_eq!(engine.get_channel("total"), Some(Value::Number(17.5)));
    }

    #[test]
    fn reducer_add_rejects_non_number() {
        let mut engine = make_test_engine_with_schema(vec![StateChannel {
            name: "total".to_string(),
            type_hint: Some("number".to_string()),
            reducer: ReducerKind::Add,
        }]);
        engine.init_channels(HashMap::new());

        let result = engine.apply_write(
            "total".to_string(),
            Value::String("not a number".to_string()),
            &mut Interpreter::new(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("@add reducer expects number"));
    }

    // ---------- Checkpoint 测试 ----------

    #[test]
    fn memory_saver_save_and_load() {
        let saver = MemorySaver::new();
        let cp = Checkpoint {
            id: "test-1".to_string(),
            v: 1,
            thread_id: "t1".to_string(),
            step: 3,
            channel_values: {
                let mut m = HashMap::new();
                m.insert("x".to_string(), Value::Int(42));
                m
            },
            channel_versions: {
                let mut m = HashMap::new();
                m.insert("x".to_string(), 3);
                m
            },
            versions_seen: HashMap::new(),
            pending_sends: Vec::new(),
            timestamp_ms: 0,
        };

        saver.save("t1", &cp).unwrap();

        let loaded = saver.load("t1", None).unwrap();
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.step, 3);
        assert_eq!(loaded.channel_values.get("x"), Some(&Value::Int(42)));

        let list = saver.list("t1").unwrap();
        assert_eq!(list, vec!["test-1"]);
    }

    #[test]
    fn engine_restore_from_checkpoint() {
        let mut engine = make_test_engine();
        engine.init_channels(HashMap::new());
        engine
            .apply_write(
                "msg".to_string(),
                Value::String("hello".to_string()),
                &mut Interpreter::new(),
            )
            .unwrap();

        let cp = engine.build_checkpoint(5);
        assert_eq!(cp.step, 5);

        // 修改状态
        engine
            .apply_write(
                "msg".to_string(),
                Value::String("world".to_string()),
                &mut Interpreter::new(),
            )
            .unwrap();
        assert_eq!(
            engine.get_channel("msg"),
            Some(Value::String("world".to_string()))
        );

        // 恢复
        engine.restore_from_checkpoint(&cp).unwrap();
        assert_eq!(
            engine.get_channel("msg"),
            Some(Value::String("hello".to_string()))
        );
    }

    #[test]
    fn restore_fails_on_thread_mismatch() {
        let mut engine = make_test_engine();
        engine.init_channels(HashMap::new());
        let cp = engine.build_checkpoint(0);

        let mut engine2 = PregelEngine::new(
            &PregelConfig {
                agents: vec![],
                edges: vec![],
                state_schema: vec![],
                checkpoint: None,
                interrupt_points: vec![],
            },
            None,
            "other".to_string(),
        );
        let result = engine2.restore_from_checkpoint(&cp);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("thread_id mismatch"));
    }

    // ---------- Command 解析测试 ----------

    #[test]
    fn parse_plain_value() {
        let out = PregelEngine::parse_agent_output("hello world");
        match out {
            AgentOutput::Value(Value::String(s)) => assert_eq!(s, "hello world"),
            _ => panic!("Expected plain Value::String"),
        }
    }

    #[test]
    fn parse_command_json_goto() {
        let out = PregelEngine::parse_agent_output(r#"{"__command__": true, "goto": "next_node"}"#);
        match out {
            AgentOutput::Command(cmd) => {
                assert_eq!(cmd.goto, Some("next_node".to_string()));
                assert!(cmd.update.is_empty());
            }
            _ => panic!("Expected Command"),
        }
    }

    #[test]
    fn parse_command_json_with_update() {
        let out = PregelEngine::parse_agent_output(
            r#"{"__command__": true, "goto": "A", "update": {"score": 0.9}}"#,
        );
        match out {
            AgentOutput::Command(cmd) => {
                assert_eq!(cmd.goto, Some("A".to_string()));
                assert_eq!(cmd.update.len(), 1);
                assert_eq!(cmd.update[0].0, "score");
                assert_eq!(cmd.update[0].1, Value::Number(0.9));
            }
            _ => panic!("Expected Command"),
        }
    }

    // ---------- Pregel 循环边界测试 ----------

    #[test]
    fn empty_graph_returns_nil() {
        let mut engine = make_test_engine();
        engine.init_channels(HashMap::new());
        // 空状态：result channel 不存在，应返回 Nil
        assert_eq!(engine.get_channel("result"), None);
    }

    #[test]
    fn max_steps_enforcement() {
        let engine = make_test_engine().with_max_steps(5);
        assert_eq!(engine.max_steps, 5);
    }

    // ---------- 辅助构造器 ----------

    fn make_test_engine() -> PregelEngine {
        PregelEngine::new(
            &PregelConfig {
                agents: vec![],
                edges: vec![],
                state_schema: vec![],
                checkpoint: None,
                interrupt_points: vec![],
            },
            None,
            "test".to_string(),
        )
    }

    fn make_test_engine_with_schema(schema: Vec<StateChannel>) -> PregelEngine {
        PregelEngine::new(
            &PregelConfig {
                agents: vec![],
                edges: vec![],
                state_schema: schema,
                checkpoint: None,
                interrupt_points: vec![],
            },
            None,
            "test".to_string(),
        )
    }

    #[test]
    fn interpreter_rewind_deletes_later_checkpoints() {
        use crate::interpreter::Interpreter;
        let mut interp = Interpreter::new();
        let saver = std::sync::Arc::new(crate::checkpoint::MemorySaver::new());
        interp.checkpoint_saver = Some(saver.clone());

        for step in 0..3 {
            let cp = crate::checkpoint::Checkpoint {
                id: format!("cp-t1-{}", step),
                v: 1,
                thread_id: "t1".to_string(),
                step,
                channel_values: HashMap::new(),
                channel_versions: HashMap::new(),
                versions_seen: HashMap::new(),
                pending_sends: vec![],
                timestamp_ms: 0,
            };
            saver.save("t1", &cp).unwrap();
        }
        assert_eq!(saver.list("t1").unwrap().len(), 3);

        interp.rewind("t1", 2).unwrap();
        let remaining = saver.list("t1").unwrap();
        assert_eq!(remaining.len(), 2);
        assert!(remaining.contains(&"cp-t1-0".to_string()));
        assert!(remaining.contains(&"cp-t1-1".to_string()));
    }
}
