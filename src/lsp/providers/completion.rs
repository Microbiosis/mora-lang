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

const STAGE3B_TYPES_AND_CTORS: &[(&str, &str)] = &[
    ("f64", "64-bit IEEE 754 float (alias float)"),
    ("f32", "32-bit IEEE 754 float"),
    ("f16", "16-bit IEEE 754 half float"),
    ("bf16", "16-bit Brain Float"),
    ("fp8", "8-bit OCP FP8 E4M3FN"),
    ("fp8_e4m3", "8-bit OCP FP8 E4M3FN"),
    ("fp8_e5m2", "8-bit OCP FP8 E5M2"),
    ("i64", "64-bit signed integer (alias int)"),
    ("i32", "32-bit signed integer"),
    ("i16", "16-bit signed integer"),
    ("i8", "8-bit signed integer"),
    ("i4", "4-bit signed quantized integer (-8..=7)"),
    ("u64", "64-bit unsigned integer"),
    ("u32", "32-bit unsigned integer"),
    ("u16", "16-bit unsigned integer"),
    ("u8", "8-bit unsigned integer"),
    (
        "Tensor",
        "Tensor<DType, Shape, Device> first-class strided tensor",
    ),
    (
        "Tensor::zeros",
        "Tensor::zeros(shape, dtype?, device?, requires_grad?)",
    ),
    (
        "Tensor::ones",
        "Tensor::ones(shape, dtype?, device?, requires_grad?)",
    ),
    (
        "Tensor::full",
        "Tensor::full(shape, value, dtype?, device?, requires_grad?)",
    ),
    (
        "Tensor::eye",
        "Tensor::eye(n, m?, dtype?, device?, requires_grad?)",
    ),
    (
        "Tensor::arange",
        "Tensor::arange(start, end?, step?, dtype?, device?, requires_grad?)",
    ),
    (
        "Tensor::randn",
        "Tensor::randn(shape, dtype?, device?, requires_grad?)",
    ),
    (
        "Tensor::rand",
        "Tensor::rand(shape, dtype?, device?, requires_grad?)",
    ),
    (
        "Tensor::from_list",
        "Tensor::from_list(data, dtype?, device?, requires_grad?)",
    ),
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

    for &(sym, detail) in STAGE3B_TYPES_AND_CTORS {
        if seen.insert(sym.to_string()) {
            items.push(make_completion(sym, 7.0, Some(detail)));
        }
    }

    for &kw in CANONICAL_KEYWORDS {
        if seen.insert(kw.to_string()) {
            items.push(make_completion(kw, 14.0, Some("keyword")));
        }
    }

    Value::Array(items)
}
