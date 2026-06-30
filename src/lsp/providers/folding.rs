use super::helpers::*;
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;
use std::collections::{BTreeMap, HashMap};

/// v0.24: folding_range 使用 ast_v2
#[allow(dead_code)]
pub fn folding_range_v2(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
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
                crate::ast_v2::StmtKind::If { then_branch, .. } => {
                    if let Some(last_id) = then_branch.last()
                        && let Some(last_stmt) = arena.get_stmt(*last_id)
                    {
                        let mut m = BTreeMap::new();
                        m.insert(
                            "startLine".to_string(),
                            Value::Number(stmt.span.line as f64 - 1.0),
                        );
                        m.insert(
                            "endLine".to_string(),
                            Value::Number(last_stmt.span.line as f64 - 1.0),
                        );
                        m.insert("kind".to_string(), Value::String_("region".to_string()));
                        out.push(Value::Object(m));
                    }
                }
                crate::ast_v2::StmtKind::For { body, .. } => {
                    if let Some(last_id) = body.last()
                        && let Some(last_stmt) = arena.get_stmt(*last_id)
                    {
                        let mut m = BTreeMap::new();
                        m.insert(
                            "startLine".to_string(),
                            Value::Number(stmt.span.line as f64 - 1.0),
                        );
                        m.insert(
                            "endLine".to_string(),
                            Value::Number(last_stmt.span.line as f64 - 1.0),
                        );
                        m.insert("kind".to_string(), Value::String_("region".to_string()));
                        out.push(Value::Object(m));
                    }
                }
                crate::ast_v2::StmtKind::TaskDef { body, .. } => {
                    if let Some(last_id) = body.last()
                        && let Some(last_stmt) = arena.get_stmt(*last_id)
                    {
                        let mut m = BTreeMap::new();
                        m.insert(
                            "startLine".to_string(),
                            Value::Number(stmt.span.line as f64 - 1.0),
                        );
                        m.insert(
                            "endLine".to_string(),
                            Value::Number(last_stmt.span.line as f64 - 1.0),
                        );
                        m.insert("kind".to_string(), Value::String_("region".to_string()));
                        out.push(Value::Object(m));
                    }
                }
                _ => {}
            }
        }
    }
    Value::Array(out)
}
