//! v0.25: LSP definition provider（跳转定义）。

use std::collections::{BTreeMap, HashMap};

use super::parsed_doc_v3;
use super::parsed_doc_v3::{ident_at_offset, position_to_offset};
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;

pub fn definition_v3(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
    let uri = match params
        .get("textDocument")
        .and_then(|t| t.get("uri"))
        .and_then(|u| u.as_str())
    {
        Some(s) => s,
        None => return Value::Array(vec![]),
    };
    let pos = match params.get("position") {
        Some(p) => p,
        None => return Value::Array(vec![]),
    };
    let line = pos.get("line").and_then(|n| n.as_i64()).unwrap_or(0) as usize;
    let col = pos.get("character").and_then(|n| n.as_i64()).unwrap_or(0) as usize;

    let (text, exprs) = match parsed_doc_v3::parsed_doc_v3(docs, uri) {
        Some(pair) => pair,
        None => return Value::Array(vec![]),
    };
    let offset = position_to_offset(&text, line, col);
    let ident = match ident_at_offset(&text, offset) {
        Some(s) => s,
        None => return Value::Array(vec![]),
    };

    let defs = parsed_doc_v3::collect_definitions_v3(&exprs);
    let mut locations: Vec<Value> = Vec::new();
    for (name, span) in defs {
        if name == ident {
            let mut m = BTreeMap::new();
            m.insert("uri".to_string(), Value::String_(uri.to_string()));
            m.insert(
                "range".to_string(),
                Value::Object({
                    let mut r = BTreeMap::new();
                    r.insert(
                        "start".to_string(),
                        Value::Object({
                            let mut s = BTreeMap::new();
                            s.insert("line".to_string(), Value::Number(span.line as f64));
                            s.insert("character".to_string(), Value::Number(span.column as f64));
                            s
                        }),
                    );
                    r.insert(
                        "end".to_string(),
                        Value::Object({
                            let mut s = BTreeMap::new();
                            s.insert("line".to_string(), Value::Number(span.line as f64));
                            s.insert(
                                "character".to_string(),
                                Value::Number((span.column + ident.len()) as f64),
                            );
                            s
                        }),
                    );
                    r
                }),
            );
            locations.push(Value::Object(m));
        }
    }
    Value::Array(locations)
}
