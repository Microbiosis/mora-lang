use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;
use std::collections::{BTreeMap, HashMap};

pub(super) fn position_to_offset(text: &str, line: usize, col: usize) -> usize {
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
pub(super) fn get_line_prefix(text: &str, line: usize, col: usize) -> String {
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
pub(super) fn make_completion(label: &str, kind: f64, detail: Option<&str>) -> Value {
    let mut m = BTreeMap::new();
    m.insert("label".to_string(), Value::String_(label.to_string()));
    m.insert("kind".to_string(), Value::Number(kind));
    if let Some(d) = detail {
        m.insert("detail".to_string(), Value::String_(d.to_string()));
    }
    Value::Object(m)
}

/// 在某 offset 取一个标识符（变量名）
pub(super) fn ident_at_offset(text: &str, offset: usize) -> Option<String> {
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
pub(super) fn parsed_doc_v2(
    docs: &HashMap<String, DocumentState>,
    uri: &str,
) -> Option<(String, Vec<crate::ast_v2::NodeId>, crate::ast_v2::AstArena)> {
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
pub(super) fn collect_definitions_v2(
    stmt_ids: &[crate::ast_v2::NodeId],
    arena: &crate::ast_v2::AstArena,
) -> HashMap<String, Vec<(usize, usize)>> {
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
pub(super) fn collect_references_v2(
    stmt_ids: &[crate::ast_v2::NodeId],
    arena: &crate::ast_v2::AstArena,
    name: &str,
    refs: &mut Vec<(usize, usize)>,
) {
    fn walk_expr_v2(
        expr_id: crate::ast_v2::NodeId,
        arena: &crate::ast_v2::AstArena,
        name: &str,
        refs: &mut Vec<(usize, usize)>,
    ) {
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
                crate::ast_v2::StmtKind::Return {
                    value: Some(expr_id),
                } => {
                    walk_expr_v2(*expr_id, arena, name, refs);
                }
                crate::ast_v2::StmtKind::If {
                    condition,
                    then_branch,
                    else_branch,
                } => {
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
