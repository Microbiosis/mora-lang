//! Wave 3 — Stage 3C: Unified HM Static Checking for Nominal Structs/Enums/Tags,
//! Source-Level Traits/Impls, Default & Named Arguments, Multi-Dim Tensor Slicing,
//! and Differentiable Programming (`grad` / `value_and_grad` / `no_grad`).

use std::collections::{HashMap, HashSet};

use crate::common::Span;
use crate::mir::effect::{EffectRow, ambient};
use crate::mir::hint::TypeHint;
use crate::mir::witness::{
    MirWitness, WitnessEnumVariant, WitnessImplMethod, WitnessKind, WitnessPattern,
    WitnessSliceAxis, WitnessStructField, WitnessTraitMethod,
};
use crate::typeck::{DimExpr, ShapeType, Type, TypeVarId};

use super::unify::{self, Constraint, SpannedConstraint};
use super::{FnParamSpec, HMInference, ImplMethodTypeSig, TypeError};

pub(super) const STRUCT_TYPE_PREFIX: &str = "__StructType__::";
pub(super) const ENUM_TYPE_PREFIX: &str = "__EnumType__::";

type AlignedCallSlots = Vec<Option<(Type, EffectRow, Span)>>;

/// Substitute rigid generic parameter names (`type_params`) with concrete/meta `args`.
pub(super) fn substitute_named_generics(ty: &Type, type_params: &[String], args: &[Type]) -> Type {
    if type_params.is_empty() || args.is_empty() {
        return ty.clone();
    }
    let map: HashMap<&str, &Type> = type_params
        .iter()
        .map(String::as_str)
        .zip(args.iter())
        .collect();
    subst_generics_inner(ty, &map)
}

fn subst_generics_inner(ty: &Type, map: &HashMap<&str, &Type>) -> Type {
    match ty {
        Type::TypeVar(TypeVarId::Rigid(name)) => map
            .get(name.as_str())
            .map(|t| (*t).clone())
            .unwrap_or_else(|| ty.clone()),
        Type::Trait { name, generics } if generics.is_empty() => {
            if let Some(rep) = map.get(name.as_str()) {
                return (*rep).clone();
            }
            ty.clone()
        }
        Type::List(e) => Type::List(Box::new(subst_generics_inner(e, map))),
        Type::Channel(e) => Type::Channel(Box::new(subst_generics_inner(e, map))),
        Type::TaskHandle(e) => Type::TaskHandle(Box::new(subst_generics_inner(e, map))),
        Type::Dict(k, v) => Type::Dict(
            Box::new(subst_generics_inner(k, map)),
            Box::new(subst_generics_inner(v, map)),
        ),
        Type::Result_(ok, err) => Type::Result_(
            Box::new(subst_generics_inner(ok, map)),
            Box::new(subst_generics_inner(err, map)),
        ),
        Type::Tuple(es) => Type::Tuple(
            es.iter()
                .map(|e| Box::new(subst_generics_inner(e, map)))
                .collect(),
        ),
        Type::Cons(h, t) => Type::Cons(
            Box::new(subst_generics_inner(h, map)),
            Box::new(subst_generics_inner(t, map)),
        ),
        Type::Union(ms) => Type::Union(ms.iter().map(|m| subst_generics_inner(m, map)).collect()),
        Type::Trait { name, generics } => Type::Trait {
            name: name.clone(),
            generics: generics
                .iter()
                .map(|g| subst_generics_inner(g, map))
                .collect(),
        },
        Type::Concrete {
            name,
            generics,
            traits,
        } => Type::Concrete {
            name: name.clone(),
            generics: generics
                .iter()
                .map(|g| subst_generics_inner(g, map))
                .collect(),
            traits: traits
                .iter()
                .map(|t| subst_generics_inner(t, map))
                .collect(),
        },
        Type::TraitObject {
            trait_name,
            generics,
        } => Type::TraitObject {
            trait_name: trait_name.clone(),
            generics: generics
                .iter()
                .map(|g| subst_generics_inner(g, map))
                .collect(),
        },
        Type::Arrow(i, o, r) => Type::Arrow(
            Box::new(subst_generics_inner(i, map)),
            Box::new(subst_generics_inner(o, map)),
            r.clone(),
        ),
        Type::ForAll(vars, inner) => {
            let filtered: HashMap<&str, &Type> = map
                .iter()
                .filter(|(k, _)| !vars.contains(&TypeVarId::Rigid((**k).to_string())))
                .map(|(k, v)| (*k, *v))
                .collect();
            Type::ForAll(
                vars.clone(),
                Box::new(subst_generics_inner(inner, &filtered)),
            )
        }
        Type::Tensor {
            dtype,
            shape,
            device,
        } => {
            let new_shape = match shape {
                ShapeType::Ranked(dims) => ShapeType::Ranked(
                    dims.iter()
                        .map(|d| {
                            d.map_leaves(&mut |leaf| match leaf {
                                DimExpr::Symbolic(sym) => match map.get(sym.as_str()) {
                                    Some(Type::ConstInt(n)) if *n >= 0 => {
                                        DimExpr::Known(*n as usize)
                                    }
                                    Some(Type::TypeVar(TypeVarId::Meta(id))) => DimExpr::Var(*id),
                                    Some(Type::TypeVar(TypeVarId::Rigid(r))) => {
                                        DimExpr::Symbolic(r.clone())
                                    }
                                    _ => leaf.clone(),
                                },
                                other => other.clone(),
                            })
                        })
                        .collect(),
                ),
                ShapeType::Unranked(Some(TypeVarId::Rigid(r))) => match map.get(r.as_str()) {
                    Some(Type::TypeVar(v)) => ShapeType::Unranked(Some(v.clone())),
                    _ => shape.clone(),
                },
                other => other.clone(),
            };
            let new_device = match device {
                crate::typeck::DeviceType::Var(TypeVarId::Rigid(r)) => match map.get(r.as_str()) {
                    Some(Type::TypeVar(v)) => crate::typeck::DeviceType::Var(v.clone()),
                    _ => device.clone(),
                },
                other => other.clone(),
            };
            Type::Tensor {
                dtype: Box::new(subst_generics_inner(dtype, map)),
                shape: new_shape,
                device: new_device,
            }
        }
        other => other.clone(),
    }
}

/// Extract canonical target type name from a resolved `Type` for trait/impl registry lookup.
pub(super) fn canonical_target_name_of(ty: &Type) -> Option<String> {
    match ty {
        Type::Concrete { name, .. } => Some(name.clone()),
        Type::Trait { name, .. } => Some(name.clone()),
        Type::Int => Some("int".to_string()),
        Type::Float => Some("float".to_string()),
        Type::Scalar(dt) => Some(dt.canonical_type_name().to_string()),
        Type::String => Some("string".to_string()),
        Type::Char => Some("char".to_string()),
        Type::Bool => Some("bool".to_string()),
        Type::Nil => Some("nil".to_string()),
        Type::List(_) => Some("list".to_string()),
        Type::Dict(_, _) => Some("dict".to_string()),
        Type::Tensor { .. } => Some("Tensor".to_string()),
        Type::Channel(_) => Some("channel".to_string()),
        Type::TaskHandle(_) => Some("task_handle".to_string()),
        Type::Context => Some("context".to_string()),
        Type::TcpListener => Some("tcp_listener".to_string()),
        Type::TcpStream => Some("tcp_stream".to_string()),
        Type::TlsListener => Some("tls_listener".to_string()),
        Type::TlsStream => Some("tls_stream".to_string()),
        Type::ComputeDag => Some("compute_dag".to_string()),
        Type::KVAllocator => Some("kv_allocator".to_string()),
        Type::MemoryPool => Some("memory_pool".to_string()),
        Type::ProcessGroup => Some("process_group".to_string()),
        Type::TraceProfiler => Some("trace_profiler".to_string()),
        Type::CheckedPath => Some("checked_path".to_string()),
        Type::GpuKernel(_) => Some("gpu_kernel".to_string()),
        _ => None,
    }
}

fn target_name_matches(registered: &str, actual: &str) -> bool {
    if registered == actual || registered.eq_ignore_ascii_case(actual) {
        return true;
    }
    registered.eq_ignore_ascii_case("number")
        && (actual.eq_ignore_ascii_case("int") || actual.eq_ignore_ascii_case("float"))
}

fn is_float_differentiable_type(ty: &Type) -> bool {
    match ty {
        Type::Float | Type::Any | Type::Error | Type::TypeVar(_) => true,
        Type::Scalar(dt) => dt.is_float(),
        Type::Tensor { dtype, .. } => match dtype.as_ref() {
            Type::Float | Type::Any | Type::Error | Type::TypeVar(_) => true,
            Type::Scalar(dt) => dt.is_float(),
            _ => false,
        },
        Type::List(elem) => is_float_differentiable_type(elem),
        Type::Dict(_, val) => is_float_differentiable_type(val),
        Type::Union(ms) if !ms.is_empty() => ms.iter().any(is_float_differentiable_type),
        _ => false,
    }
}

fn is_scalar_grad_output(ty: &Type) -> bool {
    match ty {
        Type::Float | Type::Any | Type::Error | Type::TypeVar(_) => true,
        Type::Scalar(dt) => dt.is_float(),
        Type::Tensor { dtype, shape, .. } => {
            let dt_ok = match dtype.as_ref() {
                Type::Float | Type::Any | Type::Error | Type::TypeVar(_) => true,
                Type::Scalar(dt) => dt.is_float(),
                _ => false,
            };
            if !dt_ok {
                return false;
            }
            match shape {
                ShapeType::Unranked(_) => true,
                ShapeType::Ranked(dims) => {
                    if dims.is_empty() {
                        return true;
                    }
                    // Allow 1-element static tensors `[1]` / `[1, 1]` or symbolic dims.
                    dims.iter()
                        .all(|d| !matches!(d, DimExpr::Known(n) if *n != 1))
                }
            }
        }
        Type::Union(ms) if !ms.is_empty() => ms.iter().any(is_scalar_grad_output),
        _ => false,
    }
}

/// Compute exact static slice length along one axis of known size `dim_len`.
fn static_slice_axis_len(
    dim_len: usize,
    start: Option<i64>,
    stop: Option<i64>,
    step: i64,
) -> usize {
    let n = dim_len as i64;
    if step > 0 {
        let mut s = start.unwrap_or(0);
        let mut e = stop.unwrap_or(n);
        if s < 0 {
            s += n;
        }
        if e < 0 {
            e += n;
        }
        let s = s.clamp(0, n);
        let e = e.clamp(0, n);
        if e <= s {
            0
        } else {
            ((e - s + step - 1) / step) as usize
        }
    } else {
        let mut s = start.unwrap_or(n - 1);
        let mut e = stop.unwrap_or(-n - 1);
        if s < 0 {
            s += n;
        }
        if stop.is_some() && e < 0 {
            e += n;
        }
        let s = s.clamp(-1, n - 1);
        let e = e.clamp(-1, n - 1);
        if s <= e {
            0
        } else {
            let abs_step = -step;
            ((s - e + abs_step - 1) / abs_step) as usize
        }
    }
}

impl HMInference {
    // ════════════════════════════════════════════════════════════════════════
    // 1. Struct / Enum / Trait / Impl Declaration Type Checking
    // ════════════════════════════════════════════════════════════════════════

    pub(super) fn infer_struct_def(
        &mut self,
        name: &str,
        _type_params: &[String],
        _const_params: &[String],
        fields: &[WitnessStructField],
        _span: Span,
    ) -> Result<(Type, EffectRow), Vec<TypeError>> {
        let mut seen_fields: HashSet<&str> = HashSet::new();
        for f in fields {
            if !seen_fields.insert(f.name.as_str()) {
                self.errors.push(TypeError::UnificationFailure {
                    expected: format!("distinct field names in struct '{name}'"),
                    got: format!("duplicate field '{}'", f.name),
                    span: Some(f.span),
                });
            }
            // Validate compile-time `@tag(k="v")` metadata.
            let mut seen_tags: HashSet<&str> = HashSet::new();
            for (tk, tv) in &f.tags {
                if tk.is_empty() {
                    self.errors.push(TypeError::InvalidLiteral {
                        what: "struct field @tag key".to_string(),
                        value: tk.clone(),
                        span: Some(f.span),
                    });
                    continue;
                }
                if !seen_tags.insert(tk.as_str()) {
                    self.errors.push(TypeError::UnificationFailure {
                        expected: format!("distinct @tag keys on field '{name}.{}'", f.name),
                        got: format!("duplicate @tag key '{tk}'"),
                        span: Some(f.span),
                    });
                }
                if tk == "json" && tv != "-" {
                    for opt in tv.split(',').skip(1) {
                        if !opt.is_empty() && opt != "omitempty" {
                            self.errors.push(TypeError::InvalidLiteral {
                                what: format!("@tag(json=...) option on '{name}.{}'", f.name),
                                value: opt.to_string(),
                                span: Some(f.span),
                            });
                        }
                    }
                }
                if matches!(tk.as_str(), "param" | "buffer" | "transient")
                    && !tv.eq_ignore_ascii_case("true")
                    && !tv.eq_ignore_ascii_case("false")
                {
                    self.errors.push(TypeError::InvalidLiteral {
                        what: format!("@tag({tk}=...) boolean flag on '{name}.{}'", f.name),
                        value: tv.clone(),
                        span: Some(f.span),
                    });
                }
            }
            // Type-check field default initializer against declared field type.
            if let Some(def_expr) = &f.default {
                let expected_ty = self.resolve_type_aliases(f.type_hint.to_type());
                let (def_ty, _def_row) = self.infer_recover(def_expr);
                if !matches!(def_ty, Type::Error) {
                    let c = SpannedConstraint::new(
                        Constraint::Subtype {
                            sub: Box::new(def_ty),
                            sup: Box::new(expected_ty),
                        },
                        def_expr.span,
                    )
                    .with_hint(format!(
                        "default value for struct field '{name}.{}' must match its declared type",
                        f.name
                    ));
                    match unify::solve_spanned(&c, &self.subst) {
                        Ok(s) => self.subst = s,
                        Err(e) => self.errors.push(e),
                    }
                }
            }
        }
        Ok((Type::Nil, EffectRow::Empty))
    }

    pub(super) fn infer_enum_def(
        &mut self,
        name: &str,
        _type_params: &[String],
        _const_params: &[String],
        variants: &[WitnessEnumVariant],
        _span: Span,
    ) -> Result<(Type, EffectRow), Vec<TypeError>> {
        let mut seen: HashSet<&str> = HashSet::new();
        for v in variants {
            if !seen.insert(v.name.as_str()) {
                self.errors.push(TypeError::UnificationFailure {
                    expected: format!("distinct variant names in enum '{name}'"),
                    got: format!("duplicate variant '{}'", v.name),
                    span: Some(v.span),
                });
            }
        }
        Ok((Type::Nil, EffectRow::Empty))
    }

    pub(super) fn infer_trait_def(
        &mut self,
        name: &str,
        type_params: &[String],
        parents: &[String],
        methods: &[WitnessTraitMethod],
        span: Span,
    ) -> Result<(Type, EffectRow), Vec<TypeError>> {
        // 1. Validate parent traits exist and contain no inheritance cycle.
        for parent in parents {
            if !self.trait_defs.contains_key(parent) {
                self.errors.push(TypeError::UnificationFailure {
                    expected: format!("declared supertrait for trait '{name}'"),
                    got: format!("undefined trait '{parent}'"),
                    span: Some(span),
                });
            }
        }
        if self.trait_has_cycle(name) {
            self.errors.push(TypeError::UnificationFailure {
                expected: format!("acyclic supertrait hierarchy for trait '{name}'"),
                got: format!("cyclic supertrait inheritance involving '{name}'"),
                span: Some(span),
            });
        }

        // 2. Type-check default method bodies with `self: dyn Trait` / `Trait`.
        let self_ty = Type::TraitObject {
            trait_name: name.to_string(),
            generics: type_params
                .iter()
                .map(|tp| Type::TypeVar(TypeVarId::Rigid(tp.clone())))
                .collect(),
        };
        let mut seen_methods: HashSet<&str> = HashSet::new();
        for m in methods {
            if !seen_methods.insert(m.name.as_str()) {
                self.errors.push(TypeError::UnificationFailure {
                    expected: format!("distinct method names in trait '{name}'"),
                    got: format!("duplicate method '{}'", m.name),
                    span: Some(m.span),
                });
            }
            if let Some(default_body) = &m.default_body {
                let param_types: Vec<Type> = m
                    .params
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        if i == 0 && m.has_self {
                            self_ty.clone()
                        } else {
                            let raw = p
                                .type_hint
                                .as_ref()
                                .map(|h| self.resolve_type_aliases(h.to_type()))
                                .unwrap_or_else(|| self.fresh_type_var());
                            Self::substitute_self_type(&raw, &self_ty)
                        }
                    })
                    .collect();
                let ret_ty = m
                    .return_type
                    .as_ref()
                    .map(|h| {
                        let raw = self.resolve_type_aliases(h.to_type());
                        Self::substitute_self_type(&raw, &self_ty)
                    })
                    .unwrap_or_else(|| self.fresh_type_var());
                let res = self.infer_closure_core(
                    &param_types,
                    &m.params,
                    ret_ty.clone(),
                    default_body,
                    m.span,
                );
                match res {
                    Ok((body_ty, body_row)) => {
                        if let Some(ref declared) = m.effect_annotation {
                            let ctx = format!("trait method '{name}::{}'", m.name);
                            self.check_declared_effect_row(declared, &body_row, &ctx, m.span);
                        }
                        if !matches!(body_ty, Type::Error) {
                            let c = SpannedConstraint::new(
                                Constraint::Subtype {
                                    sub: Box::new(body_ty),
                                    sup: Box::new(ret_ty),
                                },
                                default_body.span,
                            );
                            if let Err(e) = unify::solve_spanned(&c, &self.subst) {
                                self.errors.push(e);
                            }
                        }
                    }
                    Err(errs) => self.errors.extend(errs),
                }
            }
        }
        Ok((Type::Nil, EffectRow::Empty))
    }

    fn trait_has_cycle(&self, start: &str) -> bool {
        let mut stack: Vec<String> = self
            .trait_defs
            .get(start)
            .map(|d| d.parents.clone())
            .unwrap_or_default();
        let mut visited: HashSet<String> = HashSet::new();
        while let Some(cur) = stack.pop() {
            if cur == start {
                return true;
            }
            if visited.insert(cur.clone())
                && let Some(def) = self.trait_defs.get(&cur)
            {
                stack.extend(def.parents.iter().cloned());
            }
        }
        false
    }

    pub(super) fn collect_trait_chain(&self, trait_name: &str) -> Vec<String> {
        let mut out = vec![trait_name.to_string()];
        let mut seen = HashSet::new();
        seen.insert(trait_name.to_string());
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(trait_name.to_string());
        while let Some(cur) = queue.pop_front() {
            if let Some(def) = self.trait_defs.get(&cur) {
                for p in &def.parents {
                    if seen.insert(p.clone()) {
                        out.push(p.clone());
                        queue.push_back(p.clone());
                    }
                }
            }
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn infer_impl_def(
        &mut self,
        _type_params: &[String],
        const_params: &[String],
        trait_name: Option<&str>,
        _trait_generics: &[TypeHint],
        target_type: &str,
        target_generics: &[TypeHint],
        methods: &[WitnessImplMethod],
        span: Span,
    ) -> Result<(Type, EffectRow), Vec<TypeError>> {
        let self_ty = self.resolve_impl_target_type(target_type, target_generics);

        // P4-1A: Collect const-generic parameter names in scope for all method bodies (bound as `int`).
        let mut active_consts: Vec<String> = const_params.to_vec();
        if let Some(sdef) = self.struct_defs.get(target_type) {
            for (idx, tg) in target_generics.iter().enumerate() {
                if let Some(tp_name) = sdef.type_params.get(idx)
                    && sdef.const_params.contains(tp_name)
                    && let Type::TypeVar(TypeVarId::Rigid(r)) = tg.to_type()
                    && !active_consts.contains(r)
                {
                    active_consts.push(r.clone());
                }
            }
        } else if let Some(edef) = self.enum_defs.get(target_type) {
            for (idx, tg) in target_generics.iter().enumerate() {
                if let Some(tp_name) = edef.type_params.get(idx)
                    && edef.const_params.contains(tp_name)
                    && let Type::TypeVar(TypeVarId::Rigid(r)) = tg.to_type()
                    && !active_consts.contains(r)
                {
                    active_consts.push(r.clone());
                }
            }
        }

        // 1. For `impl Trait for Target`, validate trait existence, required methods, and arities.
        if let Some(tr_name) = trait_name {
            if !self.trait_defs.contains_key(tr_name) {
                self.errors.push(TypeError::UnificationFailure {
                    expected: format!("declared trait in `impl {tr_name} for {target_type}`"),
                    got: format!("undefined trait '{tr_name}'"),
                    span: Some(span),
                });
            } else {
                let chain = self.collect_trait_chain(tr_name);
                let mut required_sigs: HashMap<String, super::TraitMethodTypeSig> = HashMap::new();
                let mut all_trait_sigs: HashMap<String, super::TraitMethodTypeSig> = HashMap::new();
                for anc in &chain {
                    if let Some(tdef) = self.trait_defs.get(anc) {
                        for tm in &tdef.methods {
                            all_trait_sigs
                                .entry(tm.name.clone())
                                .or_insert_with(|| tm.clone());
                            if !tm.has_default {
                                required_sigs
                                    .entry(tm.name.clone())
                                    .or_insert_with(|| tm.clone());
                            }
                        }
                    }
                }
                let impl_method_names: HashSet<&str> =
                    methods.iter().map(|m| m.name.as_str()).collect();
                for req_name in required_sigs.keys() {
                    let has_here = impl_method_names.contains(req_name.as_str());
                    let has_registered = chain.iter().any(|anc| {
                        self.trait_impl_methods.contains_key(&(
                            anc.clone(),
                            target_type.to_string(),
                            req_name.clone(),
                        ))
                    });
                    if !has_here && !has_registered {
                        self.errors.push(TypeError::UnificationFailure {
                            expected: format!(
                                "implementation of required method '{req_name}' from trait '{tr_name}' for '{target_type}'"
                            ),
                            got: format!("missing method '{req_name}' in `impl {tr_name} for {target_type}`"),
                            span: Some(span),
                        });
                    }
                }
                // Check arity and `self` compatibility of implemented trait methods.
                for m in methods {
                    if let Some(tsig) = all_trait_sigs.get(&m.name) {
                        if tsig.has_self != m.has_self {
                            self.errors.push(TypeError::UnificationFailure {
                                expected: format!(
                                    "method '{tr_name}::{}' with has_self={}",
                                    m.name, tsig.has_self
                                ),
                                got: format!(
                                    "impl method '{}' with has_self={}",
                                    m.name, m.has_self
                                ),
                                span: Some(m.span),
                            });
                        }
                        let impl_user_params =
                            m.params.len().saturating_sub(usize::from(m.has_self));
                        if impl_user_params != tsig.params.len() {
                            self.errors.push(TypeError::ArityMismatch {
                                expected: tsig.params.len(),
                                actual: impl_user_params,
                                span: m.span,
                            });
                        }
                    }
                }
            }
        }

        // 2. Type-check every method body inside `impl` (with const-generic params bound as `int`).
        let saved_env = if active_consts.is_empty() {
            None
        } else {
            let prev = self.env.clone();
            for cname in &active_consts {
                self.env.add(cname.clone(), Type::Int);
            }
            Some(prev)
        };
        for m in methods {
            let param_types: Vec<Type> = m
                .params
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    if i == 0 && m.has_self {
                        self_ty.clone()
                    } else {
                        let raw = p
                            .type_hint
                            .as_ref()
                            .map(|h| self.resolve_type_aliases(h.to_type()))
                            .unwrap_or_else(|| self.fresh_type_var());
                        Self::substitute_self_type(&raw, &self_ty)
                    }
                })
                .collect();
            let ret_ty = m
                .return_type
                .as_ref()
                .map(|h| {
                    let raw = self.resolve_type_aliases(h.to_type());
                    Self::substitute_self_type(&raw, &self_ty)
                })
                .unwrap_or_else(|| self.fresh_type_var());
            let c_start = self.constraints.len();
            let res =
                self.infer_closure_core(&param_types, &m.params, ret_ty.clone(), &m.body, m.span);
            match res {
                Ok((body_ty, body_row)) => {
                    if !matches!(body_ty, Type::Error) {
                        self.constraints.push(
                            SpannedConstraint::new(
                                Constraint::Subtype {
                                    sub: Box::new(body_ty.clone()),
                                    sup: Box::new(ret_ty.clone()),
                                },
                                m.body.span,
                            )
                            .with_hint(format!(
                                "method '{target_type}::{}' return expression must match return type",
                                m.name
                            )),
                        );
                    }
                    let drained: Vec<SpannedConstraint> =
                        self.constraints.drain(c_start..).collect();
                    for sc in drained {
                        match unify::solve_spanned(&sc, &self.subst) {
                            Ok(s) => self.subst = s,
                            Err(e) => self.errors.push(e),
                        }
                    }
                    let inferred_ret = if m.return_type.is_some() {
                        self.subst.apply(&ret_ty)
                    } else {
                        self.subst.apply(&body_ty)
                    };
                    let effective_row = if let Some(ref declared) = m.effect_annotation {
                        let ctx = format!("method '{target_type}::{}'", m.name);
                        self.check_declared_effect_row(declared, &body_row, &ctx, m.span)
                    } else {
                        body_row
                    };
                    // Enforce trait method's declared effect row contract if implementing a trait.
                    if let Some(tr_name) = trait_name {
                        let trait_decl_eff = self
                            .collect_trait_chain(tr_name)
                            .into_iter()
                            .find_map(|anc| {
                                self.trait_defs.get(&anc).and_then(|td| {
                                    td.methods
                                        .iter()
                                        .find(|tm| tm.name == m.name)
                                        .and_then(|tm| tm.effect_row.clone())
                                })
                            });
                        if let Some(ref trait_eff) = trait_decl_eff {
                            let ctx = format!("trait '{tr_name}::{}' contract", m.name);
                            self.check_declared_effect_row(trait_eff, &effective_row, &ctx, m.span);
                        }
                    }
                    // Backfill inferred return type and effect row into the impl method table!
                    if let Some(tr_name) = trait_name {
                        if let Some(entry) = self.trait_impl_methods.get_mut(&(
                            tr_name.to_string(),
                            target_type.to_string(),
                            m.name.clone(),
                        )) {
                            entry.return_type = inferred_ret;
                            entry.effect_row = effective_row;
                        }
                    } else if let Some(entry) = self
                        .inherent_impls
                        .get_mut(&(target_type.to_string(), m.name.clone()))
                    {
                        entry.return_type = inferred_ret;
                        entry.effect_row = effective_row;
                    }
                }
                Err(errs) => self.errors.extend(errs),
            }
        }
        if let Some(prev) = saved_env {
            self.env = prev;
        }
        Ok((Type::Nil, EffectRow::Empty))
    }

    // ════════════════════════════════════════════════════════════════════════
    // 2. `dyn Trait` Coercion & Static Trait Impl Validation
    // ════════════════════════════════════════════════════════════════════════

    pub(super) fn validate_dyn_trait_coercion(
        &self,
        value_ty: &Type,
        trait_name: &str,
        span: Span,
    ) -> Result<(), Vec<TypeError>> {
        // Legacy unregistered dynamic traits (`let x: dyn Foo = 1` without `trait Foo`) remain permitted.
        if !self.trait_defs.contains_key(trait_name) {
            return Ok(());
        }
        let resolved = self.subst.apply(value_ty);
        if matches!(
            resolved,
            Type::Error | Type::Any | Type::TypeVar(_) | Type::TraitObject { .. }
        ) {
            return Ok(());
        }
        let Some(actual_name) = canonical_target_name_of(&resolved) else {
            return Err(vec![TypeError::UnificationFailure {
                expected: format!("type implementing trait '{trait_name}'"),
                got: format!(
                    "'{}' does not implement trait '{trait_name}'",
                    resolved.name()
                ),
                span: Some(span),
            }]);
        };
        let implements = self.trait_impl_pairs.iter().any(|(tr, tgt)| {
            target_name_matches(tgt, &actual_name)
                && self
                    .collect_trait_chain(tr)
                    .iter()
                    .any(|anc| anc == trait_name)
        });
        if !implements {
            return Err(vec![TypeError::UnificationFailure {
                expected: format!("type implementing trait '{trait_name}'"),
                got: format!("type '{actual_name}' does not implement trait '{trait_name}'"),
                span: Some(span),
            }]);
        }
        Ok(())
    }

    pub(super) fn infer_dyn_trait(
        &mut self,
        inner: &MirWitness,
        trait_name: &str,
        generics: &[TypeHint],
        span: Span,
    ) -> Result<(Type, EffectRow), Vec<TypeError>> {
        let (val_ty, val_row) = self.infer_recover(inner);
        self.validate_dyn_trait_coercion(&val_ty, trait_name, span)?;
        let gen_tys: Vec<Type> = generics
            .iter()
            .map(|h| self.resolve_type_aliases(h.to_type()))
            .collect();
        Ok((
            Type::TraitObject {
                trait_name: trait_name.to_string(),
                generics: gen_tys,
            },
            val_row,
        ))
    }

    // ════════════════════════════════════════════════════════════════════════
    // 3. Nominal Struct/Enum Constructors, Associated Calls, Field & Method Dispatch
    // ════════════════════════════════════════════════════════════════════════

    pub(super) fn try_infer_nominal_or_assoc_call(
        &mut self,
        name: &str,
        args: &[MirWitness],
        named_args: &[(String, MirWitness)],
        span: Span,
    ) -> Option<Result<(Type, EffectRow), Vec<TypeError>>> {
        self.try_infer_nominal_or_assoc_call_generic(name, &[], args, named_args, span)
    }

    pub(super) fn try_infer_nominal_or_assoc_call_generic(
        &mut self,
        name: &str,
        explicit_type_args: &[Type],
        args: &[MirWitness],
        named_args: &[(String, MirWitness)],
        span: Span,
    ) -> Option<Result<(Type, EffectRow), Vec<TypeError>>> {
        // Direct struct constructor call `Point(1, 2)` / `Linear<4, 3>(weight=w, bias=b)`
        if self.struct_defs.contains_key(name) {
            return Some(self.infer_struct_constructor_generic(
                name,
                explicit_type_args,
                args,
                named_args,
                span,
            ));
        }

        let (type_name, member) = name.split_once("::")?;

        // Associated calls on nominal struct (`Point::new`, `Linear<4, 3>::new`, `Point::from_dict`, ...)
        if self.struct_defs.contains_key(type_name) {
            match member {
                "new" => {
                    // Check if user defined a custom associated `impl Point task new(...)` first.
                    if let Some(sig) = self
                        .inherent_impls
                        .get(&(type_name.to_string(), "new".to_string()))
                        .cloned()
                        && !sig.has_self
                    {
                        return Some(self.apply_impl_method_call(
                            &sig,
                            explicit_type_args,
                            args,
                            named_args,
                            EffectRow::Empty,
                            span,
                        ));
                    }
                    return Some(self.infer_struct_constructor_generic(
                        type_name,
                        explicit_type_args,
                        args,
                        named_args,
                        span,
                    ));
                }
                "from_dict" => {
                    if !named_args.is_empty() || args.len() != 1 {
                        return Some(Err(vec![TypeError::ArityMismatch {
                            expected: 1,
                            actual: args.len() + named_args.len(),
                            span,
                        }]));
                    }
                    let (_, row) = self.infer_recover(&args[0]);
                    let def = self
                        .struct_defs
                        .get(type_name)
                        .expect("struct checked")
                        .clone();
                    let fresh_gens: Vec<Type> = (0..def.type_params.len())
                        .map(|_| self.fresh_type_var())
                        .collect();
                    return Some(Ok((
                        Type::Concrete {
                            name: type_name.to_string(),
                            generics: fresh_gens,
                            traits: Vec::new(),
                        },
                        row,
                    )));
                }
                "schema" => {
                    if !args.is_empty() || !named_args.is_empty() {
                        return Some(Err(vec![TypeError::ArityMismatch {
                            expected: 0,
                            actual: args.len() + named_args.len(),
                            span,
                        }]));
                    }
                    return Some(Ok((
                        Type::Dict(Box::new(Type::String), Box::new(Type::Any)),
                        EffectRow::Empty,
                    )));
                }
                "tags" => {
                    if !args.is_empty() || !named_args.is_empty() {
                        return Some(Err(vec![TypeError::ArityMismatch {
                            expected: 0,
                            actual: args.len() + named_args.len(),
                            span,
                        }]));
                    }
                    return Some(Ok((
                        Type::Dict(
                            Box::new(Type::String),
                            Box::new(Type::Dict(Box::new(Type::String), Box::new(Type::String))),
                        ),
                        EffectRow::Empty,
                    )));
                }
                "fields" => {
                    if !args.is_empty() || !named_args.is_empty() {
                        return Some(Err(vec![TypeError::ArityMismatch {
                            expected: 0,
                            actual: args.len() + named_args.len(),
                            span,
                        }]));
                    }
                    return Some(Ok((Type::List(Box::new(Type::String)), EffectRow::Empty)));
                }
                _ => {}
            }
        }

        // Enum variant construction `Option::Some(42)` / `Color::Red` / `Color::variants()`
        if let Some(edef) = self.enum_defs.get(type_name).cloned() {
            if member == "variants" {
                if !args.is_empty() || !named_args.is_empty() {
                    return Some(Err(vec![TypeError::ArityMismatch {
                        expected: 0,
                        actual: args.len() + named_args.len(),
                        span,
                    }]));
                }
                return Some(Ok((Type::List(Box::new(Type::String)), EffectRow::Empty)));
            }
            if let Some((_, payload_tys)) = edef.variants.iter().find(|(v, _)| v == member) {
                if !named_args.is_empty() {
                    return Some(Err(vec![TypeError::UnificationFailure {
                        expected: format!("positional payload arguments for {type_name}::{member}"),
                        got: "keyword arguments".to_string(),
                        span: Some(span),
                    }]));
                }
                if args.len() != payload_tys.len() {
                    return Some(Err(vec![TypeError::ArityMismatch {
                        expected: payload_tys.len(),
                        actual: args.len(),
                        span,
                    }]));
                }
                let fresh_gens: Vec<Type> = if explicit_type_args.is_empty() {
                    (0..edef.type_params.len())
                        .map(|_| self.fresh_type_var())
                        .collect()
                } else {
                    if explicit_type_args.len() != edef.type_params.len() {
                        return Some(Err(vec![TypeError::UnificationFailure {
                            expected: format!(
                                "{} generic argument(s) for enum '{type_name}'",
                                edef.type_params.len()
                            ),
                            got: format!("{} generic argument(s)", explicit_type_args.len()),
                            span: Some(span),
                        }]));
                    }
                    for (tp, arg) in edef.type_params.iter().zip(explicit_type_args.iter()) {
                        if edef.const_params.contains(tp) {
                            let ok = matches!(
                                arg,
                                Type::ConstInt(_) | Type::TypeVar(_) | Type::Any | Type::Error
                            );
                            if !ok {
                                return Some(Err(vec![TypeError::UnificationFailure {
                                    expected: format!(
                                        "compile-time integer const argument for '{type_name}::{tp}'"
                                    ),
                                    got: arg.name(),
                                    span: Some(span),
                                }]));
                            }
                        } else if matches!(arg, Type::ConstInt(_)) {
                            return Some(Err(vec![TypeError::UnificationFailure {
                                expected: format!(
                                    "type argument (not integer constant) for '{type_name}::{tp}'"
                                ),
                                got: arg.name(),
                                span: Some(span),
                            }]));
                        }
                    }
                    explicit_type_args.to_vec()
                };
                let mut acc_row = EffectRow::Empty;
                for (arg_w, raw_pty) in args.iter().zip(payload_tys.iter()) {
                    let (arg_ty, arg_row) = self.infer_recover(arg_w);
                    acc_row = self.merge_rows(acc_row, arg_row, span);
                    let expected_pty =
                        substitute_named_generics(raw_pty, &edef.type_params, &fresh_gens);
                    if !matches!(arg_ty, Type::Error) {
                        let c = SpannedConstraint::new(
                            Constraint::Subtype {
                                sub: Box::new(arg_ty),
                                sup: Box::new(expected_pty),
                            },
                            arg_w.span,
                        );
                        match unify::solve_spanned(&c, &self.subst) {
                            Ok(s) => self.subst = s,
                            Err(_) => self.constraints.push(c),
                        }
                    }
                }
                let resolved_gens: Vec<Type> =
                    fresh_gens.iter().map(|g| self.subst.apply(g)).collect();
                return Some(Ok((
                    Type::Concrete {
                        name: type_name.to_string(),
                        generics: resolved_gens,
                        traits: Vec::new(),
                    },
                    acc_row,
                )));
            }
            return Some(Err(vec![TypeError::UnboundVariable {
                name: name.to_string(),
                span,
            }]));
        }

        // Inherent associated function without `self` (`Point::origin()`)
        if let Some(sig) = self
            .inherent_impls
            .get(&(type_name.to_string(), member.to_string()))
            .cloned()
            && !sig.has_self
        {
            return Some(self.apply_impl_method_call(
                &sig,
                &[],
                args,
                named_args,
                EffectRow::Empty,
                span,
            ));
        }

        None
    }

    pub(super) fn infer_struct_constructor(
        &mut self,
        struct_name: &str,
        args: &[MirWitness],
        named_args: &[(String, MirWitness)],
        span: Span,
    ) -> Result<(Type, EffectRow), Vec<TypeError>> {
        self.infer_struct_constructor_generic(struct_name, &[], args, named_args, span)
    }

    pub(super) fn infer_struct_constructor_generic(
        &mut self,
        struct_name: &str,
        explicit_type_args: &[Type],
        args: &[MirWitness],
        named_args: &[(String, MirWitness)],
        span: Span,
    ) -> Result<(Type, EffectRow), Vec<TypeError>> {
        let Some(def) = self.struct_defs.get(struct_name).cloned() else {
            return Err(vec![TypeError::UnboundVariable {
                name: struct_name.to_string(),
                span,
            }]);
        };
        if args.len() > def.fields.len() {
            return Err(vec![TypeError::ArityMismatch {
                expected: def.fields.len(),
                actual: args.len(),
                span,
            }]);
        }
        let fresh_gens: Vec<Type> = if explicit_type_args.is_empty() {
            (0..def.type_params.len())
                .map(|_| self.fresh_type_var())
                .collect()
        } else {
            if explicit_type_args.len() != def.type_params.len() {
                return Err(vec![TypeError::UnificationFailure {
                    expected: format!(
                        "{} generic argument(s) for struct '{struct_name}'",
                        def.type_params.len()
                    ),
                    got: format!("{} generic argument(s)", explicit_type_args.len()),
                    span: Some(span),
                }]);
            }
            for (tp, arg) in def.type_params.iter().zip(explicit_type_args.iter()) {
                if def.const_params.contains(tp) {
                    let ok = matches!(
                        arg,
                        Type::ConstInt(_) | Type::TypeVar(_) | Type::Any | Type::Error
                    );
                    if !ok {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: format!(
                                "compile-time integer const argument for '{struct_name}::{tp}'"
                            ),
                            got: arg.name(),
                            span: Some(span),
                        }]);
                    }
                } else if matches!(arg, Type::ConstInt(_)) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!(
                            "type argument (not integer constant) for '{struct_name}::{tp}'"
                        ),
                        got: arg.name(),
                        span: Some(span),
                    }]);
                }
            }
            explicit_type_args.to_vec()
        };
        let mut slots: Vec<Option<&MirWitness>> = vec![None; def.fields.len()];
        for (i, arg) in args.iter().enumerate() {
            slots[i] = Some(arg);
        }
        for (k, v) in named_args {
            let Some(pos) = def.fields.iter().position(|f| &f.name == k) else {
                return Err(vec![TypeError::UnificationFailure {
                    expected: format!("known field name on struct '{struct_name}'"),
                    got: format!("unknown field '{k}'"),
                    span: Some(v.span),
                }]);
            };
            if slots[pos].is_some() {
                return Err(vec![TypeError::UnificationFailure {
                    expected: format!("single value for field '{struct_name}.{k}'"),
                    got: format!("duplicate argument for field '{k}'"),
                    span: Some(v.span),
                }]);
            }
            slots[pos] = Some(v);
        }

        let mut acc_row = EffectRow::Empty;
        for (i, f) in def.fields.iter().enumerate() {
            match slots[i] {
                Some(arg_w) => {
                    let (arg_ty, arg_row) = self.infer_recover(arg_w);
                    acc_row = self.merge_rows(acc_row, arg_row, span);
                    let expected_ty =
                        substitute_named_generics(&f.ty, &def.type_params, &fresh_gens);
                    if !matches!(arg_ty, Type::Error) {
                        let c = SpannedConstraint::new(
                            Constraint::Subtype {
                                sub: Box::new(arg_ty),
                                sup: Box::new(expected_ty),
                            },
                            arg_w.span,
                        )
                        .with_hint(format!(
                            "struct field '{struct_name}.{}' type mismatch",
                            f.name
                        ));
                        match unify::solve_spanned(&c, &self.subst) {
                            Ok(s) => self.subst = s,
                            Err(_) => self.constraints.push(c),
                        }
                    }
                }
                None => {
                    if !f.has_default {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: format!(
                                "value for required field '{struct_name}.{}'",
                                f.name
                            ),
                            got: format!("missing required field '{}'", f.name),
                            span: Some(span),
                        }]);
                    }
                }
            }
        }
        let resolved_gens: Vec<Type> = fresh_gens.iter().map(|g| self.subst.apply(g)).collect();
        Ok((
            Type::Concrete {
                name: struct_name.to_string(),
                generics: resolved_gens,
                traits: Vec::new(),
            },
            acc_row,
        ))
    }

    /// Infer field access, reflection, or method dispatch on a `Type::Concrete` receiver
    /// (including `__StructType__::`, `__EnumType__::`, nominal struct instances, and enum instances).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn infer_concrete_receiver_member(
        &mut self,
        recv_ty: &Type,
        name: &str,
        generics: &[Type],
        method: &str,
        args: &[MirWitness],
        named_args: &[(String, MirWitness)],
        acc_row: EffectRow,
        span: Span,
    ) -> Option<Result<(Type, EffectRow), Vec<TypeError>>> {
        if let Some(sname) = name.strip_prefix(STRUCT_TYPE_PREFIX) {
            return Some(
                self.try_infer_nominal_or_assoc_call(
                    &format!("{sname}::{method}"),
                    args,
                    named_args,
                    span,
                )
                .unwrap_or_else(|| {
                    Err(vec![TypeError::UnificationFailure {
                        expected: format!("known method on struct type '{sname}'"),
                        got: format!("{sname}.{method}"),
                        span: Some(span),
                    }])
                }),
            );
        }
        if let Some(ename) = name.strip_prefix(ENUM_TYPE_PREFIX) {
            return Some(
                self.try_infer_nominal_or_assoc_call(
                    &format!("{ename}::{method}"),
                    args,
                    named_args,
                    span,
                )
                .unwrap_or_else(|| {
                    Err(vec![TypeError::UnificationFailure {
                        expected: format!("known variant or method on enum '{ename}'"),
                        got: format!("{ename}.{method}"),
                        span: Some(span),
                    }])
                }),
            );
        }

        // 1. Nominal struct field read (`p.x`) when 0 args & 0 named args.
        if let Some(sdef) = self.struct_defs.get(name).cloned() {
            if args.is_empty()
                && named_args.is_empty()
                && let Some(field) = sdef.fields.iter().find(|f| f.name == method)
            {
                let fty = substitute_named_generics(&field.ty, &sdef.type_params, generics);
                return Some(Ok((self.subst.apply(&fty), acc_row)));
            }
            // 2. Inherent or trait impl method on this struct.
            if let Some(res) = self.try_infer_concrete_impl_method(
                recv_ty,
                method,
                args,
                named_args,
                acc_row.clone(),
                span,
            ) {
                return Some(res);
            }
            // 3. Built-in struct instance reflection / JSON projection methods.
            match method {
                "to_dict" if args.is_empty() && named_args.is_empty() => {
                    return Some(Ok((
                        Type::Dict(Box::new(Type::String), Box::new(Type::Any)),
                        acc_row,
                    )));
                }
                "schema" if args.is_empty() && named_args.is_empty() => {
                    return Some(Ok((
                        Type::Dict(Box::new(Type::String), Box::new(Type::Any)),
                        acc_row,
                    )));
                }
                "tags" if args.is_empty() && named_args.is_empty() => {
                    return Some(Ok((
                        Type::Dict(
                            Box::new(Type::String),
                            Box::new(Type::Dict(Box::new(Type::String), Box::new(Type::String))),
                        ),
                        acc_row,
                    )));
                }
                "json" if args.is_empty() && named_args.is_empty() => {
                    return Some(Ok((Type::String, acc_row)));
                }
                "parameters" if args.is_empty() && named_args.is_empty() => {
                    return Some(Ok((Type::List(Box::new(Type::any_tensor())), acc_row)));
                }
                "named_parameters" | "state_dict" => {
                    let mut row = acc_row;
                    let total = args.len() + named_args.len();
                    if total > 1 {
                        self.errors.push(TypeError::ArityMismatch {
                            expected: 1,
                            actual: total,
                            span,
                        });
                    }
                    if let Some(arg) = args.first() {
                        let (t, r) = self.infer_recover(arg);
                        row = self.merge_rows(row, r, arg.span);
                        if !matches!(t, Type::Error | Type::Nil) {
                            self.constraints.push(SpannedConstraint::new(
                                Constraint::Subtype {
                                    sub: Box::new(t),
                                    sup: Box::new(Type::String),
                                },
                                arg.span,
                            ));
                        }
                    }
                    for (k, arg) in named_args {
                        let (t, r) = self.infer_recover(arg);
                        row = self.merge_rows(row, r, arg.span);
                        if k != "prefix" {
                            self.errors.push(TypeError::UnificationFailure {
                                expected: format!("named parameter 'prefix' on {method}()"),
                                got: format!("unknown keyword argument '{k}'"),
                                span: Some(arg.span),
                            });
                        } else if !matches!(t, Type::Error | Type::Nil) {
                            self.constraints.push(SpannedConstraint::new(
                                Constraint::Subtype {
                                    sub: Box::new(t),
                                    sup: Box::new(Type::String),
                                },
                                arg.span,
                            ));
                        }
                    }
                    return Some(Ok((
                        Type::Dict(Box::new(Type::String), Box::new(Type::any_tensor())),
                        row,
                    )));
                }
                "load_state_dict" => {
                    let mut row = acc_row;
                    let total = args.len() + named_args.len();
                    if total == 0 || total > 2 {
                        self.errors.push(TypeError::ArityMismatch {
                            expected: 1,
                            actual: total,
                            span,
                        });
                    }
                    if let Some(state_arg) = args.first() {
                        let (t, r) = self.infer_recover(state_arg);
                        row = self.merge_rows(row, r, state_arg.span);
                        if !matches!(t, Type::Error) {
                            self.constraints.push(SpannedConstraint::new(
                                Constraint::Subtype {
                                    sub: Box::new(t),
                                    sup: Box::new(Type::Dict(
                                        Box::new(Type::String),
                                        Box::new(Type::Any),
                                    )),
                                },
                                state_arg.span,
                            ));
                        }
                    }
                    if let Some(strict_arg) = args.get(1) {
                        let (t, r) = self.infer_recover(strict_arg);
                        row = self.merge_rows(row, r, strict_arg.span);
                        if !matches!(t, Type::Error) {
                            self.constraints.push(SpannedConstraint::new(
                                Constraint::Subtype {
                                    sub: Box::new(t),
                                    sup: Box::new(Type::Bool),
                                },
                                strict_arg.span,
                            ));
                        }
                    }
                    for (k, arg) in named_args {
                        let (t, r) = self.infer_recover(arg);
                        row = self.merge_rows(row, r, arg.span);
                        match k.as_str() {
                            "state" | "state_dict" => {
                                if !matches!(t, Type::Error) {
                                    self.constraints.push(SpannedConstraint::new(
                                        Constraint::Subtype {
                                            sub: Box::new(t),
                                            sup: Box::new(Type::Dict(
                                                Box::new(Type::String),
                                                Box::new(Type::Any),
                                            )),
                                        },
                                        arg.span,
                                    ));
                                }
                            }
                            "strict" => {
                                if !matches!(t, Type::Error) {
                                    self.constraints.push(SpannedConstraint::new(
                                        Constraint::Subtype {
                                            sub: Box::new(t),
                                            sup: Box::new(Type::Bool),
                                        },
                                        arg.span,
                                    ));
                                }
                            }
                            _ => {
                                self.errors.push(TypeError::UnificationFailure {
                                    expected:
                                        "named parameter 'state' or 'strict' on load_state_dict()"
                                            .to_string(),
                                    got: format!("unknown keyword argument '{k}'"),
                                    span: Some(arg.span),
                                });
                            }
                        }
                    }
                    return Some(Ok((recv_ty.clone(), row)));
                }
                "to_device" => {
                    let mut row = acc_row;
                    if args.len() != 1 || !named_args.is_empty() {
                        self.errors.push(TypeError::ArityMismatch {
                            expected: 1,
                            actual: args.len() + named_args.len(),
                            span,
                        });
                    }
                    if let Some(dev_arg) = args.first() {
                        let (t, r) = self.infer_recover(dev_arg);
                        row = self.merge_rows(row, r, dev_arg.span);
                        if !matches!(t, Type::Error) {
                            self.constraints.push(SpannedConstraint::new(
                                Constraint::Subtype {
                                    sub: Box::new(t),
                                    sup: Box::new(Type::String),
                                },
                                dev_arg.span,
                            ));
                        }
                    }
                    return Some(Ok((recv_ty.clone(), row)));
                }
                _ => {}
            }
            // 4. Callable struct field (`obj.callback(x)`).
            if let Some(field) = sdef.fields.iter().find(|f| f.name == method) {
                let fty = substitute_named_generics(&field.ty, &sdef.type_params, generics);
                let mut arg_vals = Vec::with_capacity(args.len());
                let mut row = acc_row;
                for a in args {
                    let (t, r) = self.infer_recover(a);
                    row = self.merge_rows(row, r, span);
                    arg_vals.push((t, EffectRow::Empty, a.span));
                }
                return Some(Ok(self.apply_curried_call_types(fty, row, arg_vals, span)));
            }
            return Some(Err(vec![TypeError::UnificationFailure {
                expected: format!("field or method on struct '{name}'"),
                got: format!("struct '{name}' has no field or method '{method}'"),
                span: Some(span),
            }]));
        }

        // Enum instance method dispatch.
        if self.enum_defs.contains_key(name) {
            if let Some(res) = self
                .try_infer_concrete_impl_method(recv_ty, method, args, named_args, acc_row, span)
            {
                return Some(res);
            }
            return Some(Err(vec![TypeError::UnificationFailure {
                expected: format!("method on enum '{name}'"),
                got: format!("enum '{name}' has no method '{method}'"),
                span: Some(span),
            }]));
        }

        None
    }

    /// Look up inherent `impl Target`, trait `impl Trait for Target`, or trait default method on a concrete receiver.
    pub(super) fn try_infer_concrete_impl_method(
        &mut self,
        recv_ty: &Type,
        method: &str,
        args: &[MirWitness],
        named_args: &[(String, MirWitness)],
        acc_row: EffectRow,
        span: Span,
    ) -> Option<Result<(Type, EffectRow), Vec<TypeError>>> {
        let target_name = canonical_target_name_of(recv_ty)?;
        let recv_generics: Vec<Type> = match recv_ty {
            Type::Concrete { generics, .. } | Type::Trait { generics, .. } => generics.clone(),
            Type::List(e) | Type::Channel(e) | Type::TaskHandle(e) => vec![(**e).clone()],
            Type::Dict(k, v) => vec![(**k).clone(), (**v).clone()],
            _ => Vec::new(),
        };

        // 1. Inherent impl method
        let inherent = self
            .inherent_impls
            .iter()
            .find(|((tgt, m), sig)| {
                sig.has_self && m == method && target_name_matches(tgt, &target_name)
            })
            .map(|(_, s)| s.clone());
        if let Some(sig) = inherent {
            return Some(self.apply_impl_method_call(
                &sig,
                &recv_generics,
                args,
                named_args,
                acc_row,
                span,
            ));
        }

        // 2. Trait impl method
        let trait_impl = self
            .trait_impl_methods
            .iter()
            .find(|((_, tgt, m), sig)| {
                sig.has_self && m == method && target_name_matches(tgt, &target_name)
            })
            .map(|(_, s)| s.clone());
        if let Some(sig) = trait_impl {
            return Some(self.apply_impl_method_call(
                &sig,
                &recv_generics,
                args,
                named_args,
                acc_row,
                span,
            ));
        }

        // 3. Default method on any trait implemented by `target_name` (or its parent traits)
        let impl_traits: Vec<String> = self
            .trait_impl_pairs
            .iter()
            .filter(|(_, tgt)| target_name_matches(tgt, &target_name))
            .map(|(tr, _)| tr.clone())
            .collect();
        for tr in impl_traits {
            for anc in self.collect_trait_chain(&tr) {
                if let Some(tdef) = self.trait_defs.get(&anc).cloned()
                    && let Some(tm) = tdef
                        .methods
                        .iter()
                        .find(|m| m.name == method && m.has_default)
                {
                    let sig = ImplMethodTypeSig {
                        name: tm.name.clone(),
                        type_params: tm.type_params.clone(),
                        target_type_params: tdef.type_params.clone(),
                        params: tm
                            .params
                            .iter()
                            .map(|(pn, pt, hd)| {
                                (pn.clone(), Self::substitute_self_type(pt, recv_ty), *hd)
                            })
                            .collect(),
                        return_type: Self::substitute_self_type(&tm.return_type, recv_ty),
                        has_self: tm.has_self,
                        effect_row: EffectRow::Empty,
                    };
                    return Some(self.apply_impl_method_call(
                        &sig,
                        &recv_generics,
                        args,
                        named_args,
                        acc_row,
                        span,
                    ));
                }
            }
        }

        None
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn infer_trait_object_method(
        &mut self,
        trait_name: &str,
        generics: &[Type],
        method: &str,
        args: &[MirWitness],
        named_args: &[(String, MirWitness)],
        acc_row: EffectRow,
        span: Span,
    ) -> Option<Result<(Type, EffectRow), Vec<TypeError>>> {
        if !self.trait_defs.contains_key(trait_name) {
            return None;
        }
        for anc in self.collect_trait_chain(trait_name) {
            if let Some(tdef) = self.trait_defs.get(&anc).cloned()
                && let Some(tm) = tdef.methods.iter().find(|m| m.name == method)
            {
                let sig = ImplMethodTypeSig {
                    name: tm.name.clone(),
                    type_params: tm.type_params.clone(),
                    target_type_params: tdef.type_params.clone(),
                    params: tm.params.clone(),
                    return_type: tm.return_type.clone(),
                    has_self: tm.has_self,
                    effect_row: EffectRow::Empty,
                };
                return Some(
                    self.apply_impl_method_call(&sig, generics, args, named_args, acc_row, span),
                );
            }
        }
        Some(Err(vec![TypeError::UnificationFailure {
            expected: format!("method declared on trait '{trait_name}'"),
            got: format!("dyn {trait_name} has no method '{method}'"),
            span: Some(span),
        }]))
    }

    fn apply_impl_method_call(
        &mut self,
        sig: &ImplMethodTypeSig,
        recv_generics: &[Type],
        args: &[MirWitness],
        named_args: &[(String, MirWitness)],
        mut acc_row: EffectRow,
        span: Span,
    ) -> Result<(Type, EffectRow), Vec<TypeError>> {
        let param_spec = FnParamSpec {
            names: sig.params.iter().map(|(n, _, _)| n.clone()).collect(),
            has_default: sig.params.iter().map(|(_, _, hd)| *hd).collect(),
            min_arity: sig.params.iter().filter(|(_, _, hd)| !*hd).count(),
        };
        let aligned = self.align_call_args(&sig.name, &param_spec, args, named_args, span)?;

        // Fresh meta vars for method's own generic type params not bound by receiver.
        let mut extra_tps: Vec<String> = Vec::new();
        let mut extra_vars: Vec<Type> = Vec::new();
        for tp in &sig.type_params {
            if !sig.target_type_params.contains(tp) {
                extra_tps.push(tp.clone());
                extra_vars.push(self.fresh_type_var());
            }
        }
        let subst_sig_ty = |t: &Type| -> Type {
            let t1 = substitute_named_generics(t, &sig.target_type_params, recv_generics);
            substitute_named_generics(&t1, &extra_tps, &extra_vars)
        };

        for (slot_opt, (_, raw_pty, _)) in aligned.into_iter().zip(sig.params.iter()) {
            if let Some((arg_ty, arg_row, arg_span)) = slot_opt {
                acc_row = self.merge_rows(acc_row, arg_row, span);
                let expected_pty = subst_sig_ty(raw_pty);
                if !matches!(arg_ty, Type::Error) && !matches!(expected_pty, Type::Any) {
                    let c = SpannedConstraint::new(
                        Constraint::Subtype {
                            sub: Box::new(arg_ty),
                            sup: Box::new(expected_pty),
                        },
                        arg_span,
                    );
                    match unify::solve_spanned(&c, &self.subst) {
                        Ok(s) => self.subst = s,
                        Err(_) => self.constraints.push(c),
                    }
                }
            }
        }
        acc_row = self.merge_rows(acc_row, sig.effect_row.clone(), span);
        let ret_ty = self.subst.apply(&subst_sig_ty(&sig.return_type));
        Ok((ret_ty, acc_row))
    }

    // ════════════════════════════════════════════════════════════════════════
    // 4. Default Parameters & Named Keyword Argument Resolution
    // ════════════════════════════════════════════════════════════════════════

    /// Align positional + named keyword arguments against a [`FnParamSpec`], validating
    /// unknown keywords, duplicate bindings, excess positional args, and missing required params.
    pub(super) fn align_call_args(
        &mut self,
        callee_label: &str,
        spec: &FnParamSpec,
        args: &[MirWitness],
        named_args: &[(String, MirWitness)],
        span: Span,
    ) -> Result<AlignedCallSlots, Vec<TypeError>> {
        let total = spec.names.len();
        if args.len() > total {
            return Err(vec![TypeError::ArityMismatch {
                expected: total,
                actual: args.len(),
                span,
            }]);
        }
        let mut raw_slots: Vec<Option<&MirWitness>> = vec![None; total];
        for (i, a) in args.iter().enumerate() {
            raw_slots[i] = Some(a);
        }
        for (kw, val_w) in named_args {
            let Some(pos) = spec.names.iter().position(|n| n == kw) else {
                return Err(vec![TypeError::UnificationFailure {
                    expected: format!("known parameter name for '{callee_label}'"),
                    got: format!("unknown keyword argument '{kw}'"),
                    span: Some(val_w.span),
                }]);
            };
            if raw_slots[pos].is_some() {
                return Err(vec![TypeError::UnificationFailure {
                    expected: format!("single argument for parameter '{kw}' of '{callee_label}'"),
                    got: format!("duplicate argument for parameter '{kw}'"),
                    span: Some(val_w.span),
                }]);
            }
            raw_slots[pos] = Some(val_w);
        }
        let mut missing = Vec::new();
        for (i, slot) in raw_slots.iter().enumerate() {
            if slot.is_none() && !spec.has_default[i] {
                missing.push(spec.names[i].clone());
            }
        }
        if !missing.is_empty() {
            let supplied = args.len() + named_args.len();
            if named_args.is_empty() && supplied < spec.min_arity {
                return Err(vec![TypeError::ArityMismatch {
                    expected: spec.min_arity,
                    actual: supplied,
                    span,
                }]);
            }
            return Err(vec![TypeError::UnificationFailure {
                expected: format!(
                    "required parameter(s) {} for '{callee_label}'",
                    missing.join(", ")
                ),
                got: format!("missing required parameter '{}'", missing[0]),
                span: Some(span),
            }]);
        }

        let mut inferred = Vec::with_capacity(total);
        for slot in raw_slots {
            match slot {
                Some(w) => {
                    let (ty, row) = self.infer_recover(w);
                    inferred.push(Some((ty, row, w.span)));
                }
                None => inferred.push(None),
            }
        }
        Ok(inferred)
    }

    /// Apply aligned optional argument slots (with omitted defaulted slots as `None`) across a curried `Arrow`.
    pub(super) fn apply_curried_call_slots(
        &mut self,
        mut callee_ty: Type,
        mut acc_row: EffectRow,
        slots: Vec<Option<(Type, EffectRow, Span)>>,
        span: Span,
    ) -> (Type, EffectRow) {
        if slots.is_empty() {
            return self.apply_curried_call_types(callee_ty, acc_row, Vec::new(), span);
        }
        for slot in slots {
            let resolved = self.subst.apply(&callee_ty);
            let inst = self.instantiate_if_forall(&resolved);
            let resolved = self.subst.apply(&inst);
            match resolved {
                Type::Arrow(param_ty, ret_ty, eff_row) => {
                    if let Some((arg_ty, arg_row, arg_span)) = slot {
                        acc_row = self.merge_rows(acc_row, arg_row, span);
                        if !matches!(arg_ty, Type::Error) {
                            self.constraints.push(SpannedConstraint::new(
                                Constraint::Subtype {
                                    sub: Box::new(arg_ty),
                                    sup: param_ty,
                                },
                                arg_span,
                            ));
                        }
                    }
                    callee_ty = *ret_ty;
                    acc_row = self.merge_rows(acc_row, eff_row, span);
                }
                Type::Any
                | Type::Error
                | Type::Closure
                | Type::Task
                | Type::Partial
                | Type::Compose
                | Type::Builtin => {
                    if let Some((_, arg_row, _)) = slot {
                        acc_row = self.merge_rows(acc_row, arg_row, span);
                    }
                    callee_ty = if matches!(resolved, Type::Error) {
                        Type::Error
                    } else {
                        Type::Any
                    };
                }
                _ => {
                    let (arg_ty, arg_span) = match slot {
                        Some((t, r, s)) => {
                            acc_row = self.merge_rows(acc_row, r, span);
                            (t, s)
                        }
                        None => (self.fresh_type_var(), span),
                    };
                    let fresh_ret = self.fresh_type_var();
                    let fresh_eff = self.fresh_row_var();
                    let expected = Type::Arrow(
                        Box::new(arg_ty),
                        Box::new(fresh_ret.clone()),
                        fresh_eff.clone(),
                    );
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Eq(Box::new(callee_ty), Box::new(expected)),
                        arg_span,
                    ));
                    callee_ty = fresh_ret;
                    acc_row = self.merge_rows(acc_row, fresh_eff, span);
                }
            }
        }
        (callee_ty, acc_row)
    }

    // ════════════════════════════════════════════════════════════════════════
    // 5. Multi-Dimensional Zero-Copy Tensor Slicing (`t[0..4, :, -1]`, `t[..., None]`)
    // ════════════════════════════════════════════════════════════════════════

    pub(super) fn infer_tensor_slice(
        &mut self,
        target: &MirWitness,
        axes: &[WitnessSliceAxis],
        span: Span,
    ) -> Result<(Type, EffectRow), Vec<TypeError>> {
        let (target_ty, mut acc_row) = self.infer_recover(target);
        // Validate every slice expression has an integer-compatible type & reject static step == 0.
        for ax in axes {
            match ax {
                WitnessSliceAxis::Index(w) => {
                    let (ty, row) = self.infer_recover(w);
                    acc_row = self.merge_rows(acc_row, row, span);
                    self.require_slice_index_int(&ty, w.span)?;
                }
                WitnessSliceAxis::Range { start, stop, step } => {
                    for w in [start.as_deref(), stop.as_deref(), step.as_deref()]
                        .into_iter()
                        .flatten()
                    {
                        let (ty, row) = self.infer_recover(w);
                        acc_row = self.merge_rows(acc_row, row, span);
                        self.require_slice_index_int(&ty, w.span)?;
                    }
                    if let Some(step_w) = step.as_deref()
                        && Self::extract_static_int_literal(step_w) == Some(0)
                    {
                        return Err(vec![TypeError::InvalidLiteral {
                            what: "slice step (cannot be 0)".to_string(),
                            value: "0".to_string(),
                            span: Some(step_w.span),
                        }]);
                    }
                }
                WitnessSliceAxis::Ellipsis | WitnessSliceAxis::NewAxis => {}
            }
        }

        let resolved = self.subst.apply(&target_ty);
        match resolved {
            Type::Error => Ok((Type::Error, acc_row)),
            Type::List(_) | Type::String => {
                if axes.len() != 1 || !matches!(axes[0], WitnessSliceAxis::Range { .. }) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!("single range slice on {}", resolved.name()),
                        got: format!("multi-dimensional slice with {} axes", axes.len()),
                        span: Some(span),
                    }]);
                }
                Ok((resolved, acc_row))
            }
            Type::Tensor {
                dtype,
                shape,
                device,
            } => {
                let mut ellipsis_pos: Option<usize> = None;
                let mut consumed_axes = 0usize;
                for (i, ax) in axes.iter().enumerate() {
                    match ax {
                        WitnessSliceAxis::Ellipsis => {
                            if ellipsis_pos.is_some() {
                                return Err(vec![TypeError::UnificationFailure {
                                    expected: "at most one ellipsis ('...') in tensor slice"
                                        .to_string(),
                                    got: "multiple ellipses".to_string(),
                                    span: Some(span),
                                }]);
                            }
                            ellipsis_pos = Some(i);
                        }
                        WitnessSliceAxis::NewAxis => {}
                        WitnessSliceAxis::Index(_) | WitnessSliceAxis::Range { .. } => {
                            consumed_axes += 1;
                        }
                    }
                }
                let cur_shape = self.subst.apply_shape(&shape);
                let out_shape = match cur_shape {
                    ShapeType::Unranked(_) => ShapeType::Unranked(None),
                    ShapeType::Ranked(dims) => {
                        if consumed_axes > dims.len() {
                            return Err(vec![TypeError::UnificationFailure {
                                expected: format!(
                                    "at most {} slice axes for rank-{} tensor ({})",
                                    dims.len(),
                                    dims.len(),
                                    ShapeType::Ranked(dims)
                                ),
                                got: format!("{consumed_axes} slice axes"),
                                span: Some(span),
                            }]);
                        }
                        let expand_count = if ellipsis_pos.is_some() {
                            dims.len() - consumed_axes
                        } else {
                            0
                        };
                        let mut out_dims: Vec<DimExpr> = Vec::new();
                        let mut src_axis = 0usize;
                        for ax in axes {
                            match ax {
                                WitnessSliceAxis::Ellipsis => {
                                    for _ in 0..expand_count {
                                        out_dims.push(dims[src_axis].clone());
                                        src_axis += 1;
                                    }
                                }
                                WitnessSliceAxis::NewAxis => {
                                    out_dims.push(DimExpr::Known(1));
                                }
                                WitnessSliceAxis::Index(idx_w) => {
                                    let dim = &dims[src_axis];
                                    if let (DimExpr::Known(n), Some(k)) =
                                        (dim, Self::extract_static_int_literal(idx_w))
                                    {
                                        let ni = *n as i64;
                                        if k < -ni || k >= ni {
                                            return Err(vec![TypeError::UnificationFailure {
                                                expected: format!(
                                                    "slice index in range -{}..{} for axis {} of size {}",
                                                    n, n, src_axis, n
                                                ),
                                                got: format!("index {k} out of bounds"),
                                                span: Some(idx_w.span),
                                            }]);
                                        }
                                    }
                                    src_axis += 1;
                                }
                                WitnessSliceAxis::Range { start, stop, step } => {
                                    let dim = &dims[src_axis];
                                    let s_lit = match start.as_deref() {
                                        Some(w) => Self::extract_static_int_literal(w).map(Some),
                                        None => Some(None),
                                    };
                                    let e_lit = match stop.as_deref() {
                                        Some(w) => Self::extract_static_int_literal(w).map(Some),
                                        None => Some(None),
                                    };
                                    let st_lit = match step.as_deref() {
                                        Some(w) => Self::extract_static_int_literal(w),
                                        None => Some(1),
                                    };
                                    let out_d = match (dim, s_lit, e_lit, st_lit) {
                                        (DimExpr::Known(n), Some(s), Some(e), Some(st)) => {
                                            DimExpr::Known(static_slice_axis_len(*n, s, e, st))
                                        }
                                        (d, Some(None), Some(None), Some(1)) => d.clone(),
                                        _ => {
                                            let id = self.fresh_counter;
                                            self.fresh_counter += 1;
                                            DimExpr::Var(id)
                                        }
                                    };
                                    out_dims.push(out_d);
                                    src_axis += 1;
                                }
                            }
                        }
                        while src_axis < dims.len() {
                            out_dims.push(dims[src_axis].clone());
                            src_axis += 1;
                        }
                        if out_dims.is_empty() {
                            return Ok((self.subst.apply(&dtype), acc_row));
                        }
                        ShapeType::Ranked(out_dims)
                    }
                };
                Ok((
                    Type::Tensor {
                        dtype: Box::new(self.subst.apply(&dtype)),
                        shape: out_shape,
                        device: self.subst.apply_device(&device),
                    },
                    acc_row,
                ))
            }
            Type::TypeVar(_) | Type::Any => Ok((self.fresh_type_var(), acc_row)),
            other => Err(vec![TypeError::UnificationFailure {
                expected: "Tensor, list, or string for slicing".to_string(),
                got: other.name(),
                span: Some(span),
            }]),
        }
    }

    fn require_slice_index_int(&mut self, ty: &Type, span: Span) -> Result<(), Vec<TypeError>> {
        let resolved = self.subst.apply(ty);
        match resolved {
            Type::Error | Type::Int | Type::Any | Type::TypeVar(_) => Ok(()),
            Type::Scalar(dt) if dt.is_int() => Ok(()),
            other => Err(vec![TypeError::UnificationFailure {
                expected: "integer slice index".to_string(),
                got: other.name(),
                span: Some(span),
            }]),
        }
    }

    // ════════════════════════════════════════════════════════════════════════
    // 6. Autograd Primitives (`grad`, `value_and_grad`, `no_grad`)
    // ════════════════════════════════════════════════════════════════════════

    pub(super) fn infer_no_grad(
        &mut self,
        body: &MirWitness,
        _span: Span,
    ) -> Result<(Type, EffectRow), Vec<TypeError>> {
        self.no_grad_depth += 1;
        let (body_ty, body_row) = self.infer_recover(body);
        self.no_grad_depth -= 1;
        Ok((body_ty, body_row.remove(ambient::AUTOGRAD_LABEL)))
    }

    pub(super) fn infer_grad_builtin(
        &mut self,
        return_value: bool,
        args: &[MirWitness],
        named_args: &[(String, MirWitness)],
        span: Span,
    ) -> Result<(Type, EffectRow), Vec<TypeError>> {
        let op_name = if return_value {
            "value_and_grad"
        } else {
            "grad"
        };
        let spec = FnParamSpec {
            names: vec!["f".to_string(), "argnums".to_string()],
            has_default: vec![false, true],
            min_arity: 1,
        };
        let mut raw_slots: Vec<Option<&MirWitness>> = vec![None; 2];
        if args.len() > 2 {
            return Err(vec![TypeError::ArityMismatch {
                expected: 2,
                actual: args.len(),
                span,
            }]);
        }
        for (i, a) in args.iter().enumerate() {
            raw_slots[i] = Some(a);
        }
        for (k, v) in named_args {
            let Some(pos) = spec.names.iter().position(|n| n == k) else {
                return Err(vec![TypeError::UnificationFailure {
                    expected: format!("parameter 'f' or 'argnums' for {op_name}"),
                    got: format!("unknown keyword argument '{k}'"),
                    span: Some(v.span),
                }]);
            };
            if raw_slots[pos].is_some() {
                return Err(vec![TypeError::UnificationFailure {
                    expected: format!("single argument for '{k}'"),
                    got: format!("duplicate argument '{k}'"),
                    span: Some(v.span),
                }]);
            }
            raw_slots[pos] = Some(v);
        }
        let Some(fn_w) = raw_slots[0] else {
            return Err(vec![TypeError::ArityMismatch {
                expected: 1,
                actual: 0,
                span,
            }]);
        };

        let (fn_ty, mut acc_row) = self.infer_recover(fn_w);
        let argnums_w = raw_slots[1];
        let parsed_argnums: Option<Vec<usize>> = if let Some(aw) = argnums_w {
            let (aty, arow) = self.infer_recover(aw);
            acc_row = self.merge_rows(acc_row, arow, span);
            let resolved_aty = self.subst.apply(&aty);
            if !matches!(
                resolved_aty,
                Type::Int | Type::List(_) | Type::Nil | Type::Any | Type::TypeVar(_) | Type::Error
            ) {
                return Err(vec![TypeError::UnificationFailure {
                    expected: "int or list<int> for argnums".to_string(),
                    got: resolved_aty.name(),
                    span: Some(aw.span),
                }]);
            }
            if let Some(k) = Self::extract_static_int_literal(aw) {
                if k < 0 {
                    return Err(vec![TypeError::InvalidLiteral {
                        what: "grad argnums index".to_string(),
                        value: k.to_string(),
                        span: Some(aw.span),
                    }]);
                }
                Some(vec![k as usize])
            } else if let WitnessKind::List(items) = &aw.kind {
                let mut idxs = Vec::with_capacity(items.len());
                let mut all_static = true;
                for it in items {
                    if let Some(k) = Self::extract_static_int_literal(it)
                        && k >= 0
                    {
                        idxs.push(k as usize);
                    } else {
                        all_static = false;
                        break;
                    }
                }
                if all_static { Some(idxs) } else { None }
            } else {
                None
            }
        } else {
            Some(vec![0])
        };

        // Resolve the target callable's effect row (from Arrow, fn_effect_rows, or closure_rows)
        // and STATICALLY BAN in-place tensor mutation (`tensor_mutate`) inside differentiated functions!
        let mut fn_eff_row = EffectRow::Empty;
        if let WitnessKind::Variable(fname) = &fn_w.kind
            && let Some(r) = self.fn_effect_rows.get(fname)
        {
            fn_eff_row = Self::union_effect_rows(&fn_eff_row, r);
        }
        if let Some(r) = self.closure_rows.get(&fn_w.span) {
            fn_eff_row = Self::union_effect_rows(&fn_eff_row, r);
        }

        let resolved_fn = self.subst.apply(&fn_ty);
        let inst_fn = self.instantiate_if_forall(&resolved_fn);
        let resolved_fn = self.subst.apply(&inst_fn);

        if matches!(resolved_fn, Type::Error) {
            return Ok((Type::Error, acc_row));
        }

        // Unpack curried arrow `(p0 -> p1 -> ... -> ret)`
        let mut param_types: Vec<Type> = Vec::new();
        let mut cur = &resolved_fn;
        while let Type::Arrow(inp, out, row) = cur {
            fn_eff_row = Self::union_effect_rows(&fn_eff_row, row);
            if matches!(inp.as_ref(), Type::Tuple(es) if es.is_empty()) {
                cur = out;
                break;
            }
            param_types.push(self.subst.apply(inp));
            cur = out;
        }
        let ret_ty = self.subst.apply(cur);

        if fn_eff_row.labels().contains(&ambient::TENSOR_MUTATE_LABEL) {
            return Err(vec![TypeError::UnificationFailure {
                expected: format!(
                    "pure/out-of-place differentiable function for {op_name}(f) (no in-place tensor mutation)"
                ),
                got: format!(
                    "function performs in-place tensor mutation ('{}' effect), which invalidates reverse-mode autograd tape",
                    ambient::TENSOR_MUTATE_LABEL
                ),
                span: Some(fn_w.span),
            }]);
        }

        if param_types.is_empty() {
            if matches!(
                resolved_fn,
                Type::Any | Type::TypeVar(_) | Type::Closure | Type::Task | Type::Builtin
            ) {
                return Ok((Type::Closure, acc_row));
            }
            return Err(vec![TypeError::NotAClosure {
                found: resolved_fn.name(),
                span: fn_w.span,
            }]);
        }

        // Validate scalar differentiable output type.
        if !is_scalar_grad_output(&ret_ty) {
            return Err(vec![TypeError::UnificationFailure {
                expected: format!("scalar float or 0-D tensor return type for {op_name}(f)"),
                got: ret_ty.name(),
                span: Some(fn_w.span),
            }]);
        }

        // Validate differentiated input parameter types & compute exact gradient return type.
        let grad_out_ty = if let Some(idxs) = &parsed_argnums {
            if idxs.is_empty() {
                return Err(vec![TypeError::InvalidLiteral {
                    what: "grad argnums (must not be empty)".to_string(),
                    value: "[]".to_string(),
                    span: argnums_w.map(|w| w.span).or(Some(span)),
                }]);
            }
            let mut diff_tys = Vec::with_capacity(idxs.len());
            for &idx in idxs {
                if idx >= param_types.len() {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!(
                            "argnums index < {} (parameter count)",
                            param_types.len()
                        ),
                        got: format!("argnum {idx}"),
                        span: argnums_w.map(|w| w.span).or(Some(span)),
                    }]);
                }
                let pty = &param_types[idx];
                if !is_float_differentiable_type(pty) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!(
                            "differentiable floating-point scalar or Tensor for parameter #{idx} of {op_name}(f)"
                        ),
                        got: pty.name(),
                        span: Some(fn_w.span),
                    }]);
                }
                diff_tys.push(pty.clone());
            }
            if diff_tys.len() == 1 {
                diff_tys.pop().expect("len == 1")
            } else if diff_tys.windows(2).all(|w| w[0] == w[1]) {
                Type::List(Box::new(diff_tys[0].clone()))
            } else {
                Type::List(Box::new(Type::Any))
            }
        } else {
            Type::Any
        };

        let final_ret = if return_value {
            if ret_ty == grad_out_ty {
                Type::List(Box::new(ret_ty))
            } else {
                Type::List(Box::new(Type::Any))
            }
        } else {
            grad_out_ty
        };

        let mut grad_eff = fn_eff_row;
        if self.no_grad_depth == 0 {
            grad_eff.extend(ambient::AUTOGRAD_LABEL);
        }
        let grad_fn_ty = Type::curried_arrow(&param_types, final_ret, &grad_eff);
        Ok((grad_fn_ty, acc_row))
    }

    /// Wave 4: Static HM check for forward-mode `jvp(f, primals, tangents)`.
    pub(super) fn infer_jvp_builtin(
        &mut self,
        args: &[MirWitness],
        named_args: &[(String, MirWitness)],
        span: Span,
    ) -> Result<(Type, EffectRow), Vec<TypeError>> {
        let mut raw_slots: Vec<Option<&MirWitness>> = vec![None; 3];
        if args.len() > 3 {
            return Err(vec![TypeError::ArityMismatch {
                expected: 3,
                actual: args.len(),
                span,
            }]);
        }
        for (i, a) in args.iter().enumerate() {
            raw_slots[i] = Some(a);
        }
        for (k, v) in named_args {
            let pos = match k.as_str() {
                "f" | "fn" | "func" => 0,
                "primals" | "primal" | "x" => 1,
                "tangents" | "tangent" | "v" => 2,
                _ => {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: "parameter 'f', 'primals', or 'tangents' for jvp".to_string(),
                        got: format!("unknown keyword argument '{k}'"),
                        span: Some(v.span),
                    }]);
                }
            };
            if raw_slots[pos].is_some() {
                return Err(vec![TypeError::UnificationFailure {
                    expected: format!("single argument for '{k}'"),
                    got: format!("duplicate argument '{k}'"),
                    span: Some(v.span),
                }]);
            }
            raw_slots[pos] = Some(v);
        }
        let (Some(fn_w), Some(primal_w), Some(tangent_w)) =
            (raw_slots[0], raw_slots[1], raw_slots[2])
        else {
            return Err(vec![TypeError::ArityMismatch {
                expected: 3,
                actual: args.len() + named_args.len(),
                span,
            }]);
        };

        let (fn_ty, mut acc_row) = self.infer_recover(fn_w);

        let mut fn_eff_row = EffectRow::Empty;
        if let WitnessKind::Variable(fname) = &fn_w.kind
            && let Some(r) = self.fn_effect_rows.get(fname)
        {
            fn_eff_row = Self::union_effect_rows(&fn_eff_row, r);
        }
        if let Some(r) = self.closure_rows.get(&fn_w.span) {
            fn_eff_row = Self::union_effect_rows(&fn_eff_row, r);
        }

        let resolved_fn = self.subst.apply(&fn_ty);
        let inst_fn = self.instantiate_if_forall(&resolved_fn);
        let resolved_fn = self.subst.apply(&inst_fn);
        if matches!(resolved_fn, Type::Error) {
            return Ok((Type::Error, acc_row));
        }

        let mut param_types: Vec<Type> = Vec::new();
        let mut cur = &resolved_fn;
        while let Type::Arrow(inp, out, row) = cur {
            fn_eff_row = Self::union_effect_rows(&fn_eff_row, row);
            if matches!(inp.as_ref(), Type::Tuple(es) if es.is_empty()) {
                cur = out;
                break;
            }
            param_types.push(self.subst.apply(inp));
            cur = out;
        }
        let ret_ty = self.subst.apply(cur);

        if fn_eff_row.labels().contains(&ambient::TENSOR_MUTATE_LABEL) {
            return Err(vec![TypeError::UnificationFailure {
                expected: "pure/out-of-place differentiable function for jvp(f, ...)".to_string(),
                got: format!(
                    "function performs in-place tensor mutation ('{}' effect)",
                    ambient::TENSOR_MUTATE_LABEL
                ),
                span: Some(fn_w.span),
            }]);
        }

        if let (WitnessKind::List(p_items), WitnessKind::List(t_items)) =
            (&primal_w.kind, &tangent_w.kind)
        {
            if p_items.len() != t_items.len() || p_items.is_empty() {
                return Err(vec![TypeError::ArityMismatch {
                    expected: p_items.len(),
                    actual: t_items.len(),
                    span,
                }]);
            }
            if !param_types.is_empty() && param_types.len() != p_items.len() {
                return Err(vec![TypeError::ArityMismatch {
                    expected: param_types.len(),
                    actual: p_items.len(),
                    span,
                }]);
            }
            for (idx, (pw, tw)) in p_items.iter().zip(t_items.iter()).enumerate() {
                let (pty, pr) = self.infer_recover(pw);
                let (tty, tr) = self.infer_recover(tw);
                acc_row = self.merge_rows(acc_row, pr, span);
                acc_row = self.merge_rows(acc_row, tr, span);
                let rp = self.subst.apply(&pty);
                let rt = self.subst.apply(&tty);
                if !matches!(rp, Type::Error | Type::Any | Type::TypeVar(_))
                    && !is_float_differentiable_type(&rp)
                {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!("floating-point scalar or Tensor primal #{idx} for jvp"),
                        got: rp.name(),
                        span: Some(pw.span),
                    }]);
                }
                if !matches!(rt, Type::Error | Type::Any | Type::TypeVar(_))
                    && !is_float_differentiable_type(&rt)
                {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!("floating-point scalar or Tensor tangent #{idx} for jvp"),
                        got: rt.name(),
                        span: Some(tw.span),
                    }]);
                }
                if !matches!(rp, Type::Error) && !matches!(rt, Type::Error) {
                    self.constraints.push(
                        SpannedConstraint::new(
                            Constraint::Eq(Box::new(rp.clone()), Box::new(rt)),
                            tw.span,
                        )
                        .with_hint(
                            "jvp primal and tangent must have matching dtype, shape, and device",
                        ),
                    );
                    if let Some(expected_p) = param_types.get(idx) {
                        self.constraints.push(SpannedConstraint::new(
                            Constraint::Subtype {
                                sub: Box::new(rp),
                                sup: Box::new(expected_p.clone()),
                            },
                            pw.span,
                        ));
                    }
                }
            }
        } else {
            let (primal_ty, p_row) = self.infer_recover(primal_w);
            let (tangent_ty, t_row) = self.infer_recover(tangent_w);
            acc_row = self.merge_rows(acc_row, p_row, span);
            acc_row = self.merge_rows(acc_row, t_row, span);

            let rp = self.subst.apply(&primal_ty);
            let rt = self.subst.apply(&tangent_ty);
            if !matches!(
                rp,
                Type::Error | Type::List(_) | Type::Any | Type::TypeVar(_)
            ) && !is_float_differentiable_type(&rp)
            {
                return Err(vec![TypeError::UnificationFailure {
                    expected: "floating-point scalar or Tensor primal for jvp".to_string(),
                    got: rp.name(),
                    span: Some(primal_w.span),
                }]);
            }
            if !matches!(
                rt,
                Type::Error | Type::List(_) | Type::Any | Type::TypeVar(_)
            ) && !is_float_differentiable_type(&rt)
            {
                return Err(vec![TypeError::UnificationFailure {
                    expected: "floating-point scalar or Tensor tangent for jvp".to_string(),
                    got: rt.name(),
                    span: Some(tangent_w.span),
                }]);
            }
            if !matches!(rp, Type::Error)
                && !matches!(rt, Type::Error)
                && !matches!(rp, Type::List(_))
                && !matches!(rt, Type::List(_))
            {
                self.constraints.push(
                    SpannedConstraint::new(
                        Constraint::Eq(Box::new(rp.clone()), Box::new(rt)),
                        tangent_w.span,
                    )
                    .with_hint(
                        "jvp primal and tangent must have matching dtype, shape, and device",
                    ),
                );
                if param_types.len() == 1 {
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Subtype {
                            sub: Box::new(rp),
                            sup: Box::new(param_types[0].clone()),
                        },
                        primal_w.span,
                    ));
                }
            }
        }

        let elem_ty = if param_types.is_empty() {
            Type::Any
        } else {
            ret_ty
        };
        Ok((Type::List(Box::new(elem_ty)), acc_row))
    }

    /// Wave 4 / P6-2: Static HM check for `trace_dag(f, example_inputs...)` and
    /// `trace_value_and_grad_dag(f, params, extra_inputs...)`.
    pub(super) fn infer_trace_dag_builtin(
        &mut self,
        fn_name: &str,
        args: &[MirWitness],
        named_args: &[(String, MirWitness)],
        span: Span,
    ) -> Result<(Type, EffectRow), Vec<TypeError>> {
        if !named_args.is_empty() {
            return Err(vec![TypeError::UnificationFailure {
                expected: format!("positional arguments for {fn_name}(f, inputs...)"),
                got: format!("keyword argument '{}'", named_args[0].0),
                span: Some(named_args[0].1.span),
            }]);
        }
        if args.len() < 2 {
            return Err(vec![TypeError::ArityMismatch {
                expected: 2,
                actual: args.len(),
                span,
            }]);
        }
        let fn_w = &args[0];
        let (fn_ty, mut acc_row) = self.infer_recover(fn_w);
        for inp_w in &args[1..] {
            let (_, r) = self.infer_recover(inp_w);
            acc_row = self.merge_rows(acc_row, r, span);
        }

        let mut fn_eff_row = EffectRow::Empty;
        if let WitnessKind::Variable(fname) = &fn_w.kind
            && let Some(r) = self.fn_effect_rows.get(fname)
        {
            fn_eff_row = Self::union_effect_rows(&fn_eff_row, r);
        }
        if let Some(r) = self.closure_rows.get(&fn_w.span) {
            fn_eff_row = Self::union_effect_rows(&fn_eff_row, r);
        }
        let resolved_fn = self.subst.apply(&fn_ty);
        let mut cur = &resolved_fn;
        while let Type::Arrow(_, out, row) = cur {
            fn_eff_row = Self::union_effect_rows(&fn_eff_row, row);
            cur = out;
        }
        if fn_eff_row.labels().contains(&ambient::TENSOR_MUTATE_LABEL) {
            return Err(vec![TypeError::UnificationFailure {
                expected: format!("pure/out-of-place tensor function for {fn_name}(f, ...)"),
                got: format!(
                    "function performs in-place tensor mutation ('{}' effect)",
                    ambient::TENSOR_MUTATE_LABEL
                ),
                span: Some(fn_w.span),
            }]);
        }
        Ok((Type::ComputeDag, acc_row))
    }

    /// Static HM check for `try_call(f, args...) -> result<T, string>`.
    pub(super) fn infer_try_call_builtin(
        &mut self,
        args: &[MirWitness],
        named_args: &[(String, MirWitness)],
        span: Span,
    ) -> Result<(Type, EffectRow), Vec<TypeError>> {
        if !named_args.is_empty() {
            return Err(vec![TypeError::UnificationFailure {
                expected: "positional arguments for try_call(f, args...)".to_string(),
                got: format!("keyword argument '{}'", named_args[0].0),
                span: Some(named_args[0].1.span),
            }]);
        }
        if args.is_empty() {
            return Err(vec![TypeError::ArityMismatch {
                expected: 1,
                actual: 0,
                span,
            }]);
        }
        let fn_w = &args[0];
        let (fn_ty, mut acc_row) = self.infer_recover(fn_w);
        let mut arg_tys = Vec::with_capacity(args.len().saturating_sub(1));
        for a in &args[1..] {
            let (at, r) = self.infer_recover(a);
            acc_row = self.merge_rows(acc_row, r, span);
            arg_tys.push((at, a.span));
        }

        let resolved_fn = self.subst.apply(&fn_ty);
        let ok_ty = match resolved_fn {
            Type::Arrow(param_ty, mut out_ty, row) => {
                acc_row = self.merge_rows(acc_row, row, span);
                if arg_tys.is_empty() {
                    if !matches!(&*param_ty, Type::Tuple(es) if es.is_empty())
                        && !matches!(&*param_ty, Type::Any | Type::TypeVar(_))
                    {
                        return Err(vec![TypeError::ArityMismatch {
                            expected: 1,
                            actual: 0,
                            span,
                        }]);
                    }
                    *out_ty
                } else {
                    let first = &arg_tys[0];
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Subtype {
                            sub: Box::new(first.0.clone()),
                            sup: param_ty,
                        },
                        first.1,
                    ));
                    for (extra_ty, extra_span) in &arg_tys[1..] {
                        let applied_out = self.subst.apply(&out_ty);
                        match applied_out {
                            Type::Arrow(next_p, next_out, next_row) => {
                                acc_row = self.merge_rows(acc_row, next_row, span);
                                self.constraints.push(SpannedConstraint::new(
                                    Constraint::Subtype {
                                        sub: Box::new(extra_ty.clone()),
                                        sup: next_p,
                                    },
                                    *extra_span,
                                ));
                                out_ty = next_out;
                            }
                            Type::TypeVar(_) => {
                                let next_ret = self.fresh_type_var();
                                let next_eff = self.fresh_row_var();
                                self.constraints.push(SpannedConstraint::new(
                                    Constraint::Eq(
                                        Box::new(applied_out),
                                        Box::new(Type::Arrow(
                                            Box::new(extra_ty.clone()),
                                            Box::new(next_ret.clone()),
                                            next_eff,
                                        )),
                                    ),
                                    *extra_span,
                                ));
                                *out_ty = next_ret;
                            }
                            _ => break,
                        }
                    }
                    *out_ty
                }
            }
            Type::Any | Type::Task | Type::Closure | Type::Builtin | Type::Error => Type::Any,
            Type::TypeVar(_) => {
                let ret = self.fresh_type_var();
                let eff = self.fresh_row_var();
                let mut arrow = ret.clone();
                if arg_tys.is_empty() {
                    arrow = Type::Arrow(Box::new(Type::Tuple(Vec::new())), Box::new(arrow), eff);
                } else {
                    for (at, _) in arg_tys.iter().rev() {
                        let step_eff = self.fresh_row_var();
                        arrow = Type::Arrow(Box::new(at.clone()), Box::new(arrow), step_eff);
                    }
                }
                self.constraints.push(SpannedConstraint::new(
                    Constraint::Eq(Box::new(resolved_fn), Box::new(arrow)),
                    fn_w.span,
                ));
                ret
            }
            other => {
                return Err(vec![TypeError::NotAClosure {
                    found: other.name(),
                    span: fn_w.span,
                }]);
            }
        };

        Ok((
            Type::Result_(Box::new(ok_ty), Box::new(Type::String)),
            acc_row,
        ))
    }

    // ════════════════════════════════════════════════════════════════════════
    // 7. Nominal Variant & Struct Pattern Checking / Typed Bindings
    // ════════════════════════════════════════════════════════════════════════

    pub(super) fn infer_pattern_variant_or_struct(
        &mut self,
        pattern: &WitnessPattern,
        scrutinee_ty: &Type,
        span: Span,
    ) -> Result<(), Vec<TypeError>> {
        let resolved_scrut = self.subst.apply(scrutinee_ty);
        match pattern {
            WitnessPattern::Variant {
                enum_name,
                variant,
                fields,
            } => {
                let (edef, scrut_gens) = self.resolve_enum_for_pattern(
                    enum_name.as_deref(),
                    variant,
                    &resolved_scrut,
                    span,
                )?;
                let Some((_, payload_tys)) = edef.variants.iter().find(|(v, _)| v == variant)
                else {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!("variant of enum '{}'", edef.name),
                        got: format!("{}::{variant}", edef.name),
                        span: Some(span),
                    }]);
                };
                if fields.len() != payload_tys.len() {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: payload_tys.len(),
                        actual: fields.len(),
                        span,
                    }]);
                }
                for (sub_pat, raw_pty) in fields.iter().zip(payload_tys.iter()) {
                    let sub_ty = substitute_named_generics(raw_pty, &edef.type_params, &scrut_gens);
                    self.infer_pattern(sub_pat, &sub_ty, span)?;
                }
                Ok(())
            }
            WitnessPattern::Struct { name, fields, rest } => {
                let Some(sdef) = self.struct_defs.get(name).cloned() else {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: "declared struct type in pattern".to_string(),
                        got: name.clone(),
                        span: Some(span),
                    }]);
                };
                let scrut_gens = match &resolved_scrut {
                    Type::Concrete {
                        name: sn, generics, ..
                    } => {
                        if sn != name {
                            return Err(vec![TypeError::UnificationFailure {
                                expected: format!("pattern matching type '{sn}'"),
                                got: format!("struct pattern '{name}'"),
                                span: Some(span),
                            }]);
                        }
                        generics.clone()
                    }
                    Type::TypeVar(_) | Type::Any | Type::Error => (0..sdef.type_params.len())
                        .map(|_| self.fresh_type_var())
                        .collect(),
                    other => {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: format!("struct '{name}'"),
                            got: other.name(),
                            span: Some(span),
                        }]);
                    }
                };
                if !rest && fields.len() != sdef.fields.len() {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!(
                            "all {} fields of struct '{name}' (or `..` rest pattern)",
                            sdef.fields.len()
                        ),
                        got: format!("{} field(s) in pattern", fields.len()),
                        span: Some(span),
                    }]);
                }
                for (fname, sub_pat) in fields {
                    let Some(fdef) = sdef.fields.iter().find(|f| &f.name == fname) else {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: format!("field of struct '{name}'"),
                            got: format!("unknown field '{name}.{fname}'"),
                            span: Some(span),
                        }]);
                    };
                    let fty = substitute_named_generics(&fdef.ty, &sdef.type_params, &scrut_gens);
                    self.infer_pattern(sub_pat, &fty, span)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    pub(super) fn bind_pattern_variant_or_struct(
        &mut self,
        pattern: &WitnessPattern,
        scrutinee_ty: &Type,
        span: Span,
    ) -> Result<(), Vec<TypeError>> {
        let resolved_scrut = self.subst.apply(scrutinee_ty);
        match pattern {
            WitnessPattern::Variant {
                enum_name,
                variant,
                fields,
            } => {
                if let Ok((edef, scrut_gens)) = self.resolve_enum_for_pattern(
                    enum_name.as_deref(),
                    variant,
                    &resolved_scrut,
                    span,
                ) && let Some((_, payload_tys)) =
                    edef.variants.iter().find(|(v, _)| v == variant)
                {
                    for (sub_pat, raw_pty) in fields.iter().zip(payload_tys.iter()) {
                        let sub_ty =
                            substitute_named_generics(raw_pty, &edef.type_params, &scrut_gens);
                        self.add_typed_pattern_bindings(sub_pat, &sub_ty, span)?;
                    }
                    return Ok(());
                }
                for sub_pat in fields {
                    let fresh = self.fresh_type_var();
                    self.add_pattern_bindings(sub_pat, &fresh, span)?;
                }
                Ok(())
            }
            WitnessPattern::Struct { name, fields, .. } => {
                if let Some(sdef) = self.struct_defs.get(name).cloned() {
                    let scrut_gens = match &resolved_scrut {
                        Type::Concrete { generics, .. } => generics.clone(),
                        _ => (0..sdef.type_params.len())
                            .map(|_| self.fresh_type_var())
                            .collect(),
                    };
                    for (fname, sub_pat) in fields {
                        let fty = sdef
                            .fields
                            .iter()
                            .find(|f| &f.name == fname)
                            .map(|f| {
                                substitute_named_generics(&f.ty, &sdef.type_params, &scrut_gens)
                            })
                            .unwrap_or_else(|| self.fresh_type_var());
                        self.add_typed_pattern_bindings(sub_pat, &fty, span)?;
                    }
                    return Ok(());
                }
                for (_, sub_pat) in fields {
                    let fresh = self.fresh_type_var();
                    self.add_pattern_bindings(sub_pat, &fresh, span)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn add_typed_pattern_bindings(
        &mut self,
        pattern: &WitnessPattern,
        expected_ty: &Type,
        span: Span,
    ) -> Result<(), Vec<TypeError>> {
        match pattern {
            WitnessPattern::Variable(name) => {
                let ty = self.subst.apply(expected_ty);
                self.record_binding(span, name, &ty);
                self.env.add(name.clone(), ty);
                Ok(())
            }
            other => self.add_pattern_bindings(other, expected_ty, span),
        }
    }

    fn resolve_enum_for_pattern(
        &mut self,
        enum_name: Option<&str>,
        variant: &str,
        scrutinee_ty: &Type,
        span: Span,
    ) -> Result<(super::EnumTypeDef, Vec<Type>), Vec<TypeError>> {
        // Built-in `result<T, E>` (`Ok(v)` / `Err(e)`) pattern destructuring when not shadowed
        // by an explicit user-declared nominal enum.
        let builtin_result_edef = || super::EnumTypeDef {
            name: "result".to_string(),
            type_params: vec!["T".to_string(), "E".to_string()],
            const_params: Vec::new(),
            variants: vec![
                (
                    "Ok".to_string(),
                    vec![Type::TypeVar(TypeVarId::Rigid("T".to_string()))],
                ),
                (
                    "Err".to_string(),
                    vec![Type::TypeVar(TypeVarId::Rigid("E".to_string()))],
                ),
            ],
        };
        if let Type::Result_(ok_ty, err_ty) = scrutinee_ty
            && enum_name.is_none_or(|en| en.eq_ignore_ascii_case("result"))
        {
            return Ok((
                builtin_result_edef(),
                vec![(**ok_ty).clone(), (**err_ty).clone()],
            ));
        }

        let resolved_name = if let Some(en) = enum_name {
            en.to_string()
        } else if let Type::Concrete { name, .. } = scrutinee_ty
            && self.enum_defs.contains_key(name)
        {
            name.clone()
        } else if let Some((ename, _)) = self
            .enum_defs
            .iter()
            .find(|(_, d)| d.variants.iter().any(|(v, _)| v == variant))
        {
            ename.clone()
        } else if matches!(variant, "Ok" | "Err")
            && matches!(scrutinee_ty, Type::TypeVar(_) | Type::Any | Type::Error)
        {
            return Ok((
                builtin_result_edef(),
                vec![self.fresh_type_var(), self.fresh_type_var()],
            ));
        } else {
            return Err(vec![TypeError::UnificationFailure {
                expected: "declared enum variant in pattern".to_string(),
                got: variant.to_string(),
                span: Some(span),
            }]);
        };
        let Some(edef) = self.enum_defs.get(&resolved_name).cloned() else {
            return Err(vec![TypeError::UnificationFailure {
                expected: "declared enum type in pattern".to_string(),
                got: resolved_name,
                span: Some(span),
            }]);
        };
        let scrut_gens = match scrutinee_ty {
            Type::Concrete {
                name: sn, generics, ..
            } => {
                if sn != &resolved_name {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!("pattern matching enum '{sn}'"),
                        got: format!("variant of enum '{resolved_name}'"),
                        span: Some(span),
                    }]);
                }
                generics.clone()
            }
            Type::TypeVar(_) | Type::Any | Type::Error => (0..edef.type_params.len())
                .map(|_| self.fresh_type_var())
                .collect(),
            other => {
                return Err(vec![TypeError::UnificationFailure {
                    expected: format!("enum '{resolved_name}'"),
                    got: other.name(),
                    span: Some(span),
                }]);
            }
        };
        Ok((edef, scrut_gens))
    }

    /// Reorder positional + named keyword arguments for `tensor(...)` and `Tensor::*` constructors.
    pub(super) fn normalize_tensor_ctor_args<'a>(
        ctor: &str,
        args: &'a [MirWitness],
        named_args: &'a [(String, MirWitness)],
        span: Span,
    ) -> Result<Vec<Option<&'a MirWitness>>, Vec<TypeError>> {
        let param_names: &[&str] = match ctor {
            "tensor" | "from_list" => &["data", "dtype", "device", "requires_grad"],
            "zeros" | "ones" | "randn" | "rand" => &["shape", "dtype", "device", "requires_grad"],
            "full" => &["shape", "value", "dtype", "device", "requires_grad"],
            "eye" => &["n", "m", "dtype", "device", "requires_grad"],
            "arange" => &["start", "stop", "step", "dtype", "device", "requires_grad"],
            "from_bytes" => &["bytes", "shape", "dtype", "device", "requires_grad"],
            "from_utf8" => &["text"],
            "cat" => &["tensors", "axis"],
            _ => &[],
        };
        if named_args.is_empty() {
            return Ok(args.iter().map(Some).collect());
        }
        let mut slots: Vec<Option<&'a MirWitness>> = vec![None; param_names.len()];
        if args.len() > param_names.len() {
            return Err(vec![TypeError::ArityMismatch {
                expected: param_names.len(),
                actual: args.len(),
                span,
            }]);
        }
        for (i, a) in args.iter().enumerate() {
            slots[i] = Some(a);
        }
        for (k, v) in named_args {
            let Some(pos) = param_names.iter().position(|n| n == k) else {
                return Err(vec![TypeError::UnificationFailure {
                    expected: format!("valid keyword argument for Tensor::{ctor}"),
                    got: format!("unknown keyword argument '{k}'"),
                    span: Some(v.span),
                }]);
            };
            if slots[pos].is_some() {
                return Err(vec![TypeError::UnificationFailure {
                    expected: format!("single argument for '{k}'"),
                    got: format!("duplicate argument '{k}'"),
                    span: Some(v.span),
                }]);
            }
            slots[pos] = Some(v);
        }
        Ok(slots)
    }

    // ════════════════════════════════════════════════════════════════════════
    // 6. P4-1A: Trait-Driven Operator Overloading (`Add`/`Sub`/`Mul`/.../`Neg`/`Eq`)
    // ════════════════════════════════════════════════════════════════════════

    #[allow(clippy::too_many_arguments)]
    pub(super) fn try_infer_binary_op_overload(
        &mut self,
        op_trait: &str,
        op_method: &str,
        left_ty: &Type,
        right_ty: &Type,
        acc_row: EffectRow,
        span: Span,
    ) -> Option<Result<(Type, EffectRow), Vec<TypeError>>> {
        let (target_name, recv_generics) = match left_ty {
            Type::Concrete { name, generics, .. } => (name.clone(), generics.clone()),
            Type::Trait { name, generics } => (name.clone(), generics.clone()),
            Type::TraitObject {
                trait_name,
                generics,
            } => {
                if self
                    .collect_trait_chain(trait_name)
                    .iter()
                    .any(|t| t == op_trait)
                {
                    let ret = if op_trait == "Eq" {
                        Type::Bool
                    } else {
                        Type::Any
                    };
                    let _ = generics;
                    return Some(Ok((ret, acc_row)));
                }
                return None;
            }
            _ => return None,
        };

        let sig = self
            .trait_impl_methods
            .iter()
            .find(|((tr, tgt, m), s)| {
                tr == op_trait
                    && m == op_method
                    && s.has_self
                    && target_name_matches(tgt, &target_name)
            })
            .map(|(_, s)| s.clone())?;

        if sig.params.len() != 1 {
            return Some(Err(vec![TypeError::ArityMismatch {
                expected: 1,
                actual: sig.params.len(),
                span,
            }]));
        }

        let mut extra_tps: Vec<String> = Vec::new();
        let mut extra_vars: Vec<Type> = Vec::new();
        for tp in &sig.type_params {
            if !sig.target_type_params.contains(tp) {
                extra_tps.push(tp.clone());
                extra_vars.push(self.fresh_type_var());
            }
        }
        let subst_sig_ty = |t: &Type| -> Type {
            let t1 = substitute_named_generics(t, &sig.target_type_params, &recv_generics);
            substitute_named_generics(&t1, &extra_tps, &extra_vars)
        };
        let expected_rhs = subst_sig_ty(&sig.params[0].1);
        if !matches!(right_ty, Type::Error) && !matches!(expected_rhs, Type::Any) {
            let c = SpannedConstraint::new(
                Constraint::Subtype {
                    sub: Box::new(right_ty.clone()),
                    sup: Box::new(expected_rhs),
                },
                span,
            )
            .with_hint(format!(
                "right-hand operand for operator trait '{op_trait}::{op_method}' on '{target_name}'"
            ));
            match unify::solve_spanned(&c, &self.subst) {
                Ok(s) => self.subst = s,
                Err(e) => return Some(Err(vec![e])),
            }
        }
        let out_row = self.merge_rows(acc_row, sig.effect_row.clone(), span);
        let ret_ty = if op_trait == "Eq" {
            Type::Bool
        } else {
            self.subst.apply(&subst_sig_ty(&sig.return_type))
        };
        Some(Ok((ret_ty, out_row)))
    }

    pub(super) fn try_infer_unary_op_overload(
        &mut self,
        recv_ty: &Type,
        op_trait: &str,
        op_method: &str,
        acc_row: EffectRow,
        span: Span,
    ) -> Option<Result<(Type, EffectRow), Vec<TypeError>>> {
        let (target_name, recv_generics) = match recv_ty {
            Type::Concrete { name, generics, .. } => (name.clone(), generics.clone()),
            Type::Trait { name, generics } => (name.clone(), generics.clone()),
            Type::TraitObject { trait_name, .. } => {
                if self
                    .collect_trait_chain(trait_name)
                    .iter()
                    .any(|t| t == op_trait)
                {
                    return Some(Ok((Type::Any, acc_row)));
                }
                return None;
            }
            _ => return None,
        };
        let sig = self
            .trait_impl_methods
            .iter()
            .find(|((tr, tgt, m), s)| {
                tr == op_trait
                    && m == op_method
                    && s.has_self
                    && target_name_matches(tgt, &target_name)
            })
            .map(|(_, s)| s.clone())?;
        if !sig.params.is_empty() {
            return Some(Err(vec![TypeError::ArityMismatch {
                expected: 0,
                actual: sig.params.len(),
                span,
            }]));
        }
        let out_row = self.merge_rows(acc_row, sig.effect_row.clone(), span);
        let ret_ty = self.subst.apply(&substitute_named_generics(
            &sig.return_type,
            &sig.target_type_params,
            &recv_generics,
        ));
        Some(Ok((ret_ty, out_row)))
    }

    // ════════════════════════════════════════════════════════════════════════
    // 7. P4-1B: First-Class Source-Level GPU `kernel` Blocks & `.launch(...)`
    // ════════════════════════════════════════════════════════════════════════

    pub(super) fn infer_kernel_def(
        &mut self,
        name: &str,
        params: &[crate::common::kernel_ir::KernelParam],
        shared: &[crate::common::kernel_ir::KernelSharedArray],
        body: &[MirWitness],
        span: Span,
    ) -> Result<(Type, EffectRow), Vec<TypeError>> {
        if let Err(err) =
            crate::runtime::gpu::kernel_compiler::lower_witness_kernel(name, params, shared, body)
        {
            self.errors.push(TypeError::UnificationFailure {
                expected: format!("valid GPU kernel definition '{name}'"),
                got: err.to_string(),
                span: Some(span),
            });
        }
        let sig: Vec<crate::common::kernel_ir::KernelParamType> =
            params.iter().map(|p| p.ty).collect();
        let kty = Type::GpuKernel(Some(sig));
        self.env.add(name.to_string(), kty.clone());
        self.record_binding(span, name, &kty);
        Ok((Type::Nil, EffectRow::Empty))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn infer_gpu_kernel_method(
        &mut self,
        sig: Option<&[crate::common::kernel_ir::KernelParamType]>,
        method: &str,
        args: &[MirWitness],
        named_args: &[(String, MirWitness)],
        mut acc_row: EffectRow,
        span: Span,
    ) -> Result<(Type, EffectRow), Vec<TypeError>> {
        use crate::common::kernel_ir::KernelParamType;

        if !named_args.is_empty() {
            return Err(vec![TypeError::UnificationFailure {
                expected: format!("positional arguments for gpu_kernel.{method}"),
                got: "keyword arguments".to_string(),
                span: Some(span),
            }]);
        }

        match method {
            "emit_ptx" | "emit_msl" | "name" => {
                if !args.is_empty() {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 0,
                        actual: args.len(),
                        span,
                    }]);
                }
                Ok((Type::String, acc_row))
            }
            "params" => {
                if !args.is_empty() {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 0,
                        actual: args.len(),
                        span,
                    }]);
                }
                Ok((Type::List(Box::new(Type::String)), acc_row))
            }
            "emit_spirv_words" => {
                if args.len() > 1 {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 1,
                        actual: args.len(),
                        span,
                    }]);
                }
                if let Some(a) = args.first() {
                    let (t, r) = self.infer_recover(a);
                    acc_row = self.merge_rows(acc_row, r, a.span);
                    if !matches!(t, Type::Error) {
                        let dim_ty = Type::Union(vec![Type::Int, Type::List(Box::new(Type::Int))]);
                        let c = SpannedConstraint::new(
                            Constraint::Subtype {
                                sub: Box::new(t),
                                sup: Box::new(dim_ty),
                            },
                            a.span,
                        )
                        .with_hint(
                            "gpu_kernel.emit_spirv_words local_size must be int or list<int>",
                        );
                        match unify::solve_spanned(&c, &self.subst) {
                            Ok(s) => self.subst = s,
                            Err(e) => self.errors.push(e),
                        }
                    }
                }
                Ok((Type::List(Box::new(Type::Int)), acc_row))
            }
            "launch" => {
                if args.len() < 2 {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 2 + sig.map_or(0, |s| s.len()),
                        actual: args.len(),
                        span,
                    }]);
                }
                // Check grid & block are `int` or `list<int>` (1D/2D/3D dimensions).
                let dim_ty = Type::Union(vec![Type::Int, Type::List(Box::new(Type::Int))]);
                for (label, arg) in [("grid", &args[0]), ("block", &args[1])] {
                    let (t, r) = self.infer_recover(arg);
                    acc_row = self.merge_rows(acc_row, r, arg.span);
                    if !matches!(t, Type::Error) {
                        let c = SpannedConstraint::new(
                            Constraint::Subtype {
                                sub: Box::new(t),
                                sup: Box::new(dim_ty.clone()),
                            },
                            arg.span,
                        )
                        .with_hint(format!(
                            "kernel.launch '{label}' dimension must be int or list<int>"
                        ));
                        match unify::solve_spanned(&c, &self.subst) {
                            Ok(s) => self.subst = s,
                            Err(e) => self.errors.push(e),
                        }
                    }
                }
                let kargs = &args[2..];
                if let Some(param_tys) = sig {
                    if kargs.len() != param_tys.len() {
                        return Err(vec![TypeError::ArityMismatch {
                            expected: 2 + param_tys.len(),
                            actual: args.len(),
                            span,
                        }]);
                    }
                    // Static Device unification across ALL buffer tensor arguments in one launch!
                    let unified_dev = crate::typeck::DeviceType::Var(self.fresh_type_var_id());
                    for (idx, (pty, arg_w)) in param_tys.iter().zip(kargs.iter()).enumerate() {
                        let (arg_ty, arg_row) = self.infer_recover(arg_w);
                        acc_row = self.merge_rows(acc_row, arg_row, arg_w.span);
                        if matches!(arg_ty, Type::Error) {
                            continue;
                        }
                        let expected_ty = match pty {
                            KernelParamType::BufF32 => Type::Tensor {
                                dtype: Box::new(Type::Scalar(crate::common::DType::F32)),
                                shape: ShapeType::Unranked(None),
                                device: unified_dev.clone(),
                            },
                            KernelParamType::BufF64 => Type::Tensor {
                                dtype: Box::new(Type::Float),
                                shape: ShapeType::Unranked(None),
                                device: unified_dev.clone(),
                            },
                            KernelParamType::BufF16 => Type::Tensor {
                                dtype: Box::new(Type::Scalar(crate::common::DType::F16)),
                                shape: ShapeType::Unranked(None),
                                device: unified_dev.clone(),
                            },
                            KernelParamType::BufBF16 => Type::Tensor {
                                dtype: Box::new(Type::Scalar(crate::common::DType::BF16)),
                                shape: ShapeType::Unranked(None),
                                device: unified_dev.clone(),
                            },
                            KernelParamType::BufU8 => Type::Tensor {
                                dtype: Box::new(Type::Scalar(crate::common::DType::U8)),
                                shape: ShapeType::Unranked(None),
                                device: unified_dev.clone(),
                            },
                            KernelParamType::BufI32 => Type::Tensor {
                                dtype: Box::new(Type::Scalar(crate::common::DType::I32)),
                                shape: ShapeType::Unranked(None),
                                device: unified_dev.clone(),
                            },
                            KernelParamType::F32 => Type::Union(vec![
                                Type::Scalar(crate::common::DType::F32),
                                Type::Float,
                                Type::Int,
                            ]),
                            KernelParamType::F64 => Type::Union(vec![Type::Float, Type::Int]),
                            KernelParamType::I32 => Type::Union(vec![
                                Type::Int,
                                Type::Scalar(crate::common::DType::I32),
                            ]),
                        };
                        let c = SpannedConstraint::new(
                            Constraint::Subtype {
                                sub: Box::new(arg_ty),
                                sup: Box::new(expected_ty),
                            },
                            arg_w.span,
                        )
                        .with_hint(format!(
                            "kernel.launch argument #{} must match '{}' (all buffer tensors must share one device)",
                            idx + 1,
                            pty.canonical_name()
                        ));
                        match unify::solve_spanned(&c, &self.subst) {
                            Ok(s) => self.subst = s,
                            Err(e) => self.errors.push(e),
                        }
                    }
                } else {
                    for arg_w in kargs {
                        let (_, arg_row) = self.infer_recover(arg_w);
                        acc_row = self.merge_rows(acc_row, arg_row, arg_w.span);
                    }
                }
                Ok((Type::Nil, acc_row))
            }
            other => Err(vec![TypeError::UnificationFailure {
                expected: "gpu_kernel method ('launch', 'emit_ptx', 'emit_spirv_words', 'emit_msl', 'name', 'params')".to_string(),
                got: format!("gpu_kernel.{other}"),
                span: Some(span),
            }]),
        }
    }
}
