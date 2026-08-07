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
//! 不修改公共 API；只新增 helper 函数 [`int_literal_arms_missing`]。

use crate::common::Literal;
use crate::mir::witness::{WitnessArm, WitnessPattern};

/// v0.75.86: 检查 match arms 是否覆盖 Int literal 空间。
///
/// 返回 `Some(range_desc)` 若缺失；返回 `None` 若覆盖。
///
/// range_desc 是 `0, 1, ..., N (total <N> int values missing)` 形式
/// 用于错误报告（保守报 "其他 Int 值"——Mora Int 范围无界）。
pub fn int_literal_arms_missing(arms: &[WitnessArm]) -> Option<String> {
    // 收集所有 Int literal pattern
    let mut covered: Vec<i64> = Vec::new();
    let mut has_covering_pattern = false;

    for arm in arms {
        match &arm.pattern {
            // Wildcard / Variable / Tuple / List / Dict / TypeAscription
            // 任何 pattern 视为覆盖（保守：Mora 是动态类型，这些
            // pattern 可匹配任何值）
            WitnessPattern::Wildcard
            | WitnessPattern::Variable(_)
            | WitnessPattern::Tuple(_)
            | WitnessPattern::List { .. }
            | WitnessPattern::Dict { .. } => {
                has_covering_pattern = true;
            }
            WitnessPattern::TypeAscription { pattern, .. } => {
                // 递归解开——内层 pattern 若是 Literal(Int) 收集；
                // 否则视为覆盖
                if !has_covering_pattern {
                    collect_or_mark_covered(pattern, &mut covered, &mut has_covering_pattern);
                }
            }
            WitnessPattern::Literal(lit) => {
                if let Literal::Int(n, _) = lit {
                    covered.push(*n);
                } else {
                    // 非 Int literal（Float/String/Bool/...）
                    // 视为覆盖——保守不报错
                    has_covering_pattern = true;
                }
            }
        }
    }

    if has_covering_pattern {
        return None;
    }

    if covered.is_empty() {
        // arms 全是非覆盖 pattern（无 Int literal 也无 wildcard...）
        // ——保守不报（避免误报）
        return None;
    }

    // 覆盖了部分 Int literal——报告未覆盖范围
    // 按 AGENTS.md §6 最小修改：保守报 "其他 Int 值"
    Some(format!(
        "(covered: {:?}; other int values not covered)",
        covered
    ))
}

/// 递归 helper：展开 TypeAscription 嵌套的 pattern
fn collect_or_mark_covered(pat: &WitnessPattern, covered: &mut Vec<i64>, has_covering: &mut bool) {
    match pat {
        WitnessPattern::Wildcard
        | WitnessPattern::Variable(_)
        | WitnessPattern::Tuple(_)
        | WitnessPattern::List { .. }
        | WitnessPattern::Dict { .. } => {
            *has_covering = true;
        }
        WitnessPattern::Literal(Literal::Int(n, _)) => {
            covered.push(*n);
        }
        WitnessPattern::Literal(_) => {
            *has_covering = true;
        }
        WitnessPattern::TypeAscription { pattern, .. } => {
            collect_or_mark_covered(pattern, covered, has_covering);
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
}
