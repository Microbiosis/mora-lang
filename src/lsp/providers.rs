//! LSP 各 method 的实现
//!
//! 全部基于 Mora AST + 文本。Hover / Completion / Goto-Def / References
//! 都在一个公共骨架上：
//!   1. 取当前文档文本
//!   2. lexer + parser + typeck 拿到带 Span 的 AST
//!   3. 把 LSP position (line, col, 0-based) 映射到 AST 节点
//!   4. 按 provider 语义返回 LSP response

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::json::Value;
use super::server::DocumentState;

// ===================================================================
// Position utility
// ===================================================================

/// 把 LSP position (0-based line, col) 转成字符 offset
fn position_to_offset(text: &str, line: usize, col: usize) -> usize {
    let mut current_line = 0;
    let mut current_col = 0;
    for (i, c) in text.char_indices() {
        if current_line == line && current_col == col {
            return i;
        }
        if c == '\n' {
            current_line += 1;
            current_col = 0;
        } else {
            current_col += 1;
        }
    }
    text.len()
}

/// 取 cursor 所在行 cursor 之前的文本（用于上下文感知补全）
fn get_line_prefix(text: &str, line: usize, col: usize) -> String {
    let mut current_line = 0;
    let mut result = String::new();
    for c in text.chars() {
        if current_line == line {
            result.push(c);
            if result.len() >= col {
                break;
            }
        }
        if c == '\n' {
            current_line += 1;
        }
    }
    result
}

/// 创建 LSP completion item
fn make_completion(label: &str, kind: f64, detail: Option<&str>) -> Value {
    let mut m = BTreeMap::new();
    m.insert("label".to_string(), Value::String_(label.to_string()));
    m.insert("kind".to_string(), Value::Number(kind));
    if let Some(d) = detail {
        m.insert("detail".to_string(), Value::String_(d.to_string()));
    }
    Value::Object(m)
}

/// 在某 offset 取一个标识符（变量名）
fn ident_at_offset(text: &str, offset: usize) -> Option<String> {
    let bytes = text.as_bytes();
    if offset > bytes.len() {
        return None;
    }
    // 找边界
    let mut start = offset;
    while start > 0 {
        let prev = bytes[start - 1];
        if prev.is_ascii_alphanumeric() || prev == b'_' {
            start -= 1;
        } else {
            break;
        }
    }
    let mut end = offset;
    while end < bytes.len() {
        let c = bytes[end];
        if c.is_ascii_alphanumeric() || c == b'_' {
            end += 1;
        } else {
            break;
        }
    }
    if start == end {
        return None;
    }
    std::str::from_utf8(&bytes[start..end])
        .ok()
        .map(|s| s.to_string())
}

// ===================================================================
// AST access helpers
// ===================================================================

// v0.24: 未来将迁移为 use crate::ast_v2::*;

/// 拿一个文档的解析结果
/// v0.24: 拿一个文档的 ast_v2 解析结果
#[allow(dead_code)]
fn parsed_doc_v2(docs: &HashMap<String, DocumentState>, uri: &str) -> Option<(String, Vec<crate::ast_v2::NodeId>, crate::ast_v2::AstArena)> {
    let doc = docs.get(uri)?;
    let tokens = crate::lexer::Lexer::new(&doc.text).scan_tokens();
    let mut parser_v2 = crate::parser_v2::ParserV2::new(tokens);
    let v2_stmts = parser_v2.parse();
    let arena = parser_v2.into_arena();

    Some((doc.text.clone(), v2_stmts, arena))
}

/// 收集所有"定义点" (let / task)，按名字索引到 (line, col)
/// v0.24: 收集所有"定义点" (ast_v2 版本)
#[allow(dead_code)]
fn collect_definitions_v2(stmt_ids: &[crate::ast_v2::NodeId], arena: &crate::ast_v2::AstArena) -> HashMap<String, Vec<(usize, usize)>> {
    let mut out: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    for stmt_id in stmt_ids {
        if let Some(stmt) = arena.get_stmt(*stmt_id) {
            match &stmt.kind {
                crate::ast_v2::StmtKind::Let { name, .. } => {
                    out.entry(name.clone())
                        .or_default()
                        .push((stmt.span.line, stmt.span.column));
                }
                crate::ast_v2::StmtKind::TaskDef { name, .. } => {
                    out.entry(name.clone())
                        .or_default()
                        .push((stmt.span.line, stmt.span.column));
                }
                _ => {}
            }
        }
    }
    out
}

/// v0.24: 收集所有"引用点" (ast_v2 版本)
#[allow(dead_code)]
fn collect_references_v2(stmt_ids: &[crate::ast_v2::NodeId], arena: &crate::ast_v2::AstArena, name: &str, refs: &mut Vec<(usize, usize)>) {
    fn walk_expr_v2(expr_id: crate::ast_v2::NodeId, arena: &crate::ast_v2::AstArena, name: &str, refs: &mut Vec<(usize, usize)>) {
        if let Some(expr) = arena.get_expr(expr_id) {
            match &expr.kind {
                crate::ast_v2::ExprKind::Variable(n) if n == name => {
                    refs.push((expr.span.line, expr.span.column));
                }
                crate::ast_v2::ExprKind::Call { callee, args, .. } => {
                    if callee == name {
                        refs.push((expr.span.line, expr.span.column));
                    }
                    for a in args {
                        walk_expr_v2(*a, arena, name, refs);
                    }
                }
                crate::ast_v2::ExprKind::MethodCall { object, args, .. } => {
                    walk_expr_v2(*object, arena, name, refs);
                    for a in args {
                        walk_expr_v2(*a, arena, name, refs);
                    }
                }
                crate::ast_v2::ExprKind::Binary { left, right, .. } => {
                    walk_expr_v2(*left, arena, name, refs);
                    walk_expr_v2(*right, arena, name, refs);
                }
                crate::ast_v2::ExprKind::Pipe { left, right } => {
                    walk_expr_v2(*left, arena, name, refs);
                    walk_expr_v2(*right, arena, name, refs);
                }
                crate::ast_v2::ExprKind::Grouping(inner) => {
                    walk_expr_v2(*inner, arena, name, refs);
                }
                crate::ast_v2::ExprKind::Borrow { expr: inner }
                | crate::ast_v2::ExprKind::BorrowMut { expr: inner } => {
                    walk_expr_v2(*inner, arena, name, refs);
                }
                _ => {}
            }
        }
    }

    for stmt_id in stmt_ids {
        if let Some(stmt) = arena.get_stmt(*stmt_id) {
            match &stmt.kind {
                crate::ast_v2::StmtKind::Let { init, .. }
                | crate::ast_v2::StmtKind::Assign { value: init, .. } => {
                    walk_expr_v2(*init, arena, name, refs);
                }
                crate::ast_v2::StmtKind::Expr(expr_id) => {
                    walk_expr_v2(*expr_id, arena, name, refs);
                }
                crate::ast_v2::StmtKind::Return { value: Some(expr_id) } => {
                    walk_expr_v2(*expr_id, arena, name, refs);
                }
                crate::ast_v2::StmtKind::If { condition, then_branch, else_branch } => {
                    walk_expr_v2(*condition, arena, name, refs);
                    collect_references_v2(then_branch, arena, name, refs);
                    collect_references_v2(else_branch, arena, name, refs);
                }
                crate::ast_v2::StmtKind::For { iterable, body, .. } => {
                    walk_expr_v2(*iterable, arena, name, refs);
                    collect_references_v2(body, arena, name, refs);
                }
                _ => {}
            }
        }
    }
}

// ===================================================================
// Hover
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
                crate::ast_v2::StmtKind::Let { name, type_hint: Some(h), .. } if name == &ident => {
                    contents = format!("```mora\nlet {}: {} = ...\n```", ident, h);
                }
                crate::ast_v2::StmtKind::Let { name, type_hint: None, .. } if name == &ident => {}
                crate::ast_v2::StmtKind::TaskDef { name, params, return_type, .. } if name == &ident => {
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
    let result = super::json::parse(&format!(
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

/// v0.24: completion 使用 ast_v2
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
        "let", "task", "if", "then", "end", "for", "in", "return", "fn",
        "true", "false", "nil", "match", "with", "import", "export",
        "parallel", "break", "continue", "route", "observe",
        "stream", "tool",
    ] {
        if seen.insert(kw.to_string()) {
            items.push(make_completion(kw, 14.0, Some("keyword")));
        }
    }

    // 内置函数补全
    for builtin in ["print", "range", "len", "type_of", "is_instance", "methods_of",
                     "atom", "swap", "deref", "compose", "partial", "batch_chat"] {
        if seen.insert(builtin.to_string()) {
            items.push(make_completion(builtin, 10.0, Some("builtin")));
        }
    }

    Value::Array(items)
}

// ===================================================================
// Go-to-definition
// ===================================================================

/// v0.24: definition 使用 ast_v2
#[allow(dead_code)]
pub fn definition_v2(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
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
    let line = pos.get("line").and_then(|n| n.as_i64()).unwrap_or(0) as usize;
    let col = pos.get("character").and_then(|n| n.as_i64()).unwrap_or(0) as usize;

    let (text, stmt_ids, arena) = match parsed_doc_v2(docs, uri) {
        Some(t) => t,
        None => return Value::Array(vec![]),
    };
    let offset = position_to_offset(&text, line, col);
    let ident = match ident_at_offset(&text, offset) {
        Some(s) => s,
        None => return Value::Array(vec![]),
    };

    let defs = collect_definitions_v2(&stmt_ids, &arena);
    let mut locations: Vec<Value> = Vec::new();
    if let Some(positions) = defs.get(&ident) {
        for (l, c) in positions {
            let mut m = BTreeMap::new();
            m.insert("uri".to_string(), Value::String_(uri.to_string()));
            m.insert("range".to_string(), Value::Object({
                let mut r = BTreeMap::new();
                r.insert("start".to_string(), Value::Object({
                    let mut s = BTreeMap::new();
                    s.insert("line".to_string(), Value::Number(*l as f64 - 1.0));
                    s.insert("character".to_string(), Value::Number(*c as f64 - 1.0));
                    s
                }));
                r.insert("end".to_string(), Value::Object({
                    let mut s = BTreeMap::new();
                    s.insert("line".to_string(), Value::Number(*l as f64 - 1.0));
                    s.insert("character".to_string(), Value::Number(*c as f64 - 1.0 + ident.len() as f64));
                    s
                }));
                r
            }));
            locations.push(Value::Object(m));
        }
    }
    Value::Array(locations)
}

// ===================================================================
// References
// ===================================================================

/// v0.24: references 使用 ast_v2
#[allow(dead_code)]
pub fn references_v2(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
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
    let line = pos.get("line").and_then(|n| n.as_i64()).unwrap_or(0) as usize;
    let col = pos.get("character").and_then(|n| n.as_i64()).unwrap_or(0) as usize;

    let (text, stmt_ids, arena) = match parsed_doc_v2(docs, uri) {
        Some(t) => t,
        None => return Value::Array(vec![]),
    };
    let offset = position_to_offset(&text, line, col);
    let ident = match ident_at_offset(&text, offset) {
        Some(s) => s,
        None => return Value::Array(vec![]),
    };

    let mut refs = Vec::new();
    collect_references_v2(&stmt_ids, &arena, &ident, &mut refs);

    let mut locations: Vec<Value> = Vec::new();
    for (l, c) in &refs {
        let mut m = BTreeMap::new();
        m.insert("uri".to_string(), Value::String_(uri.to_string()));
        m.insert("range".to_string(), Value::Object({
            let mut r = BTreeMap::new();
            r.insert("start".to_string(), Value::Object({
                let mut s = BTreeMap::new();
                s.insert("line".to_string(), Value::Number(*l as f64 - 1.0));
                s.insert("character".to_string(), Value::Number(*c as f64 - 1.0));
                s
            }));
            r.insert("end".to_string(), Value::Object({
                let mut s = BTreeMap::new();
                s.insert("line".to_string(), Value::Number(*l as f64 - 1.0));
                s.insert("character".to_string(), Value::Number(*c as f64 - 1.0 + ident.len() as f64));
                s
            }));
            r
        }));
        locations.push(Value::Object(m));
    }
    Value::Array(locations)
}

// ===================================================================
// Document Symbols
// ===================================================================

/// v0.24: document_symbol 使用 ast_v2
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
                    m.insert("range".to_string(), Value::Object({
                        let mut r = BTreeMap::new();
                        r.insert("start".to_string(), Value::Object({
                            let mut p = BTreeMap::new();
                            p.insert("line".to_string(), Value::Number(stmt.span.line as f64));
                            p.insert("character".to_string(), Value::Number(stmt.span.column as f64));
                            p
                        }));
                        r.insert("end".to_string(), Value::Object({
                            let mut p = BTreeMap::new();
                            p.insert("line".to_string(), Value::Number(stmt.span.line as f64));
                            p.insert("character".to_string(), Value::Number((stmt.span.column + name.len()) as f64));
                            p
                        }));
                        r
                    }));
                    out.push(Value::Object(m));
                }
                crate::ast_v2::StmtKind::TaskDef { name, .. } => {
                    let mut m = BTreeMap::new();
                    m.insert("name".to_string(), Value::String_(name.clone()));
                    m.insert("kind".to_string(), Value::Number(12.0)); // Function
                    m.insert("range".to_string(), Value::Object({
                        let mut r = BTreeMap::new();
                        r.insert("start".to_string(), Value::Object({
                            let mut p = BTreeMap::new();
                            p.insert("line".to_string(), Value::Number(stmt.span.line as f64));
                            p.insert("character".to_string(), Value::Number(stmt.span.column as f64));
                            p
                        }));
                        r.insert("end".to_string(), Value::Object({
                            let mut p = BTreeMap::new();
                            p.insert("line".to_string(), Value::Number(stmt.span.line as f64));
                            p.insert("character".to_string(), Value::Number((stmt.span.column + name.len()) as f64));
                            p
                        }));
                        r
                    }));
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
        m.insert("range".to_string(), Value::Object({
            let mut r = BTreeMap::new();
            r.insert("start".to_string(), Value::Object({
                let mut p = BTreeMap::new();
                p.insert("line".to_string(), Value::Number(*l as f64 - 1.0));
                p.insert("character".to_string(), Value::Number(*c as f64 - 1.0));
                p
            }));
            r.insert("end".to_string(), Value::Object({
                let mut p = BTreeMap::new();
                p.insert("line".to_string(), Value::Number(*l as f64 - 1.0));
                p.insert("character".to_string(), Value::Number(*c as f64 - 1.0 + old_name.len() as f64));
                p
            }));
            r
        }));
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
                        && let Some(last_stmt) = arena.get_stmt(*last_id) {
                            let mut m = BTreeMap::new();
                            m.insert("startLine".to_string(), Value::Number(stmt.span.line as f64 - 1.0));
                            m.insert("endLine".to_string(), Value::Number(last_stmt.span.line as f64 - 1.0));
                            m.insert("kind".to_string(), Value::String_("region".to_string()));
                            out.push(Value::Object(m));
                        }
                }
                crate::ast_v2::StmtKind::For { body, .. } => {
                    if let Some(last_id) = body.last()
                        && let Some(last_stmt) = arena.get_stmt(*last_id) {
                            let mut m = BTreeMap::new();
                            m.insert("startLine".to_string(), Value::Number(stmt.span.line as f64 - 1.0));
                            m.insert("endLine".to_string(), Value::Number(last_stmt.span.line as f64 - 1.0));
                            m.insert("kind".to_string(), Value::String_("region".to_string()));
                            out.push(Value::Object(m));
                        }
                }
                crate::ast_v2::StmtKind::TaskDef { body, .. } => {
                    if let Some(last_id) = body.last()
                        && let Some(last_stmt) = arena.get_stmt(*last_id) {
                            let mut m = BTreeMap::new();
                            m.insert("startLine".to_string(), Value::Number(stmt.span.line as f64 - 1.0));
                            m.insert("endLine".to_string(), Value::Number(last_stmt.span.line as f64 - 1.0));
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

