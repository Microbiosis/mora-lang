//! v0.55: Builtin dispatch table.
//!
//! This module is the single source of truth for built-in functions,
//! type-level operators (binary, comparison), and method-dispatch
//! tables. The v0.55 Hindley-Milner engine in [`crate::typeck::hm`]
//! consumes the `Signatures` so a `Router::new()` registered here is
//! recognized by the checker.
//!
//! v0.75.47: 移除对已删除 v2 `TypeChecker` 的引用（v0.55 去 AST 化时
//! 删除，注释残留；此处不再引该类型以免 rustdoc broken link）。
//!
//! It is also where `method_return_type` lives, which used to live in
//! `typeck::mod` and was the source of a back-reference from
//! `typeck::hm`. Moving it here keeps the dependency direction clean:
//! `typeck` depends on `typeck::dispatch`, never the other way around.

use crate::typeck::Type;

///  Function signature used by the v0.55 builtin registry.
#[derive(Debug, Clone, PartialEq)]
pub struct Signature {
    /// v0.x parameter list as `(name, type)` pairs.
    pub params: Vec<(String, Type)>,
    /// v0.10 raw hint strings (e.g. "T" → number).
    pub raw_params: Vec<Option<String>>,
    pub return_type: Type,
    /// v0.10 raw return hint.
    pub raw_return_type: Option<String>,
}

impl Signature {
    /// Build a signature with no raw-hint metadata (used by HM).
    pub fn new(params: Vec<(String, Type)>, return_type: Type) -> Self {
        let raw_params = params.iter().map(|_| None).collect();
        Self {
            params,
            raw_params,
            return_type,
            raw_return_type: None,
        }
    }
}

///  Return the canonical builtin registry. This is a flat snapshot;
///  the underlying collection is intentionally immutable from the
///  outside so both checkers can read it without contention.
pub fn builtin_signatures() -> Vec<(String, Signature)> {
    vec![
        // v0.75.23: M 原语 merge_with(key, strategy) — 声明 per-key CRDT
        // 合并策略（append/add/dict_union/grow_only_set/lww），作用于其后的
        // worker/transaction/observe 块合并。
        (
            "merge_with".to_string(),
            Signature::new(
                vec![
                    ("key".to_string(), Type::String),
                    ("strategy".to_string(), Type::String),
                ],
                Type::Nil,
            ),
        ),
        // v0.13: print(x) accepts any printable primitive and returns nil.
        (
            "print".to_string(),
            Signature {
                params: vec![(
                    "x".to_string(),
                    Type::Union(vec![
                        Type::String,
                        Type::Float,
                        Type::Bool,
                        Type::Char,
                        Type::Nil,
                        Type::List(Box::new(Type::Union(vec![]))),
                        Type::Dict(Box::new(Type::Union(vec![])), Box::new(Type::Union(vec![]))),
                    ]),
                )],
                raw_params: vec![None],
                return_type: Type::Nil,
                raw_return_type: None,
            },
        ),
        // range(start, end, step) -> list<number>
        (
            "range".to_string(),
            Signature::new(
                vec![
                    ("start".to_string(), Type::Float),
                    ("end".to_string(), Type::Float),
                    ("step".to_string(), Type::Float),
                ],
                Type::List(Box::new(Type::Float)),
            ),
        ),
        // len(x) for string / list / dict
        (
            "len".to_string(),
            Signature::new(
                vec![(
                    "x".to_string(),
                    Type::Union(vec![
                        Type::String,
                        Type::List(Box::new(Type::Union(vec![]))),
                        Type::Dict(Box::new(Type::Union(vec![])), Box::new(Type::Union(vec![]))),
                    ]),
                )],
                Type::Float,
            ),
        ),
        // str(x) -> string
        (
            "str".to_string(),
            Signature::new(vec![("x".to_string(), Type::Any)], Type::String),
        ),
        // int(s) -> int
        (
            "int".to_string(),
            Signature::new(vec![("s".to_string(), Type::String)], Type::Int),
        ),
        // float(x) -> float
        (
            "float".to_string(),
            Signature::new(vec![("x".to_string(), Type::Any)], Type::Float),
        ),
        // bool(x) -> bool
        (
            "bool".to_string(),
            Signature::new(vec![("x".to_string(), Type::Any)], Type::Bool),
        ),
        // v0.06: ai.chat(cfg: AiConfig, prompt: String) -> AiResult
        (
            "ai.chat".to_string(),
            Signature::new(
                vec![
                    ("cfg".to_string(), Type::AiConfig),
                    ("prompt".to_string(), Type::String),
                ],
                Type::AiResult,
            ),
        ),
        // v0.06.3: Router::new() -> Router
        (
            "Router::new".to_string(),
            Signature::new(vec![], Type::Router),
        ),
        // v0.06.6: McpServer::new() -> McpServer
        (
            "McpServer::new".to_string(),
            Signature::new(vec![], Type::McpServer),
        ),
    ]
}

///  Look up a builtin by name. Returns `None` if the name is not a
///  registered builtin.
pub fn lookup_builtin(name: &str) -> Option<Signature> {
    builtin_signatures()
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, s)| s)
}

///  Number of declared parameters for a known receiver method, used by
///  `infer_method_call` to enforce arity.
///  Returns `None` if the method is unknown to the dispatch table.
pub fn method_arity(receiver: &Type, method: &str) -> Option<usize> {
    if let Some(sig) = method_signature(receiver, method) {
        Some(sig.params.len())
    } else {
        None
    }
}

///  Look up a method's `Signature` (parameter list + return type) for
///  `receiver`. Returns `None` for unknown `(receiver, method)` pairs.
pub fn method_signature(receiver: &Type, method: &str) -> Option<Signature> {
    if let Some(sig) = method_signature_builtin(receiver, method) {
        return Some(sig);
    }
    if let Some(sig) = method_signature_via_type(receiver, method) {
        return Some(sig);
    }
    if method == "len" {
        return Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::Float,
        ));
    }
    None
}

fn method_signature_builtin(receiver: &Type, method: &str) -> Option<Signature> {
    match (receiver, method) {
        // v0.75.16: 保留 List 元素类型（此前 map/filter/push 返回 List<Any>、
        // reduce/pop/get 返回 Any — 元素类型信息丢失）。
        // v0.75.16: 保留 List 元素类型。map/filter 接收闭包参数（M2 前
        // 用 Any 宽松约束）；push 接收元素参数（elem 类型）。
        (Type::List(elem), "map" | "filter") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("f".to_string(), Type::Any),
            ],
            Type::List(elem.clone()),
        )),
        (Type::List(elem), "push") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("value".to_string(), elem.as_ref().clone()),
            ],
            Type::List(elem.clone()),
        )),
        (Type::List(elem), "pop") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            (**elem).clone(),
        )),
        // get 接收 index（Int）
        (Type::List(elem), "get") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("index".to_string(), Type::Int),
            ],
            (**elem).clone(),
        )),
        (Type::List(_), "len") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::Float,
        )),
        // get 接收 key（String）；返回 Union<V, Nil>（v0.75.16 unify 成员合一）
        (Type::Dict(_, v), "get") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("key".to_string(), Type::String),
            ],
            Type::Union(vec![v.as_ref().clone(), Type::Nil]),
        )),
        // set 接收 key + value
        (Type::Dict(k, v), "set") => Some(Signature::new(
            vec![
                ("self".to_string(), receiver.clone()),
                ("key".to_string(), k.as_ref().clone()),
                ("value".to_string(), v.as_ref().clone()),
            ],
            Type::Dict(k.clone(), v.clone()),
        )),
        (Type::Dict(k, _), "keys") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::List(Box::new(k.as_ref().clone())),
        )),
        (Type::Dict(_, v), "values") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::List(Box::new(v.as_ref().clone())),
        )),
        (Type::Dict(_, _), "len") => Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            Type::Float,
        )),
        (Type::String, "len") => Some(Signature::new(
            vec![("self".to_string(), Type::String)],
            Type::Float,
        )),
        (Type::String, "upper" | "lower" | "trim" | "replace") => Some(Signature::new(
            vec![("self".to_string(), Type::String)],
            Type::String,
        )),
        (Type::String, "starts_with" | "ends_with" | "contains") => Some(Signature::new(
            vec![("self".to_string(), Type::String)],
            Type::Bool,
        )),
        (Type::String, "split") => Some(Signature::new(
            vec![("self".to_string(), Type::String)],
            Type::List(Box::new(Type::String)),
        )),
        (Type::Conversation, "chat") => Some(Signature::new(
            vec![("self".to_string(), Type::Conversation)],
            Type::Any,
        )),
        (Type::Conversation, "history" | "len") => Some(Signature::new(
            vec![("self".to_string(), Type::Conversation)],
            Type::List(Box::new(Type::Any)),
        )),
        (Type::Conversation, "model") => Some(Signature::new(
            vec![("self".to_string(), Type::Conversation)],
            Type::String,
        )),
        (Type::AiModule, "chat") => Some(Signature::new(
            vec![("self".to_string(), Type::AiModule)],
            Type::AiResult,
        )),
        (Type::AiConfig, "model" | "temperature" | "max_tokens" | "system" | "budget") => Some(
            Signature::new(vec![("self".to_string(), Type::AiConfig)], Type::AiConfig),
        ),
        (Type::Router, "route") => Some(Signature::new(
            vec![("self".to_string(), Type::Router)],
            Type::Router,
        )),
        (Type::Router, "listen") => Some(Signature::new(
            vec![("self".to_string(), Type::Router)],
            Type::Nil,
        )),
        (Type::McpServer, "tool") => Some(Signature::new(
            vec![("self".to_string(), Type::McpServer)],
            Type::McpServer,
        )),
        (Type::McpServer, "serve") => Some(Signature::new(
            vec![("self".to_string(), Type::McpServer)],
            Type::Nil,
        )),
        (Type::HttpRequest, "json") => Some(Signature::new(
            vec![("self".to_string(), Type::HttpRequest)],
            Type::Any,
        )),
        _ => None,
    }
}

fn method_signature_via_type(receiver: &Type, method: &str) -> Option<Signature> {
    let ret = method_return_type(receiver, method);
    if matches!(ret, Type::Any) {
        None
    } else {
        Some(Signature::new(
            vec![("self".to_string(), receiver.clone())],
            ret,
        ))
    }
}

///  Return the result type of a method call. Mirrors the v0.x dispatch
///  table. Returns `Type::Any` for unknown combinations so callers can
///  fall back gracefully.
pub fn method_return_type(receiver: &Type, method: &str) -> Type {
    if let Some(sig) = method_signature_builtin(receiver, method) {
        return sig.return_type;
    }
    method_return_type_fallback(receiver, method)
}

fn method_return_type_fallback(receiver: &Type, method: &str) -> Type {
    if let Type::Union(_) = receiver {
        return Type::Any;
    }
    if method == "len" {
        return Type::Float;
    }
    Type::Any
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_signature_is_known() {
        let sig = lookup_builtin("print").expect("print should be registered");
        assert_eq!(sig.params.len(), 1);
        assert!(matches!(sig.return_type, Type::Nil));
    }

    #[test]
    fn router_new_registered() {
        let sig = lookup_builtin("Router::new").expect("Router::new registered");
        assert_eq!(sig.params.len(), 0);
        assert!(matches!(sig.return_type, Type::Router));
    }

    #[test]
    fn mcp_server_new_registered() {
        let sig = lookup_builtin("McpServer::new").expect("McpServer::new registered");
        assert_eq!(sig.params.len(), 0);
        assert!(matches!(sig.return_type, Type::McpServer));
    }

    #[test]
    fn route_method_on_router() {
        let sig = method_signature(&Type::Router, "route").expect("Router.route");
        assert_eq!(sig.params.len(), 1); // self
        assert!(matches!(sig.return_type, Type::Router));
    }

    #[test]
    fn list_map_arity_is_two() {
        // v0.75.16: map 接收闭包参数（receiver + f = 2 参）。
        assert_eq!(
            method_arity(&Type::List(Box::new(Type::Any)), "map"),
            Some(2)
        );
    }

    #[test]
    fn unknown_method_returns_none() {
        assert!(method_signature(&Type::String, "no_such_method").is_none());
    }
}
