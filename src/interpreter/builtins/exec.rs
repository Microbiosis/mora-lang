//! v0.75.51: exec.* builtin 实现 — 从 builtins/mod.rs 拆出（P7，
//! Rhai register_plugin/Koto workspace 思想：按 domain 拆分，mod.rs 仅
//! 聚合）。方法语义与拆分前完全一致。

use super::*;
use crate::value::Value;

impl Interpreter {
    pub fn call_exec_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            "parallel" => exec_parallel(args),
            _ => Err(format!("exec.{}: unknown method", method)),
        }
    }
}
