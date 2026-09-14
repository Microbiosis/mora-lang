//! 解的 reification：把替换中的逻辑变量转成可打印符号。
//!
//! solve 边界上，解中的未绑定逻辑变量按首次出现顺序命名为 `_.0`、
//! `_.1` …（miniKanren reify 惯例）。逻辑变量因此从不逃逸出 solve，
//! 不会污染普通值空间。

use std::collections::HashMap;

use crate::value::Value;

use super::subst::Subst;

/// 完全解析 `v`，再把残留的未绑定逻辑变量改写为 `_.N` 字符串符号。
pub fn reify(v: &Value, s: &Subst) -> Value {
    let fully = s.walk_star(v);
    let mut names = HashMap::new();
    rename_unbound(&fully, &mut names)
}

fn rename_unbound(v: &Value, names: &mut HashMap<u64, String>) -> Value {
    match v {
        Value::LogicVar(id) => {
            let next = names.len();
            let name = names.entry(*id).or_insert_with(|| format!("_.{}", next));
            Value::String(name.clone())
        }
        Value::List(items) => {
            Value::List(items.iter().map(|it| rename_unbound(it, names)).collect())
        }
        Value::Cons { car, cdr } => Value::Cons {
            car: Box::new(rename_unbound(car, names)),
            cdr: Box::new(rename_unbound(cdr, names)),
        },
        Value::Dict(entries) => Value::Dict(
            entries
                .iter()
                .map(|(k, val)| (k.clone(), rename_unbound(val, names)))
                .collect(),
        ),
        _ => v.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reify_bound_var_resolves_fully() {
        let s = Subst::new().bind(0, Value::Int(42));
        assert_eq!(reify(&Value::LogicVar(0), &s), Value::Int(42));
    }

    #[test]
    fn reify_unbound_vars_named_by_first_occurrence() {
        // [x, y, x] 均未绑定 → ["_.0", "_.1", "_.0"]
        let term = Value::List(vec![
            Value::LogicVar(9),
            Value::LogicVar(4),
            Value::LogicVar(9),
        ]);
        let out = reify(&term, &Subst::new());
        assert_eq!(
            out,
            Value::List(vec![
                Value::String("_.0".into()),
                Value::String("_.1".into()),
                Value::String("_.0".into()),
            ])
        );
    }

    #[test]
    fn reify_renames_cons_nesting() {
        let term = Value::Cons {
            car: Box::new(Value::LogicVar(3)),
            cdr: Box::new(Value::Nil),
        };
        let out = reify(&term, &Subst::new());
        assert!(matches!(
            out,
            Value::Cons { ref car, .. } if **car == Value::String("_.0".into())
        ));
    }

    #[test]
    fn reify_ground_is_identity() {
        let s = Subst::new();
        assert_eq!(reify(&Value::Int(1), &s), Value::Int(1));
    }
}
