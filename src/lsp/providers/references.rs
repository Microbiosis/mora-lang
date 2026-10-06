//! v0.25: LSP references provider（引用查找）。

use std::collections::{BTreeMap, HashMap};

use super::parsed_doc_v3;
use super::parsed_doc_v3::{ident_at_offset, position_to_offset};
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;

pub fn references_v3(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
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
    // v0.104.6 D248：走唯一收口。**D245 漏了本文件与 `rename.rs`**。
    // 后果见 `rename.rs` 同处注释（负 position ⇒ 回命中文件末尾的标识符）。
    let (line, col) = parsed_doc_v3::pos_of(pos.get("line"), pos.get("character"));

    let (text, exprs) = match parsed_doc_v3::parsed_doc_v3(docs, uri) {
        Some(pair) => pair,
        None => return Value::Array(vec![]),
    };
    let offset = position_to_offset(&text, line, col);
    let ident = match ident_at_offset(&text, offset) {
        Some(s) => s,
        None => return Value::Array(vec![]),
    };

    let spans = parsed_doc_v3::collect_references_v3(&exprs, &ident);

    // v0.104.6 D103：本函数此前**完全不读** `params.context` ——
    // `includeDeclaration` 被静默忽略，而 `collect_references_v3` 只从**表达式**
    // 收集，`let x = 1` 的**声明处**（绑定，不是表达式）永远找不到。
    // 实测 `includeDeclaration: true` 时只返回使用处；而同场景的
    // `textDocument/rename` 走 `collect_definitions_v3` + `collect_references_v3`
    // 两个来源，**能找到两处** —— 两个 provider 行为不一致。
    //
    // 按 LSP 规范补上：`includeDeclaration`（默认 false）
    // 与 `onlyDeclaration`（只返回声明）。
    let ctx = params.get("context");
    let flag = |k: &str| match ctx.and_then(|c| c.get(k)) {
        Some(Value::Bool(b)) => *b,
        _ => false,
    };
    let include_declaration = flag("includeDeclaration");
    let only_declaration = flag("onlyDeclaration");

    let mut spans: Vec<crate::common::Span> = if only_declaration { Vec::new() } else { spans };
    if include_declaration || only_declaration {
        for (name, span) in parsed_doc_v3::collect_definitions_v3(&exprs) {
            if name == ident
                && !spans
                    .iter()
                    .any(|s| s.line == span.line && s.column == span.column)
            {
                spans.push(span);
            }
        }
    }
    let mut locations: Vec<Value> = Vec::new();
    // v0.104.6 D134：`span.column` 是 **1-based**（实测：源码 0-based char 16 的
    // `abc` 报 column 17），LSP 要求 0-based —— 直接透传**整体偏移 1 列**。
    // 实测 `let x = 1` / `let y = x + 1` 返回 `char 1-2` 与 `char 9-10`，
    // 而 `x` 实际在 `char 4` 与 `char 8`。
    //
    // 与 `definition` / `rename` 同一套：按 `ident` 的**完整词**双向就近查找。
    // 找不到就退回 `column - 1`（不精确但不会指到错误的符号）。
    let locate = |line: usize, column: usize| -> usize {
        let src_line = match text.lines().nth(line.saturating_sub(1)) {
            Some(l) => l,
            None => return column.saturating_sub(1),
        };
        let chars: Vec<char> = src_line.chars().collect();
        let name: Vec<char> = ident.chars().collect();
        if name.is_empty() || name.len() > chars.len() {
            return column.saturating_sub(1);
        }
        let from = column.min(chars.len());
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let hit = |i: usize| {
            chars[i..i + name.len()] == name[..]
                && !i
                    .checked_sub(1)
                    .and_then(|k| chars.get(k))
                    .is_some_and(|c| is_word(*c))
                && !chars.get(i + name.len()).is_some_and(|c| is_word(*c))
        };
        (from..=chars.len() - name.len())
            .find(|&i| hit(i))
            .or_else(|| (0..from).rev().find(|&i| hit(i)))
            .unwrap_or_else(|| column.saturating_sub(1))
    };
    for span in spans {
        let ident_col = locate(span.line, span.column);
        let ident_len = ident.chars().count();
        let mut m = BTreeMap::new();
        m.insert("uri".to_string(), Value::String_(uri.to_string()));
        m.insert(
            "range".to_string(),
            Value::Object({
                let mut r = BTreeMap::new();
                r.insert(
                    "start".to_string(),
                    Value::Object({
                        let mut s = BTreeMap::new();
                        // v0.104.6 D132：`Span::line` 是 1-based，LSP 要求 0-based。
                        // v0.104.6 D134：`Span::column` **也是 1-based**（实测确证，
                        // 修正 D132 里「column 已是 0-based」的误判），需经
                        // `locate` 校正为 0-based 的标识符位置。
                        s.insert(
                            "line".to_string(),
                            Value::Number(span.line.saturating_sub(1) as f64),
                        );
                        // v0.104.6 D211：出站 `character` 必须是 **UTF-16 码元**
                        s.insert(
                            "character".to_string(),
                            Value::Number(super::parsed_doc_v3::char_to_utf16_col(
                                text.lines().nth(span.line.saturating_sub(1)).unwrap_or(""),
                                ident_col,
                            ) as f64),
                        );
                        s
                    }),
                );
                r.insert(
                    "end".to_string(),
                    Value::Object({
                        let mut s = BTreeMap::new();
                        s.insert(
                            "line".to_string(),
                            Value::Number(span.line.saturating_sub(1) as f64),
                        );
                        s.insert(
                            "character".to_string(),
                            Value::Number(super::parsed_doc_v3::char_to_utf16_col(
                                text.lines().nth(span.line.saturating_sub(1)).unwrap_or(""),
                                ident_col + ident_len,
                            ) as f64),
                        );
                        s
                    }),
                );
                r
            }),
        );
        locations.push(Value::Object(m));
    }
    Value::Array(locations)
}
