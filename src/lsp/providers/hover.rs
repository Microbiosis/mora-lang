use super::helpers::*;
use crate::lsp::json;
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;
use std::collections::HashMap;

// ===================================================================

/// v0.24: hover 使用 ast_v2
#[allow(dead_code)]
pub fn hover_v2(docs: &HashMap<String, DocumentState>, params: &Value) -> Result<Value, String> {
    let uri = params
        .get("textDocument")
        .and_then(|t| t.get("uri"))
        .and_then(|u| u.as_str())
        .ok_or("missing textDocument.uri")?;
    let pos = params.get("position").ok_or("missing position")?;
    let line = pos.get("line").and_then(|n| n.as_i64()).unwrap_or(0) as usize;
    let col = pos.get("character").and_then(|n| n.as_i64()).unwrap_or(0) as usize;

    let (text, stmt_ids, arena) = parsed_doc_v2(docs, uri).ok_or("document not found")?;
    let offset = position_to_offset(&text, line, col);
    let ident = match ident_at_offset(&text, offset) {
        Some(s) => s,
        None => return Ok(Value::Null),
    };

    // 查找 let 的 type_hint
    let mut contents = format!("```mora\nlet {}: <inferred>\n```", ident);
    for stmt_id in &stmt_ids {
        if let Some(stmt) = arena.get_stmt(*stmt_id) {
            match &stmt.kind {
                crate::ast_v2::StmtKind::Let {
                    name,
                    type_hint: Some(h),
                    ..
                } if name == &ident => {
                    contents = format!("```mora\nlet {}: {} = ...\n```", ident, h);
                }
                crate::ast_v2::StmtKind::Let {
                    name,
                    type_hint: None,
                    ..
                } if name == &ident => {}
                crate::ast_v2::StmtKind::TaskDef {
                    name,
                    params,
                    return_type,
                    ..
                } if name == &ident => {
                    let param_strs: Vec<String> = params
                        .iter()
                        .map(|(n, h)| match h {
                            Some(t) => format!("{}: {}", n, t),
                            None => n.clone(),
                        })
                        .collect();
                    let ret = return_type.as_deref().unwrap_or("any");
                    contents = format!(
                        "```mora\ntask {}({}): {}\n```",
                        ident,
                        param_strs.join(", "),
                        ret
                    );
                }
                _ => {}
            }
        }
    }

    // 构建返回值
    let result = json::parse(&format!(
        r#"{{"contents":{{"kind":"markdown","value":"{}"}},"range":{{"start":{{"line":{},"character":{}}},"end":{{"line":{},"character":{}}}}}}}"#,
        contents.replace('\\', "\\\\").replace('"', "\\\""),
        line, col.saturating_sub(ident.len()),
        line, col
    )).unwrap_or(Value::Null);
    Ok(result)
}

// ===================================================================
// Completion
// ===================================================================
