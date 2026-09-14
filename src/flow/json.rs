//! v0.75.63: JSON 编解码 — 自 flow.rs 拆出（D6 单文件惯例）。
//! json_to_value（手写递归解析）+ value_to_json（序列化）。
//! 零 flow 依赖（纯 Value 转换），经 pub use 保持 flow:: 路径。

use crate::value::Value;

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
    let mut has_decimal = false;
    let mut has_exponent = false;
    if i < s.len() && s.as_bytes()[i] == b'-' {
        i += 1;
    }
    while i < s.len() && s.as_bytes()[i].is_ascii_digit() {
        i += 1;
    }
    if i < s.len() && s.as_bytes()[i] == b'.' {
        has_decimal = true;
        i += 1;
        while i < s.len() && s.as_bytes()[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i < s.len() && (s.as_bytes()[i] == b'e' || s.as_bytes()[i] == b'E') {
        has_exponent = true;
        i += 1;
        if i < s.len() && (s.as_bytes()[i] == b'+' || s.as_bytes()[i] == b'-') {
            i += 1;
        }
        while i < s.len() && s.as_bytes()[i].is_ascii_digit() {
            i += 1;
        }
    }
    let num_str = &s[..i];
    // v0.84: 区分整数 vs 浮点数 — 不含小数点和指数时按 Int 解析，
    // 保持 value_to_json / parse_json_number 的类型对称性。
    // value_to_json: Int(42) → "42"; Float(42.0) → "42.0"
    // parse_json_number: "42" → Int(42); "42.0" → Float(42.0)
    if !has_decimal && !has_exponent {
        // 整数路径：先尝试 i64，溢出时回退 Float
        if let Ok(n) = num_str.parse::<i64>() {
            Ok((Value::Int(n), i))
        } else {
            let num: f64 = num_str
                .parse()
                .map_err(|_| format!("Invalid number: {}", num_str))?;
            Ok((Value::Float(num), i))
        }
    } else {
        let num: f64 = num_str.parse().map_err(|_| format!("Invalid number: {}", num_str))?;
        Ok((Value::Float(num), i))
    }
}

/// Value 转 JSON 字符串
pub fn value_to_json(value: &Value) -> String {
    match value {
        Value::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Value::Char(c) => format!("\"{}\"", c),
        // v0.38: Int formatted without decimal; Float always shows decimal.
        // v0.84: Float 必须始终输出小数点，即使 fract() == 0.0（如 42.0 → "42.0"），
        // 以保持与 parse_json_number 的类型对称性。parse_json_number 对不含小数点的
        // 数字解析为 Int，含小数点的解析为 Float。若 Float 输出 "42"，反序列化后会
        // 变成 Int(42)，类型降级不可逆。
        Value::Int(i) => i.to_string(),
        Value::Float(f) => {
            // 如果 fract() == 0.0，format!("{}", f) 会输出 "42" 无小数点，
            // 与 Int 不可区分。用 "{:.1}" 强制至少一位小数："42.0"。
            if f.fract() == 0.0 {
                format!("{:.1}", f)
            } else {
                format!("{}", f)
            }
        }
        // v0.91: BigInt → JSON 数字字符串（保留精度）。
        Value::BigInt(n) => n.to_string(),
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
        Value::Tool { name, .. } => format!("\"<tool {}>\"", name),
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
        // v0.86: Curry — 柯里化函数无法 JSON 表示，用 null 占位。
        Value::Curry { .. } => "null".to_string(),
        // v0.86: Cons — 链式列表单元用 null 占位（非标准 JSON 结构）。
        Value::Cons { .. } => "null".to_string(),
        // v0.86: Code — 源码文本值用 JSON 字符串表示。
        Value::Code(s) => value_to_json(&Value::String(s.clone())),
        Value::PromptSection { .. } => "null".to_string(),
        Value::Document { backend, .. } => {
            format!("\"<document origin=\\\"{}\\\">\"", backend.origin())
        }
        // v0.83: TEA types — 占位字符串（replay 应通过专用通道恢复）
        Value::TeaApp(_) => "\"<tea_app>\"".to_string(),
        Value::TeaCmd(cmd) => match cmd {
            crate::tea::Cmd::None => "null".to_string(),
            crate::tea::Cmd::Batch(items) => {
                let parts: Vec<String> = items
                    .iter()
                    .map(|c| value_to_json(&c.to_value()))
                    .collect();
                format!("[{}]", parts.join(","))
            }
            crate::tea::Cmd::Perform { effect, args } => {
                let arg_jsons: Vec<String> =
                    args.iter().map(value_to_json).collect();
                format!(
                    "{{\"kind\":\"Perform\",\"effect\":\"{}\",\"args\":[{}}}",
                    effect,
                    arg_jsons.join(",")
                )
            }
            crate::tea::Cmd::Dispatch(msg) => format!(
                "{{\"kind\":\"Dispatch\",\"msg\":{}}}",
                value_to_json(msg)
            ),
        },
        Value::TeaMsg(msg) => value_to_json(&msg.to_value()),
        // v0.102: 声明式范式值 — 无 JSON 形态，序列化为描述串
        Value::Relation { name, clauses } => {
            format!("\"<relation {}/{}>\"", name, clauses.len())
        }
        Value::Goal(_) => "\"<goal>\"".to_string(),
        Value::LogicVar(id) => format!("\"_.{}\"", id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Int / Float 类型对称性（v0.84） ──

    #[test]
    fn int_roundtrip_symmetry() {
        // Int → JSON → Int，类型不变
        let v = Value::Int(42);
        let json = value_to_json(&v);
        assert_eq!(json, "42");
        let v2 = json_to_value(&json).unwrap();
        assert_eq!(v2, Value::Int(42));
    }

    #[test]
    fn float_with_fraction_roundtrip_symmetry() {
        // Float(1.5) → JSON → Float(1.5)，类型不变
        let v = Value::Float(1.5);
        let json = value_to_json(&v);
        assert!(json.starts_with("1.5"));
        let v2 = json_to_value(&json).unwrap();
        match v2 {
            Value::Float(f) => assert!((f - 1.5).abs() < 1e-9),
            other => panic!("expected Float, got {:?}", other),
        }
    }

    #[test]
    fn float_integer_value_uses_decimal_point() {
        // Float(42.0) → "42.0"（含小数点，与 Int(42) → "42" 区分）
        let v = Value::Float(42.0);
        let json = value_to_json(&v);
        assert_eq!(json, "42.0");
        let v2 = json_to_value(&json).unwrap();
        match v2 {
            Value::Float(f) => assert!((f - 42.0).abs() < 1e-9),
            other => panic!("expected Float, got {:?}", other),
        }
    }

    #[test]
    fn negative_int_roundtrip() {
        // Int(-42) → "-42" → Int(-42)
        let v = Value::Int(-42);
        let json = value_to_json(&v);
        assert_eq!(json, "-42");
        let v2 = json_to_value(&json).unwrap();
        assert_eq!(v2, Value::Int(-42));
    }

    #[test]
    fn negative_float_roundtrip() {
        // Float(-1.5) → "-1.5" → Float(-1.5)
        let v = Value::Float(-1.5);
        let json = value_to_json(&v);
        assert!(json.starts_with("-1.5"));
        let v2 = json_to_value(&json).unwrap();
        match v2 {
            Value::Float(f) => assert!((f - (-1.5)).abs() < 1e-9),
            other => panic!("expected Float, got {:?}", other),
        }
    }

    #[test]
    fn zero_int_vs_zero_float_distinct() {
        // Int(0) → "0" → Int(0)
        // Float(0.0) → "0.0" → Float(0.0)
        let ji = value_to_json(&Value::Int(0));
        let jf = value_to_json(&Value::Float(0.0));
        assert_eq!(ji, "0");
        assert_eq!(jf, "0.0");
        assert_eq!(json_to_value(&ji).unwrap(), Value::Int(0));
        match json_to_value(&jf).unwrap() {
            Value::Float(f) => assert!((f - 0.0).abs() < 1e-9),
            other => panic!("expected Float, got {:?}", other),
        }
    }

    #[test]
    fn scientific_notation_parsing_yields_float() {
        // "1e5" → Float(100000.0)，含 e/E 始终 Float
        let v = json_to_value("1e5").unwrap();
        match v {
            Value::Float(f) => assert!((f - 100000.0).abs() < 1e-6),
            other => panic!("expected Float, got {:?}", other),
        }
    }

    #[test]
    fn large_integer_overflow_falls_back_to_float() {
        // 超出 i64 范围的大整数 → Float（避免 panic）
        let v = json_to_value("999999999999999999999").unwrap();
        match v {
            Value::Float(_) => {} // 期望 Float
            other => panic!(
                "expected Float for overflow integer, got {:?}",
                other
            ),
        }
    }

    // ── 嵌套结构中 Int/Float 类型保留 ──

    #[test]
    fn dict_int_value_roundtrip() {
        use std::collections::HashMap;
        let mut map = HashMap::new();
        map.insert("x".to_string(), Value::Int(42));
        map.insert("y".to_string(), Value::Float(1.5));
        let v = Value::Dict(map);
        let json = value_to_json(&v);
        let v2 = json_to_value(&json).unwrap();
        match v2 {
            Value::Dict(m) => {
                assert_eq!(m.get("x"), Some(&Value::Int(42)));
                match m.get("y") {
                    Some(Value::Float(f)) => assert!((f - 1.5).abs() < 1e-9),
                    other => panic!("expected Float(1.5), got {:?}", other),
                }
            }
            other => panic!("expected Dict, got {:?}", other),
        }
    }
}
