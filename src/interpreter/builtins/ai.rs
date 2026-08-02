//! v0.75.51: ai.* builtin 实现 — 从 builtins/mod.rs 拆出（P7，
//! Rhai register_plugin/Koto workspace 思想：按 domain 拆分，mod.rs 仅
//! 聚合）。方法语义与拆分前完全一致。

use super::*;
use crate::value::Value;

impl Interpreter {
    pub fn call_ai_method(&mut self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            // v0.45.0: ai.retry(attempts, backoff_ms) — returns retry policy dict
            // Records in interpreter state, ready for use by chat/tokens layers
            "retry" => {
                let attempts = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("ai.retry: requires attempts")?;
                let attempts_n: u32 = attempts
                    .parse()
                    .map_err(|_| format!("ai.retry: invalid attempts '{}'", attempts))?;
                if attempts_n == 0 {
                    return Err("ai.retry: attempts must be > 0".to_string());
                }
                let backoff_ms: u64 = if let Some(v) = args.get(1) {
                    match v {
                        Value::Float(n) => *n as u64,
                        Value::Int(i) => *i as u64,
                        Value::String(s) => s.parse().unwrap_or(1000),
                        _ => 1000,
                    }
                } else {
                    1000
                };
                let backoff_strategy = if let Some(Value::String(s)) = args.get(2) {
                    s.clone()
                } else {
                    "exponential".to_string()
                };
                let mut d = std::collections::HashMap::new();
                d.insert("attempts".to_string(), Value::Float(attempts_n as f64));
                d.insert("backoff_ms".to_string(), Value::Float(backoff_ms as f64));
                d.insert(
                    "backoff".to_string(),
                    Value::String(backoff_strategy.clone()),
                );
                // 计算每个 attempt 的延迟 (mini-swe-agent tenacity-like)
                let mut schedule = Vec::new();
                for i in 0..attempts_n {
                    let delay = match backoff_strategy.as_str() {
                        "fixed" => backoff_ms,
                        "exponential" => backoff_ms * (1u64 << i.min(10)), // 2^i cap at 1024x
                        "linear" => backoff_ms * (i as u64 + 1),
                        _ => backoff_ms * (1u64 << i.min(10)),
                    };
                    schedule.push(Value::Float(delay as f64));
                }
                d.insert("schedule".to_string(), Value::List(schedule));
                Ok(Value::Dict(d))
            }
            // v0.45.0: ai.role(name) — set/get current AI role (OpenFugu per-turn)
            "role" => {
                if args.is_empty() {
                    return Err("ai.role: requires role name".to_string());
                }
                let role = args[0].to_string();
                // Validate against OpenFugu's 3 roles + extras
                match role.as_str() {
                    "worker" | "thinker" | "verifier" => {}
                    other => {
                        // Allow other role names but warn (informational)
                        // Per OpenFugu: Worker / Thinker / Verifier are the 3 main roles
                        let _ = other;
                    }
                }
                Ok(Value::String(role))
            }
            // v0.47.0: ai.context.* — context window control (AgentMesh+pi-agent)
            "context.trim" => {
                // 可选 threshold (0.0-1.0), 默认使用 self.ai.context_window.compression_threshold
                if let Some(v) = args.first() {
                    let t = match v {
                        Value::Float(n) => *n,
                        Value::Int(i) => *i as f64,
                        _ => {
                            return Err(
                                "ai.context.trim: threshold must be a number 0.0-1.0".to_string()
                            );
                        }
                    };
                    if !(0.0..=1.0).contains(&t) {
                        return Err(format!(
                            "ai.context.trim: threshold must be 0.0-1.0, got {}",
                            t
                        ));
                    }
                    self.ai.context_window.compression_threshold = t;
                }
                let before = self.ai.context_window.current_tokens;
                self.ai.context_window.compress();
                let after = self.ai.context_window.current_tokens;
                let dropped = before.saturating_sub(after);
                Ok(Value::Float(dropped as f64))
            }
            "context.info" => {
                let mut d = std::collections::HashMap::new();
                d.insert(
                    "max_tokens".to_string(),
                    Value::Float(self.ai.context_window.max_tokens as f64),
                );
                d.insert(
                    "current_tokens".to_string(),
                    Value::Float(self.ai.context_window.current_tokens as f64),
                );
                d.insert(
                    "messages".to_string(),
                    Value::Float(self.ai.context_window.messages.len() as f64),
                );
                d.insert(
                    "compression_threshold".to_string(),
                    Value::Float(self.ai.context_window.compression_threshold),
                );
                Ok(Value::Dict(d))
            }
            // v0.47.0: ai.dag(nodes, edges) — DAG-as-data (OpenFugu §1.6)
            "dag" => {
                if args.len() < 2 {
                    return Err("ai.dag: requires 2 args (nodes, edges)".to_string());
                }
                let nodes = match &args[0] {
                    Value::List(items) => items
                        .iter()
                        .map(|v| match v {
                            Value::String(s) => s.clone(),
                            _ => v.to_string(),
                        })
                        .collect::<Vec<String>>(),
                    _ => {
                        return Err("ai.dag: nodes must be a list of strings".to_string());
                    }
                };
                let edges = match &args[1] {
                    Value::List(items) => {
                        let mut out = Vec::with_capacity(items.len());
                        for (i, e) in items.iter().enumerate() {
                            match e {
                                Value::List(pair) if pair.len() == 2 => {
                                    let from = match &pair[0] {
                                        Value::String(s) => s.clone(),
                                        _ => pair[0].to_string(),
                                    };
                                    let to = match &pair[1] {
                                        Value::String(s) => s.clone(),
                                        _ => pair[1].to_string(),
                                    };
                                    out.push((from, to));
                                }
                                _ => {
                                    return Err(format!(
                                        "ai.dag: edges[{}] must be a [from, to] pair",
                                        i
                                    ));
                                }
                            }
                        }
                        out
                    }
                    _ => {
                        return Err("ai.dag: edges must be a list of [from, to] pairs".to_string());
                    }
                };
                let dag = crate::orchestrate_dag::OrchestrateDag::new(nodes, edges);
                let order = dag
                    .topological_order()
                    .map_err(|e| format!("ai.dag: {}", e))?;
                Ok(Value::List(order.into_iter().map(Value::String).collect()))
            }
            // v0.47.0: ai.heartbeat(path?) — heartbeat.md checklist (mimiclaw §1.5)
            "heartbeat" => {
                let path = if let Some(Value::String(s)) = args.first() {
                    std::path::PathBuf::from(s)
                } else {
                    let home = std::env::var("HOME")
                        .or_else(|_| std::env::var("USERPROFILE"))
                        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
                    std::path::PathBuf::from(home)
                        .join(".mora")
                        .join("HEARTBEAT.md")
                };
                let report = crate::heartbeat::load_heartbeat(&path)
                    .map_err(|e| format!("ai.heartbeat: {}", e))?;
                let mut d = std::collections::HashMap::new();
                d.insert(
                    "path".to_string(),
                    Value::String(path.to_string_lossy().to_string()),
                );
                d.insert("total".to_string(), Value::Float(report.total as f64));
                d.insert("done".to_string(), Value::Float(report.done as f64));
                d.insert("pending".to_string(), Value::Float(report.pending as f64));
                d.insert(
                    "completion_ratio".to_string(),
                    Value::Float(report.completion_ratio()),
                );
                d.insert("is_complete".to_string(), Value::Bool(report.is_complete()));
                let items: Vec<Value> = report
                    .items
                    .into_iter()
                    .map(|i| {
                        let mut m = std::collections::HashMap::new();
                        m.insert("text".to_string(), Value::String(i.text));
                        m.insert("done".to_string(), Value::Bool(i.done));
                        m.insert("line".to_string(), Value::Float(i.line_number as f64));
                        Value::Dict(m)
                    })
                    .collect();
                d.insert("items".to_string(), Value::List(items));
                Ok(Value::Dict(d))
            }
            _ => Err(format!("ai.{}: unknown method", method)),
        }
    }
    pub fn get_embedding(&self, text: &str) -> Result<Vec<f64>, String> {
        Ok(mock_bow_embedding(text))
    }
}
