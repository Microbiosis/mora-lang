//! Trait 分发机制
//!
//! 从 interpreter/mod.rs 提取的 trait 相关方法：
//! - dispatch_trait_method: dyn dispatch，沿 trait 继承链找 impl 调用
//! - construct_trait_instance: Trait::new("ForType") 构造 trait instance

use std::collections::HashMap;

use crate::common::Span;
use crate::common::trait_info::{TraitInfo, default_impl_method_key, impl_method_key};
use crate::error::{MoraError, MoraResult};
use crate::value::Value;

/// 收集 trait 继承链上所有必需方法名
pub(crate) fn collect_required_methods<'a>(
    registry: &'a std::collections::HashMap<String, TraitInfo>,
    trait_name: &str,
) -> Vec<&'a str> {
    let mut methods = Vec::new();
    if let Some(info) = registry.get(trait_name) {
        for m in &info.methods {
            methods.push(m.name.as_str());
        }
        for parent in &info.parents {
            for m in collect_required_methods(registry, parent) {
                if !methods.contains(&m) {
                    methods.push(m);
                }
            }
        }
    }
    methods
}

/// 沿 trait 继承链 BFS 收集所有候选 trait 名
pub(crate) fn collect_parent_traits(
    registry: &std::collections::HashMap<String, TraitInfo>,
    trait_name: &str,
) -> Vec<String> {
    let mut result = vec![trait_name.to_string()];
    let mut visited = std::collections::HashSet::new();
    visited.insert(trait_name.to_string());
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(trait_name.to_string());
    while let Some(current) = queue.pop_front() {
        if let Some(info) = registry.get(&current) {
            for parent in &info.parents {
                if visited.insert(parent.clone()) {
                    result.push(parent.clone());
                    queue.push_back(parent.clone());
                }
            }
        }
    }
    result
}

/// Result of resolving a trait method dispatch: either an immediate `Value` (`Nil`) or `(callee, args)`.
pub(crate) enum TraitMethodTarget {
    Immediate(Value),
    Call(Value, Vec<Value>),
}

/// Pure lookup for `TraitObject.method(args...)` without performing the call itself.
pub(crate) fn resolve_trait_method_call(
    core: &crate::runtime::CoreRuntime,
    receiver: &Value,
    method: &str,
    args: Vec<Value>,
    call_site: Span,
) -> MoraResult<TraitMethodTarget> {
    let (for_type, for_generics, trait_name, trait_generics) = match receiver {
        Value::TraitObject {
            for_type,
            for_generics,
            trait_name,
            trait_generics,
            ..
        } => (
            for_type.clone(),
            for_generics.clone(),
            trait_name.clone(),
            trait_generics.clone(),
        ),
        Value::Nil => return Ok(TraitMethodTarget::Immediate(Value::Nil)),
        _ => {
            return Err(MoraError::Runtime(format!(
                "trait dispatch at line {}: receiver must be trait object or nil, got {:?}",
                call_site.line, receiver
            )));
        }
    };

    let search_chain = collect_parent_traits(&core.trait_registry, &trait_name);
    for tname in &search_chain {
        let tname_str: &str = tname;
        let has_self = core
            .trait_registry
            .get(tname_str)
            .and_then(|info| info.methods.iter().find(|m| m.name == method))
            .map(|m| m.has_self)
            .unwrap_or(true);

        let impl_name =
            impl_method_key(tname_str, &trait_generics, &for_type, &for_generics, method);
        if let Some(task) = core.environment.get(&impl_name) {
            let mut all_args = if has_self {
                vec![receiver.clone()]
            } else {
                Vec::new()
            };
            all_args.extend(args);
            return Ok(TraitMethodTarget::Call(task, all_args));
        }

        let default_name = default_impl_method_key(tname_str, &trait_generics, method);
        if let Some(task) = core.environment.get(&default_name) {
            let mut all_args = if has_self {
                vec![receiver.clone()]
            } else {
                Vec::new()
            };
            all_args.extend(args);
            return Ok(TraitMethodTarget::Call(task, all_args));
        }
    }

    Err(MoraError::Runtime(format!(
        "trait dispatch at line {}: no impl for type '{}' method '{}' (searched: {})",
        call_site.line,
        for_type,
        method,
        search_chain.join(" → "),
    )))
}

/// Pure helper constructing a `TraitObject` instance for `Trait::new("ForType")`.
pub(crate) fn construct_trait_instance_core(
    core: &crate::runtime::CoreRuntime,
    trait_name: &str,
    trait_generics: &[String],
    for_type: &str,
    for_generics: &[String],
    call_site: Span,
) -> MoraResult<Value> {
    let method_names = collect_required_methods(&core.trait_registry, trait_name);
    for m in method_names {
        let impl_name = impl_method_key(trait_name, trait_generics, for_type, for_generics, m);
        let default_name = default_impl_method_key(trait_name, trait_generics, m);
        let has_specific = core.environment.get(&impl_name).is_some();
        let has_default = core.environment.get(&default_name).is_some();
        if !has_specific && !has_default {
            return Err(MoraError::Runtime(format!(
                "trait {}<{}> method '{}' has no impl for type {}<{}> and no default (line {})",
                trait_name,
                trait_generics.join(","),
                m,
                for_type,
                for_generics.join(","),
                call_site.line
            )));
        }
    }

    Ok(Value::TraitObject {
        for_generics: for_generics.to_vec(),
        trait_generics: trait_generics.to_vec(),
        for_type: for_type.to_string(),
        trait_name: trait_name.to_string(),
        data: Box::new(Value::Dict(HashMap::new())),
    })
}
