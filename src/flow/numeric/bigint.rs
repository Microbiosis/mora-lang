//! v0.104.6 D468：BigInt 数值域（任意精度）。
//!
//! ## 语义
//!
//! `num-bigint` 的原生运算，**不经 f64**。D21 之前是「转 f64 再转回 i64」
//! 的饱和路径，大数直接被截断；现在走原生大数运算。
//!
//! ## 与 Int 域的边界
//!
//! `Int ⊗ BigInt` 的混算**统一提升为 BigInt**（最小惊讶原则），
//! 提升本身是**精确**的（i64 → BigInt 无损）。
//! BigInt 与 Float 的混算由 [`crate::flow::numeric::guard`] 把关
//! （Float 侧必须能精确表示，否则报错）。

use crate::common::BinaryOp;
use crate::error::MoraError;
use num_bigint::BigInt;
use num_traits::ToPrimitive;

/// BigInt 域的算术（任意精度）。
pub fn arith(op: &BinaryOp, a: &BigInt, b: &BigInt) -> Result<BigInt, MoraError> {
    let r = match op {
        BinaryOp::Add => a + b,
        BinaryOp::Sub => a - b,
        BinaryOp::Mul => a * b,
        BinaryOp::Div => {
            if b == &BigInt::from(0u8) {
                return Err(MoraError::Other("division by zero".to_string()));
            }
            a / b
        }
        BinaryOp::Mod => {
            if b == &BigInt::from(0u8) {
                return Err(MoraError::Other("modulo by zero".to_string()));
            }
            a % b
        }
        _ => {
            return Err(MoraError::Other(format!(
                "unsupported bigint operation: {op:?}"
            )));
        }
    };
    Ok(r)
}

/// BigInt 域的比较（精确，不经 f64）。
///
/// D199 之前是 `bigint_to_f64_lossy(a) vs bigint_to_f64_lossy(b)`，
/// 于是两个相差 1 的大整数舍入到同一个 f64 后 `>` 给出 **false**。
pub fn cmp(a: &BigInt, b: &BigInt) -> std::cmp::Ordering {
    a.cmp(b)
}

/// BigInt → f64 的**有损**转换（仅用于类型提升）。
///
/// 超出 f64 范围返回 `±INFINITY`。调用方**必须**先经
/// [`crate::flow::numeric::guard`] 判定可表示性。
pub fn to_f64_lossy(n: &BigInt) -> f64 {
    use num_traits::ToPrimitive;
    n.to_f64().unwrap_or(f64::INFINITY)
}

/// BigInt → i64 的转换（当且仅当放得下）。
pub fn to_i64(n: &BigInt) -> Result<i64, MoraError> {
    n.to_i64()
        .ok_or_else(|| MoraError::Other(format!("bigint({n}) 超出 i64 范围，无法转换为 int。")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arith_is_exact_beyond_f64() {
        let a = BigInt::from(10u64).pow(30);
        let b = BigInt::from(1u64);
        assert_eq!(arith(&BinaryOp::Add, &a, &b).unwrap(), &a + &b);
        assert_eq!(arith(&BinaryOp::Sub, &a, &b).unwrap(), &a - &b);
    }

    #[test]
    fn cmp_is_exact_beyond_f64() {
        // D199 的成例：两个相差 1 的大整数，旧实现经 f64 后比较给出 false。
        let b = BigInt::from(10u64).pow(30);
        let b_plus_1 = &b + BigInt::from(1u64);
        assert_eq!(cmp(&b_plus_1, &b), std::cmp::Ordering::Greater);
    }

    #[test]
    fn div_by_zero_is_error_not_panic() {
        // D319 的成例：num-bigint 对零除数是 panic，不是 Err。
        let a = BigInt::from(7u64);
        let z = BigInt::from(0u64);
        assert!(arith(&BinaryOp::Div, &a, &z).is_err());
        assert!(arith(&BinaryOp::Mod, &a, &z).is_err());
    }
}
