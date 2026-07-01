//! v0.29: JsonSubCompressor + crush_json_core (Kneedle + 异常保留)
//!
//! Provides:
//! - `JsonSubCompressor`: `SubCompressor` trait impl that delegates to `crush_json_core`
//! - `crush_json_core`: headroom-style algorithm (30% head + 15% tail + 异常保留)
//! - `extract_constant_fields`: helper to surface fields common to every list item

use std::collections::HashMap;

use crate::compress::{CompressOptions, SubCompressor};
use crate::value::Value;

/// v0.29: `SubCompressor` trait impl for JSON-list content.
///
/// This is a thin wrapper around `crush_json_core`. The string-level
/// sniffing/parsing path is intentionally minimal for v0.29 (Task 5 will
/// improve the parser via `json.parse`); the unit tests in this module
/// construct `Value::List` directly in Rust and bypass the string parser.
#[derive(Debug)]
pub struct JsonSubCompressor;

impl SubCompressor for JsonSubCompressor {
    fn sniff(&self, content: &str) -> f32 {
        let trimmed = content.trim_start();
        if trimmed.starts_with('[') || trimmed.starts_with('{') {
            0.9
        } else {
            0.0
        }
    }

    fn compress(
        &self,
        content: &str,
        max_bytes: usize,
        options: &CompressOptions,
    ) -> Result<String, String> {
        // 解析为 Value; MVP 路径 (`parse_json_simple` 仍是 None stub in Task 1)
        let parsed = parse_json_string(content)?;
        // 粗略: 每项 ~200 bytes (与 spec §6.6 一致)
        let max_items = (max_bytes / 200).max(1);
        let crushed = crush_json_core(&parsed, max_items, &options.anomaly_keys)?;
        Ok(value_to_json_string(&crushed))
    }

    fn origin(&self) -> &'static str {
        "json"
    }
}

/// v0.29: 核心算法 — Kneedle + 异常保留 (MVP 实现)
///
/// 算法 (headroom-style):
///   1. N < min_threshold (5) 或 N ≤ max_items → 原样
///   2. 抽取常量字段 (所有项相同) — 暂记为 v0.30 输出元数据
///   3. 强制保留 anomaly_keys 字段对应的项
///   4. Kneedle-like 拐点检测 — MVP: 30% 头 + 15% 尾
///   5. 去重 + 排序 + 截断
///   6. 构造返回 `Value::List`
pub fn crush_json_core(
    input: &Value,
    max_items: usize,
    anomaly_keys: &[String],
) -> Result<Value, String> {
    // 1. 必须是 List
    let items = match input {
        Value::List(l) => l.clone(),
        other => {
            return Err(format!(
                "crush_json: expected list, got {}",
                value_type(other)
            ))
        }
    };

    // 空列表直接返回
    if items.is_empty() {
        return Ok(Value::List(vec![]));
    }

    // 2. 验证每个 item 是 Dict
    for (i, it) in items.iter().enumerate() {
        if !matches!(it, Value::Dict(_)) {
            return Err(format!(
                "crush_json: each item must be a dict (item {} is {})",
                i,
                value_type(it)
            ));
        }
    }

    // 3. N < min_threshold (5) 或 N ≤ max_items → 原样
    if items.len() <= 5 || items.len() <= max_items {
        return Ok(Value::List(items));
    }

    // 4. 抽出常量字段 (所有项相同) — MVP 不嵌入输出 (v0.30 改进)
    let constant_fields = extract_constant_fields(&items);
    // 抑制 unused 警告: constant_fields 留给 v0.30 输出 metadata
    let _ = &constant_fields;

    // 5. 标记 anomaly 项 (含 anomaly_keys 的项必须保留)
    let mut keep_indices: Vec<usize> = Vec::new();
    for (i, it) in items.iter().enumerate() {
        if let Value::Dict(d) = it {
            for k in anomaly_keys {
                if d.contains_key(k) {
                    keep_indices.push(i);
                    break;
                }
            }
        }
    }

    // 6. Kneedle-like 拐点检测 — MVP: 30% 头 + 15% 尾
    let n = items.len();
    let head_count = (n as f32 * 0.3) as usize;
    let tail_count = (n as f32 * 0.15) as usize;
    let target = max_items.saturating_sub(keep_indices.len()).max(1);
    let head_n = head_count.min(target / 2).min(n);
    let tail_n = tail_count
        .min(target - head_n)
        .min(n.saturating_sub(head_n));

    for i in 0..head_n {
        keep_indices.push(i);
    }
    for i in (n - tail_n)..n {
        keep_indices.push(i);
    }

    // 7. 去重 + 排序 + 截断
    keep_indices.sort_unstable();
    keep_indices.dedup();
    keep_indices.truncate(max_items);

    // 8. 构造返回 List
    let kept_items: Vec<Value> = keep_indices.iter().map(|&i| items[i].clone()).collect();
    Ok(Value::List(kept_items))
}

/// v0.29: 抽取所有项共有的字段 (字段名 + 值)。
///
/// 如果 items 为空 / 第一项不是 Dict / 任何项不是 Dict → 返回空 map。
pub fn extract_constant_fields(items: &[Value]) -> HashMap<String, Value> {
    if items.is_empty() {
        return HashMap::new();
    }
    let first = match &items[0] {
        Value::Dict(d) => d.clone(),
        _ => return HashMap::new(),
    };
    let mut constants = first;
    for it in &items[1..] {
        if let Value::Dict(d) = it {
            constants.retain(|k, v| d.get(k) == Some(v));
        } else {
            return HashMap::new();
        }
    }
    constants
}

// ── 本地 helpers (delegating 到 mod.rs 的 stub) ──────────────────────

fn parse_json_string(s: &str) -> Result<Value, String> {
    crate::compress::parse_json_simple(s)
        .ok_or_else(|| "crush_json: json.parse failed".to_string())
}

fn value_to_json_string(v: &Value) -> String {
    crate::compress::value_to_json_simple(v)
}

fn value_type(v: &Value) -> &'static str {
    crate::compress::value_type_simple(v)
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_items(n: usize) -> Vec<Value> {
        (0..n)
            .map(|i| {
                let mut d = HashMap::new();
                d.insert("id".to_string(), Value::Number(i as f64));
                d.insert("score".to_string(), Value::Number((i as f64) * 0.1));
                d.insert("category".to_string(), Value::String("A".into()));
                Value::Dict(d)
            })
            .collect()
    }

    #[test]
    fn test_crush_json_small_list_passthrough() {
        let items = make_items(3);
        let input = Value::List(items);
        let result = crush_json_core(&input, 10, &["error".into()]).unwrap();
        if let Value::List(l) = result {
            assert_eq!(l.len(), 3, "small list (≤5) should pass through");
        } else {
            panic!("expected List");
        }
    }

    #[test]
    fn test_crush_json_basic_compress_100_to_10() {
        let items = make_items(100);
        let input = Value::List(items);
        let result = crush_json_core(&input, 10, &["error".into()]).unwrap();
        if let Value::List(l) = result {
            assert_eq!(l.len(), 10, "should crush 100 items to 10");
        } else {
            panic!("expected List");
        }
    }

    #[test]
    fn test_crush_json_preserves_anomaly_items() {
        let mut items = make_items(50);
        // 第 25 项注入 error 字段
        if let Value::Dict(d) = &mut items[25] {
            d.insert("error".to_string(), Value::String("BOOM".into()));
        }
        let input = Value::List(items);
        let result = crush_json_core(&input, 5, &["error".into()]).unwrap();
        if let Value::List(l) = result {
            let has_anomaly = l.iter().any(|it| {
                if let Value::Dict(d) = it {
                    d.get("error").is_some()
                } else {
                    false
                }
            });
            assert!(has_anomaly, "anomaly item with error field must be preserved");
        } else {
            panic!("expected List");
        }
    }

    #[test]
    fn test_crush_json_extracts_constant_field() {
        let items = make_items(20);
        // 所有项都有 `category: "A"` 字段 — 这是常量
        let constants = extract_constant_fields(&items);
        assert!(
            constants.contains_key("category"),
            "extract_constant_fields should surface `category`"
        );
        assert_eq!(constants.get("category"), Some(&Value::String("A".into())));
    }

    #[test]
    fn test_crush_json_rejects_non_list() {
        let input = Value::String("not a list".into());
        let result = crush_json_core(&input, 10, &["error".into()]);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.starts_with("crush_json:"),
            "error should start with 'crush_json:' but got: {err}"
        );
    }
}