use super::helpers::*;
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;
use std::collections::{BTreeSet, HashMap};

#[allow(dead_code)]
pub fn completion_v2(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
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
    let line_num = pos.get("line").and_then(|n| n.as_i64()).unwrap_or(0) as usize;
    let col = pos.get("character").and_then(|n| n.as_i64()).unwrap_or(0) as usize;

    let (text, stmt_ids, arena) = match parsed_doc_v2(docs, uri) {
        Some(t) => t,
        None => return Value::Array(vec![]),
    };

    // 取当前行 cursor 之前的文本
    let _prefix = get_line_prefix(&text, line_num, col);

    let mut items: Vec<Value> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    // 收集已定义的变量和函数
    let defs = collect_definitions_v2(&stmt_ids, &arena);
    for name in defs.keys() {
        if seen.insert(name.clone()) {
            items.push(make_completion(name, 3.0, Some("variable")));
        }
    }

    // 关键字补全
    for kw in [
        "let", "task", "if", "then", "end", "for", "in", "return", "fn", "true", "false", "nil",
        "match", "with", "import", "export", "parallel", "break", "continue", "route", "observe",
        "stream", "tool",
    ] {
        if seen.insert(kw.to_string()) {
            items.push(make_completion(kw, 14.0, Some("keyword")));
        }
    }

    // 内置函数补全
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

// ===================================================================
// Go-to-definition
// ===================================================================
