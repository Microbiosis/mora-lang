//! v0.110.0: LSP JSON-RPC 2.0 轻量值表示。
//!
//! 解析与字符串转义统一复用 `crate::flow::json` 的显式栈 RFC 8259 状态机
//! 与 `escape_json_string`，仅保留 LSP 协议所需的 `Value`（`Number(f64)` 整数写法，
//! 如 `6.0 -> "6"` 以符合 `Range.line` 等协议字段线格式）。

use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Write as _;

use crate::error::{MoraError, MoraResult};
use crate::flow::json::{JsonBuilder, parse_json_with, write_escaped_json_string};

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
    Object(BTreeMap<String, Value>), // BTreeMap 让序列化确定性
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        if let Value::String_(s) = self {
            Some(s.as_str())
        } else {
            None
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        if let Value::Number(n) = self {
            Some(*n as i64)
        } else {
            None
        }
    }

    pub fn as_object(&self) -> Option<&BTreeMap<String, Value>> {
        if let Value::Object(m) = self {
            Some(m)
        } else {
            None
        }
    }

    pub fn as_array(&self) -> Option<&Vec<Value>> {
        if let Value::Array(a) = self {
            Some(a)
        } else {
            None
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_object()?.get(key)
    }
}

// ===================================================================
// Serialize (显式栈 + 共享转义器)
// ===================================================================

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&to_string(self))
    }
}

/// Value 转 JSON 字符串（显式栈迭代实现，复用共享 RFC 8259 转义）。
pub fn to_string(v: &Value) -> String {
    enum Step<'a> {
        Val(&'a Value),
        Raw(&'static str),
        Key(&'a str),
    }

    let mut out = String::new();
    let mut stack = vec![Step::Val(v)];

    while let Some(step) = stack.pop() {
        match step {
            Step::Raw(s) => out.push_str(s),
            Step::Key(k) => {
                out.push('"');
                write_escaped_json_string(&mut out, k);
                out.push_str("\":");
            }
            Step::Val(cur) => match cur {
                Value::Null => out.push_str("null"),
                Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
                Value::Number(n) => {
                    if n.is_finite() {
                        if n.fract() == 0.0 && n.abs() < 1e15 {
                            let _ = write!(out, "{}", *n as i64);
                        } else {
                            let _ = write!(out, "{}", n);
                        }
                    } else {
                        out.push_str("null");
                    }
                }
                Value::String_(s) => {
                    out.push('"');
                    write_escaped_json_string(&mut out, s);
                    out.push('"');
                }
                Value::Array(items) => {
                    out.push('[');
                    stack.push(Step::Raw("]"));
                    for (idx, it) in items.iter().enumerate().rev() {
                        stack.push(Step::Val(it));
                        if idx > 0 {
                            stack.push(Step::Raw(","));
                        }
                    }
                }
                Value::Object(map) => {
                    out.push('{');
                    stack.push(Step::Raw("}"));
                    for (idx, (k, val)) in map.iter().enumerate().rev() {
                        stack.push(Step::Val(val));
                        stack.push(Step::Key(k.as_str()));
                        if idx > 0 {
                            stack.push(Step::Raw(","));
                        }
                    }
                }
            },
        }
    }

    out
}

// ===================================================================
// Parse (委托 flow::json 统一显式栈解析内核)
// ===================================================================

struct LspJsonBuilder;

impl JsonBuilder for LspJsonBuilder {
    type Output = Value;
    type ArrayAcc = Vec<Value>;
    type ObjectAcc = BTreeMap<String, Value>;

    fn null() -> Self::Output {
        Value::Null
    }

    fn bool(v: bool) -> Self::Output {
        Value::Bool(v)
    }

    fn number(raw: &str, _has_decimal_or_exp: bool) -> MoraResult<Self::Output> {
        let num: f64 = raw
            .parse()
            .map_err(|_| MoraError::Serialization(format!("Invalid number: {}", raw)))?;
        if !num.is_finite() {
            return Err(MoraError::Serialization(format!(
                "Number literal overflow: {}",
                raw
            )));
        }
        Ok(Value::Number(num))
    }

    fn string(s: String) -> Self::Output {
        Value::String_(s)
    }

    fn begin_array() -> Self::ArrayAcc {
        Vec::new()
    }

    fn push_array(arr: &mut Self::ArrayAcc, val: Self::Output) {
        arr.push(val);
    }

    fn finish_array(arr: Self::ArrayAcc) -> Self::Output {
        Value::Array(arr)
    }

    fn begin_object() -> Self::ObjectAcc {
        BTreeMap::new()
    }

    fn insert_object(obj: &mut Self::ObjectAcc, key: String, val: Self::Output) {
        obj.insert(key, val);
    }

    fn finish_object(obj: Self::ObjectAcc) -> Self::Output {
        Value::Object(obj)
    }
}

/// 兼容历史调用形态 `Parser::new(s).parse_value()` 的零开销包装器（委托统一内核）。
pub struct Parser<'a> {
    src: &'a str,
}

impl<'a> Parser<'a> {
    pub fn new(s: &'a str) -> Self {
        Self { src: s }
    }

    pub fn parse_value(&mut self) -> MoraResult<Value> {
        parse(self.src)
    }
}

/// 解析 JSON 字符串为 LSP [`Value`]。
pub fn parse(s: &str) -> MoraResult<Value> {
    parse_json_with::<LspJsonBuilder>(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_primitives() {
        assert_eq!(Parser::new("null").parse_value().unwrap(), Value::Null);
        assert_eq!(
            Parser::new("true").parse_value().unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            Parser::new("false").parse_value().unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            Parser::new("42").parse_value().unwrap(),
            Value::Number(42.0)
        );
        assert_eq!(
            Parser::new("1.5").parse_value().unwrap(),
            Value::Number(1.5)
        );
        assert_eq!(
            Parser::new("-1").parse_value().unwrap(),
            Value::Number(-1.0)
        );
        assert_eq!(
            Parser::new("\"hi\"").parse_value().unwrap(),
            Value::String_("hi".to_string())
        );
    }

    #[test]
    fn parse_array() {
        let v = Parser::new("[1, 2, 3]").parse_value().unwrap();
        assert_eq!(
            v,
            Value::Array(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0)
            ])
        );
    }

    #[test]
    fn parse_object() {
        let v = Parser::new(r#"{"a": 1, "b": "x"}"#).parse_value().unwrap();
        if let Value::Object(m) = v {
            assert_eq!(m.get("a"), Some(&Value::Number(1.0)));
            assert_eq!(m.get("b"), Some(&Value::String_("x".to_string())));
        } else {
            panic!();
        }
    }

    #[test]
    fn parse_nested() {
        let s = r#"{"jsonrpc":"2.0","id":1,"params":{"x":[1,2]}}"#;
        let v = Parser::new(s).parse_value().unwrap();
        if let Value::Object(m) = v {
            assert_eq!(m.get("jsonrpc").and_then(|v| v.as_str()), Some("2.0"));
            assert_eq!(m.get("id").and_then(|v| v.as_i64()), Some(1));
        } else {
            panic!();
        }
    }

    #[test]
    fn serialize_roundtrip() {
        let original = Value::Object(
            [
                ("a".to_string(), Value::Number(1.0)),
                ("b".to_string(), Value::String_("hi\n".to_string())),
                (
                    "c".to_string(),
                    Value::Array(vec![Value::Bool(true), Value::Null]),
                ),
            ]
            .iter()
            .cloned()
            .collect(),
        );
        let s = original.to_string();
        let parsed = Parser::new(&s).parse_value().unwrap();
        assert_eq!(original, parsed);
    }
}
