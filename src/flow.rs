//! v0.20: 从 interpreter.rs 抽离的自由函数。
//!
//! **Move-only refactor** — code copied verbatim from src/interpreter.rs
//! No signature changes, no field changes, no visibility changes.
//! Re-exported in interpreter.rs via `use crate::flow::*;`

use crate::common::{BinaryOp, Literal};
use crate::value::Value;

/// 判断值是否为真
pub fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Nil => false,
        Value::Bool(b) => *b,
        Value::Float(n) => *n != 0.0,
        Value::String(s) => !s.is_empty(),
        Value::List(l) => !l.is_empty(),
        Value::Dict(d) => !d.is_empty(),
        _ => true,
    }
}

/// 检查是否是内置对象名
pub fn is_builtin_object(name: &str) -> bool {
    matches!(name, "ai" | "web" | "json" | "file" | "memory" | "agent")
}

/// 期望值为字符串，带上下文信息
pub fn expect_string(value: Value, context: &str) -> Result<String, String> {
    match value {
        Value::String(s) => Ok(s),
        other => Err(format!("{}: expected string, got {:?}", context, other)),
    }
}

/// hex 编码
pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// hex 解码
pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("hex string must have even length".to_string());
    }
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    for i in (0..bytes.len()).step_by(2) {
        let high = hex_nibble(bytes[i]).ok_or("invalid hex character")?;
        let low = hex_nibble(bytes[i + 1]).ok_or("invalid hex character")?;
        result.push((high << 4) | low);
    }
    Ok(result)
}

/// hex 单字符解析
pub fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// 检查是否是管道方法
pub fn is_pipe_method(name: &str) -> bool {
    matches!(
        name,
        "map"
            | "filter"
            | "reduce"
            | "push"
            | "pop"
            | "get"
            | "len"
            | "upper"
            | "lower"
            | "trim"
            | "starts_with"
            | "ends_with"
            | "contains"
            | "split"
            | "replace"
            | "take"
            | "drop"
            | "window"
            | "batch"
            | "shape"
            | "flatten"
            | "transpose"
            | "reshape"
    )
}

/// 二元操作求值
///
/// v0.38 (C5): addition follows the same Rust-strict promotion rules.
pub fn eval_binary(left: Value, op: &BinaryOp, right: Value) -> Result<Value, String> {
    match op {
        BinaryOp::Add => match (&left, &right) {
            // Strict: Int+Int -> Int
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
            // Strict: Float+Float -> Float
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
            // Mixed -> error
            (Value::Int(_), Value::Float(_)) | (Value::Float(_), Value::Int(_)) => {
                Err("operator '+' requires both operands to be same numeric type (Int or Float, Rust-strict mode)".to_string())
            }
            (Value::String(a), Value::String(b)) => Ok(Value::String(format!("{}{}", a, b))),
            // 字符串 + 任意类型 → 自动转字符串拼接
            (Value::String(a), _) => Ok(Value::String(format!("{}{}", a, right))),
            (_, Value::String(b)) => Ok(Value::String(format!("{}{}", left, b))),
            (Value::List(a), Value::List(b)) => {
                // v0.17: 等长列表逐元素相加，否则拼接
                if a.len() == b.len() {
                    let result: Vec<Value> = a
                        .iter()
                        .zip(b.iter())
                        .map(|(x, y)| match (x, y) {
                            (Value::Float(xn), Value::Float(yn)) => Value::Float(xn + yn),
                            (Value::String(xs), Value::String(ys)) => {
                                Value::String(format!("{}{}", xs, ys))
                            }
                            _ => Value::Nil,
                        })
                        .collect();
                    Ok(Value::List(result))
                } else {
                    let mut merged = a.clone();
                    merged.extend(b.clone());
                    Ok(Value::List(merged))
                }
            }
            // v0.17: 广播 - list + number
            (Value::List(list), Value::Float(scalar)) => {
                let result: Vec<Value> = list
                    .iter()
                    .map(|item| match item {
                        Value::Float(n) => Value::Float(n + scalar),
                        Value::String(s) => Value::String(format!("{}{}", s, scalar)),
                        _ => Value::Nil,
                    })
                    .collect();
                Ok(Value::List(result))
            }
            // v0.17: 广播 - number + list
            (Value::Float(scalar), Value::List(list)) => {
                let result: Vec<Value> = list
                    .iter()
                    .map(|item| match item {
                        Value::Float(n) => Value::Float(scalar + n),
                        _ => Value::Nil,
                    })
                    .collect();
                Ok(Value::List(result))
            }
            _ => Err("Operands must be two numbers, two strings, or two lists".to_string()),
        },
        BinaryOp::Sub => numeric_op(left, right, |a, b| a - b),
        BinaryOp::Mul => numeric_op(left, right, |a, b| a * b),
        BinaryOp::Div => numeric_op(left, right, |a, b| a / b),
        BinaryOp::Mod => numeric_op(left, right, |a, b| a % b),
        BinaryOp::Equal => Ok(Value::Bool(values_equal(&left, &right))),
        BinaryOp::NotEqual => Ok(Value::Bool(!values_equal(&left, &right))),
        BinaryOp::Greater => numeric_cmp(left, right, |a, b| a > b),
        BinaryOp::Less => numeric_cmp(left, right, |a, b| a < b),
        BinaryOp::GreaterEqual => numeric_cmp(left, right, |a, b| a >= b),
        BinaryOp::LessEqual => numeric_cmp(left, right, |a, b| a <= b),
    }
}

/// 数值操作辅助
///
/// v0.38 (C5): numeric tower — promotion rules (Rust-strict style):
/// - `Int + Int = Int`        (pure integer arithmetic)
/// - `Float + Float = Float`  (pure float arithmetic)
/// - `Int + Float` / `Float + Int` -> strict type error
pub fn numeric_op<F>(left: Value, right: Value, op: F) -> Result<Value, String>
where
    F: Fn(f64, f64) -> f64,
{
    use Value::*;
    match (left, right) {
        // Strict: Int+Int -> Int
        (Int(a), Int(b)) => {
            let af = a as f64;
            let bf = b as f64;
            let result = op(af, bf).round() as i64;
            Ok(Int(result))
        }
        // Strict: Float+Float -> Float
        (Float(a), Float(b)) => Ok(Float(op(a, b))),
        // Mixed types -> strict error
        (Int(_), Float(_)) | (Float(_), Int(_)) => Err(
            "numeric operator does not accept mixed Int and Float operands (Rust-strict mode)"
                .to_string(),
        ),
        // v0.17: 广播操作 - list op number
        (Value::List(list), Value::Float(scalar)) => {
            let result: Vec<Value> = list
                .iter()
                .map(|item| match item {
                    Value::Float(n) => Value::Float(op(*n, scalar)),
                    _ => Value::Nil,
                })
                .collect();
            Ok(Value::List(result))
        }
        // v0.17: 广播操作 - number op list
        (Value::Float(scalar), Value::List(list)) => {
            let result: Vec<Value> = list
                .iter()
                .map(|item| match item {
                    Value::Float(n) => Value::Float(op(scalar, *n)),
                    _ => Value::Nil,
                })
                .collect();
            Ok(Value::List(result))
        }
        // v0.17: 广播操作 - list op list (逐元素)
        (Value::List(a), Value::List(b)) => {
            if a.len() != b.len() {
                return Err(format!("List length mismatch: {} vs {}", a.len(), b.len()));
            }
            let result: Vec<Value> = a
                .iter()
                .zip(b.iter())
                .map(|(x, y)| match (x, y) {
                    (Value::Float(xn), Value::Float(yn)) => Value::Float(op(*xn, *yn)),
                    _ => Value::Nil,
                })
                .collect();
            Ok(Value::List(result))
        }
        _ => Err("Operands must be numbers".to_string()),
    }
}

/// 数值比较辅助
///
/// v0.38: Int/Int compare as i64, Float/Float as f64, mixed -> error.
pub fn numeric_cmp<F>(left: Value, right: Value, op: F) -> Result<Value, String>
where
    F: Fn(f64, f64) -> bool,
{
    use Value::*;
    match (left, right) {
        (Int(a), Int(b)) => Ok(Bool(op(a as f64, b as f64))),
        (Float(a), Float(b)) => Ok(Bool(op(a, b))),
        (Int(_), Float(_)) | (Float(_), Int(_)) => Err(
            "numeric comparison does not accept mixed Int and Float operands (Rust-strict mode)"
                .to_string(),
        ),
        _ => Err("Operands must be numbers".to_string()),
    }
}

/// 值相等比较
pub fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Nil, Value::Nil) => true,
        // v0.75.44: Int 分支 — v0.38 numeric tower 引入 Int 变体时漏加，
        // 导致 `4 == 4` 恒 false。Mixed 数字（Int vs Float）仍 false
        // （跨类型不相等，与 numeric_op strict 语义一致）。
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::List(a), Value::List(b)) => a == b,
        (Value::Dict(a), Value::Dict(b)) => a == b,
        // Conversation 不支持相等比较——比较引用无意义
        _ => false,
    }
}

/// AST Literal 转运行时 Value
pub fn literal_to_value_static(lit: &Literal) -> Value {
    match lit {
        Literal::String(s, _) => Value::String(s.clone()),
        Literal::Char(c, _) => Value::Char(*c),
        Literal::Int(i, _) => Value::Int(*i),
        Literal::Float(f, _) => Value::Float(*f),
        Literal::Bool(b, _) => Value::Bool(*b),
        Literal::Nil(_) => Value::Nil,
    }
}

/// 运行时类型检查
pub fn check_type(value: &Value, hint: &str) -> bool {
    match (value, hint) {
        (Value::String(_), "string") => true,
        (Value::Float(_), "float") => true,
        (Value::Bool(_), "bool") => true,
        (Value::Nil, "nil") => true,
        (Value::List(_), "list") => true,
        (Value::Dict(_), "dict") => true,
        (Value::Task { .. }, "task") => true,
        (Value::Tool { .. }, "tool") => true,
        (Value::Conversation { .. }, "conversation") => true,
        (Value::Stream { .. }, "stream") => true,
        (Value::Agent { .. }, "agent") => true,
        // v0.08.1: Nil 兼容 dyn Trait 标注（trait 对象占位）
        (Value::Nil, h) if h.starts_with("dyn:") => true,
        // v0.08.1: TraitObject 兼容对应的 dyn Trait 标注
        (Value::TraitObject { .. }, h) if h.starts_with("dyn:") => true,
        _ => false,
    }
}

/// 运行时类型名
pub fn type_name(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "string",
        Value::Char(_) => "char",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Bool(_) => "bool",
        Value::Nil => "nil",
        Value::List(_) => "list",
        Value::Dict(_) => "dict",
        Value::Task { .. } => "task",
        Value::Tool { .. } => "tool",
        Value::Closure { .. } => "closure",
        Value::Builtin(_) => "builtin",
        Value::Conversation { .. } => "conversation",
        Value::Stream { .. } => "stream",
        Value::Agent { .. } => "agent",
        Value::AiConfig { .. } => "ai_config",
        Value::Router { .. } => "router",
        Value::HttpRequest { .. } => "http_request",
        Value::McpServer { .. } => "mcp_server",
        Value::TraitObject { .. } => "trait_object",
        Value::Compose(_) => "compose",
        Value::Partial(_, _) => "partial",
        Value::Atom(_) => "atom",
        Value::Macro { .. } => "macro",
        Value::PromptSection { .. } => "prompt_section",
        Value::Document { .. } => "document",
    }
}

/// 返回值的类型名 (String)
pub fn value_type_name(value: &Value) -> &'static str {
    type_name(value)
}

mod json; // v0.75.63: JSON 编解码（json_to_value/value_to_json + parse_json_*）自 flow.rs 拆出
pub use json::{json_to_value, value_to_json}; // 保持 flow::json_to_value 路径

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::BinaryOp;
    use crate::typeck::Type;

    /// v0.38: Int + Int = Int (no silent promotion to Float).
    #[test]
    fn numeric_tower_int_plus_int_yields_int() {
        let l = Value::Int(2);
        let r = Value::Int(3);
        let v = numeric_op(l, r, |a, b| a + b).unwrap();
        assert_eq!(v, Value::Int(5));
    }

    /// v0.38: Float + Float = Float.
    #[test]
    fn numeric_tower_float_plus_float_yields_float() {
        let l = Value::Float(1.5);
        let r = Value::Float(2.5);
        let v = numeric_op(l, r, |a, b| a + b).unwrap();
        assert_eq!(v, Value::Float(4.0));
    }

    /// v0.38: Int + Float is a STRICT error (Rust-style).
    #[test]
    fn numeric_tower_int_plus_float_is_error() {
        let l = Value::Int(2);
        let r = Value::Float(3.0);
        let v = numeric_op(l, r, |a, b| a + b);
        assert!(v.is_err(), "expected strict error, got: {:?}", v);
    }

    /// v0.38: Float + Int is symmetric — also error.
    #[test]
    fn numeric_tower_float_plus_int_is_error() {
        let l = Value::Float(2.0);
        let r = Value::Int(3);
        let v = numeric_op(l, r, |a, b| a + b);
        assert!(v.is_err());
    }

    /// v0.38: legacy Number mixed with Float coerces to f64.
    #[test]
    fn numeric_tower_number_float_compat() {
        let l = Value::Float(2.0);
        let r = Value::Float(3.0);
        let v = numeric_op(l, r, |a, b| a + b).unwrap();
        assert_eq!(v, Value::Float(5.0));
    }

    /// v0.38: eval_binary Add(Int, Int) -> Int.
    #[test]
    fn eval_binary_int_add() {
        let v = eval_binary(Value::Int(2), &BinaryOp::Add, Value::Int(3)).unwrap();
        assert_eq!(v, Value::Int(5));
    }

    /// v0.38: eval_binary Add(Float, Float) -> Float.
    #[test]
    fn eval_binary_float_add() {
        let v = eval_binary(Value::Float(1.5), &BinaryOp::Add, Value::Float(2.5)).unwrap();
        assert_eq!(v, Value::Float(4.0));
    }

    /// v0.38: eval_binary Add(Int, Float) -> strict error.
    #[test]
    fn eval_binary_int_float_add_is_error() {
        let v = eval_binary(Value::Int(2), &BinaryOp::Add, Value::Float(3.0));
        assert!(v.is_err());
    }

    /// v0.38: numeric_cmp Int < Int.
    #[test]
    fn numeric_cmp_int_lt() {
        let v = numeric_cmp(Value::Int(1), Value::Int(2), |a, b| a < b).unwrap();
        assert_eq!(v, Value::Bool(true));
    }

    /// v0.75.44: eval_binary Equal(Int, Int) — values_equal 的 Int 分支
    /// （v0.38 引入 Int 变体时漏加，`4 == 4` 曾恒 false）。
    #[test]
    fn eval_binary_int_equal() {
        let v = eval_binary(Value::Int(4), &BinaryOp::Equal, Value::Int(4)).unwrap();
        assert_eq!(v, Value::Bool(true));
        let v2 = eval_binary(Value::Int(4), &BinaryOp::Equal, Value::Int(5)).unwrap();
        assert_eq!(v2, Value::Bool(false));
        // Mixed 数字不相等（strict 语义）
        let v3 = eval_binary(Value::Int(4), &BinaryOp::Equal, Value::Float(4.0)).unwrap();
        assert_eq!(v3, Value::Bool(false));
    }

    /// v0.38: numeric_cmp Float == Float.
    #[test]
    fn numeric_cmp_float_eq() {
        let v = numeric_cmp(Value::Float(1.5), Value::Float(1.5), |a, b| a == b).unwrap();
        assert_eq!(v, Value::Bool(true));
    }

    /// v0.38: numeric_cmp Int vs Float is error.
    #[test]
    fn numeric_cmp_int_float_is_error() {
        let v = numeric_cmp(Value::Int(1), Value::Float(1.0), |a, b| a < b);
        assert!(v.is_err());
    }

    /// v0.38: typeck still routes Int literal to Type::Int.
    #[test]
    fn type_int_name() {
        assert_eq!(Type::Int.name(), "int");
        assert_eq!(Type::Float.name(), "float");
        assert_eq!(Type::Float.name(), "float");
    }

    // ===== v0.52 regression: json_to_value 空格 bug =====
    // pre-existing: parse_json_value 在 line 414 trim_start() 但 return 的 consumed
    // 不含 trim 字节数，导致 dict 内有空格时解析错位（"Expected ',' in dict"）
    // 这是 v0.51 P0-3 修 Send 派发时发现的（见 src/runtime/infra.rs:extract_send_tasks
    // 注释里 hand-write 解析以绕开此 bug）

    #[test]
    fn json_to_value_dict_no_space() {
        // 无空格 dict — 应正常解析
        let v = json_to_value(r#"{"a":1,"b":2}"#).unwrap();
        if let Value::Dict(m) = v {
            // parse_json_number 把 int 解析为 Number(f64)（pre-existing 行为）
            assert_eq!(m.get("a"), Some(&Value::Float(1.0)));
            assert_eq!(m.get("b"), Some(&Value::Float(2.0)));
        } else {
            panic!("expected Dict");
        }
    }

    #[test]
    fn json_to_value_dict_with_space() {
        // 带空格 dict — pre-existing bug 应 panic "Expected ',' in dict"
        // 修复后期望 pass
        let v = json_to_value(r#"{"a": 1, "b": 2}"#).unwrap();
        if let Value::Dict(m) = v {
            assert_eq!(m.get("a"), Some(&Value::Float(1.0)));
            assert_eq!(m.get("b"), Some(&Value::Float(2.0)));
        } else {
            panic!("expected Dict, got {:?}", v);
        }
    }

    #[test]
    fn json_to_value_list_with_space() {
        // 带空格 list — 同样应正常解析
        let v = json_to_value("[1, 2, 3]").unwrap();
        if let Value::List(items) = v {
            assert_eq!(items.len(), 3);
        } else {
            panic!("expected List");
        }
    }

    #[test]
    fn json_to_value_nested_with_space() {
        // 嵌套 dict + 空格
        let v = json_to_value(r#"{"a": {"b": [1, 2]}}"#).unwrap();
        if let Value::Dict(m) = &v
            && let Some(Value::Dict(inner)) = m.get("a")
            && let Some(Value::List(items)) = inner.get("b")
        {
            assert_eq!(items.len(), 2);
        } else {
            panic!("nested structure mismatch: {:?}", v);
        }
    }
}
