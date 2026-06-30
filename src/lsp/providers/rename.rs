use super::helpers::*;
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// v0.24: rename 使用 ast_v2
#[allow(dead_code)]
pub fn rename_v2(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
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
    let (text, stmt_ids, arena) = match parsed_doc_v2(docs, uri) {
        Some(t) => t,
        None => return Value::Null,
    };
    let offset = position_to_offset(&text, line, col);
    let old_name = match ident_at_offset(&text, offset) {
        Some(s) => s,
        None => return Value::Null,
    };

    let defs = collect_definitions_v2(&stmt_ids, &arena);
    let mut refs = Vec::new();
    collect_references_v2(&stmt_ids, &arena, &old_name, &mut refs);

    // 编辑列表：定义 + 引用（合并，按 offset 排序去重）
    let mut edits: BTreeSet<(usize, usize)> = BTreeSet::new();
    if defs.contains_key(&old_name) {
        for (l, c) in &defs[&old_name] {
            edits.insert((*l, *c));
        }
    }
    for (l, c) in &refs {
        edits.insert((*l, *c));
    }

    let mut changes = BTreeMap::new();
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
                        p.insert("line".to_string(), Value::Number(*l as f64 - 1.0));
                        p.insert("character".to_string(), Value::Number(*c as f64 - 1.0));
                        p
                    }),
                );
                r.insert(
                    "end".to_string(),
                    Value::Object({
                        let mut p = BTreeMap::new();
                        p.insert("line".to_string(), Value::Number(*l as f64 - 1.0));
                        p.insert(
                            "character".to_string(),
                            Value::Number(*c as f64 - 1.0 + old_name.len() as f64),
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
    changes.insert(uri.to_string(), Value::Array(edit_list));

    let mut result = BTreeMap::new();
    result.insert("changes".to_string(), Value::Object(changes));
    Value::Object(result)
}

// ===================================================================
// Semantic tokens
// ===================================================================
