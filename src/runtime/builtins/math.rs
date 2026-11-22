//! v0.110.0: `math.*` — 标量数学 builtin（APL/StreamIt 启发）
//!
//! 设计原则：
//! - 函数式 API（`math.sin(x)`）— 与现有 builtin 风格一致
//! - 方法式 API（`x.sin()`）经 `builtins::numeric::call_method_numeric` 转发到
//!   本模块同一底层函数，以避免双实现
//! - 数值参数：Int/Float 接受；结果 Int 输入返回 Int（int 路径），Float 输入返回 Float
//! - 常量以模块属性形式暴露：`math.PI / math.E / math.TAU / math.INF / math.NAN`

use crate::error::{MoraError, MoraResult};
use crate::value::Value;

/// math.* builtin 入口分发（支持自动微分磁带追踪）
pub fn call_math_method_tracked(
    method: &str,
    args: &[Value],
    autograd: Option<&mut crate::runtime::autograd::AutogradContext>,
) -> MoraResult<Value> {
    if let Some(Value::Tensor(t)) = args.first()
        && matches!(
            method,
            "sin" | "cos" | "tanh" | "exp" | "log" | "sqrt" | "abs"
        )
    {
        return crate::value::tensor::eval_tensor_math_unary_tracked(t, method, autograd);
    }
    call_math_method(method, args)
}

/// math.* builtin 入口分发
pub fn call_math_method(method: &str, args: &[Value]) -> MoraResult<Value> {
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
        ),
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
        // v0.104.6 D331：三个谓词补齐数值分支 —— 此前只匹配
        // `Float` / `Int`，于是低位宽整数标量报「requires a numeric
        // argument」，**而它显然是 number**（`docs/mora-spec.md:1033` 的
        // 签名就是 `number -> bool`）。理由见下方 `unary_preserve` 的注释。
        "is_nan" => match args.first() {
            Some(Value::Float(x)) => Ok(Value::Bool(x.is_nan())),
            Some(Value::Int(_)) => Ok(Value::Bool(false)),
            Some(Value::Scalar(s)) => Ok(Value::Bool(s.dtype().is_float() && s.as_f64().is_nan())),
            _ => Err(MoraError::Runtime(
                "math.is_nan requires a numeric argument".to_string(),
            )),
        },
        "is_inf" => match args.first() {
            Some(Value::Float(x)) => Ok(Value::Bool(x.is_infinite())),
            Some(Value::Int(_)) => Ok(Value::Bool(false)),
            Some(Value::Scalar(s)) => Ok(Value::Bool(
                s.dtype().is_float() && s.as_f64().is_infinite(),
            )),
            _ => Err(MoraError::Runtime(
                "math.is_inf requires a numeric argument".to_string(),
            )),
        },
        "is_finite" => match args.first() {
            Some(Value::Float(x)) => Ok(Value::Bool(x.is_finite())),
            Some(Value::Int(_)) => Ok(Value::Bool(true)),
            Some(Value::Scalar(s)) => {
                Ok(Value::Bool(!s.dtype().is_float() || s.as_f64().is_finite()))
            }
            _ => Err(MoraError::Runtime(
                "math.is_finite requires a numeric argument".to_string(),
            )),
        },

        other => Err(MoraError::Runtime(format!(
            "math.{}: unknown method",
            other
        ))),
    }
}

/// Float 入参单参运算（支持标量、低位宽 `Value::Scalar` 与逐元素 `Value::Tensor`）。
fn unary_float<F: Fn(f64) -> f64>(args: &[Value], f: F) -> MoraResult<Value> {
    if let Some(Value::Tensor(t)) = args.first() {
        if t.dtype.is_bool() {
            return Err(MoraError::Runtime(
                "math: cannot apply floating-point math op to Tensor<bool>".to_string(),
            ));
        }
        return Ok(Value::Tensor(t.map_unary(f)?));
    }
    if let Some(Value::Scalar(s)) = args.first() {
        let out = f(s.as_f64());
        if s.dtype().is_float() {
            return Ok(crate::value::tensor::scalar_value_for_dtype(out, s.dtype()));
        }
        return Ok(Value::Float(out));
    }
    let x = expect_number(args.first(), "math")?;
    Ok(Value::Float(f(x)))
}

/// 双参 Float 运算（支持 `Value::Tensor` 与 `Value::Scalar`）。
fn binary_float<F: Fn(f64, f64) -> f64>(args: &[Value], f: F) -> MoraResult<Value> {
    match (args.first(), args.get(1)) {
        (Some(Value::Tensor(a)), Some(Value::Tensor(b))) => {
            if a.device != b.device {
                return Err(MoraError::Runtime(format!(
                    "math: device mismatch between '{}' and '{}'",
                    a.device, b.device
                )));
            }
            if a.dtype != b.dtype {
                return Err(MoraError::Runtime(format!(
                    "math: dtype mismatch between Tensor<{}> and Tensor<{}>",
                    a.dtype, b.dtype
                )));
            }
            let out_shape = crate::value::tensor::broadcast_runtime_shapes(&a.shape, &b.shape)?;
            let numel = crate::value::tensor::checked_numel(&out_shape)?;
            let fa = a.to_flat_vec();
            let fb = b.to_flat_vec();
            if a.shape == b.shape {
                let out: Vec<f64> = fa.into_iter().zip(fb).map(|(x, y)| f(x, y)).collect();
                return Ok(Value::Tensor(crate::value::tensor::Tensor::from_raw(
                    out,
                    out_shape,
                    a.dtype,
                    a.device.clone(),
                    a.requires_grad || b.requires_grad,
                )?));
            }
            let mut out = Vec::with_capacity(numel);
            let rank = out_shape.len();
            for mut lin in 0..numel {
                let mut coords = vec![0usize; rank];
                for axis in (0..rank).rev() {
                    coords[axis] = lin % out_shape[axis];
                    lin /= out_shape[axis];
                }
                let a_off = rank - a.shape.len();
                let b_off = rank - b.shape.len();
                let ca: Vec<usize> = (0..a.shape.len())
                    .map(|i| {
                        if a.shape[i] == 1 {
                            0
                        } else {
                            coords[a_off + i]
                        }
                    })
                    .collect();
                let cb: Vec<usize> = (0..b.shape.len())
                    .map(|i| {
                        if b.shape[i] == 1 {
                            0
                        } else {
                            coords[b_off + i]
                        }
                    })
                    .collect();
                out.push(f(a.get_at(&ca), b.get_at(&cb)));
            }
            return Ok(Value::Tensor(crate::value::tensor::Tensor::from_raw(
                out,
                out_shape,
                a.dtype,
                a.device.clone(),
                a.requires_grad || b.requires_grad,
            )?));
        }
        (Some(Value::Tensor(t)), Some(rhs)) => {
            let y = expect_number(Some(rhs), "math")?;
            return Ok(Value::Tensor(t.map_unary(|x| f(x, y))?));
        }
        (Some(lhs), Some(Value::Tensor(t))) => {
            let x = expect_number(Some(lhs), "math")?;
            return Ok(Value::Tensor(t.map_unary(|y| f(x, y))?));
        }
        (Some(Value::Scalar(a)), Some(Value::Scalar(b)))
            if a.dtype() == b.dtype() && a.dtype().is_float() =>
        {
            return Ok(crate::value::tensor::scalar_value_for_dtype(
                f(a.as_f64(), b.as_f64()),
                a.dtype(),
            ));
        }
        _ => {}
    }
    let x = expect_number(args.first(), "math")?;
    let y = expect_number(args.get(1), "math")?;
    Ok(Value::Float(f(x, y)))
}

/// 保留类型：Int → Int(同值)，Float → Float(变换后)
///
/// 取整一族的六个函数（`abs` / `sign` / `floor` / `ceil` / `round` /
/// `trunc`）必须**保持输入的数值类型**：`math.abs(-42)` 得 `Int(-42)`
/// 而不是 `Float(-42.0)`（`docs/mora-spec.md:1029-1030` 的签名是
/// `number -> number`，并注明「保留 Int 类型」）。因此它们不走
/// `unary_float`，而由本函数按值形态分派：
/// - `Int` → 在 `i64` 域上算，精确无损；
/// - `Float` → 在 `f64` 域上算；
/// - `Scalar` → 浮点 dtype 走其自身的浮点域；整数 dtype 装得下 `i64` 时走
///   整数域，装不下 `i64`（如超 `i64::MAX` 的 `u64`）则原样返回，
///   不做有损转换；
/// - `Tensor` → 逐元素走浮点域。
///
/// ⚠ 关键：**不能**先把整数转成 f64 再取整。`f64` 只有 53 位尾数，
/// 经一趟 f64 就会丢精度，且**静默**：
/// ```text
/// math.floor(9223372036854775807)  →  经 f64 会得 9223372036854775808.0
/// ```
/// 所以整数分支**直接在整数域上算**，只有 `sign` 需要看符号、不需要算术。
fn unary_preserve(
    args: &[Value],
    int_fn: fn(i64) -> i64,
    float_fn: fn(f64) -> f64,
) -> MoraResult<Value> {
    match args.first() {
        Some(Value::Int(n)) => Ok(Value::Int(int_fn(*n))),
        Some(Value::Float(n)) => Ok(Value::Float(float_fn(*n))),
        Some(Value::Scalar(s)) => {
            if s.dtype().is_float() {
                Ok(crate::value::tensor::scalar_value_for_dtype(
                    float_fn(s.as_f64()),
                    s.dtype(),
                ))
            } else if let Some(n) = s.as_i64() {
                let res = int_fn(n);
                crate::common::ScalarValue::from_i64_checked(res, s.dtype()).map(Value::Scalar)
            } else {
                Ok(Value::Scalar(*s))
            }
        }
        Some(Value::Tensor(t)) => Ok(Value::Tensor(t.map_unary(float_fn)?)),
        _ => Err(MoraError::Runtime(
            "math: numeric argument required".to_string(),
        )),
    }
}

/// 把 Value 转 f64，Int/Scalar 自动转 Float
fn expect_number(arg: Option<&Value>, ns: &str) -> MoraResult<f64> {
    match arg {
        Some(Value::Int(n)) => Ok(*n as f64),
        Some(Value::Float(n)) => Ok(*n),
        Some(Value::Scalar(s)) => Ok(s.as_f64()),
        _ => Err(MoraError::Runtime(format!(
            "{}: numeric argument required",
            ns
        ))),
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
