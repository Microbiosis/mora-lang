//! v0.25: LSP hover provider（悬停信息）。

use std::collections::{BTreeMap, HashMap};

use super::parsed_doc_v3;
use super::parsed_doc_v3::{ident_at_offset, position_to_offset};
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;

/// 在文档里找 `let <ident>: <Type>` 的**显式**类型标注。
///
/// v0.104.6 D193。只认源码里**确实写了**的标注 —— 找不到就返 `None`，
/// 由调用方回落到 `<inferred>`。**不猜、不推断**：猜出来的类型若与运行期
/// 不符，比 `<inferred>` 更糟（它看起来可信）。
///
/// 匹配规则保守：必须是行首（可含缩进）的 `let`，名字**整词**匹配，
/// 标注取 `:` 之后到 `=` / 行尾 / `;` 之前的部分。
fn explicit_annotation(text: &str, ident: &str) -> Option<String> {
    for line in text.lines() {
        let t = line.trim_start();
        // ⚠ 这里必须 `continue` 而不是 `?` —— 文档里第一行未必是 `let`，
        // 用 `?` 会让整个函数在那行就返回。
        let rest = match t.strip_prefix("let ") {
            Some(r) => r,
            None => continue,
        };
        // 名字必须匹配且**整词**结束（`let ax` 不能匹配 `a`）。
        let rest = match rest.strip_prefix(ident) {
            Some(r) => r,
            None => continue,
        };
        let rest = match rest.chars().next() {
            Some(c) if c.is_alphanumeric() || c == '_' => continue,
            _ => rest,
        };
        let after_colon = match rest.trim_start().strip_prefix(':') {
            Some(r) => r.trim_start(),
            None => continue, // 没标注 → 不是我们要的
        };
        // 到 `=` / `;` / 行尾为止
        let ty: String = after_colon
            .chars()
            .take_while(|c| !matches!(c, '=' | ';' | '\r'))
            .collect();
        let ty = ty.trim().trim_end_matches('{').trim();
        if !ty.is_empty() {
            return Some(ty.to_string());
        }
    }
    None
}

pub fn hover_v3(docs: &HashMap<String, DocumentState>, params: &Value) -> Result<Value, String> {
    let uri = params
        .get("textDocument")
        .and_then(|t| t.get("uri"))
        .and_then(|u| u.as_str())
        .ok_or("missing textDocument.uri")?;
    let pos = params.get("position").ok_or("missing position")?;
    // v0.104.6 D245：走唯一收口（负数 `as usize` 会回绕成 `usize::MAX`，
    // 实测会静默返回**文件末尾**的标识符和 `line: 1.8e19` 的荒谬 range）。
    let (line, col) = parsed_doc_v3::pos_of(pos.get("line"), pos.get("character"));

    let (text, exprs) = parsed_doc_v3::parsed_doc_v3(docs, uri).ok_or("document not found")?;
    let offset = position_to_offset(&text, line, col);
    let ident = match ident_at_offset(&text, offset) {
        Some(s) => s,
        None => return Ok(Value::Null),
    };

    let defs = parsed_doc_v3::collect_definitions_v3(&exprs);
    let kind = if defs.iter().any(|(n, _)| n == &ident) {
        "let"
    } else if ["print", "len", "range"].contains(&ident.as_str()) {
        "builtin"
    } else {
        "variable"
    };
    // v0.104.6 D193：**显式标注**时把标注报出来，别再一律 `<inferred>`。
    //
    // 修前 hover 对**每一个**符号都硬编码 `<inferred>`，信息量为零 ——
    // 连 `let a: Int = 5` 这种**源码里就写着 `Int`** 的都报 `<inferred>`：
    //
    // ```text
    // let a: Int = 5   →  let a: <inferred>     ← 标注就在眼前
    // let b = 7.5      →  let b: <inferred>
    // math.floor       →  variable floor: <inferred>
    // ```
    //
    // 只报**源码里确实写了**的类型 —— 不做推断、不猜。推断是 HM 层的事
    // （`typeck::dispatch` 有完整的 `*_METHODS` 签名表），把它接进 LSP
    // 是更大的工程，不在本轮擅自做。
    let ty = explicit_annotation(&text, &ident);
    let ty_str = ty.unwrap_or_else(|| "<inferred>".to_string());
    let contents = format!("```mora\n{} {}: {}\n```", kind, ident, ty_str);

    let mut m = BTreeMap::new();
    m.insert(
        "contents".to_string(),
        Value::Object({
            let mut inner = BTreeMap::new();
            inner.insert("kind".to_string(), Value::String_("markdown".to_string()));
            inner.insert("value".to_string(), Value::String_(contents));
            inner
        }),
    );
    // v0.104.6 D211：`col` 是客户端发来的 **UTF-16 码元**。`offset` 已经过
    // `position_to_offset` 正确换算，但**算 range 时**必须先把 `col` 也换算成
    // char 索引，否则范围整体偏移（修前实测发 7..13，正确的 `total` 是 12..17）。
    let src_line = text.lines().nth(line).unwrap_or("");
    let col_char = parsed_doc_v3::utf16_to_char_col(src_line, col);
    let col_u16 = parsed_doc_v3::char_to_utf16_col(src_line, col_char);
    let start_u16 =
        parsed_doc_v3::char_to_utf16_col(src_line, col_char.saturating_sub(ident.chars().count()));
    m.insert(
        "range".to_string(),
        Value::Object({
            let mut r = BTreeMap::new();
            r.insert(
                "start".to_string(),
                Value::Object({
                    let mut s = BTreeMap::new();
                    s.insert("line".to_string(), Value::Number(line as f64));
                    s.insert("character".to_string(), Value::Number(start_u16 as f64));
                    s
                }),
            );
            r.insert(
                "end".to_string(),
                Value::Object({
                    let mut s = BTreeMap::new();
                    s.insert("line".to_string(), Value::Number(line as f64));
                    s.insert("character".to_string(), Value::Number(col_u16 as f64));
                    s
                }),
            );
            r
        }),
    );
    Ok(Value::Object(m))
}
