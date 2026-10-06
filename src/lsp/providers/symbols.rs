//! v0.25: LSP document symbol provider（符号大纲）。

use std::collections::{BTreeMap, HashMap};

use super::parsed_doc_v3;
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;

pub fn document_symbol_v3(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
    let uri = match params
        .get("textDocument")
        .and_then(|t| t.get("uri"))
        .and_then(|u| u.as_str())
    {
        Some(s) => s,
        None => return Value::Array(vec![]),
    };
    let (text, exprs) = match parsed_doc_v3::parsed_doc_v3(docs, uri) {
        Some(pair) => pair,
        None => return Value::Array(vec![]),
    };

    let defs = parsed_doc_v3::collect_definitions_v3(&exprs);
    let mut symbols: Vec<Value> = Vec::new();
    for (name, span) in defs {
        let name_str = name.clone();
        // v0.104.6 D134：`span.column` 是 **1-based**（实测：源码 0-based char 16 的
        // `abc` 报 column 17），LSP 要求 0-based —— 直接透传**整体偏移 1 列**。
        // 实测 `let x = 1` 的 `documentSymbol` 返回 `char 1-2`（`let` 的 `e`），
        // 而 `x` 在 `char 4`。
        //
        // 与 definition / references / rename 同一套：按 `name` 的**完整词**双向
        // 就近查找；找不到退回 `column - 1`。
        let ident_col = {
            let chars: Vec<char> = text
                .lines()
                .nth(span.line.saturating_sub(1))
                .map(|l| l.chars().collect())
                .unwrap_or_default();
            let key: Vec<char> = name_str.chars().collect();
            if key.is_empty() || key.len() > chars.len() {
                span.column.saturating_sub(1)
            } else {
                let from = span.column.min(chars.len());
                let is_word = |c: char| c.is_alphanumeric() || c == '_';
                let hit = |i: usize| {
                    chars[i..i + key.len()] == key[..]
                        && !i
                            .checked_sub(1)
                            .and_then(|k| chars.get(k))
                            .is_some_and(|c| is_word(*c))
                        && !chars.get(i + key.len()).is_some_and(|c| is_word(*c))
                };
                (from..=chars.len() - key.len())
                    .find(|&i| hit(i))
                    .or_else(|| (0..from).rev().find(|&i| hit(i)))
                    .unwrap_or_else(|| span.column.saturating_sub(1))
            }
        };
        let name_len = name_str.chars().count();
        let mut m = BTreeMap::new();
        m.insert("name".to_string(), Value::String_(name_str.clone()));
        m.insert("kind".to_string(), Value::Number(13.0));
        m.insert(
            "location".to_string(),
            Value::Object({
                let mut loc = BTreeMap::new();
                loc.insert("uri".to_string(), Value::String_(uri.to_string()));
                loc.insert(
                    "range".to_string(),
                    Value::Object({
                        let mut r = BTreeMap::new();
                        r.insert(
                            "start".to_string(),
                            Value::Object({
                                let mut s = BTreeMap::new();
                                // v0.104.6 D132：`Span::line` 是 1-based，LSP 要求
                                // 0-based。v0.104.6 D134：`Span::column` **也是**
                                // 1-based（实测确证，修正 D132 的误判）——
                                // 正确性由上面的 `ident_col` 承担。
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
                                    // v0.104.6 D211：出站 `character` 必须是 **UTF-16 码元**
                                    Value::Number(super::parsed_doc_v3::char_to_utf16_col(
                                        text.lines().nth(span.line.saturating_sub(1)).unwrap_or(""),
                                        ident_col + name_len,
                                    ) as f64),
                                );
                                s
                            }),
                        );
                        r
                    }),
                );
                loc
            }),
        );
        symbols.push(Value::Object(m));
    }
    Value::Array(symbols)
}
