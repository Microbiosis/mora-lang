use std::collections::{BTreeMap, HashMap};

use super::parsed_doc_v3;
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;

pub fn document_symbol_v3(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
    let uri = match params
        .get("textDocument")
        .and_then(|t| t.get("uri"))
        .and_then(|u| u.as_str())
    {
        Some(s) => s,
        None => return Value::Array(vec![]),
    };
    let (_text, exprs) = match parsed_doc_v3::parsed_doc_v3(docs, uri) {
        Some(pair) => pair,
        None => return Value::Array(vec![]),
    };

    let defs = parsed_doc_v3::collect_definitions_v3(exprs);
    let mut symbols: Vec<Value> = Vec::new();
    for (name, span) in defs {
        let name_str = name.clone();
        let mut m = BTreeMap::new();
        m.insert("name".to_string(), Value::String_(name_str.clone()));
        m.insert("kind".to_string(), Value::Number(13.0));
        m.insert(
            "location".to_string(),
            Value::Object({
                let mut loc = BTreeMap::new();
                loc.insert("uri".to_string(), Value::String_(uri.to_string()));
                loc.insert(
                    "range".to_string(),
                    Value::Object({
                        let mut r = BTreeMap::new();
                        r.insert(
                            "start".to_string(),
                            Value::Object({
                                let mut s = BTreeMap::new();
                                s.insert("line".to_string(), Value::Number(span.line as f64));
                                s.insert(
                                    "character".to_string(),
                                    Value::Number(span.column as f64),
                                );
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
                                    Value::Number(span.column as f64 + name_str.len() as f64),
                                );
                                s
                            }),
                        );
                        r
                    }),
                );
                loc
            }),
        );
        symbols.push(Value::Object(m));
    }
    Value::Array(symbols)
}
