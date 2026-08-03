//! v0.75.51: ai.tokens.* builtin 实现 — 从 builtins/mod.rs 拆出（P7，
//! Rhai register_plugin/Koto workspace 思想：按 domain 拆分，mod.rs 仅
//! 聚合）。方法语义与拆分前完全一致。

use super::*;
use crate::value::Value;

impl Interpreter {
    pub fn call_ai_tokens_method(&self, method: &str, _args: &[Value]) -> Result<Value, String> {
        match method {
            "input" => Ok(Value::Float(self.ai.token_usage.input as f64)),
            "output" => Ok(Value::Float(self.ai.token_usage.output as f64)),
            "total" => Ok(Value::Float(
                (self.ai.token_usage.input + self.ai.token_usage.output) as f64,
            )),
            "calls" => Ok(Value::Float(self.ai.token_usage.input as f64)),
            _ => Err(format!("ai.tokens.{}: unknown method", method)),
        }
    }
}
