//! v0.110.0: LSP foldingRange provider —— 基于 `MirWitness` 树给出可折叠区间。
//!
//! 覆盖面：`if` / `match` / `fn` / 闭包 / 循环（`for` 在 witness 层是 `Loop`）/
//! `while` / `macro` / `parallel` / `handle` 等**带 body** 的构造。
//!
//! 结束行取「体内最后一条语句所在的行」（`Sequence` 递归取子 witness 的最大行），
//! 而不是 body 自身的 span —— 后者继承自外层构造，对结束行没有信息量。
//!
//! 坐标不变式：`Span::line` 是 **1-based**，LSP 的 `startLine` / `endLine` 是 **0-based**，
//! 出站前统一 `saturating_sub(1)`。

use std::collections::HashMap;

use super::parsed_doc_v3;
use crate::flow::json::JsonValue as Value;
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
        collect_folds(&expr, &mut ranges);
    }
    Value::Array(ranges)
}

fn collect_folds(root: &crate::mir::witness::MirWitness, out: &mut Vec<Value>) {
    use crate::mir::witness::WitnessKind;
    let mut stack = vec![root];
    while let Some(expr) = stack.pop() {
        match &expr.kind {
            WitnessKind::If { then, r#else, .. } => {
                let end_line = r#else
                    .as_ref()
                    .map(|e| e.span.line)
                    .unwrap_or(then.span.line);
                if end_line > expr.span.line {
                    out.push(make_range(expr.span.line, end_line));
                }
                if let Some(e) = r#else {
                    stack.push(e);
                }
                stack.push(then);
            }
            WitnessKind::Match { arms, .. } => {
                let end_line = arms
                    .iter()
                    .map(|a| a.body.span.line)
                    .max()
                    .unwrap_or(expr.span.line);
                if end_line > expr.span.line {
                    out.push(make_range(expr.span.line, end_line));
                }
                for arm in arms.iter().rev() {
                    stack.push(&arm.body);
                }
            }
            WitnessKind::FnDef { body, .. } | WitnessKind::Closure { body, .. } => {
                let end_line = body_end_line(body);
                if end_line > expr.span.line {
                    out.push(make_range(expr.span.line, end_line));
                }
                stack.push(body);
            }
            WitnessKind::Loop { body, .. } | WitnessKind::While { body, .. } => {
                if body.span.line > expr.span.line {
                    out.push(make_range(expr.span.line, body.span.line));
                }
                stack.push(body);
            }
            WitnessKind::MacroDef { body, .. } | WitnessKind::Parallel { body, .. } => {
                let end_line = body_end_line(body);
                if end_line > expr.span.line {
                    out.push(make_range(expr.span.line, end_line));
                }
                stack.push(body);
            }
            WitnessKind::Handle { body, handler, .. } => {
                let end_line = body_end_line(body).max(handler.span.line);
                if end_line > expr.span.line {
                    out.push(make_range(expr.span.line, end_line));
                }
                stack.push(handler);
                stack.push(body);
            }
            WitnessKind::Sequence(items) => {
                for item in items.iter().rev() {
                    stack.push(item);
                }
            }
            _ => {}
        }
    }
}

/// 体内**最后一条语句**所在的行；非 `Sequence` 就是它自己的 span（显式堆栈迭代，O(1) OS 栈）。
fn body_end_line(root: &crate::mir::witness::MirWitness) -> usize {
    use crate::mir::witness::WitnessKind;
    let mut best = root.span.line;
    let mut stack = vec![root];
    while let Some(expr) = stack.pop() {
        match &expr.kind {
            WitnessKind::Sequence(items) => {
                if items.is_empty() {
                    best = best.max(expr.span.line);
                } else {
                    for item in items {
                        stack.push(item);
                    }
                }
            }
            _ => {
                best = best.max(expr.span.line);
            }
        }
    }
    best
}

fn make_range(start_line: usize, end_line: usize) -> Value {
    let mut m = std::collections::BTreeMap::new();
    // v0.104.6 D158：LSP 的 `FoldingRange.startLine` / `endLine` 是 **0-based**
    // （与 `Position.line` 同约定），而 `Span::line` 是 **1-based**。
    // 此前**直接透传** → 折叠箭头整体下移一行。
    // 同一 LSP 里另外 5 个 provider（diagnostics / definition / references /
    // rename / documentSymbol）**都**有 `saturating_sub(1)`，唯独这里漏了
    // —— 与 D133 记的「这三个 provider 漏了」同型，folding 是漏网的第四个。
    m.insert(
        "startLine".to_string(),
        Value::Number(start_line.saturating_sub(1) as f64),
    );
    m.insert(
        "endLine".to_string(),
        Value::Number(end_line.saturating_sub(1) as f64),
    );
    Value::Object(m)
}
