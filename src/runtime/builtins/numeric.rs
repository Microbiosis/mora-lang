//! v0.92: 数值方法辅助函数（自 dispatch.rs 拆分），委托给 `math.*` builtin。

use crate::error::{MoraError, MoraResult};
use crate::value::Value;

// ===================================================================
// v0.26: compose_prompt / tail 辅助函数 (在 dispatch.rs 末尾)
// ===================================================================

/// 把 Value 转 String (用于 section.text 字段读取)
// v0.91: 数值方法链（x.abs() / x.sqrt() / x.sin() 等）。
// `is_int` 标记输入是否为 Int 类型（决定某些方法是否保留 Int 类型）。
//
// 设计：复用 math.* builtin 同一底层函数，避免双实现。
// `math.abs(x)` / `x.abs()` 走 `call_math_method("abs", &[x])`，结果一致。
//
// v0.104.6：上面那句「结果一致」此前是**未经实测的断言**。现已逐条验证
// （`tests/builtin_gaps.rs::numeric_method_and_math_module_agree`）：
// 8 个方法 × 正负输入，两种写法逐值相同，包括 `floor/ceil/round/trunc`
// 在负数上的分歧点（如 `(-2.5).floor() = -3.0` 而 `math.floor(-2.5) = -3.0`，
// 两边同样取 floor 而非截断）以及 `sqrt(-2.5)` 两边都返 `NaN` 而非报错。
//
// 顺带记一个**容易误判成缺陷**的坑：`-2.5.abs()` 解析为 `-(2.5.abs())`
// = `-2.5`，而 `math.abs(-2.5)` = `2.5`。这是标准数学优先级（类比 `-x^2`
// 是 `-(x^2)`），不是两种写法不一致 —— 要对负数取方法得写 `(-2.5).abs()`。
// 判别方法：`0 - 2.5.abs()` 同样得 `-2.5`，说明是「方法调用先算、再取负」。
pub(super) fn call_method_numeric(
    recv: &Value,
    method: &str,
    args: &[Value],
    is_int: bool,
) -> MoraResult<Value> {
    // ── 类型转换方法（不走 math.*）──
    match method {
        "dtype" => {
            let dt = match recv {
                Value::Int(_) => crate::common::DType::I64,
                Value::Float(_) => crate::common::DType::F64,
                Value::Scalar(s) => s.dtype(),
                _ => unreachable!(),
            };
            return Ok(Value::String(dt.short_name().to_string()));
        }
        "to_dtype" | "to" => {
            let Some(Value::String(name)) = args.first() else {
                return Err(MoraError::Runtime(format!(
                    "{method}(dtype) requires a dtype string argument"
                )));
            };
            let target_dt = crate::common::DType::from_name(name)
                .ok_or_else(|| MoraError::Runtime(format!("{method}: unknown dtype '{name}'")))?;
            let val_f64 = match recv {
                Value::Int(n) => *n as f64,
                Value::Float(f) => *f,
                Value::Scalar(s) => s.as_f64(),
                _ => 0.0,
            };
            return Ok(crate::value::tensor::scalar_value_for_dtype(
                val_f64, target_dt,
            ));
        }
        "to_float" => match recv {
            Value::Int(n) => return Ok(Value::Float(*n as f64)),
            Value::Float(_) => return Ok(recv.clone()),
            Value::Scalar(s) => return Ok(Value::Float(s.as_f64())),
            _ => {}
        },
        "to_int" => match recv {
            Value::Float(f) => {
                if f.is_nan() || f.is_infinite() {
                    return Err(MoraError::Runtime(
                        "to_int: NaN/Inf cannot convert to int".to_string(),
                    ));
                }
                return Ok(Value::Int(*f as i64));
            }
            Value::Int(_) => return Ok(recv.clone()),
            Value::Scalar(s) => {
                if let Some(n) = s.as_i64() {
                    return Ok(Value::Int(n));
                }
                let f = s.as_f64();
                if f.is_nan() || f.is_infinite() {
                    return Err(MoraError::Runtime(
                        "to_int: NaN/Inf cannot convert to int".to_string(),
                    ));
                }
                return Ok(Value::Int(f as i64));
            }
            _ => {}
        },
        _ => {}
    }
    // ── 其他方法委托给 math.* builtin ──
    // 构造 args = [recv, ..args]
    let mut full_args = Vec::with_capacity(args.len() + 1);
    full_args.push(recv.clone());
    full_args.extend_from_slice(args);
    let result = super::math::call_math_method(method, &full_args)?;
    // 对 Int 输入：abs/sign/signum/floor/ceil/round 保留 Int；其他转 Float
    if is_int
        && matches!(
            method,
            "abs" | "sign" | "signum" | "floor" | "ceil" | "round"
        )
        && let Value::Float(f) = &result
        && f.is_finite()
        && f.fract() == 0.0
        && *f >= i64::MIN as f64
        && *f <= i64::MAX as f64
    {
        return Ok(Value::Int(*f as i64));
    }
    Ok(result)
}
