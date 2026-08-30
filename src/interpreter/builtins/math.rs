//! v0.91: math.* — 标量数学 builtin（APL/StreamIt 启发）
//!
//! 设计原则：
//! - 函数式 API（`math.sin(x)`）— 与现有 builtin 风格一致
//! - 方法式 API（`x.sin()`）由 `dispatch.rs::call_method_int/float` 直接实现
//!   以避免双实现（参见 dispatch.rs 内对应函数）
//! - 数值参数：Int/Float 接受；结果 Int 输入返回 Int（int 路径），Float 输入返回 Float
//! - 常量以模块属性形式暴露：`math.PI / math.E / math.TAU / math.INF / math.NAN`

use crate::value::Value;

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
        "abs" => unary_preserve(args, |x| x.abs(), |x| x.abs()),
        "sign" => unary_preserve(args, |x| x.signum(), |x| {
            if x > 0.0 {
                1.0
            } else if x < 0.0 {
                -1.0
            } else {
                0.0
            }
        }),
        "floor" => unary_preserve(args, |x| x, |x| x.floor()),
        "ceil" => unary_preserve(args, |x| x, |x| x.ceil()),
        "round" => unary_preserve(args, |x| x, |x| x.round()),
        "trunc" => unary_preserve(args, |x| x, |x| x.trunc()),
        "fract" => unary_float(args, f64::fract),

        // ── 模块常量（零参）──
        "PI" => Ok(Value::Float(std::f64::consts::PI)),
        "E" => Ok(Value::Float(std::f64::consts::E)),
        "TAU" => Ok(Value::Float(std::f64::consts::TAU)),
        "INF" => Ok(Value::Float(f64::INFINITY)),
        "NAN" => Ok(Value::Float(f64::NAN)),

        // ── 类型谓词（zero-arg 之外的浮点检查方法）──
        "is_nan" => match args.first() {
            Some(Value::Float(x)) => Ok(Value::Bool(x.is_nan())),
            Some(Value::Int(_)) => Ok(Value::Bool(false)),
            _ => Err("math.is_nan requires a numeric argument".to_string()),
        },
        "is_inf" => match args.first() {
            Some(Value::Float(x)) => Ok(Value::Bool(x.is_infinite())),
            Some(Value::Int(_)) => Ok(Value::Bool(false)),
            _ => Err("math.is_inf requires a numeric argument".to_string()),
        },
        "is_finite" => match args.first() {
            Some(Value::Float(x)) => Ok(Value::Bool(x.is_finite())),
            Some(Value::Int(_)) => Ok(Value::Bool(true)),
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
fn unary_preserve(
    args: &[Value],
    int_fn: fn(i64) -> i64,
    float_fn: fn(f64) -> f64,
) -> Result<Value, String> {
    match args.first() {
        Some(Value::Int(n)) => Ok(Value::Int(int_fn(*n))),
        Some(Value::Float(n)) => Ok(Value::Float(float_fn(*n))),
        _ => Err("math: numeric argument required".to_string()),
    }
}

/// 把 Value 转 f64，Int 自动转 Float
fn expect_number(arg: Option<&Value>, ns: &str) -> Result<f64, String> {
    match arg {
        Some(Value::Int(n)) => Ok(*n as f64),
        Some(Value::Float(n)) => Ok(*n),
        Some(Value::BigInt(n)) => n.to_string().parse::<f64>().map_err(|_| {
            format!("{}.*: BigInt out of f64 range", ns)
        }),
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