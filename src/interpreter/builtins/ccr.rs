//! v0.75.51: ccr.* builtin 实现 — 从 builtins/mod.rs 拆出（P7，
//! Rhai register_plugin/Koto workspace 思想：按 domain 拆分，mod.rs 仅
//! 聚合）。方法语义与拆分前完全一致。
//!
//! v0.75.54: 自包含 trait 导入（此前经 mod.rs 顶层 use 隐式继承）。

use super::*;
use crate::ccr::CcrStore;
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
                // v0.104.6 D150：此前只匹配 `Value::Float`，于是**合法的 `Int`**
                // （如 `len(...)`）被静默丢弃、size 变 0，exit 0 零诊断
                // （实测 `ccr.marker("abcdef", 8)` → `<<ccr:abcdef,8>>`，
                //  同样的 8 用 `len()` 传入 → `<<ccr:abcdef,0>>`）。
                //
                // ⚠ 负数尺寸**当前饱和成 0**（`as usize`），这是 **D339 明确记录的
                // 「不修、只钉现状 + 报告」**的产品契约决定：负尺寸该报错还是当 0，
                // 属产品契约。判据 `tests/tea_max_steps_guard.rs::
                // d339_ccr_marker_negative_size_still_becomes_zero_for_both_types`
                // 钉着它；`tests/ccr_marker_size_guard.rs`（D404）补齐了
                // 负值/非有限值/缺省/小数截断的完整矩阵与可达性分析。
                //
                // v0.104.6 D404：我一度把它改成「负数报错」（走
                // `flow::value_as_usize` 收口），**随后回退** ——
                // 理由见 CHANGELOG D404「一次真实的自我否决」。
                let size = optional_num_arg(args, 1, "ccr.marker", "size")?
                    .map(|n| n as usize)
                    .unwrap_or(0);
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
