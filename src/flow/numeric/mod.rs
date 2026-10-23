//! v0.104.6 D468：Mora 数值塔。
//!
//! ## 数值塔
//!
//! Mora 有三个数值域，构成一个**带提升规则**的塔：
//!
//! ```text
//!        BigInt          任意精度
//!          │  提升（精确）
//!        Int             i64
//!          │  提升（有损 ⇒ 需守卫）
//!        Float           f64
//! ```
//!
//! ## 设计原则：每个域拥有自己的算术
//!
//! **不变量（v0.104.6 D468 起）**：
//!
//! 1. 每个域的运算**只经由本域的实现**（[`int`] / [`float`] / [`bigint`]），
//!    域之间**不共享**运算实现。
//! 2. 跨塔混算**必须**先经 [`guard`] 判定可表示性；
//!    装不下就**报错**，不静默降级。
//! 3. 同一个语义（加/减/乘/比较）在所有域里**行为一致**，
//!    只在**精度**上不同（精度由该域的表示能力决定）。
//!
//! ## 修复的缺陷
//!
//! D468 之前，`Int` 的算术在同一文件里**分叉成两条路**：
//!
//! | 运算 | 旧路径 | 问题 |
//! |---|---|---|
//! | `Add` | 裸 `a + b` | 溢出时 debug panic / release 静默回绕（D299） |
//! | `Sub`/`Mul` | f64 round-trip | **2^53 边界静默丢最低位** |
//! | `Int > Int` | f64 比较 | **`a > b` 在 a=2^53+1 时给出 false** |
//!
//! 实测（`a = 2^53 + 1`）：`a + 0i` 得 `...993` 而 `a - 0i` 得 `...992`
//! —— 同一个值，减法把它改了。
//!
//! 根因是**结构性的**：`numeric_op` 的回调类型是
//! `Fn(f64, f64) -> f64`，**从签名上就不允许** Int 走精确路径。
//! 本模块用「域 → 各自实现」的结构取代那个共享回调。
//!
//! ## 公共 API
//!
//! - [`eval`] —— 标量数值算术（按操作数类型分派）
//! - [`compare`] —— 数值比较（返回 `Ordering`，跨塔精确）
//! - [`equal`] —— 数值相等（跨塔精确）

pub mod bigint;
pub mod float;
pub mod guard;
pub mod int;

use crate::common::BinaryOp;
use crate::error::MoraError;
use crate::value::Value;
// `Value::BigInt` 变体与 `num_bigint::BigInt` 类型同名 ——
// 用别名区分，避免在 match 臂里写 `BigNum::from(...)` 这种噪音。
use num_bigint::BigInt as BigNum;

/// 数值比较的结果。`None` 表示 `NaN` 参与比较（IEEE：一切皆 false）。
pub type Ord = Option<std::cmp::Ordering>;

/// 标量数值算术：按操作数类型分派到对应域。
///
/// **不**处理字符串拼接、列表广播等非数值语义 —— 那些由
/// `flow::eval_binary` 在调用本函数**之前**分流。
///
/// 跨塔混算的规则：
///
/// | 左 | 右 | 结果 | 精确性 |
/// |---|---|---|---|
/// | Int | Int | Int | checked i64，溢出报错 |
/// | Int | Float | Float | Int 侧须装得下 f64，否则报错 |
/// | Int | BigInt | BigInt | 精确（i64 → BigInt 无损） |
/// | Float | Float | Float | IEEE 754 |
/// | Float | Int | Float | 同 `Int ⊗ Float` |
/// | Float | BigInt | **见下** | 整数值 float 提升为 BigInt（D198）；否则 Float，BigInt 侧须装得下 f64 |
/// | BigInt | Int | BigInt | 精确 |
/// | BigInt | Float | **见下** | 同 `Float ⊗ BigInt` |
/// | BigInt | BigInt | BigInt | 任意精度 |
///
/// ⚠ **`Float ⊗ BigInt` 的结果类型是「两段」的**（D198 的「最小惊讶」规则，
/// D468 统一）：Float 侧若是**整数值** ⇒ 提升为 BigInt，结果是 **BigInt**
/// （于是 `3.0 / 2n` = `3n / 2n` = `1n` —— 整除，与 BigInt 域一致）；
/// Float 侧带小数 ⇒ 结果只能是 **Float**（`1.5 + 1n` = `2.5`）。
///
/// ⇒ 判据是**操作数的值**（整数值与否），**不是字面量的写法** ——
/// `3.0` 与 `3n` 是同一个值，故它们与 `2n` 混算的结果类型必然相同。
/// 这也意味着**运算符不再改变结果类型**（D468 之前 `Float ⊗ BigInt` 的
/// `Div` 走 f64 round-trip、`Add` 走提升，同一组操作数因运算符而类型不同）。
pub fn eval(op: &BinaryOp, left: &Value, right: &Value) -> Result<Value, MoraError> {
    use Value::*;
    match (left, right) {
        // ── Int 域（精确 checked i64）──
        (Int(a), Int(b)) => int::arith(op, *a, *b).map(Int),

        // ── Float 域（IEEE 754）──
        (Float(a), Float(b)) => float::arith(op, *a, *b).map(Float),

        // ── BigInt 域（任意精度）──
        (BigInt(a), BigInt(b)) => bigint::arith(op, a, b).map(BigInt),
        (Int(a), BigInt(b)) => bigint::arith(op, &BigNum::from(*a), b).map(BigInt),
        (BigInt(a), Int(b)) => bigint::arith(op, a, &BigNum::from(*b)).map(BigInt),

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

        // ── 跨塔：Float ⊗ BigInt ──
        // Float 侧若带小数，BigInt 侧必须**无损**装得下 f64，
        // 否则结果无法精确表示 ⇒ 报错（D359）。
        (Float(a), BigInt(b)) => match coerce_float_bigint(*a, b)? {
            Coerced::Both(fb, ib) => bigint::arith(op, &fb, &ib).map(BigInt),
            Coerced::F64(x, y) => float::arith(op, x, y).map(Float),
        },
        (BigInt(a), Float(b)) => match coerce_float_bigint(*b, a)? {
            Coerced::Both(fb, ib) => bigint::arith(op, &ib, &fb).map(BigInt),
            Coerced::F64(x, y) => float::arith(op, y, x).map(Float),
        },

        _ => Err(MoraError::Other("Operands must be numbers".to_string())),
    }
}

/// 跨塔提升的结果（D198/D359）。
enum Coerced {
    /// 提升为 `(BigInt, BigInt)` —— **精确**。
    Both(BigNum, BigNum),
    /// 保持 `(f64, f64)` —— **确实无损**。
    F64(f64, f64),
}

/// `Float ⊗ BigInt` 的提升策略。
///
/// 规则（**绝不静默给错数**）：
/// 1. float 是**整数值** → 提升为 `(BigInt, BigInt)`，**精确**；
/// 2. float 带小数 → 结果只能是 Float，那 BigInt 侧必须**无损**装得下 f64；
/// 3. 否则 → **报错**（「大整数 + 小数」在当前类型系统里没有精确表示）。
///
/// 判据与 [`guard`] 收拢的可表示性判据同源。
fn coerce_float_bigint(f: f64, b: &BigNum) -> Result<Coerced, MoraError> {
    // 1. 整数值 float：能装下就提升为 BigInt（顺带修回类型退化）。
    if guard::float_fits_bigint_exactly(f) {
        return Ok(Coerced::Both(BigNum::from(f as i64), b.clone()));
    }
    // 2. 整数值 float 但装不下（|f| >= 2^63）⇒ 无精确表示。
    if f.is_finite() && f.fract() == 0.0 {
        return Err(guard::err_float_vs_bigint(f, b));
    }
    // 3. 带小数：BigInt 侧必须无损装得下 f64。
    if !guard::bigint_fits_float_exactly(b) {
        return Err(MoraError::Other(format!(
            "bigint({b}) 无法用 f64 无损表示，与 float({f}) 混合运算会产生\
             静默的数值损坏。\n\
             建议：整数运算请用 bigint 字面量（`<整数>n`）。"
        )));
    }
    Ok(Coerced::F64(f, bigint::to_f64_lossy(b)))
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

        // BigInt ⊗ BigInt：精确（D199 修：此前经 f64，
        // 两个相差 1 的大整数比较给出 false）。
        (BigInt(a), BigInt(b)) => Ok(Some(bigint::cmp(a, b))),

        // Int ⊗ BigInt：提升为 BigInt，精确
        (Int(a), BigInt(b)) => Ok(Some(bigint::cmp(&BigNum::from(*a), b))),
        (BigInt(a), Int(b)) => Ok(Some(bigint::cmp(a, &BigNum::from(*b)))),

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

        // Float ⊗ BigInt：Float 侧须能精确表示，否则报错（D199/D359）。
        (Float(a), BigInt(b)) => {
            if guard::float_fits_bigint_exactly(*a) {
                // Float 是整数值且装得下 ⇒ 提升为 BigInt 精确比较。
                return Ok(Some(bigint::cmp(&BigNum::from(*a as i128), b)));
            }
            // Float **带小数** ⇒ 不能提升为 BigInt（`4.5 as i128` 会截断成 4，
            // 比较的序随之出错）。此时退到 f64 比较 —— BigInt 侧必须**无损**
            // 装得下 f64（否则 f64 舍入会给出错的序 ⇒ 报错，不静默给错数）。
            //
            // v0.104.6 D472 修：此前此处**一律**走「float 超出 f64 精确表示
            // 范围」的报错分支 ⇒ `4n < 4.5` 这种**带小数**的合法比较也被拒，
            // 且错误信息驴唇不对马嘴（4.5 不是整数，与「2^53 以内的整数」无关）。
            if !guard::bigint_fits_float_exactly(b) {
                return Err(guard::err_float_vs_bigint(*a, b));
            }
            Ok(float::cmp(*a, bigint::to_f64_lossy(b)))
        }
        (BigInt(a), Float(b)) => {
            if guard::float_fits_bigint_exactly(*b) {
                return Ok(Some(bigint::cmp(a, &BigNum::from(*b as i128))));
            }
            // 同上：带小数的 float 退到 f64 比较。
            if !guard::bigint_fits_float_exactly(a) {
                return Err(guard::err_float_vs_bigint(*b, a));
            }
            Ok(float::cmp(bigint::to_f64_lossy(a), *b))
        }

        _ => Err(MoraError::Other("Operands must be numbers".to_string())),
    }
}

/// 数值相等：跨塔**精确**。
///
/// 与 [`compare`] 的区别：相等**不**因精度问题报错，而是
/// **判不相等**（返回 `false`）。
///
/// 理由：相等是**布尔谓词**，它的契约是「给一个答案」；
/// 而 `compare` 是**关系**，它的契约是「给出正确的序」。
/// 一个装不下的 Int 与一个 Float 不相等 —— 这是**正确答案**，
/// 不是错误。（`compare` 在同样情形下报错，是为了让
/// 「`a >= b` 与 `a == b` 必须自洽」这一不变式成立 —— 见下。）
///
/// # 自洽性不变式
///
/// `a >= b` 与 `a == b` 必须给出相容的答案。旧实现里
/// `a > b` 为 false 而 `a >= b` 为 true（f64 舍入导致），
/// 正是这个不变式被破坏的表现。
///
/// 本实现保证：
/// - `compare` 成功 ⇒ `==` 与 `<=`/`<`/`>`/`>=` 自洽；
/// - `compare` 失败（装不下）⇒ `==` 判 false，且顺序比较**报错**
///   —— 而不是给出可能自相矛盾的答案。
pub fn equal(left: &Value, right: &Value) -> bool {
    use Value::*;
    match (left, right) {
        (Int(a), Int(b)) => a == b,
        (Float(a), Float(b)) => a == b,
        (BigInt(a), BigInt(b)) => a == b,
        (Int(a), BigInt(b)) => BigNum::from(*a) == *b,
        (BigInt(a), Int(b)) => *a == BigNum::from(*b),
        // Int ⊗ Float：Int 装不下 f64 ⇒ 必然不相等（D468）。
        (Int(a), Float(b)) => guard::int_fits_float_exactly(*a) && *a as f64 == *b,
        (Float(a), Int(b)) => guard::int_fits_float_exactly(*b) && *a == *b as f64,
        // Float ⊗ BigInt：Float 带小数 ⇒ 不可能相等（D199）。
        // Float 是整数值且装得下 ⇒ 提升为 BigInt 精确比较。
        (Float(a), BigInt(b)) => {
            // 「带小数」的判据在 **Float 侧**（`a`），不是 BigInt 侧。
            if a.fract() != 0.0 {
                return false;
            }
            if guard::float_fits_bigint_exactly(*a) {
                BigNum::from(*a as i128) == *b
            } else {
                // 极端量级（|f| >= 2^63）双方都远超 i128 ⇒ 退回 lossy。
                bigint::to_f64_lossy(b) == *a
            }
        }
        (BigInt(a), Float(b)) => {
            if b.fract() != 0.0 {
                return false;
            }
            if guard::float_fits_bigint_exactly(*b) {
                *a == BigNum::from(*b as i128)
            } else {
                bigint::to_f64_lossy(a) == *b
            }
        }
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
    fn int_bigint_mix_is_exact() {
        let a = i(P53P1);
        let b = Value::BigInt(BigNum::from(1u64));
        // 提升为 BigInt ⇒ 精确
        assert_eq!(
            eval(&BinaryOp::Add, &a, &b).unwrap(),
            Value::BigInt(BigNum::from(P53P1) + BigNum::from(1u64))
        );
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
