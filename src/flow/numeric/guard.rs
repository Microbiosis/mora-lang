//! v0.104.6 D468：跨塔运算的**可表示性守卫**。
//!
//! ## 为什么单独成模块
//!
//! 数值塔的三个域（`Int`/`Float`/`BigInt`）各有自己的算术，
//! 但**跨塔混算**是另一个问题：一个值要参与另一种类型的运算，
//! 必须先确认它**装得下**，否则转换会静默丢位。
//!
//! 本模块把这四道守卫**收拢到一处**（此前它们散落在 `flow.rs` 的
//! 各个 match 臂里，同一对操作数在不同运算符下行为不一致）：
//!
//! | 守卫 | 方向 | 由来 | 此前散落位置 |
//! |---|---|---|---|
//! | [`float_fits_int_exactly`] | Float → 整数 | D359 | `flow.rs` `check_float_exact_int` |
//! | [`int_fits_float_exactly`] | Int → Float | D468 | `flow.rs` `check_int_exact_in_f64` |
//! | [`bigint_fits_float_exactly`] | BigInt → Float | D359 | `flow.rs` `b_f64_check` |
//! | [`float_fits_bigint_exactly`] | Float → BigInt | D359 | `flow.rs` `coerce_mixed` |
//!
//! ## 核心原则
//!
//! **宁可报错，不返回一个错的数。**
//!
//! 这不是保守，是**可用性**要求：一个静默给错数的语言，
//! 用户无法定位问题；而一个明确报错的语言，用户立刻知道该改哪里。

use crate::error::MoraError;
use num_bigint::BigInt;

/// Float 是否能**精确**参与整数运算？
///
/// 判据：有限、是整数值、且 `|f| <= 2^53`（f64 的尾数是 53 位）。
///
/// 用于 `Float ⊗ BigInt` 的 Float 侧（D359）。
pub fn float_fits_int_exactly(f: f64) -> bool {
    f.is_finite() && f.fract() == 0.0 && f.abs() <= super::float::EXACT_INT_LIMIT
}

/// Int 是否能**精确**参与 Float 运算？
///
/// 判据：`i64 → f64 → i64` 往返相等。
///
/// 用于 `Int ⊗ Float` 的 Int 侧（D468）。2^53 以内恒真。
pub fn int_fits_float_exactly(i: i64) -> bool {
    super::int::fits_f64_exactly(i)
}

/// BigInt 是否能**精确**参与 Float 运算？
///
/// 判据：转 f64 后能**无损**转回 BigInt。
///
/// 用于 `BigInt ⊗ Float` 的 BigInt 侧（D359）。
pub fn bigint_fits_float_exactly(b: &BigInt) -> bool {
    let f = super::bigint::to_f64_lossy(b);
    f.is_finite() && f.fract() == 0.0 && BigInt::from(f as i128) == *b
}

/// Float 是否能**精确**参与 BigInt 运算？
///
/// 判据：有限、是整数值、且 `|f| < 2^127`（`BigInt::from(f as i128)`
/// 的可表示范围）。
///
/// 用于 `Float ⊗ BigInt` 的 Float 侧（D359）。
pub fn float_fits_bigint_exactly(f: f64) -> bool {
    f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e18
}

/// `Float ⊗ BigInt`：Float 侧装不下 BigInt 时的统一错误。
pub fn err_float_vs_bigint(f: f64, b: &BigInt) -> MoraError {
    MoraError::Other(format!(
        "float({f}) 超出 f64 的精确表示范围（f64 只能精确表示 2^53 以内的整数），\
         与 bigint({b}) 混合运算会产生静默的数值损坏。\n\
         建议：整数运算请用 bigint 字面量（`<整数>n`）。"
    ))
}

/// `Int ⊗ Float`：Int 侧装不下 f64 时的统一错误。
pub fn err_int_vs_float(i: i64) -> MoraError {
    MoraError::Other(format!(
        "int({i}) 超出 f64 的精确表示范围（f64 只能精确表示 2^53 以内的整数），\
         与 float 混合运算会产生静默的数值损坏。\n\
         建议：需要超出该范围的整数运算时，请使用 bigint 字面量（`<整数>n`）。"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_boundary_at_2p53() {
        assert!(float_fits_int_exactly(9_007_199_254_740_992.0)); // 2^53
        assert!(!float_fits_int_exactly(1.0e20));
        assert!(!float_fits_int_exactly(1.5)); // 有小数
        assert!(!float_fits_int_exactly(f64::INFINITY));
    }

    #[test]
    fn int_boundary_at_2p53() {
        assert!(int_fits_float_exactly(9_007_199_254_740_992i64)); // 2^53
        assert!(!int_fits_float_exactly(9_007_199_254_740_993i64)); // 2^53 + 1
    }

    #[test]
    fn bigint_roundtrip_check() {
        let small = BigInt::from(42u64);
        assert!(bigint_fits_float_exactly(&small));
        let huge = BigInt::from(10u64).pow(30);
        assert!(!bigint_fits_float_exactly(&huge));
    }
}
