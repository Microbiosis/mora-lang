//! Type and metadata definition handlers.

use std::sync::Arc;

use crate::common::trait_info::{
    TraitInfo, TraitMethodSig, default_impl_method_key, impl_method_key,
};

use crate::mir::host::MirHost;

use crate::mir::MirFunction;

use crate::value::{Environment, Value};

// ============================================================
// Type definition handlers
// ============================================================

pub fn h_trait_def(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    name: &str,
    parents: &[String],
    methods: &[crate::mir::orchestrate::MirTraitMethod],
    method_bodies: &[MirFunction],
) -> Result<(), String> {
    let sigs: Vec<TraitMethodSig> = methods
        .iter()
        .map(|m| TraitMethodSig {
            name: m.name.clone(),
            params: m
                .params
                .iter()
                .map(|p| (p.name.clone(), p.type_hint.as_ref().map(|t| t.name())))
                .collect(),
            return_type: m.return_type.clone(),
            has_self: m.params.first().map(|p| p.name == "self").unwrap_or(false),
        })
        .collect();
    Arc::make_mut(interp.trait_registry()).insert(
        name.to_string(),
        TraitInfo {
            name: name.to_string(),
            parents: parents.to_vec(),
            methods: sigs,
        },
    );
    for (m, _body) in methods.iter().zip(method_bodies.iter()) {
        if let Some(mfn) = &m.body {
            let key = default_impl_method_key(name, &Vec::<String>::new(), &m.name);
            env.define(
                key,
                Value::Task {
                    name: m.name.clone(),
                    params: m.params.iter().map(|p| p.name.clone()).collect(),
                    mir_body: std::sync::Arc::new(mfn.clone()),
                },
                false,
            );
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn h_impl_def(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    trait_name: &str,
    trait_generics: &[String],
    for_type: &str,
    for_generics: &[String],
    methods: &[crate::mir::orchestrate::MirFnDef],
    method_bodies: &[MirFunction],
) -> Result<(), String> {
    Arc::make_mut(interp.impl_table())
        .entry(trait_name.to_string())
        .or_default()
        .push(for_type.to_string());
    for (m, _body) in methods.iter().zip(method_bodies.iter()) {
        if let Some(mfn) = &m.body {
            let key = impl_method_key(trait_name, trait_generics, for_type, for_generics, &m.name);
            env.define(
                key,
                Value::Task {
                    name: m.name.clone(),
                    params: m.params.iter().map(|p| p.name.clone()).collect(),
                    mir_body: std::sync::Arc::new(mfn.clone()),
                },
                false,
            );
        }
    }
    Ok(())
}
