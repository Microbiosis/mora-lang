//! v0.25: Multi-Agent 协调模式执行器
//!
//! 从 interpreter/mod.rs 提取的 orchestrate 相关方法：
//! - execute_orchestrate: 执行 orchestrate 块（Sequential/Graph/Loop）
//! - run_orchestrate_agent: 执行单个 orchestrate agent

use super::*;
/// v0.49.0 (B4): orchestrate graph max steps (was hardcoded 100)
const MAX_GRAPH_STEPS: usize = 1000;

use crate::ast_v2::{AstArena, OrchestrateAgent, OrchestrateKind};
use crate::interpreter::orchestrate_v2::PregelEngine;
use crate::value::{FlowSignal, Value};

impl Interpreter {
    /// v0.50: 执行 Pregel orchestrate 块（通过 orchestrate_v2 引擎）
    #[allow(clippy::too_many_arguments)] // 9 字段对应 v0.50 orchestrate 语句的全部部分, 不能合并
    pub fn execute_orchestrate_v2(
        &mut self,
        input_var: &str,
        result_var: &str,
        agents: &[crate::ast_v2::OrchestrateAgent],
        edges: &[crate::ast_v2::OrchestrateEdge],
        state_schema: &[crate::ast_v2::StateChannel],
        checkpoint: &Option<crate::ast_v2::CheckpointConfig>,
        interrupt_points: &[crate::ast_v2::InterruptPoint],
        arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        use crate::interpreter::orchestrate_v2::PregelConfig;

        let input = self.environment.lock().get(input_var).unwrap_or(Value::Nil);

        let config = PregelConfig {
            agents: agents.to_vec(),
            edges: edges.to_vec(),
            state_schema: state_schema.to_vec(),
            checkpoint: checkpoint.clone(),
            interrupt_points: interrupt_points.to_vec(),
        };

        let saver: Option<std::sync::Arc<dyn crate::checkpoint::CheckpointSaver>> = match &config
            .checkpoint
        {
            Some(cp) if cp.saver == "memory" => {
                Some(std::sync::Arc::new(crate::checkpoint::MemorySaver::new()))
            }
            #[cfg(feature = "checkpoint-sqlite")]
            Some(cp) if cp.saver == "sqlite" => {
                // v0.51: SQLite saver 真接通. 默认路径: ./.mora/checkpoints.sqlite
                std::fs::create_dir_all(".mora").map_err(|e| e.to_string())?;
                Some(std::sync::Arc::new(crate::checkpoint::SqliteSaver::new(
                    ".mora/checkpoints.sqlite",
                )?))
            }
            #[cfg(not(feature = "checkpoint-sqlite"))]
            Some(cp) if cp.saver == "sqlite" => {
                return Err(format!(
                    "SQLite checkpoint saver requires 'checkpoint-sqlite' feature (requested: {})",
                    cp.saver
                ));
            }
            Some(cp) => return Err(format!("Unknown checkpoint saver: {}", cp.saver)),
            None => None,
        };

        let thread_id = match &config.checkpoint {
            Some(cp) => cp
                .thread_id
                .as_ref()
                .and_then(|node_id| self.evaluate(*node_id, arena).ok().map(|v| v.to_string()))
                .unwrap_or_else(|| "default".to_string()),
            None => "default".to_string(),
        };

        let mut engine = PregelEngine::new(&config, saver, thread_id);

        let mut initial = std::collections::HashMap::new();
        if !matches!(input, Value::Nil) {
            initial.insert("input".to_string(), input);
        }
        engine.init_channels(initial);

        let result = engine
            .run(self, arena)
            .map_err(|e| format!("Pregel error: {}", e))?;
        self.environment
            .lock()
            .define(result_var.to_string(), result, false);
        Ok(FlowSignal::None)
    }

    /// v0.25: 执行 orchestrate 块
    pub fn execute_orchestrate(
        &mut self,
        input_var: &str,
        result_var: &str,
        kind: &OrchestrateKind,
        arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        match kind {
            OrchestrateKind::Pregel {
                agents,
                edges,
                state_schema,
                checkpoint,
                interrupt_points,
            } => self.execute_orchestrate_v2(
                input_var,
                result_var,
                agents,
                edges,
                state_schema,
                checkpoint,
                interrupt_points,
                arena,
            ),
            OrchestrateKind::Sequential { .. }
            | OrchestrateKind::Graph { .. }
            | OrchestrateKind::Loop { .. } => {
                self.execute_orchestrate_v1(input_var, result_var, kind, arena)
            }
        }
    }

    /// v0.25: 执行经典 orchestrate 块（Sequential/Graph/Loop）
    fn execute_orchestrate_v1(
        &mut self,
        input_var: &str,
        result_var: &str,
        kind: &OrchestrateKind,
        arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        let input = self
            .environment
            .lock()
            .get(input_var)
            .map(|v| v.to_string())
            .unwrap_or_default();

        match kind {
            OrchestrateKind::Sequential { agents } => {
                let mut current = input;
                for agent in agents {
                    current = self.run_orchestrate_agent(agent, &current, arena)?;
                }
                self.environment.lock().define(
                    result_var.to_string(),
                    Value::String(current),
                    false,
                );
            }
            OrchestrateKind::Graph { agents, edges } => {
                let mut current = input;
                let mut current_node = "@start".to_string();
                let mut step: usize = 0;
                let mut rounds_map: std::collections::HashMap<(String, String), usize> =
                    std::collections::HashMap::new();

                loop {
                    if step > MAX_GRAPH_STEPS {
                        return Err(format!(
                            "Orchestrate graph exceeded {} steps (max: {})",
                            step, MAX_GRAPH_STEPS
                        ));
                    }

                    let next_edge = edges.iter().find(|e| {
                        if e.from != current_node {
                            return false;
                        }
                        match &e.condition {
                            Some(cond_id) => {
                                {
                                    let mut env = self.environment.lock();
                                    env.define(
                                        "result".to_string(),
                                        Value::String(current.clone()),
                                        false,
                                    );
                                    env.define(
                                        "rounds".to_string(),
                                        Value::Number(
                                            *rounds_map
                                                .get(&(e.from.clone(), e.to.clone()))
                                                .unwrap_or(&0)
                                                as f64,
                                        ),
                                        false,
                                    );
                                }
                                self.evaluate(*cond_id, arena)
                                    .map(|v| matches!(v, Value::Bool(true)))
                                    .unwrap_or(false)
                            }
                            None => true,
                        }
                    });

                    match next_edge {
                        None => break,
                        Some(edge) => {
                            if edge.to == "@exit" {
                                break;
                            }

                            let agent = agents
                                .iter()
                                .find(|a| a.name == edge.to)
                                .ok_or_else(|| format!("Undefined agent: {}", edge.to))?;

                            let key = (edge.from.clone(), edge.to.clone());
                            *rounds_map.entry(key).or_insert(0) += 1;

                            current = self.run_orchestrate_agent(agent, &current, arena)?;
                            current_node = edge.to.clone();
                            step += 1;
                        }
                    }
                }

                self.environment.lock().define(
                    result_var.to_string(),
                    Value::String(current),
                    false,
                );
            }
            OrchestrateKind::Loop {
                agent,
                max_rounds,
                exit_when,
            } => {
                let mut current = input;
                for _round in 0..*max_rounds {
                    current = self.run_orchestrate_agent(agent, &current, arena)?;

                    if let Some(cond_id) = exit_when {
                        self.environment.lock().define(
                            "result".to_string(),
                            Value::String(current.clone()),
                            false,
                        );
                        let should_exit = self
                            .evaluate(*cond_id, arena)
                            .map(|v| matches!(v, Value::Bool(true)))
                            .unwrap_or(false);
                        if should_exit {
                            break;
                        }
                    }
                }

                self.environment.lock().define(
                    result_var.to_string(),
                    Value::String(current),
                    false,
                );
            }
            OrchestrateKind::Pregel { .. } => {
                return Err("Pregel must be handled by execute_orchestrate, not v1".to_string());
            }
        }

        Ok(FlowSignal::None)
    }

    /// 执行单个 orchestrate agent
    pub fn run_orchestrate_agent(
        &mut self,
        agent: &OrchestrateAgent,
        input: &str,
        arena: &AstArena,
    ) -> Result<String, String> {
        // 绑定 input 变量
        self.environment.lock().define(
            "input".to_string(),
            Value::String(input.to_string()),
            false,
        );

        // 应用 with 配置
        let prev_config = self.current_ai_config.clone();
        if let Some(ref bindings) = agent.with_config {
            for (key, val_id) in bindings {
                let val = self.evaluate(*val_id, arena)?;
                match key.as_str() {
                    "model" => {
                        if let Value::String(m) = val {
                            self.current_ai_config = Some(AiConfigValue {
                                model: Some(m),
                                ..self.current_ai_config.clone().unwrap_or_default()
                            });
                        }
                    }
                    "temperature" => {
                        if let Value::Number(t) = val {
                            self.current_ai_config = Some(AiConfigValue {
                                temperature: Some(t),
                                ..self.current_ai_config.clone().unwrap_or_default()
                            });
                        }
                    }
                    _ => {}
                }
            }
        }

        // 执行 task 表达式
        let result = self.evaluate(agent.task_expr, arena)?;
        let mut output = result.to_string();

        // verify（如果有的话）
        if let Some(verify_id) = agent.verify_expr {
            for attempt in 0..3 {
                self.environment.lock().define(
                    "result".to_string(),
                    Value::String(output.clone()),
                    false,
                );
                let ok = self
                    .evaluate(verify_id, arena)
                    .map(|v| matches!(v, Value::Bool(true)))
                    .unwrap_or(false);
                if ok {
                    break;
                }
                if attempt == 2 {
                    return Err(format!(
                        "Agent '{}' verify failed after 3 attempts",
                        agent.name
                    ));
                }
                self.environment.lock().define(
                    "input".to_string(),
                    Value::String(output.clone()),
                    false,
                );
                let retry = self.evaluate(agent.task_expr, arena)?;
                output = retry.to_string();
            }
        }

        // 恢复配置
        self.current_ai_config = prev_config;

        Ok(output)
    }
}
