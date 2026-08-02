//! v0.75.51: ccr.* builtin 实现 — 从 builtins/mod.rs 拆出（P7，
//! Rhai register_plugin/Koto workspace 思想：按 domain 拆分，mod.rs 仅
//! 聚合）。方法语义与拆分前完全一致。

use super::*;
use crate::value::Value;

impl Interpreter {
    pub fn call_ccr_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            "put" => {
                // v0.37 (P1-3.8): data must be Value::String. Avoids lossy
                // to_string() of List/Dict that would round-trip into "[...]".
                let data = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => {
                        return Err("ccr.put: data must be a string".to_string());
                    }
                    None => return Err("ccr.put: requires data as first arg".to_string()),
                };
                let hash = self.registry.ccr_store.put(&data);
                Ok(Value::String(hash))
            }
            "get" => {
                let hash = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(_) => {
                        return Err("ccr.get: hash must be a string".to_string());
                    }
                    None => return Err("ccr.get: requires hash as first arg".to_string()),
                };
                match self.registry.ccr_store.get(&hash) {
                    Some(entry) => Ok(Value::String(entry.data)),
                    None => Ok(Value::Nil),
                }
            }
            "len" => Ok(Value::Int(self.registry.ccr_store.len() as i64)),
            "marker" => {
                let hash = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("ccr.marker: requires hash as first arg")?;
                let size = if let Some(Value::Float(n)) = args.get(1) {
                    *n as usize
                } else {
                    0
                };
                Ok(Value::String(crate::ccr::make_marker(&hash, size)))
            }
            "extract" => {
                let marker = args
                    .first()
                    .map(|v| v.to_string())
                    .ok_or("ccr.extract: requires marker as first arg")?;
                match crate::ccr::extract_hash(&marker) {
                    Some(hash) => Ok(Value::String(hash.to_string())),
                    None => Err(format!("ccr.extract: not a valid CCR marker: '{}'", marker)),
                }
            }
            _ => Err(format!("ccr.{}: unknown method", method)),
        }
    }
}
