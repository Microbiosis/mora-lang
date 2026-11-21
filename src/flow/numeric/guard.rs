//! v0.110.0: 跨塔运算的**可表示性守卫**（`numeric::guard`）。
//!
//! ## 为什么单独成模块
//!
//! 数值塔的两个核心域（`Int`/`Float`）各有自己的算术，
//! 但**跨塔混算**是另一个问题：一个值要参与另一种类型的运算，
//! 必须先确认它**装得下**，否则转换会静默丢位。
//!
//! ## 核心原则
//!
//! **宁可报错，不返回一个错的数。**

use crate::error::MoraError;

/// Float 是否能**精确**参与整数运算？
///
/// 判据：有限、是整数值、且 `|f| <= 2^53`（f64 的尾数是 53 位）。
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

/// `Int ⊗ Float`：Int 侧装不下 f64 时的统一错误。
pub fn err_int_vs_float(i: i64) -> MoraError {
    MoraError::Other(format!(
        "int({i}) 超出 f64 的精确表示范围（f64 只能精确表示 2^53 以内的整数），\
         与 float 混合运算会产生静默的数值损坏。\n\
         建议：需要超出该范围的任意精度整数运算时，请使用 `std/math/bigint` 标准库。"
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
}
