//! v0.110.0: Trait 分发机制。
//!
//! - `resolve_trait_method_call`: dyn dispatch，沿 trait 继承链找 impl 调用
//! - `resolve_concrete_impl_method`: 具体类型上的 inherent / trait impl 方法查找
//! - `construct_trait_instance_core`: `Trait::new("ForType")` 构造 trait instance
//! - `collect_required_methods` / `collect_parent_traits`: trait 继承链辅助

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
    let (for_type, for_generics, trait_name, trait_generics, underlying_data) = match receiver {
        Value::TraitObject {
            for_type,
            for_generics,
            trait_name,
            trait_generics,
            data,
        } => (
            for_type.clone(),
            for_generics.clone(),
            trait_name.clone(),
            trait_generics.clone(),
            (**data).clone(),
        ),
        Value::Nil => return Ok(TraitMethodTarget::Immediate(Value::Nil)),
        _ => {
            return Err(MoraError::Runtime(format!(
                "trait dispatch at line {}: receiver must be trait object or nil, got {:?}",
                call_site.line, receiver
            )));
        }
    };

    let search_chain = collect_parent_traits(&core.registry.trait_registry, &trait_name);
    for tname in &search_chain {
        let tname_str: &str = tname;
        let has_self = core
            .registry
            .trait_registry
            .get(tname_str)
            .and_then(|info| info.methods.iter().find(|m| m.name == method))
            .map(|m| m.has_self)
            .unwrap_or(true);

        // Stage 3C: Check compiled source-level `impl Trait for Type` registry first (case-insensitive for built-in names like String/string, plus `number` matching int/float).
        let lookup_impl = |ty_key: &str| -> Option<(Value, bool)> {
            core.registry
                .trait_impls
                .get(&(
                    tname_str.to_string(),
                    ty_key.to_string(),
                    method.to_string(),
                ))
                .cloned()
                .or_else(|| {
                    let lower = ty_key.to_lowercase();
                    core.registry
                        .trait_impls
                        .iter()
                        .find(|((tr, tgt, m), _)| {
                            tr == tname_str
                                && m == method
                                && (tgt.eq_ignore_ascii_case(&lower)
                                    || (tgt.eq_ignore_ascii_case("number")
                                        && matches!(lower.as_str(), "int" | "float")))
                        })
                        .map(|(_, v)| v.clone())
                })
        };

        if let Some((task, impl_has_self)) = lookup_impl(&for_type) {
            let mut all_args = if impl_has_self {
                vec![underlying_data.clone()]
            } else {
                Vec::new()
            };
            all_args.extend(args);
            return Ok(TraitMethodTarget::Call(task, all_args));
        }

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

        if let Some((task, def_has_self)) = core
            .registry
            .trait_defaults
            .get(&(tname_str.to_string(), method.to_string()))
            .cloned()
        {
            let mut all_args = if def_has_self {
                vec![underlying_data.clone()]
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

/// Pure lookup returning `(callee_fn, has_self)` for an inherent or trait `impl` method on `receiver`
/// without cloning `receiver` or arguments, allowing `RegSlot`-based dispatch to pass borrowed `self`
/// directly into `materialize_frame_slots_named`.
pub(crate) fn lookup_concrete_impl_callee(
    core: &crate::runtime::CoreRuntime,
    receiver: &Value,
    method: &str,
) -> Option<(Value, bool)> {
    let type_key = crate::flow::dynamic_type_name(receiver);
    let lower_key = type_key.to_lowercase();

    let target_matches = |tgt: &str| -> bool {
        tgt == type_key
            || tgt.eq_ignore_ascii_case(&lower_key)
            || (tgt.eq_ignore_ascii_case("number") && matches!(lower_key.as_str(), "int" | "float"))
    };

    // 1. Inherent `impl TargetType` methods
    let inherent = core
        .registry
        .inherent_methods
        .get(&(type_key.clone(), method.to_string()))
        .cloned()
        .or_else(|| {
            core.registry
                .inherent_methods
                .iter()
                .find(|((tgt, m), _)| m == method && target_matches(tgt))
                .map(|(_, v)| v.clone())
        });
    if inherent.is_some() {
        return inherent;
    }

    // 2. Trait `impl Trait for TargetType` methods
    let trait_direct = core
        .registry
        .trait_impls
        .iter()
        .find(|((_, tgt, m), _)| m == method && target_matches(tgt))
        .map(|(_, v)| v.clone());
    if trait_direct.is_some() {
        return trait_direct;
    }

    // 3. Default method on any trait implemented by `TargetType` (or its parent traits)
    let mut implemented_traits: Vec<String> = Vec::new();
    for (tr, tgt) in core.registry.trait_impl_pairs.iter() {
        if target_matches(tgt) && !implemented_traits.contains(tr) {
            implemented_traits.push(tr.clone());
        }
    }
    for (tr, tgt, _) in core.registry.trait_impls.keys() {
        if target_matches(tgt) && !implemented_traits.contains(tr) {
            implemented_traits.push(tr.clone());
        }
    }
    for tr in implemented_traits {
        for anc in collect_parent_traits(&core.registry.trait_registry, &tr) {
            if let Some(entry) = core
                .registry
                .trait_defaults
                .get(&(anc, method.to_string()))
                .cloned()
            {
                return Some(entry);
            }
        }
    }
    None
}

/// Pure lookup for inherent or trait `impl` method on a concrete (unboxed) receiver value.
pub(crate) fn resolve_concrete_impl_method(
    core: &crate::runtime::CoreRuntime,
    receiver: &Value,
    method: &str,
    args: &[Value],
) -> Option<(Value, Vec<Value>)> {
    let (func, has_self) = lookup_concrete_impl_callee(core, receiver, method)?;
    let mut all_args = Vec::with_capacity(usize::from(has_self) + args.len());
    if has_self {
        all_args.push(receiver.clone());
    }
    all_args.extend_from_slice(args);
    Some((func, all_args))
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
    let method_names = collect_required_methods(&core.registry.trait_registry, trait_name);
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
        data: Box::new(Value::Dict(Default::default())),
    })
}
