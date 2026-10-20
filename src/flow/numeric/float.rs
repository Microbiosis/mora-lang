//! v0.104.6 D468：f64 数值域。
//!
//! ## 语义
//!
//! 纯 IEEE 754 双精度：除零给 `inf`/`NaN`，与 Rust 的 f64 运算一致。
//! **不做**任何精度包装 —— Float 本来就是近似数，包装反而会让
//! 「Float 就是 Float」这件事变得不可预期。
//!
//! ## 与 Int 域的边界
//!
//! Float 域**不负责**判定「Int 能否精确参与 Float 运算」——那是
//! [`crate::flow::numeric::guard`] 的职责，域本身只管算。
//! 这样做是为了让「算术」与「准入」两件事各自独立、可分别测试。

use crate::common::BinaryOp;
use crate::error::MoraError;

/// Float 域的算术（IEEE 754 双精度）。
///
/// `Add`/`Sub`/`Mul`/`Div`/`Mod` 全部是纯 f64 运算：
/// - 除零 → `±inf`（`0.0/0.0` → `NaN`），与 IEEE 一致；
/// - `Mod` 用 `a - (a/b).trunc() * b`，与 Rust 的 `%` 语义一致
///   （符号随被除数），且避免 `f64::rem` 的行为差异。
pub fn arith(op: &BinaryOp, a: f64, b: f64) -> Result<f64, MoraError> {
    let r = match op {
        BinaryOp::Add => a + b,
        BinaryOp::Sub => a - b,
        BinaryOp::Mul => a * b,
        BinaryOp::Div => a / b,
        BinaryOp::Mod => {
            if b == 0.0 {
                f64::NAN
            } else {
                a - (a / b).trunc() * b
            }
        }
        _ => {
            return Err(MoraError::Other(format!(
                "unsupported float operation: {op:?}"
            )));
        }
    };
    Ok(r)
}

/// Float 域的比较（IEEE 754）。
///
/// `NaN` 参与比较时返回 `None` —— 调用方按「NaN 与任何值都不相等、
/// 也不大于、也不小于」处理（四种比较一律 false）。
pub fn cmp(a: f64, b: f64) -> Option<std::cmp::Ordering> {
    a.partial_cmp(&b)
}

/// f64 能**精确**表示的整数上限：`2^53`。
pub const EXACT_INT_LIMIT: f64 = 9_007_199_254_740_992.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn div_by_zero_gives_infinity() {
        assert_eq!(arith(&BinaryOp::Div, 1.0, 0.0).unwrap(), f64::INFINITY);
        assert_eq!(arith(&BinaryOp::Div, -1.0, 0.0).unwrap(), f64::NEG_INFINITY);
        assert!(arith(&BinaryOp::Div, 0.0, 0.0).unwrap().is_nan());
    }

    #[test]
    fn mod_sign_follows_dividend() {
        assert_eq!(arith(&BinaryOp::Mod, 7.0, 3.0).unwrap(), 1.0);
        assert_eq!(arith(&BinaryOp::Mod, -7.0, 3.0).unwrap(), -1.0);
    }

    #[test]
    fn nan_cmp_is_none() {
        assert!(cmp(f64::NAN, 1.0).is_none());
        assert!(cmp(1.0, f64::NAN).is_none());
    }
}
