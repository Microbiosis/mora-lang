//! v0.80: Effect row operations — row-polymorphic HM unification 的子模块。
//!
//! 与 typeck/mod.rs::Type::Arrow 配合使用。
//!
//! 不变量：
//! - `unify_row(a, b, subst)`: 解 row equation；空 = Empty，
//!   Var = row-polymorphic 变量（∀ρ），Cons = 具名 effect 头 + 尾。
//! - `bind_row(name, val)`: 把 row var 绑到具体 row；occur check 避免循环。
//! - `rename_row(row, fresh)`: 重命名 row var（与 Type::TypeVar 同样字符命名空间）。
//! - `apply_row(row, subst)`: 把 substitution 应用到 row —— Var 替换为 bound row。

use crate::mir::effect::EffectRow;
use crate::typeck::hm::error::TypeError;

/// v0.80: row var 命名器 —— 与 typeck::Type::TypeVar(char) 命名空间独立，
/// row var 用 String（用户可见可读）。
#[derive(Default)]
pub struct FreshVars {
    counter: u32,
}

impl FreshVars {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn row_var(&mut self, _old: &str) -> String {
        let name = format!("rho{}", self.counter);
        self.counter += 1;
        name
    }
}

/// v0.84: row var occur check —— 检查 row var 名是否在 row 中出现。
/// 用于防止循环绑定（如 `v → Cons("Ai", Var(v))`）。
pub fn occurs_in_row(name: &str, row: &EffectRow) -> bool {
    use EffectRow::*;
    match row {
        Empty => false,
        Var(v) => v == name,
        Cons(_h, t) => occurs_in_row(name, t),
    }
}

/// 单步 unify EffectRow。
///
/// 算法骨架（参考 Koka row unification）：
/// - Empty vs Empty → OK
/// - Var(v) vs Var(w) → 同名 OK；都未绑定则单向绑定 v→Var(w)；一方已绑定则递归
/// - Var(v) vs 非-Var → occur check（v 不在对方 row 中）后单向绑定
/// - 非-Var vs Var(v) → 同上（v 不在非-Var 方中）
/// - Empty vs Cons(h, _) → Error: 0 ≠ n
/// - Cons(h, _) vs Empty → 同上
/// - Cons(h1, t1) vs Cons(h2, t2) → h1 == h2；递归 t1, t2
pub fn unify_row(
    a: &EffectRow,
    b: &EffectRow,
    subst: &mut super::unify::Substitution,
) -> Result<(), TypeError> {
    use EffectRow::*;
    match (a, b) {
        (Empty, Empty) => Ok(()),
        // v0.84: 两个 row var —— 区分同名 / 不同名 + occur check
        (Var(v), Var(w)) => {
            if v == w {
                Ok(())
            } else if let Some(prev) = subst.lookup_row(v) {
                // v 已绑定：用 bound 值递归
                let prev = prev.clone();
                unify_row(&prev, b, subst)
            } else if let Some(prev) = subst.lookup_row(w) {
                // w 已绑定：用 bound 值递归
                let prev = prev.clone();
                unify_row(a, &prev, subst)
            } else {
                // 都未绑定：单向绑定 v → Var(w)（禁止双向写入导致循环）
                subst.bind_row(v.clone(), EffectRow::Var(w.clone()));
                Ok(())
            }
        }
        // v0.84: Var(v) vs 非-Var 或 非-Var vs Var(v) —— occur check 后单向绑定
        (Var(v), _) | (_, Var(v)) => {
            let other = if matches!(a, Var(_)) { b } else { a };
            // Occur check：v 不能出现在 other row 中（否则循环）
            if occurs_in_row(v, other) {
                return Err(TypeError::EffectRowMismatch {
                    expected: format!("row var `{}`", v),
                    got: "self-referential row (occurs check)".to_string(),
                    span: None,
                });
            }
            if let Some(prev) = subst.lookup_row(v) {
                let prev = prev.clone();
                unify_row(&prev, other, subst)
            } else {
                subst.bind_row(v.clone(), other.clone());
                Ok(())
            }
        }
        (Empty, Cons(h, _)) | (Cons(h, _), Empty) => Err(TypeError::EffectRowMismatch {
            expected: "pure".to_string(),
            got: format!("{{ {} }}", h),
            span: None,
        }),
        (Cons(h1, t1), Cons(h2, t2)) => {
            if h1 != h2 {
                Err(TypeError::EffectRowMismatch {
                    expected: h1.clone(),
                    got: h2.clone(),
                    span: None,
                })
            } else {
                let t1_owned = t1.as_ref().clone();
                let t2_owned = t2.as_ref().clone();
                unify_row(&t1_owned, &t2_owned, subst)
            }
        }
    }
}

/// 把 row var 绑到具体 row（写入 substitution）。
/// v0.84: occur check 在 unify_row 内部完成，本函数只写入映射。
pub fn bind_row(
    subst: &mut super::unify::Substitution,
    name: String,
    row: EffectRow,
) -> Result<(), TypeError> {
    subst.bind_row(name, row);
    Ok(())
}

/// 重命名 row var（与 Type::TypeVar 同样字符命名空间）。
///
/// Mora 字符命名空间：单字符 `'a`..`'z`，fresh 由 FreshVars 提供。
/// 这里我们用字符串（Var(String)）。
pub fn rename_row(row: &EffectRow, fresh: &mut FreshVars) -> EffectRow {
    use EffectRow::*;
    match row {
        Empty => EffectRow::Empty,
        Var(v) => EffectRow::Var(fresh.row_var(v)),
        Cons(h, t) => {
            let new_t = rename_row(t, fresh);
            EffectRow::Cons(h.clone(), Box::new(new_t))
        }
    }
}

/// Apply substitution: 把 row var 替换为 bound row（递归）。
pub fn apply_row(row: &EffectRow, subst: &super::unify::Substitution) -> EffectRow {
    use EffectRow::*;
    match row {
        Empty => EffectRow::Empty,
        Var(v) => {
            if let Some(bound) = subst.lookup_row(v) {
                bound.clone()
            } else {
                EffectRow::Var(v.clone())
            }
        }
        Cons(h, t) => {
            let new_t = apply_row(t, subst);
            EffectRow::Cons(h.clone(), Box::new(new_t))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subst() -> super::super::unify::Substitution {
        super::super::unify::Substitution::new()
    }

    #[test]
    fn unify_empty_empty() {
        let mut s = subst();
        assert!(unify_row(&EffectRow::Empty, &EffectRow::Empty, &mut s).is_ok());
    }

    #[test]
    fn unify_empty_with_cons_fails() {
        let mut s = subst();
        let a = EffectRow::Empty;
        let b = EffectRow::Cons("Ai".into(), Box::new(EffectRow::Empty));
        assert!(unify_row(&a, &b, &mut s).is_err());
    }

    #[test]
    fn unify_var_with_empty_binds() {
        let mut s = subst();
        let a = EffectRow::Var("e".into());
        let b = EffectRow::Empty;
        assert!(unify_row(&a, &b, &mut s).is_ok());
        // lookup the variable to verify binding
        let bound = s.lookup_row("e").unwrap();
        assert!(matches!(bound, EffectRow::Empty));
    }

    #[test]
    fn unify_cons_same_head() {
        let mut s = subst();
        let a = EffectRow::Cons("Ai".into(), Box::new(EffectRow::Empty));
        let b = EffectRow::Cons("Ai".into(), Box::new(EffectRow::Empty));
        assert!(unify_row(&a, &b, &mut s).is_ok());
    }

    #[test]
    fn unify_cons_different_head_fails() {
        let mut s = subst();
        let a = EffectRow::Cons("Ai".into(), Box::new(EffectRow::Empty));
        let b = EffectRow::Cons("Fs".into(), Box::new(EffectRow::Empty));
        assert!(unify_row(&a, &b, &mut s).is_err());
    }

    #[test]
    fn rename_row_vars() {
        let mut fresh = FreshVars::new();
        let row = EffectRow::Var("e".into());
        let renamed = rename_row(&row, &mut fresh);
        assert!(matches!(renamed, EffectRow::Var(_)));
        // old name should be different
        if let EffectRow::Var(new_name) = renamed {
            assert_ne!(new_name, "e");
        }
    }

    #[test]
    fn unify_same_var_ok() {
        // v0.84: Var("x") vs Var("x") should succeed (trivial identity)
        let mut s = subst();
        let a = EffectRow::Var("x".into());
        let b = EffectRow::Var("x".into());
        assert!(unify_row(&a, &b, &mut s).is_ok());
    }

    #[test]
    fn unify_two_unbound_vars_binds_one_way() {
        // v0.84: Var("x") vs Var("y") should bind x→Var(y) only (no dual-bind)
        let mut s = subst();
        let a = EffectRow::Var("x".into());
        let b = EffectRow::Var("y".into());
        assert!(unify_row(&a, &b, &mut s).is_ok());
        // x should be bound to Var("y")
        let bound = s.lookup_row("x").unwrap();
        assert!(matches!(bound, EffectRow::Var(v) if v == "y"));
        // y should NOT be bound (no dual-bind)
        assert!(s.lookup_row("y").is_none());
    }

    #[test]
    fn unify_var_with_self_referential_row_fails() {
        // v0.84: Var("x") vs Cons("Ai", Var("x")) must fail (occurs check)
        let mut s = subst();
        let a = EffectRow::Var("x".into());
        let b = EffectRow::Cons("Ai".into(), Box::new(EffectRow::Var("x".into())));
        assert!(unify_row(&a, &b, &mut s).is_err());
    }

    #[test]
    fn unify_var_with_nested_self_referential_row_fails() {
        // v0.84: Var("x") vs Cons("Ai", Cons("Fs", Var("x"))) must fail
        let mut s = subst();
        let a = EffectRow::Var("x".into());
        let b = EffectRow::Cons(
            "Ai".into(),
            Box::new(EffectRow::Cons(
                "Fs".into(),
                Box::new(EffectRow::Var("x".into())),
            )),
        );
        assert!(unify_row(&a, &b, &mut s).is_err());
    }

    #[test]
    fn unify_non_var_with_containing_var_fails() {
        // v0.84: Cons("Ai", Var("x")) vs Var("x") must fail (x in Cons)
        let mut s = subst();
        let a = EffectRow::Cons("Ai".into(), Box::new(EffectRow::Var("x".into())));
        let b = EffectRow::Var("x".into());
        assert!(unify_row(&a, &b, &mut s).is_err());
    }

    #[test]
    fn unify_bound_var_with_empty() {
        // v0.84: Var("x") already bound to Empty, unify with Var("y") should bind y→Empty
        let mut s = subst();
        s.bind_row("x".into(), EffectRow::Empty);
        let a = EffectRow::Var("x".into());
        let b = EffectRow::Var("y".into());
        assert!(unify_row(&a, &b, &mut s).is_ok());
        // x still bound to Empty
        assert!(matches!(s.lookup_row("x").unwrap(), EffectRow::Empty));
        // y should now be bound to Empty (via recursive unification of Empty vs Var(y))
        assert!(matches!(s.lookup_row("y").unwrap(), EffectRow::Empty));
    }

    #[test]
    fn occurs_in_row_basic() {
        assert!(!occurs_in_row("x", &EffectRow::Empty));
        assert!(occurs_in_row("x", &EffectRow::Var("x".into())));
        assert!(!occurs_in_row("y", &EffectRow::Var("x".into())));
        assert!(occurs_in_row(
            "x",
            &EffectRow::Cons("Ai".into(), Box::new(EffectRow::Var("x".into())))
        ));
        assert!(!occurs_in_row(
            "y",
            &EffectRow::Cons("Ai".into(), Box::new(EffectRow::Var("x".into())))
        ));
    }
}
