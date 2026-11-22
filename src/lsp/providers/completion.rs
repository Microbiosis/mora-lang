//! v0.110.0: LSP completion provider —— 关键字 / 内建 / 模块 / 用户符号的合并补全。
//!
//! 数据来源（按插入顺序，后者不覆盖前者）：
//! - 文档内的 `let` / `fn` 定义（经 `MirWitness` 提取，类型由
//!   `typeck::check_mir::infer_program_types` 推断）—— `CompletionItemKind` 6（Variable）；
//! - `runtime::builtins::RUNTIME_BUILTIN_NAMES` —— 10（Property）；
//! - `value::MODULE_OBJECTS` —— 9（Module）；
//! - `STAGE3B_TYPES_AND_CTORS`（Stage 3B 数值类型与 `Tensor` 构造子）—— 7（Class）；
//! - `CANONICAL_KEYWORDS` —— 14（Keyword）。
//!
//! 所有条目按名称去重，先到先得。

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
    "trait",
    "impl",
    "no_grad",
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
        "tcp_listener",
        "Non-blocking async TCP listener type (tcp_listen)",
    ),
    (
        "tcp_stream",
        "Non-blocking async TCP stream type (tcp_connect / accept)",
    ),
    (
        "tls_listener",
        "Native async TLS 1.2/1.3 listener type (tls_listen)",
    ),
    (
        "tls_stream",
        "Native async TLS 1.2/1.3 stream type (tls_connect / accept)",
    ),
    (
        "compute_dag",
        "Fused TensorComputeDag compute graph type (trace_dag)",
    ),
    (
        "kv_allocator",
        "Paged KV-cache dynamic resource allocator type (kv_allocator)",
    ),
    (
        "memory_pool",
        "64-byte aligned size-classed tensor memory pool type (memory_pool)",
    ),
    (
        "process_group",
        "Distributed tensor collective group type (process_group / tcp_process_group)",
    ),
    (
        "trace_profiler",
        "Nanosecond compute & service trace profiler handle type (profiler / std/trace)",
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
    (
        "Tensor::from_bytes",
        "Tensor::from_bytes(bytes, shape, dtype, device?, requires_grad?) -> Tensor from raw little-endian u8 bytes",
    ),
    (
        "Tensor::from_utf8",
        "Tensor::from_utf8(text) -> Tensor<u8, [N], cpu> UTF-8 byte buffer",
    ),
    (
        "Tensor::cat",
        "Tensor::cat(tensors, axis?) -> concatenate tensors along axis",
    ),
    (
        "Tensor::devices",
        "Tensor::devices() -> list<string> of available execution devices (cpu, cuda:N, vulkan:N, metal:N)",
    ),
    (
        "Tensor::device_info",
        "Tensor::device_info(device?) -> dict<string, string> hardware telemetry",
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
