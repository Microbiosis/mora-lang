//! v0.75.96: HM 内部 utility 工具——check_union / join_types。
//!
//! ## 背景
//!
//! 架构审查报告（v0.75.90）标记「🔴 阻断级风险 4」：
//! `check_union` / `join_types` 是 Foundation 但被 `hm::unify.rs` 测试
//! 反向引用——`unify.rs:492-499` 测试代码 `use crate::typeck::check_union;
//! use crate::typeck::join_types;`，Foundation ↔ Kernel 跨层调用。
//!
//! ## 设计
//!
//! 这两个 fn 本质是 HM 双向定型（bidirectional）的内部 helper：
//!   - `check_union` —— 双向 check 模式核心，验证 actual subtype Union(members)
//!   - `join_types` —— 双向 check 模式 Phase E，合并多 arm body 类型到 Union
//!
//! 抽离到 `crate::typeck::hm::util` 子模块——保持 HM 内部 API，
//! Foundation (`typeck::mod.rs`) 不再持有这两个 fn。
//!
//! ## 收益
//!
//! - `hm::unify.rs` 测试反向引用 Foundation 的问题消除（Kernel 内部）
//! - HM 内部 helper 集中管理，未来 typeck 重构只动 util 子模块
//! - 双向路径 `bidirectional.rs::use crate::typeck::join_types` 改路径
//!   `use crate::typeck::hm::join_types`（语义清晰）
//!
//! ## 不变的
//!
//! - `check_union` / `join_types` 签名 / 行为完全保持
//! - `Result<(), String>` 返回类型（保留 v0.91 错误统一计划前的现状）
//! - 4 个 `check_union_*` 测试 + 1 个 `join_types_*` 测试语义保留

use crate::common::Span;
use crate::typeck::Type;

/// v0.75.86: Union 双向 check helper —— 为双向定型 check 模式服务。
///
/// 复用 [`Type::subtype_of`] 的 Union arm（保守方向：任一成员 subtype 即 OK），
/// 不引入新规则。当 check 失败时，收集**第一个失败成员**的 expected/actual
/// 配对作为 [`crate::typeck::TypeError`]，供 LSP `publishDiagnostics` 结构化输出。
///
/// 与 [`Type::compatible_with`] 的语义差异：
///   - `compatible_with(Union(m), T)`: 任一 m compatible T
///   - `check_union(actual, expected_union)`: 任一 m subtype T —— `subtype_of`
///     比 `compatible_with` 严格（**非对称**）
///
/// 行号/列号由调用方在 Span 上下文填入（典型用法见
/// `check_mir.rs::hm_to_external` 的 TypeError 构造）。
///
/// 典型用法（双向 check 模式）：
/// ```ignore
/// match expected {
///     Type::Union(_) => check_union(&actual, expected)?,
///     _ if actual.subtype_of(expected) => Ok(()),
///     _ => Err("mismatch".to_string()),
/// }
/// ```
///
/// 错误字符串格式：`"expected subtype of <member>, got <actual>"`，
/// 内部用 [`Type`] 的 `Debug` 输出（v0.13 Display impl 仅是设计目标，
/// 实际未实现；Debug 覆盖所有 variant 字段）。
/// 调用方可自行包装为 [`crate::typeck::TypeError`]（行号/列号由调用方注入）。
pub fn check_union(actual: &Type, expected: &Type) -> Result<(), String> {
    let Type::Union(members) = expected else {
        // 设计错误：调用方应在 dispatch 之前 match expected 是 Union。
        // 返回描述性错误而非 panic——上层 match 错误会自己 panic。
        return Err(format!(
            "check_union misuse: expected must be Union, got {:?}",
            expected
        ));
    };
    if members.is_empty() {
        // 空 Union = "any element type" 占位 —— 兼容任何
        return Ok(());
    }
    // 任一成员 subtype 即可；第一个失败成员用于诊断
    for m in members {
        if actual.subtype_of(m) {
            return Ok(());
        }
    }
    Err(format!(
        "expected subtype of `{:?}`, got `{:?}`",
        members[0], actual,
    ))
}

/// v0.75.86: 把多条 arm body 类型合并为单一 Union（双向 Phase E）。
///
/// 设计目标（v0.75.86 起）：
///   - `arms`：每个元素 `(arm_span, arm_body_inferred_type)`。
///     arm_span 来自 `MirWitness::span`（Phase E 错误定位用）
///   - `outer_span`：整个 Match 节点的 span（fallback——当无 arm span 时用）
pub fn join_types(arms: &[(Span, Type)], outer_span: Span) -> Type {
    let _ = outer_span; // 保留接口参数，Phase E+ 用于外层 fallback
    if arms.is_empty() {
        return Type::Union(vec![]);
    }
    let mut flat: Vec<Type> = Vec::new();
    let mut nested_only = true; // 整个输入是否仅由 Union 输入（保留 Union 形态）
    for (_span, t) in arms {
        match t {
            // Any 短路：top type 吞掉所有
            Type::Any => return Type::Union(vec![]),
            // v0.75.92: Unknown fail-fast — 不参与 join（Unknown 不是合法类型）
            Type::Unknown => return Type::Unknown,
            // 嵌套 Union 递归平展：Union(Union(a, b), c) → [a, b, c]
            Type::Union(members) => {
                for m in members {
                    match m {
                        // 递归：再内层的 Union 也展开
                        Type::Union(inner) => flat.extend(inner.iter().cloned()),
                        _ => flat.push(m.clone()),
                    }
                }
            }
            _ => {
                nested_only = false;
                flat.push(t.clone());
            }
        }
    }
    if flat.is_empty() {
        Type::Union(vec![])
    } else if flat.len() == 1 && !nested_only {
        // 单成员退化：join_types(&[Int]) → Int（不是 Union(vec![Int])）
        flat.pop().unwrap()
    } else {
        // 多成员 OR 整个输入是单 Union（平展后仍为 Union 形态）
        Type::Union(flat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::Literal;

    // ─── check_union 测试（v0.75.86 起在 unify.rs，v0.75.96 迁到 util.rs） ───

    #[test]
    fn check_union_empty_matches_anything() {
        // 空 Union = any element type 占位 —— 兼容任何 actual
        assert!(check_union(&Type::Int, &Type::Union(vec![])).is_ok());
        assert!(check_union(&Type::String, &Type::Union(vec![])).is_ok());
    }

    #[test]
    fn check_union_member_matches() {
        // Union[String, Int] —— Int 是成员之一
        let union_ty = Type::Union(vec![Type::String, Type::Int]);
        assert!(check_union(&Type::Int, &union_ty).is_ok());
        assert!(check_union(&Type::String, &union_ty).is_ok());
    }

    #[test]
    fn check_union_member_subtype() {
        // Union[Float, Int] —— Float subtype Float OK
        let union_ty = Type::Union(vec![Type::Float, Type::Int]);
        let int_val = Type::Int;
        assert!(check_union(&int_val, &union_ty).is_ok());
    }

    #[test]
    fn check_union_no_member_matches_returns_err_msg() {
        let union_ty = Type::Union(vec![Type::Int, Type::String]);
        let err = check_union(&Type::Float, &union_ty).unwrap_err();
        assert!(err.contains("expected subtype of"));
        assert!(err.contains("got"));
    }

    #[test]
    fn check_union_picks_first_failing_member() {
        let union_ty = Type::Union(vec![Type::Int, Type::String]);
        let err = check_union(&Type::Float, &union_ty).unwrap_err();
        // 第一个 failing member 是 Int（Float subtype Float OK 但 Float <: Int 失败）
        assert!(err.contains("Int"));
    }

    #[test]
    fn check_union_misuse_returns_err_when_expected_not_union() {
        let err = check_union(&Type::Int, &Type::Int).unwrap_err();
        assert!(err.contains("check_union misuse"));
    }

    // ─── join_types 测试 ───

    #[test]
    fn join_types_empty_returns_empty_union() {
        let result = join_types(&[] as &[(Span, Type)], Span::default());
        assert_eq!(result, Type::Union(vec![]));
    }

    #[test]
    fn join_types_single_non_union_collapses() {
        // 单非 Union 元素 → 直接返回该类型（不退化为 Union(vec![T])）
        let result = join_types(&[(Span::default(), Type::Int)], Span::default());
        assert_eq!(result, Type::Int);
    }

    #[test]
    fn join_types_multiple_non_union_builds_union() {
        let result = join_types(
            &[
                (Span::default(), Type::Int),
                (Span::default(), Type::String),
            ],
            Span::default(),
        );
        assert_eq!(result, Type::Union(vec![Type::Int, Type::String]));
    }

    #[test]
    fn join_types_single_union_preserves_union_shape() {
        // 单 Union 输入 → 平展后仍为 Union 形态（保留 nested_only）
        let result = join_types(
            &[(Span::default(), Type::Union(vec![Type::Int, Type::String]))],
            Span::default(),
        );
        assert_eq!(result, Type::Union(vec![Type::Int, Type::String]));
    }

    #[test]
    fn join_types_any_short_circuits() {
        let result = join_types(
            &[
                (Span::default(), Type::Int),
                (Span::default(), Type::Any),
                (Span::default(), Type::String),
            ],
            Span::default(),
        );
        assert_eq!(result, Type::Union(vec![]));
    }

    #[test]
    fn join_types_unknown_short_circuits() {
        // v0.75.92: Unknown fail-fast — 不参与 join（Unknown 不是合法类型）
        let result = join_types(
            &[
                (Span::default(), Type::Int),
                (Span::default(), Type::Unknown),
                (Span::default(), Type::String),
            ],
            Span::default(),
        );
        assert_eq!(result, Type::Unknown);
    }

    #[test]
    fn join_types_flatten_nested_unions() {
        // Union(Union(a, b), c) → Union(a, b, c) —— 平展
        let nested = Type::Union(vec![
            Type::Union(vec![Type::Int, Type::Float]),
            Type::String,
        ]);
        let result = join_types(&[(Span::default(), nested)], Span::default());
        assert_eq!(
            result,
            Type::Union(vec![Type::Int, Type::Float, Type::String])
        );
    }

    #[test]
    fn join_types_literal_int_propagation() {
        // sanity: Type::Int 走 default 分支，与字面量值无关（仅类型层处理）
        let _ = Literal::Int(42, Span::default());
        let result = join_types(&[(Span::default(), Type::Int)], Span::default());
        assert_eq!(result, Type::Int);
    }
}
