//! v0.53: Unification Algorithm for HM Type Inference
//!
//! Implements the unification procedure to solve type constraints.
//! Core of HM inference: finds most general unifier (MGU) for two types.

use std::collections::HashMap;

use super::error::TypeError;

///  A substitution maps type variables to concrete types
/// v0.80: 同时映射 row 变量（EffectRow::Var 的 String 名）到具体 row。
#[derive(Debug, Clone)]
pub struct Substitution {
    mapping: HashMap<char, crate::typeck::Type>,
    /// v0.80: row 变量绑定（EffectRow 共享 typeck 命名空间，但用 String 名而非 char。
    /// 原因：EffectRow::Var(String) 是公开类型，char 转换会引起兼容成本）。
    row_mapping: HashMap<String, crate::mir::effect::EffectRow>,
}

impl Substitution {
    /// Create a new empty substitution
    pub fn new() -> Self {
        Self {
            mapping: HashMap::new(),
            row_mapping: HashMap::new(),
        }
    }

    /// v0.80: row var 查询（String 键，与 EffectRow::Var(String) 同空间）。
    pub fn lookup_row(&self, name: &str) -> Option<&crate::mir::effect::EffectRow> {
        self.row_mapping.get(name)
    }

    /// v0.80: row var 绑定（直接覆盖，不做 occur check — Phase 2 仅用于
    /// builtin 累积（已被 lower 阶段解析），后期 row-poly HM 加 occur check）。
    pub fn bind_row(&mut self, name: String, row: crate::mir::effect::EffectRow) {
        self.row_mapping.insert(name, row);
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
            row_mapping: self.row_mapping.clone(),
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
            row_mapping: self.row_mapping.clone(),
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

        // v0.75.92: Unknown fail-fast — 与任何类型合一都失败（v0.75.91 引入的
        // 逃逸标签；fail-fast 语义迫使调用方用 TypeVar 推断路径产出精确类型）。
        (crate::typeck::Type::Unknown, _) | (_, crate::typeck::Type::Unknown) => {
            Err(crate::typeck::hm::error::TypeError::UnificationFailure {
                expected: format!("{:?}", ty1),
                got: format!("{:?}", ty2),
                span: None,
            })
        }

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

    // ─── v0.75.86: Type::subtype_of（非对称 subtype 关系）──

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
    fn subtype_any_is_top() {
        // Any 是 top type —— 与任何 subtype 关系成立
        assert!(Type::Any.subtype_of(&Type::Int));
        assert!(Type::String.subtype_of(&Type::Any));
        assert!(Type::Any.subtype_of(&Type::Any));
    }

    #[test]
    fn subtype_is_asymmetric() {
        // 关键不变量：A <: B 不蕴含 B <: A（区别于 compatible_with）
        assert!(Type::Int.subtype_of(&Type::Int));
        // Int 不 subtype String（同构严格相等——同构即 subtype，但不互通）
        assert!(!Type::Int.subtype_of(&Type::String));
        assert!(!Type::String.subtype_of(&Type::Int));
    }

    #[test]
    fn subtype_concrete_impls_trait() {
        // Concrete 实现 trait → Concrete subtype Trait
        let comparable = trait_ty("Comparable", vec![Type::Int]);
        let int_val = concrete("MyInt", vec![Type::Int], vec![comparable.clone()]);
        assert!(int_val.subtype_of(&comparable));
        // 反向不成立：Trait 不 subtype Concrete
        assert!(!comparable.subtype_of(&int_val));
    }

    #[test]
    fn subtype_concrete_with_no_trait_fails() {
        // Concrete 没有实现 super Trait → 不 subtype
        let comparable = trait_ty("Comparable", vec![Type::Int]);
        let bare = concrete("Plain", vec![Type::Int], vec![]);
        assert!(!bare.subtype_of(&comparable));
    }

    #[test]
    fn subtype_concrete_recurses_into_traits_list() {
        // Concrete.traits 含多个 Trait——任一匹配即 subtype
        let show = trait_ty("Show", vec![]);
        let eq = trait_ty("Eq", vec![Type::Int]);
        let val = concrete("Widget", vec![], vec![show.clone(), eq.clone()]);
        assert!(val.subtype_of(&show));
        assert!(val.subtype_of(&eq));
    }

    #[test]
    fn subtype_trait_same_name_generics() {
        // Trait<A> subtype Trait<B> 当 A subtype B（递归到 element）
        let a = trait_ty("Container", vec![Type::Int]);
        let b = trait_ty("Container", vec![Type::Int]);
        // 同名 + 同 arity + 同 element → subtype（对称同构）
        assert!(a.subtype_of(&b));
        assert!(b.subtype_of(&a));
        // 容器递归：A<Concrete<...>> subtype A<Trait> 通过 Concrete<:Trait
        // 用真实类型 Foo（trait）和 ConcreteFoo（实现 Foo 的具体类型）
        let foo = trait_ty("Foo", vec![]);
        let concrete_foo = concrete("ConcreteFoo", vec![], vec![foo.clone()]);
        let container_concrete = trait_ty("Container", vec![concrete_foo]);
        let container_trait = trait_ty("Container", vec![foo]);
        assert!(container_concrete.subtype_of(&container_trait));
    }

    #[test]
    fn subtype_list_recurses_into_element() {
        // List<Concrete> subtype List<Trait> 当 Concrete subtype Trait
        let comparable = trait_ty("Comparable", vec![Type::Int]);
        let int_val = concrete("MyInt", vec![Type::Int], vec![comparable.clone()]);
        let list_concrete = Type::List(Box::new(int_val));
        let list_trait = Type::List(Box::new(comparable));
        assert!(list_concrete.subtype_of(&list_trait));
        // 反向不成立
        assert!(!list_trait.subtype_of(&list_concrete));
    }

    #[test]
    fn subtype_dict_recurses() {
        // Dict<Concrete, ...> subtype Dict<Trait, ...>
        let comparable = trait_ty("Comparable", vec![]);
        let key_concrete = concrete("MyKey", vec![], vec![comparable.clone()]);
        let d1 = Type::Dict(Box::new(key_concrete), Box::new(Type::Int));
        let d2 = Type::Dict(Box::new(comparable), Box::new(Type::Int));
        assert!(d1.subtype_of(&d2));
    }

    #[test]
    fn subtype_union_member_matches() {
        // Union(m1, m2) subtype T 当任一 m subtype T
        let comparable = trait_ty("Comparable", vec![Type::Int]);
        let val = concrete("V", vec![Type::Int], vec![comparable.clone()]);
        let union_ty = Type::Union(vec![Type::String, val.clone()]);
        // Union 含 V，V subtype Comparable → Union subtype Comparable
        assert!(union_ty.subtype_of(&comparable));
        // 反向：Comparable 不 subtype Union[String, V]（Union 成员是 String
        // 和 V，Comparable 都不是——保守方向 false）
        assert!(!comparable.subtype_of(&union_ty));
    }

    #[test]
    fn subtype_nil_only_self() {
        assert!(Type::Nil.subtype_of(&Type::Nil));
        // Nil 不 subtype 其他类型（v0.12 后门 2 关闭）
        assert!(!Type::Nil.subtype_of(&Type::Int));
        assert!(!Type::String.subtype_of(&Type::Nil));
    }

    #[test]
    fn subtype_forall_recurses_inner() {
        // ForAll<α.τ> subtype T 当 τ subtype T（命中 env 时已实例化，
        // 此处防御——ForAll 是「所有实例化」的上界，递归到内层是保守策略）
        let inner = Type::Int;
        let forall = Type::ForAll(vec!['a'], Box::new(inner.clone()));
        assert!(forall.subtype_of(&Type::Int)); // ForAll<α.Int> <: Int 当 Int <: Int
        assert!(forall.subtype_of(&Type::Any)); // ForAll<α.Int> <: Any
        // 反向：T subtype ForAll<α.U> 当 T subtype U 成立（α 重新实例化为 T 自身）——
        // 这与 HM 标准的「T 是 ForAll 的实例」匹配：Int 是 ForAll<α.Int> 的实例
        // （α 绑定为 Int 后，body 退化为 Int）。
        assert!(inner.subtype_of(&forall)); // Int subtype ForAll<α.Int>
        // String 不 subtype ForAll<α.Int>（String 不 <: Int）
        assert!(!Type::String.subtype_of(&forall));
    }

    #[test]
    fn subtype_trait_object_matches_trait_with_same_name_generics() {
        // v0.75.86: TraitObject subtype Trait 升级为 trait_name + generics
        // 同构判断（之前 unit variant 返 false 是 stub）。
        let comparable = trait_ty("Comparable", vec![Type::Int]);
        // name 不匹配 → false
        let obj_other = Type::TraitObject {
            trait_name: "Other".to_string(),
            generics: vec![],
        };
        assert!(!obj_other.subtype_of(&comparable));
        // name + generics 匹配 → true
        let obj_match = Type::TraitObject {
            trait_name: "Comparable".to_string(),
            generics: vec![Type::Int],
        };
        assert!(obj_match.subtype_of(&comparable));
        // 反向不成立
        assert!(!comparable.subtype_of(&obj_match));
    }

    #[test]
    fn subtype_existing_compatible_is_preserved() {
        // subtype_of 的对称化形式（subtype_of(a,b) || subtype_of(b,a)）
        // 应等价于 compatible_with(a,b) —— 这是这次重构的兼容性保证
        let cases: Vec<(Type, Type)> = vec![
            (Type::Int, Type::Int),
            (Type::String, Type::String),
            (
                Type::List(Box::new(Type::Int)),
                Type::List(Box::new(Type::Int)),
            ),
            (
                Type::Dict(Box::new(Type::String), Box::new(Type::Int)),
                Type::Dict(Box::new(Type::String), Box::new(Type::Int)),
            ),
        ];
        for (a, b) in cases {
            let symmetric = a.subtype_of(&b) || b.subtype_of(&a);
            let compatible = a.compatible_with(&b);
            assert_eq!(
                symmetric, compatible,
                "subtype symmetrization ≠ compatible_with for ({:?}, {:?})",
                a, b
            );
        }
    }

    // ─── v0.75.86: HMInference::diagnosed 双向 fallback 抑制 ───
    // v0.75.94: 诊断测试（`diagnosed_*` 系列6 个 + helper `lit_witness`/`var_witness`）
    // 已迁到 [`crate::typeck::hm::diag::tests`]——DiagFilter 是新主人。

    // v0.75.92: Unknown fail-fast 测试在 [`crate::typeck::hm::unify::tests`] 已有
    // （unknown_fails_subtype_with_any_type / unknown_unifies_fails /
    // join_types_with_unknown_short_circuits_to_unknown）。
}
