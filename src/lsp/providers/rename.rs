//! v0.25: LSP rename provider（重命名）。

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::parsed_doc_v3;
use super::parsed_doc_v3::{ident_at_offset, position_to_offset};
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;

pub fn rename_v3(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
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
    // v0.104.6 D248：走唯一收口。**D245 漏了本文件与 `references.rs`**。
    //
    // 本处的后果比 hover/definition 严重一个量级 —— **rename 会写文件**：
    // 负 position 回绕成 `usize::MAX` → offset 落到 `text.len()` → 若文件
    // 末尾是标识符就命中它 → 返回一份**重命名那个标识符**的 WorkspaceEdit。
    // 真实探针（文档 `let alpha: Int = 1\nlet beta: Int = alpha\nbeta`）：
    //   position (1,17) 在 alpha 上 → 改 alpha（正确）
    //   position (-1, 0)         → **改 beta 两处**（0:4-9 之外的 1:4-8 / 2:0-4）
    // 客户端照此 apply ⇒ **改坏用户文件**。
    let (line, col) = parsed_doc_v3::pos_of(pos.get("line"), pos.get("character"));

    let (text, exprs) = match parsed_doc_v3::parsed_doc_v3(docs, uri) {
        Some(pair) => pair,
        None => return Value::Null,
    };
    let offset = position_to_offset(&text, line, col);
    let old_name = match ident_at_offset(&text, offset) {
        Some(s) => s,
        None => return Value::Null,
    };

    let defs = parsed_doc_v3::collect_definitions_v3(&exprs);
    let refs = parsed_doc_v3::collect_references_v3(&exprs, &old_name);

    // v0.104.6 D132：**必须把 span 的起点校正到标识符本身**。
    //
    // `collect_definitions_v3` / `collect_references_v3` 返回的 span 是
    // **整个 let 语句 / 表达式**的 span（`let x = 1` 起点在 `l`，column 0），
    // 而本函数用 `column + name.len()` 算 end —— 于是把 `let` 的 `e`（char 1）
    // 当成了标识符。实测（真实 LSP 会话 + 真的把 edits 应用到源码）：
    //
    //   `let x = 1` / `let y = x + 1`  上 rename x→z  ⇒
    //     `lzt x = 1` / `let y = xz+ 1`   → `mora --check` 报 3 个类型错误
    //
    // 即**按一次 F2 就把代码改坏了**。改为：从 `span.column` 起在该行**向后
    // 找 `old_name` 的完整词**（前后不是标识符字符），取第一个匹配。
    let locate = |line: usize, column: usize| -> Option<(usize, usize)> {
        let src_line = text.lines().nth(line.checked_sub(1)?)?;
        let chars: Vec<char> = src_line.chars().collect();
        let name: Vec<char> = old_name.chars().collect();
        if name.is_empty() || name.len() > chars.len() {
            return None;
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
        // 双向就近查找：`span` 起点在不同构造里落点不同 ——
        // `let x = 1` 的 span 起点在 `l`（标识符**之前**）需向后找；
        // 某些引用构造的 span 起点落在标识符**之后**（此时单向向后找不到，
        // 实测漏改 `let y = x + 1` 里的那个 `x`）。两头都找不到就**放弃这一处**
        // —— 宁可不改，也不要改错字符。
        (from..=chars.len() - name.len())
            .find(|&i| hit(i))
            .or_else(|| (0..from).rev().find(|&i| hit(i)))
            .map(|i| (line, i))
    };

    let mut edits: BTreeSet<(usize, usize)> = BTreeSet::new();
    for (name, span) in &defs {
        if name == &old_name
            && let Some(pos) = locate(span.line, span.column)
        {
            edits.insert(pos);
        }
    }
    for span in &refs {
        if let Some(pos) = locate(span.line, span.column) {
            edits.insert(pos);
        }
    }

    let mut edit_list: Vec<Value> = Vec::new();
    for (l, c) in &edits {
        // v0.104.6 D211：`character` 必须是 **UTF-16 码元**。
        //
        // 本函数是**唯一会改写用户文件**的 provider：客户端照 `range` 应用
        // `newText`，列号错一位就改坏文件。真实会话实测（源码行
        // `print("😀", total)`，把 `total` 重命名为 `sum`）：
        //
        // ```text
        // 修前发出 range = 行 1 的 11..16（**char** 索引）
        // 客户端按 UTF-16 应用 → print("😀",suml)    ← 期望 print("😀", sum)
        // ```
        //
        // 即 `11..16` 在 UTF-16 下落在 `' '` + `tota` 上。
        let src_line = text.lines().nth(l.saturating_sub(1)).unwrap_or("");
        let start_u16 = parsed_doc_v3::char_to_utf16_col(src_line, *c);
        // 结束列 = 起点 + 标识符的**字符数**
        let end_u16 = start_u16 + old_name.chars().count();
        let mut m = BTreeMap::new();
        m.insert(
            "range".to_string(),
            Value::Object({
                let mut r = BTreeMap::new();
                r.insert(
                    "start".to_string(),
                    Value::Object({
                        let mut p = BTreeMap::new();
                        // v0.104.6 D132：`Span::line` 是 1-based，LSP 要求 0-based
                        // （同 definition.rs）。`column` 已是 0-based。
                        p.insert(
                            "line".to_string(),
                            Value::Number(l.saturating_sub(1) as f64),
                        );
                        p.insert("character".to_string(), Value::Number(start_u16 as f64));
                        p
                    }),
                );
                r.insert(
                    "end".to_string(),
                    Value::Object({
                        let mut p = BTreeMap::new();
                        p.insert(
                            "line".to_string(),
                            Value::Number(l.saturating_sub(1) as f64),
                        );
                        p.insert("character".to_string(), Value::Number(end_u16 as f64));
                        p
                    }),
                );
                r
            }),
        );
        m.insert("newText".to_string(), Value::String_(new_name.clone()));
        edit_list.push(Value::Object(m));
    }

    let mut changes = BTreeMap::new();
    changes.insert(uri.to_string(), Value::Array(edit_list));

    let mut result = BTreeMap::new();
    result.insert("changes".to_string(), Value::Object(changes));
    Value::Object(result)
}
