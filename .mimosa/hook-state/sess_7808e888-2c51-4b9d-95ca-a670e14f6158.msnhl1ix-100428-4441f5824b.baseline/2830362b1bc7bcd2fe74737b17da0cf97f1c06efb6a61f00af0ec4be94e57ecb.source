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
    Ok((Value::Float(num), i))
}

/// Value 转 JSON 字符串
pub fn value_to_json(value: &Value) -> String {
    match value {
        Value::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Value::Char(c) => format!("\"{}\"", c),
        // v0.38: Int formatted without decimal; Float always shows decimal.
        Value::Int(i) => i.to_string(),
        Value::Float(f) => {
            if f.fract() == 0.0 {
                format!("{:.0}", f)
            } else {
                format!("{}", f)
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
        Value::PromptSection { .. } => "null".to_string(),
        Value::Document { backend, .. } => {
            format!("\"<document origin=\\\"{}\\\">\"", backend.origin())
        }
    }
}
