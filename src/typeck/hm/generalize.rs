//! v0.110.0: Let-Generalization for HM Type Inference
//!
//! Implements the generalization rule for let bindings:
//! Γ ⊢ e : τ, FV(Γ) = {α₁, ..., αₙ}
//!
//! Γ ⊢ let x = e in body : ∀α₁...αₙ.τ
//!
//! 量化结果用 `Type::ForAll(vars, inner)` 表示，`instantiate` 把它展开为
//! fresh TypeVar（标准 HM 的 let-polymorphism 展开规则）。

use std::collections::{HashMap, HashSet};

use super::env::collect_type_vars;
use crate::typeck::{Type, TypeVarId};

/// Generalize a type by quantifying free meta-variables not free in `free_in_env`.
/// Explicit rigid generic variables remain untouched unless wrapped explicitly
/// at `FnDef` / `Closure` boundaries.
pub fn generalize(ty: &Type, free_in_env: &[TypeVarId]) -> Type {
    let env_set: HashSet<&TypeVarId> = free_in_env.iter().collect();
    let mut to_quantify: Vec<TypeVarId> = collect_free_vars(ty)
        .into_iter()
        .filter(|v| matches!(v, TypeVarId::Meta(_)) && !env_set.contains(v))
        .collect();
    to_quantify.sort();
    to_quantify.dedup();

    if to_quantify.is_empty() {
        ty.clone()
    } else {
        Type::ForAll(to_quantify, Box::new(ty.clone()))
    }
}

/// Instantiate `∀α₁...αₙ. τ` into fresh type variables (preserving identity per variable).
pub fn instantiate(ty: &Type, fresh_id: &mut impl FnMut() -> TypeVarId) -> Type {
    instantiate_with_mapping(ty, fresh_id).0
}

/// Instantiate `∀α₁...αₙ. τ` and return the substitution map used for bound variables.
pub fn instantiate_with_mapping(
    ty: &Type,
    fresh_id: &mut impl FnMut() -> TypeVarId,
) -> (Type, HashMap<TypeVarId, TypeVarId>) {
    match ty {
        Type::ForAll(vars, inner) => {
            let mapping: HashMap<TypeVarId, TypeVarId> =
                vars.iter().map(|v| (v.clone(), fresh_id())).collect();
            let instantiated = instantiate_ty(inner, &mapping, fresh_id);
            (instantiated, mapping)
        }
        _ => (ty.clone(), HashMap::new()),
    }
}

/// Apply an arbitrary `TypeVarId -> TypeVarId` substitution across a type tree.
pub(crate) fn remap_type_vars(ty: &Type, mapping: &HashMap<TypeVarId, TypeVarId>) -> Type {
    if mapping.is_empty() {
        return ty.clone();
    }
    match ty {
        Type::TypeVar(id) => mapping
            .get(id)
            .cloned()
            .map(Type::TypeVar)
            .unwrap_or_else(|| ty.clone()),
        Type::List(elem) => Type::List(Box::new(remap_type_vars(elem, mapping))),
        Type::Channel(elem) => Type::Channel(Box::new(remap_type_vars(elem, mapping))),
        Type::TaskHandle(elem) => Type::TaskHandle(Box::new(remap_type_vars(elem, mapping))),
        Type::Dict(k, v) => Type::Dict(
            Box::new(remap_type_vars(k, mapping)),
            Box::new(remap_type_vars(v, mapping)),
        ),
        Type::Result_(ok, err) => Type::Result_(
            Box::new(remap_type_vars(ok, mapping)),
            Box::new(remap_type_vars(err, mapping)),
        ),
        Type::Tuple(elems) => Type::Tuple(
            elems
                .iter()
                .map(|e| Box::new(remap_type_vars(e, mapping)))
                .collect(),
        ),
        Type::Union(members) => Type::Union(
            members
                .iter()
                .map(|m| remap_type_vars(m, mapping))
                .collect(),
        ),
        Type::Cons(h, t) => Type::Cons(
            Box::new(remap_type_vars(h, mapping)),
            Box::new(remap_type_vars(t, mapping)),
        ),
        Type::Arrow(input, output, row) => Type::Arrow(
            Box::new(remap_type_vars(input, mapping)),
            Box::new(remap_type_vars(output, mapping)),
            row.clone(),
        ),
        Type::Trait { name, generics } => Type::Trait {
            name: name.clone(),
            generics: generics
                .iter()
                .map(|g| remap_type_vars(g, mapping))
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
                .map(|g| remap_type_vars(g, mapping))
                .collect(),
            traits: traits.iter().map(|t| remap_type_vars(t, mapping)).collect(),
        },
        Type::TraitObject {
            trait_name,
            generics,
        } => Type::TraitObject {
            trait_name: trait_name.clone(),
            generics: generics
                .iter()
                .map(|g| remap_type_vars(g, mapping))
                .collect(),
        },
        Type::ForAll(bound, inner) => {
            let shadowed: HashSet<&TypeVarId> = bound.iter().collect();
            let filtered: HashMap<TypeVarId, TypeVarId> = mapping
                .iter()
                .filter(|(k, _)| !shadowed.contains(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            Type::ForAll(bound.clone(), Box::new(remap_type_vars(inner, &filtered)))
        }
        Type::Tensor {
            dtype,
            shape,
            device,
        } => {
            let new_dtype = Box::new(remap_type_vars(dtype, mapping));
            let new_shape = match shape {
                crate::typeck::ShapeType::Unranked(Some(v)) => crate::typeck::ShapeType::Unranked(
                    Some(mapping.get(v).cloned().unwrap_or_else(|| v.clone())),
                ),
                crate::typeck::ShapeType::Unranked(None) => {
                    crate::typeck::ShapeType::Unranked(None)
                }
                crate::typeck::ShapeType::Ranked(dims) => crate::typeck::ShapeType::Ranked(
                    dims.iter()
                        .map(|d| match d {
                            crate::typeck::DimExpr::Known(n) => crate::typeck::DimExpr::Known(*n),
                            crate::typeck::DimExpr::Var(id) => {
                                match mapping.get(&TypeVarId::Meta(*id)) {
                                    Some(TypeVarId::Meta(nid)) => crate::typeck::DimExpr::Var(*nid),
                                    Some(TypeVarId::Rigid(sym)) => {
                                        crate::typeck::DimExpr::Symbolic(sym.clone())
                                    }
                                    None => crate::typeck::DimExpr::Var(*id),
                                }
                            }
                            crate::typeck::DimExpr::Symbolic(sym) => {
                                match mapping.get(&TypeVarId::Rigid(sym.clone())) {
                                    Some(TypeVarId::Meta(nid)) => crate::typeck::DimExpr::Var(*nid),
                                    Some(TypeVarId::Rigid(nsym)) => {
                                        crate::typeck::DimExpr::Symbolic(nsym.clone())
                                    }
                                    None => crate::typeck::DimExpr::Symbolic(sym.clone()),
                                }
                            }
                        })
                        .collect(),
                ),
            };
            let new_device = match device {
                crate::typeck::DeviceType::Var(v) => crate::typeck::DeviceType::Var(
                    mapping.get(v).cloned().unwrap_or_else(|| v.clone()),
                ),
                other => other.clone(),
            };
            Type::Tensor {
                dtype: new_dtype,
                shape: new_shape,
                device: new_device,
            }
        }
        _ => ty.clone(),
    }
}

fn instantiate_ty(
    ty: &Type,
    mapping: &HashMap<TypeVarId, TypeVarId>,
    fresh_id: &mut impl FnMut() -> TypeVarId,
) -> Type {
    match ty {
        Type::TypeVar(id) => mapping
            .get(id)
            .cloned()
            .map(Type::TypeVar)
            .unwrap_or_else(|| ty.clone()),
        Type::List(elem) => Type::List(Box::new(instantiate_ty(elem, mapping, fresh_id))),
        Type::Channel(elem) => Type::Channel(Box::new(instantiate_ty(elem, mapping, fresh_id))),
        Type::TaskHandle(elem) => {
            Type::TaskHandle(Box::new(instantiate_ty(elem, mapping, fresh_id)))
        }
        Type::Dict(k, v) => Type::Dict(
            Box::new(instantiate_ty(k, mapping, fresh_id)),
            Box::new(instantiate_ty(v, mapping, fresh_id)),
        ),
        Type::Result_(ok, err) => Type::Result_(
            Box::new(instantiate_ty(ok, mapping, fresh_id)),
            Box::new(instantiate_ty(err, mapping, fresh_id)),
        ),
        Type::Tuple(elems) => Type::Tuple(
            elems
                .iter()
                .map(|e| Box::new(instantiate_ty(e, mapping, fresh_id)))
                .collect(),
        ),
        Type::Union(members) => Type::Union(
            members
                .iter()
                .map(|m| instantiate_ty(m, mapping, fresh_id))
                .collect(),
        ),
        Type::Cons(h, t) => Type::Cons(
            Box::new(instantiate_ty(h, mapping, fresh_id)),
            Box::new(instantiate_ty(t, mapping, fresh_id)),
        ),
        Type::Arrow(input, output, row) => Type::Arrow(
            Box::new(instantiate_ty(input, mapping, fresh_id)),
            Box::new(instantiate_ty(output, mapping, fresh_id)),
            row.clone(),
        ),
        Type::Trait { name, generics } => Type::Trait {
            name: name.clone(),
            generics: generics
                .iter()
                .map(|g| instantiate_ty(g, mapping, fresh_id))
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
                .map(|g| instantiate_ty(g, mapping, fresh_id))
                .collect(),
            traits: traits
                .iter()
                .map(|t| instantiate_ty(t, mapping, fresh_id))
                .collect(),
        },
        Type::TraitObject {
            trait_name,
            generics,
        } => Type::TraitObject {
            trait_name: trait_name.clone(),
            generics: generics
                .iter()
                .map(|g| instantiate_ty(g, mapping, fresh_id))
                .collect(),
        },
        Type::ForAll(inner_vars, inner) => {
            let mut next_mapping = mapping.clone();
            for v in inner_vars {
                next_mapping.insert(v.clone(), fresh_id());
            }
            instantiate_ty(inner, &next_mapping, fresh_id)
        }
        Type::Tensor {
            dtype,
            shape,
            device,
        } => {
            let new_dtype = Box::new(instantiate_ty(dtype, mapping, fresh_id));
            let new_shape = match shape {
                crate::typeck::ShapeType::Unranked(Some(v)) => crate::typeck::ShapeType::Unranked(
                    Some(mapping.get(v).cloned().unwrap_or_else(|| v.clone())),
                ),
                crate::typeck::ShapeType::Unranked(None) => {
                    crate::typeck::ShapeType::Unranked(None)
                }
                crate::typeck::ShapeType::Ranked(dims) => crate::typeck::ShapeType::Ranked(
                    dims.iter()
                        .map(|d| match d {
                            crate::typeck::DimExpr::Known(n) => crate::typeck::DimExpr::Known(*n),
                            crate::typeck::DimExpr::Var(id) => {
                                match mapping.get(&TypeVarId::Meta(*id)) {
                                    Some(TypeVarId::Meta(nid)) => crate::typeck::DimExpr::Var(*nid),
                                    Some(TypeVarId::Rigid(sym)) => {
                                        crate::typeck::DimExpr::Symbolic(sym.clone())
                                    }
                                    None => crate::typeck::DimExpr::Var(*id),
                                }
                            }
                            crate::typeck::DimExpr::Symbolic(sym) => {
                                if sym == "_" {
                                    match fresh_id() {
                                        TypeVarId::Meta(nid) => crate::typeck::DimExpr::Var(nid),
                                        TypeVarId::Rigid(s) => crate::typeck::DimExpr::Symbolic(s),
                                    }
                                } else {
                                    match mapping.get(&TypeVarId::Rigid(sym.clone())) {
                                        Some(TypeVarId::Meta(nid)) => {
                                            crate::typeck::DimExpr::Var(*nid)
                                        }
                                        Some(TypeVarId::Rigid(nsym)) => {
                                            crate::typeck::DimExpr::Symbolic(nsym.clone())
                                        }
                                        None => crate::typeck::DimExpr::Symbolic(sym.clone()),
                                    }
                                }
                            }
                        })
                        .collect(),
                ),
            };
            let new_device = match device {
                crate::typeck::DeviceType::Var(v) => crate::typeck::DeviceType::Var(
                    mapping.get(v).cloned().unwrap_or_else(|| v.clone()),
                ),
                other => other.clone(),
            };
            Type::Tensor {
                dtype: new_dtype,
                shape: new_shape,
                device: new_device,
            }
        }
        _ => ty.clone(),
    }
}

/// Recursively collect all free type variable identifiers from a type.
pub fn collect_free_vars(ty: &Type) -> Vec<TypeVarId> {
    collect_type_vars(ty)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(id: u32) -> TypeVarId {
        TypeVarId::Meta(id)
    }

    fn rigid(name: &str) -> TypeVarId {
        TypeVarId::Rigid(name.to_string())
    }

    #[test]
    fn test_collect_free_var_int() {
        assert!(collect_free_vars(&Type::Int).is_empty());
    }

    #[test]
    fn test_collect_free_var_typevar() {
        let vars = collect_free_vars(&Type::TypeVar(meta(0)));
        assert_eq!(vars, vec![meta(0)]);
    }

    #[test]
    fn test_collect_free_var_list() {
        let ty = Type::List(Box::new(Type::TypeVar(meta(0))));
        let vars = collect_free_vars(&ty);
        assert_eq!(vars, vec![meta(0)]);
    }

    #[test]
    fn test_generalize_no_quantification_needed() {
        let empty_env: Vec<TypeVarId> = Vec::new();
        let ty = Type::Int;
        assert_eq!(generalize(&ty, &empty_env), ty);
    }

    #[test]
    fn test_generalize_with_free_var() {
        let empty_env: Vec<TypeVarId> = Vec::new();
        let ty = Type::TypeVar(meta(0));
        let gen_ty = generalize(&ty, &empty_env);
        assert_eq!(
            gen_ty,
            Type::ForAll(vec![meta(0)], Box::new(Type::TypeVar(meta(0))))
        );
    }

    #[test]
    fn test_generalize_skips_env_bound_and_rigid_vars() {
        let ty = Type::Tuple(vec![
            Box::new(Type::TypeVar(meta(0))),
            Box::new(Type::TypeVar(rigid("T"))),
        ]);
        let gen_ty = generalize(&ty, &[meta(0)]);
        assert_eq!(gen_ty, ty);
    }

    #[test]
    fn test_generalize_list() {
        let ty = Type::List(Box::new(Type::TypeVar(meta(0))));
        let gen_ty = generalize(&ty, &[]);
        assert_eq!(
            gen_ty,
            Type::ForAll(
                vec![meta(0)],
                Box::new(Type::List(Box::new(Type::TypeVar(meta(0)))))
            )
        );
    }

    #[test]
    fn test_instantiate_forall_preserves_arrow_identity() {
        let forall = Type::ForAll(
            vec![meta(0)],
            Box::new(Type::Arrow(
                Box::new(Type::TypeVar(meta(0))),
                Box::new(Type::TypeVar(meta(0))),
                crate::mir::effect::EffectRow::Empty,
            )),
        );
        let mut counter = 10u32;
        let mut fresh = || {
            let id = meta(counter);
            counter += 1;
            id
        };
        let inst = instantiate(&forall, &mut fresh);
        assert_eq!(
            inst,
            Type::Arrow(
                Box::new(Type::TypeVar(meta(10))),
                Box::new(Type::TypeVar(meta(10))),
                crate::mir::effect::EffectRow::Empty,
            )
        );
    }

    #[test]
    fn test_instantiate_non_forall_passthrough() {
        let mut counter = 0u32;
        let mut fresh = || {
            let id = meta(counter);
            counter += 1;
            id
        };
        assert_eq!(instantiate(&Type::Int, &mut fresh), Type::Int);
        assert_eq!(
            instantiate(&Type::List(Box::new(Type::Int)), &mut fresh),
            Type::List(Box::new(Type::Int))
        );
    }
}
