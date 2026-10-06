//! v0.25: LSP definition provider（跳转定义）。

use std::collections::{BTreeMap, HashMap};

use super::parsed_doc_v3;
use super::parsed_doc_v3::{ident_at_offset, position_to_offset};
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;

pub fn definition_v3(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
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
    // v0.104.6 D245：走唯一收口（同 `hover_v3`）。负 `line` / `character`
    // 经 `as usize` 回绕会静默命中**文件末尾**的标识符。
    let (line, col) = super::parsed_doc_v3::pos_of(pos.get("line"), pos.get("character"));

    let (text, exprs) = match parsed_doc_v3::parsed_doc_v3(docs, uri) {
        Some(pair) => pair,
        None => return Value::Array(vec![]),
    };
    let offset = position_to_offset(&text, line, col);
    let ident = match ident_at_offset(&text, offset) {
        Some(s) => s,
        None => return Value::Array(vec![]),
    };

    let defs = parsed_doc_v3::collect_definitions_v3(&exprs);
    // v0.104.6 D132：`span.column` 指向**整个 let 语句 / 表达式**的起点
    // （`let x = 1` 落在 `l`，column 0），不是标识符位置。直接透传会让
    // 「跳转到定义」停在语句开头而不是符号上（实测跳到 char 1-2 = `let` 的 `e`，
    // 而 `x` 在 char 4）。
    //
    // 与 `rename_v3` 的 `locate` 同一套：按 `ident` 的**完整词**双向就近查找。
    // 找不到就退回 span 起点（跳转位置不精确，但不会指向错误的符号）。
    let locate = |line: usize, column: usize| -> usize {
        let src_line = match text.lines().nth(line.saturating_sub(1)) {
            Some(l) => l,
            None => return column,
        };
        let chars: Vec<char> = src_line.chars().collect();
        let name: Vec<char> = ident.chars().collect();
        if name.is_empty() || name.len() > chars.len() {
            return column;
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
            .unwrap_or(column)
    };

    let mut locations: Vec<Value> = Vec::new();
    for (name, span) in defs {
        if name == ident {
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
                            s.insert(
                                "line".to_string(),
                                // v0.104.6 D132：`Span::line` 是 **1-based**（源码第 N
                                // 行记作 N），而 LSP 规范要求 **0-based**。此前直接
                                // 透传 → 「跳转到定义」整体偏移一行，编辑器把光标
                                // 落到**下一行**。`server.rs` 的诊断路径一直有
                                // `saturating_sub(1)`，这三个 provider 漏了。
                                //
                                // v0.104.6 D134 修正本行的旧判读：`Span::column`
                                // **也是 1-based**（实测：源码 0-based char 16 的
                                // `abc` 报 column 17），此处说「column 已是 0-based」
                                // 是错的 —— 正确性由上面的 `locate` 承担（它在
                                // 0-based 的 `chars` 数组上查找，结果天然 0-based）。
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
    }
    Value::Array(locations)
}
