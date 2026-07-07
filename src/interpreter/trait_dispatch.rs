//! Trait 分发机制
//!
//! 从 interpreter/mod.rs 提取的 trait 相关方法：
//! - dispatch_trait_method: dyn dispatch，沿 trait 继承链找 impl 调用
//! - construct_trait_instance: Trait::new("ForType") 构造 trait instance

use super::*;
use crate::common::Span;
use crate::value::Value;

/// 收集 trait 继承链上所有必需方法名
pub(crate) fn collect_required_methods<'a>(
    registry: &'a std::collections::HashMap<String, crate::interpreter::TraitInfo>,
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
    registry: &std::collections::HashMap<String, crate::interpreter::TraitInfo>,
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

impl Interpreter {
    /// dyn dispatch —— 接收 TraitObject + method，从 trait 继承链找 impl 调用
    pub fn dispatch_trait_method(
        &mut self,
        receiver: &Value,
        method: &str,
        args: Vec<Value>,
        call_site: Span,
    ) -> Result<Value, String> {
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
            Value::Nil => return Ok(Value::Nil),
            _ => {
                return Err(format!(
                    "trait dispatch at line {}: receiver must be trait object or nil, got {:?}",
                    call_site.line, receiver
                ));
            }
        };

        let search_chain = collect_parent_traits(&self.trait_registry, &trait_name);
        for tname in &search_chain {
            let tname_str: &str = tname;
            let has_self = self
                .trait_registry
                .get(tname_str)
                .and_then(|info| info.methods.iter().find(|m| m.name == method))
                .map(|m| m.has_self)
                .unwrap_or(true);

            // 1. 先找具体类型的 impl
            let impl_name =
                impl_method_key(tname_str, &trait_generics, &for_type, &for_generics, method);
            let env = self.environment.lock();
            if let Some(task) = env.get(&impl_name) {
                drop(env);
                let mut all_args = if has_self {
                    vec![receiver.clone()]
                } else {
                    Vec::new()
                };
                all_args.extend(args);
                return self.call_value(&task, all_args);
            }
            drop(env);

            // 2. fallback 到 trait 默认实现
            let default_name = default_impl_method_key(tname_str, &trait_generics, method);
            let env = self.environment.lock();
            if let Some(task) = env.get(&default_name) {
                drop(env);
                let mut all_args = if has_self {
                    vec![receiver.clone()]
                } else {
                    Vec::new()
                };
                all_args.extend(args);
                return self.call_value(&task, all_args);
            }
            drop(env);
        }

        Err(format!(
            "trait dispatch at line {}: no impl for type '{}' method '{}' (searched: {})",
            call_site.line,
            for_type,
            method,
            search_chain.join(" → "),
        ))
    }

    /// 构造 trait instance（Trait::new("ForType") 调用）
    pub fn construct_trait_instance(
        &mut self,
        trait_name: &str,
        trait_generics: &[String],
        for_type: &str,
        for_generics: &[String],
        call_site: Span,
    ) -> Result<Value, String> {
        let method_names = collect_required_methods(&self.trait_registry, trait_name);
        let env = self.environment.lock();
        for m in method_names {
            let impl_name = impl_method_key(trait_name, trait_generics, for_type, for_generics, m);
            let default_name = default_impl_method_key(trait_name, trait_generics, m);
            let has_specific = env.get(&impl_name).is_some();
            let has_default = env.get(&default_name).is_some();
            if !has_specific && !has_default {
                drop(env);
                return Err(format!(
                    "trait {}<{}> method '{}' has no impl for type {}<{}> and no default (line {})",
                    trait_name,
                    trait_generics.join(","),
                    m,
                    for_type,
                    for_generics.join(","),
                    call_site.line
                ));
            }
        }
        drop(env);

        Ok(Value::TraitObject {
            for_generics: for_generics.to_vec(),
            trait_generics: trait_generics.to_vec(),
            for_type: for_type.to_string(),
            trait_name: trait_name.to_string(),
            data: Box::new(Value::Dict(HashMap::new())),
        })
    }
}
