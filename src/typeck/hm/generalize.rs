//! v0.53: Let-Generalization for HM Type Inference
//!
//! Implements the generalization rule for let bindings:
//! Γ ⊢ e : τ, FV(Γ) = {α₁, ..., αₙ}
//!
//! Γ ⊢ let x = e in body : ∀α₁...αₙ.τ
//!
//! v0.75.17: 真正的量化实现 — `replace_with_generics` 现在产出
//! `Type::ForAll(vars, inner)`（此前注释 "can't represent ∀ without
//! changing Type enum"，Type 枚举已加 ForAll 变体）。同时新增 `instantiate`
//! 把 ForAll 展开为 fresh TypeVar（标准 HM 的 let-polymorphism 展开规则）。

use std::collections::HashSet;

use crate::typeck::Type;

///  Generalize a type by quantifying over free variables not in env
pub fn generalize(ty: &Type, free_in_env: &[char]) -> Type {
    // Collect all free type variables in ty
    let all_free_vars: HashSet<char> = collect_free_vars(ty).into_iter().collect();

    // Those that are not bound in env should be generalized
    let env_set: HashSet<char> = free_in_env.iter().cloned().collect();
    let to_quantify: HashSet<char> = all_free_vars.difference(&env_set).cloned().collect();

    if to_quantify.is_empty() {
        ty.clone() // Nothing to generalize
    } else {
        // v0.75.17: 替换剩余自由变量为量化参数，产出 ForAll。
        // 被量化的变量名记录在 ForAll(vars, _) 中，内层 τ 的 TypeVar
        // 保留原名（实例化时按 vars 匹配展开）。
        let quantified: Vec<char> = to_quantify.iter().cloned().collect();
        Type::ForAll(quantified, Box::new(rebuild_inner(ty)))
    }
}

/// v0.75.17: 实例化 — `∀α₁...αₙ.τ` 命中 env 时展开为 fresh TypeVar。
/// 标准 HM：每次使用泛型绑定都得到一份新的、可被单形化的副本。
pub fn instantiate(ty: &Type, fresh_id: &mut impl FnMut() -> char) -> Type {
    match ty {
        Type::ForAll(vars, inner) => {
            let quantified: HashSet<char> = vars.iter().cloned().collect();
            instantiate_ty(inner, &quantified, fresh_id)
        }
        _ => ty.clone(),
    }
}

/// 把内层 τ 中被量化的 TypeVar 替换为 fresh TypeVar（未量化的保留）。
fn instantiate_ty(
    ty: &Type,
    quantified: &HashSet<char>,
    fresh_id: &mut impl FnMut() -> char,
) -> Type {
    match ty {
        Type::TypeVar(c) => {
            if quantified.contains(c) {
                Type::TypeVar(fresh_id())
            } else {
                ty.clone()
            }
        }
        Type::List(elem) => Type::List(Box::new(instantiate_ty(elem, quantified, fresh_id))),
        Type::Dict(k, v) => Type::Dict(
            Box::new(instantiate_ty(k, quantified, fresh_id)),
            Box::new(instantiate_ty(v, quantified, fresh_id)),
        ),
        Type::Result_(ok, err) => Type::Result_(
            Box::new(instantiate_ty(ok, quantified, fresh_id)),
            Box::new(instantiate_ty(err, quantified, fresh_id)),
        ),
        Type::Union(members) => Type::Union(
            members
                .iter()
                .map(|m| instantiate_ty(m, quantified, fresh_id))
                .collect(),
        ),
        // v0.75.17: 嵌套 ForAll（外层先被剥掉；防御性处理内层）
        Type::ForAll(inner_vars, inner) => {
            let inner_quantified: HashSet<char> = inner_vars.iter().cloned().collect();
            Type::ForAll(
                inner_vars.clone(),
                Box::new(instantiate_ty(inner, &inner_quantified, fresh_id)),
            )
        }
        _ => ty.clone(),
    }
}

///  Recursively collect all free type variable identifiers from a type
fn collect_free_vars(ty: &Type) -> Vec<char> {
    match ty {
        Type::TypeVar(c) => vec![*c],
        Type::List(elem) => collect_free_vars(elem),
        Type::Dict(key, value) => {
            let mut vars = collect_free_vars(key);
            vars.extend(collect_free_vars(value));
            vars
        }
        Type::Result_(ok, err) => {
            let mut vars = collect_free_vars(ok);
            vars.extend(collect_free_vars(err));
            vars
        }
        // v0.75.17: ForAll 内层仍是自由变量的来源（量化只消除命名变量自身，
        // 内层嵌套的自由变量仍需收集）。
        Type::ForAll(_, inner) => collect_free_vars(inner),
        _ => Vec::new(),
    }
}

///  Rebuild the inner type tree so ForAll's body is a fresh structural copy.
///  TypeVar leaves are kept verbatim (their names are the quantified
///  parameters; `instantiate` expands them per use).
fn rebuild_inner(ty: &Type) -> Type {
    match ty {
        Type::List(elem) => Type::List(Box::new(rebuild_inner(elem))),
        Type::Dict(key, value) => {
            Type::Dict(Box::new(rebuild_inner(key)), Box::new(rebuild_inner(value)))
        }
        Type::Result_(ok, err) => {
            Type::Result_(Box::new(rebuild_inner(ok)), Box::new(rebuild_inner(err)))
        }
        _ => ty.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collect_free_var_int() {
        assert!(collect_free_vars(&Type::Int).is_empty());
    }

    #[test]
    fn test_collect_free_var_typevar() {
        let vars = collect_free_vars(&Type::TypeVar('a'));
        assert_eq!(vars, vec!['a']);
    }

    #[test]
    fn test_collect_free_var_list() {
        let ty = Type::List(Box::new(Type::TypeVar('a')));
        let vars = collect_free_vars(&ty);
        assert_eq!(vars, vec!['a']);
    }

    #[test]
    fn test_generalize_no_quantification_needed() {
        let empty_env: Vec<char> = Vec::new();
        let ty = Type::Int;

        let r#gen = generalize(&ty, &empty_env);
        assert_eq!(r#gen, ty); // Int has no type vars, so unchanged
    }

    #[test]
    fn test_generalize_with_free_var() {
        let empty_env: Vec<char> = Vec::new();
        let ty = Type::TypeVar('a');

        // v0.75.17: 'a' 不在 env 中 → 量化为 ForAll['a]. 'a
        let r#gen = generalize(&ty, &empty_env);
        assert_eq!(r#gen, Type::ForAll(vec!['a'], Box::new(Type::TypeVar('a'))));
    }

    #[test]
    fn test_generalize_skips_env_bound_vars() {
        // env 中已有 'a'（即 'a' 是外层绑定的）→ 不量化
        let ty = Type::TypeVar('a');
        let r#gen = generalize(&ty, &['a']);
        assert_eq!(r#gen, Type::TypeVar('a'));
    }

    #[test]
    fn test_generalize_list() {
        // list<'a> 在空 env 下 → forall<'a>. list<'a>
        let ty = Type::List(Box::new(Type::TypeVar('a')));
        let r#gen = generalize(&ty, &[]);
        assert_eq!(
            r#gen,
            Type::ForAll(
                vec!['a'],
                Box::new(Type::List(Box::new(Type::TypeVar('a'))))
            )
        );
    }

    #[test]
    fn test_instantiate_forall() {
        // ∀'a. 'a 实例化为 TypeVar(fresh)
        let forall = Type::ForAll(vec!['a'], Box::new(Type::TypeVar('a')));
        let mut counter = 0usize;
        let mut fresh = || {
            let id = std::char::from_u32(counter as u32).unwrap();
            counter += 1;
            id
        };
        let inst = instantiate(&forall, &mut fresh);
        assert_eq!(inst, Type::TypeVar('\0'));
    }

    #[test]
    fn test_instantiate_non_forall_passthrough() {
        let mut counter = 0usize;
        let mut fresh = || {
            let id = std::char::from_u32(counter as u32).unwrap();
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
