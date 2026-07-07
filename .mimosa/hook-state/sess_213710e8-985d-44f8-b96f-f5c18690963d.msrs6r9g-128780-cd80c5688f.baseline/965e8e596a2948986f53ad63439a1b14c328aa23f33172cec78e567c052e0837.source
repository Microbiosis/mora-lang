//! v0.78: Effect row type — algebraic effect 的类型表示。
//!
//! 三个变体：
//! - Empty：无副作用
//! - Var(name)：row-polymorphic 变量（用于 `forall e.` 的多态）
//! - Cons(head, tail)：具名 effect 头 + 尾
//!
//! 与 typeck/mod.rs::Type::TypeVar 同样的字符命名空间 —
//! 未来 row unification 与现有 HM 引擎共用 Substitution。

use std::fmt;

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum EffectRow {
    #[default]
    Empty,
    /// Row-polymorphic 变量。name 是用户可见字符串（如 "e" / "ρ"），
    /// 用于 `forall e. ...` 的多态实例化。
    Var(String),
    /// Cons(head, tail)：head 是具体 effect 标签（如 "Ai"、"Fs"），tail 可以是 Empty / Var / Cons。
    /// tail 用 Box<EffectRow> 与 Type::List(Box<Type>) 同样形态。
    Cons(String, Box<EffectRow>),
}

impl EffectRow {
    /// 累积一个具名 effect 到 row 中（push 到 Cons 链末尾）。
    /// 用于 mir/mod.rs::MirFunction::effects 字段的 lowering 填充。
    ///
    /// 已存在同名 label 时返回 false，未追加。
    pub fn extend(&mut self, effect_name: &str) -> bool {
        if self.contains(effect_name) {
            return false;
        }
        *self = match std::mem::take(self) {
            EffectRow::Empty => {
                EffectRow::Cons(effect_name.to_string(), Box::new(EffectRow::Empty))
            }
            EffectRow::Var(_) => EffectRow::Cons(
                effect_name.to_string(),
                Box::new(std::mem::take(self)),
            ),
            EffectRow::Cons(h, t) => {
                let mut new_tail = *t;
                new_tail.extend(effect_name);
                EffectRow::Cons(h, Box::new(new_tail))
            }
        };
        true
    }

    pub fn contains(&self, label: &str) -> bool {
        match self {
            EffectRow::Empty => false,
            // 多态变量 — 任何 label 都可能落入
            EffectRow::Var(_) => true,
            EffectRow::Cons(h, t) => h == label || t.contains(label),
        }
    }

    /// Cons 链长度（Var 视为 0，Empty 视为 0）。
    pub fn len(&self) -> usize {
        match self {
            EffectRow::Empty | EffectRow::Var(_) => 0,
            EffectRow::Cons(_, t) => 1 + t.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, EffectRow::Empty)
    }

    /// 迭代所有具名 effect label（不含 Var — Var 是多态占位符）。
    pub fn labels(&self) -> Vec<&str> {
        let mut out = Vec::new();
        Self::collect_labels(self, &mut out);
        out
    }

    fn collect_labels<'a>(row: &'a EffectRow, out: &mut Vec<&'a str>) {
        match row {
            EffectRow::Empty | EffectRow::Var(_) => {}
            EffectRow::Cons(h, t) => {
                out.push(h.as_str());
                Self::collect_labels(t, out);
            }
        }
    }
}

impl fmt::Display for EffectRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EffectRow::Empty => write!(f, "pure"),
            EffectRow::Var(s) => write!(f, "{}", s),
            EffectRow::Cons(h, t) => match t.as_ref() {
                EffectRow::Empty => write!(f, "{}", h),
                _ => write!(f, "{}, {}", h, t),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extend_into_empty() {
        let mut r = EffectRow::default();
        assert!(r.extend("Ai"));
        assert_eq!(
            r,
            EffectRow::Cons("Ai".into(), Box::new(EffectRow::Empty))
        );
    }

    #[test]
    fn extend_idempotent() {
        let mut r = EffectRow::default();
        assert!(r.extend("Ai"));
        assert!(!r.extend("Ai"), "second extend should return false");
        assert_eq!(
            r,
            EffectRow::Cons("Ai".into(), Box::new(EffectRow::Empty))
        );
    }

    #[test]
    fn extend_accumulates_multiple() {
        let mut r = EffectRow::default();
        r.extend("Ai");
        r.extend("Fs");
        assert_eq!(r.len(), 2);
        assert!(r.contains("Ai"));
        assert!(r.contains("Fs"));
        assert!(!r.contains("Mem"));
    }

    #[test]
    fn var_contains_anything() {
        let r = EffectRow::Var("e".into());
        assert!(r.contains("Ai"));
        assert!(r.contains("Fs"));
        assert_eq!(r.len(), 0, "Var 是多态占位符，不计入具名长度");
    }

    #[test]
    fn empty_contains_nothing() {
        let r = EffectRow::default();
        assert!(!r.contains("Ai"));
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn display_formatting() {
        assert_eq!(EffectRow::Empty.to_string(), "pure");
        let mut r = EffectRow::default();
        r.extend("Ai");
        assert_eq!(r.to_string(), "Ai");
        r.extend("Fs");
        assert_eq!(r.to_string(), "Ai, Fs");
    }

    #[test]
    fn labels_iterator() {
        let mut r = EffectRow::default();
        r.extend("Ai");
        r.extend("Fs");
        r.extend("Mem");
        assert_eq!(r.labels(), vec!["Ai", "Fs", "Mem"]);
    }
}