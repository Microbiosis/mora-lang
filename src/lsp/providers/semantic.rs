use super::helpers::*;
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;
use std::collections::{BTreeMap, HashMap};

pub fn semantic_tokens(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
    let uri = match params
        .get("textDocument")
        .and_then(|t| t.get("uri"))
        .and_then(|u| u.as_str())
    {
        Some(s) => s,
        None => {
            return Value::Object({
                let mut m = BTreeMap::new();
                m.insert("data".to_string(), Value::Array(vec![]));
                m
            });
        }
    };
    let text = match docs.get(uri) {
        Some(d) => d.text.clone(),
        None => {
            return Value::Object({
                let mut m = BTreeMap::new();
                m.insert("data".to_string(), Value::Array(vec![]));
                m
            });
        }
    };

    let mut data: Vec<u32> = Vec::new();
    let mut last_line = 0u32;
    let mut last_col = 0u32;

    // Token 分类：keyword / type / function / variable / string / number / comment
    let tokens = crate::lexer::Lexer::new(&text).scan_tokens();
    use crate::lexer::TokenType;
    for tok in &tokens {
        let (line, col) = (tok.line, 0u32);
        let token_type = match &tok.token_type {
            TokenType::Let | TokenType::Task | TokenType::If | TokenType::Then | TokenType::End |
            TokenType::For | TokenType::In | TokenType::Return |
            TokenType::Fn | TokenType::True | TokenType::False | TokenType::Nil | TokenType::Match |
            TokenType::WithKeyword | TokenType::Save | TokenType::Load | TokenType::Import |
            TokenType::Parallel | TokenType::Read | TokenType::Write | TokenType::Append |
            TokenType::ReadBytes | TokenType::WriteBytes | TokenType::Into | TokenType::Export |
            // v0.04.0: AI 原语关键字
            TokenType::Stream | TokenType::Tool | TokenType::Break | TokenType::Continue |
            // v0.08: trait 系统关键字
            TokenType::Trait | TokenType::Impl | TokenType::Dyn | TokenType::Self_ => 0,  // keyword
            TokenType::String(_) => 3,  // string
            TokenType::Number(_) => 4,  // number
            _ => continue,
        };
        let len = token_len(&tok.token_type) as u32;
        let dl = (line as u32).saturating_sub(last_line);
        let dc = if dl == 0 {
            (col).saturating_sub(last_col)
        } else {
            col
        };
        data.push(dl);
        data.push(dc);
        data.push(token_type);
        data.push(0); // modifiers
        data.push(len);
        last_line = line as u32;
        last_col = col;
    }

    // 给已知任务名加 function token (使用 v2)
    let (_text2, stmt_ids, arena) = parsed_doc_v2(docs, uri).unwrap_or_default();
    for stmt_id in &stmt_ids {
        if let Some(stmt) = arena.get_stmt(*stmt_id) {
            if let crate::ast_v2::StmtKind::TaskDef { name, .. } = &stmt.kind {
                let span = &stmt.span;
                let dl = (span.line as u32).saturating_sub(last_line);
                let dc = if dl == 0 {
                    (span.column as u32).saturating_sub(last_col)
                } else {
                    span.column as u32
                };
                data.push(dl);
                data.push(dc);
                data.push(1); // function
                data.push(0);
                data.push(name.len() as u32);
                last_line = span.line as u32;
                last_col = span.column as u32;
            }
            if let crate::ast_v2::StmtKind::Let { name, .. } = &stmt.kind {
                let span = &stmt.span;
                let dl = (span.line as u32).saturating_sub(last_line);
                let dc = if dl == 0 {
                    (span.column as u32).saturating_sub(last_col)
                } else {
                    span.column as u32
                };
                data.push(dl);
                data.push(dc);
                data.push(2); // variable
                data.push(0);
                data.push(name.len() as u32);
                last_line = span.line as u32;
                last_col = span.column as u32;
            }
        }
    }

    let mut m = BTreeMap::new();
    m.insert(
        "data".to_string(),
        Value::Array(data.into_iter().map(|n| Value::Number(n as f64)).collect()),
    );
    Value::Object(m)
}

fn token_len(tt: &crate::lexer::TokenType) -> usize {
    use crate::lexer::TokenType;
    match tt {
        TokenType::String(s) => s.len() + 2,
        TokenType::Number(n) => n.to_string().len(),
        TokenType::Identifier(s) => s.len(),
        _ => format!("{:?}", tt).len(),
    }
}

// ===================================================================
// Folding range
// ===================================================================
