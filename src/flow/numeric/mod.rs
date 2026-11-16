//! v0.104.6 D468：Mora 核心数值塔（Int / Float）。
//!
//! （v0.110.0 起 `BigInt` 已下沉至独立标准库 `std/math/bigint`。）

pub mod float;
pub mod guard;
pub mod int;

use crate::common::BinaryOp;
use crate::error::MoraError;
use crate::value::Value;

/// 数值比较的结果。`None` 表示 `NaN` 参与比较（IEEE：一切皆 false）。
pub type Ord = Option<std::cmp::Ordering>;

/// 标量数值算术：按操作数类型分派到对应域。
pub fn eval(op: &BinaryOp, left: &Value, right: &Value) -> Result<Value, MoraError> {
    use Value::*;
    match (left, right) {
        // ── Int 域（精确 checked i64）──
        (Int(a), Int(b)) => int::arith(op, *a, *b).map(Int),

        // ── Float 域（IEEE 754）──
        (Float(a), Float(b)) => float::arith(op, *a, *b).map(Float),

        // ── 跨塔：Int ⊗ Float ──
        // Int 侧转 f64 会丢位（2^53 外）⇒ 先守卫。
        (Int(a), Float(b)) => {
            if !guard::int_fits_float_exactly(*a) {
                return Err(guard::err_int_vs_float(*a));
            }
            float::arith(op, *a as f64, *b).map(Float)
        }
        (Float(a), Int(b)) => {
            if !guard::int_fits_float_exactly(*b) {
                return Err(guard::err_int_vs_float(*b));
            }
            float::arith(op, *a, *b as f64).map(Float)
        }

        _ => Err(MoraError::Other("Operands must be numbers".to_string())),
    }
}

/// 顺序比较（`>` / `<` / `>=` / `<=`）。
///
/// **判据统一为「先求序，再由序推导关系」** —— 因此四个运算符
/// 对同一组值**必然自洽**：`a > b` 为真 ⇒ `a >= b` 必为真。
///
/// 旧实现里 `a > b` 为 false 而 `a >= b` 为 true（f64 舍入导致），
/// 正是自洽性被破坏的表现。
///
/// `NaN` 参与比较时一律为 false（IEEE 754）。
pub fn eval_order(op: &BinaryOp, left: &Value, right: &Value) -> Result<Value, MoraError> {
    let ord = compare(left, right)?;
    let result = match ord {
        // NaN：四种顺序比较一律为 false（IEEE 754）。
        None => false,
        Some(o) => match op {
            BinaryOp::Greater => o == std::cmp::Ordering::Greater,
            BinaryOp::Less => o == std::cmp::Ordering::Less,
            BinaryOp::GreaterEqual => o != std::cmp::Ordering::Less,
            BinaryOp::LessEqual => o != std::cmp::Ordering::Greater,
            _ => {
                return Err(MoraError::Other(format!(
                    "not an ordering operation: {op:?}"
                )));
            }
        },
    };
    Ok(Value::Bool(result))
}

/// 数值比较：跨塔**精确**。
///
/// 返回 `None` 表示 `NaN` 参与比较（IEEE：与任何值的比较都无序）。
///
/// 与 `eval` 同一套守卫策略：装不下就报错，不静默降级。
pub fn compare(left: &Value, right: &Value) -> Result<Ord, MoraError> {
    use Value::*;
    match (left, right) {
        // Int ⊗ Int：精确 i64 比较（D468 修：此前经 f64，
        // 在 2^53 边界 `a > b` 给出 false）。
        (Int(a), Int(b)) => Ok(Some(int::cmp(*a, *b))),

        // Float ⊗ Float：IEEE
        (Float(a), Float(b)) => Ok(float::cmp(*a, *b)),

        // Int ⊗ Float：Int 侧须装得下 f64，否则报错（D468）。
        (Int(a), Float(b)) => {
            if !guard::int_fits_float_exactly(*a) {
                return Err(guard::err_int_vs_float(*a));
            }
            Ok(float::cmp(*a as f64, *b))
        }
        (Float(a), Int(b)) => {
            if !guard::int_fits_float_exactly(*b) {
                return Err(guard::err_int_vs_float(*b));
            }
            Ok(float::cmp(*a, *b as f64))
        }

        _ => Err(MoraError::Other("Operands must be numbers".to_string())),
    }
}

/// 数值相等：跨塔**精确**。
pub fn equal(left: &Value, right: &Value) -> bool {
    use Value::*;
    match (left, right) {
        (Int(a), Int(b)) => a == b,
        (Float(a), Float(b)) => a == b,
        // Int ⊗ Float：Int 装不下 f64 ⇒ 必然不相等（D468）。
        (Int(a), Float(b)) => guard::int_fits_float_exactly(*a) && *a as f64 == *b,
        (Float(a), Int(b)) => guard::int_fits_float_exactly(*b) && *a == *b as f64,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i(v: i64) -> Value {
        Value::Int(v)
    }
    fn f(v: f64) -> Value {
        Value::Float(v)
    }

    /// 2^53 —— f64 能精确表示的最大整数。
    const P53: i64 = 9_007_199_254_740_992;
    /// 2^53 + 1 —— f64 **装不下**的第一个整数。
    const P53P1: i64 = 9_007_199_254_740_993;

    #[test]
    fn sub_and_add_agree_beyond_2p53() {
        let a = i(P53P1);
        let z = i(0);
        assert_eq!(eval(&BinaryOp::Add, &a, &z).unwrap(), i(P53P1));
        assert_eq!(eval(&BinaryOp::Sub, &a, &z).unwrap(), i(P53P1));
    }

    #[test]
    fn int_gt_is_exact_beyond_2p53() {
        // 旧实现（f64 比较）在这里给出 false。
        let a = i(P53P1);
        let b = i(P53);
        assert_eq!(compare(&a, &b).unwrap(), Some(std::cmp::Ordering::Greater));
        assert_eq!(compare(&b, &a).unwrap(), Some(std::cmp::Ordering::Less));
    }

    #[test]
    fn comparison_operators_are_self_consistent() {
        // 旧实现：`a > b` 为 false 而 `a >= b` 为 true。
        let a = i(P53P1);
        let b = i(P53);
        let gt = compare(&a, &b).unwrap() == Some(std::cmp::Ordering::Greater);
        let ge = compare(&a, &b).unwrap() != Some(std::cmp::Ordering::Less);
        assert!(gt, "a > b 应为真");
        assert!(ge, "a >= b 应为真");
        // 自洽性：a > b ⇒ a >= b，且 !(a == b)
        assert!(!gt || ge);
        assert!(!equal(&a, &b));
    }

    #[test]
    fn int_float_mix_beyond_2p53_is_error_not_wrong_value() {
        let a = i(P53P1);
        let b = f(P53 as f64);
        // 算术：装不下 ⇒ 报错
        assert!(eval(&BinaryOp::Add, &a, &b).is_err());
        // 比较：装不下 ⇒ 报错
        assert!(compare(&a, &b).is_err());
        // 相等：谓词 ⇒ 判 false（正确答案，不是错误）
        assert!(!equal(&a, &b));
    }

    #[test]
    fn int_float_mix_within_2p53_promotes() {
        let a = i(3);
        let b = f(0.5);
        assert_eq!(eval(&BinaryOp::Add, &a, &b).unwrap(), f(3.5));
        assert_eq!(compare(&a, &b).unwrap(), Some(std::cmp::Ordering::Greater));
    }

    #[test]
    fn overflow_is_error() {
        let max = i(i64::MAX);
        let one = i(1);
        assert!(eval(&BinaryOp::Add, &max, &one).is_err());
        assert!(eval(&BinaryOp::Sub, &i(i64::MIN), &one).is_err());
        assert!(eval(&BinaryOp::Mul, &max, &i(2)).is_err());
    }

    #[test]
    fn nan_compare_is_none_and_equal_is_false() {
        let n = f(f64::NAN);
        let o = f(1.0);
        assert!(compare(&n, &o).unwrap().is_none());
        assert!(!equal(&n, &o));
    }
}
