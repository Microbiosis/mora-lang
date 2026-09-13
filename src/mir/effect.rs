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

    /// v0.96: 移除一个具名 effect 标签（handle 吸收语义的纯函数原语）。
    ///
    /// `handle X { body }` 的残差行 = body 行去掉所有 X 出现点。Var 是
    /// 多态占位符，保持不变（未知行交由约束推迟消解）。
    pub fn remove(&self, label: &str) -> EffectRow {
        match self {
            EffectRow::Empty | EffectRow::Var(_) => self.clone(),
            EffectRow::Cons(h, t) => {
                if h == label {
                    t.remove(label)
                } else {
                    EffectRow::Cons(h.clone(), Box::new(t.remove(label)))
                }
            }
        }
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

// ─── v0.99: ambient effect 标签（单一事实源）────────────────────────
//
// ambient effect = 运行时**根 handler 兜底**、类型层**行传播**的效果。
// 典型对象是隐式全局状态机（v0.99 前的 `random`：进程级
// `static Mutex<Xoshiro256>`）—— 副作用真实存在，但用户从未声明。
// 数据流化之后：状态线性持有在根 handler 对象内（每运行时一份、无锁、
// worker 克隆天然独立），类型层把 `random.*` 方法调用记入效果行，
// 程序根边界的残差行断言放行 ambient 标签（根 handler 即其处理者）。
//
// 本模块是 typeck（签名预置 / 行分类 / 边界豁免）与 runtime（根 handler
// 安装 / 方法分派）共用的标签事实源 —— 标签即 `EffectRow::Cons` 的 head
// 字符串，放 mir 层避免 typeck→runtime 反向依赖。
pub mod ambient {
    /// `random` 模块各方法对应的 ambient effect 标签。
    /// 命名规则：`random_` + 方法名（系统化、可 grep、无特例）。
    pub const RANDOM_LABELS: [&str; 6] = [
        "random_random",
        "random_rand_int",
        "random_rand_float",
        "random_rand_choice",
        "random_seed",
        "random_shuffle",
    ];

    /// `random.<method>(...)` 调用对应的 ambient effect 标签。
    /// 未知方法返回 None（调用方报 unknown method）。
    pub fn random_label_for_method(method: &str) -> Option<&'static str> {
        match method {
            "random" => Some("random_random"),
            "rand_int" => Some("random_rand_int"),
            "rand_float" => Some("random_rand_float"),
            "rand_choice" => Some("random_rand_choice"),
            "seed" => Some("random_seed"),
            "shuffle" => Some("random_shuffle"),
            _ => None,
        }
    }

    /// 标签是否属于 ambient 集合（运行时根 handler 已兜底）。
    pub fn is_ambient_label(label: &str) -> bool {
        RANDOM_LABELS.contains(&label)
    }
}

// ─── v0.93: 运行时效应值（effect-as-data）────────────────────────────
//
// `EffectRow` 是效果的**类型**表示（编译期）；`Effect`/`Effects` 是效果的
// **运行时数据**表示。BSP 引擎的 send / aggregate 语句过去直接 push 到宿主
// 上的 `&mut Vec`（可变状态机式侧信道），导致：
//   1. 并行 worker 各自持有克隆宿主，worker 产出的 effect 与主线程的缓冲
//      互相不可见 —— worker 的 aggregator 贡献被静默丢弃（正确性缺陷）；
//   2. 合并顺序依赖「谁先改缓冲」，而非数据的确定性 fold。
//
// 改为「效果即数据」：执行产出一个 `Effects` 值（纯数据），worker 边界按
// 确定顺序 `merge` 折叠。`merge` 满足结合律（Vec 拼接），因此并发执行的结果
// 与串行执行逐字节一致 —— 并发成为数据的自然属性，而非需要加锁防御的状态。

use crate::checkpoint::SendTask;
use crate::mir::orchestrate::AggregatorContribution;

/// 一次执行产生的单个效应。与 `EffectRow` 的具名标签一一对应：
/// `Send` ↔ BSP 消息，`Contribute` ↔ per-super-step 聚合器贡献。
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// `send value to target` — 投递到目标顶点的下一条 BSP 消息。
    Send(SendTask),
    /// `aggregate name, value` — 向具名聚合器提交一次贡献。
    Contribute(AggregatorContribution),
}

/// 一次执行累积的效应集合。**纯数据**：可克隆、可比较、可结合律合并。
///
/// 执行器在 worker 边界产出 `Effects`，主线程按拓扑/索引顺序 `merge` 折叠。
/// 合并顺序固定 → 结果确定，无需共享可变缓冲，也无跨 worker 可见性问题。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Effects {
    pub sends: Vec<SendTask>,
    pub contributions: Vec<AggregatorContribution>,
}

impl Effects {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.sends.is_empty() && self.contributions.is_empty()
    }

    /// 追加单个效应（就地累加器语义，等价于 `Vec::push` 的折叠）。
    pub fn push(&mut self, effect: Effect) {
        match effect {
            Effect::Send(task) => self.sends.push(task),
            Effect::Contribute(contrib) => self.contributions.push(contrib),
        }
    }

    /// 结合律合并：`a.merge(b)` 保留 `a` 的元素在前。
    /// 结合律保证 worker 结果的 fold 顺序不影响最终集合内容
    /// （对同目标的多条 send，combiner 按 fold 顺序折叠，因此顺序固定即可确定）。
    pub fn merge(mut self, other: Effects) -> Effects {
        self.sends.extend(other.sends);
        self.contributions.extend(other.contributions);
        self
    }

    /// 就地合并（`merge` 的引用变体，避免 move）。
    pub fn absorb(&mut self, other: Effects) {
        self.sends.extend(other.sends);
        self.contributions.extend(other.contributions);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_absorbs_all_occurrences() {
        let row = EffectRow::Cons(
            "Ai".into(),
            Box::new(EffectRow::Cons(
                "Log".into(),
                Box::new(EffectRow::Cons("Ai".into(), Box::new(EffectRow::Empty))),
            )),
        );
        let residual = row.remove("Ai");
        assert_eq!(residual.labels(), vec!["Log"]);
    }

    #[test]
    fn remove_keeps_var_and_other_labels() {
        let var_row = EffectRow::Var("rho".into());
        assert!(matches!(var_row.remove("Ai"), EffectRow::Var(_)));
        let row = EffectRow::Cons("Log".into(), Box::new(EffectRow::Empty));
        assert_eq!(row.remove("Ai").labels(), vec!["Log"]);
        assert_eq!(EffectRow::Empty.remove("Ai"), EffectRow::Empty);
    }

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

    // ─── v0.93: Effects（effect-as-data）────────────────────────────

    fn send(target: &str, n: i64) -> Effect {
        Effect::Send(SendTask {
            target_node: target.to_string(),
            input: crate::value::Value::Int(n),
        })
    }

    fn contribute(name: &str, n: i64) -> Effect {
        Effect::Contribute(AggregatorContribution {
            name: name.to_string(),
            value: crate::value::Value::Int(n),
        })
    }

    #[test]
    fn effects_push_routes_by_variant() {
        let mut e = Effects::new();
        e.push(send("a", 1));
        e.push(contribute("sum", 2));
        assert_eq!(e.sends.len(), 1);
        assert_eq!(e.contributions.len(), 1);
        assert!(!e.is_empty());
        assert!(Effects::new().is_empty());
    }

    /// merge 满足结合律：三种分组方式结果完全一致（含顺序）。
    /// 这是并发结果的确定性保证 —— worker 产出 Effects，主线程按固定
    /// 顺序 fold，分组（= worker 边界）不影响结果。
    #[test]
    fn effects_merge_is_associative() {
        let mut a = Effects::new();
        a.push(send("x", 1));
        a.push(contribute("sum", 1));
        let mut b = Effects::new();
        b.push(send("y", 2));
        let mut c = Effects::new();
        c.push(contribute("min", 3));
        c.push(send("x", 4));

        let left = a.clone().merge(b.clone()).merge(c.clone());
        let right = a.clone().merge(b.clone().merge(c.clone()));
        assert_eq!(left, right, "merge 必须满足结合律");
        // 顺序固定：a 的元素在前，c 的在后。
        assert_eq!(
            left.sends
                .iter()
                .map(|s| s.target_node.clone())
                .collect::<Vec<_>>(),
            vec!["x".to_string(), "y".to_string(), "x".to_string()]
        );
        assert_eq!(
            left.contributions
                .iter()
                .map(|c| c.name.clone())
                .collect::<Vec<_>>(),
            vec!["sum".to_string(), "min".to_string()]
        );
    }

    /// absorb 与 merge 等价（引用变体）。
    #[test]
    fn effects_absorb_matches_merge() {
        let mut a = Effects::new();
        a.push(send("x", 1));
        let mut b = Effects::new();
        b.push(contribute("sum", 2));

        let merged = a.clone().merge(b.clone());
        a.absorb(b);
        assert_eq!(a, merged);
    }

    #[test]
    fn effects_merge_with_empty_is_identity() {
        let mut a = Effects::new();
        a.push(send("x", 1));
        assert_eq!(a.clone().merge(Effects::new()), a);
        assert_eq!(Effects::new().merge(a.clone()), a);
    }
}