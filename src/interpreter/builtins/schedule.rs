//! v0.75.51: schedule.* builtin 实现 — 从 builtins/mod.rs 拆出（P7，
//! Rhai register_plugin/Koto workspace 思想：按 domain 拆分，mod.rs 仅
//! 聚合）。方法语义与拆分前完全一致。

use super::*;
use crate::value::Value;

impl Interpreter {
    pub fn call_schedule_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            "add" => {
                // v0.37 (P1-3.9): name/kind/message must all be Value::String.
                let name = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => return Err("schedule.add: name must be a string".to_string()),
                    None => return Err("schedule.add: requires name".to_string()),
                };
                let kind_str = match args.get(1) {
                    Some(Value::String(s)) => s.as_str(),
                    Some(_) => {
                        return Err("schedule.add: kind must be a string".to_string());
                    }
                    None => {
                        return Err("schedule.add: requires kind ('every' or 'at')".to_string());
                    }
                };
                let kind = match kind_str {
                    "every" => crate::schedule::JobKind::Every,
                    "at" => crate::schedule::JobKind::At,
                    _ => {
                        return Err(format!(
                            "schedule.add: kind must be 'every' or 'at', got '{}'",
                            kind_str
                        ));
                    }
                };
                let message = match args.get(2) {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => {
                        return Err("schedule.add: message must be a string".to_string());
                    }
                    None => return Err("schedule.add: requires message".to_string()),
                };
                let interval_s = if let Some(Value::Float(n)) = args.get(3) {
                    *n as u64
                } else {
                    0
                };
                let at_epoch = if let Some(Value::Float(n)) = args.get(4) {
                    *n as u64
                } else {
                    0
                };
                self.infra
                    .scheduler
                    .add(&name, kind, &message, interval_s, at_epoch)
                    .map(Value::String)
            }
            "list" => {
                let jobs = self.infra.scheduler.list();
                let arr: Vec<Value> = jobs
                    .into_iter()
                    .map(|j| {
                        let mut m = std::collections::HashMap::new();
                        m.insert("id".to_string(), Value::String(j.id));
                        m.insert("name".to_string(), Value::String(j.name));
                        m.insert(
                            "kind".to_string(),
                            Value::String(match j.kind {
                                crate::schedule::JobKind::Every => "every".to_string(),
                                crate::schedule::JobKind::At => "at".to_string(),
                            }),
                        );
                        m.insert("message".to_string(), Value::String(j.message));
                        m.insert("interval_s".to_string(), Value::Float(j.interval_s as f64));
                        m.insert("at_epoch".to_string(), Value::Float(j.at_epoch as f64));
                        Value::Dict(m)
                    })
                    .collect();
                Ok(Value::List(arr))
            }
            "remove" => {
                let id = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("schedule.remove: requires id")?;
                Ok(Value::Bool(self.infra.scheduler.remove(&id)))
            }
            "tick" => {
                let messages = self.infra.scheduler.tick(crate::schedule::Scheduler::now());
                Ok(Value::List(
                    messages.into_iter().map(Value::String).collect(),
                ))
            }
            "count" => Ok(Value::Float(self.infra.scheduler.count() as f64)),
            _ => Err(format!("schedule.{}: unknown method", method)),
        }
    }
}
