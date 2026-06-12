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

/// 在某 offset 取一个标识符（变量名）
fn ident_at_offset(text: &str, offset: usize) -> Option<String> {
    let bytes = text.as_bytes();
    if offset > bytes.len() { return None; }
    // 找边界
    let mut start = offset;
    while start > 0 {
        let prev = bytes[start - 1];
        if prev.is_ascii_alphanumeric() || prev == b'_' {
            start -= 1;
        } else { break; }
    }
    let mut end = offset;
    while end < bytes.len() {
        let c = bytes[end];
        if c.is_ascii_alphanumeric() || c == b'_' {
            end += 1;
        } else { break; }
    }
    if start == end { return None; }
    std::str::from_utf8(&bytes[start..end]).ok().map(|s| s.to_string())
}

// ===================================================================
// AST access helpers
// ===================================================================

use crate::ast::*;

/// 拿一个文档的解析结果
fn parsed_doc<'a>(docs: &'a HashMap<String, DocumentState>, uri: &str)
    -> Option<(String, Vec<Stmt>)>
{
    let doc = docs.get(uri)?;
    let tokens = crate::lexer::Lexer::new(&doc.text).scan_tokens();
    let stmts = crate::parser::Parser::new(tokens).parse();
    Some((doc.text.clone(), stmts))
}

/// 收集所有"定义点" (let / task)，按名字索引到 (line, col)
fn collect_definitions(stmts: &[Stmt]) -> HashMap<String, Vec<(usize, usize)>> {
    let mut out: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    fn walk(stmts: &[Stmt], out: &mut HashMap<String, Vec<(usize, usize)>>) {
        for stmt in stmts {
            match stmt {
                Stmt::Let { name, span, .. } => {
                    out.entry(name.clone()).or_default().push((span.line, span.column));
                }
                Stmt::TaskDef { name, span, .. } => {
                    out.entry(name.clone()).or_default().push((span.line, span.column));
                }
                Stmt::If { then_branch, .. } => walk(then_branch, out),
                Stmt::For { body, .. } => walk(body, out),
                Stmt::Try { try_block, catch_block, .. } => {
                    walk(try_block, out);
                    walk(catch_block, out);
                }
                Stmt::Parallel { stmts, .. } => walk(stmts, out),
                Stmt::Match { arms, .. } => {
                    for (_p, arm_stmts) in arms {
                        walk(arm_stmts, out);
                    }
                }
                _ => {}
            }
        }
    }
    walk(stmts, &mut out);
    out
}

/// 收集所有"引用点"（Expr::Variable / Expr::Call / Expr::MethodCall 出现的位置）
fn collect_references(stmts: &[Stmt], name: &str, refs: &mut Vec<(usize, usize)>) {
    fn walk_expr(expr: &Expr, name: &str, refs: &mut Vec<(usize, usize)>) {
        match expr {
            Expr::Variable(n, span) if n == name => refs.push((span.line, span.column)),
            Expr::Variable(_, _) => {}
            Expr::Call { callee, args, span, .. } => {
                if callee == name {
                    refs.push((span.line, span.column));
                }
                for a in args { walk_expr(a, name, refs); }
            }
            Expr::Binary { left, right, .. } => {
                walk_expr(left, name, refs);
                walk_expr(right, name, refs);
            }
            Expr::Pipe { left, right, .. } => {
                walk_expr(left, name, refs);
                walk_expr(right, name, refs);
            }
            Expr::MethodCall { object, args, .. } => {
                walk_expr(object, name, refs);
                for a in args { walk_expr(a, name, refs); }
            }
            Expr::Index { object, index, .. } => {
                walk_expr(object, name, refs);
                walk_expr(index, name, refs);
            }
            Expr::Grouping(e, _) => walk_expr(e, name, refs),
            Expr::Literal(_) => {}
            Expr::Closure { body, .. } => {
                for s in body { walk_stmt(s, name, refs); }
            }
            Expr::Match { expr, arms, .. } => {
                walk_expr(expr, name, refs);
                for (_p, arm) in arms { walk_expr(arm, name, refs); }
            }
        }
    }
    fn walk_stmt(stmt: &Stmt, name: &str, refs: &mut Vec<(usize, usize)>) {
        match stmt {
            Stmt::Let { init, .. } | Stmt::Assign { value: init, .. } => walk_expr(init, name, refs),
            Stmt::IndexAssign { object, index, value, .. } => {
                walk_expr(object, name, refs);
                walk_expr(index, name, refs);
                walk_expr(value, name, refs);
            }
            Stmt::Expr(e) => walk_expr(e, name, refs),
            Stmt::If { condition, then_branch, .. } => {
                walk_expr(condition, name, refs);
                for s in then_branch { walk_stmt(s, name, refs); }
            }
            Stmt::For { iterable, body, .. } => {
                walk_expr(iterable, name, refs);
                for s in body { walk_stmt(s, name, refs); }
            }
            Stmt::Try { try_block, catch_block, .. } => {
                for s in try_block { walk_stmt(s, name, refs); }
                for s in catch_block { walk_stmt(s, name, refs); }
            }
            Stmt::Parallel { stmts, .. } => {
                for s in stmts { walk_stmt(s, name, refs); }
            }
            Stmt::Match { expr, arms, .. } => {
                walk_expr(expr, name, refs);
                for (_p, arm_stmts) in arms {
                    for s in arm_stmts { walk_stmt(s, name, refs); }
                }
            }
            Stmt::Return { value: Some(v), .. } => walk_expr(v, name, refs),
            _ => {}
        }
    }
    for stmt in stmts { walk_stmt(stmt, name, refs); }
}

// ===================================================================
// Hover
// ===================================================================

pub fn hover(docs: &HashMap<String, DocumentState>, params: &Value) -> Result<Value, String> {
    let uri = params.get("textDocument").and_then(|t| t.get("uri")).and_then(|u| u.as_str())
        .ok_or("missing textDocument.uri")?;
    let pos = params.get("position")
        .ok_or("missing position")?;
    let line = pos.get("line").and_then(|n| n.as_i64()).unwrap_or(0) as usize;
    let col = pos.get("character").and_then(|n| n.as_i64()).unwrap_or(0) as usize;

    let (text, stmts) = parsed_doc(docs, uri).ok_or("document not found")?;
    let offset = position_to_offset(&text, line, col);
    let ident = match ident_at_offset(&text, offset) {
        Some(s) => s,
        None => return Ok(Value::Null),
    };

    // 找 typeck 类型
    let type_errors = crate::typeck::check_program(&stmts);

    // 简化的 hover：返回变量名 + 推断类型
    let mut contents = format!("```mora\nlet {}: <inferred>\n```", ident);
    // 查找 let 的 type_hint
    for stmt in &stmts {
        if let Stmt::Let { name, type_hint, .. } = stmt {
            if name == &ident {
                if let Some(h) = type_hint {
                    contents = format!("```mora\nlet {}: {} = ...\n```", ident, h);
                }
            }
        }
        if let Stmt::TaskDef { name, params, return_type, .. } = stmt {
            if name == &ident {
                let param_strs: Vec<String> = params.iter()
                    .map(|(n, h)| match h {
                        Some(t) => format!("{}: {}", n, t),
                        None => n.clone(),
                    })
                    .collect();
                let ret = return_type.as_deref().unwrap_or("any");
                contents = format!("```mora\ntask {}({}): {}\n```", ident, param_strs.join(", "), ret);
            }
        }
    }
    // 把 typeck 错误数也带回去
    contents.push_str(&format!("\n\n---\ntypeck: {} error(s)", type_errors.len()));

    let mut m = BTreeMap::new();
    m.insert("contents".to_string(), Value::String_(contents));
    Ok(Value::Object(m))
}

// ===================================================================
// Completion
// ===================================================================

pub fn completion(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
    let uri = match params.get("textDocument").and_then(|t| t.get("uri")).and_then(|u| u.as_str()) {
        Some(s) => s,
        None => return Value::Array(vec![]),
    };
    let (_text, stmts) = match parsed_doc(docs, uri) {
        Some(t) => t,
        None => return Value::Array(vec![]),
    };

    let mut items: Vec<Value> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    // 1. 关键字
    for kw in ["let", "task", "if", "then", "end", "for", "in", "try", "catch",
               "return", "fn", "true", "false", "nil", "match", "with",
               "save", "load", "import", "parallel", "read", "write", "append",
               "read_bytes", "write_bytes", "into", "export"] {
        if seen.insert(kw.to_string()) {
            let mut m = BTreeMap::new();
            m.insert("label".to_string(), Value::String_(kw.to_string()));
            m.insert("kind".to_string(), Value::Number(14.0));  // Keyword
            items.push(Value::Object(m));
        }
    }

    // 2. 局部变量（所有 let 名字）
    for stmt in &stmts {
        if let Stmt::Let { name, .. } = stmt {
            if seen.insert(name.clone()) {
                let mut m = BTreeMap::new();
                m.insert("label".to_string(), Value::String_(name.clone()));
                m.insert("kind".to_string(), Value::Number(6.0));  // Variable
                items.push(Value::Object(m));
            }
        }
        if let Stmt::For { var, .. } = stmt {
            if seen.insert(var.clone()) {
                let mut m = BTreeMap::new();
                m.insert("label".to_string(), Value::String_(var.clone()));
                m.insert("kind".to_string(), Value::Number(6.0));
                items.push(Value::Object(m));
            }
        }
    }

    // 3. 任务名
    for stmt in &stmts {
        if let Stmt::TaskDef { name, .. } = stmt {
            if seen.insert(name.clone()) {
                let mut m = BTreeMap::new();
                m.insert("label".to_string(), Value::String_(name.clone()));
                m.insert("kind".to_string(), Value::Number(3.0));  // Function
                items.push(Value::Object(m));
            }
        }
    }

    // 4. 内置对象 / builtin 方法
    for builtin in ["ai", "web", "json", "file", "memory", "agent"] {
        if seen.insert(builtin.to_string()) {
            let mut m = BTreeMap::new();
            m.insert("label".to_string(), Value::String_(builtin.to_string()));
            m.insert("kind".to_string(), Value::Number(9.0));  // Module
            items.push(Value::Object(m));
        }
    }

    Value::Array(items)
}

// ===================================================================
// Go-to-definition
// ===================================================================

pub fn definition(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
    let uri = match params.get("textDocument").and_then(|t| t.get("uri")).and_then(|u| u.as_str()) {
        Some(s) => s,
        None => return Value::Array(vec![]),
    };
    let pos = match params.get("position") {
        Some(p) => p,
        None => return Value::Array(vec![]),
    };
    let line = pos.get("line").and_then(|n| n.as_i64()).unwrap_or(0) as usize;
    let col = pos.get("character").and_then(|n| n.as_i64()).unwrap_or(0) as usize;

    let (text, stmts) = match parsed_doc(docs, uri) {
        Some(t) => t,
        None => return Value::Array(vec![]),
    };
    let offset = position_to_offset(&text, line, col);
    let ident = match ident_at_offset(&text, offset) {
        Some(s) => s,
        None => return Value::Array(vec![]),
    };

    let defs = collect_definitions(&stmts);
    let locations: Vec<Value> = defs.get(&ident)
        .map(|v| v.iter().map(|(l, c)| {
            let mut m = BTreeMap::new();
            m.insert("uri".to_string(), Value::String_(uri.to_string()));
            m.insert("range".to_string(), Value::Object({
                let mut r = BTreeMap::new();
                r.insert("start".to_string(), Value::Object({
                    let mut p = BTreeMap::new();
                    p.insert("line".to_string(), Value::Number(*l as f64));
                    p.insert("character".to_string(), Value::Number(*c as f64));
                    p
                }));
                r.insert("end".to_string(), Value::Object({
                    let mut p = BTreeMap::new();
                    p.insert("line".to_string(), Value::Number(*l as f64));
                    p.insert("character".to_string(), Value::Number((*c + ident.len()) as f64));
                    p
                }));
                r
            }));
            Value::Object(m)
        }).collect())
        .unwrap_or_default();
    Value::Array(locations)
}

// ===================================================================
// References
// ===================================================================

pub fn references(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
    let uri = match params.get("textDocument").and_then(|t| t.get("uri")).and_then(|u| u.as_str()) {
        Some(s) => s,
        None => return Value::Array(vec![]),
    };
    let pos = match params.get("position") {
        Some(p) => p,
        None => return Value::Array(vec![]),
    };
    let line = pos.get("line").and_then(|n| n.as_i64()).unwrap_or(0) as usize;
    let col = pos.get("character").and_then(|n| n.as_i64()).unwrap_or(0) as usize;

    let (text, stmts) = match parsed_doc(docs, uri) {
        Some(t) => t,
        None => return Value::Array(vec![]),
    };
    let offset = position_to_offset(&text, line, col);
    let ident = match ident_at_offset(&text, offset) {
        Some(s) => s,
        None => return Value::Array(vec![]),
    };
    let _include_decl = params.get("context").and_then(|c| c.get("includeDeclaration")).and_then(|v| v.as_i64()).unwrap_or(1) != 0;

    let mut refs = Vec::new();
    collect_references(&stmts, &ident, &mut refs);

    // 简化：refs 里的 (0, 0) 表示"找到但行号未知"
    // 对于引用查找我们只能给出找到的事实；行号精确化是后续工作
    let locations: Vec<Value> = refs.iter().map(|(l, c)| {
        let mut m = BTreeMap::new();
        m.insert("uri".to_string(), Value::String_(uri.to_string()));
        m.insert("range".to_string(), Value::Object({
            let mut r = BTreeMap::new();
            r.insert("start".to_string(), Value::Object({
                let mut p = BTreeMap::new();
                p.insert("line".to_string(), Value::Number(*l as f64));
                p.insert("character".to_string(), Value::Number(*c as f64));
                p
            }));
            r.insert("end".to_string(), Value::Object({
                let mut p = BTreeMap::new();
                p.insert("line".to_string(), Value::Number(*l as f64));
                p.insert("character".to_string(), Value::Number((*c + ident.len()) as f64));
                p
            }));
            r
        }));
        Value::Object(m)
    }).collect();
    Value::Array(locations)
}

// ===================================================================
// Document Symbols
// ===================================================================

pub fn document_symbol(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
    let uri = match params.get("textDocument").and_then(|t| t.get("uri")).and_then(|u| u.as_str()) {
        Some(s) => s,
        None => return Value::Array(vec![]),
    };
    let (_text, stmts) = match parsed_doc(docs, uri) {
        Some(t) => t,
        None => return Value::Array(vec![]),
    };
    let mut out = Vec::new();
    for stmt in &stmts {
        match stmt {
            Stmt::Let { name, span, .. } => {
                let mut m = BTreeMap::new();
                m.insert("name".to_string(), Value::String_(name.clone()));
                m.insert("kind".to_string(), Value::Number(13.0));  // Variable
                m.insert("range".to_string(), Value::Object({
                    let mut r = BTreeMap::new();
                    r.insert("start".to_string(), Value::Object({
                        let mut p = BTreeMap::new();
                        p.insert("line".to_string(), Value::Number(span.line as f64));
                        p.insert("character".to_string(), Value::Number(span.column as f64));
                        p
                    }));
                    r.insert("end".to_string(), Value::Object({
                        let mut p = BTreeMap::new();
                        p.insert("line".to_string(), Value::Number(span.line as f64));
                        p.insert("character".to_string(), Value::Number((span.column + name.len()) as f64));
                        p
                    }));
                    r
                }));
                m.insert("selectionRange".to_string(), Value::Object({
                    let mut r = BTreeMap::new();
                    r.insert("start".to_string(), Value::Object({
                        let mut p = BTreeMap::new();
                        p.insert("line".to_string(), Value::Number(span.line as f64));
                        p.insert("character".to_string(), Value::Number(span.column as f64));
                        p
                    }));
                    r.insert("end".to_string(), Value::Object({
                        let mut p = BTreeMap::new();
                        p.insert("line".to_string(), Value::Number(span.line as f64));
                        p.insert("character".to_string(), Value::Number((span.column + name.len()) as f64));
                        p
                    }));
                    r
                }));
                out.push(Value::Object(m));
            }
            Stmt::TaskDef { name, span, .. } => {
                let mut m = BTreeMap::new();
                m.insert("name".to_string(), Value::String_(name.clone()));
                m.insert("kind".to_string(), Value::Number(12.0));  // Function
                m.insert("range".to_string(), Value::Object({
                    let mut r = BTreeMap::new();
                    r.insert("start".to_string(), Value::Object({
                        let mut p = BTreeMap::new();
                        p.insert("line".to_string(), Value::Number(span.line as f64));
                        p.insert("character".to_string(), Value::Number(span.column as f64));
                        p
                    }));
                    r.insert("end".to_string(), Value::Object({
                        let mut p = BTreeMap::new();
                        p.insert("line".to_string(), Value::Number(span.line as f64));
                        p.insert("character".to_string(), Value::Number((span.column + name.len()) as f64));
                        p
                    }));
                    r
                }));
                out.push(Value::Object(m));
            }
            _ => {}
        }
    }
    Value::Array(out)
}

// ===================================================================
// Formatting
// ===================================================================

pub fn formatting(docs: &HashMap<String, DocumentState>, params: &Value, range: bool) -> Value {
    let uri = match params.get("textDocument").and_then(|t| t.get("uri")).and_then(|u| u.as_str()) {
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
        let s = params.get("range").unwrap();
        let start = s.get("start").unwrap();
        let end = s.get("end").unwrap();
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
    m.insert("range".to_string(), Value::Object({
        let mut r = BTreeMap::new();
        r.insert("start".to_string(), Value::Object({
            let mut p = BTreeMap::new();
            p.insert("line".to_string(), Value::Number(start_line as f64));
            p.insert("character".to_string(), Value::Number(start_col as f64));
            p
        }));
        r.insert("end".to_string(), Value::Object({
            let mut p = BTreeMap::new();
            p.insert("line".to_string(), Value::Number(end_line as f64));
            p.insert("character".to_string(), Value::Number(end_col as f64));
            p
        }));
        r
    }));
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
                if needs_indent { push_indent(&mut out, depth); needs_indent = false; }
                out.push_str(&token_text(&tok.token_type));
            }
            TokenType::RBrace | TokenType::RBracket | TokenType::RParen => {
                if !out.ends_with('\n') && !out.is_empty() {
                    // 同行的右括号前面 trim 一下
                }
                if depth > 0 { depth -= 1; }
                if !out.ends_with('\n') && !out.is_empty() && !out.ends_with('(') && !out.ends_with('[') && !out.ends_with('{') {
                    // 简单：直接接
                }
                out.push_str(&token_text(&tok.token_type));
            }
            _ => {
                if needs_indent { push_indent(&mut out, depth); needs_indent = false; }
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
    for _ in 0..depth { out.push_str("  "); }
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

pub fn rename(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
    let uri = match params.get("textDocument").and_then(|t| t.get("uri")).and_then(|u| u.as_str()) {
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
    let (text, stmts) = match parsed_doc(docs, uri) {
        Some(t) => t,
        None => return Value::Null,
    };
    let offset = position_to_offset(&text, line, col);
    let old_name = match ident_at_offset(&text, offset) {
        Some(s) => s,
        None => return Value::Null,
    };

    let defs = collect_definitions(&stmts);
    let mut refs = Vec::new();
    collect_references(&stmts, &old_name, &mut refs);

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

    let edit_list: Vec<Value> = edits.iter().map(|(l, c)| {
        let mut m = BTreeMap::new();
        m.insert("range".to_string(), Value::Object({
            let mut r = BTreeMap::new();
            r.insert("start".to_string(), Value::Object({
                let mut p = BTreeMap::new();
                p.insert("line".to_string(), Value::Number(*l as f64));
                p.insert("character".to_string(), Value::Number(*c as f64));
                p
            }));
            r.insert("end".to_string(), Value::Object({
                let mut p = BTreeMap::new();
                p.insert("line".to_string(), Value::Number(*l as f64));
                p.insert("character".to_string(), Value::Number((*c + old_name.len()) as f64));
                p
            }));
            r
        }));
        m.insert("newText".to_string(), Value::String_(new_name.clone()));
        Value::Object(m)
    }).collect();

    let mut result = BTreeMap::new();
    let mut changes = BTreeMap::new();
    changes.insert(uri.to_string(), Value::Array(edit_list));
    result.insert("changes".to_string(), Value::Object(changes));
    Value::Object(result)
}

// ===================================================================
// Semantic tokens
// ===================================================================

pub fn semantic_tokens(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
    let uri = match params.get("textDocument").and_then(|t| t.get("uri")).and_then(|u| u.as_str()) {
        Some(s) => s,
        None => return Value::Object({
            let mut m = BTreeMap::new();
            m.insert("data".to_string(), Value::Array(vec![]));
            m
        }),
    };
    let (text, stmts) = match parsed_doc(docs, uri) {
        Some(t) => t,
        None => return Value::Object({
            let mut m = BTreeMap::new();
            m.insert("data".to_string(), Value::Array(vec![]));
            m
        }),
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
            TokenType::For | TokenType::In | TokenType::Try | TokenType::Catch | TokenType::Return |
            TokenType::Fn | TokenType::True | TokenType::False | TokenType::Nil | TokenType::Match |
            TokenType::With | TokenType::Save | TokenType::Load | TokenType::Import |
            TokenType::Parallel | TokenType::Read | TokenType::Write | TokenType::Append |
            TokenType::ReadBytes | TokenType::WriteBytes | TokenType::Into | TokenType::Export => 0,  // keyword
            TokenType::String(_) => 3,  // string
            TokenType::Number(_) => 4,  // number
            _ => continue,
        };
        let len = token_len(&tok.token_type) as u32;
        let dl = (line as u32).saturating_sub(last_line);
        let dc = if dl == 0 { (col).saturating_sub(last_col) } else { col };
        data.push(dl);
        data.push(dc);
        data.push(token_type);
        data.push(0);  // modifiers
        data.push(len);
        last_line = line as u32;
        last_col = col;
    }

    // 给已知任务名加 function token
    for stmt in &stmts {
        if let Stmt::TaskDef { name, span, .. } = stmt {
            let dl = (span.line as u32).saturating_sub(last_line);
            let dc = if dl == 0 { (span.column as u32).saturating_sub(last_col) } else { span.column as u32 };
            data.push(dl);
            data.push(dc);
            data.push(1);  // function
            data.push(0);
            data.push(name.len() as u32);
            last_line = span.line as u32;
            last_col = span.column as u32;
        }
        if let Stmt::Let { name, span, .. } = stmt {
            let dl = (span.line as u32).saturating_sub(last_line);
            let dc = if dl == 0 { (span.column as u32).saturating_sub(last_col) } else { span.column as u32 };
            data.push(dl);
            data.push(dc);
            data.push(2);  // variable
            data.push(0);
            data.push(name.len() as u32);
            last_line = span.line as u32;
            last_col = span.column as u32;
        }
    }

    let mut m = BTreeMap::new();
    m.insert("data".to_string(), Value::Array(data.into_iter().map(|n| Value::Number(n as f64)).collect()));
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

pub fn folding_range(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
    let uri = match params.get("textDocument").and_then(|t| t.get("uri")).and_then(|u| u.as_str()) {
        Some(s) => s,
        None => return Value::Array(vec![]),
    };
    let (_text, stmts) = match parsed_doc(docs, uri) {
        Some(t) => t,
        None => return Value::Array(vec![]),
    };

    let mut out = Vec::new();
    fn collect(stmts: &[Stmt], out: &mut Vec<(usize, usize)>) {
        for stmt in stmts {
            match stmt {
                Stmt::If { span, then_branch, .. } => {
                    if let Some(end_stmt) = then_branch.last() {
                        out.push((span.line, end_stmt_span_line(end_stmt)));
                    }
                }
                Stmt::For { span, body, .. } => {
                    if let Some(end_stmt) = body.last() {
                        out.push((span.line, end_stmt_span_line(end_stmt)));
                    }
                }
                Stmt::TaskDef { span, body, .. } => {
                    if let Some(end_stmt) = body.last() {
                        out.push((span.line, end_stmt_span_line(end_stmt)));
                    }
                }
                Stmt::Try { span, catch_block, .. } => {
                    if let Some(end_stmt) = catch_block.last() {
                        out.push((span.line, end_stmt_span_line(end_stmt)));
                    }
                }
                Stmt::Match { span, arms, .. } => {
                    if let Some((_p, last_arm)) = arms.last() {
                        if let Some(end_stmt) = last_arm.last() {
                            out.push((span.line, end_stmt_span_line(end_stmt)));
                        }
                    }
                }
                Stmt::Parallel { span, stmts: inner } => {
                    if let Some(end_stmt) = inner.last() {
                        out.push((span.line, end_stmt_span_line(end_stmt)));
                    }
                }
                _ => {}
            }
        }
    }
    collect(&stmts, &mut out);

    let ranges: Vec<Value> = out.iter().map(|(s, e)| {
        let mut m = BTreeMap::new();
        m.insert("startLine".to_string(), Value::Number(*s as f64));
        m.insert("endLine".to_string(), Value::Number(*e as f64));
        m.insert("kind".to_string(), Value::String_("region".to_string()));
        Value::Object(m)
    }).collect();
    Value::Array(ranges)
}

fn end_stmt_span_line(stmt: &Stmt) -> usize {
    match stmt {
        Stmt::Let { span, .. }
        | Stmt::Assign { span, .. }
        | Stmt::IndexAssign { span, .. }
        | Stmt::TaskDef { span, .. }
        | Stmt::If { span, .. }
        | Stmt::For { span, .. }
        | Stmt::Try { span, .. }
        | Stmt::Import { span, .. }
        | Stmt::Parallel { span, .. }
        | Stmt::Match { span, .. }
        | Stmt::Save { span, .. }
        | Stmt::Load { span, .. }
        | Stmt::ReadFile { span, .. }
        | Stmt::WriteFile { span, .. }
        | Stmt::AppendFile { span, .. }
        | Stmt::ReadBytesFile { span, .. }
        | Stmt::WriteBytesFile { span, .. }
        | Stmt::Return { span, .. } => span.line,
        Stmt::Expr(_) => 0,
    }
}
