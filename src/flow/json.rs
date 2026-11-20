//! v0.110.0: 统一 JSON 编解码内核（RFC 8259 迭代状态机，显式栈 + 深度边界）。
//!
//! 为运行时 `Value`（`json_to_value` / `value_to_json`）与 LSP `lsp::json::Value`
//! 提供单一共享的词法/语法解析引擎、UTF-8/代理对解码与字符串转义实现。

use crate::error::{MoraError, MoraResult};
use crate::value::Value;
use std::collections::HashMap;
use std::fmt::Write as _;

/// JSON 容器最大允许嵌套深度，防止深层嵌套输入导致过度栈/内存消耗。
pub const MAX_JSON_DEPTH: usize = 256;

/// JSON AST 构造策略，供 `flow::json` 与 `lsp::json` 共享同一套解析状态机。
pub(crate) trait JsonBuilder {
    type Output;
    type ArrayAcc;
    type ObjectAcc;

    fn null() -> Self::Output;
    fn bool(v: bool) -> Self::Output;
    fn number(raw: &str, has_decimal_or_exp: bool) -> MoraResult<Self::Output>;
    fn string(s: String) -> Self::Output;

    fn begin_array() -> Self::ArrayAcc;
    fn push_array(arr: &mut Self::ArrayAcc, val: Self::Output);
    fn finish_array(arr: Self::ArrayAcc) -> Self::Output;

    fn begin_object() -> Self::ObjectAcc;
    fn insert_object(obj: &mut Self::ObjectAcc, key: String, val: Self::Output);
    fn finish_object(obj: Self::ObjectAcc) -> Self::Output;
}

struct FlowJsonBuilder;

impl JsonBuilder for FlowJsonBuilder {
    type Output = Value;
    type ArrayAcc = Vec<Value>;
    type ObjectAcc = HashMap<String, Value>;

    fn null() -> Self::Output {
        Value::Nil
    }

    fn bool(v: bool) -> Self::Output {
        Value::Bool(v)
    }

    fn number(raw: &str, has_decimal_or_exp: bool) -> MoraResult<Self::Output> {
        if !has_decimal_or_exp {
            if let Ok(n) = raw.parse::<i64>() {
                Ok(Value::Int(n))
            } else {
                Err(MoraError::Serialization(format!(
                    "Integer literal out of i64 range: {}",
                    raw
                )))
            }
        } else {
            let num: f64 = raw
                .parse()
                .map_err(|_| MoraError::Serialization(format!("Invalid number: {}", raw)))?;
            if !num.is_finite() {
                return Err(MoraError::Serialization(format!(
                    "Number literal overflow: {}",
                    raw
                )));
            }
            Ok(Value::Float(num))
        }
    }

    fn string(s: String) -> Self::Output {
        Value::String(s)
    }

    fn begin_array() -> Self::ArrayAcc {
        Vec::new()
    }

    fn push_array(arr: &mut Self::ArrayAcc, val: Self::Output) {
        arr.push(val);
    }

    fn finish_array(arr: Self::ArrayAcc) -> Self::Output {
        Value::List(arr.into())
    }

    fn begin_object() -> Self::ObjectAcc {
        HashMap::new()
    }

    fn insert_object(obj: &mut Self::ObjectAcc, key: String, val: Self::Output) {
        obj.insert(key, val);
    }

    fn finish_object(obj: Self::ObjectAcc) -> Self::Output {
        Value::Dict(obj)
    }
}

/// JSON 字符串转 Value（统一返回 [`MoraError::Serialization`]）。
pub fn json_to_value(json: &str) -> MoraResult<Value> {
    parse_json_with::<FlowJsonBuilder>(json)
}

enum OpenContainer<B: JsonBuilder> {
    Array(B::ArrayAcc),
    Object {
        acc: B::ObjectAcc,
        pending_key: Option<String>,
    },
}

fn attach_or_set<B: JsonBuilder>(
    stack: &mut [OpenContainer<B>],
    root: &mut Option<B::Output>,
    val: B::Output,
) {
    match stack.last_mut() {
        Some(OpenContainer::Array(arr)) => B::push_array(arr, val),
        Some(OpenContainer::Object { acc, pending_key }) => {
            if let Some(key) = pending_key.take() {
                B::insert_object(acc, key, val);
            }
        }
        None => *root = Some(val),
    }
}

/// 使用显式堆栈与严格 RFC 8259 规则解析单个根 JSON 值。
pub(crate) fn parse_json_with<B: JsonBuilder>(input: &str) -> MoraResult<B::Output> {
    let bytes = input.as_bytes();
    let mut pos = skip_ws(bytes, 0);
    if pos >= bytes.len() {
        return Err(MoraError::Serialization("Empty JSON".to_string()));
    }

    enum Phase {
        ExpectValue,
        AfterValue,
    }

    let mut stack: Vec<OpenContainer<B>> = Vec::new();
    let mut root: Option<B::Output> = None;
    let mut phase = Phase::ExpectValue;

    loop {
        match phase {
            Phase::ExpectValue => {
                pos = skip_ws(bytes, pos);
                if pos >= bytes.len() {
                    return Err(MoraError::Serialization(if stack.is_empty() {
                        "Unexpected end of JSON input".to_string()
                    } else {
                        "Unterminated JSON container".to_string()
                    }));
                }
                match bytes[pos] {
                    b'[' => {
                        if stack.len() >= MAX_JSON_DEPTH {
                            return Err(MoraError::Serialization(format!(
                                "JSON nesting depth exceeds maximum of {MAX_JSON_DEPTH}"
                            )));
                        }
                        pos += 1;
                        pos = skip_ws(bytes, pos);
                        if pos < bytes.len() && bytes[pos] == b']' {
                            pos += 1;
                            attach_or_set::<B>(
                                &mut stack,
                                &mut root,
                                B::finish_array(B::begin_array()),
                            );
                            phase = Phase::AfterValue;
                        } else {
                            stack.push(OpenContainer::Array(B::begin_array()));
                        }
                    }
                    b'{' => {
                        if stack.len() >= MAX_JSON_DEPTH {
                            return Err(MoraError::Serialization(format!(
                                "JSON nesting depth exceeds maximum of {MAX_JSON_DEPTH}"
                            )));
                        }
                        pos += 1;
                        pos = skip_ws(bytes, pos);
                        if pos < bytes.len() && bytes[pos] == b'}' {
                            pos += 1;
                            attach_or_set::<B>(
                                &mut stack,
                                &mut root,
                                B::finish_object(B::begin_object()),
                            );
                            phase = Phase::AfterValue;
                        } else {
                            let key = parse_string_at(bytes, &mut pos)?;
                            pos = skip_ws(bytes, pos);
                            if pos >= bytes.len() || bytes[pos] != b':' {
                                return Err(MoraError::Serialization(
                                    "Expected ':' in JSON object".to_string(),
                                ));
                            }
                            pos += 1;
                            stack.push(OpenContainer::Object {
                                acc: B::begin_object(),
                                pending_key: Some(key),
                            });
                        }
                    }
                    _ => {
                        let val = parse_scalar_at::<B>(bytes, &mut pos)?;
                        attach_or_set::<B>(&mut stack, &mut root, val);
                        phase = Phase::AfterValue;
                    }
                }
            }
            Phase::AfterValue => {
                pos = skip_ws(bytes, pos);
                match stack.pop() {
                    None => {
                        if pos < bytes.len() {
                            return Err(MoraError::Serialization(format!(
                                "Unexpected trailing JSON data at byte {pos}"
                            )));
                        }
                        return root
                            .ok_or_else(|| MoraError::Serialization("Empty JSON".to_string()));
                    }
                    Some(OpenContainer::Array(arr)) => {
                        if pos >= bytes.len() {
                            return Err(MoraError::Serialization(
                                "Unterminated JSON array".to_string(),
                            ));
                        }
                        match bytes[pos] {
                            b',' => {
                                pos += 1;
                                stack.push(OpenContainer::Array(arr));
                                phase = Phase::ExpectValue;
                            }
                            b']' => {
                                pos += 1;
                                attach_or_set::<B>(&mut stack, &mut root, B::finish_array(arr));
                            }
                            other => {
                                return Err(MoraError::Serialization(format!(
                                    "Expected ',' or ']' in JSON array, found '{}'",
                                    other as char
                                )));
                            }
                        }
                    }
                    Some(OpenContainer::Object { acc, .. }) => {
                        if pos >= bytes.len() {
                            return Err(MoraError::Serialization(
                                "Unterminated JSON object".to_string(),
                            ));
                        }
                        match bytes[pos] {
                            b',' => {
                                pos += 1;
                                pos = skip_ws(bytes, pos);
                                let key = parse_string_at(bytes, &mut pos)?;
                                pos = skip_ws(bytes, pos);
                                if pos >= bytes.len() || bytes[pos] != b':' {
                                    return Err(MoraError::Serialization(
                                        "Expected ':' in JSON object".to_string(),
                                    ));
                                }
                                pos += 1;
                                stack.push(OpenContainer::Object {
                                    acc,
                                    pending_key: Some(key),
                                });
                                phase = Phase::ExpectValue;
                            }
                            b'}' => {
                                pos += 1;
                                attach_or_set::<B>(&mut stack, &mut root, B::finish_object(acc));
                            }
                            other => {
                                return Err(MoraError::Serialization(format!(
                                    "Expected ',' or '}}' in JSON object, found '{}'",
                                    other as char
                                )));
                            }
                        }
                    }
                }
            }
        }
    }
}

fn parse_scalar_at<B: JsonBuilder>(bytes: &[u8], pos: &mut usize) -> MoraResult<B::Output> {
    match bytes[*pos] {
        b'"' => parse_string_at(bytes, pos).map(B::string),
        b't' => {
            if bytes[*pos..].starts_with(b"true") {
                *pos += 4;
                Ok(B::bool(true))
            } else {
                Err(MoraError::Serialization("Expected 'true'".to_string()))
            }
        }
        b'f' => {
            if bytes[*pos..].starts_with(b"false") {
                *pos += 5;
                Ok(B::bool(false))
            } else {
                Err(MoraError::Serialization("Expected 'false'".to_string()))
            }
        }
        b'n' => {
            if bytes[*pos..].starts_with(b"null") {
                *pos += 4;
                Ok(B::null())
            } else {
                Err(MoraError::Serialization("Expected 'null'".to_string()))
            }
        }
        b'-' | b'0'..=b'9' => parse_number_at::<B>(bytes, pos),
        other => Err(MoraError::Serialization(format!(
            "Unexpected character in JSON: '{}'",
            other as char
        ))),
    }
}

fn parse_string_at(bytes: &[u8], pos: &mut usize) -> MoraResult<String> {
    if *pos >= bytes.len() || bytes[*pos] != b'"' {
        return Err(MoraError::Serialization("Expected '\"'".to_string()));
    }
    *pos += 1;
    let mut out = String::new();
    while *pos < bytes.len() {
        match bytes[*pos] {
            b'"' => {
                *pos += 1;
                return Ok(out);
            }
            b'\\' => {
                *pos += 1;
                if *pos >= bytes.len() {
                    return Err(MoraError::Serialization(
                        "Unterminated string escape".to_string(),
                    ));
                }
                let esc = bytes[*pos];
                *pos += 1;
                match esc {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{0008}'),
                    b'f' => out.push('\u{000C}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let code = parse_hex4(bytes, pos)?;
                        if (0xD800..0xDC00).contains(&code) {
                            if *pos + 6 > bytes.len()
                                || bytes[*pos] != b'\\'
                                || bytes[*pos + 1] != b'u'
                            {
                                return Err(MoraError::Serialization(format!(
                                    "Invalid escape: unpaired surrogate \\u{:04X}",
                                    code
                                )));
                            }
                            *pos += 2;
                            let lo = parse_hex4(bytes, pos)?;
                            if !(0xDC00..0xE000).contains(&lo) {
                                return Err(MoraError::Serialization(format!(
                                    "Invalid escape: \\u{:04X} is not followed by a low surrogate",
                                    code
                                )));
                            }
                            let combined = 0x10000 + ((code - 0xD800) << 10) + (lo - 0xDC00);
                            let ch = char::from_u32(combined).ok_or_else(|| {
                                MoraError::Serialization(
                                    "Invalid escape: bad surrogate pair".to_string(),
                                )
                            })?;
                            out.push(ch);
                        } else if (0xDC00..0xE000).contains(&code) {
                            return Err(MoraError::Serialization(format!(
                                "Invalid escape: unpaired low surrogate \\u{:04X}",
                                code
                            )));
                        } else {
                            let ch = char::from_u32(code).ok_or_else(|| {
                                MoraError::Serialization(format!("Invalid escape: \\u{:04X}", code))
                            })?;
                            out.push(ch);
                        }
                    }
                    other => {
                        return Err(MoraError::Serialization(format!(
                            "Invalid escape: \\{}",
                            other as char
                        )));
                    }
                }
            }
            c if c < 0x20 => {
                return Err(MoraError::Serialization(format!(
                    "Unescaped control character 0x{:02X} in JSON string",
                    c
                )));
            }
            _ => {
                let start = *pos;
                while *pos < bytes.len()
                    && bytes[*pos] != b'"'
                    && bytes[*pos] != b'\\'
                    && bytes[*pos] >= 0x20
                {
                    *pos += 1;
                }
                let chunk = std::str::from_utf8(&bytes[start..*pos]).map_err(|e| {
                    MoraError::Serialization(format!("Invalid UTF-8 in JSON string: {}", e))
                })?;
                out.push_str(chunk);
            }
        }
    }
    Err(MoraError::Serialization("Unterminated string".to_string()))
}

fn parse_hex4(bytes: &[u8], pos: &mut usize) -> MoraResult<u32> {
    if *pos + 4 > bytes.len() {
        return Err(MoraError::Serialization("Truncated \\u escape".to_string()));
    }
    let hex = std::str::from_utf8(&bytes[*pos..*pos + 4])
        .map_err(|e| MoraError::Serialization(format!("Invalid \\u escape: {}", e)))?;
    *pos += 4;
    u32::from_str_radix(hex, 16)
        .map_err(|_| MoraError::Serialization(format!("Invalid \\u escape: \\u{}", hex)))
}

fn parse_number_at<B: JsonBuilder>(bytes: &[u8], pos: &mut usize) -> MoraResult<B::Output> {
    let start = *pos;
    if bytes.get(*pos) == Some(&b'-') {
        *pos += 1;
    }
    match bytes.get(*pos) {
        Some(b'0') => {
            *pos += 1;
            if bytes.get(*pos).is_some_and(u8::is_ascii_digit) {
                return Err(MoraError::Serialization(
                    "Leading zero in JSON number".to_string(),
                ));
            }
        }
        Some(b'1'..=b'9') => {
            *pos += 1;
            while *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
                *pos += 1;
            }
        }
        _ => {
            return Err(MoraError::Serialization(
                "Expected digit in JSON number".to_string(),
            ));
        }
    }

    let mut has_decimal_or_exp = false;
    if bytes.get(*pos) == Some(&b'.') {
        has_decimal_or_exp = true;
        *pos += 1;
        if !bytes.get(*pos).is_some_and(u8::is_ascii_digit) {
            return Err(MoraError::Serialization(
                "Missing digits after decimal point in JSON number".to_string(),
            ));
        }
        while *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
            *pos += 1;
        }
    }

    if matches!(bytes.get(*pos), Some(b'e' | b'E')) {
        has_decimal_or_exp = true;
        *pos += 1;
        if matches!(bytes.get(*pos), Some(b'+' | b'-')) {
            *pos += 1;
        }
        if !bytes.get(*pos).is_some_and(u8::is_ascii_digit) {
            return Err(MoraError::Serialization(
                "Missing digits in JSON number exponent".to_string(),
            ));
        }
        while *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
            *pos += 1;
        }
    }

    let raw = std::str::from_utf8(&bytes[start..*pos])
        .map_err(|e| MoraError::Serialization(format!("Invalid number encoding: {}", e)))?;
    B::number(raw, has_decimal_or_exp)
}

/// 跳过 RFC 8259 空白字符（仅空格 / `\t` / `\n` / `\r`）。
fn skip_ws(s: &[u8], mut i: usize) -> usize {
    while i < s.len() {
        match s[i] {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            _ => break,
        }
    }
    i
}

/// JSON 字符串转义（RFC 8259）—— 返回**不含**外层引号的转义结果。
pub fn escape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    write_escaped_json_string(&mut out, s);
    out
}

pub(crate) fn write_escaped_json_string(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
}

/// Value 转 JSON 字符串（显式栈迭代实现，避免深层嵌套爆栈）。
pub fn value_to_json(value: &Value) -> String {
    use std::borrow::Cow;

    enum SerStep<'a> {
        Val(Cow<'a, Value>, usize),
        Raw(&'static str),
        KeyQuoted(String),
    }

    let mut out = String::new();
    let mut stack = vec![SerStep::Val(Cow::Borrowed(value), 0)];

    while let Some(step) = stack.pop() {
        match step {
            SerStep::Raw(s) => out.push_str(s),
            SerStep::KeyQuoted(k) => {
                out.push('"');
                write_escaped_json_string(&mut out, &k);
                out.push_str("\":");
            }
            SerStep::Val(cow, atom_depth) => match cow.as_ref() {
                Value::String(s) | Value::Code(s) => {
                    out.push('"');
                    write_escaped_json_string(&mut out, s);
                    out.push('"');
                }
                Value::Char(c) => {
                    let mut buf = [0u8; 4];
                    let s = c.encode_utf8(&mut buf);
                    out.push('"');
                    write_escaped_json_string(&mut out, s);
                    out.push('"');
                }
                Value::Int(i) => {
                    let _ = write!(out, "{}", i);
                }
                Value::Scalar(s) => {
                    if s.dtype().is_float() {
                        let f = s.as_f64();
                        if !f.is_finite() {
                            out.push_str("null");
                        } else if f.fract() == 0.0 {
                            let _ = write!(out, "{:.1}", f);
                        } else {
                            let _ = write!(out, "{}", f);
                        }
                    } else {
                        let _ = write!(out, "{}", s);
                    }
                }
                Value::Float(f) => {
                    if !f.is_finite() {
                        out.push_str("null");
                    } else if f.fract() == 0.0 {
                        let _ = write!(out, "{:.1}", f);
                    } else {
                        let _ = write!(out, "{}", f);
                    }
                }
                Value::Bool(true) => out.push_str("true"),
                Value::Bool(false) => out.push_str("false"),
                Value::Nil
                | Value::Compose(_)
                | Value::Partial(_, _)
                | Value::Macro { .. }
                | Value::Curry { .. }
                | Value::Cons { .. } => out.push_str("null"),
                Value::Task { name, .. } => {
                    out.push_str("\"<task ");
                    write_escaped_json_string(&mut out, name);
                    out.push_str(">\"");
                }
                Value::Closure { .. } => out.push_str("\"<closure>\""),
                Value::Builtin(name) => {
                    let _ = write!(out, "\"<builtin {}>\"", name);
                }
                Value::TraitObject { .. } => out.push_str("\"<trait_object>\""),
                Value::Continuation(_) => out.push_str("\"<continuation>\""),
                Value::Channel(_) => out.push_str("\"<channel>\""),
                Value::TaskHandle(_) => out.push_str("\"<task_handle>\""),
                Value::Context(_) => out.push_str("\"<context>\""),
                Value::TcpListener(_) => out.push_str("\"<tcp_listener>\""),
                Value::TcpStream(_) => out.push_str("\"<tcp_stream>\""),
                Value::ComputeDag(_) => out.push_str("\"<compute_dag>\""),
                Value::KVAllocator(_) => out.push_str("\"<kv_allocator>\""),
                Value::External(obj) => match obj.to_json() {
                    Some(v) if atom_depth < MAX_JSON_DEPTH => {
                        stack.push(SerStep::Val(Cow::Owned(v), atom_depth + 1));
                    }
                    Some(_) => out.push_str("null"),
                    None => {
                        out.push_str("\"<external:");
                        write_escaped_json_string(&mut out, obj.type_name());
                        out.push_str(">\"");
                    }
                },
                Value::Atom(arc) => {
                    if atom_depth >= MAX_JSON_DEPTH {
                        out.push_str("null");
                    } else {
                        let inner = arc.lock().clone();
                        stack.push(SerStep::Val(Cow::Owned(inner), atom_depth + 1));
                    }
                }
                Value::List(_) => {
                    out.push('[');
                    stack.push(SerStep::Raw("]"));
                    let children: Vec<_> = match cow {
                        Cow::Borrowed(Value::List(items)) => {
                            items.iter().map(Cow::Borrowed).collect()
                        }
                        Cow::Owned(Value::List(items)) => {
                            items.iter().cloned().map(Cow::Owned).collect()
                        }
                        _ => unreachable!(),
                    };
                    for (idx, item) in children.into_iter().enumerate().rev() {
                        stack.push(SerStep::Val(item, atom_depth));
                        if idx > 0 {
                            stack.push(SerStep::Raw(","));
                        }
                    }
                }
                Value::Tensor(t) => {
                    stack.push(SerStep::Val(Cow::Owned(t.to_nested_list()), atom_depth));
                }
                Value::StructType(def) => {
                    stack.push(SerStep::Val(Cow::Owned(def.to_schema_value()), atom_depth));
                }
                Value::StructInstance {
                    fields,
                    field_order,
                    def,
                    ..
                } => {
                    out.push('{');
                    stack.push(SerStep::Raw("}"));
                    let mut projected: Vec<(String, Value)> = Vec::new();
                    for fname in field_order {
                        let Some(val) = fields.get(fname) else {
                            continue;
                        };
                        let json_tag = def.fields.iter().find(|f| &f.name == fname).and_then(|f| {
                            f.tags
                                .iter()
                                .find(|(k, _)| k == "json")
                                .map(|(_, v)| v.as_str())
                        });
                        let mut key = fname.clone();
                        let mut omit_empty = false;
                        if let Some(tag) = json_tag {
                            if tag == "-" {
                                continue;
                            }
                            let mut parts = tag.split(',');
                            if let Some(alias) = parts.next()
                                && !alias.is_empty()
                            {
                                key = alias.to_string();
                            }
                            for opt in parts {
                                if opt.trim() == "omitempty" {
                                    omit_empty = true;
                                }
                            }
                        }
                        if omit_empty && is_json_empty_value(val) {
                            continue;
                        }
                        projected.push((key, val.clone()));
                    }
                    for (idx, (k, val)) in projected.into_iter().enumerate().rev() {
                        stack.push(SerStep::Val(Cow::Owned(val), atom_depth));
                        stack.push(SerStep::KeyQuoted(k));
                        if idx > 0 {
                            stack.push(SerStep::Raw(","));
                        }
                    }
                }
                Value::EnumType(def) => {
                    out.push_str("\"<enum ");
                    write_escaped_json_string(&mut out, &def.name);
                    out.push_str(">\"");
                }
                Value::EnumVariant {
                    variant, payload, ..
                } => {
                    if payload.is_empty() {
                        out.push('"');
                        write_escaped_json_string(&mut out, variant);
                        out.push('"');
                    } else {
                        out.push('{');
                        stack.push(SerStep::Raw("}"));
                        let inner = if payload.len() == 1 {
                            payload[0].clone()
                        } else {
                            Value::List(payload.clone().into())
                        };
                        stack.push(SerStep::Val(Cow::Owned(inner), atom_depth));
                        stack.push(SerStep::KeyQuoted(variant.clone()));
                    }
                }
                Value::GradFn { .. } => out.push_str("\"<grad>\""),
                Value::MissingArg => out.push_str("null"),
                Value::Dict(_) => {
                    out.push('{');
                    stack.push(SerStep::Raw("}"));
                    let mut entries: Vec<_> = match cow {
                        Cow::Borrowed(Value::Dict(map)) => map
                            .iter()
                            .map(|(k, v)| (k.clone(), Cow::Borrowed(v)))
                            .collect(),
                        Cow::Owned(Value::Dict(map)) => {
                            map.into_iter().map(|(k, v)| (k, Cow::Owned(v))).collect()
                        }
                        _ => unreachable!(),
                    };
                    entries.sort_by(|a, b| a.0.cmp(&b.0));
                    for (idx, (k, val)) in entries.into_iter().enumerate().rev() {
                        stack.push(SerStep::Val(val, atom_depth));
                        stack.push(SerStep::KeyQuoted(k));
                        if idx > 0 {
                            stack.push(SerStep::Raw(","));
                        }
                    }
                }
            },
        }
    }

    out
}

fn is_json_empty_value(v: &Value) -> bool {
    match v {
        Value::Nil | Value::MissingArg => true,
        Value::Bool(false) => true,
        Value::Int(0) => true,
        Value::Float(f) if *f == 0.0 => true,
        Value::String(s) => s.is_empty(),
        Value::List(l) => l.is_empty(),
        Value::Dict(d) => d.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_roundtrip_symmetry() {
        let v = Value::Int(42);
        let json = value_to_json(&v);
        assert_eq!(json, "42");
        let v2 = json_to_value(&json).unwrap();
        assert_eq!(v2, Value::Int(42));
    }

    #[test]
    fn float_with_fraction_roundtrip_symmetry() {
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
        let v = Value::Int(-42);
        let json = value_to_json(&v);
        assert_eq!(json, "-42");
        let v2 = json_to_value(&json).unwrap();
        assert_eq!(v2, Value::Int(-42));
    }

    #[test]
    fn negative_float_roundtrip() {
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
        let v = json_to_value("1e5").unwrap();
        match v {
            Value::Float(f) => assert!((f - 100000.0).abs() < 1e-6),
            other => panic!("expected Float, got {:?}", other),
        }
    }

    #[test]
    fn large_integer_overflow_returns_explicit_error() {
        let err = json_to_value("999999999999999999999")
            .expect_err("out-of-i64 integer should return explicit range error");
        assert!(err.to_string().contains("out of i64 range"), "got: {err}");
    }

    #[test]
    fn rejects_trailing_data_and_malformed_numbers() {
        for bad in [
            "007",
            "0x1F",
            "1.2.3",
            "5.",
            ".5",
            "+7",
            "1e",
            "1e+",
            r#"{"a":1}xyz"#,
            "1 2",
            "truefalse",
            "\"\n\"",
        ] {
            assert!(json_to_value(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn enforces_max_json_depth_iteratively() {
        let ok = format!(
            "{}1{}",
            "[".repeat(MAX_JSON_DEPTH),
            "]".repeat(MAX_JSON_DEPTH)
        );
        assert!(json_to_value(&ok).is_ok());

        let too_deep = format!(
            "{}1{}",
            "[".repeat(MAX_JSON_DEPTH + 1),
            "]".repeat(MAX_JSON_DEPTH + 1)
        );
        let err = json_to_value(&too_deep).expect_err("depth > MAX_JSON_DEPTH must fail");
        assert!(err.to_string().contains("depth"), "got: {err}");
    }

    #[test]
    fn dict_int_value_roundtrip() {
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
