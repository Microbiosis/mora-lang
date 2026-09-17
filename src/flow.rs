//! v0.20: 自由函数（自 interpreter.rs 抽出）。
//!
//! **Move-only refactor** — 代码自 src/interpreter.rs **迁移**（非复制）：
//! interpreter.rs 不再持有这些函数的副本，而是通过 `use crate::flow::*`
//! 重新导出。此处是唯一定义点。

use crate::common::{BinaryOp, Literal};
use crate::error::MoraError;
use crate::value::Value;

/// 判断值是否为真 — MIR 条件分支的单一真值源（v0.75.83 收敛）。
///
/// 语义：Bool 取自身；Nil/Int(0)/Float(0.0)/空 String/空 List/空 Dict 为
/// falsy；其余为 truthy。此前存在两份实现：本函数（缺 Int 分支，Int(0)
/// 落 `_ => true` 误判为真）与 mir/vm.rs 版（List/Dict 恒真，空容器误判
/// 为真）——两处语义分叉是隐蔽 bug 温床，已收敛为本单一实现。
pub fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Nil => false,
        Value::Bool(b) => *b,
        Value::Int(i) => *i != 0,
        Value::Float(n) => *n != 0.0,
        Value::String(s) => !s.is_empty(),
        Value::List(l) => !l.is_empty(),
        Value::Dict(d) => !d.is_empty(),
        _ => true,
    }
}

/// 检查是否是内置模块对象名（`name.method(...)` 形式）。
///
/// v0.103: 改为从 [`crate::value::MODULE_OBJECTS`] 派生 —— 此前此处硬编码
/// 6 个名字，与 globals 注册表（22 个模块）漂移，导致 13 个已注册模块被
/// typeck 判为 Unbound variable。
pub fn is_builtin_object(name: &str) -> bool {
    crate::value::MODULE_OBJECTS.iter().any(|(n, _)| *n == name)
}

/// hex 编码
pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// hex 解码
pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("hex string must have even length".to_string());
    }
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    for i in (0..bytes.len()).step_by(2) {
        let high = hex_nibble(bytes[i]).ok_or("invalid hex character")?;
        let low = hex_nibble(bytes[i + 1]).ok_or("invalid hex character")?;
        result.push((high << 4) | low);
    }
    Ok(result)
}

/// hex 单字符解析
pub fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// 检查是否是管道方法
pub fn is_pipe_method(name: &str) -> bool {
    matches!(
        name,
        "map"
            | "filter"
            | "reduce"
            | "push"
            | "pop"
            | "get"
            | "len"
            | "upper"
            | "lower"
            | "trim"
            | "starts_with"
            | "ends_with"
            | "contains"
            | "split"
            | "replace"
            | "take"
            | "drop"
            | "window"
            | "batch"
            | "shape"
            | "flatten"
            | "transpose"
            | "reshape"
    )
}

/// 二元操作求值
///
    /// v0.38: addition follows the numeric-tower promotion rules.
    /// v0.76.00: 返回 `Result<Value, MoraError>`（MoraError 统一计划推进）。
    /// v0.103: Int ⊂ Float —— 混合运算提升为 Float（此前 Rust-strict 报错）。
pub fn eval_binary(left: Value, op: &BinaryOp, right: Value) -> Result<Value, MoraError> {
    match op {
        BinaryOp::Add => match (&left, &right) {
            // Strict: Int+Int -> Int
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
            // Strict: Float+Float -> Float
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
            // v0.91: BigInt promotion — 任一含 BigInt 时结果 BigInt（最小惊讶）
            (Value::BigInt(a), Value::BigInt(b)) => Ok(Value::BigInt(a + b)),
            (Value::Int(a), Value::BigInt(b)) => {
                Ok(Value::BigInt(num_bigint::BigInt::from(*a) + b))
            }
            (Value::BigInt(a), Value::Int(b)) => {
                Ok(Value::BigInt(a + num_bigint::BigInt::from(*b)))
            }
            // Float + BigInt — 优先 BigInt（保留精度），仅当能无损转换时回 Float
            (Value::Float(a), Value::BigInt(b)) => {
                b.to_string().parse::<f64>().map_or_else(
                    |_| Ok(Value::BigInt(num_bigint::BigInt::from(*a as i64) + b.clone())),
                    |bf| Ok(Value::Float(*a + bf)),
                )
            }
            (Value::BigInt(a), Value::Float(b)) => {
                a.to_string().parse::<f64>().map_or_else(
                    |_| Ok(Value::BigInt(a.clone() + num_bigint::BigInt::from(*b as i64))),
                    |af| Ok(Value::Float(af + *b)),
                )
            }
            // v0.103: numeric tower — Int ⊂ Float，混合运算提升为 Float。
            // 此前此处报 Rust-strict 错误，与三处既有事实矛盾：类型系统
            // （`unify.rs` 的 Numeric 约束把 Int/Float 提升为 Float）、
            // spec §15.1（`number` 按数值比较）以及本函数紧邻的 BigInt 分支
            // （Int+BigInt / Float+BigInt 均可混算）。结果是**类型检查通过的
            // 程序在运行期报类型错误** —— 契约分叉，非设计意图。
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 + b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a + *b as f64)),
            (Value::String(a), Value::String(b)) => Ok(Value::String(format!("{}{}", a, b))),
            // 字符串 + 任意类型 → 自动转字符串拼接
            (Value::String(a), _) => Ok(Value::String(format!("{}{}", a, right))),
            (_, Value::String(b)) => Ok(Value::String(format!("{}{}", left, b))),
            (Value::List(a), Value::List(b)) => {
                // v0.17: 等长列表逐元素相加，否则拼接
                if a.len() == b.len() {
                    // v0.104.2: 逐元素加法委托 `eval_binary(Add)` ——
                    // 此前只列了 `Float+Float` 与 `String+String`，
                    // **Int 落到 `_ => Nil`**：`[1i] + [2i]` 得 `[nil]`
                    //（等长时逐元素、不等长才拼接，于是同一运算的结果取决于
                    // 长度 —— `[] + [1i]` 得 `[1]` 而 `[1i] + [2i]` 得 `[nil]`）。
                    // 委托后与标量加法同一套规则（Int+Int / Int+Float /
                    // Float+Float / BigInt 提升 / 字符串拼接），语义收敛。
                    let result: Vec<Value> = a
                        .iter()
                        .zip(b.iter())
                        .map(|(x, y)| {
                            eval_binary(x.clone(), &BinaryOp::Add, y.clone())
                                .unwrap_or(Value::Nil) // 不支持加法的元素对（如 dict+dict）→ Nil
                        })
                        .collect();
                    Ok(Value::List(result))
                } else {
                    let mut merged = a.clone();
                    merged.extend(b.clone());
                    Ok(Value::List(merged))
                }
            }
            // v0.17: 广播 - list + number
            (Value::List(list), Value::Float(scalar)) => {
                let result: Vec<Value> = list
                    .iter()
                    .map(|item| match item {
                        Value::Float(n) => Value::Float(n + scalar),
                        Value::String(s) => Value::String(format!("{}{}", s, scalar)),
                        _ => Value::Nil,
                    })
                    .collect();
                Ok(Value::List(result))
            }
            // v0.17: 广播 - number + list
            (Value::Float(scalar), Value::List(list)) => {
                let result: Vec<Value> = list
                    .iter()
                    .map(|item| match item {
                        Value::Float(n) => Value::Float(scalar + n),
                        _ => Value::Nil,
                    })
                    .collect();
                Ok(Value::List(result))
            }
            _ => Err(MoraError::Other("Operands must be two numbers, two strings, or two lists".to_string())),
        },
        BinaryOp::Sub => numeric_op(left, right, |a, b| a - b),
        BinaryOp::Mul => numeric_op(left, right, |a, b| a * b),
        BinaryOp::Div => numeric_op(left, right, |a, b| a / b),
        BinaryOp::Mod => numeric_op(left, right, |a, b| a % b),
        BinaryOp::Equal => Ok(Value::Bool(values_equal(&left, &right))),
        BinaryOp::NotEqual => Ok(Value::Bool(!values_equal(&left, &right))),
        BinaryOp::Greater => numeric_cmp(left, right, |a, b| a > b),
        BinaryOp::Less => numeric_cmp(left, right, |a, b| a < b),
        BinaryOp::GreaterEqual => numeric_cmp(left, right, |a, b| a >= b),
        BinaryOp::LessEqual => numeric_cmp(left, right, |a, b| a <= b),
    }
}

/// 数值操作辅助
///
/// v0.38 (C5): numeric tower — promotion rules:
/// - `Int op Int`     = Int        (纯整数算术)
/// - `Float op Float` = Float      (纯浮点算术)
/// - `Int op Float`   = Float      (v0.103: Int ⊂ Float，混合提升为 Float)
///
/// v0.91: 把 BigInt 转为 f64（如果超出 f64 范围返回 ±INFINITY）。
/// 这是 lossy 转换，仅用于类型提升（Float + BigInt → Float）。
fn bigint_to_f64_lossy(n: &num_bigint::BigInt) -> f64 {
    use num_traits::ToPrimitive;
    n.to_f64().unwrap_or(f64::INFINITY)
}

/// v0.76.00: 返回 `Result<Value, MoraError>`（MoraError 统一计划推进）。
pub fn numeric_op<F>(left: Value, right: Value, op: F) -> Result<Value, MoraError>
where
    F: Fn(f64, f64) -> f64,
{
    use Value::*;
    match (left, right) {
        // Strict: Int+Int -> Int
        (Int(a), Int(b)) => {
            let af = a as f64;
            let bf = b as f64;
            let result = op(af, bf).round() as i64;
            Ok(Int(result))
        }
        // Strict: Float+Float -> Float
        (Float(a), Float(b)) => Ok(Float(op(a, b))),
        // v0.91: BigInt promotion — 任一含 BigInt 时结果 BigInt
        // BigInt 通过 f64 转换执行算术；超过 f64 精度时回退 BigInt 原生路径
        (BigInt(a), BigInt(b)) => {
            let af = bigint_to_f64_lossy(&a);
            let bf = bigint_to_f64_lossy(&b);
            if af.is_finite() && bf.is_finite() {
                let result = op(af, bf);
                // 整数与浮点结果都用 i64 近似 — 浮点 BigInt 标记表示"原运算含 BigInt"
                Ok(BigInt(num_bigint::BigInt::from(result as i64)))
            } else {
                Err(MoraError::Other(
                    "BigInt op out of f64 range".to_string(),
                ))
            }
        }
        (Int(a), BigInt(b)) => {
            let af = a as f64;
            let bf = bigint_to_f64_lossy(&b);
            if af.is_finite() && bf.is_finite() {
                let result = op(af, bf);
                Ok(BigInt(num_bigint::BigInt::from(result as i64)))
            } else {
                Err(MoraError::Other("BigInt op out of f64 range".to_string()))
            }
        }
        (BigInt(a), Int(b)) => {
            let af = bigint_to_f64_lossy(&a);
            let bf = b as f64;
            if af.is_finite() && bf.is_finite() {
                let result = op(af, bf);
                Ok(BigInt(num_bigint::BigInt::from(result as i64)))
            } else {
                Err(MoraError::Other("BigInt op out of f64 range".to_string()))
            }
        }
        (Float(a), BigInt(b)) => {
            Ok(Float(op(a, bigint_to_f64_lossy(&b))))
        }
        (BigInt(a), Float(b)) => {
            Ok(Float(op(bigint_to_f64_lossy(&a), b)))
        }
        // v0.103: numeric tower — Int ⊂ Float，混合提升为 Float（与 typeck
        // 的 Numeric 约束一致；此前报 Rust-strict 错误，属契约分叉）。
        (Int(a), Float(b)) => Ok(Float(op(a as f64, b))),
        (Float(a), Int(b)) => Ok(Float(op(a, b as f64))),
        // v0.17: 广播操作 - list op number
        (Value::List(list), Value::Float(scalar)) => {
            let result: Vec<Value> = list
                .iter()
                .map(|item| match item {
                    Value::Float(n) => Value::Float(op(*n, scalar)),
                    _ => Value::Nil,
                })
                .collect();
            Ok(Value::List(result))
        }
        // v0.17: 广播操作 - number op list
        (Value::Float(scalar), Value::List(list)) => {
            let result: Vec<Value> = list
                .iter()
                .map(|item| match item {
                    Value::Float(n) => Value::Float(op(scalar, *n)),
                    _ => Value::Nil,
                })
                .collect();
            Ok(Value::List(result))
        }
        // v0.17: 广播操作 - list op list (逐元素)
        (Value::List(a), Value::List(b)) => {
            if a.len() != b.len() {
                return Err(MoraError::Other(format!(
                    "List length mismatch: {} vs {}",
                    a.len(),
                    b.len()
                )));
            }
            let result: Vec<Value> = a
                .iter()
                .zip(b.iter())
                .map(|(x, y)| match (x, y) {
                    (Value::Float(xn), Value::Float(yn)) => Value::Float(op(*xn, *yn)),
                    _ => Value::Nil,
                })
                .collect();
            Ok(Value::List(result))
        }
        _ => Err(MoraError::Other("Operands must be numbers".to_string())),
    }
}

/// 数值比较辅助
///
/// v0.103: numeric tower — Int ⊂ Float。
/// - `Int cmp Int`     → 按 i64 比较
/// - `Float cmp Float` → 按 f64 比较
/// - `Int cmp Float`   → 提升为 f64 比较（此前报 Rust-strict 错误，
///   与 typeck 及 spec §15.1「`number` 数值比较」分叉）
///
/// v0.76.00: 返回 `Result<Value, MoraError>`（MoraError 统一计划推进）。
pub fn numeric_cmp<F>(left: Value, right: Value, op: F) -> Result<Value, MoraError>
where
    F: Fn(f64, f64) -> bool,
{
    use Value::*;
    match (left, right) {
        (Int(a), Int(b)) => Ok(Bool(op(a as f64, b as f64))),
        (Float(a), Float(b)) => Ok(Bool(op(a, b))),
        (Int(a), Float(b)) => Ok(Bool(op(a as f64, b))),
        (Float(a), Int(b)) => Ok(Bool(op(a, b as f64))),
        _ => Err(MoraError::Other("Operands must be numbers".to_string())),
    }
}

/// 值相等比较
///
/// v0.103: 数值相等遵循 numeric tower —— `Int ⊂ Float`，故 `4 == 4.0` 为真；
/// `numeric_cmp` 已把 Int/Float 视为可比较（`4 <= 4.0` 为真），若 `==` 判否
/// 则 `<=` 与 `==` 自相矛盾。BigInt 亦纳入（v0.91 引入变体时漏加 —— 与
/// `Value::eq` 的 BigInt arm 保持一致）。
pub fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Nil, Value::Nil) => true,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => a == b,
        // v0.103: tower 提升 —— 混合数值按 f64 比较
        (Value::Int(a), Value::Float(b)) | (Value::Float(b), Value::Int(a)) => *a as f64 == *b,
        (Value::BigInt(a), Value::BigInt(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::List(a), Value::List(b)) => a == b,
        (Value::Dict(a), Value::Dict(b)) => a == b,
        // Conversation 不支持相等比较——比较引用无意义
        _ => false,
    }
}

/// AST Literal 转运行时 Value
pub fn literal_to_value_static(lit: &Literal) -> Value {
    match lit {
        Literal::String(s, _) => Value::String(s.clone()),
        Literal::Char(c, _) => Value::Char(*c),
        Literal::Int(i, _) => Value::Int(*i),
        Literal::Float(f, _) => Value::Float(*f),
        Literal::BigInt(n, _) => Value::BigInt(n.clone()),
        Literal::Bool(b, _) => Value::Bool(*b),
        Literal::Nil(_) => Value::Nil,
    }
}

/// 运行时类型名
pub fn type_name(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "string",
        Value::Char(_) => "char",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::BigInt(_) => "bigint",
        Value::Bool(_) => "bool",
        Value::Nil => "nil",
        Value::List(_) => "list",
        Value::Dict(_) => "dict",
        // v0.102: 声明式范式值
        Value::Relation { .. } => "relation",
        Value::Goal(_) => "goal",
        Value::LogicVar(_) => "logicvar",
        Value::Task { .. } => "task",
        Value::Tool { .. } => "tool",
        Value::Closure { .. } => "closure",
        Value::Builtin(_) => "builtin",
        Value::Conversation { .. } => "conversation",
        Value::Stream { .. } => "stream",
        Value::Agent { .. } => "agent",
        Value::AiConfig { .. } => "ai_config",
        Value::Router { .. } => "router",
        Value::HttpRequest { .. } => "http_request",
        Value::McpServer { .. } => "mcp_server",
        Value::TraitObject { .. } => "trait_object",
        Value::Compose(_) => "compose",
        Value::Partial(_, _) => "partial",
        Value::Atom(_) => "atom",
        Value::Macro { .. } => "macro",
        // v0.86: Curry — 柯里化函数值
        Value::Curry { .. } => "curry",
        // v0.86: Cons — Lisp 链式列表单元
        Value::Cons { .. } => "cons",
        // v0.86: Code — quote(expr) 捕获的源码文本值
        Value::Code(_) => "code",
        Value::PromptSection { .. } => "prompt_section",
        // v0.83: TEA types
        Value::TeaApp(_) => "tea_app",
        Value::TeaCmd(_) => "tea_cmd",
        Value::TeaMsg(_) => "tea_msg",
        Value::Document { .. } => "document",
    }
}

/// 返回值的类型名 (String)
pub fn value_type_name(value: &Value) -> &'static str {
    type_name(value)
}

mod json; // v0.75.63: JSON 编解码（json_to_value/value_to_json + parse_json_*）自 flow.rs 拆出
pub use json::{json_to_value, value_to_json}; // 保持 flow::json_to_value 路径

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::BinaryOp;
    use crate::typeck::Type;

    /// v0.38: Int + Int = Int (no silent promotion to Float).
    #[test]
    fn numeric_tower_int_plus_int_yields_int() {
        let l = Value::Int(2);
        let r = Value::Int(3);
        let v = numeric_op(l, r, |a, b| a + b).unwrap();
        assert_eq!(v, Value::Int(5));
    }

    /// v0.38: Float + Float = Float.
    #[test]
    fn numeric_tower_float_plus_float_yields_float() {
        let l = Value::Float(1.5);
        let r = Value::Float(2.5);
        let v = numeric_op(l, r, |a, b| a + b).unwrap();
        assert_eq!(v, Value::Float(4.0));
    }

    /// v0.103: Int op Float 提升为 Float（取代 v0.38 的 strict error）。
    /// 旧断言（`is_err`）与 typeck 的 Numeric 提升、spec §15.1、以及本文件
    /// 的 BigInt 混算分支三处矛盾；此处断言新的 tower 语义。
    #[test]
    fn numeric_tower_int_plus_float_promotes() {
        let l = Value::Int(2);
        let r = Value::Float(3.0);
        let v = numeric_op(l, r, |a, b| a + b).unwrap();
        assert_eq!(v, Value::Float(5.0), "Int + Float → Float(提升)");
    }

    /// v0.103: Float op Int 对称提升为 Float。
    #[test]
    fn numeric_tower_float_plus_int_promotes() {
        let l = Value::Float(2.0);
        let r = Value::Int(3);
        let v = numeric_op(l, r, |a, b| a + b).unwrap();
        assert_eq!(v, Value::Float(5.0), "Float + Int → Float(提升)");
    }

    /// v0.38: Float + Float → Float via numeric_op (补充用例：整数 Float)。
    #[test]
    fn numeric_tower_float_plus_float_integer_values() {
        let l = Value::Float(2.0);
        let r = Value::Float(3.0);
        let v = numeric_op(l, r, |a, b| a + b).unwrap();
        assert_eq!(v, Value::Float(5.0));
    }

    /// v0.38: eval_binary Add(Int, Int) -> Int.
    #[test]
    fn eval_binary_int_add() {
        let v = eval_binary(Value::Int(2), &BinaryOp::Add, Value::Int(3)).unwrap();
        assert_eq!(v, Value::Int(5));
    }

    /// v0.38: eval_binary Add(Float, Float) -> Float.
    #[test]
    fn eval_binary_float_add() {
        let v = eval_binary(Value::Float(1.5), &BinaryOp::Add, Value::Float(2.5)).unwrap();
        assert_eq!(v, Value::Float(4.0));
    }

    /// v0.103: eval_binary Add(Int, Float) 提升为 Float（取代 strict error）。
    #[test]
    fn eval_binary_int_float_add_promotes() {
        let v = eval_binary(Value::Int(2), &BinaryOp::Add, Value::Float(3.0)).unwrap();
        assert_eq!(v, Value::Float(5.0));
    }

    /// v0.38: numeric_cmp Int < Int.
    #[test]
    fn numeric_cmp_int_lt() {
        let v = numeric_cmp(Value::Int(1), Value::Int(2), |a, b| a < b).unwrap();
        assert_eq!(v, Value::Bool(true));
    }

    /// v0.75.44: eval_binary Equal(Int, Int) — values_equal 的 Int 分支
    /// （v0.38 引入 Int 变体时漏加，`4 == 4` 曾恒 false）。
    /// v0.103: 混合数值按 numeric tower 比较 —— `4 == 4.0` 为真（与
    /// `numeric_cmp` 的 `4 <= 4.0` 一致；此前判否使 `<=` 与 `==` 矛盾）。
    #[test]
    fn eval_binary_int_equal() {
        let v = eval_binary(Value::Int(4), &BinaryOp::Equal, Value::Int(4)).unwrap();
        assert_eq!(v, Value::Bool(true));
        let v2 = eval_binary(Value::Int(4), &BinaryOp::Equal, Value::Int(5)).unwrap();
        assert_eq!(v2, Value::Bool(false));
        // 混合数值：tower 提升后相等
        let v3 = eval_binary(Value::Int(4), &BinaryOp::Equal, Value::Float(4.0)).unwrap();
        assert_eq!(v3, Value::Bool(true), "4 == 4.0（Int ⊂ Float）");
        let v4 = eval_binary(Value::Int(4), &BinaryOp::Equal, Value::Float(4.5)).unwrap();
        assert_eq!(v4, Value::Bool(false));
    }

    /// v0.38: numeric_cmp Float == Float.
    #[test]
    fn numeric_cmp_float_eq() {
        let v = numeric_cmp(Value::Float(1.5), Value::Float(1.5), |a, b| a == b).unwrap();
        assert_eq!(v, Value::Bool(true));
    }

    /// v0.103: numeric_cmp Int vs Float 提升比较（取代 v0.38 的 error）。
    #[test]
    fn numeric_cmp_int_float_promotes() {
        let v = numeric_cmp(Value::Int(1), Value::Float(2.0), |a, b| a < b).unwrap();
        assert_eq!(v, Value::Bool(true));
        let v2 = numeric_cmp(Value::Float(2.0), Value::Int(1), |a, b| a < b).unwrap();
        assert_eq!(v2, Value::Bool(false));
    }

    /// v0.38: typeck still routes Int literal to Type::Int.
    #[test]
    fn type_int_name() {
        assert_eq!(Type::Int.name(), "int");
        assert_eq!(Type::Float.name(), "float");
        assert_eq!(Type::Float.name(), "float");
    }

    // ─ v0.52 regression: json_to_value 空格 bug ────
    // pre-existing: parse_json_value 在 line 414 trim_start() 但 return 的 consumed
    // 不含 trim 字节数，导致 dict 内有空格时解析错位（"Expected ',' in dict"）
    // 这是 v0.51 P0-3 修 Send 派发时发现的（见 src/runtime/infra.rs:extract_send_tasks
    // 注释里 hand-write 解析以绕开此 bug）

    #[test]
    fn json_to_value_dict_no_space() {
        // 无空格 dict — 应正常解析
        let v = json_to_value(r#"{"a":1,"b":2}"#).unwrap();
        if let Value::Dict(m) = v {
            // v0.84: parse_json_number 区分 Int/Float — "1" → Int(1), "1.0" → Float(1.0)
            assert_eq!(m.get("a"), Some(&Value::Int(1)));
            assert_eq!(m.get("b"), Some(&Value::Int(2)));
        } else {
            panic!("expected Dict");
        }
    }

    #[test]
    fn json_to_value_dict_with_space() {
        // 带空格 dict — pre-existing bug 应 panic "Expected ',' in dict"
        // 修复后期望 pass
        let v = json_to_value(r#"{"a": 1, "b": 2}"#).unwrap();
        if let Value::Dict(m) = v {
            // v0.84: Int/Float 类型区分 — " 1" 和 " 2" 解析为 Int
            assert_eq!(m.get("a"), Some(&Value::Int(1)));
            assert_eq!(m.get("b"), Some(&Value::Int(2)));
        } else {
            panic!("expected Dict, got {:?}", v);
        }
    }

    #[test]
    fn json_to_value_list_with_space() {
        // 带空格 list — 同样应正常解析
        let v = json_to_value("[1, 2, 3]").unwrap();
        if let Value::List(items) = v {
            assert_eq!(items.len(), 3);
        } else {
            panic!("expected List");
        }
    }

    #[test]
    fn json_to_value_nested_with_space() {
        // 嵌套 dict + 空格
        let v = json_to_value(r#"{"a": {"b": [1, 2]}}"#).unwrap();
        if let Value::Dict(m) = &v
            && let Some(Value::Dict(inner)) = m.get("a")
            && let Some(Value::List(items)) = inner.get("b")
        {
            assert_eq!(items.len(), 2);
        } else {
            panic!("nested structure mismatch: {:?}", v);
        }
    }
}
