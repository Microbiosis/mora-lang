//! v0.104.6 D468：精确 i64 数值域。
//!
//! ## 为什么单独成域
//!
//! Mora 的数值塔有三个域：`Int`(i64) / `Float`(f64) / `BigInt`(任意精度)。
//! **每个域必须自己拥有自己的算术实现** —— 共享一条 f64 通道会让 `Int`
//! 静默丢精度（f64 只有 53 位尾数，装不下 i64 的全部取值）。
//!
//! ## 修复的具体缺陷
//!
//! v0.104.6 D468 之前，`Int` 的算术在同一个文件里**分叉成两条路**：
//!
//! | 运算 | 旧路径 | 精度 |
//! |---|---|---|
//! | `Add` | 裸 `a + b`（i64） | 精确，但溢出时 debug panic / release 静默回绕 |
//! | `Sub`/`Mul` | `numeric_op` 的 f64 round-trip | **不精确**（2^53 边界丢最低位） |
//!
//! 实测（`a = 2^53 + 1`）：
//!
//! ```text
//! a + 0i   →  9007199254740993   ✅
//! a - 0i   →  9007199254740992   ❌ 同一个值，减法把它改了
//! ```
//!
//! 本模块给出**唯一**的 Int 算术实现：checked i64，溢出报 `MoraError`。
//!
//! ## 不变量
//!
//! - 本模块的每个函数对**任意** i64 输入都给出确定结果；
//! - 溢出**绝不**静默（不 panic、不回绕、不饱和）—— 一律 `Err`；
//! - 除法/取模向零截断（与 Rust 的 i64 `/` `%` 一致），除零报错。

use crate::common::BinaryOp;
use crate::error::MoraError;

/// i64 溢出的统一错误构造。
///
/// 覆盖 Add/Sub/Mul/Div/Mod。`Div`/`Mod` 的**除零**不走这里
/// （由 [`div`] / [`rem`] 单独判，且它们先判零再判溢出）。
pub fn overflow(op: &BinaryOp, a: i64, b: i64) -> MoraError {
    MoraError::Other(format!(
        "integer overflow in {op:?}: {a} {op:?} {b}（超出 i64 范围）。\n\
         提示：值超出 i64 范围时，请使用 bigint 字面量（如 `123n`）。"
    ))
}

/// Int 域的精确算术（checked i64）。
///
/// `Div`/`Mod` 的除零在此判（`int_div`/`int_mod` 会在调用前先判零，
/// 但本函数**自身也判**——它是本域的唯一入口，必须自洽）。
///
/// 溢出返回 [`overflow`] 构造的 `MoraError`。
pub fn arith(op: &BinaryOp, a: i64, b: i64) -> Result<i64, MoraError> {
    match op {
        BinaryOp::Add => a.checked_add(b).ok_or_else(|| overflow(op, a, b)),
        BinaryOp::Sub => a.checked_sub(b).ok_or_else(|| overflow(op, a, b)),
        BinaryOp::Mul => a.checked_mul(b).ok_or_else(|| overflow(op, a, b)),
        BinaryOp::Div => div(a, b),
        BinaryOp::Mod => rem(a, b),
        _ => Err(MoraError::Other(format!(
            "unsupported integer operation: {op:?}"
        ))),
    }
}

/// 精确整数除法：向零截断，除零报错，`i64::MIN / -1` 溢出报错。
///
/// 向零截断是 Rust 的 i64 `/` 语义（`-7 / 2 == -3`），
/// 与 BigInt 域的 `num_bigint` 行为一致（D19 修的就是这两者的分叉）。
pub fn div(a: i64, b: i64) -> Result<i64, MoraError> {
    if b == 0 {
        return Err(MoraError::Other("division by zero".to_string()));
    }
    a.checked_div(b)
        .ok_or_else(|| overflow(&BinaryOp::Div, a, b))
}

/// 精确整数取模：符号随被除数，除零报错，`i64::MIN % -1` 溢出报错。
pub fn rem(a: i64, b: i64) -> Result<i64, MoraError> {
    if b == 0 {
        return Err(MoraError::Other("modulo by zero".to_string()));
    }
    a.checked_rem(b)
        .ok_or_else(|| overflow(&BinaryOp::Mod, a, b))
}

/// 精确整数比较（不经 f64）。
///
/// v0.104.6 D468 之前这里是 `ord_f64(a as f64, b as f64)`，于是 2^53 边界上
/// `a > b` 给出 **false**（f64 里两者相等），而 `a >= b` 给出 **true**
/// （判据是 `!= Less`）—— 两个运算符对同一组值给出不自洽的答案。
pub fn cmp(a: i64, b: i64) -> std::cmp::Ordering {
    a.cmp(&b)
}

/// 该 i64 能否被 f64 **精确**表示？
///
/// f64 的尾数是 53 位 ⇒ `|i| > 2^53` 的整数转 f64 会**静默丢位**：
///
/// ```text
/// (2^53 + 1) as f64  ==  2^53
/// ```
///
/// 判据是「往返相等」：`i64 → f64 → i64` 结果不变 ⇔ f64 装得下。
/// 2^53 以内恒真。
///
/// 供 `Float` 域在 Int ⊗ Float 混算前查询（见 `guard` 模块）。
pub fn fits_f64_exactly(i: i64) -> bool {
    (i as f64) as i64 == i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_is_exact_beyond_2p53() {
        // 2^53 + 1 在 f64 里不可表示 —— 旧路径（f64 round-trip）会丢最低位。
        let a = 9_007_199_254_740_993i64; // 2^53 + 1
        let b = 0i64;
        assert_eq!(arith(&BinaryOp::Add, a, b).unwrap(), a);
        assert_eq!(arith(&BinaryOp::Sub, a, b).unwrap(), a);
        // 乘以 1 保持不变（乘以 0 恒为 0，测不出精度问题）。
        assert_eq!(arith(&BinaryOp::Mul, a, 1).unwrap(), a);
    }

    #[test]
    fn sub_and_add_agree_on_identity() {
        // v0.104.6 D468 的核心判据：`a - 0 == a + 0`。
        let a = 9_007_199_254_740_993i64;
        assert_eq!(
            arith(&BinaryOp::Sub, a, 0).unwrap(),
            arith(&BinaryOp::Add, a, 0).unwrap()
        );
    }

    #[test]
    fn overflow_is_error_not_wrap() {
        // D299 记录的旧行为：debug panic / release 静默回绕成 i64::MIN。
        let max = i64::MAX;
        let err = arith(&BinaryOp::Add, max, 1).unwrap_err();
        assert!(err.to_string().contains("overflow"), "{err}");
        // 回绕值不得出现。
        assert!(!err.to_string().contains(&i64::MIN.to_string()));
    }

    #[test]
    fn div_truncates_toward_zero() {
        assert_eq!(div(7, 2).unwrap(), 3);
        assert_eq!(div(-7, 2).unwrap(), -3);
        assert_eq!(div(7, -2).unwrap(), -3);
        assert_eq!(div(-7, -2).unwrap(), 3);
    }

    #[test]
    fn div_by_zero_is_error() {
        assert!(div(1, 0).is_err());
    }

    #[test]
    fn min_div_minus_one_overflows_without_panic() {
        // 旧路径（裸 `a / b`）在 debug 下 panic、release 下回绕。
        let err = div(i64::MIN, -1).unwrap_err();
        assert!(err.to_string().contains("overflow"), "{err}");
    }

    #[test]
    fn rem_sign_follows_dividend() {
        assert_eq!(rem(7, 3).unwrap(), 1);
        assert_eq!(rem(-7, 3).unwrap(), -1);
        assert_eq!(rem(7, -3).unwrap(), 1);
    }

    #[test]
    fn cmp_is_exact_beyond_2p53() {
        let a = 9_007_199_254_740_993i64; // 2^53 + 1
        let b = 9_007_199_254_740_992i64; // 2^53
        assert_eq!(cmp(a, b), std::cmp::Ordering::Greater);
        assert_eq!(cmp(b, a), std::cmp::Ordering::Less);
    }

    #[test]
    fn fits_f64_boundary() {
        assert!(fits_f64_exactly(9_007_199_254_740_992i64)); // 2^53
        assert!(!fits_f64_exactly(9_007_199_254_740_993i64)); // 2^53 + 1
        assert!(fits_f64_exactly(0));
        assert!(fits_f64_exactly(i64::MIN));
    }
}
