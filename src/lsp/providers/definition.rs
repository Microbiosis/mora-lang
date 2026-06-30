use super::helpers::*;
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;
use std::collections::{BTreeMap, HashMap};

#[allow(dead_code)]
pub fn definition_v2(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
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

    let (text, stmt_ids, arena) = match parsed_doc_v2(docs, uri) {
        Some(t) => t,
        None => return Value::Array(vec![]),
    };
    let offset = position_to_offset(&text, line, col);
    let ident = match ident_at_offset(&text, offset) {
        Some(s) => s,
        None => return Value::Array(vec![]),
    };

    let defs = collect_definitions_v2(&stmt_ids, &arena);
    let mut locations: Vec<Value> = Vec::new();
    if let Some(positions) = defs.get(&ident) {
        for (l, c) in positions {
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
                            s.insert("line".to_string(), Value::Number(*l as f64 - 1.0));
                            s.insert("character".to_string(), Value::Number(*c as f64 - 1.0));
                            s
                        }),
                    );
                    r.insert(
                        "end".to_string(),
                        Value::Object({
                            let mut s = BTreeMap::new();
                            s.insert("line".to_string(), Value::Number(*l as f64 - 1.0));
                            s.insert(
                                "character".to_string(),
                                Value::Number(*c as f64 - 1.0 + ident.len() as f64),
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

// ===================================================================
// References
// ===================================================================
