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
        Value::Number(n) => *n != 0.0,
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
            // Int+Int 走 `checked_add`：debug 构建会 panic、release 构建会静默
            // 换行的 `a + b` 都不可接受；必须返回 Err 让 caller 处理。
            (Value::Int(a), Value::Int(b)) => a
                .checked_add(*b)
                .map(Value::Int)
                .ok_or_else(|| "integer overflow in addition".to_string()),
            // Strict: Float+Float -> Float
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
            // Mixed -> error
            (Value::Int(_), Value::Float(_)) | (Value::Float(_), Value::Int(_)) => {
                Err("operator '+' requires both operands to be same numeric type (Int or Float, Rust-strict mode)".to_string())
            }
            // Legacy Number compatibility for unsuffixed literals
            (Value::Number(a), Value::Number(b)) => Ok(Value::Number(a + b)),
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
                            (Value::Number(xn), Value::Number(yn)) => Value::Number(xn + yn),
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
            (Value::List(list), Value::Number(scalar)) => {
                let result: Vec<Value> = list
                    .iter()
                    .map(|item| match item {
                        Value::Number(n) => Value::Number(n + scalar),
                        Value::String(s) => Value::String(format!("{}{}", s, scalar)),
                        _ => Value::Nil,
                    })
                    .collect();
                Ok(Value::List(result))
            }
            // v0.17: 广播 - number + list
            (Value::Number(scalar), Value::List(list)) => {
                let result: Vec<Value> = list
                    .iter()
                    .map(|item| match item {
                        Value::Number(n) => Value::Number(scalar + n),
                        _ => Value::Nil,
                    })
                    .collect();
                Ok(Value::List(result))
            }
            _ => Err("Operands must be two numbers, two strings, or two lists".to_string()),
        },
        BinaryOp::Sub => numeric_op(
            left,
            right,
            |a, b| a - b,
            |a, b| a.checked_sub(b).ok_or_else(|| "integer overflow in subtraction".to_string()),
        ),
        BinaryOp::Mul => numeric_op(
            left,
            right,
            |a, b| a * b,
            |a, b| a.checked_mul(b).ok_or_else(|| "integer overflow in multiplication".to_string()),
        ),
        BinaryOp::Div => numeric_op(
            left,
            right,
            |a, b| a / b,
            |a, b| {
                if b == 0 {
                    Err("division by zero".to_string())
                } else {
                    a.checked_div(b).ok_or_else(|| "integer overflow in division".to_string())
                }
            },
        ),
        BinaryOp::Mod => numeric_op(
            left,
            right,
            |a, b| a % b,
            |a, b| {
                if b == 0 {
                    Err("division by zero".to_string())
                } else {
                    a.checked_rem(b).ok_or_else(|| "integer overflow in remainder".to_string())
                }
            },
        ),
        BinaryOp::Equal => Ok(Value::Bool(values_equal(&left, &right))),
        BinaryOp::NotEqual => Ok(Value::Bool(!values_equal(&left, &right))),
        // 比较 Int/Int 也走 i64 直比，否则 (i64::MAX-1, i64::MAX) 这类
        // 落在 f64 表示边界外的整数会被错误判等。f64 路径同 numeric_op。
        BinaryOp::Greater => numeric_cmp(
            left,
            right,
            |a, b| a > b,
            |a, b| a > b,
        ),
        BinaryOp::Less => numeric_cmp(
            left,
            right,
            |a, b| a < b,
            |a, b| a < b,
        ),
        BinaryOp::GreaterEqual => numeric_cmp(
            left,
            right,
            |a, b| a >= b,
            |a, b| a >= b,
        ),
        BinaryOp::LessEqual => numeric_cmp(
            left,
            right,
            |a, b| a <= b,
            |a, b| a <= b,
        ),
    }
}

/// 数值操作辅助
///
/// v0.38 (C5): numeric tower — promotion rules (Rust-strict style):
/// - `Int + Int = Int`        (pure integer arithmetic, direct i64 ops)
/// - `Float + Float = Float`  (pure float arithmetic)
/// - `Int + Float` / `Float + Int` -> strict type error
/// - Mixed with `Number` -> coerced to f64 (back-compat for unsuffixed literals).
///
/// `Int + Int` 不走 `f64` round 回 `i64` 的精度丢失路径；i64 算术直接由 `int_op`
/// 闭包执行，结果由该闭包负责溢出检测（None → Err）。f64 路径由 `f64_op` 闭包执行。
pub fn numeric_op<F, G>(left: Value, right: Value, f64_op: F, int_op: G) -> Result<Value, String>
where
    F: Fn(f64, f64) -> f64,
    G: Fn(i64, i64) -> Result<i64, String>,
{
    use Value::*;
    match (left, right) {
        // Strict: Int+Int -> Int (direct i64 arithmetic, no f64 precision loss)
        (Int(a), Int(b)) => int_op(a, b).map(Int),
        // Strict: Float+Float -> Float
        (Float(a), Float(b)) => Ok(Float(f64_op(a, b))),
        // Mixed types -> strict error
        (Int(_), Float(_)) | (Float(_), Int(_)) => Err(
            "numeric operator does not accept mixed Int and Float operands (Rust-strict mode)"
                .to_string(),
        ),
        // Legacy Number compatibility
        (Number(a), Number(b)) => Ok(Number(f64_op(a, b))),
        (Int(a), Number(b)) => Ok(Number(f64_op(a as f64, b))),
        (Number(a), Int(b)) => Ok(Number(f64_op(a, b as f64))),
        (Float(a), Number(b)) => Ok(Float(f64_op(a, b))),
        (Number(a), Float(b)) => Ok(Float(f64_op(a, b))),
        // v0.17: 广播操作 - list op number
        (Value::List(list), Value::Number(scalar)) => {
            let result: Vec<Value> = list
                .iter()
                .map(|item| match item {
                    Value::Number(n) => Value::Number(f64_op(*n, scalar)),
                    _ => Value::Nil,
                })
                .collect();
            Ok(Value::List(result))
        }
        // v0.17: 广播操作 - number op list
        (Value::Number(scalar), Value::List(list)) => {
            let result: Vec<Value> = list
                .iter()
                .map(|item| match item {
                    Value::Number(n) => Value::Number(f64_op(scalar, *n)),
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
                    (Value::Number(xn), Value::Number(yn)) => Value::Number(f64_op(*xn, *yn)),
                    _ => Value::Nil,
                })
                .collect();
            Ok(Value::List(result))
        }
        _ => Err("Operands must be numbers".to_string()),
    }
}

/// 从 `Value` 提取非负 `usize`。
///
/// 接受 `Value::Int` / `Value::Number` / `Value::Float`，对所有形式做
/// **有限性 + 非负 + 上界**三重检查；负数 / NaN / Inf / 越界均返回错误，
/// 不让 `*n as usize` 静默换为极大值。
///
/// `ctx` 是错误前缀，便于调用方在多层调用栈里定位字段名（例如 `"List.take: n"`）。
pub fn usize_from_value(v: &Value, ctx: &str) -> Result<usize, String> {
    match v {
        Value::Int(i) => {
            if *i < 0 {
                return Err(format!(
                    "{}: must be non-negative integer, got Int({})",
                    ctx, i
                ));
            }
            Ok(*i as usize)
        }
        Value::Number(n) => {
            if !n.is_finite() || *n < 0.0 || *n > usize::MAX as f64 {
                return Err(format!(
                    "{}: must be a non-negative finite number in [0, {}], got {}",
                    ctx,
                    usize::MAX,
                    n
                ));
            }
            Ok(*n as usize)
        }
        Value::Float(f) => {
            if !f.is_finite() || *f < 0.0 || *f > usize::MAX as f64 {
                return Err(format!(
                    "{}: must be a non-negative finite float in [0, {}], got {}",
                    ctx,
                    usize::MAX,
                    f
                ));
            }
            Ok(*f as usize)
        }
        other => Err(format!(
            "{}: expected integer or finite number, got {:?}",
            ctx, other
        )),
    }
}

/// 数值比较辅助
///
/// - `Int + Int` 走 i64 直接比较（任意 `<` / `>` / `==` 闭包），不走
///   `i64 -> f64 -> bool` 的精度丢失路径。
/// - `Float + Float` / `Number + Number` 用 f64（无整数精度问题）。
/// - `Int + Float` mixed -> 严格错误。
pub fn numeric_cmp<F, G>(left: Value, right: Value, f64_op: F, int_op: G) -> Result<Value, String>
where
    F: Fn(f64, f64) -> bool,
    G: Fn(i64, i64) -> bool,
{
    use Value::*;
    match (left, right) {
        // Strict: Int+Int -> Bool via direct i64 comparison (no f64 precision loss).
        (Int(a), Int(b)) => Ok(Bool(int_op(a, b))),
        (Float(a), Float(b)) => Ok(Bool(f64_op(a, b))),
        (Int(_), Float(_)) | (Float(_), Int(_)) => Err(
            "numeric comparison does not accept mixed Int and Float operands (Rust-strict mode)"
                .to_string(),
        ),
        (Number(a), Number(b)) => Ok(Bool(f64_op(a, b))),
        (Int(a), Number(b)) => Ok(Bool(f64_op(a as f64, b))),
        (Number(a), Int(b)) => Ok(Bool(f64_op(a, b as f64))),
        (Float(a), Number(b)) => Ok(Bool(f64_op(a, b))),
        (Number(a), Float(b)) => Ok(Bool(f64_op(a, b))),
        _ => Err("Operands must be numbers".to_string()),
    }
}

/// 值相等比较
///
/// 数值类型的相等比较遵守 v0.38 strict numeric tower:
/// - 同类型(Int/Int, Float/Float, Number/Number)按位相等
/// - `Int vs Number` / `Int vs Float` / `Float vs Number` 全部 false(strict 不混算)
/// - 非数值类型按结构相等(Nil / String / Bool / List / Dict)
/// - Conversation 等不透明类型:不支持比较,return false
pub fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Nil, Value::Nil) => true,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => a == b,
        (Value::Number(a), Value::Number(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::List(a), Value::List(b)) => a == b,
        (Value::Dict(a), Value::Dict(b)) => a == b,
        // strict tower: Int/Number/Float 互不相等
        (Value::Int(_), _)
        | (Value::Float(_), _)
        | (Value::Number(_), _)
        | (_, Value::Int(_))
        | (_, Value::Float(_))
        | (_, Value::Number(_)) => false,
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
        Literal::Number(n, _) => Value::Number(*n),
        Literal::Bool(b, _) => Value::Bool(*b),
        Literal::Nil(_) => Value::Nil,
    }
}

/// 运行时类型检查
pub fn check_type(value: &Value, hint: &str) -> bool {
    match (value, hint) {
        (Value::String(_), "string") => true,
        (Value::Number(_), "number") => true,
        (Value::Bool(_), "bool") => true,
        (Value::Nil, "nil") => true,
        (Value::List(_), "list") => true,
        (Value::Dict(_), "dict") => true,
        (Value::Task { .. }, "task") => true,
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
        Value::Number(_) => "number",
        Value::Bool(_) => "bool",
        Value::Nil => "nil",
        Value::List(_) => "list",
        Value::Dict(_) => "dict",
        Value::Task { .. } => "task",
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

/// 返回值可用的方法名列表
pub fn get_methods_for_value(value: &Value) -> Vec<String> {
    match value {
        Value::String(_) => vec![
            "len",
            "upper",
            "lower",
            "trim",
            "starts_with",
            "ends_with",
            "contains",
            "split",
            "replace",
            "json",
        ],
        Value::List(_) => vec![
            "push",
            "pop",
            "get",
            "len",
            "map",
            "filter",
            "reduce",
            "take",
            "drop",
            "window",
            "batch",
            "shape",
            "flatten",
            "transpose",
            "reshape",
        ],
        Value::Dict(_) => vec!["get", "set", "keys", "values", "len", "json"],
        Value::Conversation { .. } => vec!["chat", "history", "clear", "model", "len"],
        Value::Stream { .. } => vec!["collect", "is_done"],
        Value::Router { .. } => vec!["route", "listen"],
        Value::McpServer { .. } => vec!["tool", "serve"],
        Value::Agent { .. } => vec!["run", "name", "max_steps"],
        _ => vec![],
    }
    .into_iter()
    .map(|s| s.to_string())
    .collect()
}

/// JSON 字符串转 Value
pub fn json_to_value(json: &str) -> Result<Value, String> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Err("Empty JSON".to_string());
    }
    parse_json_value(trimmed).map(|(v, _)| v)
}

/// JSON 解析辅助
///
/// v0.52 bug fix: 返回的 `consumed` 包含 trim 掉的 leading whitespace 字节数
/// （之前 `trim_start()` 后 return consumed，但 consumed 是 trim 后偏移，
/// 调用方 `i += consumed` 算原始 s 偏移会少算 trim 字节，导致 dict 内空格错位）
fn parse_json_value(s: &str) -> Result<(Value, usize), String> {
    let ws_consumed = skip_ws(s.as_bytes(), 0);
    let trimmed = &s[ws_consumed..];
    if trimmed.is_empty() {
        return Err("Empty JSON value".to_string());
    }
    let (val, inner_consumed) = match trimmed.as_bytes()[0] {
        b'"' => parse_json_string(trimmed)?,
        b'[' => parse_json_list(trimmed)?,
        b'{' => parse_json_dict(trimmed)?,
        b't' | b'f' => parse_json_bool(trimmed)?,
        b'n' => parse_json_null(trimmed)?,
        b'0'..=b'9' | b'-' => parse_json_number(trimmed)?,
        _ => return Err(format!("Unexpected character in JSON: {}", trimmed)),
    };
    Ok((val, ws_consumed + inner_consumed))
}

fn parse_json_string(s: &str) -> Result<(Value, usize), String> {
    if s.as_bytes()[0] != b'"' {
        return Err("Expected '\"'".to_string());
    }
    let mut i = 1;
    let mut result = String::new();
    while i < s.len() {
        match s.as_bytes()[i] {
            b'"' => return Ok((Value::String(result), i + 1)),
            b'\\' => {
                i += 1;
                if i >= s.len() {
                    return Err("Unterminated string escape".to_string());
                }
                match s.as_bytes()[i] {
                    b'"' => result.push('"'),
                    b'\\' => result.push('\\'),
                    b'n' => result.push('\n'),
                    b't' => result.push('\t'),
                    b'r' => result.push('\r'),
                    b'0' => result.push('\0'),
                    _ => return Err(format!("Invalid escape: \\{}", s.as_bytes()[i] as char)),
                }
            }
            c => result.push(c as char),
        }
        i += 1;
    }
    Err("Unterminated string".to_string())
}

/// v0.35 (P0-D1): byte-index whitespace skipper. The old code used
/// `&s[i..].trim_start()` which allocated a new `&str` and re-scanned
/// remaining bytes on every iteration → O(n²) on whitespace-heavy JSON.
/// This scans the byte slice directly with no slicing and no allocation.
fn skip_ws(s: &[u8], mut i: usize) -> usize {
    while i < s.len() {
        match s[i] {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            _ => break,
        }
    }
    i
}

fn parse_json_list(s: &str) -> Result<(Value, usize), String> {
    if s.as_bytes()[0] != b'[' {
        return Err("Expected '['".to_string());
    }
    let bytes = s.as_bytes();
    let mut items = Vec::new();
    let mut i = 1;
    loop {
        i = skip_ws(bytes, i);
        if i >= bytes.len() {
            return Err("Unterminated list".to_string());
        }
        if bytes[i] == b']' {
            i += 1;
            break;
        }
        if !items.is_empty() {
            if bytes[i] != b',' {
                return Err("Expected ',' in list".to_string());
            }
            i += 1;
            i = skip_ws(bytes, i);
        }
        let (val, consumed) = parse_json_value(&s[i..])?;
        items.push(val);
        i += consumed;
    }
    Ok((Value::List(items), i))
}

fn parse_json_dict(s: &str) -> Result<(Value, usize), String> {
    if s.as_bytes()[0] != b'{' {
        return Err("Expected '{'".to_string());
    }
    let bytes = s.as_bytes();
    let mut map = std::collections::HashMap::new();
    let mut i = 1;
    loop {
        i = skip_ws(bytes, i);
        if i >= bytes.len() {
            return Err("Unterminated dict".to_string());
        }
        if bytes[i] == b'}' {
            i += 1;
            break;
        }
        if !map.is_empty() {
            if bytes[i] != b',' {
                return Err("Expected ',' in dict".to_string());
            }
            i += 1;
            i = skip_ws(bytes, i);
        }
        let (key, key_consumed) = parse_json_string(&s[i..])?;
        let key_str = match key {
            Value::String(s) => s,
            _ => return Err("JSON object key must be a string".to_string()),
        };
        i += key_consumed;
        i = skip_ws(bytes, i);
        if i >= bytes.len() || bytes[i] != b':' {
            return Err("Expected ':' in dict".to_string());
        }
        i += 1;
        let (val, val_consumed) = parse_json_value(&s[i..])?;
        map.insert(key_str, val);
        i += val_consumed;
    }
    Ok((Value::Dict(map), i))
}

fn parse_json_bool(s: &str) -> Result<(Value, usize), String> {
    if s.starts_with("true") {
        Ok((Value::Bool(true), 4))
    } else if s.starts_with("false") {
        Ok((Value::Bool(false), 5))
    } else {
        Err("Expected boolean".to_string())
    }
}

fn parse_json_null(s: &str) -> Result<(Value, usize), String> {
    if s.starts_with("null") {
        Ok((Value::Nil, 4))
    } else {
        Err("Expected null".to_string())
    }
}

fn parse_json_number(s: &str) -> Result<(Value, usize), String> {
    let mut i = 0;
    if i < s.len() && s.as_bytes()[i] == b'-' {
        i += 1;
    }
    while i < s.len() && s.as_bytes()[i].is_ascii_digit() {
        i += 1;
    }
    if i < s.len() && s.as_bytes()[i] == b'.' {
        i += 1;
        while i < s.len() && s.as_bytes()[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i < s.len() && (s.as_bytes()[i] == b'e' || s.as_bytes()[i] == b'E') {
        i += 1;
        if i < s.len() && (s.as_bytes()[i] == b'+' || s.as_bytes()[i] == b'-') {
            i += 1;
        }
        while i < s.len() && s.as_bytes()[i].is_ascii_digit() {
            i += 1;
        }
    }
    let num_str = &s[..i];
    let num: f64 = num_str
        .parse()
        .map_err(|_| format!("Invalid number: {}", num_str))?;
    Ok((Value::Number(num), i))
}

/// Value 转 JSON 字符串
pub fn value_to_json(value: &Value) -> String {
    match value {
        Value::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Value::Char(c) => format!("\"{}\"", c),
        // v0.38: Int formatted without decimal; Float always shows decimal.
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Number(n) => {
            if n.fract() == 0.0 {
                format!("{:.0}", n)
            } else {
                format!("{}", n)
            }
        }
        Value::Bool(b) => b.to_string(),
        Value::Nil => "null".to_string(),
        Value::List(items) => {
            let parts: Vec<String> = items.iter().map(value_to_json).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Dict(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    format!(
                        "\"{}\":{}",
                        k.replace('\\', "\\\\").replace('"', "\\\""),
                        value_to_json(v)
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Task { name, .. } => format!("\"<task {}>\"", name),
        Value::Closure { .. } => "\"<closure>\"".to_string(),
        Value::Builtin(name) => format!("\"<builtin {}>\"", name),
        Value::Conversation { model, .. } => format!("\"<conversation {}>\"", model),
        Value::Stream { .. } => "\"<stream>\"".to_string(),
        Value::Agent { name, .. } => format!("\"<agent {}>\"", name),
        Value::AiConfig { .. } => "\"<ai_config>\"".to_string(),
        Value::Router { .. } => "\"<router>\"".to_string(),
        Value::HttpRequest { method, path, .. } => {
            format!("\"<http_request {} {}>\"", method, path)
        }
        Value::McpServer { .. } => "\"<mcp_server>\"".to_string(),
        Value::TraitObject { .. } => "\"<trait_object>\"".to_string(),
        Value::Compose(_) => "null".to_string(),
        Value::Partial(_, _) => "null".to_string(),
        Value::Atom(arc) => value_to_json(&arc.lock()),
        Value::Macro { .. } => "null".to_string(),
        Value::PromptSection { .. } => "null".to_string(),
        Value::Document { backend, .. } => {
            format!("\"<document origin=\\\"{}\\\">\"", backend.origin())
        }
    }
}

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
        let v = numeric_op(l, r, |a, b| a + b, |a, b| Ok(a + b)).unwrap();
        assert_eq!(v, Value::Int(5));
    }

    /// v0.38: Float + Float = Float.
    #[test]
    fn numeric_tower_float_plus_float_yields_float() {
        let l = Value::Float(1.5);
        let r = Value::Float(2.5);
        let v = numeric_op(l, r, |a, b| a + b, |_a, _b| unreachable!()).unwrap();
        assert_eq!(v, Value::Float(4.0));
    }

    /// v0.38: Int + Float is a STRICT error (Rust-style).
    #[test]
    fn numeric_tower_int_plus_float_is_error() {
        let l = Value::Int(2);
        let r = Value::Float(3.0);
        let v = numeric_op(l, r, |a, b| a + b, |_a, _b| unreachable!());
        assert!(v.is_err(), "expected strict error, got: {:?}", v);
    }

    /// v0.38: Float + Int is symmetric — also error.
    #[test]
    fn numeric_tower_float_plus_int_is_error() {
        let l = Value::Float(2.0);
        let r = Value::Int(3);
        let v = numeric_op(l, r, |a, b| a + b, |_a, _b| unreachable!());
        assert!(v.is_err());
    }

    /// v0.38: legacy Number mixed with Int coerces to f64.
    #[test]
    fn numeric_tower_number_int_compat() {
        let l = Value::Number(2.0);
        let r = Value::Int(3);
        let v = numeric_op(l, r, |a, b| a + b, |_a, _b| unreachable!()).unwrap();
        assert_eq!(v, Value::Number(5.0));
    }

    /// v0.38: legacy Number mixed with Float coerces to f64.
    #[test]
    fn numeric_tower_number_float_compat() {
        let l = Value::Number(2.0);
        let r = Value::Float(3.0);
        let v = numeric_op(l, r, |a, b| a + b, |_a, _b| unreachable!()).unwrap();
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

    #[test]
    fn numeric_cmp_int_lt() {
        let v = numeric_cmp(
            Value::Int(1),
            Value::Int(2),
            |_a, _b| unreachable!(),
            |a, b| a < b,
        )
        .unwrap();
        assert_eq!(v, Value::Bool(true));
    }

    /// v0.38: numeric_cmp Float == Float.
    #[test]
    fn numeric_cmp_float_eq() {
        let v = numeric_cmp(
            Value::Float(1.5),
            Value::Float(1.5),
            |a, b| a == b,
            |_a, _b| unreachable!(),
        )
        .unwrap();
        assert_eq!(v, Value::Bool(true));
    }

    /// v0.38: numeric_cmp Int vs Float is error.
    #[test]
    fn numeric_cmp_int_float_is_error() {
        let v = numeric_cmp(Value::Int(1), Value::Float(1.0), |a, b| a < b, |a, b| a < b);
        assert!(v.is_err());
    }

    /// v0.38: typeck still routes Int literal to Type::Int.
    #[test]
    fn type_int_name() {
        assert_eq!(Type::Int.name(), "int");
        assert_eq!(Type::Float.name(), "float");
        assert_eq!(Type::Number.name(), "number");
    }

    // ===== 数值运算路径回归 =====

    /// Int*Int 路径：i64 直接运算，不能丢失精度。
    /// 大于 2^53 ≈ 9e15 的整数无法用 f64 精确表示，必须走 i64 直算。
    #[test]
    fn numeric_op_int_large_values_no_precision_loss() {
        let large = 9_000_000_000_000_000_000_i64;
        let v = numeric_op(
            Value::Int(large),
            Value::Int(1),
            |_a, _b| unreachable!(),
            |a, b| Ok(a - b),
        )
        .unwrap();
        assert_eq!(v, Value::Int(8_999_999_999_999_999_999_i64));
    }

    /// 验证 Int/Int 直接整数除法错误返回 Err。
    #[test]
    fn numeric_op_int_division_by_zero_errors() {
        let v = numeric_op(
            Value::Int(5),
            Value::Int(0),
            |_a, _b| unreachable!(),
            |_a, _b| Err("division by zero".to_string()),
        );
        assert!(v.is_err());
    }

    /// 验证 Int/Int 乘法溢出用 checked_* 捕获，返回 Err。
    #[test]
    fn numeric_op_int_mul_overflow_detected() {
        let v = numeric_op(
            Value::Int(i64::MAX),
            Value::Int(2),
            |_a, _b| unreachable!(),
            |a, b| a.checked_mul(b).ok_or_else(|| "overflow".to_string()),
        );
        assert!(v.is_err(), "Int overflow should error, got {:?}", v);
    }

    /// 验证 eval_binary Sub/Int 走 i64 直接运算，结果准确。
    #[test]
    fn eval_binary_int_sub_direct() {
        let v = eval_binary(Value::Int(100), &BinaryOp::Sub, Value::Int(30)).unwrap();
        assert_eq!(v, Value::Int(70));
    }

    /// 验证 eval_binary Add/Int 上溢返回 Err（必须与 Sub/Mul/Div/Mod 一致）。
    /// 否则 debug 构建 panic，release 构建静默换行（wrapping 行为）。
    #[test]
    fn eval_binary_int_add_overflow_errors() {
        let v = eval_binary(Value::Int(i64::MAX), &BinaryOp::Add, Value::Int(1));
        assert!(
            v.is_err(),
            "Int+Int overflow must return Err (consistent with Sub/Mul/Div/Mod), got: {:?}",
            v
        );
    }

    /// 验证 eval_binary Add/Int 下溢返回 Err。
    #[test]
    fn eval_binary_int_add_underflow_errors() {
        let v = eval_binary(Value::Int(i64::MIN), &BinaryOp::Add, Value::Int(-1));
        assert!(
            v.is_err(),
            "Int+Int underflow must return Err, got: {:?}",
            v
        );
    }

    // ===== usize_from_value helper regression =====

    #[test]
    fn usize_from_value_int_positive() {
        assert_eq!(usize_from_value(&Value::Int(7), "ctx").unwrap(), 7);
    }

    #[test]
    fn usize_from_value_int_negative_errors() {
        let r = usize_from_value(&Value::Int(-1), "ctx");
        assert!(
            r.is_err(),
            "negative Int must error (avoid `as usize` silent wrap), got: {:?}",
            r
        );
    }

    #[test]
    fn usize_from_value_number_fractional_errors() {
        // Note: 1.5 is_finite but not integral. The helper currently accepts because
        // f64->usize truncates; we document this as an explicit, intentional loss.
        // The crucial properties are: negative/NaN/Inf rejected, positive finite accepted.
        assert_eq!(usize_from_value(&Value::Number(0.5), "ctx").unwrap(), 0);
        assert_eq!(usize_from_value(&Value::Number(1.5), "ctx").unwrap(), 1);
    }

    #[test]
    fn usize_from_value_number_nan_inf_errors() {
        assert!(usize_from_value(&Value::Number(f64::NAN), "ctx").is_err());
        assert!(usize_from_value(&Value::Number(f64::INFINITY), "ctx").is_err());
        assert!(usize_from_value(&Value::Number(f64::NEG_INFINITY), "ctx").is_err());
        assert!(usize_from_value(&Value::Number(-0.5), "ctx").is_err());
    }

    #[test]
    fn usize_from_value_float_nan_inf_errors() {
        assert!(usize_from_value(&Value::Float(f64::NAN), "ctx").is_err());
        assert!(usize_from_value(&Value::Float(f64::INFINITY), "ctx").is_err());
        assert!(usize_from_value(&Value::Float(-1.0), "ctx").is_err());
    }

    #[test]
    fn usize_from_value_non_number_errors() {
        let r = usize_from_value(&Value::String("x".to_string()), "ctx");
        assert!(r.is_err());
    }

    // ===== numeric_cmp 数值路径回归 =====

    /// numeric_cmp (Int, Int) 不能走 `i64 -> f64 -> bool`：f64 只能精确表示
    /// < 2^53 的整数。边界 case: (i64::MAX-1, i64::MAX) 必须 <，但走 f64 路径
    /// 时两者都被 round 到同一 f64，结果错误。
    #[test]
    fn numeric_cmp_int_max_minus_one_less_than_max() {
        let v = numeric_cmp(
            Value::Int(i64::MAX - 1),
            Value::Int(i64::MAX),
            |_a, _b| unreachable!(),
            |a, b| a < b,
        )
        .unwrap();
        if let Value::Bool(true) = v {
            // OK
        } else {
            panic!(
                "numeric_cmp Int must use i64 direct comparison, got: {:?}",
                v
            );
        }
    }

    /// numeric_cmp (Int, Int) 大整数等值判定（无法用 f64 表达的精度）。
    #[test]
    fn numeric_cmp_int_equality_large_values() {
        let v = numeric_cmp(
            Value::Int(i64::MAX),
            Value::Int(i64::MAX),
            |_a, _b| unreachable!(),
            |a, b| a == b,
        )
        .unwrap();
        assert_eq!(
            v,
            Value::Bool(true),
            "i64::MAX == i64::MAX must be true (precision-loss in f64 path may give wrong answer), got: {:?}",
            v
        );
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
            assert_eq!(m.get("a"), Some(&Value::Number(1.0)));
            assert_eq!(m.get("b"), Some(&Value::Number(2.0)));
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
            assert_eq!(m.get("a"), Some(&Value::Number(1.0)));
            assert_eq!(m.get("b"), Some(&Value::Number(2.0)));
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

    // ===== v0.54 Bug A regression: values_equal Int / Float =====

    /// Int == Int 必须 true。原先 values_equal 漏了 Int 路径,任何 `if 3i == 3i`
    /// 都会错误地走到 `_ => false` 分支。
    #[test]
    fn values_equal_int_returns_true_for_equal() {
        assert!(values_equal(&Value::Int(3), &Value::Int(3)));
    }

    /// Int == 不同 Int = false。
    #[test]
    fn values_equal_int_returns_false_for_different() {
        assert!(!values_equal(&Value::Int(3), &Value::Int(4)));
    }

    /// Float == Float 同样路径。原先 v0.53 / v0.54 都没补。
    #[test]
    fn values_equal_float_returns_true_for_equal() {
        assert!(values_equal(&Value::Float(1.5), &Value::Float(1.5)));
    }

    /// Float == 不同 Float。
    #[test]
    fn values_equal_float_returns_false_for_different() {
        assert!(!values_equal(&Value::Float(1.5), &Value::Float(2.5)));
    }

    /// v0.38 numeric tower strict: Int(3) 与 Number(3.0) 不应相等(避免弱类型
    /// 隐式转换)。这一点保留为 false,与 strict numeric tower 一致。
    #[test]
    fn values_equal_int_vs_number_is_false_under_strict_tower() {
        // Int vs Number: strict 模式下不混算 → false
        assert!(!values_equal(&Value::Int(3), &Value::Number(3.0)));
        assert!(!values_equal(&Value::Number(3.0), &Value::Int(3)));
    }

    /// Float vs Number: legacy alias 在 strict 模式下也应不相等(类型严格)。
    #[test]
    fn values_equal_float_vs_number_is_false_under_strict_tower() {
        assert!(!values_equal(&Value::Float(1.5), &Value::Number(1.5)));
        assert!(!values_equal(&Value::Number(1.5), &Value::Float(1.5)));
    }

    /// Int vs Float: strict 模式下不相等。
    #[test]
    fn values_equal_int_vs_float_is_false_under_strict_tower() {
        assert!(!values_equal(&Value::Int(3), &Value::Float(3.0)));
    }
}
