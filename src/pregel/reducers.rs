//! v0.67–v0.75.10: Pregel reducer helpers + node input serialization.
//!
//! Extracted from `pregel/mod.rs` so reducer logic and node-input construction
//! are independently testable and no longer live inside the engine impl block.

use std::collections::HashMap;

use crate::value::{MergeStrategy, Value};

// ─── Reducer helpers ─────────────────────────────────────────────────

/// v0.67: Numeric accumulator for Sum/Product. First write initializes
/// to the op identity (`0` for `+`, `1` for `*`). Subsequent writes fold.
pub fn accumulator_reduce(
    current: Option<Value>,
    incoming: Value,
    op: &str,
) -> Result<Value, String> {
    let identity = match op {
        "+" => Value::Int(0),
        "*" => Value::Int(1),
        _ => return Err(format!("Unknown accumulator op: {}", op)),
    };
    let cur = current.unwrap_or(identity);
    match op {
        "+" => crate::flow::eval_binary(cur, &crate::common::BinaryOp::Add, incoming)
            .map_err(|e| e.to_string()),
        "*" => crate::flow::eval_binary(cur, &crate::common::BinaryOp::Mul, incoming)
            .map_err(|e| e.to_string()),
        _ => Err(format!("Unknown accumulator op: {}", op)),
    }
}

/// v0.67: Concat reducer — append incoming string to current.
/// Non-string incoming values are stringified via Display.
pub fn concat_reduce(current: Option<Value>, incoming: Value) -> Result<Value, String> {
    let cur = match current {
        Some(Value::String(s)) => s,
        Some(v) => format!("{}", v),
        None => String::new(),
    };
    let inc = match incoming {
        Value::String(s) => s,
        v => format!("{}", v),
    };
    Ok(Value::String(cur + &inc))
}

/// v0.61: Build per-key merge strategies from state schema.
pub fn build_per_key_strategies(
    state_schema: &[crate::mir::orchestrate::MirStateChannel],
) -> HashMap<String, MergeStrategy> {
    let mut map = HashMap::new();
    for channel in state_schema {
        if let Some(strategy) = channel.reducer.to_merge_strategy() {
            map.insert(channel.name.clone(), strategy);
        }
    }
    map
}

/// v0.67: Custom merge body helper.（自 `pregel::engine::custom_merge` 收编 ——
/// v0.94 删除未接线的平行引擎树，唯一被生产路径使用的 helper 移入本模块。）
///
/// Parses a `MirReducerKind::Custom` payload string into a `MirWitness` for
/// lowering.  Heuristic: integer → IntLit, anything else → Variable reference;
/// full custom bodies should be arbitrary Mora code.
pub fn parse_custom_merge_expr(s: &str) -> crate::mir::witness::MirWitness {
    use crate::mir::witness::{MirWitness, WitnessKind};
    let span = crate::common::Span::default();
    if let Ok(n) = s.parse::<i64>() {
        MirWitness {
            kind: WitnessKind::Literal(crate::common::Literal::Int(n, span)),
            span,
        }
    } else {
        MirWitness {
            kind: WitnessKind::Variable(s.to_string()),
            span,
        }
    }
}

// ─── Node input serialization ────────────────────────────────────────

/// v0.57: Serialize a `Value` to a JSON string fragment (for `build_node_input`).
pub fn value_to_json_string(v: &Value) -> String {
    match v {
        Value::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Value::Int(n) => format!("{}", n),
        Value::Float(n) => format!("{}", n),
        Value::Bool(b) => format!("{}", b),
        Value::Nil => "null".to_string(),
        Value::List(items) => {
            let parts: Vec<String> = items.iter().map(value_to_json_string).collect();
            format!("[{}]", parts.join(","))
        }
        _ => format!("\"{}\"", v),
    }
}
