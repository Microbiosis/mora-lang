use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::parsed_doc_v3;
use super::parsed_doc_v3::{position_to_offset, ident_at_offset};
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;

pub fn rename_v3(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
    let uri = match params
        .get("textDocument")
        .and_then(|t| t.get("uri"))
        .and_then(|u| u.as_str())
    {
        Some(s) => s,
        None => return Value::Null,
    };
    let new_name = match params.get("newName").and_then(|n| n.as_str()) {
        Some(s) => s.to_string(),
        None => return Value::Null,
    };
    let pos = match params.get("position") {
        Some(p) => p,
        None => return Value::Null,
    };
    let line = pos.get("line").and_then(|n| n.as_i64()).unwrap_or(0) as usize;
    let col = pos.get("character").and_then(|n| n.as_i64()).unwrap_or(0) as usize;

    let (text, exprs) = match parsed_doc_v3::parsed_doc_v3(docs, uri) {
        Some(pair) => pair,
        None => return Value::Null,
    };
    let offset = position_to_offset(&text, line, col);
    let old_name = match ident_at_offset(&text, offset) {
        Some(s) => s,
        None => return Value::Null,
    };

    let defs = parsed_doc_v3::collect_definitions_v3(&exprs);
    let refs = parsed_doc_v3::collect_references_v3(&exprs, &old_name);

    let mut edits: BTreeSet<(usize, usize)> = BTreeSet::new();
    for (name, span) in &defs {
        if name == &old_name {
            edits.insert((span.line, span.column));
        }
    }
    for span in &refs {
        edits.insert((span.line, span.column));
    }

    let mut edit_list: Vec<Value> = Vec::new();
    for (l, c) in &edits {
        let mut m = BTreeMap::new();
        m.insert(
            "range".to_string(),
            Value::Object({
                let mut r = BTreeMap::new();
                r.insert(
                    "start".to_string(),
                    Value::Object({
                        let mut p = BTreeMap::new();
                        p.insert("line".to_string(), Value::Number(*l as f64));
                        p.insert("character".to_string(), Value::Number(*c as f64));
                        p
                    }),
                );
                r.insert(
                    "end".to_string(),
                    Value::Object({
                        let mut p = BTreeMap::new();
                        p.insert("line".to_string(), Value::Number(*l as f64));
                        p.insert(
                            "character".to_string(),
                            Value::Number((*c + old_name.len()) as f64),
                        );
                        p
                    }),
                );
                r
            }),
        );
        m.insert("newText".to_string(), Value::String_(new_name.clone()));
        edit_list.push(Value::Object(m));
    }

    let mut changes = BTreeMap::new();
    changes.insert(uri.to_string(), Value::Array(edit_list));

    let mut result = BTreeMap::new();
    result.insert("changes".to_string(), Value::Object(changes));
    Value::Object(result)
}
