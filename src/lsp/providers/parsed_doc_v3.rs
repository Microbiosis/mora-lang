//! v0.55: Parser V3 / MirWitness LSP data accessor.
//!
//! `parsed_doc_v3` is the MirWitness counterpart of the historical
//! `parsed_doc_v2` helper. Every LSP provider in this folder should pull
//! its parsed data through this function so the cache, the parser, and
//! the typeck layer stay in sync.
//!
//! v0.75.42: 单遍编译 — ParserV3::compile 直接产出 witness（零 MirExpr
//! 桥接），解析失败返回 None（与旧 parse 路径一致）。

use std::collections::HashMap;

use crate::lsp::json::Value as JsonValue;
use crate::lsp::server::DocumentState;
use crate::mir::witness::{MirWitness, WitnessKind, WitnessOrchestrateKind};

///  Look up and parse MirWitness list for `uri`.
///  Returns `None` when the document has not been opened yet or parse fails.
pub fn parsed_doc_v3(
    docs: &HashMap<String, DocumentState>,
    uri: &str,
) -> Option<(String, Vec<MirWitness>)> {
    let doc = docs.get(uri)?;
    let (_, witnesses) = crate::parser_v3::ParserV3::compile(&doc.text).ok()?;
    Some((doc.text.clone(), witnesses))
}

///  Walks the entire MirWitness tree and invokes `visit` for every
///  [`MirWitness`] node. Used by the references, rename, and semantic
///  providers to avoid recursive duplication.
pub fn walk_witness<F: FnMut(&MirWitness)>(expr: &MirWitness, visit: &mut F) {
    visit(expr);
    walk_witness_kind(&expr.kind, visit);
}

fn walk_witness_kind<F: FnMut(&MirWitness)>(kind: &WitnessKind, visit: &mut F) {
    match kind {
        WitnessKind::Literal(_) | WitnessKind::Variable(_) => {}
        WitnessKind::Binary { left, right, .. } => {
            walk_witness(left, visit);
            walk_witness(right, visit);
        }
        WitnessKind::Call { args, .. } => {
            for arg in args {
                walk_witness(arg, visit);
            }
        }
        WitnessKind::MethodCall { receiver, args, .. } => {
            walk_witness(receiver, visit);
            for arg in args {
                walk_witness(arg, visit);
            }
        }
        WitnessKind::Closure { body, .. } => walk_witness(body, visit),
        WitnessKind::FnDef { body, .. } => walk_witness(body, visit),
        WitnessKind::Match { scrutinee, arms } => {
            walk_witness(scrutinee, visit);
            for arm in arms {
                walk_witness(&arm.body, visit);
            }
        }
        WitnessKind::If { cond, then, r#else } => {
            walk_witness(cond, visit);
            walk_witness(then, visit);
            if let Some(e) = r#else {
                walk_witness(e, visit);
            }
        }
        WitnessKind::List(items) => {
            for item in items {
                walk_witness(item, visit);
            }
        }
        WitnessKind::Dict(entries) => {
            for (_, value) in entries {
                walk_witness(value, visit);
            }
        }
        WitnessKind::DynTrait { expr, .. } => walk_witness(expr, visit),
        WitnessKind::Prompt { parts } => {
            for part in parts {
                walk_witness(part, visit);
            }
        }
        WitnessKind::LetBinding {
            value, init_body, ..
        } => {
            walk_witness(value, visit);
            walk_witness(init_body, visit);
        }
        WitnessKind::Assign { value, .. } => walk_witness(value, visit),
        WitnessKind::Orchestrate { kind, .. } => walk_witness_orchestrate(kind, visit),
        WitnessKind::Loop { iterable, body, .. } => {
            walk_witness(iterable, visit);
            walk_witness(body, visit);
        }
        WitnessKind::While { cond, body } => {
            walk_witness(cond, visit);
            walk_witness(body, visit);
        }
        WitnessKind::Or { left, right } | WitnessKind::And { left, right } => {
            walk_witness(left, visit);
            walk_witness(right, visit);
        }
        WitnessKind::Return(value) => {
            if let Some(v) = value {
                walk_witness(v, visit);
            }
        }
        WitnessKind::Break(_) | WitnessKind::Continue(_) => {}
        WitnessKind::IndexAssign {
            object,
            index,
            value,
        } => {
            walk_witness(object, visit);
            walk_witness(index, visit);
            walk_witness(value, visit);
        }
        WitnessKind::TypeAlias { .. }
        | WitnessKind::EnumDef { .. }
        | WitnessKind::StructDef { .. }
        | WitnessKind::Import(_)
        | WitnessKind::MacroDef { .. }
        | WitnessKind::Sequence(_) => {}
    }
}

fn walk_witness_orchestrate(kind: &WitnessOrchestrateKind, visit: &mut dyn FnMut(&MirWitness)) {
    match kind {
        WitnessOrchestrateKind::Sequential { agents }
        | WitnessOrchestrateKind::Graph { agents, .. }
        | WitnessOrchestrateKind::Pregel { agents, .. } => {
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
        WitnessOrchestrateKind::Loop {
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
        // v0.75.84: MoA — prompt 表达式参与 walk（LSP 语义/折叠）。
        WitnessOrchestrateKind::Moa { prompt, .. } => {
            visit(prompt);
        }
    }
}

///  Collect every `(name, span)` pair introduced by `let` bindings or
///  `fn` definitions in the program. Used by completion/definition/hover.
pub fn collect_definitions_v3(exprs: &[MirWitness]) -> Vec<(String, crate::common::Span)> {
    let mut out = Vec::new();
    for expr in exprs {
        collect_definitions_in_expr(expr, &mut out);
    }
    out
}

fn collect_definitions_in_expr(expr: &MirWitness, out: &mut Vec<(String, crate::common::Span)>) {
    match &expr.kind {
        WitnessKind::LetBinding { name, .. } => {
            out.push((name.clone(), expr.span));
        }
        WitnessKind::FnDef { name, .. } => {
            out.push((name.clone(), expr.span));
        }
        _ => {}
    }
    walk_witness(expr, &mut |e| match &e.kind {
        WitnessKind::LetBinding { name, .. } => {
            out.push((name.clone(), e.span));
        }
        WitnessKind::FnDef { name, .. } => {
            out.push((name.clone(), e.span));
        }
        _ => {}
    });
}

///  Collect every read site of `name` across the program.
pub fn collect_references_v3(exprs: &[MirWitness], name: &str) -> Vec<crate::common::Span> {
    let mut out = Vec::new();
    for expr in exprs {
        collect_references_in_expr(expr, name, &mut out);
    }
    out
}

fn collect_references_in_expr(expr: &MirWitness, name: &str, out: &mut Vec<crate::common::Span>) {
    walk_witness(expr, &mut |e| {
        if let WitnessKind::Variable(n) = &e.kind
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
