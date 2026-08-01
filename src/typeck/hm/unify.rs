//! v0.53: Unification Algorithm for HM Type Inference
//!
//! Implements the unification procedure to solve type constraints.
//! Core of HM inference: finds most general unifier (MGU) for two types.

use std::collections::HashMap;

use super::error::TypeError;

///  A substitution maps type variables to concrete types
#[derive(Debug, Clone)]
pub struct Substitution {
    mapping: HashMap<char, crate::typeck::Type>,
}

impl Substitution {
    /// Create a new empty substitution
    pub fn new() -> Self {
        Self {
            mapping: HashMap::new(),
        }
    }

    /// Apply substitution to a type (replace all type variables)
    pub fn apply(&self, ty: &crate::typeck::Type) -> crate::typeck::Type {
        match ty {
            crate::typeck::Type::TypeVar(c) => {
                self.mapping.get(c).cloned().unwrap_or_else(|| ty.clone())
            }
            crate::typeck::Type::List(elem) => {
                crate::typeck::Type::List(Box::new(self.apply(elem)))
            }
            crate::typeck::Type::Dict(key, value) => {
                crate::typeck::Type::Dict(Box::new(self.apply(key)), Box::new(self.apply(value)))
            }
            crate::typeck::Type::Result_(ok, err) => {
                crate::typeck::Type::Result_(Box::new(self.apply(ok)), Box::new(self.apply(err)))
            }
            crate::typeck::Type::Union(members) => {
                crate::typeck::Type::Union(members.iter().map(|m| self.apply(m)).collect())
            }
            // v0.75.17: ForAll 内层应用替换（量化变量自身不会被替换 — 它们
            // 与活跃 TypeVar 命名空间隔离；替换只作用于内层 τ 中的自由变量）。
            crate::typeck::Type::ForAll(vars, inner) => {
                crate::typeck::Type::ForAll(vars.clone(), Box::new(self.apply(inner)))
            }
            _ => ty.clone(),
        }
    }

    /// Extend substitution with new mapping
    pub fn extend(&self, var: char, ty: crate::typeck::Type) -> Result<Self, TypeError> {
        // Occurs check: ty cannot contain var
        if contains_typevar(&ty, var) {
            return Err(TypeError::OccursCheck {
                var,
                with_ty: format!("{:?}", ty),
                span: None,
            });
        }

        let mut new_mapping = self.mapping.clone();
        new_mapping.insert(var, ty);
        Ok(Self {
            mapping: new_mapping,
        })
    }

    /// Compose this substitution with another
    pub fn compose(&self, other: &Self) -> Self {
        let mut new_mapping = self.mapping.clone();

        for (var, ty) in &other.mapping {
            let applied_ty = self.apply(ty);
            new_mapping.insert(*var, applied_ty);
        }

        Self {
            mapping: new_mapping,
        }
    }
}

impl Default for Substitution {
    fn default() -> Self {
        Self::new()
    }
}

///  Type constraint that needs to be solved
#[derive(Debug, Clone)]
pub enum Constraint {
    /// Two types must be equal
    Eq(Box<crate::typeck::Type>, Box<crate::typeck::Type>),

    /// Both types must be numeric (Int or Float) - for arithmetic operations
    Numeric(BinaryConstraint),
}

///  Arithmetic binary operator constraint: both operands must be compatible numeric types
#[derive(Debug, Clone)]
pub struct BinaryConstraint {
    pub left: Box<crate::typeck::Type>,
    pub right: Box<crate::typeck::Type>,
}

impl std::fmt::Display for Constraint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Constraint::Eq(t1, t2) => write!(f, "({} == {})", format_type(t1), format_type(t2)),
            Constraint::Numeric(bc) => write!(
                f,
                "(+ {:?}, {:?})",
                format_type(&bc.left),
                format_type(&bc.right)
            ),
        }
    }
}

///  Format type with proper display
fn format_type(ty: &crate::typeck::Type) -> String {
    ty.name()
}

///  Solve a single constraint, returning updated substitution
pub fn solve(constraint: &Constraint, subst: &Substitution) -> Result<Substitution, TypeError> {
    match constraint {
        Constraint::Eq(ty1, ty2) => unify(ty1, ty2, subst),
        Constraint::Numeric(_) => {
            // For now, treat numeric constraints as satisfied if both types resolve to Int or Float
            // TODO: Add proper numeric type checking with subtyping rules
            Ok(subst.clone())
        }
    }
}

///  Main unification algorithm: find MGU of two types under current substitution
fn unify(
    ty1: &crate::typeck::Type,
    ty2: &crate::typeck::Type,
    subst: &Substitution,
) -> Result<Substitution, TypeError> {
    let t1 = subst.apply(ty1);
    let t2 = subst.apply(ty2);

    match (&t1, &t2) {
        // Variable cases
        (crate::typeck::Type::TypeVar(v1), _) => {
            if contains_typevar(&t2, *v1) {
                Err(TypeError::OccursCheck {
                    var: *v1,
                    with_ty: format!("{:?}", t2),
                    span: None,
                })
            } else {
                subst.extend(*v1, t2.clone())
            }
        }
        (_, crate::typeck::Type::TypeVar(v2)) => {
            if contains_typevar(&t1, *v2) {
                Err(TypeError::OccursCheck {
                    var: *v2,
                    with_ty: format!("{:?}", t1),
                    span: None,
                })
            } else {
                subst.extend(*v2, t1.clone())
            }
        }

        // Same type constructor
        (crate::typeck::Type::Int, crate::typeck::Type::Int) => Ok(subst.clone()),
        (crate::typeck::Type::Float, crate::typeck::Type::Float) => Ok(subst.clone()),
        (crate::typeck::Type::String, crate::typeck::Type::String) => Ok(subst.clone()),
        (crate::typeck::Type::Bool, crate::typeck::Type::Bool) => Ok(subst.clone()),
        (crate::typeck::Type::Nil, crate::typeck::Type::Nil) => Ok(subst.clone()),
        (crate::typeck::Type::Any, crate::typeck::Type::Any) => Ok(subst.clone()),

        // v0.75.16: Any 是 top type — 与任何类型合一成功（"未知类型"语义）。
        // 修复：`ys[0]` 降成 `Call(Name("ys_index"))`（receiver_operation 糖），
        // typeck 当未知调用时把 arg 约束到 callee_ty=Any，此前无 (X, Any) arm 报错。
        (crate::typeck::Type::Any, _) | (_, crate::typeck::Type::Any) => Ok(subst.clone()),

        // v0.75.17: ForAll — 防御性合一（正常路径在 env.get 已实例化，
        // 泛型值不会以 ForAll 形态进入约束；若进入，与内层合一）。
        (crate::typeck::Type::ForAll(_, inner), other)
        | (other, crate::typeck::Type::ForAll(_, inner)) => unify(inner, other, subst),

        // Compound types
        (crate::typeck::Type::List(elem1), crate::typeck::Type::List(elem2)) => {
            unify(elem1, elem2, subst)
        }
        (crate::typeck::Type::Dict(k1, v1), crate::typeck::Type::Dict(k2, v2)) => {
            let s1 = unify(k1, k2, subst)?;
            unify(v1, v2, &s1)
        }
        (crate::typeck::Type::Result_(ok1, err1), crate::typeck::Type::Result_(ok2, err2)) => {
            let s1 = unify(ok1, ok2, subst)?;
            unify(err1, err2, &s1)
        }

        // v0.75.16: Union 合一 — 任一成员与另一侧匹配即通过（dict.get 返回
        // Union<V, Nil>；`d.get("k") == x` 需允许 x 与 V 或 Nil 之一合一）。
        // 防膨胀：成员为空或含 Any 时直接视为 Any（退化成功）。
        (crate::typeck::Type::Union(members), other)
        | (other, crate::typeck::Type::Union(members)) => {
            if members.is_empty()
                || members
                    .iter()
                    .any(|m| matches!(m, crate::typeck::Type::Any))
            {
                Ok(subst.clone())
            } else {
                // 任一成员成功合一即可；全部失败则报第一个失败
                let mut first_err: Option<TypeError> = None;
                for m in members {
                    match unify(m, other, subst) {
                        Ok(s) => return Ok(s),
                        Err(e) => {
                            if first_err.is_none() {
                                first_err = Some(e);
                            }
                        }
                    }
                }
                Err(first_err.unwrap_or(TypeError::UnificationFailure {
                    expected: format_type(&t1),
                    got: format_type(&t2),
                    span: None,
                }))
            }
        }

        // Mismatch
        _ => Err(TypeError::UnificationFailure {
            expected: format_type(&t1),
            got: format_type(&t2),
            span: None,
        }),
    }
}

///  Check if a type contains a specific type variable
fn contains_typevar(ty: &crate::typeck::Type, var: char) -> bool {
    match ty {
        crate::typeck::Type::TypeVar(v) => *v == var,
        crate::typeck::Type::List(elem) => contains_typevar(elem, var),
        crate::typeck::Type::Dict(k, v) => contains_typevar(k, var) || contains_typevar(v, var),
        crate::typeck::Type::Result_(ok, err) => {
            contains_typevar(ok, var) || contains_typevar(err, var)
        }
        crate::typeck::Type::Union(members) => members.iter().any(|m| contains_typevar(m, var)),
        // v0.75.17: ForAll 内层递归（量化变量与活跃 TypeVar 命名空间隔离，
        // 递归可查内层嵌套的自由变量）。
        crate::typeck::Type::ForAll(_, inner) => contains_typevar(inner, var),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typeck::Type;

    #[test]
    fn test_unify_same_types() {
        let subst = Substitution::new();
        let result = unify(&Type::Int, &Type::Int, &subst);
        assert!(result.is_ok());
    }

    #[test]
    fn test_unify_type_var() {
        let subst = Substitution::new();
        let t1 = Type::TypeVar('a');
        let t2 = Type::Int;

        let result = unify(&t1, &t2, &subst).unwrap();
        assert_eq!(result.mapping.get(&'a'), Some(&Type::Int));
    }

    #[test]
    fn test_unify_list() {
        let subst = Substitution::new();
        let t1 = Type::List(Box::new(Type::TypeVar('a')));
        let t2 = Type::List(Box::new(Type::Int));

        let result = unify(&t1, &t2, &subst).unwrap();
        assert_eq!(result.mapping.get(&'a'), Some(&Type::Int));
    }

    #[test]
    fn test_occurs_check() {
        let subst = Substitution::new();
        let t1 = Type::TypeVar('a');
        let t2 = Type::List(Box::new(Type::TypeVar('a')));

        let result = unify(&t1, &t2, &subst);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), TypeError::OccursCheck { .. }));
    }
}
