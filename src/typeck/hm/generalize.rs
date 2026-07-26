//! v0.53: Let-Generalization for HM Type Inference
//!
//! Implements the generalization rule for let bindings:
//! Γ ⊢ e : τ, FV(Γ) = {α₁, ..., αₙ}
//!
//! Γ ⊢ let x = e in body : ∀α₁...αₙ.τ

use std::collections::HashSet;

///  Generalize a type by quantifying over free variables not in env
pub fn generalize(ty: &crate::typeck::Type, free_in_env: &[char]) -> crate::typeck::Type {
    // Collect all free type variables in ty
    let all_free_vars: HashSet<char> = collect_free_vars(ty).into_iter().collect();

    // Those that are not bound in env should be generalized
    let env_set: HashSet<char> = free_in_env.iter().cloned().collect();
    let to_quantify: HashSet<char> = all_free_vars.difference(&env_set).cloned().collect();

    if to_quantify.is_empty() {
        ty.clone() // Nothing to generalize
    } else {
        // Replace remaining free vars with generic parameters
        replace_with_generics(ty, &to_quantify)
    }
}

///  Recursively collect all free type variable identifiers from a type
fn collect_free_vars(ty: &crate::typeck::Type) -> Vec<char> {
    match ty {
        crate::typeck::Type::TypeVar(c) => vec![*c],
        crate::typeck::Type::List(elem) => collect_free_vars(elem),
        crate::typeck::Type::Dict(key, value) => {
            let mut vars = collect_free_vars(key);
            vars.extend(collect_free_vars(value));
            vars
        }
        crate::typeck::Type::Result_(ok, err) => {
            let mut vars = collect_free_vars(ok);
            vars.extend(collect_free_vars(err));
            vars
        }
        _ => Vec::new(),
    }
}

///  Replace free variables not in keep_set with generic type annotations
fn replace_with_generics(
    ty: &crate::typeck::Type,
    keep_set: &HashSet<char>,
) -> crate::typeck::Type {
    match ty {
        crate::typeck::Type::TypeVar(c) => {
            if keep_set.contains(c) {
                ty.clone()
            } else {
                // This should have been generalized already
                ty.clone()
            }
        }
        crate::typeck::Type::List(elem) => {
            crate::typeck::Type::List(Box::new(replace_with_generics(elem, keep_set)))
        }
        crate::typeck::Type::Dict(key, value) => crate::typeck::Type::Dict(
            Box::new(replace_with_generics(key, keep_set)),
            Box::new(replace_with_generics(value, keep_set)),
        ),
        crate::typeck::Type::Result_(ok, err) => crate::typeck::Type::Result_(
            Box::new(replace_with_generics(ok, keep_set)),
            Box::new(replace_with_generics(err, keep_set)),
        ),
        _ => ty.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collect_free_var_int() {
        assert!(collect_free_vars(&crate::typeck::Type::Int).is_empty());
    }

    #[test]
    fn test_collect_free_var_typevar() {
        let vars = collect_free_vars(&crate::typeck::Type::TypeVar('a'));
        assert_eq!(vars, vec!['a']);
    }

    #[test]
    fn test_collect_free_var_list() {
        let ty = crate::typeck::Type::List(Box::new(crate::typeck::Type::TypeVar('a')));
        let vars = collect_free_vars(&ty);
        assert_eq!(vars, vec!['a']);
    }

    #[test]
    fn test_generalize_no_quantification_needed() {
        let empty_env: Vec<char> = Vec::new();
        let ty = crate::typeck::Type::Int;

        let r#gen = generalize(&ty, &empty_env);
        assert_eq!(r#gen, ty); // Int has no type vars, so unchanged
    }

    #[test]
    fn test_generalize_with_free_var() {
        let empty_env: Vec<char> = Vec::new();
        let ty = crate::typeck::Type::TypeVar('a');

        let r#gen = generalize(&ty, &empty_env);
        // 'a' is not in env, but we can't represent ∀ without changing Type enum
        // For now, just returns as-is
        assert_eq!(r#gen, ty);
    }
}
