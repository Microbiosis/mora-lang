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
use crate::typeck::TypeError;

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

/// 单步 unify EffectRow。
///
/// 算法骨架（参考 Koka row unification）：
/// - Empty vs Empty → OK
/// - Var(v) vs _ → 绑定 v -> 对方（双向同步，保持 typeck::Substitution 双向一致）
/// - _ vs Var(v) → 同上
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
        (Var(v), _) | (_, Var(v)) => {
            // Already bound? Recurse with bound value.
            if let Some(prev) = subst.lookup_row(v) {
                let prev = prev.clone();
                let b_clone = b.clone();
                unify_row(&prev, &b_clone, subst)?;
                let a_clone = if matches!(a, Var(_)) { prev.clone() } else { a.clone() };
                unify_row(&a_clone, &b_clone, subst)?;
                Ok(())
            } else {
                let a_owned = a.clone();
                let b_owned = b.clone();
                subst.bind_row(v.clone(), a_owned);
                subst.bind_row(v.clone(), b_owned);
                Ok(())
            }
        }
        (Empty, Cons(h, _)) | (Cons(h, _), Empty) => {
            Err(TypeError::new(0, format!("effect row mismatch: empty vs {{ {} }}", h)))
        }
        (Cons(h1, t1), Cons(h2, t2)) => {
            if h1 != h2 {
                Err(TypeError::new(
                    0,
                    format!("effect label mismatch: {} vs {}", h1, h2),
                ))
            } else {
                let t1_owned = t1.as_ref().clone();
                let t2_owned = t2.as_ref().clone();
                unify_row(&t1_owned, &t2_owned, subst)
            }
        }
    }
}

/// 把 row var 绑到具体 row（写入 substitution）。
/// occur check 在 lookup_row 内部完成。
pub fn bind_row(subst: &mut super::unify::Substitution, name: String, row: EffectRow) -> Result<(), TypeError> {
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
}