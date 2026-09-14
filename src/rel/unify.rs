//! 值级合一（unification）。
//!
//! 与 typeck 层的类型合一（`typeck::hm::unify`）不同，本模块在**值层**
//! 运作：逻辑变量是 `Value::LogicVar` 叶子，项是任意 Value 树。合一
//! 沿替换解析（walk）后按结构递归；Int 与 Float 是不同变体，不做隐式
//! 数值提升（与类型层数值塔的显式性一致）。

use crate::value::Value;

use super::subst::Subst;

/// 合一 `u` 与 `v`，成功返回扩展后的替换，失败返回 `None`。
pub fn unify(u: &Value, v: &Value, s: &Subst) -> Option<Subst> {
    let uw = s.walk_ref(u);
    let vw = s.walk_ref(v);
    match (&*uw, &*vw) {
        (Value::LogicVar(a), Value::LogicVar(b)) => {
            if a == b {
                Some(s.clone())
            } else {
                Some(s.bind(*a, Value::LogicVar(*b)))
            }
        }
        (Value::LogicVar(a), t) => bind_var(*a, t, s),
        (t, Value::LogicVar(a)) => bind_var(*a, t, s),
        (Value::List(xs), Value::List(ys)) => {
            if xs.len() != ys.len() {
                return None;
            }
            let mut acc = s.clone();
            for (x, y) in xs.iter().zip(ys.iter()) {
                acc = unify(x, y, &acc)?;
            }
            Some(acc)
        }
        (Value::Cons { car: c1, cdr: d1 }, Value::Cons { car: c2, cdr: d2 }) => {
            let s1 = unify(c1, c2, s)?;
            unify(d1, d2, &s1)
        }
        (Value::Dict(a), Value::Dict(b)) => {
            if a.len() != b.len() {
                return None;
            }
            let mut acc = s.clone();
            for (k, av) in a.iter() {
                let bv = b.get(k)?;
                acc = unify(av, bv, &acc)?;
            }
            Some(acc)
        }
        (Value::Nil, Value::Nil) => Some(s.clone()),
        (a, b) if is_ground_scalar(a) && is_ground_scalar(b) => {
            if a == b {
                Some(s.clone())
            } else {
                None
            }
        }
        _ => None,
    }
}

fn bind_var(var: u64, t: &Value, s: &Subst) -> Option<Subst> {
    if s.occurs(var, t) {
        None
    } else {
        Some(s.bind(var, t.clone()))
    }
}

fn is_ground_scalar(v: &Value) -> bool {
    matches!(
        v,
        Value::String(_)
            | Value::Char(_)
            | Value::Int(_)
            | Value::Float(_)
            | Value::BigInt(_)
            | Value::Bool(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(id: u64) -> Value {
        Value::LogicVar(id)
    }

    #[test]
    fn unify_var_with_ground() {
        let s = unify(&v(0), &Value::Int(5), &Subst::new()).expect("unify");
        assert_eq!(s.walk(&v(0)), Value::Int(5));
    }

    #[test]
    fn unify_ground_with_var_symmetric() {
        let s = unify(&Value::String("a".into()), &v(3), &Subst::new()).expect("unify");
        assert_eq!(s.walk(&v(3)), Value::String("a".into()));
    }

    #[test]
    fn unify_var_var_links_chain() {
        let s = unify(&v(0), &v(1), &Subst::new()).expect("unify");
        let s = unify(&v(1), &Value::Int(9), &s).expect("unify");
        assert_eq!(s.walk(&v(0)), Value::Int(9));
    }

    #[test]
    fn unify_same_var_is_noop() {
        let s = Subst::new();
        let out = unify(&v(4), &v(4), &s).expect("unify");
        assert!(out.is_empty());
    }

    #[test]
    fn occur_check_rejects_self_reference() {
        // x = cons(x, nil) 必须失败（无限项）
        let t = Value::Cons { car: Box::new(v(0)), cdr: Box::new(Value::Nil) };
        assert!(unify(&v(0), &t, &Subst::new()).is_none());
    }

    #[test]
    fn occur_check_rejects_nested_through_binding() {
        // x → y（已绑定），再 y = [x] 应失败
        let s = Subst::new().bind(1, v(0));
        let t = Value::List(vec![v(1)]);
        assert!(unify(&v(0), &t, &s).is_none());
    }

    #[test]
    fn unify_lists_elementwise() {
        let a = Value::List(vec![v(0), Value::Int(2)]);
        let b = Value::List(vec![Value::Int(1), v(1)]);
        let s = unify(&a, &b, &Subst::new()).expect("unify");
        assert_eq!(s.walk(&v(0)), Value::Int(1));
        assert_eq!(s.walk(&v(1)), Value::Int(2));
    }

    #[test]
    fn unify_list_length_mismatch_fails() {
        let a = Value::List(vec![Value::Int(1)]);
        let b = Value::List(vec![Value::Int(1), Value::Int(2)]);
        assert!(unify(&a, &b, &Subst::new()).is_none());
    }

    #[test]
    fn unify_dicts_by_key() {
        let a = Value::Dict(HashMap::from([("x".into(), v(0)), ("y".into(), Value::Int(2))]));
        let b = Value::Dict(HashMap::from([("x".into(), Value::Int(1)), ("y".into(), v(1))]));
        let s = unify(&a, &b, &Subst::new()).expect("unify");
        assert_eq!(s.walk(&v(0)), Value::Int(1));
        assert_eq!(s.walk(&v(1)), Value::Int(2));
    }

    #[test]
    fn unify_dict_key_mismatch_fails() {
        let a = Value::Dict(HashMap::from([("x".into(), Value::Int(1))]));
        let b = Value::Dict(HashMap::from([("z".into(), Value::Int(1))]));
        assert!(unify(&a, &b, &Subst::new()).is_none());
    }

    #[test]
    fn unify_cons_structurally() {
        let a = Value::Cons { car: Box::new(v(0)), cdr: Box::new(v(1)) };
        let b = Value::Cons { car: Box::new(Value::Int(1)), cdr: Box::new(Value::Nil) };
        let s = unify(&a, &b, &Subst::new()).expect("unify");
        assert_eq!(s.walk(&v(0)), Value::Int(1));
        assert_eq!(s.walk(&v(1)), Value::Nil);
    }

    #[test]
    fn unify_cons_vs_nil_fails() {
        let a = Value::Cons { car: Box::new(Value::Int(1)), cdr: Box::new(Value::Nil) };
        assert!(unify(&a, &Value::Nil, &Subst::new()).is_none());
    }

    #[test]
    fn unify_int_float_no_implicit_promotion() {
        assert!(unify(&Value::Int(1), &Value::Float(1.0), &Subst::new()).is_none());
    }

    #[test]
    fn unify_scalars_eq_and_neq() {
        let s = Subst::new();
        assert!(unify(&Value::String("a".into()), &Value::String("a".into()), &s).is_some());
        assert!(unify(&Value::Bool(true), &Value::Bool(false), &s).is_none());
        assert!(unify(&Value::Nil, &Value::Nil, &s).is_some());
    }

    use std::collections::HashMap;
}
