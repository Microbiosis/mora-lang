//! v0.92: Numeric + budget + text conversion helpers extracted from dispatch.rs (P1.2 split).
//!
//! dispatch.rs 的 `impl Interpreter` 是 1781 行巨型 impl block（P0 时期遗留），
//! 这里把无 self 依赖的自由函数（数值方法链、BigInt 方法、Value→String、budget 解析）
//! 拆分出来，保持 dispatch.rs 聚焦于 builtin dispatch + method dispatch。

use crate::value::Value;
use num_traits::Signed;

// ===================================================================
// v0.26: compose_prompt / tail 辅助函数 (在 dispatch.rs 末尾)
// ===================================================================

/// 把 Value 转 String (用于 section.text 字段读取)
// v0.91: 数值方法链（x.abs() / x.sqrt() / x.sin() 等）。
// `is_int` 标记输入是否为 Int 类型（决定某些方法是否保留 Int 类型）。
//
// 设计：复用 math.* builtin 同一底层函数，避免双实现。
// `math.abs(x)` / `x.abs()` 走 `call_math_method("abs", &[x])`，结果一致。
pub(super) fn call_method_numeric(
    recv: &Value,
    method: &str,
    args: &[Value],
    is_int: bool,
) -> Result<Value, String> {
    // ── 类型转换方法（不走 math.*）──
    match method {
        "to_float" => match recv {
            Value::Int(n) => return Ok(Value::Float(*n as f64)),
            Value::Float(_) => return Ok(recv.clone()),
            _ => {}
        },
        "to_int" => match recv {
            Value::Float(f) => {
                if f.is_nan() || f.is_infinite() {
                    return Err("to_int: NaN/Inf cannot convert to int".to_string());
                }
                return Ok(Value::Int(*f as i64));
            }
            Value::Int(_) => return Ok(recv.clone()),
            _ => {}
        },
        _ => {}
    }
    // ── 其他方法委托给 math.* builtin ──
    // 构造 args = [recv, ..args]
    let mut full_args = Vec::with_capacity(args.len() + 1);
    full_args.push(recv.clone());
    full_args.extend_from_slice(args);
    let result = crate::interpreter::builtins::math::call_math_method(method, &full_args)?;
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

// v0.91: BigInt 方法链
pub(super) fn call_method_bigint(
    recv: &Value,
    method: &str,
    _args: &[Value],
) -> Result<Value, String> {
    let n = match recv {
        Value::BigInt(n) => n,
        _ => return Err("call_method_bigint: not BigInt".to_string()),
    };
    match method {
        "abs" => Ok(Value::BigInt(n.abs())),
        "sign" => {
            // BigInt 无内置 signum（num-bigint 0.4 不导出 Signed trait）；
            // 手动判定：0 → 0，正 → 1，负 → -1
            let zero = num_bigint::BigInt::from(0);
            let s = if *n == zero {
                zero.clone()
            } else if *n > zero {
                num_bigint::BigInt::from(1)
            } else {
                num_bigint::BigInt::from(-1)
            };
            Ok(Value::BigInt(s))
        }
        "to_int" => {
            // 仅当 n 适合 i64 时才转；否则返回错误（用户用 to_string 取文本）
            if n.bits() < 64 {
                let v: i64 = n
                    .try_into()
                    .map_err(|_| "BigInt.to_int: value exceeds i64 range".to_string())?;
                Ok(Value::Int(v))
            } else {
                Err("BigInt.to_int: value exceeds i64 range".to_string())
            }
        }
        "to_float" => n
            .to_string()
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| "BigInt.to_float: value cannot be represented as f64".to_string()),
        _ => Err(format!("BigInt has no method: {}", method)),
    }
}

pub(super) fn text_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Float(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Nil => String::new(),
        other => other.to_string(),
    }
}

/// 解析 budget 值 (dispatch 层副本,与 execute.rs 同语义)
pub(super) fn parse_budget_dispatch(v: Value, ctx: &str) -> Result<usize, String> {
    match v {
        Value::Float(n) => {
            if n < 0.0 {
                return Err(format!("{}: budget must be non-negative", ctx));
            }
            Ok(n as usize)
        }
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                return Err(format!("{}: empty budget string", ctx));
            }
            let bytes = s.as_bytes();
            let mut i = 0;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            let num_part = &s[..i];
            let unit_part = s[i..].trim();
            let num: f64 = num_part
                .parse()
                .map_err(|_| format!("{}: invalid budget '{}'", ctx, s))?;
            let mult: usize = match unit_part.to_uppercase().as_str() {
                "" | "B" => 1,
                "KB" | "K" => 1024,
                "MB" | "M" => 1024 * 1024,
                "GB" | "G" => 1024 * 1024 * 1024,
                other => {
                    return Err(format!(
                        "{}: unknown budget unit '{}' (B/KB/MB/GB)",
                        ctx, other
                    ));
                }
            };
            Ok((num * mult as f64) as usize)
        }
        other => Err(format!(
            "{}: budget must be string or number, got {:?}",
            ctx, other
        )),
    }
}
