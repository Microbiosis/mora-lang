//! v0.25: LSP completion provider（补全建议）。

use std::collections::{BTreeSet, HashMap};

use super::parsed_doc_v3;
use super::parsed_doc_v3::make_completion;
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;

pub fn completion_v3(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
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

    let mut items: Vec<Value> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for (name, _) in parsed_doc_v3::collect_definitions_v3(&exprs) {
        if seen.insert(name.clone()) {
            items.push(make_completion(&name, 6.0, Some("variable")));
        }
    }

    for kw in [
        "let", "task", "if", "then", "else", "end", "for", "in", "return", "fn", "true", "false",
        "nil", "match", "with", "import", "export", "parallel", "break", "continue", "route",
        "observe", "stream", "tool",
    ] {
        if seen.insert(kw.to_string()) {
            items.push(make_completion(kw, 14.0, Some("keyword")));
        }
    }

    for builtin in [
        "print",
        "range",
        "len",
        "type_of",
        "is_instance",
        "methods_of",
        "atom",
        "swap",
        "deref",
        "compose",
        "partial",
        "batch_chat",
    ] {
        if seen.insert(builtin.to_string()) {
            items.push(make_completion(builtin, 10.0, Some("builtin")));
        }
    }

    Value::Array(items)
}
