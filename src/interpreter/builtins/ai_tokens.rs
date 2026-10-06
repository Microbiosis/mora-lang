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
            // v0.104.6 D76：此前此处返回 `token_usage.input` —— 把**输入
            // token 数**当作**调用次数**报出去（`TokenUsage` 里当时压根没有
            // `calls` 字段，是复制粘贴留下的）。二者在真实调用下必然不同：
            // 一次调用往往带来几百个 input token，于是这个方法静默给出
            // 错误的数字。现读真正累加的 `calls`。
            "calls" => Ok(Value::Float(self.ai.token_usage.calls as f64)),
            _ => Err(format!("ai.tokens.{}: unknown method", method)),
        }
    }
}
