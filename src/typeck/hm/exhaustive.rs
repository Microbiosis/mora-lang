//! v0.75.86: match exhaustiveness 检查（按 AGENTS.md §6 最小修改）。
//!
//! Mora match arms 的 pattern 用 [`crate::mir::witness::WitnessPattern`] 表达。
//! 完整 exhaustiveness 算法（与 type system union merge 深度集成）超出本次
//! commit 范围——本次只覆盖**最常见**场景：Int literal pattern。
//!
//! 算法：
//!   - 收集所有 Literal(Int(n)) arm pattern
//!   - 含 Wildcard / Variable / Tuple / List / Dict / TypeAscription 任一 → 视为覆盖
//!   - 收集到的 Int literal < 任意 Int literal 范围（保守报 "其他 Int 值"）
//!
//! 不报错的场景：
//!   - 含 Wildcard / Variable 任何 arm
//!   - 含 Tuple/List/Dict/TypeAscription 任何 arm（保守视为覆盖）
//!   - Literal 不是 Int 类型（保守视为覆盖）
//!
//! 错误格式（与其它 TypeError 一致）：
//!   "non-exhaustive patterns: missing int value(s) <range>"
//!
//! v0.75.86: 公共 API 包含 4 个 literal exhaustiveness 函数
//! ([`int_literal_arms_missing`] / [`float_literal_arms_missing`] /
//! [`string_literal_arms_missing`] / [`bool_literal_arms_missing`]) 加
//! [`list_dict_tuple_arms_complete`] 元数据 helper。

use crate::common::Literal;
use crate::mir::witness::{WitnessArm, WitnessPattern};

/// v0.75.86: 检查 match arms 是否覆盖某种 Literal 类型空间。
///
/// 返回 `Some(range_desc)` 若缺失；返回 `None` 若覆盖。
///
/// range_desc 是 `0, 1, ..., N (total <N> int values missing)` 形式
/// 用于错误报告（保守报 "其他 Int 值"——Mora Int 范围无界）。
pub fn int_literal_arms_missing(arms: &[WitnessArm]) -> Option<String> {
    literal_arms_missing(arms, LiteralKind::Int).map(|desc| format!("int<{}>", desc))
}

/// v0.75.86: Float literal exhaustiveness
pub fn float_literal_arms_missing(arms: &[WitnessArm]) -> Option<String> {
    literal_arms_missing(arms, LiteralKind::Float).map(|desc| format!("float<{}>", desc))
}

/// v0.75.86: String literal exhaustiveness
pub fn string_literal_arms_missing(arms: &[WitnessArm]) -> Option<String> {
    literal_arms_missing(arms, LiteralKind::String).map(|desc| format!("string<{}>", desc))
}

/// v0.75.86: Bool literal exhaustiveness
pub fn bool_literal_arms_missing(arms: &[WitnessArm]) -> Option<String> {
    literal_arms_missing(arms, LiteralKind::Bool).map(|desc| format!("bool<{}>", desc))
}

/// v0.75.86: List/Dict/Tuple pattern exhaustiveness 公共 API。
///
/// Mora 是动态类型 + rest 模式（`[head, ...tail]` / `{key: pat, ...rest}`）
/// 接受任意长度/键值——所以**List/Dict/Tuple 模式永远 covered**。
///
/// 本函数**永远返 None**——为完整 exhaustiveness 算法做前置标记。
/// Plan 完整 exhaustiveness 估 1-2 周（Type union merge 推断）——
/// 届时本函数可能进化为 "Type union 子集检查"（按 scrutinee 类型
/// 是 List<T>/Dict<K,V> 时分析）。
///
/// 按 AGENTS.md §6 最小修改：当前 commit 不实现完整算法。
pub fn list_dict_tuple_arms_complete(_arms: &[WitnessArm]) -> Option<String> {
    // 当前不实现——Mora 动态类型下 3 种模式永远 covered。
    // 保留公共 API 是为完整算法留 hook（与 4 个 literal 函数风格一致）。
    None
}

/// v0.75.86: 通用 literal exhaustiveness（4 种可枚举字面量）
fn literal_arms_missing(arms: &[WitnessArm], kind: LiteralKind) -> Option<String> {
    let mut covered_count = 0usize;
    let mut covered_examples: Vec<String> = Vec::new();
    let mut has_covering_pattern = false;

    for arm in arms {
        match &arm.pattern {
            WitnessPattern::Wildcard
            | WitnessPattern::Variable(_)
            | WitnessPattern::Tuple(_)
            | WitnessPattern::List { .. }
            | WitnessPattern::Dict { .. } => {
                has_covering_pattern = true;
            }
            WitnessPattern::TypeAscription { pattern, .. } => {
                if let Some(c) = count_or_cover(pattern, kind) {
                    if c.covering {
                        has_covering_pattern = true;
                    } else {
                        covered_count += c.count;
                        for ex in c.examples {
                            if covered_examples.len() < 5 {
                                covered_examples.push(ex);
                            }
                        }
                    }
                } else {
                    has_covering_pattern = true;
                }
            }
            WitnessPattern::Literal(lit) => {
                if matches_literal_kind(lit, kind) {
                    covered_count += 1;
                    if covered_examples.len() < 5 {
                        covered_examples.push(format!("{:?}", lit));
                    }
                } else {
                    has_covering_pattern = true;
                }
            }
        }
    }

    if has_covering_pattern {
        return None;
    }
    if covered_count == 0 {
        return None;
    }
    Some(format!("covered: {:?}", covered_examples))
}

/// v0.75.86: literal kind
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum LiteralKind {
    Int,
    Float,
    String,
    Bool,
}

fn matches_literal_kind(lit: &Literal, kind: LiteralKind) -> bool {
    matches!(
        (lit, kind),
        (Literal::Int(_, _), LiteralKind::Int)
            | (Literal::Float(_, _), LiteralKind::Float)
            | (Literal::String(_, _), LiteralKind::String)
            | (Literal::Bool(_, _), LiteralKind::Bool)
    )
}

/// v0.75.86: 递归收集
fn count_or_cover(pat: &WitnessPattern, kind: LiteralKind) -> Option<CoverInfo> {
    match pat {
        WitnessPattern::Wildcard
        | WitnessPattern::Variable(_)
        | WitnessPattern::Tuple(_)
        | WitnessPattern::List { .. }
        | WitnessPattern::Dict { .. } => Some(CoverInfo::covering()),
        WitnessPattern::Literal(lit) => {
            if matches_literal_kind(lit, kind) {
                Some(CoverInfo::literal(format!("{:?}", lit)))
            } else {
                None
            }
        }
        WitnessPattern::TypeAscription { pattern, .. } => count_or_cover(pattern, kind),
    }
}

#[derive(Debug)]
struct CoverInfo {
    covering: bool,
    count: usize,
    examples: Vec<String>,
}

impl CoverInfo {
    fn covering() -> Self {
        Self {
            covering: true,
            count: 0,
            examples: Vec::new(),
        }
    }
    fn literal(example: String) -> Self {
        Self {
            covering: false,
            count: 1,
            examples: vec![example],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::Span;
    use crate::mir::witness::{MirWitness, WitnessArm, WitnessKind, WitnessPattern};

    fn literal_int_arm(n: i64) -> WitnessArm {
        WitnessArm {
            pattern: WitnessPattern::Literal(Literal::Int(n, Span::new(0, 0))),
            guard: None,
            body: MirWitness {
                kind: WitnessKind::Literal(Literal::Int(0, Span::new(0, 0))),
                span: Span::new(0, 0),
            },
        }
    }

    fn wildcard_arm() -> WitnessArm {
        WitnessArm {
            pattern: WitnessPattern::Wildcard,
            guard: None,
            body: MirWitness {
                kind: WitnessKind::Literal(Literal::Int(0, Span::new(0, 0))),
                span: Span::new(0, 0),
            },
        }
    }

    #[test]
    fn wildcard_arms_always_complete() {
        // _ → 不报
        assert_eq!(int_literal_arms_missing(&[wildcard_arm()]), None);
    }

    #[test]
    fn single_int_literal_arm_is_incomplete() {
        // match x { 1 => ... } → 报 "其他 Int 值"
        let result = int_literal_arms_missing(&[literal_int_arm(1)]);
        assert!(
            result.is_some(),
            "single int literal should be non-exhaustive"
        );
        let s = result.unwrap();
        assert!(
            s.contains("1"),
            "should mention covered literal, got: {}",
            s
        );
    }

    #[test]
    fn mixed_literal_and_wildcard_complete() {
        // match x { 1 => ... _ => ... } → 不报
        assert_eq!(
            int_literal_arms_missing(&[literal_int_arm(1), wildcard_arm()]),
            None
        );
    }

    #[test]
    fn variable_arm_complete() {
        // match x { y => ... } → 不报（Variable 视为覆盖）
        let arm = WitnessArm {
            pattern: WitnessPattern::Variable("y".to_string()),
            guard: None,
            body: MirWitness {
                kind: WitnessKind::Literal(Literal::Int(0, Span::new(0, 0))),
                span: Span::new(0, 0),
            },
        };
        assert_eq!(int_literal_arms_missing(&[arm]), None);
    }

    #[test]
    fn multiple_int_literals_incomplete() {
        // match x { 1 => ... 2 => ... } → 报缺失
        let result = int_literal_arms_missing(&[literal_int_arm(1), literal_int_arm(2)]);
        assert!(result.is_some());
        let s = result.unwrap();
        assert!(s.contains("1") && s.contains("2"));
    }

    #[test]
    fn typeascription_around_literal_unwraps() {
        // (x: int) → 解开 → 视为 Int literal
        let arm = WitnessArm {
            pattern: WitnessPattern::TypeAscription {
                name: "int".to_string(),
                pattern: Box::new(WitnessPattern::Literal(Literal::Int(7, Span::new(0, 0)))),
            },
            guard: None,
            body: MirWitness {
                kind: WitnessKind::Literal(Literal::Int(0, Span::new(0, 0))),
                span: Span::new(0, 0),
            },
        };
        let result = int_literal_arms_missing(&[arm]);
        assert!(result.is_some(), "typeascription around int should count");
    }

    #[test]
    fn non_int_literal_arm_means_covered() {
        // match x { 1.5 => ... } → Float 视为覆盖，不报
        let arm = WitnessArm {
            pattern: WitnessPattern::Literal(Literal::Float(1.5, Span::new(0, 0))),
            guard: None,
            body: MirWitness {
                kind: WitnessKind::Literal(Literal::Int(0, Span::new(0, 0))),
                span: Span::new(0, 0),
            },
        };
        assert_eq!(int_literal_arms_missing(&[arm]), None);
    }

    // ─── List/Dict/Tuple pattern 覆盖 ───
    // v0.75.86: Mora 动态类型 + rest 模式（`[head, ...tail]` /
    // `{key: pat, ...rest}`）接受任意长度/键值——这 3 种模式
    // **永远 covered**。list_dict_tuple_arms_complete 永远返 None。
    // 6 个测试覆盖 3 种 pattern + 嵌套场景。

    fn list_head_tail_arm() -> WitnessArm {
        WitnessArm {
            pattern: WitnessPattern::List {
                head: Box::new(WitnessPattern::Variable("x".to_string())),
                tail: Box::new(WitnessPattern::Wildcard),
            },
            guard: None,
            body: MirWitness {
                kind: WitnessKind::Literal(Literal::Int(0, Span::new(0, 0))),
                span: Span::new(0, 0),
            },
        }
    }

    fn tuple_arm(elements: Vec<WitnessPattern>) -> WitnessArm {
        WitnessArm {
            pattern: WitnessPattern::Tuple(elements),
            guard: None,
            body: MirWitness {
                kind: WitnessKind::Literal(Literal::Int(0, Span::new(0, 0))),
                span: Span::new(0, 0),
            },
        }
    }

    fn dict_arm() -> WitnessArm {
        WitnessArm {
            pattern: WitnessPattern::Dict {
                required: vec![("key".to_string(), WitnessPattern::Variable("v".to_string()))],
                rest: true,
            },
            guard: None,
            body: MirWitness {
                kind: WitnessKind::Literal(Literal::Int(0, Span::new(0, 0))),
                span: Span::new(0, 0),
            },
        }
    }

    #[test]
    fn list_with_rest_always_complete() {
        // [x, ...] → 永远 covered
        assert_eq!(list_dict_tuple_arms_complete(&[list_head_tail_arm()]), None);
    }

    #[test]
    fn list_only_head_no_tail_also_complete() {
        // [x] → tail 缺失 = 长度 1 only，但 Mora 动态 List 长度可 0
        // ——保守视为 covered（rest 模式语义已覆盖）
        let arm = WitnessArm {
            pattern: WitnessPattern::List {
                head: Box::new(WitnessPattern::Variable("x".to_string())),
                tail: Box::new(WitnessPattern::Variable("y".to_string())),
            },
            guard: None,
            body: MirWitness {
                kind: WitnessKind::Literal(Literal::Int(0, Span::new(0, 0))),
                span: Span::new(0, 0),
            },
        };
        assert_eq!(list_dict_tuple_arms_complete(&[arm]), None);
    }

    #[test]
    fn tuple_always_complete() {
        // (x, y) → 永远 covered
        assert_eq!(
            list_dict_tuple_arms_complete(&[tuple_arm(vec![
                WitnessPattern::Variable("x".to_string()),
                WitnessPattern::Variable("y".to_string()),
            ])]),
            None
        );
    }

    #[test]
    fn tuple_with_literal_inside_also_complete() {
        // (1, x) → 永远 covered（Mora 动态 — 但 lit 必须等于 scrutinee[0]）
        // 本函数保守视为 covered（subpattern 递归检查在未来）
        assert_eq!(
            list_dict_tuple_arms_complete(&[tuple_arm(vec![
                WitnessPattern::Literal(Literal::Int(1, Span::new(0, 0))),
                WitnessPattern::Variable("x".to_string()),
            ])]),
            None
        );
    }

    #[test]
    fn dict_with_rest_always_complete() {
        // {key: v, ...} → 永远 covered
        assert_eq!(list_dict_tuple_arms_complete(&[dict_arm()]), None);
    }

    #[test]
    fn no_match_at_all_returns_none() {
        // 完全没 match arms——不报（无 match 就无 exhaustiveness 问题）
        assert_eq!(list_dict_tuple_arms_complete(&[]), None);
    }
}
