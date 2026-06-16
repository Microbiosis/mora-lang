//! 最小手写 JSON Value + serialize/parse 子集
//!
//! 只支持 LSP 用到的类型：
//! - object: HashMap<String, Value>
//! - array: Vec<Value>
//! - string, number, bool, null
//!
//! 故意不支持：嵌套深度限制宽松（不做 strict JSON）、不做性能优化。
//! 目的：让 LSP server 零依赖工作。

use std::collections::BTreeMap;
use std::fmt;

// ===================================================================
// Value
// ===================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String_(String),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),  // BTreeMap 让序列化确定性
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        if let Value::String_(s) = self { Some(s.as_str()) } else { None }
    }

    pub fn as_i64(&self) -> Option<i64> {
        if let Value::Number(n) = self { Some(*n as i64) } else { None }
    }

    pub fn as_object(&self) -> Option<&BTreeMap<String, Value>> {
        if let Value::Object(m) = self { Some(m) } else { None }
    }

    pub fn as_array(&self) -> Option<&Vec<Value>> {
        if let Value::Array(a) = self { Some(a) } else { None }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_object()?.get(key)
    }
}

// ===================================================================
// Serialize
// ===================================================================

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_value(f, self)
    }
}

fn write_value(f: &mut fmt::Formatter<'_>, v: &Value) -> fmt::Result {
    match v {
        Value::Null => f.write_str("null"),
        Value::Bool(b) => f.write_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_finite() {
                // 整数 if 是整数，否则 %g
                if n.fract() == 0.0 && n.abs() < 1e15 {
                    write!(f, "{}", *n as i64)
                } else {
                    write!(f, "{}", n)
                }
            } else {
                f.write_str("null")
            }
        }
        Value::String_(s) => write!(f, "\"{}\"", escape_string(s)),
        Value::Array(items) => {
            f.write_str("[")?;
            for (i, it) in items.iter().enumerate() {
                if i > 0 { f.write_str(",")?; }
                write_value(f, it)?;
            }
            f.write_str("]")
        }
        Value::Object(map) => {
            f.write_str("{")?;
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 { f.write_str(",")?; }
                write!(f, "\"{}\":", escape_string(k))?;
                write_value(f, v)?;
            }
            f.write_str("}")
        }
    }
}

fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

// ===================================================================
// Parse
// ===================================================================

pub struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    pub fn new(s: &'a str) -> Self {
        Self { bytes: s.as_bytes(), pos: 0 }
    }

    pub fn parse_value(&mut self) -> Result<Value, String> {
        self.skip_ws();
        if self.pos >= self.bytes.len() {
            return Err("unexpected end of input".to_string());
        }
        match self.bytes[self.pos] {
            b'n' => self.parse_literal("null", Value::Null),
            b't' => self.parse_literal("true", Value::Bool(true)),
            b'f' => self.parse_literal("false", Value::Bool(false)),
            b'"' => Ok(Value::String_(self.parse_string()?)),
            b'[' => self.parse_array(),
            b'{' => self.parse_object(),
            b'-' | b'0'..=b'9' => self.parse_number(),
            other => Err(format!("unexpected char: {}", other as char)),
        }
    }

    fn parse_literal(&mut self, lit: &str, val: Value) -> Result<Value, String> {
        let bytes = lit.as_bytes();
        if self.bytes[self.pos..].starts_with(bytes) {
            self.pos += bytes.len();
            Ok(val)
        } else {
            Err(format!("expected literal '{}'", lit))
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        if self.bytes[self.pos] != b'"' {
            return Err("expected '\"'".to_string());
        }
        self.pos += 1;
        let mut out = String::new();
        while self.pos < self.bytes.len() {
            let c = self.bytes[self.pos];
            match c {
                b'"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.pos += 1;
                    if self.pos >= self.bytes.len() {
                        return Err("unterminated escape".to_string());
                    }
                    let esc = self.bytes[self.pos];
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000C}'),
                        b'u' => {
                            // 4 hex digits
                            if self.pos + 4 > self.bytes.len() {
                                return Err("short unicode escape".to_string());
                            }
                            let hex = std::str::from_utf8(&self.bytes[self.pos..self.pos + 4])
                                .map_err(|e| e.to_string())?;
                            self.pos += 4;
                            let code = u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
                            if let Some(ch) = char::from_u32(code) {
                                out.push(ch);
                            } else {
                                return Err(format!("invalid unicode: {}", code));
                            }
                        }
                        other => return Err(format!("unknown escape: \\{}", other as char)),
                    }
                }
                _ => {
                    out.push(c as char);
                    self.pos += 1;
                }
            }
        }
        Err("unterminated string".to_string())
    }

    fn parse_number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        if self.bytes[self.pos] == b'-' { self.pos += 1; }
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        if self.pos < self.bytes.len() && self.bytes[self.pos] == b'.' {
            self.pos += 1;
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }
        if self.pos < self.bytes.len() && (self.bytes[self.pos] == b'e' || self.bytes[self.pos] == b'E') {
            self.pos += 1;
            if self.pos < self.bytes.len() && (self.bytes[self.pos] == b'+' || self.bytes[self.pos] == b'-') {
                self.pos += 1;
            }
            while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }
        let s = std::str::from_utf8(&self.bytes[start..self.pos]).map_err(|e| e.to_string())?;
        s.parse::<f64>()
            .map(Value::Number)
            .map_err(|e| format!("invalid number: {}", e))
    }

    fn parse_array(&mut self) -> Result<Value, String> {
        self.pos += 1; // consume '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.pos < self.bytes.len() && self.bytes[self.pos] == b']' {
            self.pos += 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.parse_value()?);
            self.skip_ws();
            if self.pos >= self.bytes.len() {
                return Err("unterminated array".to_string());
            }
            match self.bytes[self.pos] {
                b',' => { self.pos += 1; }
                b']' => { self.pos += 1; return Ok(Value::Array(items)); }
                _ => return Err("expected ',' or ']'".to_string()),
            }
        }
    }

    fn parse_object(&mut self) -> Result<Value, String> {
        self.pos += 1; // consume '{'
        let mut map = BTreeMap::new();
        self.skip_ws();
        if self.pos < self.bytes.len() && self.bytes[self.pos] == b'}' {
            self.pos += 1;
            return Ok(Value::Object(map));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            if self.pos >= self.bytes.len() || self.bytes[self.pos] != b':' {
                return Err("expected ':'".to_string());
            }
            self.pos += 1;
            self.skip_ws();
            let val = self.parse_value()?;
            map.insert(key, val);
            self.skip_ws();
            if self.pos >= self.bytes.len() {
                return Err("unterminated object".to_string());
            }
            match self.bytes[self.pos] {
                b',' => { self.pos += 1; }
                b'}' => { self.pos += 1; return Ok(Value::Object(map)); }
                _ => return Err("expected ',' or '}'".to_string()),
            }
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }
}

// ===================================================================
// 公共 API
// ===================================================================

/// 解析 JSON 字符串为 Value
pub fn parse(s: &str) -> Result<Value, String> {
    Parser::new(s).parse_value()
}

/// Value 转 JSON 字符串
pub fn to_string(v: &Value) -> String {
    format!("{}", v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_primitives() {
        assert_eq!(Parser::new("null").parse_value().unwrap(), Value::Null);
        assert_eq!(Parser::new("true").parse_value().unwrap(), Value::Bool(true));
        assert_eq!(Parser::new("false").parse_value().unwrap(), Value::Bool(false));
        assert_eq!(Parser::new("42").parse_value().unwrap(), Value::Number(42.0));
        assert_eq!(Parser::new("3.14").parse_value().unwrap(), Value::Number(3.14));
        assert_eq!(Parser::new("-1").parse_value().unwrap(), Value::Number(-1.0));
        assert_eq!(Parser::new("\"hi\"").parse_value().unwrap(), Value::String_("hi".to_string()));
    }

    #[test]
    fn parse_array() {
        let v = Parser::new("[1, 2, 3]").parse_value().unwrap();
        assert_eq!(v, Value::Array(vec![Value::Number(1.0), Value::Number(2.0), Value::Number(3.0)]));
    }

    #[test]
    fn parse_object() {
        let v = Parser::new(r#"{"a": 1, "b": "x"}"#).parse_value().unwrap();
        if let Value::Object(m) = v {
            assert_eq!(m.get("a"), Some(&Value::Number(1.0)));
            assert_eq!(m.get("b"), Some(&Value::String_("x".to_string())));
        } else { panic!(); }
    }

    #[test]
    fn parse_nested() {
        let s = r#"{"jsonrpc":"2.0","id":1,"params":{"x":[1,2]}}"#;
        let v = Parser::new(s).parse_value().unwrap();
        if let Value::Object(m) = v {
            assert_eq!(m.get("jsonrpc").and_then(|v| v.as_str()), Some("2.0"));
            assert_eq!(m.get("id").and_then(|v| v.as_i64()), Some(1));
        } else { panic!(); }
    }

    #[test]
    fn serialize_roundtrip() {
        let original = Value::Object([
            ("a".to_string(), Value::Number(1.0)),
            ("b".to_string(), Value::String_("hi\n".to_string())),
            ("c".to_string(), Value::Array(vec![Value::Bool(true), Value::Null])),
        ].iter().cloned().collect());
        let s = original.to_string();
        let parsed = Parser::new(&s).parse_value().unwrap();
        assert_eq!(original, parsed);
    }
}
