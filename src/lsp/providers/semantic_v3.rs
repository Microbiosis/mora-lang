use std::collections::HashMap;

use super::parsed_doc_v3;
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;

const TOKEN_KIND_VARIABLE: f64 = 13.0;
const TOKEN_KIND_STRING: f64 = 12.0;
const TOKEN_KIND_NUMBER: f64 = 10.0;
const TOKEN_KIND_FUNCTION: f64 = 9.0;

pub fn semantic_tokens_v3(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
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

    let mut data: Vec<f64> = Vec::new();
    let mut last_line = 0usize;
    let mut last_col = 0usize;
    for expr in exprs {
        emit_tokens(expr, &mut data, &mut last_line, &mut last_col);
    }
    let mut m = std::collections::BTreeMap::new();
    m.insert(
        "data".to_string(),
        Value::Array(data.into_iter().map(Value::Number).collect()),
    );
    let _ = uri;
    Value::Object(m)
}

fn emit_tokens(
    expr: &crate::mir::MirExpr,
    out: &mut Vec<f64>,
    last_line: &mut usize,
    last_col: &mut usize,
) {
    use crate::mir::expr::MirExprKind;
    let line = expr.span.line.saturating_sub(1);
    let col = expr.span.column.saturating_sub(1);
    let kind = match &expr.kind {
        MirExprKind::Variable(_) => TOKEN_KIND_VARIABLE,
        MirExprKind::Literal(crate::common::Literal::String(_, _)) => TOKEN_KIND_STRING,
        MirExprKind::Literal(crate::common::Literal::Int(_, _))
        | MirExprKind::Literal(crate::common::Literal::Float(_, _)) => TOKEN_KIND_NUMBER,
        MirExprKind::Call { .. } => TOKEN_KIND_FUNCTION,
        _ => 0.0,
    };
    if kind > 0.0 {
        push_token(out, last_line, last_col, line, col, kind);
    }
    parsed_doc_v3::walk_mir_expr(expr, &mut |e| {
        let l = e.span.line.saturating_sub(1);
        let c = e.span.column.saturating_sub(1);
        let k = match &e.kind {
            MirExprKind::Variable(_) => TOKEN_KIND_VARIABLE,
            MirExprKind::Literal(crate::common::Literal::String(_, _)) => TOKEN_KIND_STRING,
            MirExprKind::Literal(crate::common::Literal::Int(_, _))
            | MirExprKind::Literal(crate::common::Literal::Float(_, _)) => TOKEN_KIND_NUMBER,
            MirExprKind::Call { .. } => TOKEN_KIND_FUNCTION,
            _ => 0.0,
        };
        if k > 0.0 {
            push_token(out, last_line, last_col, l, c, k);
        }
    });
}

fn push_token(
    out: &mut Vec<f64>,
    last_line: &mut usize,
    last_col: &mut usize,
    line: usize,
    col: usize,
    kind: f64,
) {
    let delta_line = line as isize - *last_line as isize;
    let delta_col = if delta_line == 0 {
        col as isize - *last_col as isize
    } else {
        col as isize
    };
    out.push(delta_line as f64);
    out.push(delta_col as f64);
    out.push(1.0); // length
    out.push(kind);
    *last_line = line;
    *last_col = col;
}
