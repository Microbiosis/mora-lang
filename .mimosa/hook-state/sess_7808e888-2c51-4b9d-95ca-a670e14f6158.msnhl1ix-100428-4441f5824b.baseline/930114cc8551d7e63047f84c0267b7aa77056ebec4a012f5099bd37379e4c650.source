//! v0.25: LSP hover provider（悬停信息）。

use std::collections::{BTreeMap, HashMap};

use super::parsed_doc_v3;
use super::parsed_doc_v3::{ident_at_offset, position_to_offset};
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;

pub fn hover_v3(docs: &HashMap<String, DocumentState>, params: &Value) -> Result<Value, String> {
    let uri = params
        .get("textDocument")
        .and_then(|t| t.get("uri"))
        .and_then(|u| u.as_str())
        .ok_or("missing textDocument.uri")?;
    let pos = params.get("position").ok_or("missing position")?;
    let line = pos.get("line").and_then(|n| n.as_i64()).unwrap_or(0) as usize;
    let col = pos.get("character").and_then(|n| n.as_i64()).unwrap_or(0) as usize;

    let (text, exprs) = parsed_doc_v3::parsed_doc_v3(docs, uri).ok_or("document not found")?;
    let offset = position_to_offset(&text, line, col);
    let ident = match ident_at_offset(&text, offset) {
        Some(s) => s,
        None => return Ok(Value::Null),
    };

    let defs = parsed_doc_v3::collect_definitions_v3(&exprs);
    let kind = if defs.iter().any(|(n, _)| n == &ident) {
        "let"
    } else if ["print", "len", "range"].contains(&ident.as_str()) {
        "builtin"
    } else {
        "variable"
    };
    let contents = format!("```mora\n{} {}: <inferred>\n```", kind, ident);

    let mut m = BTreeMap::new();
    m.insert(
        "contents".to_string(),
        Value::Object({
            let mut inner = BTreeMap::new();
            inner.insert("kind".to_string(), Value::String_("markdown".to_string()));
            inner.insert("value".to_string(), Value::String_(contents));
            inner
        }),
    );
    m.insert(
        "range".to_string(),
        Value::Object({
            let mut r = BTreeMap::new();
            r.insert(
                "start".to_string(),
                Value::Object({
                    let mut s = BTreeMap::new();
                    s.insert("line".to_string(), Value::Number(line as f64));
                    s.insert(
                        "character".to_string(),
                        Value::Number(col.saturating_sub(ident.len()) as f64),
                    );
                    s
                }),
            );
            r.insert(
                "end".to_string(),
                Value::Object({
                    let mut s = BTreeMap::new();
                    s.insert("line".to_string(), Value::Number(line as f64));
                    s.insert("character".to_string(), Value::Number(col as f64));
                    s
                }),
            );
            r
        }),
    );
    Ok(Value::Object(m))
}
