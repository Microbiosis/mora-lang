//! v0.75.51: event.* builtin 实现 — 从 builtins/mod.rs 拆出（P7，
//! Rhai register_plugin/Koto workspace 思想：按 domain 拆分，mod.rs 仅
//! 聚合）。方法语义与拆分前完全一致。

use super::*;
use crate::value::Value;

impl Interpreter {
    pub fn call_event_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            "emit" => {
                // v0.37 (P1-3.7): event name must be Value::String.
                let event = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => {
                        return Err("bus.emit: first arg must be a string event name".to_string());
                    }
                    None => return Err("bus.emit: requires event name as first arg".to_string()),
                };
                let payload = args.get(1).cloned().unwrap_or(Value::Nil);
                self.infra.bus.emit(&event, &payload);
                Ok(Value::Nil)
            }
            "off" => {
                // v0.37: pattern must be Value::String.
                let pattern = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => {
                        return Err("bus.off: first arg must be a string pattern".to_string());
                    }
                    None => return Err("bus.off: requires pattern as first arg".to_string()),
                };
                self.infra.bus.off(&pattern);
                Ok(Value::Nil)
            }
            "count" => Ok(Value::Float(self.infra.bus.pattern_count() as f64)),
            // v0.43.1: bus.subscribe(pattern) — pub-sub subscribe (Puter / AgentMesh / Solace)
            // Returns: token (Value::Float) for later unsubscribe
            // Note: handler is internal — actual mora-level callback support would
            // require lifting Fn closures to a sandboxed layer; for now subscribe()
            // registers the subscription slot, and publish() fires it.
            // (Future: integrate with Mora task scheduler)
            "subscribe" => {
                let pattern = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => return Err("bus.subscribe: pattern must be a string".to_string()),
                    None => return Err("bus.subscribe: requires pattern arg".to_string()),
                };
                // 注册一个 no-op handler 让 pattern 进入订阅表
                // 真实 handler 由上层 (LSP / HTTP / MCP) 通过更高级 API 提供
                // 这里用空 handler 占位, 返回 token = pattern_count (递增)
                self.infra.bus.on(
                    &pattern,
                    Arc::new(|_, _| {
                        // no-op: subscribe 占位
                    }),
                );
                let token = self.infra.bus.pattern_count() as u64;
                Ok(Value::Float(token as f64))
            }
            // v0.43.1: bus.publish(topic, payload) — pub-sub publish (Puter / AgentMesh verified)
            // Returns: Number of registered patterns (informational; actual fire via emit)
            "publish" => {
                let topic = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => return Err("bus.publish: topic must be a string".to_string()),
                    None => return Err("bus.publish: requires topic arg".to_string()),
                };
                let payload = args.get(1).cloned().unwrap_or(Value::Nil);
                // 直接走 EventBus::emit, 它已经支持通配符 (Puter O(segments) 索引, v0.41.0)
                self.infra.bus.emit(&topic, &payload);
                // 返回注册的 pattern 数 (informational)
                Ok(Value::Float(self.infra.bus.pattern_count() as f64))
            }
            _ => Err(format!("bus.{}: unknown method", method)),
        }
    }
}
