//! v0.91: math.* — 标量数学 builtin（APL/StreamIt 启发）
//!
//! 设计原则：
//! - 函数式 API（`math.sin(x)`）— 与现有 builtin 风格一致
//! - 方法式 API（`x.sin()`）由 `dispatch.rs::call_method_int/float` 直接实现
//!   以避免双实现（参见 dispatch.rs 内对应函数）
//! - 数值参数：Int/Float 接受；结果 Int 输入返回 Int（int 路径），Float 输入返回 Float
//! - 常量以模块属性形式暴露：`math.PI / math.E / math.TAU / math.INF / math.NAN`

use crate::value::Value;
use num_bigint::BigInt;

/// math.* builtin 入口分发
pub fn call_math_method(method: &str, args: &[Value]) -> Result<Value, String> {
    match method {
        // ── 三角函数（Float 强制）──
        "sin" => unary_float(args, f64::sin),
        "cos" => unary_float(args, f64::cos),
        "tan" => unary_float(args, f64::tan),
        "asin" => unary_float(args, f64::asin),
        "acos" => unary_float(args, f64::acos),
        "atan" => unary_float(args, f64::atan),
        "sinh" => unary_float(args, f64::sinh),
        "cosh" => unary_float(args, f64::cosh),
        "tanh" => unary_float(args, f64::tanh),

        // ── 指数/对数（Float 强制）──
        "exp" => unary_float(args, f64::exp),
        "log" => unary_float(args, f64::ln),
        "log2" => unary_float(args, f64::log2),
        "log10" => unary_float(args, f64::log10),
        "log1p" => unary_float(args, f64::ln_1p),
        "sqrt" => unary_float(args, f64::sqrt),
        "cbrt" => unary_float(args, f64::cbrt),

        // ── 双参函数 ──
        "pow" => binary_float(args, f64::powf),
        "hypot" => binary_float(args, f64::hypot),
        "atan2" => binary_float(args, f64::atan2),

        // ── 取整/舍入（保持类型：Int 输入 → Int，Float 输入 → Float）──
        "abs" => unary_preserve(args, |x| x.abs(), |x| x.abs(), BigIntOp::Abs),
        "sign" => unary_preserve(
            args,
            |x| x.signum(),
            |x| {
                if x > 0.0 {
                    1.0
                } else if x < 0.0 {
                    -1.0
                } else {
                    0.0
                }
            },
            BigIntOp::Sign,
        ),
        "floor" => unary_preserve(args, |x| x, |x| x.floor(), BigIntOp::Identity),
        "ceil" => unary_preserve(args, |x| x, |x| x.ceil(), BigIntOp::Identity),
        "round" => unary_preserve(args, |x| x, |x| x.round(), BigIntOp::Identity),
        "trunc" => unary_preserve(args, |x| x, |x| x.trunc(), BigIntOp::Identity),
        "fract" => unary_float(args, f64::fract),

        // ── 模块常量（零参）──
        "PI" => Ok(Value::Float(std::f64::consts::PI)),
        "E" => Ok(Value::Float(std::f64::consts::E)),
        "TAU" => Ok(Value::Float(std::f64::consts::TAU)),
        "INF" => Ok(Value::Float(f64::INFINITY)),
        "NAN" => Ok(Value::Float(f64::NAN)),

        // ── 类型谓词（zero-arg 之外的浮点检查方法）──
        // v0.104.6 D331：三个谓词补 `BigInt` 分支 —— 此前只匹配
        // `Float` / `Int`，于是 `math.is_finite(1n)` 报「requires a numeric
        // argument」，**而 `1n` 显然是 number**（`docs/mora-spec.md:970` 的
        // 签名就是 `number -> bool`）。理由见下方 `unary_preserve` 的注释。
        "is_nan" => match args.first() {
            Some(Value::Float(x)) => Ok(Value::Bool(x.is_nan())),
            Some(Value::Int(_)) => Ok(Value::Bool(false)),
            // BigInt 是精确整数，**永远**不是 NaN / inf / non-finite
            Some(Value::BigInt(_)) => Ok(Value::Bool(false)),
            _ => Err("math.is_nan requires a numeric argument".to_string()),
        },
        "is_inf" => match args.first() {
            Some(Value::Float(x)) => Ok(Value::Bool(x.is_infinite())),
            Some(Value::Int(_)) => Ok(Value::Bool(false)),
            // 同上：BigInt 不可能是 inf
            Some(Value::BigInt(_)) => Ok(Value::Bool(false)),
            _ => Err("math.is_inf requires a numeric argument".to_string()),
        },
        "is_finite" => match args.first() {
            Some(Value::Float(x)) => Ok(Value::Bool(x.is_finite())),
            Some(Value::Int(_)) => Ok(Value::Bool(true)),
            // 任意精度整数**总是**有限的 —— 没有溢出这回事（这正是它存在的意义）
            Some(Value::BigInt(_)) => Ok(Value::Bool(true)),
            _ => Err("math.is_finite requires a numeric argument".to_string()),
        },

        other => Err(format!("math.{}: unknown method", other)),
    }
}

/// Float 入参单参运算（Int 自动转 Float）。结果总是 Float。
fn unary_float<F: Fn(f64) -> f64>(args: &[Value], f: F) -> Result<Value, String> {
    let x = expect_number(args.first(), "math")?;
    Ok(Value::Float(f(x)))
}

/// 双参 Float 运算
fn binary_float<F: Fn(f64, f64) -> f64>(args: &[Value], f: F) -> Result<Value, String> {
    let x = expect_number(args.first(), "math")?;
    let y = expect_number(args.get(1), "math")?;
    Ok(Value::Float(f(x, y)))
}

/// 保留类型：Int → Int(同值)，Float → Float(变换后)
///
/// v0.104.6 D331：补 `BigInt` 分支 —— 此前**只**匹配 `Int` / `Float`，
/// 于是取整一族的六个函数（`abs` / `sign` / `floor` / `ceil` / `round` /
/// `trunc`）**全部拒绝 BigInt**：
///
/// ```text
/// math.abs(-5n)     → math: numeric argument required   ← 而 -5n 显然是 number
/// math.floor(2.7n)  → 同上
/// math.sqrt(4n)     → 2.0                                 ← 同一个模块，认了
/// ```
///
/// **为什么是缺陷而不是设计**（三条独立证据）：
/// ① `docs/mora-spec.md:966-967` 的签名是 **`number -> number`**，
///    `number` 涵盖 BigInt；
/// ② `math.rs` **自己**的 `expect_number`（第 116 行）就认 BigInt，
///    且 CHANGELOG v0.150 那条**把它当成同族做对了的样板**明写：
///    「**必选**数值实参走 `math.rs::expect_number`，认 `Int`/`Float`/`BigInt`」；
/// ③ 实测 20 个走 `expect_number` 的函数**全部**接受 BigInt，9 个走
///    `unary_preserve` / 手写 match 的**全部**拒绝 —— 分界线与
///    「用哪个提取函数」**逐个吻合**，不是按语义分类的结果。
///
/// ⇒ 是**实现层的分裂**，不是语言层面的取舍。
///
/// ⚠ 关键：**不能**把 BigInt 转成 f64 再取整。`f64` 只有 53 位尾数，
/// 而 BigInt 任意精度 —— 经一趟 f64 就会丢精度，且**静默**：
/// ```text
/// math.floor(9999999999999999999999n)  →  经 f64 会得 10000000000000000000000.0
/// ```
/// 所以 BigInt 分支**直接在整数域上算**，用 `BigInt` 自己的除法取整。
/// 只有 `sign` 需要看符号、不需要算术。
fn unary_preserve(
    args: &[Value],
    int_fn: fn(i64) -> i64,
    float_fn: fn(f64) -> f64,
    bigint_op: BigIntOp,
) -> Result<Value, String> {
    match args.first() {
        Some(Value::Int(n)) => Ok(Value::Int(int_fn(*n))),
        Some(Value::Float(n)) => Ok(Value::Float(float_fn(*n))),
        Some(v @ Value::BigInt(_)) => Ok(Value::BigInt(bigint_unary(v, bigint_op))),
        _ => Err("math: numeric argument required".to_string()),
    }
}

/// BigInt 分支要做的事，由这个枚举显式声明。
#[derive(Clone, Copy)]
enum BigIntOp {
    /// 恒等（`floor` / `ceil` / `round` / `trunc` 对**整数**都是恒等）
    Identity,
    /// 绝对值
    Abs,
    /// 符号：`> 0` → 1，`< 0` → -1，`== 0` → 0
    Sign,
}

fn bigint_unary(v: &Value, op: BigIntOp) -> BigInt {
    use num_bigint::BigInt as BI;
    use num_traits::{Signed, Zero};
    let n: BI = match v {
        Value::BigInt(b) => b.clone(),
        _ => return BI::from(0),
    };
    match op {
        // 任意精度整数**已经是**整数，四种取整全是恒等 —— 不经 f64，
        // 因此不会因 53 位尾数而丢精度。
        BigIntOp::Identity => n,
        // 附带收益：`i64::MIN.abs()` 在 Rust 里会 panic / 溢出，
        // BigInt 无此问题。
        BigIntOp::Abs => n.abs(),
        BigIntOp::Sign => {
            if n.is_zero() {
                BI::from(0)
            } else if n.is_positive() {
                BI::from(1)
            } else {
                BI::from(-1)
            }
        }
    }
}

/// 把 Value 转 f64，Int 自动转 Float
fn expect_number(arg: Option<&Value>, ns: &str) -> Result<f64, String> {
    match arg {
        Some(Value::Int(n)) => Ok(*n as f64),
        Some(Value::Float(n)) => Ok(*n),
        Some(Value::BigInt(n)) => n
            .to_string()
            .parse::<f64>()
            .map_err(|_| format!("{}.*: BigInt out of f64 range", ns)),
        _ => Err(format!("{}: numeric argument required", ns)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f64_close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn trig_basic() {
        if let Value::Float(x) = call_math_method("sin", &[Value::Float(0.0)]).unwrap() {
            assert!(f64_close(x, 0.0));
        }
        if let Value::Float(x) = call_math_method("cos", &[Value::Float(0.0)]).unwrap() {
            assert!(f64_close(x, 1.0));
        }
        if let Value::Float(x) = call_math_method("tan", &[Value::Float(0.0)]).unwrap() {
            assert!(f64_close(x, 0.0));
        }
    }

    #[test]
    fn pi_constant() {
        let pi = call_math_method("PI", &[]).unwrap();
        assert!(matches!(pi, Value::Float(x) if f64_close(x, std::f64::consts::PI)));
    }

    #[test]
    fn pow_basic() {
        let r = call_math_method("pow", &[Value::Int(2), Value::Int(10)]).unwrap();
        assert!(matches!(r, Value::Float(x) if f64_close(x, 1024.0)));
    }

    #[test]
    fn abs_int_preserves_int() {
        let r = call_math_method("abs", &[Value::Int(-42)]).unwrap();
        assert!(matches!(r, Value::Int(42)));
    }

    #[test]
    fn floor_float_returns_float() {
        let r = call_math_method("floor", &[Value::Float(3.7)]).unwrap();
        assert!(matches!(r, Value::Float(x) if f64_close(x, 3.0)));
    }

    #[test]
    fn floor_int_returns_int() {
        let r = call_math_method("floor", &[Value::Int(42)]).unwrap();
        assert!(matches!(r, Value::Int(42)));
    }

    #[test]
    fn sqrt_negative_int_errors_via_nan() {
        // f64::sqrt(-1.0) = NaN，符合 IEEE 754，不报错
        let r = call_math_method("sqrt", &[Value::Int(-1)]).unwrap();
        assert!(matches!(r, Value::Float(x) if x.is_nan()));
    }

    #[test]
    fn unknown_method_errors() {
        let r = call_math_method("nonexistent", &[Value::Int(1)]);
        assert!(r.is_err());
    }

    #[test]
    fn is_nan_works() {
        assert!(matches!(
            call_math_method("is_nan", &[Value::Float(f64::NAN)]).unwrap(),
            Value::Bool(true)
        ));
        assert!(matches!(
            call_math_method("is_nan", &[Value::Int(0)]).unwrap(),
            Value::Bool(false)
        ));
    }
}
