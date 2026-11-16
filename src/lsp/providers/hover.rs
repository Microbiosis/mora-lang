//! v0.25: LSP hover provider（悬停信息）。

use std::collections::{BTreeMap, HashMap};

use super::parsed_doc_v3;
use super::parsed_doc_v3::{format_hm_type, ident_with_col_at_offset, position_to_offset};
use crate::common::Span;
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;
use crate::mir::witness::{MirWitness, WitnessKind};

/// Extract an active explicit `: <Type>` annotation for `let <ident>: <Type>` up to `hover_line`.
fn explicit_annotation(text: &str, ident: &str, hover_line: usize) -> Option<String> {
    let mut latest: Option<String> = None;
    for (idx, line) in text.lines().enumerate() {
        if idx > hover_line && latest.is_some() {
            break;
        }
        let t = line.trim_start();
        let rest = match t.strip_prefix("let ") {
            Some(r) => r,
            None => continue,
        };
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
            None => {
                if idx <= hover_line {
                    latest = None;
                }
                continue;
            }
        };
        let ty: String = after_colon
            .chars()
            .take_while(|c| !matches!(c, '=' | ';' | '\r'))
            .collect();
        let ty = ty.trim().trim_end_matches('{').trim();
        if !ty.is_empty() && (idx <= hover_line || latest.is_none()) {
            latest = Some(ty.to_string());
        }
    }
    latest
}

fn classify_symbol_kind(exprs: &[MirWitness], ident: &str) -> &'static str {
    let mut is_fn = false;
    let mut is_let = false;
    for expr in exprs {
        parsed_doc_v3::walk_witness(expr, &mut |e| match &e.kind {
            WitnessKind::FnDef { name, .. } if name == ident => is_fn = true,
            WitnessKind::LetBinding { name, .. } if name == ident => is_let = true,
            _ => {}
        });
    }
    if is_fn {
        "fn"
    } else if is_let {
        "let"
    } else if crate::runtime::builtins::RUNTIME_BUILTIN_NAMES.contains(&ident) {
        "builtin"
    } else if ident == "random" || crate::flow::is_builtin_object(ident) {
        "module"
    } else {
        "variable"
    }
}

fn resolve_hover_type(
    text: &str,
    exprs: &[MirWitness],
    ident: &str,
    hover_line: usize,
    ident_col_1based: usize,
) -> String {
    if let Some(explicit) = explicit_annotation(text, ident, hover_line) {
        return explicit;
    }

    let info = crate::typeck::check_mir::infer_program_types(exprs);
    let target_line = hover_line + 1;

    // 1. Prefer binding declared on the hovered line.
    if let Some(ty_str) = info
        .bindings
        .iter()
        .rev()
        .filter(|(sp, name, _)| name == ident && sp.line == target_line)
        .find_map(|(_, _, ty)| format_hm_type(ty))
    {
        return ty_str;
    }

    // 2. Otherwise pick the nearest prior declaration in scope before the cursor line.
    if let Some(ty_str) = info
        .bindings
        .iter()
        .enumerate()
        .filter(|(_, (sp, name, _))| name == ident && sp.line <= target_line)
        .max_by_key(|(idx, (sp, _, _))| (sp.line, *idx))
        .and_then(|(_, (_, _, ty))| format_hm_type(ty))
    {
        return ty_str;
    }

    // 3. Fall back to any matching declaration (e.g. forward-referenced `fn`).
    if let Some(ty_str) = info
        .bindings
        .iter()
        .rev()
        .filter(|(_, name, _)| name == ident)
        .find_map(|(_, _, ty)| format_hm_type(ty))
    {
        return ty_str;
    }

    // 4. Expression-span match at the exact cursor identifier location.
    if let Some(ty_str) = info
        .span_types
        .get(&Span::new(target_line, ident_col_1based))
        .and_then(format_hm_type)
    {
        return ty_str;
    }

    // 5. Final top-level env lookup or builtin/module signature.
    if let Some(ty_str) = info.env_types.get(ident).and_then(format_hm_type) {
        return ty_str;
    }
    if let Some(ty_str) = crate::typeck::check_mir::lookup_builtin_or_module_type(ident)
        .as_ref()
        .and_then(format_hm_type)
    {
        return ty_str;
    }

    "<inferred>".to_string()
}

pub fn hover_v3(
    docs: &HashMap<String, DocumentState>,
    params: &Value,
) -> crate::error::MoraResult<Value> {
    let uri = params
        .get("textDocument")
        .and_then(|t| t.get("uri"))
        .and_then(|u| u.as_str())
        .ok_or_else(|| crate::error::MoraError::Runtime("missing textDocument.uri".to_string()))?;
    let pos = params
        .get("position")
        .ok_or_else(|| crate::error::MoraError::Runtime("missing position".to_string()))?;
    // v0.104.6 D245：走唯一收口（负数 `as usize` 会回绕成 `usize::MAX`，
    // 实测会静默返回**文件末尾**的标识符和 `line: 1.8e19` 的荒谬 range）。
    let (line, col) = parsed_doc_v3::pos_of(pos.get("line"), pos.get("character"));

    let (text, exprs) = parsed_doc_v3::parsed_doc_v3(docs, uri)
        .ok_or_else(|| crate::error::MoraError::Runtime("document not found".to_string()))?;
    let offset = position_to_offset(&text, line, col);
    let (ident, ident_col_1based) = match ident_with_col_at_offset(&text, offset) {
        Some(pair) => pair,
        None => return Ok(Value::Null),
    };

    let kind = classify_symbol_kind(&exprs, &ident);
    let ty_str = resolve_hover_type(&text, &exprs, &ident, line, ident_col_1based);
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
