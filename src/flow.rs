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
pub fn eval_binary(left: Value, op: &BinaryOp, right: Value) -> Result<Value, String> {
    match op {
        BinaryOp::Add => match (&left, &right) {
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
pub fn numeric_op<F>(left: Value, right: Value, op: F) -> Result<Value, String>
where
    F: Fn(f64, f64) -> f64,
{
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => Ok(Value::Number(op(a, b))),
        // v0.17: 广播操作 - list op number
        (Value::List(list), Value::Number(scalar)) => {
            let result: Vec<Value> = list
                .iter()
                .map(|item| match item {
                    Value::Number(n) => Value::Number(op(*n, scalar)),
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
                    Value::Number(n) => Value::Number(op(scalar, *n)),
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
                    (Value::Number(xn), Value::Number(yn)) => Value::Number(op(*xn, *yn)),
                    _ => Value::Nil,
                })
                .collect();
            Ok(Value::List(result))
        }
        _ => Err("Operands must be numbers".to_string()),
    }
}

/// 数值比较辅助
pub fn numeric_cmp<F>(left: Value, right: Value, op: F) -> Result<Value, String>
where
    F: Fn(f64, f64) -> bool,
{
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => Ok(Value::Bool(op(a, b))),
        _ => Err("Operands must be numbers".to_string()),
    }
}

/// 值相等比较
pub fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Nil, Value::Nil) => true,
        (Value::Number(a), Value::Number(b)) => a == b,
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
fn parse_json_value(s: &str) -> Result<(Value, usize), String> {
    let s = s.trim_start();
    if s.is_empty() {
        return Err("Empty JSON value".to_string());
    }
    match s.as_bytes()[0] {
        b'"' => parse_json_string(s),
        b'[' => parse_json_list(s),
        b'{' => parse_json_dict(s),
        b't' | b'f' => parse_json_bool(s),
        b'n' => parse_json_null(s),
        b'0'..=b'9' | b'-' => parse_json_number(s),
        _ => Err(format!("Unexpected character in JSON: {}", s)),
    }
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

fn parse_json_list(s: &str) -> Result<(Value, usize), String> {
    if s.as_bytes()[0] != b'[' {
        return Err("Expected '['".to_string());
    }
    let mut items = Vec::new();
    let mut i = 1;
    loop {
        let rest = &s[i..].trim_start();
        if rest.is_empty() {
            return Err("Unterminated list".to_string());
        }
        if rest.as_bytes()[0] == b']' {
            i += s.len() - i - rest.len() + 1;
            break;
        }
        if !items.is_empty() {
            if rest.as_bytes()[0] != b',' {
                return Err("Expected ',' in list".to_string());
            }
            i += 1;
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
    let mut map = std::collections::HashMap::new();
    let mut i = 1;
    loop {
        let rest = &s[i..].trim_start();
        if rest.is_empty() {
            return Err("Unterminated dict".to_string());
        }
        if rest.as_bytes()[0] == b'}' {
            i += s.len() - i - rest.len() + 1;
            break;
        }
        if !map.is_empty() {
            if rest.as_bytes()[0] != b',' {
                return Err("Expected ',' in dict".to_string());
            }
            i += 1;
        }
        let (key, key_consumed) = parse_json_string(&s[i..])?;
        let key_str = match key {
            Value::String(s) => s,
            _ => unreachable!(),
        };
        i += key_consumed;
        let rest = &s[i..].trim_start();
        if rest.is_empty() || rest.as_bytes()[0] != b':' {
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
        Value::Atom(arc) => value_to_json(&arc.lock().expect("Atom mutex poisoned")),
        Value::Macro { .. } => "null".to_string(),
        Value::PromptSection { .. } => "null".to_string(),
        Value::Document { backend, .. } => {
            format!("\"<document origin=\\\"{}\\\">\"", backend.origin())
        }
    }
}
