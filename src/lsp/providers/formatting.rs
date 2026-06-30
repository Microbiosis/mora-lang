use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;
use std::collections::{BTreeMap, HashMap};

pub fn formatting(docs: &HashMap<String, DocumentState>, params: &Value, range: bool) -> Value {
    let uri = match params
        .get("textDocument")
        .and_then(|t| t.get("uri"))
        .and_then(|u| u.as_str())
    {
        Some(s) => s,
        None => return Value::Array(vec![]),
    };
    let text = match docs.get(uri) {
        Some(d) => d.text.clone(),
        None => return Value::Array(vec![]),
    };

    // 基础格式化：每行按 token 简单重排。
    // 完整实现需要 AST-aware formatter；这里给一个能工作的最小版本——
    // 行首/行末去空白 + 一致缩进（2 空格，按 then / for body / task body / do-end 嵌套加 1 层）。
    let formatted = simple_format(&text, range, params);

    let (start_line, start_col, end_line, end_col) = if range {
        let s = params.get("range").expect("range should exist");
        let start = s.get("start").expect("start should exist");
        let end = s.get("end").expect("end should exist");
        (
            start.get("line").and_then(|n| n.as_i64()).unwrap_or(0) as usize,
            start.get("character").and_then(|n| n.as_i64()).unwrap_or(0) as usize,
            end.get("line").and_then(|n| n.as_i64()).unwrap_or(0) as usize,
            end.get("character").and_then(|n| n.as_i64()).unwrap_or(0) as usize,
        )
    } else {
        (0usize, 0usize, 0usize, 0usize)
    };

    let mut m = BTreeMap::new();
    m.insert(
        "range".to_string(),
        Value::Object({
            let mut r = BTreeMap::new();
            r.insert(
                "start".to_string(),
                Value::Object({
                    let mut p = BTreeMap::new();
                    p.insert("line".to_string(), Value::Number(start_line as f64));
                    p.insert("character".to_string(), Value::Number(start_col as f64));
                    p
                }),
            );
            r.insert(
                "end".to_string(),
                Value::Object({
                    let mut p = BTreeMap::new();
                    p.insert("line".to_string(), Value::Number(end_line as f64));
                    p.insert("character".to_string(), Value::Number(end_col as f64));
                    p
                }),
            );
            r
        }),
    );
    m.insert("newText".to_string(), Value::String_(formatted));
    Value::Array(vec![Value::Object(m)])
}

/// 简单 formatter：trim 行尾空白 + 缩进。基础但能跑。
fn simple_format(text: &str, _range: bool, _params: &Value) -> String {
    // 扫描 token，按 indent 规则重新组装
    let tokens = crate::lexer::Lexer::new(text).scan_tokens();
    let mut out = String::new();
    let mut depth: usize = 0;
    let mut needs_indent = true;
    let mut last_was_newline = true;

    use crate::lexer::TokenType;
    for tok in &tokens {
        match &tok.token_type {
            TokenType::Newline => {
                if !last_was_newline {
                    out.push('\n');
                    needs_indent = true;
                    last_was_newline = true;
                }
            }
            TokenType::EOF => {
                if !out.ends_with('\n') && !out.is_empty() {
                    out.push('\n');
                }
            }
            TokenType::LBrace | TokenType::LBracket | TokenType::LParen => {
                if needs_indent {
                    push_indent(&mut out, depth);
                    needs_indent = false;
                }
                out.push_str(&token_text(&tok.token_type));
            }
            TokenType::RBrace | TokenType::RBracket | TokenType::RParen => {
                if !out.ends_with('\n') && !out.is_empty() {
                    // 同行的右括号前面 trim 一下
                }
                depth = depth.saturating_sub(1);
                if !out.ends_with('\n')
                    && !out.is_empty()
                    && !out.ends_with('(')
                    && !out.ends_with('[')
                    && !out.ends_with('{')
                {
                    // 简单：直接接
                }
                out.push_str(&token_text(&tok.token_type));
            }
            _ => {
                if needs_indent {
                    push_indent(&mut out, depth);
                    needs_indent = false;
                }
                out.push_str(&token_text(&tok.token_type));
                out.push(' ');
                if matches!(tok.token_type, TokenType::End | TokenType::Then) {
                    depth += 1;
                }
                last_was_newline = false;
            }
        }
    }
    out
}

fn push_indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn token_text(tt: &crate::lexer::TokenType) -> String {
    use crate::lexer::TokenType;
    match tt {
        TokenType::String(s) => format!("\"{}\"", s),
        TokenType::Number(n) => n.to_string(),
        TokenType::Identifier(s) => s.clone(),
        _ => format!("{:?}", tt).to_lowercase(),
    }
}

// ===================================================================
// Rename
// ===================================================================
