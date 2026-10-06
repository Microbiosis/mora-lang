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

// v0.104.6 D235：此处原有的 `pub fn value_to_json_string` 已**删除**。
//
// 它是 `pregel/mod.rs` 里同名函数的**重复实现**（D209 已记录「改一处须
// 同步另一处」的维护陷阱），且有两个 D235 缺陷：
//   ① 没有 `Dict` 分支 ⇒ dict 落到 `_ => format!("\"{}\"", v)`，
//      经 `Value::Display` 得 `{k: v}` —— **key 无引号，不是 JSON**；
//   ② `Float(42.0)` 输出 `42` ⇒ 往返变 `Int`（D84/D99 要求 Float
//      必带小数点，类型降级不可逆）。
//
// `build_node_input` 现直接构造 `Value::Dict` 交给 `flow::value_to_json`
// —— 仓内**唯一**的序列化实现。本函数失去全部调用者；
// 保留一个已被证明错误、又无人调用的 `pub` 重复实现，只会诱使后来者
// 直接用它。
