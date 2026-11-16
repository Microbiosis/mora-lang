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
        } else if let WitnessKind::Variable(rhs) = &value.kind
            && let Some(&arity) = self.fn_arities.get(rhs)
        {
            self.fn_arities.insert(name.to_string(), arity);
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
        let ty_inner = self.resolve_type_aliases(type_hint.to_type());
        let (value_ty, value_row) = self.infer_recover(value);
        if let WitnessKind::Closure { params, .. } | WitnessKind::FnDef { params, .. } = &value.kind
        {
            self.fn_arities.insert(name.to_string(), params.len());
        }
        if !matches!(ty_inner, Type::Any | Type::TraitObject { .. })
            && !matches!(value_ty, Type::Error)
        {
            self.constraints.push(SpannedConstraint::new(
                Constraint::Subtype {
                    sub: Box::new(value_ty),
                    sup: Box::new(ty_inner.clone()),
                },
                value.span,
            ));
        }
        let gen_hint = generalize::generalize(&ty_inner, &self.free_in_resolved_env());
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
        Ok((value_ty, value_row))
    }

    pub(super) fn infer_var(&mut self, name: &str, span: Span) -> Result<Type, Vec<TypeError>> {
        if let Some(ty) = self.env.get(name).cloned() {
            return Ok(self.instantiate_named_binding(name, &ty, span));
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
                        expected: "list<number> or list<list<number>>".to_string(),
                        got: left_ty.name(),
                        span: Some(span),
                    }]);
                };
                let Some((dr, er)) = matmul_shape(&right_ty) else {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: "list<number> or list<list<number>>".to_string(),
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
                if !left_ty.compatible_with(&right_ty) {
                    return Err(vec![TypeError::UnificationFailure {
                        expected: left_ty.name(),
                        got: right_ty.name(),
                        span: Some(span),
                    }]);
                }
                if is_numeric(&left_ty) && is_numeric(&right_ty) {
                    // Int == Float valid.
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
            && let Some(variants) = self.enum_defs.get(type_name)
        {
            if !variants.contains(member) {
                return Err(vec![TypeError::UnboundVariable {
                    name: name.to_string(),
                    span,
                }]);
            }
            return Ok((
                Type::Trait {
                    name: type_name.to_string(),
                    generics: vec![],
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
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        if let WitnessCallee::Method(_, method) = callee {
            let Some((recv, method_args)) = args.split_first() else {
                return Err(vec![TypeError::ArityMismatch {
                    expected: 1,
                    actual: 0,
                    span,
                }]);
            };
            return self.infer_method_call(recv, method, method_args, span);
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
                _ => return Ok((self.fresh_type_var(), acc_row)),
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
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let (recv_ty, recv_row) = self.infer_recover(receiver);
        let recv_module_name = match &receiver.kind {
            WitnessKind::Variable(module) => Some(module.as_str()),
            _ => None,
        };
        self.infer_method_call_with_recv(recv_ty, recv_row, recv_module_name, method, args, span)
    }

    pub(super) fn infer_method_call_with_recv(
        &mut self,
        recv_ty: Type,
        recv_row: crate::mir::effect::EffectRow,
        recv_module_name: Option<&str>,
        method: &str,
        args: &[MirWitness],
        span: Span,
    ) -> Result<(Type, crate::mir::effect::EffectRow), Vec<TypeError>> {
        let mut acc_row = recv_row;
        let mut arg_types: Vec<Type> = Vec::with_capacity(args.len());
        for a in args {
            let (t, r) = self.infer_recover(a);
            arg_types.push(t);
            acc_row = self.merge_rows(acc_row, r, span);
        }

        let resolved_recv = self.subst.apply(&recv_ty);
        if matches!(resolved_recv, Type::Error) {
            return Ok((Type::Error, acc_row));
        }
        if matches!(resolved_recv, Type::RandomModule) {
            return self.infer_random_method(method, arg_types, acc_row, span);
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
        let module_sig = mod_name
            .and_then(|module| crate::typeck::dispatch::module_method_signature(module, method));
        let resolved_sig = module_sig
            .clone()
            .or_else(|| crate::typeck::dispatch::method_signature(&resolved_recv, method));
        let module_return_ty = module_sig.map(|s| s.return_type);

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

    fn infer_closure_core(
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
        for (p, ty) in params.iter().zip(param_types.iter()) {
            self.record_binding(span, &p.name, ty);
            self.env.add(p.name.clone(), ty.clone());
        }
        self.return_targets.push(expected_ret);
        self.closure_depth += 1;
        let body_res = self.infer_expr(body);
        self.closure_depth -= 1;
        self.return_targets.pop();
        self.env = saved_env;
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
        let ty = if type_params.is_empty() {
            arrow
        } else {
            let rigids: Vec<TypeVarId> = type_params
                .iter()
                .map(|p| TypeVarId::Rigid(p.clone()))
                .collect();
            Type::ForAll(rigids, Box::new(arrow))
        };
        self.closure_rows.insert(span, body_row);
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
        let (body_ty, body_row) =
            self.infer_closure_core(&param_types, params, ret_placeholder.clone(), body, span)?;

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
        let final_ty = if !type_params.is_empty() {
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
            let rigids: Vec<TypeVarId> = type_params
                .iter()
                .map(|p| TypeVarId::Rigid(p.clone()))
                .collect();
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
            WitnessSpawnTarget::Call { callee, args } => self.infer_call(callee, args, span)?,
            WitnessSpawnTarget::MethodCall {
                receiver,
                method,
                args,
            } => self.infer_method_call(receiver, method, args, span)?,
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

    fn infer_pattern(
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
