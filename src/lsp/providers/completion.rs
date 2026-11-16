//! v0.25: LSP completion provider（补全建议）。

use std::collections::{BTreeSet, HashMap};

use super::parsed_doc_v3;
use super::parsed_doc_v3::{format_hm_type, make_completion};
use crate::lsp::json::Value;
use crate::lsp::server::DocumentState;

const CANONICAL_KEYWORDS: &[&str] = &[
    "let",
    "task",
    "if",
    "then",
    "else",
    "end",
    "for",
    "in",
    "return",
    "fn",
    "true",
    "false",
    "nil",
    "match",
    "with",
    "import",
    "export",
    "parallel",
    "break",
    "continue",
    "do",
    "while",
    "loop",
    "as",
    "macro",
    "quote",
    "dyn",
    "self",
    "type",
    "enum",
    "struct",
    "effect",
    "perform",
    "handle",
    "spawn",
    "select",
    "case",
    "default",
    "timeout",
    "when",
    "assign",
    "transaction",
    "worker",
    "aggregate",
    "commit",
    "rollback",
];

pub fn completion_v3(docs: &HashMap<String, DocumentState>, params: &Value) -> Value {
    let uri = match params
        .get("textDocument")
        .and_then(|t| t.get("uri"))
        .and_then(|u| u.as_str())
    {
        Some(s) => s,
        None => return Value::Array(vec![]),
    };

    if !docs.contains_key(uri) {
        return Value::Array(vec![]);
    }

    let mut items: Vec<Value> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    if let Some((_text, exprs)) = parsed_doc_v3::parsed_doc_v3(docs, uri) {
        let info = crate::typeck::check_mir::infer_program_types(&exprs);
        for (name, def_span) in parsed_doc_v3::collect_definitions_v3(&exprs) {
            if seen.insert(name.clone()) {
                let detail = info
                    .bindings
                    .iter()
                    .find(|(sp, b_name, _)| *sp == def_span && b_name == &name)
                    .and_then(|(_, _, ty)| format_hm_type(ty))
                    .or_else(|| info.env_types.get(&name).and_then(format_hm_type));
                items.push(make_completion(
                    &name,
                    6.0,
                    Some(detail.as_deref().unwrap_or("variable")),
                ));
            }
        }
    }

    for &builtin in crate::runtime::builtins::RUNTIME_BUILTIN_NAMES {
        if seen.insert(builtin.to_string()) {
            let detail = crate::typeck::check_mir::lookup_builtin_or_module_type(builtin)
                .as_ref()
                .and_then(format_hm_type);
            items.push(make_completion(
                builtin,
                10.0,
                Some(detail.as_deref().unwrap_or("builtin")),
            ));
        }
    }

    for &(module_name, _) in crate::value::MODULE_OBJECTS {
        if seen.insert(module_name.to_string()) {
            items.push(make_completion(module_name, 9.0, Some("module")));
        }
    }

    for &kw in CANONICAL_KEYWORDS {
        if seen.insert(kw.to_string()) {
            items.push(make_completion(kw, 14.0, Some("keyword")));
        }
    }

    Value::Array(items)
}
