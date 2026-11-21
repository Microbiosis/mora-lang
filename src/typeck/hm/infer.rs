use super::*;
use crate::mir::hint::TypeHint;
use crate::typeck::{Type, TypeVarId, is_known_type};
use std::collections::HashSet;

fn is_numeric(ty: &Type) -> bool {
    match ty {
        Type::Int | Type::Float => true,
        Type::Union(ms) if !ms.is_empty() => ms.iter().all(is_numeric),
        _ => false,
    }
}

fn promote_numeric(a: &Type, b: &Type) -> Type {
    match (a, b) {
        (Type::Error, _) | (_, Type::Error) => Type::Error,
        (Type::Int, Type::Int) => Type::Int,
        (Type::Float, x) | (x, Type::Float) if is_numeric(x) => Type::Float,
        (x, y) if is_numeric(x) && is_numeric(y) => Type::Union(vec![Type::Int, Type::Float]),
        _ => Type::Float,
    }
}

fn promote_list_elem(a: &Type, b: &Type) -> Type {
    match (a, b) {
        (Type::Error, _) | (_, Type::Error) => Type::Error,
        (Type::TypeVar(TypeVarId::Meta(_)), other) | (other, Type::TypeVar(TypeVarId::Meta(_))) => {
            other.clone()
        }
        _ if is_numeric(a) && is_numeric(b) => promote_numeric(a, b),
        _ => a.clone(),
    }
}

/// Extract (dimensions, scalar_element_type) for a @ matrix-multiply operand.
fn matmul_shape(ty: &Type) -> Option<(usize, Type)> {
    match ty {
        Type::List(inner) => match inner.as_ref() {
            Type::List(elem) => Some((2, (**elem).clone())),
            other => Some((1, other.clone())),
        },
        _ => None,
    }
}

/// ML Value Restriction (Wright 1995): only pure syntactic values may be let-generalized.
fn is_generalizable_value(w: &MirWitness) -> bool {
    match &w.kind {
        WitnessKind::Literal(_)
        | WitnessKind::Variable(_)
        | WitnessKind::Closure { .. }
        | WitnessKind::FnDef { .. } => true,
        WitnessKind::List(elems) => elems.iter().all(is_generalizable_value),
        WitnessKind::Dict(pairs) => pairs.iter().all(|(_, v)| is_generalizable_value(v)),
        _ => false,
    }
}

impl HMInference {
    fn free_in_resolved_env(&self) -> Vec<TypeVarId> {
        self.free_in_resolved_env_except(None)
    }

    fn free_in_resolved_env_except(&self, skip_name: Option<&str>) -> Vec<TypeVarId> {
        let mut vars = Vec::new();
        for (name, binding) in self.env.all_bindings() {
            if skip_name == Some(name.as_str()) {
                continue;
            }
            let applied = self.subst.apply(&binding);
            vars.extend(env::collect_type_vars(&applied));
        }
        vars.sort();
        vars.dedup();
        vars
    }

    /// Solve non-scheme structural constraints generated within [c_start..] and
    /// return generalized ForAll + any deferred Numeric scheme constraints.
    fn generalize_pure_binding(&mut self, name: Option<&str>, ty: &Type, c_start: usize) -> Type {
        let drained: Vec<SpannedConstraint> = self.constraints.drain(c_start..).collect();
        let mut numerics: Vec<SpannedConstraint> = Vec::new();
        for sc in drained {
            match &sc.kind {
                Constraint::Numeric(_) => numerics.push(sc),
                _ => match unify::solve_spanned(&sc, &self.subst) {
                    Ok(s) => self.subst = s,
                    Err(e) => self.errors.push(e),
                },
            }
        }

        let resolved_ty = self.subst.apply(ty);
        // HM let-rec generalization: exclude the binding's own placeholder slot
        // from env free vars so recursive/pre-registered functions still quantify.
        let env_free = self.free_in_resolved_env_except(name);
        let env_free_set: HashSet<&TypeVarId> = env_free.iter().collect();
        let candidate_metas: HashSet<TypeVarId> = generalize::collect_free_vars(&resolved_ty)
            .into_iter()
            .filter(|v| matches!(v, TypeVarId::Meta(_)) && !env_free_set.contains(v))
            .collect();

        let mut scheme_constraints: Vec<SpannedConstraint> = Vec::new();
        for mut sc in numerics {
            if let Constraint::Numeric(ref mut bc) = sc.kind {
                *bc.left = self.subst.apply(&bc.left);
                *bc.right = self.subst.apply(&bc.right);
                bc.result = bc.result.as_ref().map(|r| Box::new(self.subst.apply(r)));
            }
            if unify::constraint_mentions_any(&sc.kind, &candidate_metas) {
                scheme_constraints.push(sc);
            } else {
                self.constraints.push(sc);
            }
        }

        if let Some(binding_name) = name {
            if scheme_constraints.is_empty() {
                self.let_schemes.remove(binding_name);
            } else {
                self.let_schemes
                    .insert(binding_name.to_string(), scheme_constraints);
            }
        } else {
            self.constraints.extend(scheme_constraints);
        }

        generalize::generalize(&resolved_ty, &env_free)
    }

    pub(super) fn infer_let(
        &mut self,
        name: &str,
        value: &MirWitness,
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let c_start = self.constraints.len();
        let (value_ty, value_row) = self.infer_recover(value);

        if let WitnessKind::Closure { params, .. } | WitnessKind::FnDef { params, .. } = &value.kind
        {
            self.fn_arities.insert(name.to_string(), params.len());
            let min_arity = params.iter().filter(|p| p.default.is_none()).count();
            self.fn_param_specs.insert(
                name.to_string(),
                FnParamSpec {
                    names: params.iter().map(|p| p.name.clone()).collect(),
                    has_default: params.iter().map(|p| p.default.is_some()).collect(),
                    min_arity,
                },
            );
        } else if let WitnessKind::Variable(rhs) = &value.kind {
            if let Some(&arity) = self.fn_arities.get(rhs) {
                self.fn_arities.insert(name.to_string(), arity);
            }
            if let Some(spec) = self.fn_param_specs.get(rhs).cloned() {
                self.fn_param_specs.insert(name.to_string(), spec);
            }
        }

        let gen_ty = if matches!(value_ty, Type::Error) {
            self.let_schemes.remove(name);
            Type::Error
        } else if matches!(value_row, crate::mir::effect::EffectRow::Empty)
            && is_generalizable_value(value)
        {
            self.generalize_pure_binding(Some(name), &value_ty, c_start)
        } else {
            self.let_schemes.remove(name);
            value_ty.clone()
        };

        self.update_tensor_resource_flags(name, value);
        self.record_binding(span, name, &gen_ty);
        self.env.add(name.to_string(), gen_ty.clone());
        if matches!(
            value.kind,
            WitnessKind::Closure { .. } | WitnessKind::FnDef { .. }
        ) && let Some(row) = self.closure_rows.get(&value.span)
        {
            self.fn_effect_rows.insert(name.to_string(), row.clone());
        }
        Ok((gen_ty, value_row))
    }

    pub(super) fn infer_let_typed(
        &mut self,
        name: &str,
        type_hint: &TypeHint,
        value: &MirWitness,
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let ty_aliased = self.resolve_type_aliases(type_hint.to_type());
        let ty_inner = self.instantiate_wildcards(&ty_aliased);
        let (value_ty, value_row) = self.infer_recover(value);
        if let WitnessKind::Closure { params, .. } | WitnessKind::FnDef { params, .. } = &value.kind
        {
            self.fn_arities.insert(name.to_string(), params.len());
            let min_arity = params.iter().filter(|p| p.default.is_none()).count();
            self.fn_param_specs.insert(
                name.to_string(),
                FnParamSpec {
                    names: params.iter().map(|p| p.name.clone()).collect(),
                    has_default: params.iter().map(|p| p.default.is_some()).collect(),
                    min_arity,
                },
            );
        } else if let WitnessKind::Variable(rhs) = &value.kind
            && let Some(spec) = self.fn_param_specs.get(rhs).cloned()
        {
            self.fn_arities.insert(name.to_string(), spec.names.len());
            self.fn_param_specs.insert(name.to_string(), spec);
        }
        if let Type::TraitObject { trait_name, .. } = &ty_inner {
            self.validate_dyn_trait_coercion(&value_ty, trait_name, value.span)?;
        }
        if !matches!(ty_inner, Type::Any | Type::TraitObject { .. })
            && !matches!(value_ty, Type::Error)
        {
            let sub_c = SpannedConstraint::new(
                Constraint::Subtype {
                    sub: Box::new(value_ty),
                    sup: Box::new(ty_inner.clone()),
                },
                value.span,
            );
            // Eagerly solve when possible so wildcard dims (`_`) resolve immediately in `subst`.
            match unify::solve_spanned(&sub_c, &self.subst) {
                Ok(new_s) => self.subst = new_s,
                Err(_) => self.constraints.push(sub_c),
            }
        }
        let resolved_hint = self.subst.apply(&ty_inner);
        let gen_hint = generalize::generalize(&resolved_hint, &self.free_in_resolved_env());
        self.update_tensor_resource_flags(name, value);
        self.record_binding(span, name, &gen_hint);
        self.env.add(name.to_string(), gen_hint.clone());
        if matches!(
            value.kind,
            WitnessKind::Closure { .. } | WitnessKind::FnDef { .. }
        ) && let Some(row) = self.closure_rows.get(&value.span)
        {
            self.fn_effect_rows.insert(name.to_string(), row.clone());
        }
        Ok((gen_hint, value_row))
    }

    pub(super) fn infer_assign(
        &mut self,
        target: &str,
        value: &MirWitness,
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let (value_ty, value_row) = self.infer_recover(value);
        let current = self.env.get(target).cloned();
        match current {
            Some(existing) => {
                let existing = self.instantiate_named_binding(target, &existing, span);
                if !matches!(value_ty, Type::Error) {
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Subtype {
                            sub: Box::new(value_ty.clone()),
                            sup: Box::new(existing),
                        },
                        value.span,
                    ));
                }
            }
            None => {
                return Err(vec![TypeError::UnboundVariable {
                    name: target.to_string(),
                    span,
                }]);
            }
        }
        self.update_tensor_resource_flags(target, value);
        Ok((value_ty, value_row))
    }

    pub(super) fn infer_var(&mut self, name: &str, span: Span) -> Result<Type, Vec<TypeError>> {
        if let Some(ty) = self.env.get(name).cloned() {
            return Ok(self.instantiate_named_binding(name, &ty, span));
        }
        if self.struct_defs.contains_key(name) {
            return Ok(Type::Concrete {
                name: format!("{}{name}", super::stage3c::STRUCT_TYPE_PREFIX),
                generics: Vec::new(),
                traits: Vec::new(),
            });
        }
        if self.enum_defs.contains_key(name) {
            return Ok(Type::Concrete {
                name: format!("{}{name}", super::stage3c::ENUM_TYPE_PREFIX),
                generics: Vec::new(),
                traits: Vec::new(),
            });
        }
        if let Some((type_name, member)) = name.split_once("::") {
            if let Some(edef) = self.enum_defs.get(type_name).cloned() {
                if let Some((_, payload_tys)) = edef.variants.iter().find(|(v, _)| v == member) {
                    let fresh_gens: Vec<Type> = (0..edef.type_params.len())
                        .map(|_| self.fresh_type_var())
                        .collect();
                    let enum_ty = Type::Concrete {
                        name: type_name.to_string(),
                        generics: fresh_gens.clone(),
                        traits: Vec::new(),
                    };
                    if payload_tys.is_empty() {
                        return Ok(enum_ty);
                    }
                    let arg_tys: Vec<Type> = payload_tys
                        .iter()
                        .map(|pt| {
                            super::stage3c::substitute_named_generics(
                                pt,
                                &edef.type_params,
                                &fresh_gens,
                            )
                        })
                        .collect();
                    return Ok(Type::curried_arrow(
                        &arg_tys,
                        enum_ty,
                        &crate::mir::effect::EffectRow::Empty,
                    ));
                }
                return Err(vec![TypeError::UnboundVariable {
                    name: name.to_string(),
                    span,
                }]);
            }
            if let Some(sig) = self
                .inherent_impls
                .get(&(type_name.to_string(), member.to_string()))
                .cloned()
                && !sig.has_self
            {
                let param_tys: Vec<Type> = sig.params.iter().map(|(_, t, _)| t.clone()).collect();
                return Ok(Type::curried_arrow(
                    &param_tys,
                    sig.return_type,
                    &sig.effect_row,
                ));
            }
        }
        match name {
            "random" => Ok(Type::RandomModule),
            n if crate::flow::is_builtin_object(n) => Ok(Type::BuiltinModule(n.to_string())),
            n => {
                if let Some(ty) = self.builtin_callee_ty(n) {
                    return Ok(ty);
                }
                Err(vec![TypeError::UnboundVariable {
                    name: name.to_string(),
                    span,
                }])
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn apply_binop_types(
        &mut self,
        op: &crate::common::BinaryOp,
        left_ty: Type,
        left_row: crate::mir::effect::EffectRow,
        right_ty: Type,
        right_row: crate::mir::effect::EffectRow,
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        use crate::common::BinaryOp::*;
        let merged_row = self.merge_rows(left_row, right_row, span);
        let left_ty = self.subst.apply(&left_ty);
        let right_ty = self.subst.apply(&right_ty);
        if matches!(left_ty, Type::Error) || matches!(right_ty, Type::Error) {
            return Ok((Type::Error, merged_row));
        }
        if matches!(left_ty, Type::Unknown) || matches!(right_ty, Type::Unknown) {
            return Err(vec![TypeError::UnificationFailure {
                expected: "known operand types".to_string(),
                got: format!("{} and {}", left_ty.name(), right_ty.name()),
                span: Some(span),
            }]);
        }

        match op {
            Add | Sub | Mul | Div | Mod | Pow => {
                // Stage 3B: Tensor elementwise binary operations (DType + Shape broadcast + Device).
                if matches!(left_ty, Type::Tensor { .. }) || matches!(right_ty, Type::Tensor { .. })
                {
                    let out_ty = self.infer_tensor_binary_op(&left_ty, &right_ty, span)?;
                    return Ok((out_ty, merged_row));
                }

                // Stage 3B: Low-bitwidth scalar binary operations require exact DType matching.
                if matches!(left_ty, Type::Scalar(_)) || matches!(right_ty, Type::Scalar(_)) {
                    let out_ty = self.infer_scalar_binary_op(&left_ty, &right_ty, false, span)?;
                    return Ok((out_ty, merged_row));
                }

                let is_add = matches!(op, Add);
                let is_num_or_open = |t: &Type| {
                    is_numeric(t) || matches!(t, Type::TypeVar(TypeVarId::Meta(_)) | Type::Any)
                };
                let bcast_elem: Option<Type> = match (&left_ty, &right_ty) {
                    (Type::List(e), n) if is_num_or_open(e) && is_num_or_open(n) => {
                        Some(promote_list_elem(e, n))
                    }
                    (n, Type::List(e)) if is_num_or_open(n) && is_num_or_open(e) => {
                        Some(promote_list_elem(n, e))
                    }
                    (Type::List(a), Type::List(b)) if is_num_or_open(a) && is_num_or_open(b) => {
                        Some(promote_list_elem(a, b))
                    }
                    (Type::List(a), Type::List(b)) if is_add && a.compatible_with(b) => {
                        Some(a.as_ref().clone())
                    }
                    _ => None,
                };
                if let Some(elem) = bcast_elem {
                    let result_ty = self.fresh_type_var();
                    let target = Type::List(Box::new(elem));
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Eq(Box::new(result_ty.clone()), Box::new(target.clone())),
                        span,
                    ));
                    if matches!(left_ty, Type::List(_)) {
                        self.constraints.push(SpannedConstraint::new(
                            Constraint::Subtype {
                                sub: Box::new(left_ty),
                                sup: Box::new(target.clone()),
                            },
                            span,
                        ));
                    }
                    if matches!(right_ty, Type::List(_)) {
                        self.constraints.push(SpannedConstraint::new(
                            Constraint::Subtype {
                                sub: Box::new(right_ty),
                                sup: Box::new(target),
                            },
                            span,
                        ));
                    }
                    return Ok((result_ty, merged_row));
                }

                if is_numeric(&left_ty) && is_numeric(&right_ty) {
                    return Ok((promote_numeric(&left_ty, &right_ty), merged_row));
                }

                match (&left_ty, &right_ty) {
                    (Type::String, Type::String) if is_add => Ok((Type::String, merged_row)),
                    (Type::TypeVar(TypeVarId::Meta(_)), _)
                    | (_, Type::TypeVar(TypeVarId::Meta(_)))
                    | (Type::Any, _)
                    | (_, Type::Any) => {
                        let allow_string =
                            is_add && !is_numeric(&left_ty) && !is_numeric(&right_ty);
                        let non_op = |t: &Type| {
                            !is_numeric(t)
                                && !matches!(t, Type::TypeVar(TypeVarId::Meta(_)) | Type::Any)
                                && !(allow_string && matches!(t, Type::String))
                        };
                        if non_op(&left_ty) || non_op(&right_ty) {
                            return Err(vec![TypeError::UnificationFailure {
                                expected: if allow_string {
                                    "numeric or string".to_string()
                                } else {
                                    "numeric type (int or float)".to_string()
                                },
                                got: format!("{} and {}", left_ty.name(), right_ty.name()),
                                span: Some(span),
                            }]);
                        }
                        let result_ty = self.fresh_type_var();
                        self.constraints.push(SpannedConstraint::new(
                            Constraint::Numeric(super::unify::BinaryConstraint {
                                left: Box::new(left_ty),
                                right: Box::new(right_ty),
                                result: Some(Box::new(result_ty.clone())),
                                allow_string,
                            }),
                            span,
                        ));
                        Ok((result_ty, merged_row))
                    }
                    _ => Err(vec![TypeError::UnificationFailure {
                        expected: if is_add {
                            format!("compatible operands for '+' (got {})", left_ty.name())
                        } else {
                            "numeric operands (int or float)".to_string()
                        },
                        got: format!("{} and {}", left_ty.name(), right_ty.name()),
                        span: Some(span),
                    }]),
                }
            }
            MatMul => {
                // Stage 3B: First-class Tensor matrix/tensor contraction (@).
                if matches!(left_ty, Type::Tensor { .. }) || matches!(right_ty, Type::Tensor { .. })
                {
                    let out_ty = self.infer_tensor_matmul_op(&left_ty, &right_ty, span)?;
                    return Ok((out_ty, merged_row));
                }

                if matches!(left_ty, Type::TypeVar(TypeVarId::Meta(_)) | Type::Any)
                    || matches!(right_ty, Type::TypeVar(TypeVarId::Meta(_)) | Type::Any)
                {
                    if let Some((_dim, elem)) =
                        matmul_shape(&left_ty).or_else(|| matmul_shape(&right_ty))
                        && !is_numeric(&elem)
                        && !matches!(elem, Type::TypeVar(TypeVarId::Meta(_)) | Type::Any)
                    {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: "numeric vector or matrix".to_string(),
                            got: format!("matrix element {}", elem.name()),
                            span: Some(span),
                        }]);
                    }
                    return Ok((self.fresh_type_var(), merged_row));
                }
                let Some((dl, el)) = matmul_shape(&left_ty) else {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: "Tensor or list<number> or list<list<number>>".to_string(),
                        got: left_ty.name(),
                        span: Some(span),
                    }]);
                };
                let Some((dr, er)) = matmul_shape(&right_ty) else {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: "Tensor or list<number> or list<list<number>>".to_string(),
                        got: right_ty.name(),
                        span: Some(span),
                    }]);
                };
                let ok_el = is_numeric(&el)
                    || matches!(
                        el,
                        Type::TypeVar(TypeVarId::Meta(_)) | Type::Any | Type::Error
                    );
                let ok_er = is_numeric(&er)
                    || matches!(
                        er,
                        Type::TypeVar(TypeVarId::Meta(_)) | Type::Any | Type::Error
                    );
                if !ok_el || !ok_er {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: "numeric elements for '@' matrix multiply".to_string(),
                        got: format!("{} and {}", el.name(), er.name()),
                        span: Some(span),
                    }]);
                }
                let scalar = if matches!(el, Type::TypeVar(TypeVarId::Meta(_)))
                    || matches!(er, Type::TypeVar(TypeVarId::Meta(_)))
                {
                    let res = self.fresh_type_var();
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Numeric(super::unify::BinaryConstraint {
                            left: Box::new(el),
                            right: Box::new(er),
                            result: Some(Box::new(res.clone())),
                            allow_string: false,
                        }),
                        span,
                    ));
                    res
                } else {
                    promote_numeric(&el, &er)
                };
                let ret = match (dl, dr) {
                    (1, 1) => scalar,
                    (2, 2) => Type::List(Box::new(Type::List(Box::new(scalar)))),
                    (2, 1) | (1, 2) => Type::List(Box::new(scalar)),
                    _ => unreachable!(),
                };
                Ok((ret, merged_row))
            }
            Equal | NotEqual => {
                if matches!(left_ty, Type::Scalar(_)) || matches!(right_ty, Type::Scalar(_)) {
                    let _ = self.infer_scalar_binary_op(&left_ty, &right_ty, true, span)?;
                    return Ok((Type::Bool, merged_row));
                }
                if !left_ty.compatible_with(&right_ty) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: left_ty.name(),
                        got: right_ty.name(),
                        span: Some(span),
                    }]);
                }
                if is_numeric(&left_ty) && is_numeric(&right_ty) {
                    // Int == Float valid.
                } else if (matches!(left_ty, Type::Nil)
                    && matches!(right_ty, Type::TypeVar(TypeVarId::Meta(_))))
                    || (matches!(right_ty, Type::Nil)
                        && matches!(left_ty, Type::TypeVar(TypeVarId::Meta(_))))
                {
                    // `x == nil` / `x != nil` on an open meta variable is a nil-check guard;
                    // do not collapse `x` to `Nil` across subsequent branches.
                } else if (is_numeric(&left_ty)
                    && matches!(right_ty, Type::TypeVar(TypeVarId::Meta(_))))
                    || (is_numeric(&right_ty)
                        && matches!(left_ty, Type::TypeVar(TypeVarId::Meta(_))))
                {
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Numeric(super::unify::BinaryConstraint {
                            left: Box::new(left_ty),
                            right: Box::new(right_ty),
                            result: None,
                            allow_string: false,
                        }),
                        span,
                    ));
                } else {
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Eq(Box::new(left_ty), Box::new(right_ty)),
                        span,
                    ));
                }
                Ok((Type::Bool, merged_row))
            }
            Greater | Less | GreaterEqual | LessEqual => {
                if matches!(left_ty, Type::Scalar(_)) || matches!(right_ty, Type::Scalar(_)) {
                    let _ = self.infer_scalar_binary_op(&left_ty, &right_ty, true, span)?;
                    return Ok((Type::Bool, merged_row));
                }
                if !left_ty.compatible_with(&right_ty) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: left_ty.name(),
                        got: right_ty.name(),
                        span: Some(span),
                    }]);
                }
                if is_numeric(&left_ty) && is_numeric(&right_ty) {
                    // Numeric comparison valid.
                } else if matches!(left_ty, Type::TypeVar(TypeVarId::Meta(_)))
                    || matches!(right_ty, Type::TypeVar(TypeVarId::Meta(_)))
                {
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Numeric(super::unify::BinaryConstraint {
                            left: Box::new(left_ty),
                            right: Box::new(right_ty),
                            result: None,
                            allow_string: true,
                        }),
                        span,
                    ));
                } else {
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Eq(Box::new(left_ty), Box::new(right_ty)),
                        span,
                    ));
                }
                Ok((Type::Bool, merged_row))
            }
        }
    }

    pub(super) fn resolve_named_callee_header(
        &mut self,
        name: &str,
        span: Span,
    ) -> Result<
        (
            Type,
            crate::mir::effect::EffectRow,
            Option<builtin::BuiltinCallSpec>,
        ),
        Vec<TypeError>,
    > {
        if let Some(t) = self.env.get(name).cloned() {
            let inst = self.instantiate_named_binding(name, &t, span);
            let row = match Self::arrow_row_of(&inst) {
                crate::mir::effect::EffectRow::Empty => self
                    .fn_effect_rows
                    .get(name)
                    .cloned()
                    .unwrap_or(crate::mir::effect::EffectRow::Empty),
                r => r,
            };
            return Ok((inst, row, None));
        }
        if name == "random" {
            return Ok((
                Type::RandomModule,
                crate::mir::effect::EffectRow::Empty,
                None,
            ));
        }
        if crate::flow::is_builtin_object(name) {
            return Ok((
                Type::BuiltinModule(name.to_string()),
                crate::mir::effect::EffectRow::Empty,
                None,
            ));
        }
        if let Some(spec) = self.builtin_callee_sig(name) {
            return Ok((
                Type::Builtin,
                crate::mir::effect::EffectRow::Empty,
                Some(spec),
            ));
        }
        if let Some((type_name, member)) = name.split_once("::")
            && let Some(edef) = self.enum_defs.get(type_name)
        {
            if !edef.variants.iter().any(|(v, _)| v == member) {
                return Err(vec![TypeError::UnboundVariable {
                    name: name.to_string(),
                    span,
                }]);
            }
            let fresh_gens: Vec<Type> = (0..edef.type_params.len())
                .map(|_| self.fresh_type_var())
                .collect();
            return Ok((
                Type::Concrete {
                    name: type_name.to_string(),
                    generics: fresh_gens,
                    traits: Vec::new(),
                },
                crate::mir::effect::EffectRow::Empty,
                None,
            ));
        }
        Err(vec![TypeError::UnboundVariable {
            name: name.to_string(),
            span,
        }])
    }

    pub(super) fn check_call_arity(
        &self,
        callee: &WitnessCallee,
        callee_ty: &Type,
        builtin_sig: Option<&builtin::BuiltinCallSpec>,
        arg_len: usize,
        span: Span,
    ) -> Result<(), Vec<TypeError>> {
        fn arrow_chain_info(ty: &Type) -> (usize, bool) {
            if let Type::Arrow(inp, _, _) = ty
                && matches!(&**inp, Type::Tuple(elems) if elems.is_empty())
            {
                return (0, false);
            }
            let mut cur = ty;
            let mut count = 0;
            while let Type::Arrow(_, out, _) = cur {
                count += 1;
                cur = out;
            }
            let open_tail = matches!(cur, Type::TypeVar(_) | Type::Any | Type::Error);
            (count, open_tail)
        }

        if let Some(spec) = builtin_sig {
            if arg_len < spec.min_arity {
                return Err(vec![TypeError::ArityMismatch {
                    expected: spec.min_arity,
                    actual: arg_len,
                    span,
                }]);
            }
            if !spec.variadic && arg_len > spec.params.len() {
                return Err(vec![TypeError::ArityMismatch {
                    expected: spec.params.len(),
                    actual: arg_len,
                    span,
                }]);
            }
            return Ok(());
        }

        if let WitnessCallee::Name(n) | WitnessCallee::Var(n) = callee
            && let Some(&want) = self.fn_arities.get(n)
        {
            if arg_len != want {
                return Err(vec![TypeError::ArityMismatch {
                    expected: want,
                    actual: arg_len,
                    span,
                }]);
            }
            return Ok(());
        }

        if let WitnessCallee::Evaluated(e) = callee
            && let WitnessKind::Closure { params, .. } = &e.kind
        {
            if arg_len != params.len() {
                return Err(vec![TypeError::ArityMismatch {
                    expected: params.len(),
                    actual: arg_len,
                    span,
                }]);
            }
            return Ok(());
        }

        let (arrows, open_tail) = arrow_chain_info(callee_ty);
        if arrows > 0 && ((!open_tail && arg_len != arrows) || (open_tail && arg_len == 0)) {
            return Err(vec![TypeError::ArityMismatch {
                expected: arrows,
                actual: arg_len,
                span,
            }]);
        }
        Ok(())
    }

    pub(super) fn apply_builtin_call_types(
        &mut self,
        spec: &builtin::BuiltinCallSpec,
        arg_vals: Vec<(Type, crate::mir::effect::EffectRow, Span)>,
        span: Span,
    ) -> (Type, crate::mir::effect::EffectRow) {
        let mut acc_row = crate::mir::effect::EffectRow::Empty;
        for (i, (arg_ty, arg_row, arg_span)) in arg_vals.into_iter().enumerate() {
            acc_row = self.merge_rows(acc_row, arg_row, span);
            if matches!(arg_ty, Type::Error) {
                continue;
            }
            if let Some(param_ty) = spec.params.get(i).or_else(|| spec.params.last())
                && !matches!(param_ty, Type::Any)
            {
                self.constraints.push(SpannedConstraint::new(
                    Constraint::Subtype {
                        sub: Box::new(arg_ty),
                        sup: Box::new(param_ty.clone()),
                    },
                    arg_span,
                ));
            }
        }
        (spec.return_type.clone(), acc_row)
    }

    pub(super) fn apply_curried_call_types(
        &mut self,
        mut callee_ty: Type,
        mut acc_row: crate::mir::effect::EffectRow,
        arg_vals: Vec<(Type, crate::mir::effect::EffectRow, Span)>,
        span: Span,
    ) -> (Type, crate::mir::effect::EffectRow) {
        if arg_vals.is_empty() {
            let resolved = self.subst.apply(&callee_ty);
            let inst = self.instantiate_if_forall(&resolved);
            let resolved = self.subst.apply(&inst);
            return match resolved {
                Type::Arrow(inp, ret_ty, eff_row) if matches!(&*inp, Type::Tuple(elems) if elems.is_empty()) => {
                    (*ret_ty, self.merge_rows(acc_row, eff_row, span))
                }
                Type::Error => (Type::Error, acc_row),
                Type::Any
                | Type::Closure
                | Type::Task
                | Type::Partial
                | Type::Compose
                | Type::Builtin => (Type::Any, acc_row),
                other => {
                    let fresh_ret = self.fresh_type_var();
                    let fresh_eff = self.fresh_row_var();
                    let expected = Type::Arrow(
                        Box::new(Type::Tuple(Vec::new())),
                        Box::new(fresh_ret.clone()),
                        fresh_eff.clone(),
                    );
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Eq(Box::new(other), Box::new(expected)),
                        span,
                    ));
                    (fresh_ret, self.merge_rows(acc_row, fresh_eff, span))
                }
            };
        }

        for (arg_ty, arg_row, arg_span) in arg_vals {
            acc_row = self.merge_rows(acc_row, arg_row, span);
            let resolved = self.subst.apply(&callee_ty);
            let inst = self.instantiate_if_forall(&resolved);
            let resolved = self.subst.apply(&inst);
            match resolved {
                Type::Arrow(param_ty, ret_ty, eff_row) => {
                    if !matches!(arg_ty, Type::Error) {
                        self.constraints.push(SpannedConstraint::new(
                            Constraint::Subtype {
                                sub: Box::new(arg_ty),
                                sup: param_ty,
                            },
                            arg_span,
                        ));
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
                    callee_ty = if matches!(resolved, Type::Error) {
                        Type::Error
                    } else {
                        Type::Any
                    };
                }
                _ => {
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

    pub(super) fn infer_call(
        &mut self,
        callee: &WitnessCallee,
        args: &[MirWitness],
        named_args: &[(String, MirWitness)],
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        if matches!(
            callee,
            WitnessCallee::Builtin(crate::mir::witness::BuiltinOp::Neg)
        ) {
            if args.len() != 1 || !named_args.is_empty() {
                return Err(vec![TypeError::ArityMismatch {
                    expected: 1,
                    actual: args.len() + named_args.len(),
                    span,
                }]);
            }
            let (arg_ty, arg_row) = self.infer_recover(&args[0]);
            let resolved = self.subst.apply(&arg_ty);
            match &resolved {
                Type::Error => return Ok((Type::Error, arg_row)),
                Type::Int | Type::Float | Type::Any | Type::TypeVar(_) => {
                    return Ok((resolved, arg_row));
                }
                Type::Scalar(dt) => {
                    if dt.is_unsigned_int() || dt.is_bool() {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: "signed numeric type for unary '-'".to_string(),
                            got: resolved.name(),
                            span: Some(span),
                        }]);
                    }
                    return Ok((resolved, arg_row));
                }
                Type::Tensor { dtype, .. } => {
                    let dt_resolved = self.subst.apply(dtype);
                    if let Some(dt) = dt_resolved.as_dtype()
                        && (dt.is_unsigned_int() || dt.is_bool())
                    {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: "signed numeric Tensor DType for unary '-'".to_string(),
                            got: resolved.name(),
                            span: Some(span),
                        }]);
                    }
                    return Ok((resolved, arg_row));
                }
                _ => {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: "numeric type or Tensor for unary '-'".to_string(),
                        got: resolved.name(),
                        span: Some(span),
                    }]);
                }
            }
        }

        if let WitnessCallee::Method(_, method) = callee {
            let Some((recv, method_args)) = args.split_first() else {
                return Err(vec![TypeError::ArityMismatch {
                    expected: 1,
                    actual: 0,
                    span,
                }]);
            };
            return self.infer_method_call(recv, method, method_args, named_args, span);
        }

        if matches!(callee, WitnessCallee::Name(n) | WitnessCallee::Var(n) if n == "[]") {
            let Some((recv, _idx)) = args.split_first() else {
                return Err(vec![TypeError::ArityMismatch {
                    expected: 2,
                    actual: args.len(),
                    span,
                }]);
            };
            let (recv_ty, mut acc_row) = self.infer_recover(recv);
            if let Some(rest) = args.get(1) {
                let (_idx_ty, idx_row) = self.infer_recover(rest);
                acc_row = self.merge_rows(acc_row, idx_row, span);
            }
            let resolved_recv = self.subst.apply(&recv_ty);
            match &resolved_recv {
                Type::Error => return Ok((Type::Error, acc_row)),
                Type::String => return Ok((Type::Char, acc_row)),
                Type::List(e) => return Ok(((**e).clone(), acc_row)),
                Type::Dict(_, v) => return Ok((v.as_ref().clone(), acc_row)),
                Type::Tensor {
                    dtype,
                    shape: crate::typeck::ShapeType::Ranked(dims),
                    device,
                } => {
                    if dims.is_empty() {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: "tensor with rank >= 1 for indexing".to_string(),
                            got: resolved_recv.name(),
                            span: Some(span),
                        }]);
                    } else if dims.len() == 1 {
                        return Ok(((**dtype).clone(), acc_row));
                    } else {
                        return Ok((
                            Type::Tensor {
                                dtype: dtype.clone(),
                                shape: crate::typeck::ShapeType::Ranked(dims[1..].to_vec()),
                                device: device.clone(),
                            },
                            acc_row,
                        ));
                    }
                }
                _ => return Ok((self.fresh_type_var(), acc_row)),
            }
        }

        if let WitnessCallee::Name(name) | WitnessCallee::Var(name) = callee
            && !self.env.contains(name)
        {
            if let Some(res) = self.try_infer_nominal_or_assoc_call(name, args, named_args, span) {
                return res;
            }
            if name == "grad" {
                return self.infer_grad_builtin(false, args, named_args, span);
            }
            if name == "value_and_grad" {
                return self.infer_grad_builtin(true, args, named_args, span);
            }
            if name == "jvp" {
                return self.infer_jvp_builtin(args, named_args, span);
            }
            if name == "trace_dag" {
                return self.infer_trace_dag_builtin(args, named_args, span);
            }
            if name == "tensor" {
                return self.infer_tensor_constructor_call("tensor", args, named_args, span);
            }
            if let Some(assoc) = name
                .strip_prefix("Tensor::")
                .or_else(|| name.strip_prefix("tensor::"))
            {
                if assoc == "devices" {
                    if !args.is_empty() || !named_args.is_empty() {
                        return Err(vec![TypeError::ArityMismatch {
                            expected: 0,
                            actual: args.len() + named_args.len(),
                            span,
                        }]);
                    }
                    return Ok((
                        Type::List(Box::new(Type::String)),
                        crate::mir::effect::EffectRow::Empty,
                    ));
                }
                if assoc == "device_info" {
                    let total = args.len() + named_args.len();
                    if total > 1 {
                        return Err(vec![TypeError::ArityMismatch {
                            expected: 1,
                            actual: total,
                            span,
                        }]);
                    }
                    let mut acc_row = crate::mir::effect::EffectRow::Empty;
                    for a in args {
                        let (ty, row) = self.infer_expr(a)?;
                        acc_row = self.merge_rows(acc_row, row, a.span);
                        self.constraints.push(SpannedConstraint::new(
                            Constraint::Subtype {
                                sub: Box::new(ty),
                                sup: Box::new(Type::Union(vec![Type::String, Type::Nil])),
                            },
                            a.span,
                        ));
                    }
                    for (k, a) in named_args {
                        if k != "device" {
                            return Err(vec![TypeError::UnificationFailure {
                                expected: "valid keyword argument 'device' for Tensor::device_info"
                                    .to_string(),
                                got: format!("unknown keyword argument '{k}'"),
                                span: Some(a.span),
                            }]);
                        }
                        let (ty, row) = self.infer_expr(a)?;
                        acc_row = self.merge_rows(acc_row, row, a.span);
                        self.constraints.push(SpannedConstraint::new(
                            Constraint::Subtype {
                                sub: Box::new(ty),
                                sup: Box::new(Type::Union(vec![Type::String, Type::Nil])),
                            },
                            a.span,
                        ));
                    }
                    return Ok((
                        Type::Dict(Box::new(Type::String), Box::new(Type::String)),
                        acc_row,
                    ));
                }
                return self.infer_tensor_constructor_call(assoc, args, named_args, span);
            }
        }

        if let WitnessCallee::Name(name) = callee
            && name == "merge_with"
            && let Some(WitnessKind::Literal(crate::common::Literal::String(s, _))) =
                args.get(1).map(|a| &a.kind)
            && crate::value::MergeStrategy::from_name(s).is_none()
        {
            return Err(vec![TypeError::InvalidLiteral {
                what: "merge_with strategy".to_string(),
                value: s.clone(),
                span: Some(span),
            }]);
        }

        let (callee_ty, acc_row, builtin_sig) = match callee {
            WitnessCallee::Name(name) | WitnessCallee::Var(name) => {
                self.resolve_named_callee_header(name, span)?
            }
            WitnessCallee::Evaluated(expr) => {
                let (t, r) = self.infer_expr(expr)?;
                (t, r, None)
            }
            WitnessCallee::Builtin(op) => {
                let ty = self.builtin_type(op)?;
                let row = Self::arrow_row_of(&ty);
                (ty, row, None)
            }
            WitnessCallee::Method(_, _) => unreachable!(),
        };

        let resolved_callee = self.subst.apply(&callee_ty);
        if let Type::Concrete { name: cname, .. } = &resolved_callee
            && let Some(sname) = cname.strip_prefix(super::stage3c::STRUCT_TYPE_PREFIX)
        {
            return self.infer_struct_constructor(sname, args, named_args, span);
        }
        if matches!(resolved_callee, Type::BuiltinModule(_) | Type::RandomModule) {
            let mod_name = match &resolved_callee {
                Type::BuiltinModule(m) => m.as_str(),
                Type::RandomModule => "random",
                _ => unreachable!(),
            };
            return Err(vec![TypeError::UnificationFailure {
                expected: "callable function".to_string(),
                got: format!("'{mod_name}' is a module, not a function"),
                span: Some(span),
            }]);
        }
        if !matches!(
            resolved_callee,
            Type::Arrow(_, _, _)
                | Type::ForAll(_, _)
                | Type::TypeVar(_)
                | Type::Any
                | Type::Error
                | Type::Closure
                | Type::Task
                | Type::Builtin
                | Type::Partial
                | Type::Compose
                | Type::Trait { .. }
        ) {
            return Err(vec![TypeError::NotAClosure {
                found: resolved_callee.name(),
                span,
            }]);
        }

        let param_spec: Option<(String, FnParamSpec)> = match callee {
            WitnessCallee::Name(n) | WitnessCallee::Var(n) => {
                self.fn_param_specs.get(n).cloned().map(|s| (n.clone(), s))
            }
            WitnessCallee::Evaluated(e) => {
                if let Some(s) = self.closure_param_specs.get(&e.span).cloned() {
                    Some(("<closure>".to_string(), s))
                } else if let WitnessKind::Closure { params, .. } = &e.kind {
                    Some((
                        "<closure>".to_string(),
                        FnParamSpec {
                            names: params.iter().map(|p| p.name.clone()).collect(),
                            has_default: params.iter().map(|p| p.default.is_some()).collect(),
                            min_arity: params.iter().filter(|p| p.default.is_none()).count(),
                        },
                    ))
                } else {
                    None
                }
            }
            _ => None,
        };

        if let Some((label, spec)) = param_spec {
            let slots = self.align_call_args(&label, &spec, args, named_args, span)?;
            if let WitnessCallee::Name(fn_name) | WitnessCallee::Var(fn_name) = callee
                && let Some(mut_indices) = self.fn_mutates_tensor_params.get(fn_name).cloned()
            {
                for idx in mut_indices {
                    let arg_w = args.get(idx).or_else(|| {
                        spec.names.get(idx).and_then(|pname| {
                            named_args.iter().find(|(k, _)| k == pname).map(|(_, w)| w)
                        })
                    });
                    if let Some(arg_w) = arg_w {
                        if self.expr_produces_grad_tensor(arg_w) {
                            return Err(vec![TypeError::UnificationFailure {
                                expected: format!(
                                    "non-differentiable tensor for parameter #{} of '{fn_name}' (mutated in-place)",
                                    idx + 1
                                ),
                                got: "gradient-tracked tensor (requires_grad=true)".to_string(),
                                span: Some(arg_w.span),
                            }]);
                        }
                        if self.expr_produces_tensor_view(arg_w)
                            || self.expr_is_aliased_source(arg_w)
                        {
                            return Err(vec![TypeError::UnificationFailure {
                                expected: format!(
                                    "unique owning tensor for parameter #{} of '{fn_name}' (mutated in-place; call .contiguous() first)",
                                    idx + 1
                                ),
                                got: "aliased or strided tensor view".to_string(),
                                span: Some(arg_w.span),
                            }]);
                        }
                    }
                }
            }
            return Ok(self.apply_curried_call_slots(callee_ty, acc_row, slots, span));
        }

        if !named_args.is_empty() {
            if let Some(spec) = builtin_sig.as_ref()
                && let WitnessCallee::Name(bname) | WitnessCallee::Var(bname) = callee
                && (bname == "tcp_listen"
                    || bname == "tcp_connect"
                    || bname == "tls_listen"
                    || bname == "tls_connect"
                    || bname == "kv_allocator"
                    || bname == "memory_pool")
            {
                let pnames: Vec<String> = if bname == "tcp_listen" {
                    vec!["addr".to_string()]
                } else if bname == "tcp_connect" {
                    vec![
                        "addr".to_string(),
                        "timeout_ms".to_string(),
                        "ctx".to_string(),
                    ]
                } else if bname == "tls_listen" {
                    vec!["addr".to_string(), "config".to_string()]
                } else if bname == "tls_connect" {
                    vec![
                        "addr".to_string(),
                        "server_name".to_string(),
                        "config".to_string(),
                        "timeout_ms".to_string(),
                        "ctx".to_string(),
                    ]
                } else if bname == "memory_pool" {
                    vec!["max_bytes".to_string(), "device".to_string()]
                } else {
                    vec![
                        "total_pages".to_string(),
                        "page_tokens".to_string(),
                        "num_heads".to_string(),
                        "head_dim".to_string(),
                        "dtype".to_string(),
                        "device".to_string(),
                        "max_bytes".to_string(),
                    ]
                };
                let fspec = FnParamSpec {
                    names: pnames,
                    has_default: (0..spec.params.len())
                        .map(|i| i >= spec.min_arity)
                        .collect(),
                    min_arity: spec.min_arity,
                };
                let slots = self.align_call_args(bname, &fspec, args, named_args, span)?;
                let arg_vals: Vec<(Type, crate::mir::effect::EffectRow, Span)> = slots
                    .into_iter()
                    .enumerate()
                    .map(|(i, slot)| match slot {
                        Some(v) => v,
                        None => (
                            spec.params.get(i).cloned().unwrap_or(Type::Nil),
                            crate::mir::effect::EffectRow::Empty,
                            span,
                        ),
                    })
                    .collect();
                return Ok(self.apply_builtin_call_types(spec, arg_vals, span));
            }
            if matches!(
                resolved_callee,
                Type::Any
                    | Type::TypeVar(_)
                    | Type::Closure
                    | Type::Task
                    | Type::Builtin
                    | Type::Error
            ) {
                let mut acc_row = acc_row;
                for a in args {
                    let (_, r) = self.infer_recover(a);
                    acc_row = self.merge_rows(acc_row, r, span);
                }
                for (_, a) in named_args {
                    let (_, r) = self.infer_recover(a);
                    acc_row = self.merge_rows(acc_row, r, span);
                }
                return Ok((
                    if matches!(resolved_callee, Type::Error) {
                        Type::Error
                    } else {
                        self.fresh_type_var()
                    },
                    acc_row,
                ));
            }
            return Err(vec![TypeError::UnificationFailure {
                expected: "function with statically known parameter names for keyword arguments"
                    .to_string(),
                got: format!(
                    "keyword argument '{}' on {}",
                    named_args[0].0,
                    resolved_callee.name()
                ),
                span: Some(named_args[0].1.span),
            }]);
        }

        self.check_call_arity(callee, &callee_ty, builtin_sig.as_ref(), args.len(), span)?;

        if matches!(callee, WitnessCallee::Name(n) if n.contains("::"))
            && matches!(resolved_callee, Type::Trait { .. })
        {
            if !args.is_empty() {
                return Err(vec![TypeError::ArityMismatch {
                    expected: 0,
                    actual: args.len(),
                    span,
                }]);
            }
            return Ok((resolved_callee, acc_row));
        }

        if let Some(spec) = builtin_sig {
            let mut arg_vals = Vec::with_capacity(args.len());
            for arg in args {
                let (t, r) = self.infer_recover(arg);
                arg_vals.push((t, r, arg.span));
            }
            return Ok(self.apply_builtin_call_types(&spec, arg_vals, span));
        }

        let mut arg_vals = Vec::with_capacity(args.len());
        for arg in args {
            let (t, r) = self.infer_recover(arg);
            arg_vals.push((t, r, arg.span));
        }

        // Stage 3B inter-procedural Resource safety check: reject passing a gradient-tracked
        // or view-aliased tensor into a function parameter mutated in-place (`fill_`/`add_`/`mul_`).
        if let WitnessCallee::Name(fn_name) | WitnessCallee::Var(fn_name) = callee
            && let Some(mut_indices) = self.fn_mutates_tensor_params.get(fn_name).cloned()
        {
            for idx in mut_indices {
                if let Some(arg_w) = args.get(idx) {
                    if self.expr_produces_grad_tensor(arg_w) {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: format!(
                                "non-differentiable tensor for parameter #{} of '{fn_name}' (mutated in-place)",
                                idx + 1
                            ),
                            got: "gradient-tracked tensor (requires_grad=true)".to_string(),
                            span: Some(arg_w.span),
                        }]);
                    }
                    if self.expr_produces_tensor_view(arg_w) || self.expr_is_aliased_source(arg_w) {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: format!(
                                "unique owning tensor for parameter #{} of '{fn_name}' (mutated in-place; call .contiguous() first)",
                                idx + 1
                            ),
                            got: "aliased or strided tensor view".to_string(),
                            span: Some(arg_w.span),
                        }]);
                    }
                }
            }
        }

        Ok(self.apply_curried_call_types(callee_ty, acc_row, arg_vals, span))
    }

    fn arrow_row_of(ty: &Type) -> crate::mir::effect::EffectRow {
        match ty {
            Type::Arrow(_, _, row) => row.clone(),
            _ => crate::mir::effect::EffectRow::Empty,
        }
    }

    pub(super) fn infer_method_call(
        &mut self,
        receiver: &MirWitness,
        method: &str,
        args: &[MirWitness],
        named_args: &[(String, MirWitness)],
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let (recv_ty, recv_row) = self.infer_recover(receiver);
        let recv_module_name = match &receiver.kind {
            WitnessKind::Variable(module) => Some(module.as_str()),
            _ => None,
        };
        self.infer_method_call_with_recv(
            Some(receiver),
            recv_ty,
            recv_row,
            recv_module_name,
            method,
            args,
            named_args,
            span,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn infer_method_call_with_recv(
        &mut self,
        receiver_witness: Option<&MirWitness>,
        recv_ty: Type,
        recv_row: crate::mir::effect::EffectRow,
        recv_module_name: Option<&str>,
        method: &str,
        args: &[MirWitness],
        named_args: &[(String, MirWitness)],
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let resolved_recv = self.subst.apply(&recv_ty);
        if matches!(resolved_recv, Type::Error) {
            let mut acc_row = recv_row;
            for a in args {
                let (_, r) = self.infer_recover(a);
                acc_row = self.merge_rows(acc_row, r, span);
            }
            for (_, a) in named_args {
                let (_, r) = self.infer_recover(a);
                acc_row = self.merge_rows(acc_row, r, span);
            }
            return Ok((Type::Error, acc_row));
        }

        if let Type::Concrete { name, generics, .. } = &resolved_recv {
            if let Some(res) = self.infer_concrete_receiver_member(
                &resolved_recv,
                name,
                generics,
                method,
                args,
                named_args,
                recv_row.clone(),
                span,
            ) {
                return res;
            }
            if let Some(res) = self.try_infer_concrete_impl_method(
                &resolved_recv,
                method,
                args,
                named_args,
                recv_row.clone(),
                span,
            ) {
                return res;
            }
        }
        if let Type::TraitObject {
            trait_name,
            generics,
        } = &resolved_recv
            && let Some(res) = self.infer_trait_object_method(
                trait_name,
                generics,
                method,
                args,
                named_args,
                recv_row.clone(),
                span,
            )
        {
            return res;
        }

        if !named_args.is_empty()
            && !matches!(resolved_recv, Type::Tensor { .. })
            && let Some(res) = self.try_infer_concrete_impl_method(
                &resolved_recv,
                method,
                args,
                named_args,
                recv_row.clone(),
                span,
            )
        {
            return res;
        }

        let mut acc_row = recv_row.clone();
        let mut arg_types: Vec<Type> = Vec::with_capacity(args.len());
        for a in args {
            let (t, r) = self.infer_recover(a);
            arg_types.push(t);
            acc_row = self.merge_rows(acc_row, r, span);
        }

        if matches!(resolved_recv, Type::RandomModule) {
            return self.infer_random_method(method, arg_types, acc_row, span);
        }
        if matches!(resolved_recv, Type::Tensor { .. }) {
            if named_args.is_empty() {
                match self.infer_tensor_method_call(
                    receiver_witness,
                    &resolved_recv,
                    method,
                    args,
                    &arg_types,
                    acc_row.clone(),
                    span,
                ) {
                    Ok(ok) => return Ok(ok),
                    Err(errs) => {
                        if let Some(res) = self.try_infer_concrete_impl_method(
                            &resolved_recv,
                            method,
                            args,
                            named_args,
                            recv_row.clone(),
                            span,
                        ) {
                            return res;
                        }
                        return Err(errs);
                    }
                }
            } else if let Some(res) = self.try_infer_concrete_impl_method(
                &resolved_recv,
                method,
                args,
                named_args,
                recv_row.clone(),
                span,
            ) {
                return res;
            }
        }
        if matches!(resolved_recv, Type::Int | Type::Float | Type::Scalar(_)) {
            match method {
                "dtype" => {
                    if !args.is_empty() || !named_args.is_empty() {
                        return Err(vec![TypeError::ArityMismatch {
                            expected: 0,
                            actual: args.len() + named_args.len(),
                            span,
                        }]);
                    }
                    return Ok((Type::String, acc_row));
                }
                "to_dtype" | "to" => {
                    if args.len() != 1 || !named_args.is_empty() {
                        return Err(vec![TypeError::ArityMismatch {
                            expected: 1,
                            actual: args.len() + named_args.len(),
                            span,
                        }]);
                    }
                    let target = Self::parse_static_dtype_witness(args.first())?
                        .unwrap_or_else(|| self.fresh_type_var());
                    return Ok((target, acc_row));
                }
                _ => {}
            }
        }
        if matches!(resolved_recv, Type::Context) && method == "spawn" {
            if args.is_empty() {
                return Err(vec![TypeError::ArityMismatch {
                    expected: 1,
                    actual: 0,
                    span,
                }]);
            }
            let callee_ty = arg_types[0].clone();
            let resolved_callee = self.subst.apply(&callee_ty);
            if !matches!(
                resolved_callee,
                Type::Arrow(_, _, _)
                    | Type::ForAll(_, _)
                    | Type::TypeVar(_)
                    | Type::Any
                    | Type::Error
                    | Type::Closure
                    | Type::Task
                    | Type::Builtin
                    | Type::Partial
                    | Type::Compose
            ) {
                return Err(vec![TypeError::NotAClosure {
                    found: resolved_callee.name(),
                    span: args[0].span,
                }]);
            }
            let call_arg_vals: Vec<(Type, crate::mir::effect::EffectRow, Span)> = args[1..]
                .iter()
                .zip(arg_types[1..].iter())
                .map(|(a, t)| (t.clone(), crate::mir::effect::EffectRow::Empty, a.span))
                .collect();
            let (ret_ty, spawned_row) = self.apply_curried_call_types(
                callee_ty,
                crate::mir::effect::EffectRow::Empty,
                call_arg_vals,
                span,
            );
            acc_row = self.merge_rows(acc_row, spawned_row, span);
            return Ok((Type::TaskHandle(Box::new(ret_ty)), acc_row));
        }

        let mod_name = match &resolved_recv {
            Type::BuiltinModule(m) => Some(m.as_str()),
            _ => recv_module_name.filter(|_| matches!(resolved_recv, Type::Any | Type::TypeVar(_))),
        };
        if let Some(mod_n) = mod_name
            && let Some(tensor_res) =
                self.try_infer_module_tensor_method(mod_n, method, args, &arg_types, &acc_row, span)
        {
            return tensor_res;
        }
        let module_sig = mod_name
            .and_then(|module| crate::typeck::dispatch::module_method_signature(module, method));
        let has_resolved_sig = module_sig.is_some()
            || crate::typeck::dispatch::method_signature(&resolved_recv, method).is_some();
        let resolved_sig = module_sig
            .clone()
            .or_else(|| crate::typeck::dispatch::method_signature(&resolved_recv, method));
        let resolved_return_ty = resolved_sig.as_ref().map(|s| s.return_type.clone());
        let module_return_ty = module_sig.map(|s| s.return_type);

        if !has_resolved_sig
            && mod_name.is_none()
            && let Some(res) = self.try_infer_concrete_impl_method(
                &resolved_recv,
                method,
                args,
                named_args,
                recv_row,
                span,
            )
        {
            return res;
        }

        if !has_resolved_sig && mod_name.is_none() {
            let is_closed_transport = |t: &Type| {
                matches!(
                    t,
                    Type::TcpListener | Type::TcpStream | Type::TlsListener | Type::TlsStream
                )
            };
            let reject_unknown = is_closed_transport(&resolved_recv)
                || matches!(
                    &resolved_recv,
                    Type::Union(members) if !members.is_empty() && members.iter().all(is_closed_transport)
                );
            if reject_unknown {
                return Err(vec![TypeError::UnificationFailure {
                    expected: format!("valid method on '{}'", resolved_recv.name()),
                    got: format!("unknown method '.{method}()'"),
                    span: Some(span),
                }]);
            }
        }

        if let Some(sig) = resolved_sig {
            let user_arity = sig.params.len().saturating_sub(1);
            let min_arity = {
                let mut min = user_arity;
                while min > 0 {
                    let ty = &sig.params[min].1;
                    let optional = match ty {
                        Type::Nil => true,
                        Type::Union(members) => members.iter().any(|m| matches!(m, Type::Nil)),
                        _ => false,
                    };
                    if optional {
                        min -= 1;
                    } else {
                        break;
                    }
                }
                min
            };
            if sig.variadic {
                if arg_types.len() < min_arity {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: min_arity,
                        actual: arg_types.len(),
                        span,
                    }]);
                }
            } else {
                let extra_configurable = arg_types
                    .iter()
                    .skip(min_arity)
                    .all(|t| matches!(t, Type::Dict(_, _) | Type::Nil))
                    || arg_types
                        .iter()
                        .skip(user_arity)
                        .all(|t| matches!(t, Type::Dict(_, _)));
                if arg_types.len() < min_arity
                    || (arg_types.len() > user_arity && !extra_configurable)
                {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: min_arity,
                        actual: arg_types.len(),
                        span,
                    }]);
                }
            }
            for ((_param_name, param_ty), (arg, arg_ty)) in sig
                .params
                .iter()
                .skip(1)
                .zip(args.iter().zip(arg_types.iter()))
            {
                if matches!(arg_ty, Type::Error) || matches!(param_ty, Type::Any) {
                    continue;
                }
                self.constraints.push(SpannedConstraint::new(
                    Constraint::Subtype {
                        sub: Box::new(arg_ty.clone()),
                        sup: Box::new(param_ty.clone()),
                    },
                    arg.span,
                ));
            }
        }

        if arg_types.is_empty()
            && crate::typeck::dispatch::method_signature(&resolved_recv, method).is_none()
            && let Some(field_ty) = self.dict_field_type(&resolved_recv, method)
        {
            return Ok((field_ty, acc_row));
        }

        if mod_name == Some("math")
            && matches!(
                method,
                "abs" | "sign" | "floor" | "ceil" | "round" | "trunc"
            )
            && let Some(arg0) = arg_types.first()
            && !matches!(arg0, Type::Error)
        {
            if let Some(arg_w) = args.first() {
                self.constraints.push(SpannedConstraint::new(
                    Constraint::Subtype {
                        sub: Box::new(arg0.clone()),
                        sup: Box::new(Type::Union(vec![Type::Int, Type::Float])),
                    },
                    arg_w.span,
                ));
            }
            return Ok((arg0.clone(), acc_row));
        }

        let return_ty = module_return_ty
            .or(resolved_return_ty)
            .unwrap_or_else(|| crate::typeck::dispatch::method_return_type(&resolved_recv, method));
        let return_ty = if matches!(return_ty, Type::Unknown) {
            self.fresh_type_var()
        } else {
            return_ty
        };
        Ok((return_ty, acc_row))
    }

    fn dict_field_type(&self, recv_ty: &Type, _field: &str) -> Option<Type> {
        match recv_ty {
            Type::Dict(_, v) => Some(v.as_ref().clone()),
            _ => None,
        }
    }

    fn infer_random_method(
        &mut self,
        method: &str,
        arg_types: Vec<Type>,
        mut row: crate::mir::effect::EffectRow,
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let label = match crate::mir::effect::ambient::random_label_for_method(method) {
            Some(l) => l,
            None => {
                return Err(vec![TypeError::UnificationFailure {
                    expected: format!(
                        "known random method: random/{}",
                        crate::typeck::dispatch::RANDOM_METHODS.join("/")
                    ),
                    got: format!("random.{}", method),
                    span: Some(span),
                }]);
            }
        };
        let result_ty = match self.effect_signatures.get(label).cloned() {
            Some(sig) => {
                if sig.params.len() != arg_types.len() {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: sig.params.len(),
                        actual: arg_types.len(),
                        span,
                    }]);
                }
                for (a_ty, p_ty) in arg_types.iter().zip(sig.params.iter()) {
                    if matches!(a_ty, Type::Error) {
                        continue;
                    }
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Subtype {
                            sub: Box::new(a_ty.clone()),
                            sup: Box::new(p_ty.clone()),
                        },
                        span,
                    ));
                }
                sig.result
            }
            None => Type::Any,
        };
        row = self.merge_rows(
            row,
            crate::mir::effect::EffectRow::Cons(
                label.to_string(),
                Box::new(crate::mir::effect::EffectRow::Empty),
            ),
            span,
        );
        Ok((result_ty, row))
    }

    pub(super) fn infer_sequence(
        &mut self,
        exprs: &[MirWitness],
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        if exprs.is_empty() {
            return Ok((Type::Nil, crate::mir::effect::EffectRow::Empty));
        }
        let mut acc_row = crate::mir::effect::EffectRow::Empty;
        let mut last_ty: Option<Type> = None;
        for expr in exprs {
            let (ty, row) = self.infer_recover(expr);
            acc_row = self.merge_rows(acc_row, row, span);
            last_ty = Some(ty);
        }
        Ok((last_ty.unwrap_or(Type::Nil), acc_row))
    }

    pub(super) fn infer_closure_core(
        &mut self,
        param_types: &[Type],
        params: &[WitnessParam],
        expected_ret: Type,
        body: &MirWitness,
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let mut seen_names: HashSet<String> = HashSet::new();
        for p in params {
            if !seen_names.insert(p.name.clone()) {
                return Err(vec![TypeError::UnificationFailure {
                    expected: "distinct parameter names".to_string(),
                    got: format!("duplicate parameter `{}`", p.name),
                    span: Some(span),
                }]);
            }
        }
        let saved_env = self.env.clone();
        let saved_resources = (
            self.grad_tracked_tensors.clone(),
            self.view_aliased_tensors.clone(),
            self.aliased_source_tensors.clone(),
        );
        for (p, ty) in params.iter().zip(param_types.iter()) {
            if let Some(def_expr) = &p.default {
                let (def_ty, _def_row) = self.infer_recover(def_expr);
                if !matches!(def_ty, Type::Error) {
                    self.constraints.push(
                        SpannedConstraint::new(
                            Constraint::Subtype {
                                sub: Box::new(def_ty),
                                sup: Box::new(ty.clone()),
                            },
                            def_expr.span,
                        )
                        .with_hint(format!(
                            "default value for parameter '{}' must match its parameter type",
                            p.name
                        )),
                    );
                }
            }
            self.record_binding(span, &p.name, ty);
            self.env.add(p.name.clone(), ty.clone());
            self.grad_tracked_tensors.remove(&p.name);
            self.view_aliased_tensors.remove(&p.name);
            self.aliased_source_tensors.remove(&p.name);
        }
        self.return_targets.push(expected_ret);
        self.closure_depth += 1;
        let body_res = self.infer_expr(body);
        self.closure_depth -= 1;
        self.return_targets.pop();
        self.env = saved_env;
        self.grad_tracked_tensors = saved_resources.0;
        self.view_aliased_tensors = saved_resources.1;
        self.aliased_source_tensors = saved_resources.2;
        body_res
    }

    pub(super) fn infer_closure(
        &mut self,
        type_params: &[String],
        params: &[WitnessParam],
        body: &MirWitness,
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let param_types: Vec<Type> = params
            .iter()
            .map(|p| {
                p.type_hint
                    .as_ref()
                    .map(|h| self.resolve_type_aliases(h.to_type()))
                    .unwrap_or_else(|| self.fresh_type_var())
            })
            .collect();
        let ret_target = self.fresh_type_var();
        let (body_ty, body_row) =
            self.infer_closure_core(&param_types, params, ret_target.clone(), body, span)?;
        if !matches!(body_ty, Type::Error) {
            self.constraints.push(SpannedConstraint::new(
                Constraint::Subtype {
                    sub: Box::new(body_ty.clone()),
                    sup: Box::new(ret_target),
                },
                body.span,
            ));
        }
        let arrow = Type::curried_arrow(&param_types, body_ty, &body_row);
        let mut rigids: Vec<TypeVarId> = type_params
            .iter()
            .map(|p| TypeVarId::Rigid(p.clone()))
            .collect();
        for v in env::collect_type_vars(&arrow) {
            if matches!(v, TypeVarId::Rigid(_)) && !rigids.contains(&v) {
                rigids.push(v);
            }
        }
        let ty = if rigids.is_empty() {
            arrow
        } else {
            Type::ForAll(rigids, Box::new(arrow))
        };
        self.closure_rows.insert(span, body_row);
        let min_arity = params.iter().filter(|p| p.default.is_none()).count();
        self.closure_param_specs.insert(
            span,
            FnParamSpec {
                names: params.iter().map(|p| p.name.clone()).collect(),
                has_default: params.iter().map(|p| p.default.is_some()).collect(),
                min_arity,
            },
        );
        Ok((ty, crate::mir::effect::EffectRow::Empty))
    }

    pub(super) fn infer_macro_def(
        &mut self,
        name: &str,
        params: &[String],
        body: &MirWitness,
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let params: Vec<WitnessParam> = params
            .iter()
            .map(|p| WitnessParam {
                name: p.clone(),
                type_hint: None,
                default: None,
            })
            .collect();
        self.infer_fn_def(Some(name), &[], &params, None, body, span)
    }

    pub(super) fn infer_fn_def(
        &mut self,
        name: Option<&str>,
        type_params: &[String],
        params: &[WitnessParam],
        return_type: Option<&Type>,
        body: &MirWitness,
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let (param_types, ret_placeholder) = if let Some(n) = name
            && let Some(ph) = self.fn_placeholders.get(n).cloned()
        {
            ph
        } else {
            let pts: Vec<Type> = params
                .iter()
                .map(|p| {
                    p.type_hint
                        .as_ref()
                        .map(|h| self.resolve_type_aliases(h.to_type()))
                        .unwrap_or_else(|| self.fresh_type_var())
                })
                .collect();
            let rt = return_type
                .map(|t| self.resolve_type_aliases(t))
                .unwrap_or_else(|| self.fresh_type_var());
            (pts, rt)
        };

        let mut ph_metas: HashSet<TypeVarId> = HashSet::new();
        for t in &param_types {
            for v in generalize::collect_free_vars(t) {
                if matches!(v, TypeVarId::Meta(_)) {
                    ph_metas.insert(v);
                }
            }
        }
        for v in generalize::collect_free_vars(&ret_placeholder) {
            if matches!(v, TypeVarId::Meta(_)) {
                ph_metas.insert(v);
            }
        }
        let was_forward_referenced = !ph_metas.is_empty()
            && self
                .constraints
                .iter()
                .any(|sc| unify::constraint_mentions_any(&sc.kind, &ph_metas));

        let c_start = self.constraints.len();
        let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
        self.active_fn_mut_stack.push((param_names, HashSet::new()));
        let body_res =
            self.infer_closure_core(&param_types, params, ret_placeholder.clone(), body, span);
        let (_, mutated_param_set) = self
            .active_fn_mut_stack
            .pop()
            .unwrap_or_else(|| (Vec::new(), HashSet::new()));
        let (body_ty, body_row) = body_res?;
        if let Some(n) = name
            && !mutated_param_set.is_empty()
        {
            let mut indices: Vec<usize> = mutated_param_set.into_iter().collect();
            indices.sort_unstable();
            self.fn_mutates_tensor_params.insert(n.to_string(), indices);
        }

        if !matches!(body_ty, Type::Error) {
            self.constraints.push(
                SpannedConstraint::new(
                    Constraint::Subtype {
                        sub: Box::new(body_ty.clone()),
                        sup: Box::new(ret_placeholder.clone()),
                    },
                    body.span,
                )
                .with_hint("return expression must match function return type"),
            );
        }

        let effective_ret = if return_type.is_some() {
            ret_placeholder.clone()
        } else if !matches!(body_ty, Type::Error) && !was_forward_referenced {
            self.constraints.push(SpannedConstraint::new(
                Constraint::Eq(Box::new(ret_placeholder.clone()), Box::new(body_ty.clone())),
                body.span,
            ));
            body_ty
        } else {
            ret_placeholder.clone()
        };

        let arrow = Type::curried_arrow(&param_types, effective_ret, &body_row);
        let mut rigids: Vec<TypeVarId> = type_params
            .iter()
            .map(|p| TypeVarId::Rigid(p.clone()))
            .collect();
        for v in env::collect_type_vars(&arrow) {
            if matches!(v, TypeVarId::Rigid(_)) && !rigids.contains(&v) {
                rigids.push(v);
            }
        }
        let final_ty = if !rigids.is_empty() {
            let drained: Vec<SpannedConstraint> = self.constraints.drain(c_start..).collect();
            for sc in drained {
                match &sc.kind {
                    Constraint::Numeric(_) => self.constraints.push(sc),
                    _ => match unify::solve_spanned(&sc, &self.subst) {
                        Ok(s) => self.subst = s,
                        Err(e) => self.errors.push(e),
                    },
                }
            }
            Type::ForAll(rigids, Box::new(self.subst.apply(&arrow)))
        } else if let Some(n) = name
            && !was_forward_referenced
        {
            self.generalize_pure_binding(Some(n), &arrow, c_start)
        } else {
            arrow
        };

        if let Some(n) = name {
            self.record_binding(span, n, &final_ty);
            self.env.add(n.to_string(), final_ty.clone());
            self.fn_arities.entry(n.to_string()).or_insert(params.len());
            let merged = match self.fn_effect_rows.get(n) {
                Some(prev) => Self::union_effect_rows(prev, &body_row),
                None => body_row.clone(),
            };
            self.fn_effect_rows.insert(n.to_string(), merged);
        }
        self.closure_rows.insert(span, body_row);
        Ok((final_ty, crate::mir::effect::EffectRow::Empty))
    }

    pub(super) fn infer_match(
        &mut self,
        scrutinee: &MirWitness,
        arms: &[WitnessArm],
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let (scrutinee_ty, scrutinee_row) = self.infer_recover(scrutinee);
        let mut acc_row = scrutinee_row;
        let mut result_ty: Option<Type> = None;
        for arm in arms {
            if !matches!(scrutinee_ty, Type::Error)
                && let Err(errs) = self.infer_pattern(&arm.pattern, &scrutinee_ty, span)
            {
                self.errors.extend(errs);
            }

            let saved_env = self.env.clone();
            let saved_res = (
                self.grad_tracked_tensors.clone(),
                self.view_aliased_tensors.clone(),
                self.aliased_source_tensors.clone(),
            );
            if let Err(errs) = self.add_pattern_bindings(&arm.pattern, &scrutinee_ty, span) {
                self.errors.extend(errs);
            }
            if let Some(guard) = &arm.guard {
                let (guard_ty, guard_row) = self.infer_recover(guard);
                acc_row = self.merge_rows(acc_row, guard_row, guard.span);
                if !matches!(guard_ty, Type::Error) {
                    self.constraints.push(
                        SpannedConstraint::new(
                            Constraint::Subtype {
                                sub: Box::new(guard_ty),
                                sup: Box::new(Type::Bool),
                            },
                            guard.span,
                        )
                        .with_hint("match guard must be bool"),
                    );
                }
            }
            let (arm_ty, arm_row) = self.infer_recover(&arm.body);
            self.env = saved_env;
            self.grad_tracked_tensors = saved_res.0;
            self.view_aliased_tensors = saved_res.1;
            self.aliased_source_tensors = saved_res.2;
            acc_row = self.merge_rows(acc_row, arm_row, arm.body.span);
            self.merge_arm_result_type(&mut result_ty, arm_ty, arm.body.span);
        }
        Ok((result_ty.unwrap_or(Type::Nil), acc_row))
    }

    fn merge_arm_result_type(
        &mut self,
        result_ty: &mut Option<Type>,
        arm_ty: Type,
        arm_span: Span,
    ) {
        if matches!(arm_ty, Type::Error) {
            return;
        }
        match result_ty.take() {
            None => *result_ty = Some(arm_ty),
            Some(prev) => {
                if matches!(prev, Type::Error) {
                    *result_ty = Some(arm_ty);
                } else if matches!(
                    (&prev, &arm_ty),
                    (Type::Int, Type::Float) | (Type::Float, Type::Int)
                ) {
                    *result_ty = Some(Type::Float);
                } else if !matches!(prev, Type::TypeVar(_))
                    && !matches!(arm_ty, Type::TypeVar(_))
                    && !arm_ty.subtype_of(&prev)
                    && prev.subtype_of(&arm_ty)
                {
                    *result_ty = Some(arm_ty);
                } else {
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Subtype {
                            sub: Box::new(arm_ty),
                            sup: Box::new(prev.clone()),
                        },
                        arm_span,
                    ));
                    *result_ty = Some(prev);
                }
            }
        }
    }

    pub(super) fn infer_spawn(
        &mut self,
        target: &crate::mir::witness::WitnessSpawnTarget,
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        use crate::mir::witness::WitnessSpawnTarget;
        let (ret_ty, spawned_row) = match target {
            WitnessSpawnTarget::Call {
                callee,
                args,
                named_args,
            } => self.infer_call(callee, args, named_args, span)?,
            WitnessSpawnTarget::MethodCall {
                receiver,
                method,
                args,
                named_args,
            } => self.infer_method_call(receiver, method, args, named_args, span)?,
            WitnessSpawnTarget::Expr(expr) => {
                let (expr_ty, expr_row) = self.infer_expr(expr)?;
                let resolved = self.subst.apply(&expr_ty);
                match resolved {
                    Type::Error => (Type::Error, expr_row),
                    Type::Any | Type::Task | Type::Closure | Type::Builtin => (Type::Any, expr_row),
                    Type::Arrow(inp, out, eff_row) => {
                        if !matches!(&*inp, Type::Tuple(elems) if elems.is_empty())
                            && !matches!(&*inp, Type::Any | Type::Union(_))
                        {
                            return Err(vec![TypeError::ArityMismatch {
                                expected: 1,
                                actual: 0,
                                span,
                            }]);
                        }
                        let merged = self.merge_rows(expr_row, eff_row, span);
                        (*out, merged)
                    }
                    Type::ForAll(_, _) | Type::TypeVar(_) => {
                        self.apply_curried_call_types(expr_ty, expr_row, Vec::new(), span)
                    }
                    other => {
                        return Err(vec![TypeError::NotAClosure {
                            found: other.name(),
                            span: expr.span,
                        }]);
                    }
                }
            }
        };
        Ok((Type::TaskHandle(Box::new(ret_ty)), spawned_row))
    }

    pub(super) fn infer_select(
        &mut self,
        arms: &[crate::mir::witness::WitnessSelectArm],
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        use crate::mir::witness::WitnessSelectArm;
        if arms.is_empty() {
            return Err(vec![TypeError::UnificationFailure {
                expected: "at least one arm in select".to_string(),
                got: "empty select".to_string(),
                span: Some(span),
            }]);
        }
        let mut acc_row = crate::mir::effect::EffectRow::Empty;
        let mut result_ty: Option<Type> = None;

        for arm in arms {
            match arm {
                WitnessSelectArm::Recv {
                    binding,
                    channel,
                    body,
                } => {
                    let (ch_ty, ch_row) = self.infer_recover(channel);
                    acc_row = self.merge_rows(acc_row, ch_row, channel.span);
                    let elem_ty = self.fresh_type_var();
                    if !matches!(ch_ty, Type::Error) {
                        self.constraints.push(
                            SpannedConstraint::new(
                                Constraint::Subtype {
                                    sub: Box::new(ch_ty),
                                    sup: Box::new(Type::Channel(Box::new(elem_ty.clone()))),
                                },
                                channel.span,
                            )
                            .with_hint("select recv arm requires a channel<T>"),
                        );
                    }
                    let saved_env = self.env.clone();
                    if let Some(name) = binding {
                        self.record_binding(channel.span, name, &elem_ty);
                        self.env.add(name.clone(), elem_ty);
                    }
                    let (body_ty, body_row) = self.infer_recover(body);
                    self.env = saved_env;
                    acc_row = self.merge_rows(acc_row, body_row, body.span);
                    self.merge_arm_result_type(&mut result_ty, body_ty, body.span);
                }
                WitnessSelectArm::Send {
                    channel,
                    value,
                    body,
                } => {
                    let (ch_ty, ch_row) = self.infer_recover(channel);
                    let (val_ty, val_row) = self.infer_recover(value);
                    acc_row = self.merge_rows(acc_row, ch_row, channel.span);
                    acc_row = self.merge_rows(acc_row, val_row, value.span);
                    let elem_ty = self.fresh_type_var();
                    if !matches!(ch_ty, Type::Error) {
                        self.constraints.push(
                            SpannedConstraint::new(
                                Constraint::Subtype {
                                    sub: Box::new(ch_ty),
                                    sup: Box::new(Type::Channel(Box::new(elem_ty.clone()))),
                                },
                                channel.span,
                            )
                            .with_hint("select send arm requires a channel<T>"),
                        );
                    }
                    if !matches!(val_ty, Type::Error) {
                        self.constraints.push(
                            SpannedConstraint::new(
                                Constraint::Subtype {
                                    sub: Box::new(val_ty),
                                    sup: Box::new(elem_ty),
                                },
                                value.span,
                            )
                            .with_hint("select send value must match channel element type"),
                        );
                    }
                    let (body_ty, body_row) = self.infer_recover(body);
                    acc_row = self.merge_rows(acc_row, body_row, body.span);
                    self.merge_arm_result_type(&mut result_ty, body_ty, body.span);
                }
                WitnessSelectArm::Timeout { ms, body } => {
                    let (ms_ty, ms_row) = self.infer_recover(ms);
                    acc_row = self.merge_rows(acc_row, ms_row, ms.span);
                    if !matches!(ms_ty, Type::Error) {
                        self.constraints.push(
                            SpannedConstraint::new(
                                Constraint::Subtype {
                                    sub: Box::new(ms_ty),
                                    sup: Box::new(Type::Union(vec![Type::Int, Type::Float])),
                                },
                                ms.span,
                            )
                            .with_hint("select timeout duration must be numeric"),
                        );
                    }
                    let (body_ty, body_row) = self.infer_recover(body);
                    acc_row = self.merge_rows(acc_row, body_row, body.span);
                    self.merge_arm_result_type(&mut result_ty, body_ty, body.span);
                }
                WitnessSelectArm::Default { body } => {
                    let (body_ty, body_row) = self.infer_recover(body);
                    acc_row = self.merge_rows(acc_row, body_row, body.span);
                    self.merge_arm_result_type(&mut result_ty, body_ty, body.span);
                }
            }
        }
        Ok((result_ty.unwrap_or(Type::Nil), acc_row))
    }

    pub(super) fn infer_pattern(
        &mut self,
        pattern: &crate::mir::witness::WitnessPattern,
        scrutinee_ty: &Type,
        span: Span,
    ) -> Result<(), Vec<TypeError>> {
        use crate::mir::witness::WitnessPattern;
        match pattern {
            WitnessPattern::Wildcard | WitnessPattern::Variable(_) | WitnessPattern::Literal(_) => {
                Ok(())
            }
            WitnessPattern::Tuple(items) => {
                let elem_count = match scrutinee_ty {
                    Type::List(_) | Type::Tuple(_) | Type::Any | Type::TypeVar(_) => None,
                    _ => Some(1),
                };
                if let Some(expected) = elem_count
                    && items.len() != expected
                {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!("Tuple/List ({} elements)", expected),
                        got: format!("Tuple ({} elements)", items.len()),
                        span: Some(span),
                    }]);
                }
                self.infer_pattern(
                    items.first().unwrap_or(&WitnessPattern::Wildcard),
                    scrutinee_ty,
                    span,
                )?;
                Ok(())
            }
            WitnessPattern::List { head, tail } => {
                let elem_ty = match scrutinee_ty {
                    Type::List(e) => e.as_ref().clone(),
                    Type::TypeVar(_) | Type::Any => self.fresh_type_var(),
                    _ => {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: "List".to_string(),
                            got: scrutinee_ty.name(),
                            span: Some(span),
                        }]);
                    }
                };
                self.infer_pattern(head, &elem_ty, span)?;
                let rest_list_ty = Type::List(Box::new(elem_ty));
                self.infer_pattern(tail, &rest_list_ty, span)?;
                Ok(())
            }
            WitnessPattern::ListVec { elements, rest } => {
                let elem_ty = match scrutinee_ty {
                    Type::List(e) => e.as_ref().clone(),
                    Type::TypeVar(_) | Type::Any => self.fresh_type_var(),
                    _ => {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: "List".to_string(),
                            got: scrutinee_ty.name(),
                            span: Some(span),
                        }]);
                    }
                };
                for e in elements {
                    self.infer_pattern(e, &elem_ty, span)?;
                }
                if let Some(r) = rest {
                    let rest_list_ty = Type::List(Box::new(elem_ty));
                    self.infer_pattern(r, &rest_list_ty, span)?;
                }
                Ok(())
            }
            WitnessPattern::Dict { required, rest: _ } => {
                let value_ty = match scrutinee_ty {
                    Type::Dict(_, v) => v.as_ref().clone(),
                    Type::TypeVar(_) | Type::Any => self.fresh_type_var(),
                    _ => {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: "Dict".to_string(),
                            got: scrutinee_ty.name(),
                            span: Some(span),
                        }]);
                    }
                };
                for (_key, value_pat) in required {
                    self.infer_pattern(value_pat, &value_ty, span)?;
                }
                Ok(())
            }
            WitnessPattern::TypeAscription { name, pattern } => {
                if !is_known_type(name) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: "known type name".to_string(),
                        got: name.clone(),
                        span: Some(span),
                    }]);
                }
                self.infer_pattern(pattern, scrutinee_ty, span)?;
                Ok(())
            }
            WitnessPattern::Variant { .. } | WitnessPattern::Struct { .. } => {
                self.infer_pattern_variant_or_struct(pattern, scrutinee_ty, span)
            }
        }
    }

    pub(crate) fn add_pattern_bindings(
        &mut self,
        pattern: &crate::mir::witness::WitnessPattern,
        _scrutinee_ty: &Type,
        span: Span,
    ) -> Result<(), Vec<TypeError>> {
        use crate::mir::witness::WitnessPattern;
        match pattern {
            WitnessPattern::Wildcard | WitnessPattern::Literal(_) => Ok(()),
            WitnessPattern::Variable(name) => {
                let ty = self.fresh_type_var();
                self.record_binding(span, name, &ty);
                self.env.add(name.clone(), ty);
                Ok(())
            }
            WitnessPattern::Tuple(items) => {
                for item in items {
                    self.add_pattern_bindings(item, _scrutinee_ty, span)?;
                }
                Ok(())
            }
            WitnessPattern::List { head, tail } => {
                self.add_pattern_bindings(head, _scrutinee_ty, span)?;
                self.add_pattern_bindings(tail, _scrutinee_ty, span)?;
                Ok(())
            }
            WitnessPattern::ListVec { elements, rest } => {
                for e in elements {
                    self.add_pattern_bindings(e, _scrutinee_ty, span)?;
                }
                if let Some(r) = rest {
                    self.add_pattern_bindings(r, _scrutinee_ty, span)?;
                }
                Ok(())
            }
            WitnessPattern::Dict { required, rest: _ } => {
                for (_key, value_pat) in required {
                    self.add_pattern_bindings(value_pat, _scrutinee_ty, span)?;
                }
                Ok(())
            }
            WitnessPattern::TypeAscription {
                name: _name,
                pattern,
            } => {
                self.add_pattern_bindings(pattern, _scrutinee_ty, span)?;
                Ok(())
            }
            WitnessPattern::Variant { .. } | WitnessPattern::Struct { .. } => {
                self.bind_pattern_variant_or_struct(pattern, _scrutinee_ty, span)
            }
        }
    }

    pub(super) fn infer_if(
        &mut self,
        cond: &MirWitness,
        then_branch: &MirWitness,
        else_branch: Option<&MirWitness>,
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let (cond_ty, cond_row) = self.infer_recover(cond);
        if !matches!(cond_ty, Type::Error) {
            self.constraints.push(
                SpannedConstraint::new(
                    Constraint::Subtype {
                        sub: Box::new(cond_ty),
                        sup: Box::new(Type::Bool),
                    },
                    cond.span,
                )
                .with_hint("if condition must be bool"),
            );
        }
        let (then_ty, then_row) = self.infer_recover(then_branch);
        let mut acc_row = self.merge_rows(cond_row, then_row, span);
        let result = if let Some(e) = else_branch {
            let (else_ty, else_row) = self.infer_recover(e);
            acc_row = self.merge_rows(acc_row, else_row, span);
            if matches!(then_ty, Type::Error) {
                else_ty
            } else if matches!(else_ty, Type::Error) {
                then_ty
            } else if matches!(
                (&then_ty, &else_ty),
                (Type::Int, Type::Float) | (Type::Float, Type::Int)
            ) {
                Type::Float
            } else if !matches!(then_ty, Type::TypeVar(_))
                && !matches!(else_ty, Type::TypeVar(_))
                && !else_ty.subtype_of(&then_ty)
                && then_ty.subtype_of(&else_ty)
            {
                else_ty
            } else {
                self.constraints.push(SpannedConstraint::new(
                    Constraint::Subtype {
                        sub: Box::new(else_ty),
                        sup: Box::new(then_ty.clone()),
                    },
                    e.span,
                ));
                then_ty
            }
        } else {
            Type::Union(vec![then_ty, Type::Nil])
        };
        Ok((result, acc_row))
    }

    fn finalize_container_elements(
        &mut self,
        items: &[(Type, Span)],
        joined: Option<Type>,
    ) -> Type {
        let target = joined.unwrap_or_else(|| self.fresh_type_var());
        for (ty, item_span) in items {
            if !matches!(ty, Type::Error) {
                self.constraints.push(SpannedConstraint::new(
                    Constraint::Subtype {
                        sub: Box::new(ty.clone()),
                        sup: Box::new(target.clone()),
                    },
                    *item_span,
                ));
            }
        }
        target
    }

    pub(super) fn infer_list(
        &mut self,
        items: &[MirWitness],
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let mut acc_row = crate::mir::effect::EffectRow::Empty;
        let mut first_ty: Option<Type> = None;
        let mut acc_elem: Option<Type> = None;
        let mut valid: Vec<(Type, Span)> = Vec::with_capacity(items.len());
        for (idx, item) in items.iter().enumerate() {
            let (ty, item_row) = self.infer_recover(item);
            acc_row = self.merge_rows(acc_row, item_row, span);
            if matches!(ty, Type::Error) {
                continue;
            }
            if let Some(prev) = &first_ty
                && !ty.compatible_with(prev)
            {
                self.errors.push(TypeError::ListElementTypeMismatch {
                    index: idx,
                    expected: prev.name(),
                    got: ty.name(),
                    span: Some(item.span),
                });
                continue;
            }
            if first_ty.is_none() {
                first_ty = Some(ty.clone());
            }
            acc_elem = Some(match acc_elem {
                None => ty.clone(),
                Some(prev) => promote_list_elem(&prev, &ty),
            });
            valid.push((ty, item.span));
        }
        let elem = self.finalize_container_elements(&valid, acc_elem);
        Ok((Type::List(Box::new(elem)), acc_row))
    }

    pub(super) fn infer_dict(
        &mut self,
        entries: &[(String, MirWitness)],
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let k_ty = Type::String;
        let mut acc_row = crate::mir::effect::EffectRow::Empty;
        let mut first_v: Option<Type> = None;
        let mut acc_val: Option<Type> = None;
        let mut valid: Vec<(Type, Span)> = Vec::with_capacity(entries.len());
        for (key, value) in entries {
            let (ty, val_row) = self.infer_recover(value);
            acc_row = self.merge_rows(acc_row, val_row, span);
            if matches!(ty, Type::Error) {
                continue;
            }
            if let Some(prev) = &first_v
                && !ty.compatible_with(prev)
            {
                self.errors.push(TypeError::DictValueTypeMismatch {
                    key: key.clone(),
                    expected: prev.name(),
                    got: ty.name(),
                    span: Some(value.span),
                });
                continue;
            }
            if first_v.is_none() {
                first_v = Some(ty.clone());
            }
            acc_val = Some(match acc_val {
                None => ty.clone(),
                Some(prev) => promote_list_elem(&prev, &ty),
            });
            valid.push((ty, value.span));
        }
        let val_ty = self.finalize_container_elements(&valid, acc_val);
        Ok((Type::Dict(Box::new(k_ty), Box::new(val_ty)), acc_row))
    }

    // ════════════════════════════════════════════════════════════════════════
    // Stage 3B: 5 AI-Native Static Checks (DType, Shape, Device, Resource, Effect)
    // ════════════════════════════════════════════════════════════════════════

    fn update_tensor_resource_flags(&mut self, name: &str, value: &MirWitness) {
        if self.expr_produces_grad_tensor(value) {
            self.grad_tracked_tensors.insert(name.to_string());
        } else {
            self.grad_tracked_tensors.remove(name);
        }
        if self.expr_produces_tensor_view(value) {
            self.view_aliased_tensors.insert(name.to_string());
            if let Some(src_var) = Self::tensor_view_source_var(value)
                && src_var != name
            {
                self.aliased_source_tensors.insert(src_var);
            }
        } else {
            self.view_aliased_tensors.remove(name);
            // Reassigning `name` to a fresh contiguous/owning tensor clears prior source aliasing on `name`.
            self.aliased_source_tensors.remove(name);
        }
        // Creating a second variable alias to an existing tensor shares `Arc<TensorStorage>` at runtime.
        if let WitnessKind::Variable(src_var) = &value.kind
            && src_var != name
            && self
                .env
                .get(src_var)
                .map(|t| self.subst.apply(t))
                .is_some_and(|t| matches!(t, Type::Tensor { .. }))
        {
            self.aliased_source_tensors.insert(src_var.clone());
            self.aliased_source_tensors.insert(name.to_string());
        }
    }

    fn tensor_view_source_var(w: &MirWitness) -> Option<String> {
        match &w.kind {
            WitnessKind::Variable(v) => Some(v.clone()),
            WitnessKind::MethodCall {
                receiver, method, ..
            } => match method.as_str() {
                "transpose" | "reshape" | "with_grad" => Self::tensor_view_source_var(receiver),
                _ => None,
            },
            WitnessKind::TensorSlice { target, .. } => Self::tensor_view_source_var(target),
            WitnessKind::Call { callee, args, .. } => {
                if matches!(callee, WitnessCallee::Name(n) | WitnessCallee::Var(n) if n == "[]") {
                    args.first().and_then(Self::tensor_view_source_var)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn expr_is_aliased_source(&self, w: &MirWitness) -> bool {
        match &w.kind {
            WitnessKind::Variable(v) => self.aliased_source_tensors.contains(v),
            WitnessKind::MethodCall {
                receiver, method, ..
            } if method == "with_grad" => self.expr_is_aliased_source(receiver),
            _ => false,
        }
    }

    fn expr_produces_grad_tensor(&self, w: &MirWitness) -> bool {
        match &w.kind {
            WitnessKind::Variable(v) => self.grad_tracked_tensors.contains(v),
            WitnessKind::TensorSlice { target, .. } => self.expr_produces_grad_tensor(target),
            WitnessKind::NoGrad { .. } => false,
            WitnessKind::Call {
                callee,
                args,
                named_args,
            } => {
                if let WitnessCallee::Name(n) | WitnessCallee::Var(n) = callee {
                    if let Some((_, rg)) = named_args.iter().find(|(k, _)| k == "requires_grad") {
                        return matches!(
                            &rg.kind,
                            WitnessKind::Literal(crate::common::Literal::Bool(true, _))
                        );
                    }
                    if n == "tensor"
                        || n.eq_ignore_ascii_case("Tensor::from_list")
                        || n.eq_ignore_ascii_case("Tensor::zeros")
                        || n.eq_ignore_ascii_case("Tensor::ones")
                        || n.eq_ignore_ascii_case("Tensor::randn")
                        || n.eq_ignore_ascii_case("Tensor::rand")
                    {
                        return matches!(
                            args.get(3).map(|a| &a.kind),
                            Some(WitnessKind::Literal(crate::common::Literal::Bool(true, _)))
                        );
                    }
                    if n.eq_ignore_ascii_case("Tensor::full") {
                        return matches!(
                            args.get(4).map(|a| &a.kind),
                            Some(WitnessKind::Literal(crate::common::Literal::Bool(true, _)))
                        );
                    }
                    if n.eq_ignore_ascii_case("Tensor::eye")
                        || n.eq_ignore_ascii_case("Tensor::arange")
                    {
                        return args.iter().any(|a| {
                            matches!(
                                &a.kind,
                                WitnessKind::Literal(crate::common::Literal::Bool(true, _))
                            )
                        });
                    }
                    if n == "[]"
                        && let Some(recv) = args.first()
                    {
                        return self.expr_produces_grad_tensor(recv);
                    }
                }
                false
            }
            WitnessKind::MethodCall {
                receiver,
                method,
                args,
                ..
            } => match method.as_str() {
                "with_grad" => match args.first().map(|a| &a.kind) {
                    None | Some(WitnessKind::Literal(crate::common::Literal::Bool(true, _))) => {
                        true
                    }
                    Some(WitnessKind::Literal(crate::common::Literal::Bool(false, _))) => false,
                    _ => true,
                },
                "reshape" | "transpose" | "contiguous" | "to" | "to_dtype" | "to_device"
                | "abs" | "sqrt" | "exp" | "ln" | "relu" | "gelu" | "silu" | "tanh" | "sigmoid"
                | "softmax" | "rms_norm" | "layer_norm" => self.expr_produces_grad_tensor(receiver),
                _ => false,
            },
            WitnessKind::Binary { left, right, .. } => {
                self.expr_produces_grad_tensor(left) || self.expr_produces_grad_tensor(right)
            }
            _ => false,
        }
    }

    fn expr_produces_tensor_view(&self, w: &MirWitness) -> bool {
        match &w.kind {
            WitnessKind::Variable(v) => self.view_aliased_tensors.contains(v),
            WitnessKind::TensorSlice { .. } => true,
            WitnessKind::MethodCall {
                receiver, method, ..
            } => match method.as_str() {
                "transpose" | "reshape" => true,
                "contiguous" | "to" | "to_dtype" | "to_device" => false,
                "with_grad" => self.expr_produces_tensor_view(receiver),
                _ => false,
            },
            WitnessKind::Call { callee, args, .. } => {
                matches!(callee, WitnessCallee::Name(n) | WitnessCallee::Var(n) if n == "[]")
                    && !args.is_empty()
            }
            _ => false,
        }
    }

    fn infer_scalar_binary_op(
        &mut self,
        left_ty: &Type,
        right_ty: &Type,
        is_cmp: bool,
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        match (left_ty, right_ty) {
            (Type::Scalar(dt1), Type::Scalar(dt2)) => {
                if dt1 == dt2 {
                    Ok(if is_cmp {
                        Type::Bool
                    } else {
                        Type::Scalar(*dt1)
                    })
                } else {
                    Err(vec![TypeError::UnificationFailure {
                        expected: format!(
                            "matching scalar DType '{}' (no implicit cross-precision coercion)",
                            dt1.canonical_type_name()
                        ),
                        got: format!("{} and {}", left_ty.name(), right_ty.name()),
                        span: Some(span),
                    }])
                }
            }
            (Type::Scalar(dt), Type::TypeVar(TypeVarId::Meta(_)))
            | (Type::TypeVar(TypeVarId::Meta(_)), Type::Scalar(dt)) => {
                let sty = Type::Scalar(*dt);
                let eq_c = SpannedConstraint::new(
                    Constraint::Eq(Box::new(left_ty.clone()), Box::new(right_ty.clone())),
                    span,
                );
                match unify::solve_spanned(&eq_c, &self.subst) {
                    Ok(new_s) => self.subst = new_s,
                    Err(e) => return Err(vec![e]),
                }
                Ok(if is_cmp { Type::Bool } else { sty })
            }
            (Type::Scalar(dt), Type::Any) | (Type::Any, Type::Scalar(dt)) => Ok(if is_cmp {
                Type::Bool
            } else {
                Type::Scalar(*dt)
            }),
            _ => Err(vec![TypeError::UnificationFailure {
                expected: format!(
                    "matching scalar DType (cannot mix {} with {})",
                    left_ty.name(),
                    right_ty.name()
                ),
                got: format!("{} and {}", left_ty.name(), right_ty.name()),
                span: Some(span),
            }]),
        }
    }

    fn unify_tensor_dtypes(
        &mut self,
        d1: &Type,
        d2: &Type,
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        let a = self.subst.apply(d1);
        let b = self.subst.apply(d2);
        let eq_c = SpannedConstraint::new(
            Constraint::Eq(Box::new(a.clone()), Box::new(b.clone())),
            span,
        );
        match unify::solve_spanned(&eq_c, &self.subst) {
            Ok(new_s) => {
                self.subst = new_s;
                Ok(self.subst.apply(&a))
            }
            Err(_) => Err(vec![TypeError::UnificationFailure {
                expected: format!(
                    "matching tensor DType '{}' (use .to(\"{}\") for explicit conversion)",
                    a.name(),
                    a.name()
                ),
                got: format!("{} and {}", a.name(), b.name()),
                span: Some(span),
            }]),
        }
    }

    fn unify_tensor_devices(
        &mut self,
        dev1: &crate::typeck::DeviceType,
        dev2: &crate::typeck::DeviceType,
        span: Span,
    ) -> Result<crate::typeck::DeviceType, Vec<TypeError>> {
        use crate::typeck::DeviceType;
        let a = self.subst.apply_device(dev1);
        let b = self.subst.apply_device(dev2);
        let dummy1 = Type::Tensor {
            dtype: Box::new(Type::Any),
            shape: crate::typeck::ShapeType::Unranked(None),
            device: a.clone(),
        };
        let dummy2 = Type::Tensor {
            dtype: Box::new(Type::Any),
            shape: crate::typeck::ShapeType::Unranked(None),
            device: b.clone(),
        };
        let eq_c = SpannedConstraint::new(Constraint::Eq(Box::new(dummy1), Box::new(dummy2)), span);
        match unify::solve_spanned(&eq_c, &self.subst) {
            Ok(new_s) => {
                self.subst = new_s;
                let resolved = if matches!(a, DeviceType::Any) {
                    self.subst.apply_device(&b)
                } else {
                    self.subst.apply_device(&a)
                };
                Ok(resolved)
            }
            Err(_) => Err(vec![TypeError::UnificationFailure {
                expected: format!(
                    "tensors on same device '{}' (use .to(\"{}\") to transfer explicitly)",
                    a, a
                ),
                got: format!("'{}' and '{}'", a, b),
                span: Some(span),
            }]),
        }
    }

    fn broadcast_tensor_shapes(
        &mut self,
        s1: &crate::typeck::ShapeType,
        s2: &crate::typeck::ShapeType,
        span: Span,
    ) -> Result<crate::typeck::ShapeType, Vec<TypeError>> {
        use crate::typeck::{DimExpr, ShapeType};
        let a = self.subst.apply_shape(s1);
        let b = self.subst.apply_shape(s2);
        match (&a, &b) {
            (ShapeType::Ranked(dims_a), ShapeType::Ranked(dims_b)) => {
                let max_rank = dims_a.len().max(dims_b.len());
                let mut out_dims = Vec::with_capacity(max_rank);
                for i in 0..max_rank {
                    let da = if i < max_rank - dims_a.len() {
                        DimExpr::Known(1)
                    } else {
                        self.subst.apply_dim(&dims_a[i - (max_rank - dims_a.len())])
                    };
                    let db = if i < max_rank - dims_b.len() {
                        DimExpr::Known(1)
                    } else {
                        self.subst.apply_dim(&dims_b[i - (max_rank - dims_b.len())])
                    };
                    let merged = match (&da, &db) {
                        (DimExpr::Known(1), other) | (other, DimExpr::Known(1)) => other.clone(),
                        (DimExpr::Known(x), DimExpr::Known(y)) if x == y => DimExpr::Known(*x),
                        (DimExpr::Symbolic(sa), DimExpr::Symbolic(sb)) if sa == sb => {
                            DimExpr::Symbolic(sa.clone())
                        }
                        (DimExpr::Symbolic(s), other) | (other, DimExpr::Symbolic(s))
                            if s == "_" =>
                        {
                            other.clone()
                        }
                        (DimExpr::Var(id1), DimExpr::Var(id2)) if id1 == id2 => DimExpr::Var(*id1),
                        (DimExpr::Var(_), _) | (_, DimExpr::Var(_)) => {
                            let d1_ty = Type::Tensor {
                                dtype: Box::new(Type::Any),
                                shape: ShapeType::Ranked(vec![da.clone()]),
                                device: crate::typeck::DeviceType::Any,
                            };
                            let d2_ty = Type::Tensor {
                                dtype: Box::new(Type::Any),
                                shape: ShapeType::Ranked(vec![db.clone()]),
                                device: crate::typeck::DeviceType::Any,
                            };
                            if let Ok(new_s) = unify::solve_spanned(
                                &SpannedConstraint::new(
                                    Constraint::Eq(Box::new(d1_ty), Box::new(d2_ty)),
                                    span,
                                ),
                                &self.subst,
                            ) {
                                self.subst = new_s;
                            }
                            self.subst.apply_dim(&da)
                        }
                        _ => {
                            return Err(vec![TypeError::UnificationFailure {
                                expected: format!("broadcast-compatible tensor shapes (got {a})"),
                                got: format!("{a} and {b}"),
                                span: Some(span),
                            }]);
                        }
                    };
                    out_dims.push(merged);
                }
                Ok(ShapeType::Ranked(out_dims))
            }
            (ShapeType::Unranked(None), other) | (other, ShapeType::Unranked(None)) => {
                Ok(other.clone())
            }
            (ShapeType::Unranked(Some(v1)), ShapeType::Unranked(Some(v2))) if v1 == v2 => {
                Ok(ShapeType::Unranked(Some(v1.clone())))
            }
            (ShapeType::Unranked(Some(TypeVarId::Meta(_))), _)
            | (_, ShapeType::Unranked(Some(TypeVarId::Meta(_)))) => {
                let d1_ty = Type::Tensor {
                    dtype: Box::new(Type::Any),
                    shape: a.clone(),
                    device: crate::typeck::DeviceType::Any,
                };
                let d2_ty = Type::Tensor {
                    dtype: Box::new(Type::Any),
                    shape: b.clone(),
                    device: crate::typeck::DeviceType::Any,
                };
                match unify::solve_spanned(
                    &SpannedConstraint::new(Constraint::Eq(Box::new(d1_ty), Box::new(d2_ty)), span),
                    &self.subst,
                ) {
                    Ok(new_s) => {
                        self.subst = new_s;
                        Ok(self.subst.apply_shape(&a))
                    }
                    Err(e) => Err(vec![e]),
                }
            }
            _ => Err(vec![TypeError::UnificationFailure {
                expected: format!("compatible tensor shapes (got {a})"),
                got: format!("{a} and {b}"),
                span: Some(span),
            }]),
        }
    }

    fn infer_tensor_binary_op(
        &mut self,
        left_ty: &Type,
        right_ty: &Type,
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        match (left_ty, right_ty) {
            (
                Type::Tensor {
                    dtype: d1,
                    shape: s1,
                    device: dev1,
                },
                Type::Tensor {
                    dtype: d2,
                    shape: s2,
                    device: dev2,
                },
            ) => {
                let out_dtype = self.unify_tensor_dtypes(d1, d2, span)?;
                if out_dtype.as_dtype().is_some_and(|d| d.is_bool()) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: "numeric Tensor DType for arithmetic operator".to_string(),
                        got: format!("Tensor<{}>", out_dtype.name()),
                        span: Some(span),
                    }]);
                }
                let out_device = self.unify_tensor_devices(dev1, dev2, span)?;
                let out_shape = self.broadcast_tensor_shapes(s1, s2, span)?;
                Ok(Type::Tensor {
                    dtype: Box::new(out_dtype),
                    shape: out_shape,
                    device: out_device,
                })
            }
            (
                Type::Tensor {
                    dtype,
                    shape,
                    device,
                },
                scalar_ty,
            )
            | (
                scalar_ty,
                Type::Tensor {
                    dtype,
                    shape,
                    device,
                },
            ) => {
                if !is_numeric(scalar_ty)
                    && !matches!(scalar_ty, Type::Scalar(_) | Type::TypeVar(_) | Type::Any)
                {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!("Tensor or scalar matching DType '{}'", dtype.name()),
                        got: format!("{} and {}", left_ty.name(), right_ty.name()),
                        span: Some(span),
                    }]);
                }
                let out_dtype = self.unify_tensor_dtypes(dtype, scalar_ty, span)?;
                Ok(Type::Tensor {
                    dtype: Box::new(out_dtype),
                    shape: self.subst.apply_shape(shape),
                    device: self.subst.apply_device(device),
                })
            }
            _ => unreachable!(),
        }
    }

    fn unify_contract_dim(
        &mut self,
        k1: &crate::typeck::DimExpr,
        k2: &crate::typeck::DimExpr,
        sa: &crate::typeck::ShapeType,
        sb: &crate::typeck::ShapeType,
        span: Span,
    ) -> Result<(), Vec<TypeError>> {
        use crate::typeck::{DeviceType, ShapeType};
        let d1_ty = Type::Tensor {
            dtype: Box::new(Type::Any),
            shape: ShapeType::Ranked(vec![k1.clone()]),
            device: DeviceType::Any,
        };
        let d2_ty = Type::Tensor {
            dtype: Box::new(Type::Any),
            shape: ShapeType::Ranked(vec![k2.clone()]),
            device: DeviceType::Any,
        };
        match unify::solve_spanned(
            &SpannedConstraint::new(Constraint::Eq(Box::new(d1_ty), Box::new(d2_ty)), span),
            &self.subst,
        ) {
            Ok(new_s) => {
                self.subst = new_s;
                Ok(())
            }
            Err(_) => Err(vec![TypeError::UnificationFailure {
                expected: format!(
                    "matching contraction inner dimension for '@' (left inner dim is {})",
                    self.subst.apply_dim(k1)
                ),
                got: format!(
                    "{sa} @ {sb} ({} != {})",
                    self.subst.apply_dim(k1),
                    self.subst.apply_dim(k2)
                ),
                span: Some(span),
            }]),
        }
    }

    fn infer_tensor_matmul_op(
        &mut self,
        left_ty: &Type,
        right_ty: &Type,
        span: Span,
    ) -> Result<Type, Vec<TypeError>> {
        use crate::typeck::ShapeType;
        let (
            Type::Tensor {
                dtype: d1,
                shape: s1,
                device: dev1,
            },
            Type::Tensor {
                dtype: d2,
                shape: s2,
                device: dev2,
            },
        ) = (left_ty, right_ty)
        else {
            if matches!(left_ty, Type::TypeVar(TypeVarId::Meta(_)) | Type::Any)
                || matches!(right_ty, Type::TypeVar(TypeVarId::Meta(_)) | Type::Any)
            {
                return Ok(self.fresh_type_var());
            }
            return Err(vec![TypeError::UnificationFailure {
                expected: "two Tensor operands for '@' matrix contraction".to_string(),
                got: format!("{} @ {}", left_ty.name(), right_ty.name()),
                span: Some(span),
            }]);
        };

        let out_dtype = self.unify_tensor_dtypes(d1, d2, span)?;
        if out_dtype.as_dtype().is_some_and(|d| d.is_bool()) {
            return Err(vec![TypeError::UnificationFailure {
                expected: "numeric Tensor DType for '@' matrix contraction".to_string(),
                got: format!("Tensor<{}>", out_dtype.name()),
                span: Some(span),
            }]);
        }
        let out_device = self.unify_tensor_devices(dev1, dev2, span)?;
        let sa = self.subst.apply_shape(s1);
        let sb = self.subst.apply_shape(s2);

        match (&sa, &sb) {
            (ShapeType::Ranked(da), ShapeType::Ranked(db)) => {
                if da.is_empty() || db.is_empty() {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: "tensors with rank >= 1 for '@'".to_string(),
                        got: format!("{sa} @ {sb}"),
                        span: Some(span),
                    }]);
                }
                match (da.len(), db.len()) {
                    (1, 1) => {
                        self.unify_contract_dim(&da[0], &db[0], &sa, &sb, span)?;
                        Ok(out_dtype)
                    }
                    (2, 2) => {
                        self.unify_contract_dim(&da[1], &db[0], &sa, &sb, span)?;
                        let m = self.subst.apply_dim(&da[0]);
                        let n = self.subst.apply_dim(&db[1]);
                        Ok(Type::Tensor {
                            dtype: Box::new(out_dtype),
                            shape: ShapeType::Ranked(vec![m, n]),
                            device: out_device,
                        })
                    }
                    (2, 1) => {
                        self.unify_contract_dim(&da[1], &db[0], &sa, &sb, span)?;
                        let m = self.subst.apply_dim(&da[0]);
                        Ok(Type::Tensor {
                            dtype: Box::new(out_dtype),
                            shape: ShapeType::Ranked(vec![m]),
                            device: out_device,
                        })
                    }
                    (1, 2) => {
                        self.unify_contract_dim(&da[0], &db[0], &sa, &sb, span)?;
                        let n = self.subst.apply_dim(&db[1]);
                        Ok(Type::Tensor {
                            dtype: Box::new(out_dtype),
                            shape: ShapeType::Ranked(vec![n]),
                            device: out_device,
                        })
                    }
                    (ra, rb) if ra >= 2 && rb >= 2 => {
                        let m = da[ra - 2].clone();
                        let k1 = da[ra - 1].clone();
                        let k2 = db[rb - 2].clone();
                        let n = db[rb - 1].clone();
                        self.unify_contract_dim(&k1, &k2, &sa, &sb, span)?;
                        let batch_a = ShapeType::Ranked(da[..ra - 2].to_vec());
                        let batch_b = ShapeType::Ranked(db[..rb - 2].to_vec());
                        let ShapeType::Ranked(mut out_dims) =
                            self.broadcast_tensor_shapes(&batch_a, &batch_b, span)?
                        else {
                            unreachable!()
                        };
                        out_dims.push(self.subst.apply_dim(&m));
                        out_dims.push(self.subst.apply_dim(&n));
                        Ok(Type::Tensor {
                            dtype: Box::new(out_dtype),
                            shape: ShapeType::Ranked(out_dims),
                            device: out_device,
                        })
                    }
                    _ => Err(vec![TypeError::UnificationFailure {
                        expected: "compatible tensor ranks for '@' (1D, 2D, or batched >=2D)"
                            .to_string(),
                        got: format!("{sa} @ {sb}"),
                        span: Some(span),
                    }]),
                }
            }
            _ => Ok(Type::Tensor {
                dtype: Box::new(out_dtype),
                shape: ShapeType::Unranked(None),
                device: out_device,
            }),
        }
    }

    pub(super) fn extract_static_int_literal(w: &MirWitness) -> Option<i64> {
        match &w.kind {
            WitnessKind::Literal(crate::common::Literal::Int(n, _)) => Some(*n),
            WitnessKind::Literal(crate::common::Literal::TypedInt(n, _, _)) => Some(*n),
            WitnessKind::Call {
                callee: WitnessCallee::Builtin(crate::mir::witness::BuiltinOp::Neg),
                args,
                ..
            } if args.len() == 1 => Self::extract_static_int_literal(&args[0])?.checked_neg(),
            _ => None,
        }
    }

    fn extract_static_shape_list(w: &MirWitness) -> Option<Vec<crate::typeck::DimExpr>> {
        let WitnessKind::List(items) = &w.kind else {
            return None;
        };
        let mut dims = Vec::with_capacity(items.len());
        for item in items {
            let n = Self::extract_static_int_literal(item)?;
            if n < 0 {
                return None;
            }
            dims.push(crate::typeck::DimExpr::Known(n as usize));
        }
        Some(dims)
    }

    fn extract_nested_list_shape_and_dtype(
        w: &MirWitness,
    ) -> Result<(crate::typeck::ShapeType, Option<Type>), Vec<TypeError>> {
        use crate::typeck::{DimExpr, ShapeType};
        fn walk(
            node: &MirWitness,
            shape: &mut Vec<usize>,
            depth: usize,
            leaf_dt: &mut Option<Type>,
        ) -> Result<(), Vec<TypeError>> {
            match &node.kind {
                WitnessKind::List(items) => {
                    if shape.len() == depth {
                        shape.push(items.len());
                    } else if shape[depth] != items.len() {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: format!(
                                "rectangular tensor nested list (dimension {} has length {})",
                                depth, shape[depth]
                            ),
                            got: format!("sub-list of length {}", items.len()),
                            span: Some(node.span),
                        }]);
                    }
                    for item in items {
                        walk(item, shape, depth + 1, leaf_dt)?;
                    }
                    Ok(())
                }
                WitnessKind::Call {
                    callee: WitnessCallee::Builtin(crate::mir::witness::BuiltinOp::Neg),
                    args,
                    ..
                } if args.len() == 1 => walk(&args[0], shape, depth, leaf_dt),
                WitnessKind::Literal(lit) => {
                    let ty = super::infer_lit(lit);
                    match leaf_dt {
                        None => *leaf_dt = Some(ty),
                        Some(prev) => {
                            if prev != &ty {
                                if matches!(
                                    (&prev, &ty),
                                    (Type::Int, Type::Float) | (Type::Float, Type::Int)
                                ) {
                                    *leaf_dt = Some(Type::Float);
                                } else {
                                    return Err(vec![TypeError::UnificationFailure {
                                        expected: format!(
                                            "uniform scalar DType '{}' in tensor literal",
                                            prev.name()
                                        ),
                                        got: ty.name(),
                                        span: Some(node.span),
                                    }]);
                                }
                            }
                        }
                    }
                    Ok(())
                }
                _ => Err(vec![]),
            }
        }
        let mut shape = Vec::new();
        let mut leaf_dt = None;
        match walk(w, &mut shape, 0, &mut leaf_dt) {
            Ok(()) => {
                let dims = shape.into_iter().map(DimExpr::Known).collect();
                Ok((ShapeType::Ranked(dims), leaf_dt))
            }
            Err(errs) if errs.is_empty() => Ok((ShapeType::Unranked(None), None)),
            Err(errs) => Err(errs),
        }
    }

    fn parse_static_dtype_witness(
        arg: Option<&MirWitness>,
    ) -> Result<Option<Type>, Vec<TypeError>> {
        let Some(w) = arg else {
            return Ok(None);
        };
        match &w.kind {
            WitnessKind::Literal(crate::common::Literal::Nil(_)) => Ok(None),
            WitnessKind::Literal(crate::common::Literal::String(s, _)) => {
                match crate::common::DType::from_name(s) {
                    Some(dt) => Ok(Some(Type::from_dtype(dt))),
                    None => Err(vec![TypeError::InvalidLiteral {
                        what: "tensor dtype".to_string(),
                        value: s.clone(),
                        span: Some(w.span),
                    }]),
                }
            }
            _ => Ok(None),
        }
    }

    fn parse_static_device_witness(
        arg: Option<&MirWitness>,
    ) -> Result<crate::typeck::DeviceType, Vec<TypeError>> {
        use crate::typeck::DeviceType;
        let Some(w) = arg else {
            return Ok(DeviceType::Concrete(crate::common::Device::Cpu));
        };
        match &w.kind {
            WitnessKind::Literal(crate::common::Literal::Nil(_)) => {
                Ok(DeviceType::Concrete(crate::common::Device::Cpu))
            }
            WitnessKind::Literal(crate::common::Literal::String(s, _)) => {
                match crate::common::Device::parse(s) {
                    Ok(dev) => Ok(DeviceType::Concrete(dev)),
                    Err(_) => Err(vec![TypeError::InvalidLiteral {
                        what: "tensor device".to_string(),
                        value: s.clone(),
                        span: Some(w.span),
                    }]),
                }
            }
            _ => Ok(DeviceType::Any),
        }
    }

    fn infer_tensor_constructor_call(
        &mut self,
        ctor: &str,
        args: &[MirWitness],
        named_args: &[(String, MirWitness)],
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        use crate::typeck::{DimExpr, ShapeType};
        if !matches!(
            ctor,
            "tensor"
                | "from_list"
                | "from_bytes"
                | "from_utf8"
                | "cat"
                | "zeros"
                | "ones"
                | "randn"
                | "rand"
                | "full"
                | "eye"
                | "arange"
        ) {
            return Err(vec![TypeError::UnboundVariable {
                name: format!("Tensor::{ctor}"),
                span,
            }]);
        }

        let mut acc_row = crate::mir::effect::EffectRow::Empty;
        if named_args.is_empty() {
            let mut arg_types = Vec::with_capacity(args.len());
            for a in args {
                let (t, r) = self.infer_recover(a);
                arg_types.push(t);
                acc_row = self.merge_rows(acc_row, r, span);
            }
            return self.infer_tensor_constructor_positional(ctor, args, &arg_types, acc_row, span);
        }

        let slots = Self::normalize_tensor_ctor_args(ctor, args, named_args, span)?;
        let mut slot_types: Vec<Option<Type>> = Vec::with_capacity(slots.len());
        for s in &slots {
            match s {
                Some(w) => {
                    let (t, r) = self.infer_recover(w);
                    acc_row = self.merge_rows(acc_row, r, span);
                    slot_types.push(Some(t));
                }
                None => slot_types.push(None),
            }
        }

        match ctor {
            "tensor" | "from_list" => {
                let Some(data_w) = slots.first().copied().flatten() else {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 1,
                        actual: 0,
                        span,
                    }]);
                };
                let (inferred_shape, leaf_dt) = Self::extract_nested_list_shape_and_dtype(data_w)?;
                let explicit_dt =
                    Self::parse_static_dtype_witness(slots.get(1).copied().flatten())?;
                let device = Self::parse_static_device_witness(slots.get(2).copied().flatten())?;
                let dtype = explicit_dt.or(leaf_dt).unwrap_or_else(|| {
                    match slot_types
                        .first()
                        .and_then(|t| t.as_ref())
                        .map(|t| self.subst.apply(t))
                    {
                        Some(Type::Int) => Type::Int,
                        Some(Type::Float) => Type::Float,
                        Some(Type::Scalar(dt)) => Type::Scalar(dt),
                        _ => Type::Float,
                    }
                });
                Ok((
                    Type::Tensor {
                        dtype: Box::new(dtype),
                        shape: inferred_shape,
                        device,
                    },
                    acc_row,
                ))
            }
            "zeros" | "ones" | "randn" | "rand" => {
                let Some(shape_w) = slots.first().copied().flatten() else {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 1,
                        actual: 0,
                        span,
                    }]);
                };
                let shape = match Self::extract_static_shape_list(shape_w) {
                    Some(dims) => ShapeType::Ranked(dims),
                    None => ShapeType::Unranked(None),
                };
                let dtype = Self::parse_static_dtype_witness(slots.get(1).copied().flatten())?
                    .unwrap_or(Type::Float);
                if matches!(ctor, "randn" | "rand")
                    && let Some(dt) = dtype.as_dtype()
                    && !dt.is_float()
                {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!("floating-point DType for Tensor::{ctor}"),
                        got: dtype.name(),
                        span: Some(span),
                    }]);
                }
                let device = Self::parse_static_device_witness(slots.get(2).copied().flatten())?;
                if matches!(ctor, "randn" | "rand") {
                    acc_row = self.merge_rows(
                        acc_row,
                        crate::mir::effect::EffectRow::Cons(
                            "random_random".to_string(),
                            Box::new(crate::mir::effect::EffectRow::Empty),
                        ),
                        span,
                    );
                }
                Ok((
                    Type::Tensor {
                        dtype: Box::new(dtype),
                        shape,
                        device,
                    },
                    acc_row,
                ))
            }
            "full" => {
                let Some(shape_w) = slots.first().copied().flatten() else {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 2,
                        actual: 0,
                        span,
                    }]);
                };
                if slots.get(1).copied().flatten().is_none() {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 2,
                        actual: 1,
                        span,
                    }]);
                }
                let shape = match Self::extract_static_shape_list(shape_w) {
                    Some(dims) => ShapeType::Ranked(dims),
                    None => ShapeType::Unranked(None),
                };
                let fill_ty = slot_types[1]
                    .as_ref()
                    .map(|t| self.subst.apply(t))
                    .unwrap_or(Type::Float);
                let default_dt = match fill_ty {
                    Type::Int => Type::Int,
                    Type::Scalar(dt) => Type::Scalar(dt),
                    _ => Type::Float,
                };
                let dtype = Self::parse_static_dtype_witness(slots.get(2).copied().flatten())?
                    .unwrap_or(default_dt);
                let device = Self::parse_static_device_witness(slots.get(3).copied().flatten())?;
                Ok((
                    Type::Tensor {
                        dtype: Box::new(dtype),
                        shape,
                        device,
                    },
                    acc_row,
                ))
            }
            "eye" => {
                let Some(n_w) = slots.first().copied().flatten() else {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 1,
                        actual: 0,
                        span,
                    }]);
                };
                let n_opt = Self::extract_static_int_literal(n_w);
                let m_opt = match slots.get(1).copied().flatten() {
                    Some(mw) => Self::extract_static_int_literal(mw),
                    None => n_opt,
                };
                let shape = match (n_opt, m_opt) {
                    (Some(n), Some(m)) if n >= 0 && m >= 0 => ShapeType::Ranked(vec![
                        DimExpr::Known(n as usize),
                        DimExpr::Known(m as usize),
                    ]),
                    _ => ShapeType::Ranked(vec![
                        DimExpr::Var(self.fresh_counter),
                        DimExpr::Var(self.fresh_counter + 1),
                    ]),
                };
                self.fresh_counter += 2;
                let dtype = Self::parse_static_dtype_witness(slots.get(2).copied().flatten())?
                    .unwrap_or(Type::Float);
                let device = Self::parse_static_device_witness(slots.get(3).copied().flatten())?;
                Ok((
                    Type::Tensor {
                        dtype: Box::new(dtype),
                        shape,
                        device,
                    },
                    acc_row,
                ))
            }
            "arange" => {
                let Some(s_w) = slots.first().copied().flatten() else {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 1,
                        actual: 0,
                        span,
                    }]);
                };
                let len_opt = match (
                    Self::extract_static_int_literal(s_w),
                    slots
                        .get(1)
                        .copied()
                        .flatten()
                        .and_then(Self::extract_static_int_literal),
                    slots
                        .get(2)
                        .copied()
                        .flatten()
                        .and_then(Self::extract_static_int_literal),
                ) {
                    (Some(e), None, None) if slots.get(1).copied().flatten().is_none() => {
                        Some(e.max(0) as usize)
                    }
                    (Some(s), Some(e), None) if slots.get(2).copied().flatten().is_none() => {
                        Some((e - s).max(0) as usize)
                    }
                    (Some(s), Some(e), Some(step)) if step > 0 => {
                        Some(((e - s + step - 1).max(0) / step) as usize)
                    }
                    _ => None,
                };
                let shape = match len_opt {
                    Some(n) => ShapeType::Ranked(vec![DimExpr::Known(n)]),
                    None => {
                        let id = self.fresh_counter;
                        self.fresh_counter += 1;
                        ShapeType::Ranked(vec![DimExpr::Var(id)])
                    }
                };
                let all_ints = slot_types[..3]
                    .iter()
                    .flatten()
                    .all(|t| matches!(t, Type::Int));
                let default_dt = if all_ints { Type::Int } else { Type::Float };
                let dtype = Self::parse_static_dtype_witness(slots.get(3).copied().flatten())?
                    .unwrap_or(default_dt);
                let device = Self::parse_static_device_witness(slots.get(4).copied().flatten())?;
                Ok((
                    Type::Tensor {
                        dtype: Box::new(dtype),
                        shape,
                        device,
                    },
                    acc_row,
                ))
            }
            "from_bytes" => {
                let Some(buf_w) = slots.first().copied().flatten() else {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 3,
                        actual: 0,
                        span,
                    }]);
                };
                let Some(shape_w) = slots.get(1).copied().flatten() else {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 3,
                        actual: 1,
                        span,
                    }]);
                };
                let Some(dt_w) = slots.get(2).copied().flatten() else {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 3,
                        actual: 2,
                        span,
                    }]);
                };
                let buf_ty = slot_types[0]
                    .as_ref()
                    .map(|t| self.subst.apply(t))
                    .unwrap_or(Type::Any);
                self.constraints.push(SpannedConstraint::new(
                    Constraint::Subtype {
                        sub: Box::new(buf_ty),
                        sup: Box::new(Type::Tensor {
                            dtype: Box::new(Type::Scalar(crate::common::DType::U8)),
                            shape: ShapeType::Unranked(None),
                            device: crate::typeck::DeviceType::Any,
                        }),
                    },
                    buf_w.span,
                ));
                let shape = match Self::extract_static_shape_list(shape_w) {
                    Some(dims) => ShapeType::Ranked(dims),
                    None => ShapeType::Unranked(None),
                };
                let dtype = Self::parse_static_dtype_witness(Some(dt_w))?.unwrap_or(Type::Any);
                let device = Self::parse_static_device_witness(slots.get(3).copied().flatten())?;
                Ok((
                    Type::Tensor {
                        dtype: Box::new(dtype),
                        shape,
                        device,
                    },
                    acc_row,
                ))
            }
            "from_utf8" => {
                let Some(text_w) = slots.first().copied().flatten() else {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 1,
                        actual: 0,
                        span,
                    }]);
                };
                let text_ty = slot_types[0]
                    .as_ref()
                    .map(|t| self.subst.apply(t))
                    .unwrap_or(Type::Any);
                self.constraints.push(SpannedConstraint::new(
                    Constraint::Subtype {
                        sub: Box::new(text_ty),
                        sup: Box::new(Type::String),
                    },
                    text_w.span,
                ));
                let id = self.fresh_counter;
                self.fresh_counter += 1;
                Ok((
                    Type::Tensor {
                        dtype: Box::new(Type::Scalar(crate::common::DType::U8)),
                        shape: ShapeType::Ranked(vec![DimExpr::Var(id)]),
                        device: crate::typeck::DeviceType::Concrete(crate::common::Device::Cpu),
                    },
                    acc_row,
                ))
            }
            "cat" => {
                let Some(list_w) = slots.first().copied().flatten() else {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 1,
                        actual: 0,
                        span,
                    }]);
                };
                let list_ty = slot_types[0]
                    .as_ref()
                    .map(|t| self.subst.apply(t))
                    .unwrap_or(Type::Any);
                let elem_ty = match list_ty {
                    Type::List(inner) => self.subst.apply(&inner),
                    Type::Any | Type::Error | Type::TypeVar(_) => Type::any_tensor(),
                    other => {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: "list<Tensor> for Tensor::cat".to_string(),
                            got: other.name(),
                            span: Some(list_w.span),
                        }]);
                    }
                };
                if let Some(ax_w) = slots.get(1).copied().flatten() {
                    let ax_ty = slot_types[1]
                        .as_ref()
                        .map(|t| self.subst.apply(t))
                        .unwrap_or(Type::Any);
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Subtype {
                            sub: Box::new(ax_ty),
                            sup: Box::new(Type::Union(vec![Type::Int, Type::Nil])),
                        },
                        ax_w.span,
                    ));
                }
                let ret = match elem_ty {
                    Type::Tensor { dtype, device, .. } => Type::Tensor {
                        dtype,
                        shape: ShapeType::Unranked(None),
                        device,
                    },
                    _ => Type::any_tensor(),
                };
                Ok((ret, acc_row))
            }
            _ => unreachable!(),
        }
    }

    fn infer_tensor_constructor_positional(
        &mut self,
        ctor: &str,
        args: &[MirWitness],
        arg_types: &[Type],
        mut acc_row: crate::mir::effect::EffectRow,
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        use crate::typeck::{DimExpr, ShapeType};

        match ctor {
            "tensor" | "from_list" => {
                if args.is_empty() || args.len() > 4 {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 1,
                        actual: args.len(),
                        span,
                    }]);
                }
                let (inferred_shape, leaf_dt) =
                    Self::extract_nested_list_shape_and_dtype(&args[0])?;
                let explicit_dt = Self::parse_static_dtype_witness(args.get(1))?;
                let device = Self::parse_static_device_witness(args.get(2))?;
                let dtype = explicit_dt.or(leaf_dt).unwrap_or_else(|| {
                    match self.subst.apply(&arg_types[0]) {
                        Type::Int => Type::Int,
                        Type::Float => Type::Float,
                        Type::Scalar(dt) => Type::Scalar(dt),
                        _ => Type::Float,
                    }
                });
                Ok((
                    Type::Tensor {
                        dtype: Box::new(dtype),
                        shape: inferred_shape,
                        device,
                    },
                    acc_row,
                ))
            }
            "zeros" | "ones" => {
                if args.is_empty() || args.len() > 4 {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 1,
                        actual: args.len(),
                        span,
                    }]);
                }
                let shape = match Self::extract_static_shape_list(&args[0]) {
                    Some(dims) => ShapeType::Ranked(dims),
                    None => ShapeType::Unranked(None),
                };
                let dtype = Self::parse_static_dtype_witness(args.get(1))?.unwrap_or(Type::Float);
                let device = Self::parse_static_device_witness(args.get(2))?;
                Ok((
                    Type::Tensor {
                        dtype: Box::new(dtype),
                        shape,
                        device,
                    },
                    acc_row,
                ))
            }
            "randn" | "rand" => {
                if args.is_empty() || args.len() > 4 {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 1,
                        actual: args.len(),
                        span,
                    }]);
                }
                let shape = match Self::extract_static_shape_list(&args[0]) {
                    Some(dims) => ShapeType::Ranked(dims),
                    None => ShapeType::Unranked(None),
                };
                let dtype = Self::parse_static_dtype_witness(args.get(1))?.unwrap_or(Type::Float);
                if let Some(dt) = dtype.as_dtype()
                    && !dt.is_float()
                {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!("floating-point DType for Tensor::{ctor}"),
                        got: dtype.name(),
                        span: Some(span),
                    }]);
                }
                let device = Self::parse_static_device_witness(args.get(2))?;
                let eff_label = "random_random";
                acc_row = self.merge_rows(
                    acc_row,
                    crate::mir::effect::EffectRow::Cons(
                        eff_label.to_string(),
                        Box::new(crate::mir::effect::EffectRow::Empty),
                    ),
                    span,
                );
                Ok((
                    Type::Tensor {
                        dtype: Box::new(dtype),
                        shape,
                        device,
                    },
                    acc_row,
                ))
            }
            "full" => {
                if args.len() < 2 || args.len() > 5 {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 2,
                        actual: args.len(),
                        span,
                    }]);
                }
                let shape = match Self::extract_static_shape_list(&args[0]) {
                    Some(dims) => ShapeType::Ranked(dims),
                    None => ShapeType::Unranked(None),
                };
                let fill_ty = self.subst.apply(&arg_types[1]);
                let default_dt = match fill_ty {
                    Type::Int => Type::Int,
                    Type::Scalar(dt) => Type::Scalar(dt),
                    _ => Type::Float,
                };
                let dtype = Self::parse_static_dtype_witness(args.get(2))?.unwrap_or(default_dt);
                let device = Self::parse_static_device_witness(args.get(3))?;
                Ok((
                    Type::Tensor {
                        dtype: Box::new(dtype),
                        shape,
                        device,
                    },
                    acc_row,
                ))
            }
            "eye" => {
                if args.is_empty() || args.len() > 5 {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 1,
                        actual: args.len(),
                        span,
                    }]);
                }
                let n_opt = Self::extract_static_int_literal(&args[0]);
                let (m_opt, opt_start) =
                    match args.get(1).and_then(Self::extract_static_int_literal) {
                        Some(m) => (Some(m), 2),
                        None => {
                            if matches!(
                                arg_types.get(1),
                                Some(Type::Int | Type::Float | Type::Scalar(_))
                            ) {
                                (None, 2)
                            } else {
                                (n_opt, 1)
                            }
                        }
                    };
                let shape = match (n_opt, m_opt) {
                    (Some(n), Some(m)) if n >= 0 && m >= 0 => ShapeType::Ranked(vec![
                        DimExpr::Known(n as usize),
                        DimExpr::Known(m as usize),
                    ]),
                    _ => ShapeType::Ranked(vec![
                        DimExpr::Var(self.fresh_counter),
                        DimExpr::Var(self.fresh_counter + 1),
                    ]),
                };
                self.fresh_counter += 2;
                let dtype =
                    Self::parse_static_dtype_witness(args.get(opt_start))?.unwrap_or(Type::Float);
                let device = Self::parse_static_device_witness(args.get(opt_start + 1))?;
                Ok((
                    Type::Tensor {
                        dtype: Box::new(dtype),
                        shape,
                        device,
                    },
                    acc_row,
                ))
            }
            "arange" => {
                if args.is_empty() || args.len() > 6 {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 1,
                        actual: args.len(),
                        span,
                    }]);
                }
                let mut num_count = 0usize;
                while num_count < args.len()
                    && num_count < 3
                    && matches!(
                        arg_types[num_count],
                        Type::Int | Type::Float | Type::Scalar(_)
                    )
                {
                    num_count += 1;
                }
                let len_opt = match num_count {
                    1 => Self::extract_static_int_literal(&args[0]).map(|e| e.max(0) as usize),
                    2 => match (
                        Self::extract_static_int_literal(&args[0]),
                        Self::extract_static_int_literal(&args[1]),
                    ) {
                        (Some(s), Some(e)) => Some((e - s).max(0) as usize),
                        _ => None,
                    },
                    3 => match (
                        Self::extract_static_int_literal(&args[0]),
                        Self::extract_static_int_literal(&args[1]),
                        Self::extract_static_int_literal(&args[2]),
                    ) {
                        (Some(s), Some(e), Some(step)) if step > 0 => {
                            Some(((e - s + step - 1).max(0) / step) as usize)
                        }
                        _ => None,
                    },
                    _ => None,
                };
                let shape = match len_opt {
                    Some(n) => ShapeType::Ranked(vec![DimExpr::Known(n)]),
                    None => {
                        let id = self.fresh_counter;
                        self.fresh_counter += 1;
                        ShapeType::Ranked(vec![DimExpr::Var(id)])
                    }
                };
                let all_ints = arg_types[..num_count]
                    .iter()
                    .all(|t| matches!(t, Type::Int));
                let default_dt = if all_ints { Type::Int } else { Type::Float };
                let dtype =
                    Self::parse_static_dtype_witness(args.get(num_count))?.unwrap_or(default_dt);
                let device = Self::parse_static_device_witness(args.get(num_count + 1))?;
                Ok((
                    Type::Tensor {
                        dtype: Box::new(dtype),
                        shape,
                        device,
                    },
                    acc_row,
                ))
            }
            "from_bytes" => {
                if args.len() < 3 || args.len() > 5 {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 3,
                        actual: args.len(),
                        span,
                    }]);
                }
                let buf_ty = self.subst.apply(&arg_types[0]);
                self.constraints.push(SpannedConstraint::new(
                    Constraint::Subtype {
                        sub: Box::new(buf_ty),
                        sup: Box::new(Type::Tensor {
                            dtype: Box::new(Type::Scalar(crate::common::DType::U8)),
                            shape: ShapeType::Unranked(None),
                            device: crate::typeck::DeviceType::Any,
                        }),
                    },
                    args[0].span,
                ));
                let shape = match Self::extract_static_shape_list(&args[1]) {
                    Some(dims) => ShapeType::Ranked(dims),
                    None => ShapeType::Unranked(None),
                };
                let dtype = Self::parse_static_dtype_witness(args.get(2))?.unwrap_or(Type::Any);
                let device = Self::parse_static_device_witness(args.get(3))?;
                Ok((
                    Type::Tensor {
                        dtype: Box::new(dtype),
                        shape,
                        device,
                    },
                    acc_row,
                ))
            }
            "from_utf8" => {
                if args.len() != 1 {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 1,
                        actual: args.len(),
                        span,
                    }]);
                }
                let text_ty = self.subst.apply(&arg_types[0]);
                self.constraints.push(SpannedConstraint::new(
                    Constraint::Subtype {
                        sub: Box::new(text_ty),
                        sup: Box::new(Type::String),
                    },
                    args[0].span,
                ));
                let id = self.fresh_counter;
                self.fresh_counter += 1;
                Ok((
                    Type::Tensor {
                        dtype: Box::new(Type::Scalar(crate::common::DType::U8)),
                        shape: ShapeType::Ranked(vec![DimExpr::Var(id)]),
                        device: crate::typeck::DeviceType::Concrete(crate::common::Device::Cpu),
                    },
                    acc_row,
                ))
            }
            "cat" => {
                if args.is_empty() || args.len() > 2 {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 1,
                        actual: args.len(),
                        span,
                    }]);
                }
                let list_ty = self.subst.apply(&arg_types[0]);
                let elem_ty = match list_ty {
                    Type::List(inner) => self.subst.apply(&inner),
                    Type::Any | Type::Error | Type::TypeVar(_) => Type::any_tensor(),
                    other => {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: "list<Tensor> for Tensor::cat".to_string(),
                            got: other.name(),
                            span: Some(args[0].span),
                        }]);
                    }
                };
                if let Some(ax_ty) = arg_types.get(1) {
                    let ax_resolved = self.subst.apply(ax_ty);
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Subtype {
                            sub: Box::new(ax_resolved),
                            sup: Box::new(Type::Union(vec![Type::Int, Type::Nil])),
                        },
                        args[1].span,
                    ));
                }
                let ret = match elem_ty {
                    Type::Tensor { dtype, device, .. } => Type::Tensor {
                        dtype,
                        shape: ShapeType::Unranked(None),
                        device,
                    },
                    _ => Type::any_tensor(),
                };
                Ok((ret, acc_row))
            }
            _ => Err(vec![TypeError::UnboundVariable {
                name: format!("Tensor::{ctor}"),
                span,
            }]),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn infer_tensor_method_call(
        &mut self,
        receiver_witness: Option<&MirWitness>,
        recv_ty: &Type,
        method: &str,
        args: &[MirWitness],
        arg_types: &[Type],
        mut acc_row: crate::mir::effect::EffectRow,
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        use crate::typeck::{DeviceType, DimExpr, ShapeType};
        let Type::Tensor {
            dtype,
            shape,
            device,
        } = recv_ty
        else {
            unreachable!()
        };
        let cur_dtype = self.subst.apply(dtype);
        let cur_shape = self.subst.apply_shape(shape);
        let cur_device = self.subst.apply_device(device);

        match method {
            "shape" => {
                if !args.is_empty() {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 0,
                        actual: args.len(),
                        span,
                    }]);
                }
                Ok((Type::List(Box::new(Type::Int)), acc_row))
            }
            "ndim" | "rank" | "numel" | "nbytes" | "byte_len" | "len" => {
                if !args.is_empty() {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 0,
                        actual: args.len(),
                        span,
                    }]);
                }
                Ok((Type::Int, acc_row))
            }
            "dtype" | "device" => {
                if !args.is_empty() {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 0,
                        actual: args.len(),
                        span,
                    }]);
                }
                Ok((Type::String, acc_row))
            }
            "is_contiguous" | "requires_grad" => {
                if !args.is_empty() {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 0,
                        actual: args.len(),
                        span,
                    }]);
                }
                Ok((Type::Bool, acc_row))
            }
            "contiguous" | "abs" | "sqrt" | "exp" | "ln" | "relu" | "gelu" | "silu" | "tanh"
            | "sigmoid" | "softmax" => {
                if !args.is_empty() {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 0,
                        actual: args.len(),
                        span,
                    }]);
                }
                if method == "softmax"
                    && let Some(dt) = cur_dtype.as_dtype()
                    && !dt.is_float()
                {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: "floating-point tensor DType for softmax()".to_string(),
                        got: cur_dtype.name(),
                        span: Some(span),
                    }]);
                }
                Ok((
                    Type::Tensor {
                        dtype: Box::new(cur_dtype),
                        shape: cur_shape,
                        device: cur_device,
                    },
                    acc_row,
                ))
            }
            "rms_norm" | "layer_norm" => {
                if args.len() > 1 {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 1,
                        actual: args.len(),
                        span,
                    }]);
                }
                if let Some(dt) = cur_dtype.as_dtype()
                    && !dt.is_float()
                {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: format!("floating-point tensor DType for {method}()"),
                        got: cur_dtype.name(),
                        span: Some(span),
                    }]);
                }
                Ok((
                    Type::Tensor {
                        dtype: Box::new(cur_dtype),
                        shape: cur_shape,
                        device: cur_device,
                    },
                    acc_row,
                ))
            }
            "with_grad" => {
                if args.len() > 1 {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 1,
                        actual: args.len(),
                        span,
                    }]);
                }
                if let Some(arg0) = arg_types.first()
                    && !matches!(arg0, Type::Error)
                {
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Subtype {
                            sub: Box::new(arg0.clone()),
                            sup: Box::new(Type::Bool),
                        },
                        args[0].span,
                    ));
                }
                Ok((
                    Type::Tensor {
                        dtype: Box::new(cur_dtype),
                        shape: cur_shape,
                        device: cur_device,
                    },
                    acc_row,
                ))
            }
            "sum" | "max" | "min" | "item" => {
                if !args.is_empty() {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 0,
                        actual: args.len(),
                        span,
                    }]);
                }
                if method == "item"
                    && let ShapeType::Ranked(dims) = &cur_shape
                    && dims.iter().all(|d| matches!(d, DimExpr::Known(_)))
                {
                    let numel: usize = dims
                        .iter()
                        .map(|d| match d {
                            DimExpr::Known(n) => *n,
                            _ => 1,
                        })
                        .product();
                    if numel != 1 {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: "1-element tensor for .item()".to_string(),
                            got: format!("shape {cur_shape} ({numel} elements)"),
                            span: Some(span),
                        }]);
                    }
                }
                Ok((cur_dtype, acc_row))
            }
            "mean" => {
                if !args.is_empty() {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 0,
                        actual: args.len(),
                        span,
                    }]);
                }
                let ret = match cur_dtype.as_dtype() {
                    Some(dt) if dt.is_float() => Type::from_dtype(dt),
                    _ => Type::Float,
                };
                Ok((ret, acc_row))
            }
            "to_list" => {
                if !args.is_empty() {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 0,
                        actual: args.len(),
                        span,
                    }]);
                }
                Ok((Type::List(Box::new(Type::Any)), acc_row))
            }
            "to_bytes" => {
                if !args.is_empty() {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 0,
                        actual: args.len(),
                        span,
                    }]);
                }
                let id = self.fresh_counter;
                self.fresh_counter += 1;
                Ok((
                    Type::Tensor {
                        dtype: Box::new(Type::Scalar(crate::common::DType::U8)),
                        shape: ShapeType::Ranked(vec![DimExpr::Var(id)]),
                        device: DeviceType::Concrete(crate::common::Device::Cpu),
                    },
                    acc_row,
                ))
            }
            "to_utf8" => {
                if !args.is_empty() {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 0,
                        actual: args.len(),
                        span,
                    }]);
                }
                if let Some(dt) = cur_dtype.as_dtype()
                    && dt != crate::common::DType::U8
                {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: "Tensor<u8> for .to_utf8()".to_string(),
                        got: format!("Tensor<{}>", dt.short_name()),
                        span: Some(span),
                    }]);
                }
                Ok((Type::String, acc_row))
            }
            "reshape" => {
                if args.is_empty() {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 1,
                        actual: 0,
                        span,
                    }]);
                }
                let target_dims_opt = if args.len() == 1 {
                    Self::extract_static_shape_list(&args[0]).or_else(|| {
                        Self::extract_static_int_literal(&args[0])
                            .filter(|&n| n >= 0)
                            .map(|n| vec![DimExpr::Known(n as usize)])
                    })
                } else {
                    let mut dims = Vec::with_capacity(args.len());
                    let mut ok = true;
                    for a in args {
                        if let Some(n) = Self::extract_static_int_literal(a)
                            && n >= 0
                        {
                            dims.push(DimExpr::Known(n as usize));
                        } else {
                            ok = false;
                            break;
                        }
                    }
                    if ok { Some(dims) } else { None }
                };

                let new_shape = match target_dims_opt {
                    Some(target_dims) => {
                        if let ShapeType::Ranked(cur_dims) = &cur_shape
                            && cur_dims.iter().all(|d| matches!(d, DimExpr::Known(_)))
                        {
                            let cur_numel: usize = cur_dims
                                .iter()
                                .map(|d| match d {
                                    DimExpr::Known(n) => *n,
                                    _ => 1,
                                })
                                .product();
                            let target_numel: usize = target_dims
                                .iter()
                                .map(|d| match d {
                                    DimExpr::Known(n) => *n,
                                    _ => 1,
                                })
                                .product();
                            if cur_numel != target_numel {
                                return Err(vec![TypeError::UnificationFailure {
                                    expected: format!(
                                        "reshape target with {cur_numel} elements (matching source shape {cur_shape})"
                                    ),
                                    got: format!(
                                        "shape {} with {target_numel} elements",
                                        ShapeType::Ranked(target_dims)
                                    ),
                                    span: Some(span),
                                }]);
                            }
                        }
                        ShapeType::Ranked(target_dims)
                    }
                    None => ShapeType::Unranked(None),
                };
                Ok((
                    Type::Tensor {
                        dtype: Box::new(cur_dtype),
                        shape: new_shape,
                        device: cur_device,
                    },
                    acc_row,
                ))
            }
            "transpose" => {
                if args.len() > 2 {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 2,
                        actual: args.len(),
                        span,
                    }]);
                }
                let new_shape = match &cur_shape {
                    ShapeType::Ranked(cur_dims) => {
                        if cur_dims.len() < 2 {
                            return Err(vec![TypeError::UnificationFailure {
                                expected: "tensor with rank >= 2 for transpose()".to_string(),
                                got: format!("rank {} ({cur_shape})", cur_dims.len()),
                                span: Some(span),
                            }]);
                        }
                        let rank_i = cur_dims.len() as i64;
                        let resolve_axis =
                            |opt: Option<&MirWitness>,
                             default_ax: usize|
                             -> Result<Option<usize>, Vec<TypeError>> {
                                match opt {
                                    None => Ok(Some(default_ax)),
                                    Some(w) => {
                                        let Some(raw) = Self::extract_static_int_literal(w) else {
                                            return Ok(None);
                                        };
                                        let adj = if raw < 0 { raw + rank_i } else { raw };
                                        if adj < 0 || adj >= rank_i {
                                            return Err(vec![TypeError::UnificationFailure {
                                                expected: format!(
                                                    "transpose axis within [-{rank_i}, {})",
                                                    rank_i - 1
                                                ),
                                                got: format!("axis {raw}"),
                                                span: Some(w.span),
                                            }]);
                                        }
                                        Ok(Some(adj as usize))
                                    }
                                }
                            };
                        let ax0 = resolve_axis(args.first(), cur_dims.len() - 2)?;
                        let ax1 = resolve_axis(args.get(1), cur_dims.len() - 1)?;
                        if let (Some(a0), Some(a1)) = (ax0, ax1) {
                            let mut swapped = cur_dims.clone();
                            swapped.swap(a0, a1);
                            ShapeType::Ranked(swapped)
                        } else {
                            ShapeType::Unranked(None)
                        }
                    }
                    other => other.clone(),
                };
                Ok((
                    Type::Tensor {
                        dtype: Box::new(cur_dtype),
                        shape: new_shape,
                        device: cur_device,
                    },
                    acc_row,
                ))
            }
            "to_dtype" => {
                if args.len() != 1 {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 1,
                        actual: args.len(),
                        span,
                    }]);
                }
                let new_dt = Self::parse_static_dtype_witness(args.first())?.unwrap_or(Type::Any);
                Ok((
                    Type::Tensor {
                        dtype: Box::new(new_dt),
                        shape: cur_shape,
                        device: cur_device,
                    },
                    acc_row,
                ))
            }
            "to_device" => {
                if args.len() != 1 {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 1,
                        actual: args.len(),
                        span,
                    }]);
                }
                let new_dev = Self::parse_static_device_witness(args.first())?;
                Ok((
                    Type::Tensor {
                        dtype: Box::new(cur_dtype),
                        shape: cur_shape,
                        device: new_dev,
                    },
                    acc_row,
                ))
            }
            "to" => {
                if args.is_empty() || args.len() > 2 {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 1,
                        actual: args.len(),
                        span,
                    }]);
                }
                let mut next_dt = cur_dtype;
                let mut next_dev = cur_device;
                for arg in args {
                    if let WitnessKind::Literal(crate::common::Literal::String(s, _)) = &arg.kind {
                        if let Some(dt) = crate::common::DType::from_name(s) {
                            next_dt = Type::from_dtype(dt);
                        } else if let Ok(dev) = crate::common::Device::parse(s) {
                            next_dev = DeviceType::Concrete(dev);
                        } else {
                            return Err(vec![TypeError::InvalidLiteral {
                                what: "tensor dtype or device".to_string(),
                                value: s.clone(),
                                span: Some(arg.span),
                            }]);
                        }
                    } else {
                        // Non-literal target: allocate a fresh device meta-variable instead of
                        // silently widening to `DeviceType::Any`, preserving cross-device safety.
                        let id = self.fresh_counter;
                        self.fresh_counter += 1;
                        next_dev = DeviceType::Var(TypeVarId::Meta(id));
                    }
                }
                Ok((
                    Type::Tensor {
                        dtype: Box::new(next_dt),
                        shape: cur_shape,
                        device: next_dev,
                    },
                    acc_row,
                ))
            }
            "fill_" | "add_" | "mul_" => {
                if args.len() != 1 {
                    return Err(vec![TypeError::ArityMismatch {
                        expected: 1,
                        actual: args.len(),
                        span,
                    }]);
                }
                // Check 4 (Resource safety): forbid in-place mutation on gradient-tracked,
                // view-aliased, or storage-aliased source tensors.
                if let Some(recv_w) = receiver_witness {
                    if self.expr_produces_grad_tensor(recv_w) {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: "non-differentiable tensor for in-place mutation".to_string(),
                            got: format!(
                                "cannot perform in-place '{method}' on a gradient-tracked tensor (requires_grad=true)"
                            ),
                            span: Some(span),
                        }]);
                    }
                    if self.expr_produces_tensor_view(recv_w) || self.expr_is_aliased_source(recv_w)
                    {
                        return Err(vec![TypeError::UnificationFailure {
                            expected: "unique contiguous owning tensor for in-place mutation"
                                .to_string(),
                            got: format!(
                                "cannot perform in-place '{method}' on an aliased or strided tensor view (call .contiguous() first)"
                            ),
                            span: Some(span),
                        }]);
                    }
                    if let WitnessKind::Variable(v) = &recv_w.kind
                        && let Some((param_names, mut_set)) = self.active_fn_mut_stack.last_mut()
                        && let Some(pos) = param_names.iter().position(|p| p == v)
                    {
                        mut_set.insert(pos);
                    }
                }
                // Check 1/2/3 (DType, Shape, Device) on the mutation operand.
                let rhs_ty = self.subst.apply(&arg_types[0]);
                if method == "fill_" {
                    let _ = self.unify_tensor_dtypes(&cur_dtype, &rhs_ty, args[0].span)?;
                } else {
                    match &rhs_ty {
                        Type::Tensor {
                            dtype: d2,
                            shape: s2,
                            device: dev2,
                        } => {
                            let _ = self.unify_tensor_dtypes(&cur_dtype, d2, args[0].span)?;
                            let _ = self.unify_tensor_devices(&cur_device, dev2, args[0].span)?;
                            let rhs_dummy = Type::Tensor {
                                dtype: Box::new(cur_dtype.clone()),
                                shape: s2.clone(),
                                device: cur_device.clone(),
                            };
                            let lhs_dummy = Type::Tensor {
                                dtype: Box::new(cur_dtype.clone()),
                                shape: cur_shape.clone(),
                                device: cur_device.clone(),
                            };
                            match unify::solve_spanned(
                                &SpannedConstraint::new(
                                    Constraint::Eq(Box::new(lhs_dummy), Box::new(rhs_dummy)),
                                    args[0].span,
                                ),
                                &self.subst,
                            ) {
                                Ok(new_s) => self.subst = new_s,
                                Err(e) => return Err(vec![e]),
                            }
                        }
                        _ => {
                            let _ = self.unify_tensor_dtypes(&cur_dtype, &rhs_ty, args[0].span)?;
                        }
                    }
                }
                // Check 5 (Effect): record in-place tensor mutation effect in the active EffectRow.
                acc_row = self.merge_rows(
                    acc_row,
                    crate::mir::effect::EffectRow::Cons(
                        crate::mir::effect::ambient::TENSOR_MUTATE_LABEL.to_string(),
                        Box::new(crate::mir::effect::EffectRow::Empty),
                    ),
                    span,
                );
                Ok((
                    Type::Tensor {
                        dtype: Box::new(self.subst.apply(&cur_dtype)),
                        shape: self.subst.apply_shape(&cur_shape),
                        device: self.subst.apply_device(&cur_device),
                    },
                    acc_row,
                ))
            }
            _ => Err(vec![TypeError::UnificationFailure {
                expected: "known Tensor method".to_string(),
                got: format!("Tensor.{method}"),
                span: Some(span),
            }]),
        }
    }

    /// Stage 3B: Native HM type inference for `linalg.*`, `math.*`, and `stats.*` calls
    /// when invoked on `Type::Tensor` or hardware-width `Type::Scalar` operands.
    fn try_infer_module_tensor_method(
        &mut self,
        module: &str,
        method: &str,
        args: &[MirWitness],
        arg_types: &[Type],
        acc_row: &crate::mir::effect::EffectRow,
        span: Span,
    ) -> Option<Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>>> {
        let resolved: Vec<Type> = arg_types.iter().map(|t| self.subst.apply(t)).collect();
        if module == "file" {
            match method {
                "read_binary" => {
                    if args.len() != 1 {
                        return Some(Err(vec![TypeError::ArityMismatch {
                            expected: 1,
                            actual: args.len(),
                            span,
                        }]));
                    }
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Subtype {
                            sub: Box::new(resolved[0].clone()),
                            sup: Box::new(Type::String),
                        },
                        args[0].span,
                    ));
                    let id = self.fresh_counter;
                    self.fresh_counter += 1;
                    return Some(Ok((
                        Type::Tensor {
                            dtype: Box::new(Type::Scalar(crate::common::DType::U8)),
                            shape: crate::typeck::ShapeType::Ranked(vec![
                                crate::typeck::DimExpr::Var(id),
                            ]),
                            device: crate::typeck::DeviceType::Concrete(crate::common::Device::Cpu),
                        },
                        acc_row.clone(),
                    )));
                }
                "read_binary_range" => {
                    if args.len() != 3 {
                        return Some(Err(vec![TypeError::ArityMismatch {
                            expected: 3,
                            actual: args.len(),
                            span,
                        }]));
                    }
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Subtype {
                            sub: Box::new(resolved[0].clone()),
                            sup: Box::new(Type::String),
                        },
                        args[0].span,
                    ));
                    for idx in [1usize, 2usize] {
                        self.constraints.push(SpannedConstraint::new(
                            Constraint::Subtype {
                                sub: Box::new(resolved[idx].clone()),
                                sup: Box::new(Type::Int),
                            },
                            args[idx].span,
                        ));
                    }
                    let id = self.fresh_counter;
                    self.fresh_counter += 1;
                    return Some(Ok((
                        Type::Tensor {
                            dtype: Box::new(Type::Scalar(crate::common::DType::U8)),
                            shape: crate::typeck::ShapeType::Ranked(vec![
                                crate::typeck::DimExpr::Var(id),
                            ]),
                            device: crate::typeck::DeviceType::Concrete(crate::common::Device::Cpu),
                        },
                        acc_row.clone(),
                    )));
                }
                "write_binary" => {
                    if args.len() != 2 {
                        return Some(Err(vec![TypeError::ArityMismatch {
                            expected: 2,
                            actual: args.len(),
                            span,
                        }]));
                    }
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Subtype {
                            sub: Box::new(resolved[0].clone()),
                            sup: Box::new(Type::String),
                        },
                        args[0].span,
                    ));
                    self.constraints.push(SpannedConstraint::new(
                        Constraint::Subtype {
                            sub: Box::new(resolved[1].clone()),
                            sup: Box::new(Type::Tensor {
                                dtype: Box::new(Type::Scalar(crate::common::DType::U8)),
                                shape: crate::typeck::ShapeType::Unranked(None),
                                device: crate::typeck::DeviceType::Any,
                            }),
                        },
                        args[1].span,
                    ));
                    return Some(Ok((Type::Nil, acc_row.clone())));
                }
                _ => {}
            }
        }
        let has_tensor = resolved.iter().any(|t| matches!(t, Type::Tensor { .. }));
        let has_scalar = resolved.iter().any(|t| matches!(t, Type::Scalar(_)));
        if !has_tensor && !has_scalar {
            return None;
        }

        match module {
            "linalg" if has_tensor => match method {
                "matmul" => {
                    if args.len() != 2 {
                        return Some(Err(vec![TypeError::ArityMismatch {
                            expected: 2,
                            actual: args.len(),
                            span,
                        }]));
                    }
                    Some(
                        self.infer_tensor_matmul_op(&resolved[0], &resolved[1], span)
                            .map(|ty| (ty, acc_row.clone())),
                    )
                }
                "dot" => {
                    if args.len() != 2 {
                        return Some(Err(vec![TypeError::ArityMismatch {
                            expected: 2,
                            actual: args.len(),
                            span,
                        }]));
                    }
                    if let (Type::Tensor { shape: s1, .. }, Type::Tensor { shape: s2, .. }) =
                        (&resolved[0], &resolved[1])
                        && (matches!(s1, crate::typeck::ShapeType::Ranked(d) if d.len() != 1)
                            || matches!(s2, crate::typeck::ShapeType::Ranked(d) if d.len() != 1))
                    {
                        return Some(Err(vec![TypeError::UnificationFailure {
                            expected: "1D tensors for linalg.dot".to_string(),
                            got: format!("{s1} and {s2}"),
                            span: Some(span),
                        }]));
                    }
                    Some(
                        self.infer_tensor_matmul_op(&resolved[0], &resolved[1], span)
                            .map(|ty| (ty, acc_row.clone())),
                    )
                }
                "cross" => {
                    if args.len() != 2 {
                        return Some(Err(vec![TypeError::ArityMismatch {
                            expected: 2,
                            actual: args.len(),
                            span,
                        }]));
                    }
                    if let (
                        Type::Tensor {
                            dtype: d1,
                            shape: s1,
                            device: dev1,
                        },
                        Type::Tensor {
                            dtype: d2,
                            shape: s2,
                            device: dev2,
                        },
                    ) = (&resolved[0], &resolved[1])
                    {
                        let out_dt = match self.unify_tensor_dtypes(d1, d2, span) {
                            Ok(d) => d,
                            Err(e) => return Some(Err(e)),
                        };
                        let out_dev = match self.unify_tensor_devices(dev1, dev2, span) {
                            Ok(d) => d,
                            Err(e) => return Some(Err(e)),
                        };
                        let expected_3 =
                            crate::typeck::ShapeType::Ranked(vec![crate::typeck::DimExpr::Known(
                                3,
                            )]);
                        if !s1.compatible_with(&expected_3) || !s2.compatible_with(&expected_3) {
                            return Some(Err(vec![TypeError::UnificationFailure {
                                expected: "1D tensors of shape [3] for linalg.cross".to_string(),
                                got: format!("{s1} and {s2}"),
                                span: Some(span),
                            }]));
                        }
                        return Some(Ok((
                            Type::Tensor {
                                dtype: Box::new(out_dt),
                                shape: expected_3,
                                device: out_dev,
                            },
                            acc_row.clone(),
                        )));
                    }
                    Some(Err(vec![TypeError::UnificationFailure {
                        expected: "two 1D tensors of shape [3] for linalg.cross".to_string(),
                        got: format!("{}, {}", resolved[0].name(), resolved[1].name()),
                        span: Some(span),
                    }]))
                }
                "transpose" => {
                    if args.len() != 1 {
                        return Some(Err(vec![TypeError::ArityMismatch {
                            expected: 1,
                            actual: args.len(),
                            span,
                        }]));
                    }
                    Some(self.infer_tensor_method_call(
                        args.first(),
                        &resolved[0],
                        "transpose",
                        &[],
                        &[],
                        acc_row.clone(),
                        span,
                    ))
                }
                "norm" => {
                    if args.is_empty() || args.len() > 2 {
                        return Some(Err(vec![TypeError::ArityMismatch {
                            expected: 1,
                            actual: args.len(),
                            span,
                        }]));
                    }
                    if let Type::Tensor { dtype, .. } = &resolved[0]
                        && self
                            .subst
                            .apply(dtype)
                            .as_dtype()
                            .is_some_and(|d| d.is_bool())
                    {
                        return Some(Err(vec![TypeError::UnificationFailure {
                            expected: "numeric tensor for linalg.norm".to_string(),
                            got: resolved[0].name(),
                            span: Some(span),
                        }]));
                    }
                    Some(Ok((Type::Float, acc_row.clone())))
                }
                _ => None,
            },
            "math" => {
                let is_unary = matches!(
                    method,
                    "sin"
                        | "cos"
                        | "tan"
                        | "asin"
                        | "acos"
                        | "atan"
                        | "sinh"
                        | "cosh"
                        | "tanh"
                        | "exp"
                        | "log"
                        | "log2"
                        | "log10"
                        | "log1p"
                        | "sqrt"
                        | "cbrt"
                        | "fract"
                        | "abs"
                        | "sign"
                        | "floor"
                        | "ceil"
                        | "round"
                        | "trunc"
                );
                let is_binary = matches!(method, "pow" | "hypot" | "atan2");
                if is_unary && args.len() == 1 {
                    match &resolved[0] {
                        Type::Tensor {
                            dtype,
                            shape,
                            device,
                        } => {
                            let dt_r = self.subst.apply(dtype);
                            if dt_r.as_dtype().is_some_and(|d| d.is_bool()) {
                                return Some(Err(vec![TypeError::UnificationFailure {
                                    expected: format!("numeric tensor for math.{method}"),
                                    got: resolved[0].name(),
                                    span: Some(span),
                                }]));
                            }
                            return Some(Ok((
                                Type::Tensor {
                                    dtype: Box::new(dt_r),
                                    shape: self.subst.apply_shape(shape),
                                    device: self.subst.apply_device(device),
                                },
                                acc_row.clone(),
                            )));
                        }
                        Type::Scalar(dt) => {
                            let ret = if dt.is_float()
                                || matches!(
                                    method,
                                    "abs" | "sign" | "floor" | "ceil" | "round" | "trunc"
                                ) {
                                Type::Scalar(*dt)
                            } else {
                                Type::Float
                            };
                            return Some(Ok((ret, acc_row.clone())));
                        }
                        _ => return None,
                    }
                }
                if is_binary && args.len() == 2 && has_tensor {
                    return Some(
                        self.infer_tensor_binary_op(&resolved[0], &resolved[1], span)
                            .map(|ty| (ty, acc_row.clone())),
                    );
                }
                if is_binary
                    && args.len() == 2
                    && let (Type::Scalar(d1), Type::Scalar(d2)) = (&resolved[0], &resolved[1])
                    && d1 == d2
                    && d1.is_float()
                {
                    return Some(Ok((Type::Scalar(*d1), acc_row.clone())));
                }
                None
            }
            "stats" if has_tensor => {
                for (arg_w, ty) in args.iter().zip(resolved.iter()) {
                    if let Type::Tensor { dtype, .. } = ty
                        && self
                            .subst
                            .apply(dtype)
                            .as_dtype()
                            .is_some_and(|d| d.is_bool())
                    {
                        return Some(Err(vec![TypeError::UnificationFailure {
                            expected: format!("numeric tensor for stats.{method}"),
                            got: ty.name(),
                            span: Some(arg_w.span),
                        }]));
                    }
                }
                match method {
                    "sum" | "mean" | "median" | "var" | "stddev" | "min" | "max" => {
                        if args.len() != 1 {
                            return Some(Err(vec![TypeError::ArityMismatch {
                                expected: 1,
                                actual: args.len(),
                                span,
                            }]));
                        }
                        Some(Ok((Type::Float, acc_row.clone())))
                    }
                    "quantile" | "corr" | "cov" => {
                        if args.len() != 2 {
                            return Some(Err(vec![TypeError::ArityMismatch {
                                expected: 2,
                                actual: args.len(),
                                span,
                            }]));
                        }
                        Some(Ok((Type::Float, acc_row.clone())))
                    }
                    "histogram" => {
                        if args.len() != 2 {
                            return Some(Err(vec![TypeError::ArityMismatch {
                                expected: 2,
                                actual: args.len(),
                                span,
                            }]));
                        }
                        Some(Ok((
                            Type::List(Box::new(Type::Dict(
                                Box::new(Type::String),
                                Box::new(Type::Float),
                            ))),
                            acc_row.clone(),
                        )))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::Span;
    use crate::mir::witness::WitnessPattern;

    // v0.76.02: infer_pattern 5 变体测试

    #[test]
    fn pattern_wildcard_always_succeeds() {
        let mut hm = HMInference::new();
        // Wildcard 任何 type 都 match
        let r = hm.infer_pattern(&WitnessPattern::Wildcard, &Type::Int, Span::default());
        assert!(r.is_ok());
    }

    #[test]
    fn pattern_variable_always_succeeds() {
        // Variable 由 arm.body 内 Variable 引用触发 infer_var（env 查找）——
        // pattern inference 阶段 no-op
        let mut hm = HMInference::new();
        let r = hm.infer_pattern(
            &WitnessPattern::Variable("x".to_string()),
            &Type::Int,
            Span::default(),
        );
        assert!(r.is_ok());
    }

    #[test]
    fn pattern_tuple_non_list_scrutinee_errors() {
        // Type enum 无 Tuple variant——非 List scrutinee 走保守路径
        // （视为 1 元素，items.len() != 1 时报错）
        let mut hm = HMInference::new();
        let items = vec![WitnessPattern::Wildcard, WitnessPattern::Wildcard];
        let r = hm.infer_pattern(&WitnessPattern::Tuple(items), &Type::Int, Span::default());
        assert!(r.is_err(), "non-list scrutinee + 2-tuple pattern 应报错");
    }

    #[test]
    fn pattern_list_head_tail_succeeds() {
        // List scrutinee 上 head/tail pattern 推断
        let mut hm = HMInference::new();
        let elem_ty = Type::Int;
        let list_ty = Type::List(Box::new(elem_ty.clone()));
        let r = hm.infer_pattern(
            &WitnessPattern::List {
                head: Box::new(WitnessPattern::Wildcard),
                tail: Box::new(WitnessPattern::Wildcard),
            },
            &list_ty,
            Span::default(),
        );
        assert!(r.is_ok());
    }

    #[test]
    fn pattern_listvec_rest_succeeds() {
        // v0.87: ListVec [a, b, ..rest] on List<Int>
        let mut hm = HMInference::new();
        let elem_ty = Type::Int;
        let list_ty = Type::List(Box::new(elem_ty.clone()));
        let r = hm.infer_pattern(
            &WitnessPattern::ListVec {
                elements: vec![WitnessPattern::Wildcard, WitnessPattern::Wildcard],
                rest: Some(Box::new(WitnessPattern::Variable("rest".to_string()))),
            },
            &list_ty,
            Span::default(),
        );
        assert!(r.is_ok());
    }

    #[test]
    fn pattern_listvec_no_rest_succeeds() {
        // v0.87: ListVec [a, b] (fixed-length) on List<Int>
        let mut hm = HMInference::new();
        let elem_ty = Type::Int;
        let list_ty = Type::List(Box::new(elem_ty.clone()));
        let r = hm.infer_pattern(
            &WitnessPattern::ListVec {
                elements: vec![WitnessPattern::Wildcard, WitnessPattern::Wildcard],
                rest: None,
            },
            &list_ty,
            Span::default(),
        );
        assert!(r.is_ok());
    }

    #[test]
    fn pattern_dict_required_keys_succeeds() {
        // Dict scrutinee 上 required key/value subpattern 推断
        let mut hm = HMInference::new();
        let value_ty = Type::Int;
        let dict_ty = Type::Dict(Box::new(Type::String), Box::new(value_ty));
        let r = hm.infer_pattern(
            &WitnessPattern::Dict {
                required: vec![("k".to_string(), WitnessPattern::Wildcard)],
                rest: false,
            },
            &dict_ty,
            Span::default(),
        );
        assert!(r.is_ok());
    }

    #[test]
    fn pattern_typeascription_unknown_name_errors() {
        // TypeAscription 名字必须 is_known_type
        let mut hm = HMInference::new();
        let r = hm.infer_pattern(
            &WitnessPattern::TypeAscription {
                name: "not_a_real_type".to_string(),
                pattern: Box::new(WitnessPattern::Wildcard),
            },
            &Type::Int,
            Span::default(),
        );
        assert!(r.is_err(), "unknown type name 应报错");
    }

    #[test]
    fn pattern_typeascription_known_name_succeeds() {
        // v0.76.02: is_known_type 名单内合法类型（"any" / "list" / "string" / etc.）
        // 实际 parser 接受 "int"/"float" 等简写——那是 parser alias 层，不在
        // is_known_type 名单（双层语义）。这里用 "any" 验证核心逻辑：
        // 合法 known type → 通过。
        let mut hm = HMInference::new();
        let r = hm.infer_pattern(
            &WitnessPattern::TypeAscription {
                name: "any".to_string(),
                pattern: Box::new(WitnessPattern::Wildcard),
            },
            &Type::Any,
            Span::default(),
        );
        assert!(r.is_ok());
    }

    // v0.84: Layer 2a — Sequence 推断

    fn make_lit_w(n: i64) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Literal(crate::common::Literal::Int(n, Span::default())),
            span: Span::default(),
        }
    }

    fn make_var_w(name: &str) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Variable(name.to_string()),
            span: Span::default(),
        }
    }

    #[test]
    fn sequence_empty_returns_nil() {
        let mut hm = HMInference::new();
        let result = hm.infer_sequence(&[], Span::default());
        assert!(result.is_ok());
        let (ty, row) = result.unwrap();
        assert_eq!(ty, Type::Nil);
        assert!(matches!(row, crate::mir::effect::EffectRow::Empty));
    }

    #[test]
    fn sequence_returns_last_expr_type() {
        let mut hm = HMInference::new();
        let exprs = vec![make_lit_w(1), make_lit_w(2), make_lit_w(42)];
        let result = hm.infer_sequence(&exprs, Span::default());
        assert!(result.is_ok(), "all-int sequence should succeed");
        // Solve constraints to get concrete types
        let (ty, _row) = result.unwrap();
        // After constraint solving, all ints unify to Int
        let result = hm.instantiate_type(&ty);
        // The inferred type is a TypeVar that got constrained to Int via
        // the literal 42. With Unknown-compatible-with, the literals
        // produce concrete Int types.
        assert!(
            matches!(result, Type::Int) || matches!(result, Type::TypeVar(_)),
            "expected Int or TypeVar, got {:?}",
            result
        );
    }

    #[test]
    fn sequence_merges_effect_rows() {
        let mut hm = HMInference::new();
        // Two int literals — pure, no effects. Row stays Empty.
        let exprs = vec![make_lit_w(1), make_lit_w(2)];
        let result = hm.infer_sequence(&exprs, Span::default());
        assert!(result.is_ok());
        let (_ty, row) = result.unwrap();
        assert!(
            matches!(row, crate::mir::effect::EffectRow::Empty),
            "pure sequence should have Empty row"
        );
    }

    #[test]
    fn sequence_with_variable_infer_ok() {
        let mut hm = HMInference::new();
        // Register x as Int in env
        hm.env.add("x".to_string(), crate::typeck::Type::Int);
        let exprs = vec![make_var_w("x")];
        let result = hm.infer_sequence(&exprs, Span::default());
        assert!(result.is_ok(), "variable lookup should succeed");
        let (ty, _row) = result.unwrap();
        assert_eq!(ty, Type::Int);
    }

    // v0.84: Layer 2b — Closure 重复参数检测

    #[test]
    fn closure_duplicate_param_names_error() {
        use crate::mir::witness::WitnessParam;
        let params = vec![
            WitnessParam {
                name: "x".to_string(),
                type_hint: None,
                default: None,
            },
            WitnessParam {
                name: "x".to_string(), // duplicate
                type_hint: None,
                default: None,
            },
        ];
        let body = make_lit_w(1);
        let mut hm = HMInference::new();
        let result = hm.infer_closure(&[], &params, &body, Span::default());
        assert!(result.is_err(), "duplicate param 'x' should error");
        let errors = result.unwrap_err();
        assert!(
            errors.iter().any(|e| {
                let msg = format!("{:?}", e);
                msg.contains("duplicate") || msg.contains("x")
            }),
            "error should mention duplicate param, got: {:?}",
            errors
        );
    }

    #[test]
    fn closure_distinct_params_ok() {
        use crate::mir::witness::WitnessParam;
        let params = vec![
            WitnessParam {
                name: "x".to_string(),
                type_hint: None,
                default: None,
            },
            WitnessParam {
                name: "y".to_string(),
                type_hint: None,
                default: None,
            },
        ];
        let body = make_lit_w(1);
        let mut hm = HMInference::new();
        let result = hm.infer_closure(&[], &params, &body, Span::default());
        assert!(result.is_ok(), "distinct params should succeed");
        // Result is Arrow(TypeVar, Arrow(TypeVar, TypeVar, Empty), Empty)
        let (ty, row) = result.unwrap();
        assert!(matches!(ty, Type::Arrow(_, _, _)));
        assert!(matches!(row, crate::mir::effect::EffectRow::Empty));
    }

    #[test]
    fn closure_single_param_ok() {
        use crate::mir::witness::WitnessParam;
        let params = vec![WitnessParam {
            name: "x".to_string(),
            type_hint: None,
            default: None,
        }];
        let body = make_lit_w(42);
        let mut hm = HMInference::new();
        let result = hm.infer_closure(&[], &params, &body, Span::default());
        assert!(result.is_ok());
        let (ty, row) = result.unwrap();
        // Single param → Arrow(param, body_ty, row)
        assert!(matches!(ty, Type::Arrow(_, _, _)));
        assert!(matches!(row, crate::mir::effect::EffectRow::Empty));
    }

    #[test]
    fn closure_type_hint_param_ok() {
        use crate::mir::witness::WitnessParam;
        let hint = TypeHint::from_type(Type::Float);
        let params = vec![WitnessParam {
            name: "x".to_string(),
            type_hint: Some(hint),
            default: None,
        }];
        let body = make_lit_w(1);
        let mut hm = HMInference::new();
        let result = hm.infer_closure(&[], &params, &body, Span::default());
        assert!(result.is_ok(), "typed param should succeed");
    }
}
