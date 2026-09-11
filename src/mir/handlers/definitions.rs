//! Type and metadata definition handlers.

use std::collections::HashMap;

use std::sync::Arc;

use crate::common::trait_info::{TraitInfo, TraitMethodSig, default_impl_method_key, impl_method_key};

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
            let key = default_impl_method_key(
                name,
                &Vec::<String>::new(),
                &m.name,
            );
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
            let key = impl_method_key(
                trait_name,
                trait_generics,
                for_type,
                for_generics,
                &m.name,
            );
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

#[allow(clippy::too_many_arguments)] // skill def 需携带完整元数据（9 字段，与 emit 端对齐）
pub fn h_skill_def(
    env: &mut Environment,
    name: &str,
    description: &Option<String>,
    version: &Option<String>,
    requires: &[String],
    tasks: &[crate::mir::orchestrate::MirSkillTask],
    task_bodies: &[MirFunction],
    verify: &Option<crate::mir::orchestrate::MirSkillVerify>,
    verify_body: &Option<MirFunction>,
) {
    let mut meta = HashMap::new();
    meta.insert("name".to_string(), Value::String(name.to_string()));
    if let Some(d) = description {
        meta.insert("description".to_string(), Value::String(d.clone()));
    }
    if let Some(v) = version {
        meta.insert("version".to_string(), Value::String(v.clone()));
    }
    meta.insert(
        "requires".to_string(),
        Value::List(requires.iter().map(|r| Value::String(r.clone())).collect()),
    );
    for (task, _body) in tasks.iter().zip(task_bodies.iter()) {
        if let Some(mfn) = &task.body {
            meta.insert(
                task.name.clone(),
                Value::Task {
                    name: task.name.clone(),
                    params: task.params.iter().map(|p| p.name.clone()).collect(),
                    mir_body: std::sync::Arc::new(mfn.clone()),
                },
            );
        }
    }
    if let Some(v) = verify {
        let vp: Vec<String> = v.params.iter().map(|p| p.name.clone()).collect();
        let empty = MirFunction {
            params: vp.clone(),
            body: Vec::new(),
            n_regs: 0,
            ..Default::default()
        };
        let verify_mir = v.body.clone().unwrap_or(empty);
        let _ = verify_body;
        meta.insert(
            "verify".to_string(),
            Value::Task {
                name: "verify".to_string(),
                params: vp,
                mir_body: std::sync::Arc::new(verify_mir),
            },
        );
    }
    env.define(name.to_string(), Value::Dict(meta), false);
}
