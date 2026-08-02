//! v0.75.51: mock.* builtin 实现 — 从 builtins/mod.rs 拆出（P7，
//! Rhai register_plugin/Koto workspace 思想：按 domain 拆分，mod.rs 仅
//! 聚合）。方法语义与拆分前完全一致。

use super::*;
use crate::value::Value;

impl Interpreter {
    pub fn call_mock_method(&mut self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            "register" => {
                let name = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => {
                        return Err("mock.register: name must be a string".to_string());
                    }
                    None => return Err("mock.register: requires name".to_string()),
                };
                let handler = args
                    .get(1)
                    .cloned()
                    .ok_or("mock.register: requires handler")?;
                self.registry
                    .mock_registry
                    .register(&name, crate::mock::MockHandler::Script(handler));
                Ok(Value::String(format!("mock.{} registered", name)))
            }
            "unregister" => {
                let name = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => {
                        return Err("mock.unregister: name must be a string".to_string());
                    }
                    None => return Err("mock.unregister: requires name".to_string()),
                };
                self.registry.mock_registry.unregister(&name);
                Ok(Value::Nil)
            }
            "call" => {
                let name = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => return Err("mock.call: name must be a string".to_string()),
                    None => return Err("mock.call: requires name".to_string()),
                };
                let call_args = args.get(1).cloned().unwrap_or(Value::Nil);
                match self.registry.mock_registry.get(&name) {
                    Some(crate::mock::MockHandler::Native(f)) => Ok(f(&call_args)),
                    Some(crate::mock::MockHandler::Script(closure)) => {
                        self.call_value(&closure, vec![call_args])
                    }
                    None => Ok(Value::Nil),
                }
            }
            "count" => Ok(Value::Float(self.registry.mock_registry.count() as f64)),
            "names" => {
                let names = self.registry.mock_registry.names();
                Ok(Value::List(names.into_iter().map(Value::String).collect()))
            }
            _ => Err(format!("mock.{}: unknown method", method)),
        }
    }
}
