use std::collections::HashMap;

use super::parsed_doc_v3;
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;

pub fn folding_range_v3(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
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

    let mut ranges: Vec<Value> = Vec::new();
    for expr in exprs {
        collect_folds(expr, &mut ranges);
    }
    Value::Array(ranges)
}

fn collect_folds(expr: &crate::mir::MirExpr, out: &mut Vec<Value>) {
    use crate::mir::expr::MirExprKind;
    match &expr.kind {
        MirExprKind::If { then, r#else, .. } => {
            let end_line = r#else
                .as_ref()
                .map(|e| e.span.line)
                .unwrap_or(then.span.line);
            if end_line > expr.span.line {
                out.push(make_range(expr.span.line, end_line));
            }
            collect_folds(then, out);
            if let Some(e) = r#else {
                collect_folds(e, out);
            }
        }
        MirExprKind::Match { arms, .. } => {
            let end_line = arms
                .iter()
                .map(|a| a.body.span.line)
                .max()
                .unwrap_or(expr.span.line);
            if end_line > expr.span.line {
                out.push(make_range(expr.span.line, end_line));
            }
            for arm in arms {
                collect_folds(&arm.body, out);
            }
        }
        MirExprKind::FnDef { body, .. } | MirExprKind::Closure { body, .. } => {
            if body.span.line > expr.span.line {
                out.push(make_range(expr.span.line, body.span.line));
            }
            collect_folds(body, out);
        }
        _ => {}
    }
}

fn make_range(start_line: usize, end_line: usize) -> Value {
    let mut m = std::collections::BTreeMap::new();
    m.insert("startLine".to_string(), Value::Number(start_line as f64));
    m.insert("endLine".to_string(), Value::Number(end_line as f64));
    Value::Object(m)
}
