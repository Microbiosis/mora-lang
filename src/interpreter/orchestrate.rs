//! v0.25: Multi-Agent 协调模式执行器
//!
//! 从 interpreter/mod.rs 提取的 orchestrate 相关方法：
//! - execute_orchestrate: 执行 orchestrate 块（Sequential/Graph/Loop）
//! - run_orchestrate_agent: 执行单个 orchestrate agent

use super::*;
use crate::ast_v2::{AstArena, OrchestrateAgent, OrchestrateKind};
use crate::value::{FlowSignal, Value};

impl Interpreter {
    /// v0.25: 执行 orchestrate 块
    pub fn execute_orchestrate(
        &mut self,
        input_var: &str,
        result_var: &str,
        kind: &OrchestrateKind,
        arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        let input = self
            .environment
            .lock()
            .map_err(|_| "env mutex poisoned".to_string())?
            .get(input_var)
            .map(|v| v.to_string())
            .unwrap_or_default();

        match kind {
            OrchestrateKind::Sequential { agents } => {
                let mut current = input;
                for agent in agents {
                    current = self.run_orchestrate_agent(agent, &current, arena)?;
                }
                self.environment
                    .lock()
                    .map_err(|_| "env mutex poisoned".to_string())?
                    .define(result_var.to_string(), Value::String(current), false);
            }
            OrchestrateKind::Graph { agents, edges } => {
                let mut current = input;
                let mut current_node = "@start".to_string();
                let mut step: usize = 0;
                let mut rounds_map: std::collections::HashMap<(String, String), usize> =
                    std::collections::HashMap::new();

                loop {
                    if step > 100 {
                        return Err("Orchestrate exceeded 100 steps".to_string());
                    }

                    let next_edge = edges.iter().find(|e| {
                        if e.from != current_node {
                            return false;
                        }
                        match &e.condition {
                            Some(cond_id) => {
                                if let Ok(mut env) = self.environment.lock() {
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

                self.environment
                    .lock()
                    .map_err(|_| "env mutex poisoned".to_string())?
                    .define(result_var.to_string(), Value::String(current), false);
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
                        self.environment
                            .lock()
                            .map_err(|_| "env mutex poisoned".to_string())?
                            .define("result".to_string(), Value::String(current.clone()), false);
                        let should_exit = self
                            .evaluate(*cond_id, arena)
                            .map(|v| matches!(v, Value::Bool(true)))
                            .unwrap_or(false);
                        if should_exit {
                            break;
                        }
                    }
                }

                self.environment
                    .lock()
                    .map_err(|_| "env mutex poisoned".to_string())?
                    .define(result_var.to_string(), Value::String(current), false);
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
        self.environment
            .lock()
            .map_err(|_| "env mutex poisoned".to_string())?
            .define("input".to_string(), Value::String(input.to_string()), false);

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
                self.environment
                    .lock()
                    .map_err(|_| "env mutex poisoned".to_string())?
                    .define("result".to_string(), Value::String(output.clone()), false);
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
                self.environment
                    .lock()
                    .map_err(|_| "env mutex poisoned".to_string())?
                    .define("input".to_string(), Value::String(output.clone()), false);
                let retry = self.evaluate(agent.task_expr, arena)?;
                output = retry.to_string();
            }
        }

        // 恢复配置
        self.current_ai_config = prev_config;

        Ok(output)
    }
}
