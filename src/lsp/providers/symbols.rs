use super::helpers::*;
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;
use std::collections::{BTreeMap, HashMap};

/// v0.51: AST span 是 1-based，LSP Position 是 0-based，统一减 1.
fn lsp_position(line: usize, column: usize) -> Value {
    Value::Object({
        let mut p = BTreeMap::new();
        p.insert(
            "line".to_string(),
            Value::Number(line.saturating_sub(1) as f64),
        );
        p.insert(
            "character".to_string(),
            Value::Number(column.saturating_sub(1) as f64),
        );
        p
    })
}

#[allow(dead_code)]
pub fn document_symbol_v2(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
    let uri = match params
        .get("textDocument")
        .and_then(|t| t.get("uri"))
        .and_then(|u| u.as_str())
    {
        Some(s) => s,
        None => return Value::Array(vec![]),
    };
    let (_text, stmt_ids, arena) = match parsed_doc_v2(docs, uri) {
        Some(t) => t,
        None => return Value::Array(vec![]),
    };
    let mut out = Vec::new();
    for stmt_id in &stmt_ids {
        if let Some(stmt) = arena.get_stmt(*stmt_id) {
            match &stmt.kind {
                crate::ast_v2::StmtKind::Let { name, .. } => {
                    let mut m = BTreeMap::new();
                    m.insert("name".to_string(), Value::String_(name.clone()));
                    m.insert("kind".to_string(), Value::Number(13.0)); // Variable
                    m.insert(
                        "range".to_string(),
                        Value::Object({
                            let mut r = BTreeMap::new();
                            r.insert(
                                "start".to_string(),
                                lsp_position(stmt.span.line, stmt.span.column),
                            );
                            r.insert(
                                "end".to_string(),
                                lsp_position(stmt.span.line, stmt.span.column + name.len()),
                            );
                            r
                        }),
                    );
                    out.push(Value::Object(m));
                }
                crate::ast_v2::StmtKind::TaskDef { name, .. } => {
                    let mut m = BTreeMap::new();
                    m.insert("name".to_string(), Value::String_(name.clone()));
                    m.insert("kind".to_string(), Value::Number(12.0)); // Function
                    m.insert(
                        "range".to_string(),
                        Value::Object({
                            let mut r = BTreeMap::new();
                            r.insert(
                                "start".to_string(),
                                lsp_position(stmt.span.line, stmt.span.column),
                            );
                            r.insert(
                                "end".to_string(),
                                lsp_position(stmt.span.line, stmt.span.column + name.len()),
                            );
                            r
                        }),
                    );
                    out.push(Value::Object(m));
                }
                _ => {}
            }
        }
    }
    Value::Array(out)
}

// ===================================================================
// Formatting
// ===================================================================
