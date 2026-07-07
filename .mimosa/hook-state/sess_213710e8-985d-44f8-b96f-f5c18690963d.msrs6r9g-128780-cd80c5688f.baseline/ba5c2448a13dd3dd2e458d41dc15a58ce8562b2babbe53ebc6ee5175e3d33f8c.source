//! v0.75.54: event.* builtin 实现 — P7 拆 domain 后补全：bus 测试
//! (tests_v0431_memory_bus 的 bus.subscribe/publish 部分) 从 builtins/mod.rs 迁入。
//! 语义与拆分前完全一致（纯搬移）。

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

#[cfg(test)]
mod tests_v0431_bus {
    #![allow(unused_mut)]
    use super::*;
    use crate::value::Value;

    /// v0.43.1: bus.subscribe / bus.publish

    #[test]
    fn bus_subscribe_returns_token() {
        let mut interp = Interpreter::new();
        let token = interp
            .call_event_method(
                "subscribe",
                &[Value::String("agent.research.*".to_string())],
            )
            .expect("subscribe");
        // token 是 Number (pattern_count 1)
        match token {
            Value::Float(n) => assert_eq!(n, 1.0),
            other => panic!("expected Number, got: {:?}", other),
        }
    }

    #[test]
    fn bus_subscribe_validates_pattern() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_event_method("subscribe", &[Value::Float(42.0)])
            .expect_err("non-string pattern should fail");
        assert!(err.contains("pattern must be a string"), "got: {}", err);
    }

    #[test]
    fn bus_publish_returns_pattern_count() {
        let mut interp = Interpreter::new();
        // subscribe 2 个
        interp
            .call_event_method("subscribe", &[Value::String("ai.*".to_string())])
            .unwrap();
        interp
            .call_event_method("subscribe", &[Value::String("ai.chat.*".to_string())])
            .unwrap();
        // publish
        let count = interp
            .call_event_method(
                "publish",
                &[
                    Value::String("ai.chat.completed".to_string()),
                    Value::String("data".to_string()),
                ],
            )
            .expect("publish");
        // 返回 pattern_count (2)
        match count {
            Value::Float(n) => assert_eq!(n, 2.0),
            other => panic!("expected Number, got: {:?}", other),
        }
    }

    #[test]
    fn bus_publish_validates_topic() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_event_method("publish", &[Value::Float(42.0)])
            .expect_err("non-string topic should fail");
        assert!(err.contains("topic must be a string"), "got: {}", err);
    }

    #[test]
    fn bus_subscribe_then_publish_wildcard_match() {
        // end-to-end: subscribe "user.*", publish "user.created", 验证 pattern 进入订阅表
        let mut interp = Interpreter::new();
        interp
            .call_event_method("subscribe", &[Value::String("user.*".to_string())])
            .unwrap();
        // emit() 走通配符匹配 (v0.41.0 O(segments) 索引, 验证过)
        interp
            .call_event_method("emit", &[Value::String("user.created".to_string())])
            .unwrap();
        // pattern_count 应 = 1
        let count = interp.call_event_method("count", &[]).unwrap();
        assert_eq!(count, Value::Float(1.0));
    }

    #[test]
    fn bus_subscribe_uses_existing_pattern_matching() {
        // 验证 subscribe 用的就是 EventBus::on() (已经在 v0.41.0 + v0.41.1 测试覆盖)
        let mut interp = Interpreter::new();
        interp
            .call_event_method("subscribe", &[Value::String("exact.event".to_string())])
            .unwrap();
        interp
            .call_event_method("subscribe", &[Value::String("prefix.*".to_string())])
            .unwrap();
        // 两个 patterns
        let count = interp.call_event_method("count", &[]).unwrap();
        assert_eq!(count, Value::Float(2.0));
    }
}
