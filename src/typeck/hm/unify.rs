//! v0.53: Unification Algorithm for HM Type Inference
//!
//! Implements the unification procedure to solve type constraints.
//! Core of HM inference: finds most general unifier (MGU) for two types.

use std::collections::{HashMap, HashSet};

use super::error::TypeError;
use crate::common::Span;
use crate::typeck::{DeviceType, DimExpr, ShapeType, Type, TypeVarId};

/// Substitution mapping inference meta-variables, effect row variables, and tensor shape/device variables.
#[derive(Debug, Clone)]
pub struct Substitution {
    mapping: HashMap<TypeVarId, Type>,
    row_mapping: HashMap<String, crate::mir::effect::EffectRow>,
    dim_mapping: HashMap<u32, DimExpr>,
    shape_mapping: HashMap<TypeVarId, ShapeType>,
    device_mapping: HashMap<TypeVarId, DeviceType>,
}

impl Substitution {
    pub fn new() -> Self {
        Self {
            mapping: HashMap::new(),
            row_mapping: HashMap::new(),
            dim_mapping: HashMap::new(),
            shape_mapping: HashMap::new(),
            device_mapping: HashMap::new(),
        }
    }

    pub fn lookup(&self, id: &TypeVarId) -> Option<&Type> {
        self.mapping.get(id)
    }

    pub fn lookup_row(&self, name: &str) -> Option<&crate::mir::effect::EffectRow> {
        self.row_mapping.get(name)
    }

    pub fn bind_row(&mut self, name: String, row: crate::mir::effect::EffectRow) {
        self.row_mapping.insert(name, row);
    }

    pub fn apply_dim(&self, dim: &DimExpr) -> DimExpr {
        match dim {
            DimExpr::Var(id) => {
                if let Some(resolved) = self.dim_mapping.get(id)
                    && resolved != dim
                {
                    return self.apply_dim(resolved);
                }
                dim.clone()
            }
            other => other.clone(),
        }
    }

    pub fn apply_shape(&self, shape: &ShapeType) -> ShapeType {
        match shape {
            ShapeType::Ranked(dims) => {
                ShapeType::Ranked(dims.iter().map(|d| self.apply_dim(d)).collect())
            }
            ShapeType::Unranked(Some(var)) => {
                if let Some(resolved) = self.shape_mapping.get(var)
                    && resolved != shape
                {
                    return self.apply_shape(resolved);
                }
                shape.clone()
            }
            ShapeType::Unranked(None) => ShapeType::Unranked(None),
        }
    }

    pub fn apply_device(&self, device: &DeviceType) -> DeviceType {
        match device {
            DeviceType::Var(var) => {
                if let Some(resolved) = self.device_mapping.get(var)
                    && resolved != device
                {
                    return self.apply_device(resolved);
                }
                device.clone()
            }
            other => other.clone(),
        }
    }

    /// Apply substitution transitively to a type, avoiding capture under `ForAll`.
    pub fn apply(&self, ty: &Type) -> Type {
        self.apply_with_bound(ty, &HashSet::new())
    }

    fn apply_with_bound(&self, ty: &Type, bound: &HashSet<TypeVarId>) -> Type {
        match ty {
            Type::TypeVar(id) => {
                if !bound.contains(id)
                    && let Some(resolved) = self.mapping.get(id)
                {
                    return self.apply_with_bound(resolved, bound);
                }
                ty.clone()
            }
            Type::List(elem) => Type::List(Box::new(self.apply_with_bound(elem, bound))),
            Type::Channel(elem) => Type::Channel(Box::new(self.apply_with_bound(elem, bound))),
            Type::TaskHandle(elem) => {
                Type::TaskHandle(Box::new(self.apply_with_bound(elem, bound)))
            }
            Type::Dict(key, value) => Type::Dict(
                Box::new(self.apply_with_bound(key, bound)),
                Box::new(self.apply_with_bound(value, bound)),
            ),
            Type::Result_(ok, err) => Type::Result_(
                Box::new(self.apply_with_bound(ok, bound)),
                Box::new(self.apply_with_bound(err, bound)),
            ),
            Type::Tuple(elems) => Type::Tuple(
                elems
                    .iter()
                    .map(|e| Box::new(self.apply_with_bound(e, bound)))
                    .collect(),
            ),
            Type::Cons(h, t) => Type::Cons(
                Box::new(self.apply_with_bound(h, bound)),
                Box::new(self.apply_with_bound(t, bound)),
            ),
            Type::Union(members) => Type::Union(
                members
                    .iter()
                    .map(|m| self.apply_with_bound(m, bound))
                    .collect(),
            ),
            Type::Trait { name, generics } => Type::Trait {
                name: name.clone(),
                generics: generics
                    .iter()
                    .map(|g| self.apply_with_bound(g, bound))
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
                    .map(|g| self.apply_with_bound(g, bound))
                    .collect(),
                traits: traits
                    .iter()
                    .map(|t| self.apply_with_bound(t, bound))
                    .collect(),
            },
            Type::TraitObject {
                trait_name,
                generics,
            } => Type::TraitObject {
                trait_name: trait_name.clone(),
                generics: generics
                    .iter()
                    .map(|g| self.apply_with_bound(g, bound))
                    .collect(),
            },
            Type::ForAll(vars, inner) => {
                let mut next_bound = bound.clone();
                for v in vars {
                    next_bound.insert(v.clone());
                }
                Type::ForAll(
                    vars.clone(),
                    Box::new(self.apply_with_bound(inner, &next_bound)),
                )
            }
            Type::Arrow(input, output, row) => Type::Arrow(
                Box::new(self.apply_with_bound(input, bound)),
                Box::new(self.apply_with_bound(output, bound)),
                crate::typeck::hm::row::apply_row(row, self),
            ),
            Type::Tensor {
                dtype,
                shape,
                device,
            } => Type::Tensor {
                dtype: Box::new(self.apply_with_bound(dtype, bound)),
                shape: self.apply_shape(shape),
                device: self.apply_device(device),
            },
            _ => ty.clone(),
        }
    }

    pub fn extend(&self, var: TypeVarId, ty: Type) -> Result<Self, TypeError> {
        self.extend_at(var, ty, None)
    }

    pub fn extend_at(
        &self,
        var: TypeVarId,
        ty: Type,
        span: Option<Span>,
    ) -> Result<Self, TypeError> {
        if matches!(var, TypeVarId::Rigid(_)) {
            return Err(TypeError::UnificationFailure {
                expected: Type::TypeVar(var).name(),
                got: ty.name(),
                span,
            });
        }
        if occurs_in_subst(self, &var, &ty) {
            return Err(TypeError::OccursCheck {
                var,
                with_ty: ty.name(),
                span,
            });
        }

        let mut new_mapping = self.mapping.clone();
        new_mapping.insert(var, ty);
        Ok(Self {
            mapping: new_mapping,
            row_mapping: self.row_mapping.clone(),
            dim_mapping: self.dim_mapping.clone(),
            shape_mapping: self.shape_mapping.clone(),
            device_mapping: self.device_mapping.clone(),
        })
    }

    pub fn compose(&self, other: &Self) -> Self {
        let mut new_mapping = self.mapping.clone();
        for (var, ty) in &other.mapping {
            let applied_ty = self.apply(ty);
            new_mapping.insert(var.clone(), applied_ty);
        }
        let mut new_rows = self.row_mapping.clone();
        for (k, v) in &other.row_mapping {
            new_rows.insert(k.clone(), crate::typeck::hm::row::apply_row(v, self));
        }
        let mut new_dims = self.dim_mapping.clone();
        for (k, v) in &other.dim_mapping {
            new_dims.insert(*k, self.apply_dim(v));
        }
        let mut new_shapes = self.shape_mapping.clone();
        for (k, v) in &other.shape_mapping {
            new_shapes.insert(k.clone(), self.apply_shape(v));
        }
        let mut new_devices = self.device_mapping.clone();
        for (k, v) in &other.device_mapping {
            new_devices.insert(k.clone(), self.apply_device(v));
        }
        Self {
            mapping: new_mapping,
            row_mapping: new_rows,
            dim_mapping: new_dims,
            shape_mapping: new_shapes,
            device_mapping: new_devices,
        }
    }
}

impl Default for Substitution {
    fn default() -> Self {
        Self::new()
    }
}

/// Type constraint kind emitted during HM inference.
#[derive(Debug, Clone)]
pub enum Constraint {
    /// Two types must unify strictly.
    Eq(Box<Type>, Box<Type>),
    /// Subtype constraint `sub <: sup` (allows `Int <: Float`, rejects `Float <: Int`).
    Subtype { sub: Box<Type>, sup: Box<Type> },
    /// Numeric (or overloaded `+`) binary constraint with optional result unification.
    Numeric(BinaryConstraint),
    /// Row-polymorphic effect equation.
    RowEq(crate::mir::effect::EffectRow, crate::mir::effect::EffectRow),
}

/// Constraint paired with originating source span and optional diagnostic hint.
#[derive(Debug, Clone)]
pub struct SpannedConstraint {
    pub kind: Constraint,
    pub span: Span,
    pub hint: Option<String>,
}

impl SpannedConstraint {
    pub fn new(kind: Constraint, span: Span) -> Self {
        Self {
            kind,
            span,
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct BinaryConstraint {
    pub left: Box<Type>,
    pub right: Box<Type>,
    pub result: Option<Box<Type>>,
    pub allow_string: bool,
}

impl std::fmt::Display for Constraint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Constraint::Eq(t1, t2) => write!(f, "({} == {})", format_type(t1), format_type(t2)),
            Constraint::Subtype { sub, sup } => {
                write!(f, "({} <: {})", format_type(sub), format_type(sup))
            }
            Constraint::Numeric(bc) => write!(
                f,
                "(+ {}, {})",
                format_type(&bc.left),
                format_type(&bc.right)
            ),
            Constraint::RowEq(r1, r2) => write!(f, "({} ~ {})", r1, r2),
        }
    }
}

fn format_type(ty: &Type) -> String {
    ty.name()
}

pub(crate) fn remap_constraint(
    sc: &SpannedConstraint,
    mapping: &HashMap<TypeVarId, TypeVarId>,
    span_override: Option<Span>,
) -> SpannedConstraint {
    use crate::typeck::hm::generalize::remap_type_vars;
    let kind = match &sc.kind {
        Constraint::Eq(a, b) => Constraint::Eq(
            Box::new(remap_type_vars(a, mapping)),
            Box::new(remap_type_vars(b, mapping)),
        ),
        Constraint::Subtype { sub, sup } => Constraint::Subtype {
            sub: Box::new(remap_type_vars(sub, mapping)),
            sup: Box::new(remap_type_vars(sup, mapping)),
        },
        Constraint::Numeric(bc) => Constraint::Numeric(BinaryConstraint {
            left: Box::new(remap_type_vars(&bc.left, mapping)),
            right: Box::new(remap_type_vars(&bc.right, mapping)),
            result: bc
                .result
                .as_ref()
                .map(|r| Box::new(remap_type_vars(r, mapping))),
            allow_string: bc.allow_string,
        }),
        Constraint::RowEq(r1, r2) => Constraint::RowEq(r1.clone(), r2.clone()),
    };
    SpannedConstraint {
        kind,
        span: span_override.unwrap_or(sc.span),
        hint: sc.hint.clone(),
    }
}

pub(crate) fn constraint_mentions_any(c: &Constraint, vars: &HashSet<TypeVarId>) -> bool {
    use crate::typeck::hm::generalize::collect_free_vars;
    let has = |t: &Type| collect_free_vars(t).iter().any(|v| vars.contains(v));
    match c {
        Constraint::Eq(a, b) => has(a) || has(b),
        Constraint::Subtype { sub, sup } => has(sub) || has(sup),
        Constraint::Numeric(bc) => {
            has(&bc.left) || has(&bc.right) || bc.result.as_ref().is_some_and(|r| has(r))
        }
        Constraint::RowEq(_, _) => false,
    }
}

pub(crate) fn numeric_has_unbound_meta(bc: &BinaryConstraint, subst: &Substitution) -> bool {
    let l = subst.apply(&bc.left);
    let r = subst.apply(&bc.right);
    matches!(l, Type::TypeVar(TypeVarId::Meta(_))) || matches!(r, Type::TypeVar(TypeVarId::Meta(_)))
}

pub fn solve(constraint: &Constraint, subst: &Substitution) -> Result<Substitution, TypeError> {
    solve_at(constraint, subst, None)
}

pub fn solve_spanned(
    sc: &SpannedConstraint,
    subst: &Substitution,
) -> Result<Substitution, TypeError> {
    match solve_at(&sc.kind, subst, Some(sc.span)) {
        Ok(s) => Ok(s),
        Err(e) => Err(match &sc.hint {
            Some(hint) => e.with_hint(hint.clone()),
            None => e,
        }),
    }
}

fn solve_at(
    constraint: &Constraint,
    subst: &Substitution,
    span: Option<Span>,
) -> Result<Substitution, TypeError> {
    match constraint {
        Constraint::Eq(ty1, ty2) => unify_at(ty1, ty2, subst, span),
        Constraint::Subtype { sub, sup } => unify_subtype_at(sub, sup, subst, span),
        Constraint::Numeric(bc) => solve_numeric(bc, subst, span),
        Constraint::RowEq(r1, r2) => {
            let mut new_subst = subst.clone();
            crate::typeck::hm::row::unify_row_at(r1, r2, &mut new_subst, span)?;
            Ok(new_subst)
        }
    }
}

fn is_number_union(ty: &Type) -> bool {
    matches!(ty, Type::Union(ms) if !ms.is_empty() && ms.iter().all(|m| matches!(m, Type::Int | Type::Float)))
}

fn solve_numeric(
    bc: &BinaryConstraint,
    subst: &Substitution,
    span: Option<Span>,
) -> Result<Substitution, TypeError> {
    let left = subst.apply(&bc.left);
    let right = subst.apply(&bc.right);

    if matches!(left, Type::Error) || matches!(right, Type::Error) {
        if let Some(ref result) = bc.result {
            return unify_at(&Type::Error, result, subst, span);
        }
        return Ok(subst.clone());
    }
    if matches!(left, Type::Unknown) || matches!(right, Type::Unknown) {
        return Err(TypeError::UnificationFailure {
            expected: "numeric type (int or float)".to_string(),
            got: format!("{} and {}", format_type(&left), format_type(&right)),
            span,
        });
    }

    let promoted = match (&left, &right) {
        (Type::Int, Type::Int) => Type::Int,
        (Type::Float, Type::Float) | (Type::Int, Type::Float) | (Type::Float, Type::Int) => {
            Type::Float
        }
        (Type::Scalar(dt1), Type::Scalar(dt2)) if dt1 == dt2 => Type::Scalar(*dt1),
        (Type::Tensor { .. }, Type::Tensor { .. }) => {
            let s = unify_at(&left, &right, subst, span)?;
            let res_ty = s.apply(&left);
            if let Some(ref result) = bc.result {
                return unify_at(&res_ty, result, &s, span);
            }
            return Ok(s);
        }
        (Type::Float, u) | (u, Type::Float) if is_number_union(u) => Type::Float,
        (Type::Int, u) | (u, Type::Int) if is_number_union(u) => {
            Type::Union(vec![Type::Int, Type::Float])
        }
        (a, b) if is_number_union(a) && is_number_union(b) => {
            Type::Union(vec![Type::Int, Type::Float])
        }
        (Type::String, Type::String) if bc.allow_string => Type::String,
        (Type::Any, Type::Int) | (Type::Int, Type::Any) => Type::Int,
        (Type::Any, Type::Float) | (Type::Float, Type::Any) => Type::Float,
        (Type::Any, Type::Scalar(dt)) | (Type::Scalar(dt), Type::Any) => Type::Scalar(*dt),
        (Type::Any, u) | (u, Type::Any) if is_number_union(u) => u.clone(),
        (Type::Any, Type::String) | (Type::String, Type::Any) if bc.allow_string => Type::String,
        (Type::Any, Type::Any) => Type::Any,
        // Unresolved inference variables when paired with a concrete type:
        (Type::TypeVar(v @ TypeVarId::Meta(_)), Type::Int)
        | (Type::Int, Type::TypeVar(v @ TypeVarId::Meta(_))) => {
            let s = subst.extend_at(v.clone(), Type::Int, span)?;
            if let Some(ref result) = bc.result {
                return unify_at(&Type::Int, result, &s, span);
            }
            return Ok(s);
        }
        (Type::TypeVar(v @ TypeVarId::Meta(_)), Type::Float)
        | (Type::Float, Type::TypeVar(v @ TypeVarId::Meta(_))) => {
            let s = subst.extend_at(v.clone(), Type::Float, span)?;
            if let Some(ref result) = bc.result {
                return unify_at(&Type::Float, result, &s, span);
            }
            return Ok(s);
        }
        (Type::TypeVar(v @ TypeVarId::Meta(_)), Type::Scalar(dt))
        | (Type::Scalar(dt), Type::TypeVar(v @ TypeVarId::Meta(_))) => {
            let sty = Type::Scalar(*dt);
            let s = subst.extend_at(v.clone(), sty.clone(), span)?;
            if let Some(ref result) = bc.result {
                return unify_at(&sty, result, &s, span);
            }
            return Ok(s);
        }
        (Type::TypeVar(v @ TypeVarId::Meta(_)), t @ Type::Tensor { .. })
        | (t @ Type::Tensor { .. }, Type::TypeVar(v @ TypeVarId::Meta(_))) => {
            let s = subst.extend_at(v.clone(), t.clone(), span)?;
            if let Some(ref result) = bc.result {
                return unify_at(t, result, &s, span);
            }
            return Ok(s);
        }
        (Type::TypeVar(v @ TypeVarId::Meta(_)), u) | (u, Type::TypeVar(v @ TypeVarId::Meta(_)))
            if is_number_union(u) =>
        {
            let s = subst.extend_at(v.clone(), u.clone(), span)?;
            if let Some(ref result) = bc.result {
                return unify_at(u, result, &s, span);
            }
            return Ok(s);
        }
        (Type::TypeVar(v @ TypeVarId::Meta(_)), Type::String)
        | (Type::String, Type::TypeVar(v @ TypeVarId::Meta(_)))
            if bc.allow_string =>
        {
            let s = subst.extend_at(v.clone(), Type::String, span)?;
            if let Some(ref result) = bc.result {
                return unify_at(&Type::String, result, &s, span);
            }
            return Ok(s);
        }
        (Type::TypeVar(TypeVarId::Meta(_)), Type::TypeVar(TypeVarId::Meta(_))) => {
            let s = unify_at(&left, &right, subst, span)?;
            if let Some(ref result) = bc.result {
                return unify_at(&left, result, &s, span);
            }
            return Ok(s);
        }
        (Type::TypeVar(TypeVarId::Meta(_)), Type::Any)
        | (Type::Any, Type::TypeVar(TypeVarId::Meta(_))) => Type::Any,
        // Rigid generic parameters or non-numeric concrete types must fail here.
        _ => {
            let expected = if bc.allow_string {
                "numeric or string type".to_string()
            } else {
                "numeric type (int or float)".to_string()
            };
            return Err(TypeError::UnificationFailure {
                expected,
                got: format!("{} and {}", format_type(&left), format_type(&right)),
                span,
            });
        }
    };

    if let Some(ref result) = bc.result {
        unify_at(&promoted, result, subst, span)
    } else {
        Ok(subst.clone())
    }
}

fn unify_dim_at(
    d1: &DimExpr,
    d2: &DimExpr,
    subst: &mut Substitution,
    full_t1: &Type,
    full_t2: &Type,
    span: Option<Span>,
) -> Result<(), TypeError> {
    let a = subst.apply_dim(d1);
    let b = subst.apply_dim(d2);
    match (&a, &b) {
        (DimExpr::Known(x), DimExpr::Known(y)) if x == y => Ok(()),
        (DimExpr::Symbolic(s1), DimExpr::Symbolic(s2)) if s1 == s2 || s1 == "_" || s2 == "_" => {
            Ok(())
        }
        (DimExpr::Symbolic(s), _) | (_, DimExpr::Symbolic(s)) if s == "_" => Ok(()),
        (DimExpr::Var(id1), DimExpr::Var(id2)) if id1 == id2 => Ok(()),
        (DimExpr::Var(id), other) | (other, DimExpr::Var(id)) => {
            subst.dim_mapping.insert(*id, other.clone());
            Ok(())
        }
        _ => Err(TypeError::UnificationFailure {
            expected: format_type(&subst.apply(full_t1)),
            got: format_type(&subst.apply(full_t2)),
            span,
        }),
    }
}

fn unify_shape_at(
    s1: &ShapeType,
    s2: &ShapeType,
    subst: &mut Substitution,
    full_t1: &Type,
    full_t2: &Type,
    span: Option<Span>,
) -> Result<(), TypeError> {
    let a = subst.apply_shape(s1);
    let b = subst.apply_shape(s2);
    match (&a, &b) {
        (ShapeType::Unranked(None), _) | (_, ShapeType::Unranked(None)) => Ok(()),
        (ShapeType::Unranked(Some(v1)), ShapeType::Unranked(Some(v2))) if v1 == v2 => Ok(()),
        (ShapeType::Unranked(Some(v @ TypeVarId::Meta(_))), other)
        | (other, ShapeType::Unranked(Some(v @ TypeVarId::Meta(_)))) => {
            subst.shape_mapping.insert(v.clone(), other.clone());
            Ok(())
        }
        (ShapeType::Ranked(dims1), ShapeType::Ranked(dims2)) if dims1.len() == dims2.len() => {
            for (d1, d2) in dims1.iter().zip(dims2.iter()) {
                unify_dim_at(d1, d2, subst, full_t1, full_t2, span)?;
            }
            Ok(())
        }
        _ => Err(TypeError::UnificationFailure {
            expected: format_type(&subst.apply(full_t1)),
            got: format_type(&subst.apply(full_t2)),
            span,
        }),
    }
}

fn unify_device_at(
    dev1: &DeviceType,
    dev2: &DeviceType,
    subst: &mut Substitution,
    full_t1: &Type,
    full_t2: &Type,
    span: Option<Span>,
) -> Result<(), TypeError> {
    let a = subst.apply_device(dev1);
    let b = subst.apply_device(dev2);
    match (&a, &b) {
        (DeviceType::Any, _) | (_, DeviceType::Any) => Ok(()),
        (DeviceType::Concrete(d1), DeviceType::Concrete(d2)) if d1 == d2 => Ok(()),
        (DeviceType::Var(v1), DeviceType::Var(v2)) if v1 == v2 => Ok(()),
        (DeviceType::Var(v @ TypeVarId::Meta(_)), other)
        | (other, DeviceType::Var(v @ TypeVarId::Meta(_))) => {
            subst.device_mapping.insert(v.clone(), other.clone());
            Ok(())
        }
        _ => Err(TypeError::UnificationFailure {
            expected: format_type(&subst.apply(full_t1)),
            got: format_type(&subst.apply(full_t2)),
            span,
        }),
    }
}

#[cfg(test)]
fn unify(ty1: &Type, ty2: &Type, subst: &Substitution) -> Result<Substitution, TypeError> {
    unify_at(ty1, ty2, subst, None)
}

fn unify_at(
    ty1: &Type,
    ty2: &Type,
    subst: &Substitution,
    span: Option<Span>,
) -> Result<Substitution, TypeError> {
    let t1 = subst.apply(ty1);
    let t2 = subst.apply(ty2);

    match (&t1, &t2) {
        (Type::Error, _) | (_, Type::Error) => Ok(subst.clone()),
        (Type::Unknown, _) | (_, Type::Unknown) => Err(TypeError::UnificationFailure {
            expected: format_type(&t1),
            got: format_type(&t2),
            span,
        }),
        (Type::TypeVar(TypeVarId::Rigid(a)), Type::TypeVar(TypeVarId::Rigid(b))) => {
            if a == b {
                Ok(subst.clone())
            } else {
                Err(TypeError::UnificationFailure {
                    expected: format_type(&t1),
                    got: format_type(&t2),
                    span,
                })
            }
        }
        (Type::Any, _) | (_, Type::Any) => Ok(subst.clone()),
        (Type::TypeVar(v1 @ TypeVarId::Meta(_)), _) => {
            if let Type::TypeVar(v2) = &t2
                && v1 == v2
            {
                return Ok(subst.clone());
            }
            subst.extend_at(v1.clone(), t2.clone(), span)
        }
        (_, Type::TypeVar(v2 @ TypeVarId::Meta(_))) => {
            subst.extend_at(v2.clone(), t1.clone(), span)
        }
        (Type::TypeVar(TypeVarId::Rigid(_)), _) | (_, Type::TypeVar(TypeVarId::Rigid(_))) => {
            Err(TypeError::UnificationFailure {
                expected: format_type(&t1),
                got: format_type(&t2),
                span,
            })
        }
        (Type::ForAll(_, inner), other) | (other, Type::ForAll(_, inner)) => {
            unify_at(inner, other, subst, span)
        }
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
            let mut next = unify_at(d1, d2, subst, span)?;
            unify_shape_at(s1, s2, &mut next, &t1, &t2, span)?;
            unify_device_at(dev1, dev2, &mut next, &t1, &t2, span)?;
            Ok(next)
        }
        (Type::Cons(h1, t1_rest), Type::Cons(h2, t2_rest)) => {
            let s1 = unify_at(h1, h2, subst, span)?;
            unify_at(t1_rest, t2_rest, &s1, span)
        }
        (Type::List(elem1), Type::List(elem2)) => unify_at(elem1, elem2, subst, span),
        (Type::Channel(elem1), Type::Channel(elem2)) => unify_at(elem1, elem2, subst, span),
        (Type::TaskHandle(elem1), Type::TaskHandle(elem2)) => unify_at(elem1, elem2, subst, span),
        (Type::Dict(k1, v1), Type::Dict(k2, v2)) => {
            let s1 = unify_at(k1, k2, subst, span)?;
            unify_at(v1, v2, &s1, span)
        }
        (Type::Result_(ok1, err1), Type::Result_(ok2, err2)) => {
            let s1 = unify_at(ok1, ok2, subst, span)?;
            unify_at(err1, err2, &s1, span)
        }
        (Type::Tuple(a), Type::Tuple(b)) if a.len() == b.len() => {
            let mut s = subst.clone();
            for (ea, eb) in a.iter().zip(b.iter()) {
                s = unify_at(ea, eb, &s, span)?;
            }
            Ok(s)
        }
        (
            Type::Trait {
                name: a,
                generics: ga,
            },
            Type::Trait {
                name: b,
                generics: gb,
            },
        ) if a == b && ga.len() == gb.len() => {
            let mut s = subst.clone();
            for (xa, xb) in ga.iter().zip(gb.iter()) {
                s = unify_at(xa, xb, &s, span)?;
            }
            Ok(s)
        }
        (
            Type::Concrete {
                name: a,
                generics: ga,
                ..
            },
            Type::Concrete {
                name: b,
                generics: gb,
                ..
            },
        ) if a == b && ga.len() == gb.len() => {
            let mut s = subst.clone();
            for (xa, xb) in ga.iter().zip(gb.iter()) {
                s = unify_at(xa, xb, &s, span)?;
            }
            Ok(s)
        }
        (
            Type::TraitObject {
                trait_name: a,
                generics: ga,
            },
            Type::TraitObject {
                trait_name: b,
                generics: gb,
            },
        ) if (a.is_empty() || b.is_empty() || a == b) && ga.len() == gb.len() => {
            let mut s = subst.clone();
            for (xa, xb) in ga.iter().zip(gb.iter()) {
                s = unify_at(xa, xb, &s, span)?;
            }
            Ok(s)
        }
        (Type::Arrow(i1, o1, r1), Type::Arrow(i2, o2, r2)) => {
            let s1 = unify_at(i1, i2, subst, span)?;
            let s2 = unify_at(o1, o2, &s1, span)?;
            let mut s3 = s2;
            crate::typeck::hm::row::unify_row_at(r1, r2, &mut s3, span)?;
            Ok(s3)
        }
        (Type::Union(members), other) | (other, Type::Union(members)) => {
            if members.is_empty()
                || members.iter().any(|m| matches!(m, Type::Any))
                || (matches!(other, Type::Scalar(_))
                    && members.contains(&Type::Int)
                    && members.contains(&Type::Float))
            {
                Ok(subst.clone())
            } else {
                let mut special_err: Option<TypeError> = None;
                for m in members {
                    match unify_at(m, other, subst, span) {
                        Ok(s) => return Ok(s),
                        Err(
                            e @ (TypeError::OccursCheck { .. }
                            | TypeError::EffectRowMismatch { .. }),
                        ) => {
                            if special_err.is_none() {
                                special_err = Some(e);
                            }
                        }
                        Err(_) => {}
                    }
                }
                Err(special_err.unwrap_or(TypeError::UnificationFailure {
                    expected: format_type(&t1),
                    got: format_type(&t2),
                    span,
                }))
            }
        }
        _ if t1 == t2 => Ok(subst.clone()),
        _ => Err(TypeError::UnificationFailure {
            expected: format_type(&t1),
            got: format_type(&t2),
            span,
        }),
    }
}

fn unify_subtype_at(
    sub: &Type,
    sup: &Type,
    subst: &Substitution,
    span: Option<Span>,
) -> Result<Substitution, TypeError> {
    let t_sub = subst.apply(sub);
    let t_sup = subst.apply(sup);

    match (&t_sub, &t_sup) {
        (Type::Error, _) | (_, Type::Error) => Ok(subst.clone()),
        (Type::Unknown, _) | (_, Type::Unknown) => Err(TypeError::UnificationFailure {
            expected: format_type(&t_sup),
            got: format_type(&t_sub),
            span,
        }),
        (Type::TypeVar(TypeVarId::Rigid(a)), Type::TypeVar(TypeVarId::Rigid(b))) => {
            if a == b {
                Ok(subst.clone())
            } else {
                Err(TypeError::UnificationFailure {
                    expected: format_type(&t_sup),
                    got: format_type(&t_sub),
                    span,
                })
            }
        }
        (Type::Any, _) | (_, Type::Any) => Ok(subst.clone()),
        (Type::Union(ms), _) if ms.is_empty() => Ok(subst.clone()),
        (_, Type::Union(ms)) if ms.is_empty() => Ok(subst.clone()),
        // Subtype widening: Int <: Float (never Float <: Int).
        (Type::Int, Type::Float) => Ok(subst.clone()),
        (Type::TypeVar(v1 @ TypeVarId::Meta(_)), _) => {
            if let Type::TypeVar(v2) = &t_sup
                && v1 == v2
            {
                return Ok(subst.clone());
            }
            subst.extend_at(v1.clone(), t_sup.clone(), span)
        }
        (_, Type::TypeVar(v2 @ TypeVarId::Meta(_))) => {
            subst.extend_at(v2.clone(), t_sub.clone(), span)
        }
        (Type::Union(members), _) => {
            let mut s = subst.clone();
            for m in members {
                s = unify_subtype_at(m, &t_sup, &s, span)?;
            }
            Ok(s)
        }
        (_, Type::Union(members)) => {
            if matches!(t_sub, Type::Scalar(_))
                && members.contains(&Type::Int)
                && members.contains(&Type::Float)
            {
                return Ok(subst.clone());
            }
            let mut special_err: Option<TypeError> = None;
            for m in members {
                match unify_subtype_at(&t_sub, m, subst, span) {
                    Ok(s) => return Ok(s),
                    Err(
                        e @ (TypeError::OccursCheck { .. } | TypeError::EffectRowMismatch { .. }),
                    ) => {
                        if special_err.is_none() {
                            special_err = Some(e);
                        }
                    }
                    Err(_) => {}
                }
            }
            Err(special_err.unwrap_or(TypeError::UnificationFailure {
                expected: format_type(&t_sup),
                got: format_type(&t_sub),
                span,
            }))
        }
        (Type::TypeVar(TypeVarId::Rigid(_)), _) | (_, Type::TypeVar(TypeVarId::Rigid(_))) => {
            Err(TypeError::UnificationFailure {
                expected: format_type(&t_sup),
                got: format_type(&t_sub),
                span,
            })
        }
        (Type::ForAll(_, inner), other) => unify_subtype_at(inner, other, subst, span),
        (other, Type::ForAll(_, inner)) => unify_subtype_at(other, inner, subst, span),
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
            // Tensor DType is invariant in memory representation.
            let mut next = unify_at(d1, d2, subst, span)?;
            unify_shape_at(s1, s2, &mut next, &t_sup, &t_sub, span)?;
            unify_device_at(dev1, dev2, &mut next, &t_sup, &t_sub, span)?;
            Ok(next)
        }
        (Type::List(a), Type::List(b)) => unify_subtype_at(a, b, subst, span),
        // Channel<T> is INVARIANT in T because channels both receive and send.
        (Type::Channel(a), Type::Channel(b)) => unify_at(a, b, subst, span),
        // TaskHandle<T> is COVARIANT in T because join() only reads the produced result.
        (Type::TaskHandle(a), Type::TaskHandle(b)) => unify_subtype_at(a, b, subst, span),
        (Type::Dict(k1, v1), Type::Dict(k2, v2)) => {
            let s1 = unify_subtype_at(k1, k2, subst, span)?;
            unify_subtype_at(v1, v2, &s1, span)
        }
        (Type::Result_(ok1, err1), Type::Result_(ok2, err2)) => {
            let s1 = unify_subtype_at(ok1, ok2, subst, span)?;
            unify_subtype_at(err1, err2, &s1, span)
        }
        (Type::Cons(h1, t1_rest), Type::Cons(h2, t2_rest)) => {
            let s1 = unify_subtype_at(h1, h2, subst, span)?;
            unify_subtype_at(t1_rest, t2_rest, &s1, span)
        }
        (Type::Tuple(a), Type::Tuple(b)) if a.len() == b.len() => {
            let mut s = subst.clone();
            for (ta, tb) in a.iter().zip(b.iter()) {
                s = unify_subtype_at(ta, tb, &s, span)?;
            }
            Ok(s)
        }
        (
            Type::Concrete {
                name: a,
                generics: ga,
                ..
            },
            Type::Concrete {
                name: b,
                generics: gb,
                ..
            },
        ) if a == b && ga.len() == gb.len() => {
            let mut s = subst.clone();
            for (xa, xb) in ga.iter().zip(gb.iter()) {
                s = unify_subtype_at(xa, xb, &s, span)?;
            }
            Ok(s)
        }
        (Type::Concrete { traits, .. }, Type::Trait { .. }) => {
            for t in traits {
                if let Ok(s) = unify_subtype_at(t, &t_sup, subst, span) {
                    return Ok(s);
                }
            }
            Err(TypeError::UnificationFailure {
                expected: format_type(&t_sup),
                got: format_type(&t_sub),
                span,
            })
        }
        (
            Type::TraitObject {
                trait_name: tn1,
                generics: g1,
            },
            Type::Trait {
                name: n2,
                generics: g2,
            },
        ) if tn1 == n2 && g1.len() == g2.len() => {
            let mut s = subst.clone();
            for (a, b) in g1.iter().zip(g2.iter()) {
                s = unify_subtype_at(a, b, &s, span)?;
            }
            Ok(s)
        }
        (
            Type::Trait {
                name: a,
                generics: ga,
            },
            Type::Trait {
                name: b,
                generics: gb,
            },
        ) if a == b && ga.len() == gb.len() => {
            let mut s = subst.clone();
            for (xa, xb) in ga.iter().zip(gb.iter()) {
                s = unify_subtype_at(xa, xb, &s, span)?;
            }
            Ok(s)
        }
        (
            Type::TraitObject {
                trait_name: tn1,
                generics: g1,
            },
            Type::TraitObject {
                trait_name: tn2,
                generics: g2,
            },
        ) if (tn1.is_empty() || tn2.is_empty() || tn1 == tn2) && g1.len() == g2.len() => {
            let mut s = subst.clone();
            for (a, b) in g1.iter().zip(g2.iter()) {
                s = unify_subtype_at(a, b, &s, span)?;
            }
            Ok(s)
        }
        (Type::Arrow(i1, o1, r1), Type::Arrow(i2, o2, r2)) => {
            // Contravariant arg, covariant return, subsumed effect row.
            let s1 = unify_subtype_at(i2, i1, subst, span)?;
            let s2 = unify_subtype_at(o1, o2, &s1, span)?;
            let r1_a = crate::typeck::hm::row::apply_row(r1, &s2);
            let r2_a = crate::typeck::hm::row::apply_row(r2, &s2);
            if matches!(
                (&r1_a, &r2_a),
                (_, crate::mir::effect::EffectRow::Var(_))
                    | (crate::mir::effect::EffectRow::Var(_), _)
            ) {
                let mut s3 = s2;
                crate::typeck::hm::row::unify_row_at(&r1_a, &r2_a, &mut s3, span)?;
                return Ok(s3);
            }
            if !crate::typeck::effect_row_subsumes(&r1_a, &r2_a) {
                return Err(TypeError::EffectRowMismatch {
                    expected: r2_a.to_string(),
                    got: r1_a.to_string(),
                    span,
                });
            }
            Ok(s2)
        }
        _ if t_sub == t_sup => Ok(subst.clone()),
        _ => Err(TypeError::UnificationFailure {
            expected: format_type(&t_sup),
            got: format_type(&t_sub),
            span,
        }),
    }
}

/// Transitive occurs check following substitution chains.
fn occurs_in_subst(s: &Substitution, var: &TypeVarId, ty: &Type) -> bool {
    match s.apply(ty) {
        Type::TypeVar(id) => {
            if &id == var {
                true
            } else {
                s.mapping
                    .get(&id)
                    .is_some_and(|next| occurs_in_subst(s, var, next))
            }
        }
        other => contains_typevar_resolved(s, var, &other),
    }
}

fn contains_typevar_resolved(s: &Substitution, var: &TypeVarId, ty: &Type) -> bool {
    match ty {
        Type::List(elem) | Type::Channel(elem) | Type::TaskHandle(elem) => {
            occurs_in_subst(s, var, elem)
        }
        Type::Tensor { dtype, .. } => occurs_in_subst(s, var, dtype),
        Type::ForAll(bound, elem) => {
            if bound.contains(var) {
                false
            } else {
                occurs_in_subst(s, var, elem)
            }
        }
        Type::Dict(k, v) => occurs_in_subst(s, var, k) || occurs_in_subst(s, var, v),
        Type::Result_(ok, err) => occurs_in_subst(s, var, ok) || occurs_in_subst(s, var, err),
        Type::Union(members) => members.iter().any(|m| occurs_in_subst(s, var, m)),
        Type::Arrow(i, o, _) => occurs_in_subst(s, var, i) || occurs_in_subst(s, var, o),
        Type::Tuple(elems) => elems.iter().any(|e| occurs_in_subst(s, var, e)),
        Type::Cons(h, t) => occurs_in_subst(s, var, h) || occurs_in_subst(s, var, t),
        Type::Trait { generics, .. }
        | Type::Concrete { generics, .. }
        | Type::TraitObject { generics, .. } => generics.iter().any(|g| occurs_in_subst(s, var, g)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typeck::Type;

    fn tv(id: u32) -> Type {
        Type::TypeVar(TypeVarId::Meta(id))
    }

    fn tvid(id: u32) -> TypeVarId {
        TypeVarId::Meta(id)
    }

    fn rigid(name: &str) -> Type {
        Type::TypeVar(TypeVarId::Rigid(name.to_string()))
    }

    #[test]
    fn test_unify_same_types() {
        let subst = Substitution::new();
        assert!(unify(&Type::Int, &Type::Int, &subst).is_ok());
    }

    #[test]
    fn test_unify_type_var() {
        let subst = Substitution::new();
        let result = unify(&tv(0), &Type::Int, &subst).unwrap();
        assert_eq!(result.mapping.get(&tvid(0)), Some(&Type::Int));
    }

    #[test]
    fn test_unify_list() {
        let subst = Substitution::new();
        let t1 = Type::List(Box::new(tv(0)));
        let t2 = Type::List(Box::new(Type::Int));
        let result = unify(&t1, &t2, &subst).unwrap();
        assert_eq!(result.mapping.get(&tvid(0)), Some(&Type::Int));
    }

    #[test]
    fn test_occurs_check() {
        let subst = Substitution::new();
        let t2 = Type::List(Box::new(tv(0)));
        let result = unify(&tv(0), &t2, &subst);
        assert!(matches!(result, Err(TypeError::OccursCheck { .. })));
    }

    #[test]
    fn unify_concrete_types_are_reflexive() {
        let subst = Substitution::new();
        for (name, ty) in [
            ("Int", Type::Int),
            ("String", Type::String),
            ("Char", Type::Char),
            ("RandomModule", Type::RandomModule),
            ("BuiltinModule", Type::BuiltinModule("json".to_string())),
            (
                "Concrete",
                Type::Concrete {
                    name: "Foo".to_string(),
                    generics: vec![],
                    traits: vec![],
                },
            ),
        ] {
            let r = unify(&ty, &ty, &subst);
            assert!(r.is_ok(), "unify({name}, {name}) must succeed");
        }
    }

    #[test]
    fn unify_unknown_with_itself_and_any_fails() {
        let subst = Substitution::new();
        assert!(unify(&Type::Unknown, &Type::Unknown, &subst).is_err());
        assert!(unify(&Type::Unknown, &Type::Any, &subst).is_err());
        assert!(unify(&tv(0), &Type::Unknown, &subst).is_err());
        assert!(!Type::Unknown.compatible_with(&Type::Unknown));
        assert!(!Type::Unknown.compatible_with(&Type::Any));
        assert!(!Type::Unknown.subtype_of(&Type::Any));
    }

    #[test]
    fn unify_error_sentinel_recovers_without_cascading() {
        let subst = Substitution::new();
        assert!(unify(&Type::Error, &Type::Int, &subst).is_ok());
        assert!(unify(&Type::String, &Type::Error, &subst).is_ok());
        assert!(Type::Error.compatible_with(&Type::Int));
        assert!(Type::Error.subtype_of(&Type::String));
    }

    #[test]
    fn unify_rigid_vars_enforce_parametricity() {
        let subst = Substitution::new();
        assert!(unify(&rigid("T"), &rigid("T"), &subst).is_ok());
        assert!(unify(&rigid("T"), &rigid("U"), &subst).is_err());
        assert!(unify(&rigid("T"), &Type::Int, &subst).is_err());
        // A meta var may unify with a rigid var, not the inverse:
        let res = unify(&tv(0), &rigid("T"), &subst).unwrap();
        assert_eq!(res.mapping.get(&tvid(0)), Some(&rigid("T")));
    }

    #[test]
    fn unify_same_typevar_is_reflexive() {
        let subst = Substitution::new();
        let result = unify(&tv(0), &tv(0), &subst).unwrap();
        assert!(result.mapping.is_empty());
    }

    #[test]
    fn unify_same_typevar_after_resolution_is_reflexive() {
        let subst = Substitution::new().extend(tvid(0), tv(1)).expect("bind");
        assert!(unify(&tv(0), &tv(1), &subst).is_ok());
    }

    #[test]
    fn occurs_check_follows_substitution_chain() {
        let subst = Substitution::new().extend(tvid(0), tv(1)).expect("bind");
        let rhs = Type::List(Box::new(tv(0)));
        let result = unify(&tv(1), &rhs, &subst);
        assert!(matches!(result, Err(TypeError::OccursCheck { .. })));
    }

    #[test]
    fn occurs_check_covers_tuple_and_cons() {
        let subst = Substitution::new();
        let tup = Type::Tuple(vec![Box::new(tv(0))]);
        assert!(matches!(
            unify(&tv(0), &tup, &subst),
            Err(TypeError::OccursCheck { .. })
        ));
        let cons = Type::Cons(Box::new(tv(0)), Box::new(Type::Nil));
        assert!(matches!(
            unify(&tv(0), &cons, &subst),
            Err(TypeError::OccursCheck { .. })
        ));
    }

    fn trait_ty(name: &str, generics: Vec<Type>) -> Type {
        Type::Trait {
            name: name.to_string(),
            generics,
        }
    }

    fn concrete(name: &str, generics: Vec<Type>, traits: Vec<Type>) -> Type {
        Type::Concrete {
            name: name.to_string(),
            generics,
            traits,
        }
    }

    #[test]
    fn subtype_int_widens_to_float_and_not_reverse() {
        assert!(Type::Int.subtype_of(&Type::Float));
        assert!(!Type::Float.subtype_of(&Type::Int));
        let subst = Substitution::new();
        let span = Span::new(4, 12);
        let ok = SpannedConstraint::new(
            Constraint::Subtype {
                sub: Box::new(Type::Int),
                sup: Box::new(Type::Float),
            },
            span,
        );
        assert!(solve_spanned(&ok, &subst).is_ok());
        let bad = SpannedConstraint::new(
            Constraint::Subtype {
                sub: Box::new(Type::Float),
                sup: Box::new(Type::Int),
            },
            span,
        );
        let err = solve_spanned(&bad, &subst).unwrap_err();
        match err {
            TypeError::UnificationFailure { span: s, .. } => assert_eq!(s, Some(span)),
            other => panic!("expected UnificationFailure with span, got {other:?}"),
        }
    }

    #[test]
    fn subtype_result_is_covariant_in_both_type_args() {
        let r_int = Type::Result_(Box::new(Type::Int), Box::new(Type::Int));
        let r_float = Type::Result_(Box::new(Type::Float), Box::new(Type::Float));
        assert!(r_int.subtype_of(&r_float));
        assert!(!r_float.subtype_of(&r_int));
    }

    #[test]
    fn subtype_arrow_uses_effect_row_subsumption() {
        let pure_fn = Type::Arrow(
            Box::new(Type::Int),
            Box::new(Type::Int),
            crate::mir::effect::EffectRow::Empty,
        );
        let ai_fn = Type::Arrow(
            Box::new(Type::Int),
            Box::new(Type::Int),
            crate::mir::effect::EffectRow::Cons(
                "Ai".to_string(),
                Box::new(crate::mir::effect::EffectRow::Empty),
            ),
        );
        assert!(pure_fn.subtype_of(&ai_fn));
        assert!(!ai_fn.subtype_of(&pure_fn));
    }

    #[test]
    fn subtype_concrete_and_trait_object_and_containers() {
        let comparable = trait_ty("Comparable", vec![Type::Int]);
        let int_val = concrete("MyInt", vec![Type::Int], vec![comparable.clone()]);
        assert!(int_val.subtype_of(&comparable));
        assert!(!comparable.subtype_of(&int_val));

        let obj_match = Type::TraitObject {
            trait_name: "Comparable".to_string(),
            generics: vec![Type::Int],
        };
        assert!(obj_match.subtype_of(&comparable));
        assert!(!comparable.subtype_of(&obj_match));
    }

    #[test]
    fn unify_arrow_binds_type_vars() {
        let subst = Substitution::new();
        let a = Type::Arrow(
            Box::new(tv(0)),
            Box::new(tv(1)),
            crate::mir::effect::EffectRow::Empty,
        );
        let b = Type::Arrow(
            Box::new(Type::Int),
            Box::new(Type::String),
            crate::mir::effect::EffectRow::Empty,
        );
        let result = unify(&a, &b, &subst).unwrap();
        assert_eq!(result.mapping.get(&tvid(0)), Some(&Type::Int));
        assert_eq!(result.mapping.get(&tvid(1)), Some(&Type::String));
    }

    #[test]
    fn unify_arrow_effect_row_propagates_span() {
        let subst = Substitution::new();
        let span = Span::new(9, 5);
        let row_a = crate::mir::effect::EffectRow::Cons(
            "Ai".to_string(),
            Box::new(crate::mir::effect::EffectRow::Empty),
        );
        let row_b = crate::mir::effect::EffectRow::Cons(
            "Fs".to_string(),
            Box::new(crate::mir::effect::EffectRow::Empty),
        );
        let sc = SpannedConstraint::new(
            Constraint::Eq(
                Box::new(Type::Arrow(Box::new(Type::Int), Box::new(Type::Int), row_a)),
                Box::new(Type::Arrow(Box::new(Type::Int), Box::new(Type::Int), row_b)),
            ),
            span,
        );
        let err = solve_spanned(&sc, &subst).unwrap_err();
        match err {
            TypeError::EffectRowMismatch { span: s, .. } => assert_eq!(s, Some(span)),
            other => panic!("expected EffectRowMismatch, got {other:?}"),
        }
    }
}
