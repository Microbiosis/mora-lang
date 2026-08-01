//! v0.55: Parser V3 / MirExpr LSP data accessor.
//!
//! `parsed_doc_v3` is the MirExpr counterpart of the historical
//! `parsed_doc_v2` helper. Every LSP provider in this folder should pull
//! its parsed data through this function so the cache, the parser, and
//! the typeck layer stay in sync.

use std::collections::HashMap;

use crate::lsp::json::Value as JsonValue;
use crate::lsp::server::DocumentState;
use crate::mir::MirExpr;

///  Look up and parse MirExpr list for `uri`.
///  Returns `None` when the document has not been opened yet or parse fails.
pub fn parsed_doc_v3(
    docs: &HashMap<String, DocumentState>,
    uri: &str,
) -> Option<(String, Vec<MirExpr>)> {
    let doc = docs.get(uri)?;
    // Parse MirExpr from text (no cache in DocumentState yet)
    use crate::lexer::Lexer;
    use crate::parser_v3::ParserV3;
    let tokens = Lexer::new(&doc.text).scan_tokens();
    let parser = ParserV3::new(tokens);
    let exprs = parser.parse().ok()?;
    Some((doc.text.clone(), exprs))
}

///  Walks the entire MirExpr tree and invokes `visit` for every
///  [`MirExpr`] node. Used by the references, rename, and semantic
///  providers to avoid recursive duplication.
pub fn walk_mir_expr<F: FnMut(&MirExpr)>(expr: &MirExpr, visit: &mut F) {
    visit(expr);
    walk_mir_expr_kind(&expr.kind, visit);
}

fn walk_mir_expr_kind<F: FnMut(&MirExpr)>(kind: &crate::mir::expr::MirExprKind, visit: &mut F) {
    use crate::mir::expr::MirExprKind;
    match kind {
        MirExprKind::Literal(_) | MirExprKind::Variable(_) => {}
        MirExprKind::Binary { left, right, .. } => {
            walk_mir_expr(left, visit);
            walk_mir_expr(right, visit);
        }
        MirExprKind::Call { args, .. } => {
            for arg in args {
                walk_mir_expr(arg, visit);
            }
        }
        MirExprKind::MethodCall { receiver, args, .. } => {
            walk_mir_expr(receiver, visit);
            for arg in args {
                walk_mir_expr(arg, visit);
            }
        }
        MirExprKind::Closure { body, .. } => walk_mir_expr(body, visit),
        MirExprKind::FnDef { body, .. } => walk_mir_expr(body, visit),
        MirExprKind::Match { scrutinee, arms } => {
            walk_mir_expr(scrutinee, visit);
            for arm in arms {
                walk_mir_expr(&arm.body, visit);
            }
        }
        MirExprKind::If { cond, then, r#else } => {
            walk_mir_expr(cond, visit);
            walk_mir_expr(then, visit);
            if let Some(e) = r#else {
                walk_mir_expr(e, visit);
            }
        }
        MirExprKind::List(items) => {
            for item in items {
                walk_mir_expr(item, visit);
            }
        }
        MirExprKind::Dict(entries) => {
            for (_, value) in entries {
                walk_mir_expr(value, visit);
            }
        }
        MirExprKind::DynTrait { expr, .. } => walk_mir_expr(expr, visit),
        MirExprKind::Prompt { parts } => {
            for part in parts {
                walk_mir_expr(part, visit);
            }
        }
        MirExprKind::LetBinding {
            value, init_body, ..
        } => {
            walk_mir_expr(value, visit);
            walk_mir_expr(init_body, visit);
        }
        MirExprKind::Assign { value, .. } => walk_mir_expr(value, visit),
        MirExprKind::Orchestrate { kind, .. } => walk_mir_orchestrate(kind, visit),
        MirExprKind::Loop {
            var: _,
            iterable,
            body,
        } => {
            walk_mir_expr(iterable, visit);
            walk_mir_expr(body, visit);
        }
        MirExprKind::While { cond, body } => {
            walk_mir_expr(cond, visit);
            walk_mir_expr(body, visit);
        }
        MirExprKind::Or { left, right } | MirExprKind::And { left, right } => {
            walk_mir_expr(left, visit);
            walk_mir_expr(right, visit);
        }
        MirExprKind::Return(value) => {
            if let Some(v) = value {
                walk_mir_expr(v, visit);
            }
        }
        MirExprKind::Break(_) | MirExprKind::Continue(_) => {}
        MirExprKind::IndexAssign {
            object,
            index,
            value,
        } => {
            walk_mir_expr(object, visit);
            walk_mir_expr(index, visit);
            walk_mir_expr(value, visit);
        }
        MirExprKind::TypeAlias { .. }
        | MirExprKind::EnumDef { .. }
        | MirExprKind::StructDef { .. }
        | MirExprKind::Import(_)
        | MirExprKind::MacroDef { .. }
        | MirExprKind::Sequence(_) => {}
    }
}

fn walk_mir_orchestrate(
    kind: &crate::mir::expr::MirOrchestrateKind,
    visit: &mut dyn FnMut(&MirExpr),
) {
    use crate::mir::expr::MirOrchestrateKind;
    match kind {
        MirOrchestrateKind::Sequential { agents }
        | MirOrchestrateKind::Graph { agents, .. }
        | MirOrchestrateKind::Pregel { agents, .. } => {
            for a in agents {
                visit(&a.task_expr);
                if let Some(v) = &a.verify_expr {
                    visit(v);
                }
                if let Some(cfg) = &a.with_config {
                    for e in cfg.values() {
                        visit(e);
                    }
                }
            }
        }
        MirOrchestrateKind::Loop {
            agents, exit_when, ..
        } => {
            for agent in agents {
                visit(&agent.task_expr);
                if let Some(v) = &agent.verify_expr {
                    visit(v);
                }
                if let Some(cfg) = &agent.with_config {
                    for e in cfg.values() {
                        visit(e);
                    }
                }
            }
            if let Some(e) = exit_when {
                visit(e);
            }
        }
    }
}

///  Collect every `(name, span)` pair introduced by `let` bindings or
///  `fn` definitions in the program. Used by completion/definition/hover.
pub fn collect_definitions_v3(exprs: &[MirExpr]) -> Vec<(String, crate::common::Span)> {
    let mut out = Vec::new();
    for expr in exprs {
        collect_definitions_in_expr(expr, &mut out);
    }
    out
}

fn collect_definitions_in_expr(expr: &MirExpr, out: &mut Vec<(String, crate::common::Span)>) {
    match &expr.kind {
        crate::mir::expr::MirExprKind::LetBinding { name, .. } => {
            out.push((name.clone(), expr.span));
        }
        crate::mir::expr::MirExprKind::FnDef { name, .. } => {
            out.push((name.clone(), expr.span));
        }
        _ => {}
    }
    walk_mir_expr(expr, &mut |e| match &e.kind {
        crate::mir::expr::MirExprKind::LetBinding { name, .. } => {
            out.push((name.clone(), e.span));
        }
        crate::mir::expr::MirExprKind::FnDef { name, .. } => {
            out.push((name.clone(), e.span));
        }
        _ => {}
    });
}

///  Collect every read site of `name` across the program.
pub fn collect_references_v3(exprs: &[MirExpr], name: &str) -> Vec<crate::common::Span> {
    let mut out = Vec::new();
    for expr in exprs {
        collect_references_in_expr(expr, name, &mut out);
    }
    out
}

fn collect_references_in_expr(expr: &MirExpr, name: &str, out: &mut Vec<crate::common::Span>) {
    walk_mir_expr(expr, &mut |e| {
        if let crate::mir::expr::MirExprKind::Variable(n) = &e.kind
            && n == name
        {
            out.push(e.span);
        }
    });
}

use std::collections::BTreeMap;

/// 文本位置 → 字节偏移
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

/// 创建 LSP completion item
pub(super) fn make_completion(label: &str, kind: f64, detail: Option<&str>) -> JsonValue {
    let mut m = BTreeMap::new();
    m.insert("label".to_string(), JsonValue::String_(label.to_string()));
    m.insert("kind".to_string(), JsonValue::Number(kind));
    if let Some(d) = detail {
        m.insert("detail".to_string(), JsonValue::String_(d.to_string()));
    }
    JsonValue::Object(m)
}

/// 在某 offset 取一个标识符（变量名）
pub(super) fn ident_at_offset(text: &str, offset: usize) -> Option<String> {
    let bytes = text.as_bytes();
    if offset > bytes.len() {
        return None;
    }
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
