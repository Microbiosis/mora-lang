//! 逻辑变量替换映射（substitution）。
//!
//! 载体是 [`PersistentMap`]（HAMT，路径复制）：`assoc`/`remove` 返回新
//! 版本且 clone 为 O(1) 结构共享，因此搜索的每个备选分支持有独立替换
//! 快照的代价是常数级的——回溯不需要「撤销」动作，这是把数据流代替
//! 状态机用在搜索语义上的直接落地。
//!
//! 映射方向：`var id → Value`（值可以是另一个 `Value::LogicVar`，形成
//! walk 链；occur check 保证链不会成环）。

use std::borrow::Cow;

use crate::value::Value;
use crate::value::persistent::PersistentMap;

#[derive(Debug, Clone, Default)]
pub struct Subst {
    /// var id（十进制字符串键，PersistentMap 内建 String 键）→ 绑定值。
    map: PersistentMap<Value>,
}

fn key_of(var: u64) -> String {
    var.to_string()
}

impl Subst {
    pub fn new() -> Subst {
        Subst {
            map: PersistentMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// 绑定逻辑变量，返回新版本替换。
    pub fn bind(&self, var: u64, val: Value) -> Subst {
        Subst {
            map: self.map.assoc(&key_of(var), val),
        }
    }

    /// 查询变量的直接绑定（不沿链解析）。
    pub fn lookup(&self, var: u64) -> Option<&Value> {
        self.map.get(&key_of(var))
    }

    /// 沿 LogicVar 链解析到尽头。未绑定的变量原样返回（Cow 借用，零拷贝）。
    pub fn walk_ref<'a>(&self, v: &'a Value) -> Cow<'a, Value> {
        match v {
            Value::LogicVar(id) => match self.lookup(*id) {
                Some(b) => Cow::Owned(self.walk(b)),
                None => Cow::Borrowed(v),
            },
            _ => Cow::Borrowed(v),
        }
    }

    /// [`walk_ref`](Self::walk_ref) 的拥有值版本。
    pub fn walk(&self, v: &Value) -> Value {
        self.walk_ref(v).into_owned()
    }

    /// 完全解析：递归下潜 List/Dict/Cons，解析所有逻辑变量叶子。
    pub fn walk_star(&self, v: &Value) -> Value {
        let walked = self.walk(v);
        match &walked {
            Value::List(items) => {
                let mut out = Vec::with_capacity(items.len());
                let changed = items.iter().any(|it| matches!(it, Value::LogicVar(_)));
                for it in items {
                    out.push(self.walk_star(it));
                }
                if changed { Value::List(out) } else { walked }
            }
            Value::Cons { car, cdr } => Value::Cons {
                car: Box::new(self.walk_star(car)),
                cdr: Box::new(self.walk_star(cdr)),
            },
            Value::Dict(entries) => {
                let mut out = entries.clone();
                let mut changed = false;
                // v0.104.4: `values_mut()` 取代 `for (_k, val) in iter_mut()`
                //（clippy 1.98 的 for_kv_map）。
                for val in out.values_mut() {
                    let resolved = self.walk_star(val);
                    if &resolved != val {
                        changed = true;
                    }
                    *val = resolved;
                }
                if changed { Value::Dict(out) } else { walked }
            }
            _ => walked,
        }
    }

    /// occur check：`var` 是否出现在 `v`（沿替换解析后）中。
    pub fn occurs(&self, var: u64, v: &Value) -> bool {
        let walked = self.walk(v);
        match &walked {
            Value::LogicVar(id) => *id == var,
            Value::List(items) => items.iter().any(|it| self.occurs(var, it)),
            Value::Cons { car, cdr } => self.occurs(var, car) || self.occurs(var, cdr),
            Value::Dict(entries) => entries.values().any(|val| self.occurs(var, val)),
            _ => false,
        }
    }

    /// `v`（沿替换解析后）是否仍含未绑定逻辑变量（Project 实参守卫用）。
    pub fn has_unbound(&self, v: &Value) -> bool {
        let walked = self.walk(v);
        match &walked {
            Value::LogicVar(_) => true,
            Value::List(items) => items.iter().any(|it| self.has_unbound(it)),
            Value::Cons { car, cdr } => self.has_unbound(car) || self.has_unbound(cdr),
            Value::Dict(entries) => entries.values().any(|val| self.has_unbound(val)),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(id: u64) -> Value {
        Value::LogicVar(id)
    }

    #[test]
    fn bind_and_lookup() {
        let s = Subst::new();
        let s = s.bind(0, Value::Int(42));
        assert_eq!(s.lookup(0), Some(&Value::Int(42)));
        assert_eq!(s.lookup(1), None);
    }

    #[test]
    fn assoc_is_persistent() {
        let s0 = Subst::new();
        let s1 = s0.bind(0, Value::Int(1));
        let s2 = s1.bind(1, Value::Int(2));
        // 旧版本不受影响（路径复制）
        assert!(s0.lookup(0).is_none());
        assert!(s1.lookup(1).is_none());
        assert!(s2.lookup(0).is_some() && s2.lookup(1).is_some());
    }

    #[test]
    fn walk_resolves_chains() {
        // x=0 → y=1 → 42
        let s = Subst::new().bind(0, var(1)).bind(1, Value::Int(42));
        assert_eq!(s.walk(&var(0)), Value::Int(42));
    }

    #[test]
    fn walk_unbound_is_identity() {
        let s = Subst::new();
        assert!(matches!(s.walk(&var(7)), Value::LogicVar(7)));
        assert!(matches!(
            s.walk_ref(&Value::Int(3)),
            Cow::Borrowed(Value::Int(3))
        ));
    }

    #[test]
    fn walk_star_resolves_nested() {
        // x=0 → "a"；列表 [x, 2] → ["a", 2]
        let s = Subst::new().bind(0, Value::String("a".into()));
        let term = Value::List(vec![var(0), Value::Int(2)]);
        assert_eq!(
            s.walk_star(&term),
            Value::List(vec![Value::String("a".into()), Value::Int(2)])
        );
    }

    #[test]
    fn walk_star_resolves_cons() {
        let s = Subst::new().bind(0, Value::Int(1));
        let term = Value::Cons {
            car: Box::new(var(0)),
            cdr: Box::new(Value::Nil),
        };
        let out = s.walk_star(&term);
        assert!(matches!(out, Value::Cons { ref car, .. } if **car == Value::Int(1)));
    }

    #[test]
    fn occurs_direct_and_nested() {
        let s = Subst::new();
        assert!(s.occurs(0, &var(0)));
        assert!(s.occurs(0, &Value::List(vec![Value::Int(1), var(0)])));
        assert!(!s.occurs(0, &Value::List(vec![Value::Int(1)])));
    }

    #[test]
    fn occurs_through_binding() {
        // x=0 → y=1：occurs(1, x) 应沿链命中
        let s = Subst::new().bind(0, var(1));
        assert!(s.occurs(1, &var(0)));
    }
}
