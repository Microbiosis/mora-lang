//! v0.25: LSP folding provider（折叠范围）。

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
        collect_folds(&expr, &mut ranges);
    }
    Value::Array(ranges)
}

fn collect_folds(expr: &crate::mir::witness::MirWitness, out: &mut Vec<Value>) {
    use crate::mir::witness::WitnessKind;
    match &expr.kind {
        WitnessKind::If { then, r#else, .. } => {
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
        WitnessKind::Match { arms, .. } => {
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
        WitnessKind::FnDef { body, .. } | WitnessKind::Closure { body, .. } => {
            // v0.104.6 D158：此前用 `body.span.line` 当折叠**结束行**，而
            // `parser_v3/emit.rs::block_witness` 给多条语句的 body 构造
            // `Sequence` 时用的是**外层构造的 span**（即 `task` 那一行），
            // 于是：
            //
            //   task alpha()          ← 第 1 行
            //     let x = 1           ← 第 2 行
            //     print(x)            ← 第 3 行
            //   end                    ← 第 4 行
            //
            // `body.span.line == 1 == expr.span.line` → `1 > 1` 为假 →
            // **整个折叠区间不产生**（实测：两个 task 只返回一个折叠）。
            // 单语句 body 走 `Call`/其它 witness，span 正确，才没暴露。
            //
            // 改用「体内最后一条语句的行」作为结束行 —— 对 `Sequence` 递归
            // 取子 witness 的最大行，否则退回 `body.span.line`。
            let end_line = body_end_line(body);
            if end_line > expr.span.line {
                out.push(make_range(expr.span.line, end_line));
            }
            collect_folds(body, out);
        }
        // v0.104.6 D136：补 `for` / `while` 两种循环 —— 此前**完全不可折叠**。
        //
        // 实测（真实 LSP 会话）修复前：
        //   `for i in [1, 2]` ⏎ `  print(i)` ⏎ `end`      → `[]`
        //   `while n < 2` ⏎ `  n = n + 1` ⏎ `end`          → `[]`
        //   `if` / `task`（以及 `task` 套 `if`）             → 正常，两层都出
        //
        // 即编辑器里最需要折叠的循环块恰恰一个都不折叠。注意 `for` 在 witness
        // 层是 **`Loop`**（`{ var, iterable, body }`）而**不是** `For` ——
        // 按 `For` 写分支是查不到它的。
        WitnessKind::Loop { body, .. } => {
            if body.span.line > expr.span.line {
                out.push(make_range(expr.span.line, body.span.line));
            }
            collect_folds(body, out);
        }
        WitnessKind::While { body, .. } => {
            if body.span.line > expr.span.line {
                out.push(make_range(expr.span.line, body.span.line));
            }
            collect_folds(body, out);
        }
        // v0.104.6 D159：补齐**声明块 / 配置块 / 可观测性块**的折叠覆盖面。
        //
        // D136 给 `for`/`while` 补了覆盖面，D158 修了位置 —— 但两者都只碰
        // 「语句块」。实测（真实 LSP 会话，文档含 5 种块形态，源码 1-based）：
        //
        // | 形态 | 源码行 | 修复前 |
        // |---|---|---|
        // | `model Point … end` | 1–4  | ✗ |
        // | `msg Move … end`   | 6–9  | ✗ |
        // | `task work() … end`| 11–15| ✓ |
        // | `with model = "m" … end` | 17–19 | ✗ |
        // | `app Counter … end`| 21–27| ✗ |
        //
        // 即 5 种里只有 `task` 能折叠。TEA 的 `app` 块动辄二十多行，
        // 不折叠等于编辑器里完全没法收起来。
        //
        // 本 arm 收的是**带 `MirWitness` body** 的那批（结束行可从 body 精确算出）。
        // `ModelDef` / `MsgDef` / `StructDef` / `EnumDef` 的字段是纯字符串 /
        // `TypeHint`，**不带 span**，结束行无从算起 —— 属 parser 层缺口，记档。
        WitnessKind::WithConfig { body, .. }
        | WitnessKind::UpdateDef { body, .. }
        | WitnessKind::MacroDef { body, .. }
        | WitnessKind::PromptSection { body, .. }
        | WitnessKind::DocumentSection { body, .. }
        | WitnessKind::Observe { body, .. }
        | WitnessKind::Span { body, .. }
        | WitnessKind::Parallel { body, .. } => {
            let end_line = body_end_line(body);
            if end_line > expr.span.line {
                out.push(make_range(expr.span.line, end_line));
            }
            collect_folds(body, out);
        }
        // `handle ask { … } { … }` —— body 与 handler **两段**都可含块
        WitnessKind::Handle { body, handler, .. } => {
            let end_line = body_end_line(body).max(handler.span.line);
            if end_line > expr.span.line {
                out.push(make_range(expr.span.line, end_line));
            }
            collect_folds(body, out);
            collect_folds(handler, out);
        }
        // TEA `app … end` —— 三个子 witness（init/update/view）取最大行
        WitnessKind::AppDef {
            init_w,
            update_w,
            view_w,
            ..
        } => {
            let end_line = body_end_line(init_w)
                .max(body_end_line(update_w))
                .max(body_end_line(view_w));
            if end_line > expr.span.line {
                out.push(make_range(expr.span.line, end_line));
            }
            collect_folds(init_w, out);
            collect_folds(update_w, out);
            collect_folds(view_w, out);
        }
        _ => {}
    }
}

/// 体内**最后一条语句**所在的行；非 `Sequence` 就是它自己的 span。
///
/// v0.104.6 D158：`Sequence` 的 span 继承自外层构造（见 `block_witness`），
/// 对「结束行」这个用途没有信息量，故必须递归到子 witness 取真实最大行。
fn body_end_line(expr: &crate::mir::witness::MirWitness) -> usize {
    use crate::mir::witness::WitnessKind;
    match &expr.kind {
        WitnessKind::Sequence(items) => items
            .iter()
            .map(body_end_line)
            .max()
            .unwrap_or(expr.span.line),
        _ => expr.span.line,
    }
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
