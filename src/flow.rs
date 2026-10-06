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
        // v0.104.6 D358：**BigInt 必须与 Int/Float 同语义**。
        // 此前落进下方 `_ => true` 兜底 ⇒ `BigInt(0)` 被判为 **truthy**，
        // 而 `Int(0)` / `Float(0.0)` / `""` / `[]` / `{}` 全部 falsy。
        // 端到端确认（D374）：`[0n, 1n, 2n].filter(fn(x) x end)` 修前得
        // `[0n, 1n, 2n]`（0 被保留），与 Int/Float 版本不一致。
        Value::BigInt(b) => *b != num_bigint::BigInt::from(0),
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
/// v0.104.6 D198：把 `(f64, BigInt)` 这一对操作数**归一到同一类型**。
///
/// 规则（详见 `eval_binary` 里 `Float ⊕ BigInt` 分支处的说明）：
/// 1. float 侧是**整数值** → `(BigInt, BigInt)`，**精确**（顺带修回类型退化）；
/// 2. float 侧有小数、且 BigInt 能**无损**转 f64（往返相等）→ `(f64, f64)`，无损；
/// 3. 否则 → `Err`。因为「大整数 + 小数」在当前类型系统里**没有精确表示**
///    （Float 装不下、BigInt 存不了小数）。**宁可报错，不返回一个错的数。**
enum Coerced {
    Both(num_bigint::BigInt, num_bigint::BigInt),
    F64(f64, f64),
}

/// f64 能**精确**表示的整数上限：`2^53`。
const F64_EXACT_INT_LIMIT: f64 = 9_007_199_254_740_992.0;

/// 校验 float 侧能否在 Float ⊗ BigInt 混算中**精确**参与运算。
///
/// v0.104.6 D359：`Float ⊕ BigInt`（`eval_binary`）与
/// `Float ⊗ BigInt`（`numeric_op`，含 Sub/Mul/Div/Mod）此前**各写各的**：
/// - `Add` 有往返校验，BigInt 侧超范围会报错；
/// - 其余七个运算符**无守卫**，BigInt 侧走 `bigint_to_f64_lossy`
///   **无条件**转 f64。
///
/// 结果是同一个类型对、两种行为，且丢的是**最隐蔽**的那种错 ——
///
/// ```text
/// let f = 1e20
/// f + 1n   →  报错（D359 修前也是静默：加的 1 凭空消失）
/// f - 1n   →  1e20      ← 减的 1 凭空消失
/// f * 2n   →  2e20      ← 量级对、精度错，肉眼几乎无法察觉
/// 2n / f   →  2e-20
/// ```
///
/// **判据**：f 是整数值但 `|f| > 2^53` ⇒ 该整数**装不下** ⇒ 报错。
/// （f 是整数值且 `|f| <= 2^53` 时，转 BigInt 或转 f64 都精确，无需拦。）
fn check_float_exact_int(f: f64) -> Result<(), MoraError> {
    if f.is_finite() && f.fract() == 0.0 && f.abs() > F64_EXACT_INT_LIMIT {
        return Err(MoraError::Other(format!(
            "float({f}) 超出 f64 的精确表示范围（f64 只能精确表示 2^53 以内的\
             整数），与 bigint 混合运算会产生静默的数值损坏。\n\
             建议：整数运算请用 bigint 字面量（`<整数>n`）。"
        )));
    }
    Ok(())
}

fn coerce_mixed(f: f64, b: num_bigint::BigInt) -> Result<Coerced, String> {
    // v0.104.6 D359：整数值 float 提升为 BigInt 时，**必须校验 f 侧能精确
    // 表示**。此前守卫只有 `f.abs() < 9.0e18`（i64 范围），于是
    // `1e20 + 1n` 这类**超出 2^53** 的整数值 float 落到下面的「非整数」
    // 分支，被当成 `(f64, f64)` 处理 ⇒ `1.0` 加进 `1e20` 被 f64 精度吃掉，
    // **静默返回 `100000000000000000000.0`**（加的 1 凭空消失）。
    //
    // 同一函数里 `1e20 + 99999999999999999999n` 却被往返校验拦下报错
    // （L152）—— 一个静默错、一个明确报错，是**最坏的组合**。
    //
    // 修法：整数分支加上**往返相等**校验。`BigInt::from(f as i128) == f`
    // 是 f64 能否精确表示该整数的判据（2^53 以内恒真）。
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e18 {
        // f64 → i128 的 `as` 在超范围时**饱和**，故先用 i64 范围守住，
        // 再做一次「转回 f64 是否还是原值」的校验。
        let ib = num_bigint::BigInt::from(f as i64);
        if ib
            .to_string()
            .parse::<f64>()
            .map(|back| back == f)
            .unwrap_or(false)
        {
            return Ok(Coerced::Both(ib, b));
        }
        return Err(format!(
            "float({}) 超出 f64 的精确表示范围，与 bigint({}) 混合运算会产生\
             静默的数值损坏（f64 只能精确表示 2^53 以内的整数）。\n\
             建议：整数运算请用 bigint 字面量（`<整数>n`）。",
            f, b
        ));
    }
    // v0.104.6 D359：**判据与 `check_float_exact_int` 共用** ——
    // float 侧超精度时，无论 BigInt 侧多小，结果都无法精确表示。
    if f.is_finite() && f.fract() == 0.0 && f.abs() > F64_EXACT_INT_LIMIT {
        return Err(format!(
            "float({}) 超出 f64 的精确表示范围（f64 只能精确表示 2^53 以内的\
             整数），与 bigint({}) 混合运算会产生静默的数值损坏。\n\
             建议：整数运算请用 bigint 字面量（`<整数>n`）。",
            f, b
        ));
    }
    // 非整数 → 结果只能是 Float；那 BigInt 侧必须**无损**装得下。
    // 本版本的 num-bigint 没有 `TryFrom<f64>`，故用字符串往返做校验：
    // f64 → 整数 i128（`as` 在超范围时饱和，落到 i128::MAX/MIN）→ BigInt，
    // 与原值不等即说明**装不下**（宁可误报为「装不下」，不可误报为「装得下」）。
    let Ok(bf) = b.to_string().parse::<f64>() else {
        return Err(format!(
            "bigint({}) 无法用 f64 表示，与 float({}) 混合运算会产生静默的数值损坏。\n\
             建议：整数运算请用 bigint 字面量（`<整数>n`）。",
            b, f
        ));
    };
    if !bf.is_finite() || bf.fract() != 0.0 {
        return Err(format!(
            "bigint({}) 超出 f64 的精确表示范围，与 float({}) 混合运算会产生\
             静默的数值损坏（f64 只能精确表示 2^53 以内的整数）。\n\
             建议：整数运算请用 bigint 字面量（`<整数>n`）。",
            b, f
        ));
    }
    if num_bigint::BigInt::from(bf as i128) != b {
        return Err(format!(
            "bigint({}) 超出 f64 的精确表示范围，与 float({}) 混合运算会产生\
             静默的数值损坏（f64 只能精确表示 2^53 以内的整数）。\n\
             建议：整数运算请用 bigint 字面量（`<整数>n`）。",
            b, f
        ));
    }
    Ok(Coerced::F64(f, bf))
}

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
            // v0.104.6 D198：Float ⊕ BigInt —— 修前是「把 BigInt 转成 f64 再算」，
            // 对大值**静默给出完全错误的结果**：
            //
            // ```text
            // b = 1000000000000000000000000000000n     （10^30）
            // b + 1.0   →  1000000000000000019884624838656.0   ← 前 18 位全错，零提示
            // 12345n + 1 →  12346.0                          ← 值对，但**类型**退化成 Float
            // ```
            //
            // 而 `Int ⊕ BigInt`（上面两臂）一直是**精确**的 —— 所以问题**只**出在
            // float 这一侧。`value.rs` 写明的推广规则是「任一含 BigInt 时结果
            // 为 BigInt（最小惊讶）」，实现与文档**相反**。
            //
            // 现在的规则（**绝不静默给错数**）：
            // 1. float 若是**整数值** → 提升为 **BigInt**，精确（顺带修回类型退化）；
            // 2. 否则，若该 BigInt 能**无损**转回 f64（`BigInt::from(f) == b`）→
            //    结果 Float，且**确实无损**；
            // 3. 否则 → **报错**。因为「大整数 + 小数」在当前类型系统里
            //    **没有精确表示**（Float 装不下、BigInt 存不了小数），
            //    与其返回一个错的数，不如明说。
            (Value::Float(a), Value::BigInt(b)) => match coerce_mixed(*a, b.clone())? {
                Coerced::Both(fb, ib) => Ok(Value::BigInt(fb + ib)),
                Coerced::F64(x, y) => Ok(Value::Float(x + y)),
            },
            (Value::BigInt(a), Value::Float(b)) => match coerce_mixed(*b, a.clone())? {
                Coerced::Both(fb, ib) => Ok(Value::BigInt(ib + fb)),
                Coerced::F64(x, y) => Ok(Value::Float(y + x)),
            },
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
                            eval_binary(x.clone(), &BinaryOp::Add, y.clone()).unwrap_or(Value::Nil) // 不支持加法的元素对（如 dict+dict）→ Nil
                        })
                        .collect();
                    Ok(Value::List(result.into()))
                } else {
                    // v0.104.6：List 不可变，拼接走「取回 Vec → extend → 装回」
                    let mut merged = a.to_vec();
                    merged.extend(b.iter().cloned());
                    Ok(Value::List(merged.into()))
                }
            }
            // v0.17: 广播 - list + number
            //
            // v0.104.6 D22：改为与上面 `(List, List)` 臂**同构** —— 逐元素
            // 递归回 `eval_binary` 派发，而不是在这里手搓一张迷你表。
            //
            // 手搓版只 match 了 `Value::Float` 与 `Value::String`，其余一律
            // `_ => Value::Nil`，于是**静默把元素变成 Nil**：
            //
            // ```text
            // 修前： [1n, 2n] + 1.0        → List([Nil, Nil])     ← BigInt 全丢
            //       [int(1),int(2)] + 1.0  → List([Nil, Nil])     ← Int 全丢
            //       [true, false] + 1.0     → List([Nil, Nil])     ← Bool 变 Nil
            //       ['a'] + 1.0             → List([Nil])
            //       [[1,2]] + 1.0           → List([Nil])          ← 嵌套 list 变 Nil
            // ```
            //
            // 且标量侧只认 `Float`：`[1,2] + int("1")` 直接报
            // 「Operands must be two numbers…」。**`len()` 返回 `Int`**，所以
            // `[..] + len(xs)` 这种最自然的写法反而不行。
            //
            // 递归派发之后标量侧覆盖 Int/Float/BigInt，元素侧自动获得 `Add`
            // 支持的全部组合 —— 与 `(List, List)` 臂的能力对齐（那条臂本来就
            // 正确处理 BigInt：`[1n] + [2n]` → `BigInt(3)`）。
            // 真正无意义的元素对（如 `Bool + Float`）仍落 `unwrap_or(Nil)`，
            // 与上面那条臂的既有约定一致。
            (Value::List(list), scalar @ (Value::Float(_) | Value::Int(_) | Value::BigInt(_))) => {
                let result: Vec<Value> = list
                    .iter()
                    .map(|item| {
                        eval_binary(item.clone(), &BinaryOp::Add, scalar.clone())
                            .unwrap_or(Value::Nil)
                    })
                    .collect();
                Ok(Value::List(result.into()))
            }
            // v0.17: 广播 - number + list
            (scalar @ (Value::Float(_) | Value::Int(_) | Value::BigInt(_)), Value::List(list)) => {
                let result: Vec<Value> = list
                    .iter()
                    .map(|item| {
                        eval_binary(scalar.clone(), &BinaryOp::Add, item.clone())
                            .unwrap_or(Value::Nil)
                    })
                    .collect();
                Ok(Value::List(result.into()))
            }
            _ => Err(MoraError::Other(
                "Operands must be two numbers, two strings, or two lists".to_string(),
            )),
        },
        BinaryOp::Sub => numeric_op(left, right, |a, b| a - b, |a, b| a - b, &BinaryOp::Sub),
        BinaryOp::Mul => numeric_op(left, right, |a, b| a * b, |a, b| a * b, &BinaryOp::Mul),
        // v0.104.6 D19：整数除 / 模走**专用**路径，不进 `numeric_op`。
        //
        // `numeric_op` 的签名是 `F: Fn(f64, f64) -> f64`，所以整数除法
        // 实际是「浮点除法 + `.round() as i64`」，实测两处错：
        //
        // 1. **四舍五入而非截断** —— `5/2` 得 **3**、`1/2` 得 **1**、
        //    `-5/2` 得 **-3**（整数除法应向零截断：2 / 0 / -2）。
        //    任何 `count / 2` 的程序在 count 为奇数时都算错。
        // 2. **除零静默返回垃圾** —— `1/0` 得 `i64::MAX`（inf 经 Rust 饱和
        //    转换）、`0/0` 得 **0**（NaN → 0）、`5%0` 得 0，且会静默传播
        //    （`1/0 > 0` 为真）。浮点给 `inf`/`NaN` 是 IEEE 标准可辩护，
        //    **整数没有这个惯例**（Rust 的 `/` 会 panic）。
        //
        // 顺带修一处 **Int 与 BigInt 语义分叉**：BigInt 分支用
        // `result as i64`（截断），Int 分支用 `.round()` —— 同一组数
        // `7/2` 得 `Int(4)` 而 `7n/2n` 得 `BigInt(3)`。
        BinaryOp::Div => int_div(left, right),
        BinaryOp::Mod => int_mod(left, right),
        BinaryOp::Equal => Ok(Value::Bool(values_equal(&left, &right))),
        BinaryOp::NotEqual => Ok(Value::Bool(!values_equal(&left, &right))),
        BinaryOp::Greater => numeric_cmp(left, right, |o| o == NumOrd::Greater),
        BinaryOp::Less => numeric_cmp(left, right, |o| o == NumOrd::Less),
        BinaryOp::GreaterEqual => numeric_cmp(left, right, |o| o != NumOrd::Less),
        BinaryOp::LessEqual => numeric_cmp(left, right, |o| o != NumOrd::Greater),
    }
}

/// v0.104.6 D19：整数除法 —— 截断（向零），除零报错。
///
/// 任一操作数为 `Float` 时落回 `numeric_op`（IEEE 除法，给 `inf`/`NaN`）。
fn int_div(left: Value, right: Value) -> Result<Value, MoraError> {
    use Value::*;
    match (&left, &right) {
        (Int(a), Int(b)) => {
            if *b == 0 {
                return Err(MoraError::Other("division by zero".to_string()));
            }
            Ok(Int(a / b)) // Rust 的 `/` 对 i64 即向零截断
        }
        (BigInt(a), BigInt(b)) => {
            if b == &num_bigint::BigInt::from(0u8) {
                return Err(MoraError::Other("division by zero".to_string()));
            }
            Ok(BigInt(a / b))
        }
        // v0.104.6 D319：`Int ⊗ BigInt` / `BigInt ⊗ Int` 此前落进 `_` arm →
        // `numeric_op`，在那里 Int 被提升成 BigInt 后直接 `a / b` ——
        // **绕过了上面两条 arm 的零检查**，而 num-bigint 对零除数是
        // **panic**（不是返回错误）：
        //
        // ```text
        // print(7i / 0i)   → Runtime error (MIR): division by zero   exit 1  ✅
        // print(7n / 0n)   → Runtime error (MIR): division by zero   exit 1  ✅
        // print(7n / 0i)   → thread 'mora-main' panicked at
        //                    num-bigint-0.4.8/src/biguint/division.rs:174:9:
        //                    attempt to divide by zero               exit 101 ❌
        // print(7i / 0n)   → 同上 panic                                                 ❌
        // ```
        //
        // Rust panic 逃到用户面前（还带 backtrace 提示），而同族的整数除零
        // 是干净的 `MoraError`。此处补齐两条混合 arm，**只新增此前会 panic
        // 的那条路径，不改动任何既有行为**。
        //
        // 截断语义与 Int/Int 一致：Rust 的 i64 `/` 与 num-bigint 的 `/`
        // 都向零截断（`7/2` 得 3，负数同理）。
        (Int(a), BigInt(b)) => {
            if b == &num_bigint::BigInt::from(0u8) {
                return Err(MoraError::Other("division by zero".to_string()));
            }
            Ok(BigInt((&num_bigint::BigInt::from(*a)) / b))
        }
        (BigInt(a), Int(b)) => {
            if *b == 0 {
                return Err(MoraError::Other("division by zero".to_string()));
            }
            Ok(BigInt(a / &num_bigint::BigInt::from(*b)))
        }
        _ => numeric_op(left, right, |a, b| a / b, |a, b| a / b, &BinaryOp::Div),
    }
}

/// v0.104.6 D19：整数取模 —— 符号随被除数，除零报错。
fn int_mod(left: Value, right: Value) -> Result<Value, MoraError> {
    use Value::*;
    match (&left, &right) {
        (Int(a), Int(b)) => {
            if *b == 0 {
                return Err(MoraError::Other("modulo by zero".to_string()));
            }
            Ok(Int(a % b))
        }
        (BigInt(a), BigInt(b)) => {
            if b == &num_bigint::BigInt::from(0u8) {
                return Err(MoraError::Other("modulo by zero".to_string()));
            }
            Ok(BigInt(a % b))
        }
        // v0.104.6 D319：同 `int_div` —— 混合类型此前落进 `_` arm 绕过零检查，
        // num-bigint 对零除数 panic（exit 101）。详见 `int_div` 处的说明。
        (Int(a), BigInt(b)) => {
            if b == &num_bigint::BigInt::from(0u8) {
                return Err(MoraError::Other("modulo by zero".to_string()));
            }
            Ok(BigInt((&num_bigint::BigInt::from(*a)) % b))
        }
        (BigInt(a), Int(b)) => {
            if *b == 0 {
                return Err(MoraError::Other("modulo by zero".to_string()));
            }
            Ok(BigInt(a % &num_bigint::BigInt::from(*b)))
        }
        _ => numeric_op(left, right, |a, b| a % b, |a, b| a % b, &BinaryOp::Mod),
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
///
/// v0.104.6 D21：新增 `big_op` —— 任一操作数为 `BigInt` 时走**原生大数运算**。
///
/// 此前 BigInt 侧一律 `BigInt::from(f64_result as i64)`，而 `f64 as i64` 是
/// **饱和转换**：超出 i64 范围时静默变成 `i64::MAX`。实测：
///
/// ```text
/// 10^20 * 2n  →  BigInt(9223372036854775807)   应为 200000000000000000000
/// 10^20 - 1n  →  BigInt(9223372036854775807)   应为 99999999999999999999
/// ```
///
/// 而 `BinaryOp::Add` **另有**一条原生 BigInt 分支（`flow.rs:112` 的 `a + b`），
/// 所以 `10^20 + 1n` 是对的 —— 同一族里 Add 精确、Sub/Mul 饱和，能力不一致。
/// （D19 已把 Div/Mod 也改成原生，于是 Sub/Mul 成了仅剩的两个漏网。）
///
/// spec 与 EBNF（`literal` 的 `BIGINT = digits "n"  -- v0.91: 任意精度整数`）
/// 承诺的就是任意精度，这里让它对所有算术运算真正成立。
///
/// v0.104.6 D22：新增 `op_name` —— 广播时逐元素要把**同一个 op** 回传给
/// `eval_binary`，否则元素会被按 `Add` 派发（`[1,2] - 1.0` 变成加法）。
pub fn numeric_op<F, G>(
    left: Value,
    right: Value,
    op: F,
    big_op: G,
    op_name: &BinaryOp,
) -> Result<Value, MoraError>
where
    F: Fn(f64, f64) -> f64,
    G: Fn(&num_bigint::BigInt, &num_bigint::BigInt) -> num_bigint::BigInt,
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
        // v0.104.6 D21：BigInt 走**原生**大数运算（任意精度，不再经 f64+i64 饱和）。
        (BigInt(a), BigInt(b)) => Ok(BigInt(big_op(&a, &b))),
        (Int(a), BigInt(b)) => Ok(BigInt(big_op(&num_bigint::BigInt::from(a), &b))),
        (BigInt(a), Int(b)) => Ok(BigInt(big_op(&a, &num_bigint::BigInt::from(b)))),
        // Float ⊗ BigInt → Float（与 `Add` 的既有策略一致：BigInt 走 f64 后
        // 混合运算按 tower 提升为 Float）
        //
        // v0.104.6 D359：**float 侧超出 f64 精确整数范围时必须报错**。
        // 此前这两行无条件走 lossy 路径，于是：
        //
        // ```text
        // let f = 1e20
        // f - 1n   →  1e20        ← 减的 1 凭空消失
        // 1n - f   →  -1e20       ← 符号被吞
        // f * 2n   →  2e20        ← 精度丢失但量级对，肉眼难辨
        // 2n / f   →  2e-20
        // ```
        //
        // 全部**不报错、不警告**。而 `eval_binary` 的 `Add` 分支在
        // D359 加了同一守卫 —— 同一个类型对、两个运算符、两种行为。
        // 判据与 `coerce_mixed` 完全一致，抽成共用函数以免再分叉。
        (Float(a), BigInt(b)) => {
            check_float_exact_int(a)?;
            Ok(Float(op(a, bigint_to_f64_lossy(&b))))
        }
        (BigInt(a), Float(b)) => {
            check_float_exact_int(b)?;
            Ok(Float(op(bigint_to_f64_lossy(&a), b)))
        }
        // v0.103: numeric tower — Int ⊂ Float，混合提升为 Float（与 typeck
        // 的 Numeric 约束一致；此前报 Rust-strict 错误，属契约分叉）。
        (Int(a), Float(b)) => Ok(Float(op(a as f64, b))),
        (Float(a), Int(b)) => Ok(Float(op(a, b as f64))),
        // v0.17: 广播操作 - list op number
        //
        // v0.104.6 D22：与 `eval_binary` 的 `Add` 分支**同构** —— 逐元素递归
        // 回 `eval_binary` 派发，而不是手搓只认 `Float` 的迷你表。
        //
        // 手搓版把非 Float 元素一律变成 `Nil`（修前 `[1n,2n] - 1.0` →
        // `[Nil, Nil]`），且标量侧只认 `Float`（修前 `[1,2] - len(xs)` 直接报
        // 「Operands must be numbers」，而 `len()` 返 `Int`）。
        (Value::List(list), scalar @ (Value::Float(_) | Value::Int(_) | Value::BigInt(_))) => {
            broadcast_with(op_name, list.iter().map(|i| (i.clone(), scalar.clone())))
        }
        // v0.17: 广播操作 - number op list
        (scalar @ (Value::Float(_) | Value::Int(_) | Value::BigInt(_)), Value::List(list)) => {
            broadcast_with(op_name, list.iter().map(|i| (scalar.clone(), i.clone())))
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
            broadcast_with(
                op_name,
                a.iter().zip(b.iter()).map(|(x, y)| (x.clone(), y.clone())),
            )
        }
        _ => Err(MoraError::Other("Operands must be numbers".to_string())),
    }
}

/// 逐元素广播：每一对都回到 `eval_binary` 用**同一个 op** 走完整派发；
/// 无意义的元素对落 `Nil`（与既有约定一致）。
///
/// v0.104.6 D22：这是**唯一**的广播实现，`Add` 分支与 `numeric_op` 共用 ——
/// 修前两处各手搓一张表，都只 match `Float`，其余静默变 `Nil`，于是
/// `[1n,2n] - 1.0` → `[Nil, Nil]`、`[1,2] + len(xs)` 直接报错。
/// 递归派发之后，任何「该 op 能处理」的元素组合自动获得广播能力。
fn broadcast_with(
    op: &BinaryOp,
    pairs: impl Iterator<Item = (Value, Value)>,
) -> Result<Value, MoraError> {
    let result: Vec<Value> = pairs
        .map(|(x, y)| eval_binary(x, op, y).unwrap_or(Value::Nil))
        .collect();
    Ok(Value::List(result.into()))
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
/// 数值比较得到的三种关系。
///
/// v0.104.6 D199：改用「关系」而不是 `Fn(f64, f64) -> bool` 作为 `numeric_cmp`
/// 的回调 —— 后者逼着**所有**组合都先转成 f64，于是 BigInt 之间的比较
/// 也被降级（见 `numeric_cmp` 里的说明）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NumOrd {
    Less,
    Equal,
    Greater,
}

fn ord_f64(a: f64, b: f64) -> Option<NumOrd> {
    a.partial_cmp(&b).map(|o| match o {
        std::cmp::Ordering::Less => NumOrd::Less,
        std::cmp::Ordering::Equal => NumOrd::Equal,
        std::cmp::Ordering::Greater => NumOrd::Greater,
    })
}

/// v0.104.6 D199：BigInt 之间的比较**必须精确**，不得经 f64。
///
/// 原实现是
/// `op(bigint_to_f64_lossy(&a), bigint_to_f64_lossy(&b))` —— 函数名自己
/// 就写着 `lossy`，而它**上一行的注释**却写着「两个 BigInt 之间保持精确 /
/// 大数比较（不经 f64，避免精度丢失）」。**实现与自己的注释相反。**
///
/// 实测（`b = 10^30`）：
///
/// ```text
/// b > (b-1)   →  false    ← 错！两个值舍入到同一个 f64，严格大于变 false
/// (b + 1) > b  →  false    ← 错！同上
/// ```
///
/// 即**大整数比较静默给出错误的 false** —— 比 D197 的解析降级更隐蔽，
/// 因为错的是**比较结果**本身，代码看起来完全正常。
pub fn numeric_cmp<F>(left: Value, right: Value, op: F) -> Result<Value, MoraError>
where
    F: Fn(NumOrd) -> bool,
{
    use Value::*;
    match (left, right) {
        (Int(a), Int(b)) => Ok(Bool(match ord_f64(a as f64, b as f64) {
            Some(o) => op(o),
            // NaN：四种比较一律为 false（与 IEEE 一致）
            None => false,
        })),
        (Float(a), Float(b)) => Ok(Bool(match ord_f64(a, b) {
            Some(o) => op(o),
            None => false,
        })),
        (Int(a), Float(b)) => Ok(Bool(match ord_f64(a as f64, b) {
            Some(o) => op(o),
            None => false,
        })),
        (Float(a), Int(b)) => Ok(Bool(match ord_f64(a, b as f64) {
            Some(o) => op(o),
            None => false,
        })),
        // v0.104.6 D199：下面五条全部**不走 f64**（或只在确实无损时走）。
        (BigInt(a), BigInt(b)) => Ok(Bool(op(big_ord(&a, &b)))),
        (Int(a), BigInt(b)) => Ok(Bool(op(big_ord(&num_bigint::BigInt::from(a), &b)))),
        (BigInt(a), Int(b)) => Ok(Bool(op(big_ord(&a, &num_bigint::BigInt::from(b))))),
        // BigInt ⊕ Float：要比较就**必须**能无损装进 f64，否则结果是错的
        // （`b > 1.5` 在 b 超限时会被舍入成相等而给出 false）。
        // 宁可报错，不返回错的比较结果 —— 与 D198 的算术侧同一原则。
        (Float(a), BigInt(b)) => {
            b_f64_check(a, &b)?;
            Ok(Bool(match ord_f64(a, bigint_to_f64_lossy(&b)) {
                Some(o) => op(o),
                None => false,
            }))
        }
        (BigInt(a), Float(b)) => {
            b_f64_check(b, &a)?;
            Ok(Bool(match ord_f64(bigint_to_f64_lossy(&a), b) {
                Some(o) => op(o),
                None => false,
            }))
        }
        _ => Err(MoraError::Other("Operands must be numbers".to_string())),
    }
}

/// BigInt 之间的精确序关系（`num_bigint::BigInt` 自带 `Ord`）。
fn big_ord(a: &num_bigint::BigInt, b: &num_bigint::BigInt) -> NumOrd {
    match a.cmp(b) {
        std::cmp::Ordering::Less => NumOrd::Less,
        std::cmp::Ordering::Equal => NumOrd::Equal,
        std::cmp::Ordering::Greater => NumOrd::Greater,
    }
}

/// BigInt 与 Float 比较前的可表示性检查。
fn b_f64_check(f: f64, b: &num_bigint::BigInt) -> Result<(), MoraError> {
    let bf = bigint_to_f64_lossy(b);
    let ok = bf.is_finite() && bf.fract() == 0.0 && num_bigint::BigInt::from(bf as i128) == *b;
    if ok {
        return Ok(());
    }
    Err(MoraError::Other(format!(
        "bigint({}) 无法用 f64 精确表示，与 float({}) 比较会给出**错误**的结论。\n\
         建议：与 bigint 比较时用整数（把 float 写成整数值或改用 bigint 字面量）。",
        b, f
    )))
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
        // v0.104.6 D20：BigInt 与 Int/Float 混比走 tower（提升为 f64）。
        // 此前这两组**漏加**，于是：
        //   `4n == 4.0`    → Bool(false)   ← 明明 `4 <= 4.0` 为真
        //   `int("4") == 4n` → Bool(false)
        // 而 `numeric_cmp`（`<` `>` `<=` `>=`）同一时刻报「Operands must be
        // numbers」—— 出现「顺序比较报错、等值比较说不等」的自相矛盾。
        // v0.104.6 D199：`values_equal` 的 BigInt 混比同样**经 f64 降级**，
        // 与刚修好的 `numeric_cmp` 是同一个洞。改精确：
        //
        // - `Int ⊕ BigInt`：整数两边都提升为 BigInt，**精确**。
        // - `Float ⊕ BigInt`：float 带小数时**必然**不等于整数 → 直接 `false`；
        //   float 是整数值时提升为 BigInt 精确比较。（真与天文数字相等的
        //   极端情形退回 lossy 比较 —— 那个量级下两者都远超 i128。）
        (Value::BigInt(a), Value::Float(b)) | (Value::Float(b), Value::BigInt(a)) => {
            if b.fract() != 0.0 {
                return false; // 非整数 float 不可能等于整数
            }
            if b.abs() < 1.7e38 {
                num_bigint::BigInt::from(*b as i128) == *a
            } else {
                bigint_to_f64_lossy(a) == *b
            }
        }
        (Value::BigInt(a), Value::Int(b)) | (Value::Int(b), Value::BigInt(a)) => {
            num_bigint::BigInt::from(*b) == *a
        }
        (Value::String(a), Value::String(b)) => a == b,
        // v0.104.6 D16：补 `Char` 分支 —— 此前**漏了**，落到末尾 `_ => false`，
        // 于是 `'a' == 'a'` 得 false（即使两个操作数都是同一个字符）。
        //
        // 根因是两个相等函数分叉：`Value::PartialEq`（value.rs:463）**有** Char
        // 分支，`flow::values_equal`（`==` 运算符走这里）没有。实测：
        //
        // ```text
        // 'a' == 'a'   → Bool(false)   ← 修前
        // "a" == "a"   → Bool(true)    ← 对照正常
        // 1   == 1     → Bool(true)    ← 对照正常
        // ```
        //
        // 本函数的文档本就写着「与 `Value::eq` 的 … arm 保持一致」（v0.91 补
        // BigInt 时就是这么对齐的），Char 属同类漏项。spec :1312 也明列
        // 「`char` 字符相等」。
        (Value::Char(a), Value::Char(b)) => a == b,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        // v0.104.6 D391：容器比较必须把**元素**交回 `container_elem_eq`，
        // 不能退回 `Value::eq`（`List::eq` / `HashMap::eq` 内部就是这么比的）。
        //
        // 修前实测（真实 CLI，值取自 `json.parse` —— typeck 的列表同质性
        // 规则使**字面量**造不出混比列表，只有解析出来的值才有 Int 元素）：
        //
        // ```text
        // j[0]     == 1.0   → true    ← 标量走数值塔
        // j        == [1.0, 2.0] → false   ← 容器丢掉数值塔
        // d.get("a") == 1.0 → true
        // d        == {"a": 1.0} → false
        // ```
        //
        // ⇒ 同一个值**单独相等、放进容器就不等**。这正是 v0.103 / D20 / D199
        // 一路在标量侧修掉的那个洞，只是当时只改了标量臂，容器臂漏了。
        (Value::List(a), Value::List(b)) => {
            a.len() == b.len()
                && (0..a.len()).all(|i| match (a.get(i), b.get(i)) {
                    (Some(x), Some(y)) => container_elem_eq(x, y),
                    _ => false,
                })
        }
        (Value::Dict(a), Value::Dict(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, va)| b.get(k).is_some_and(|vb| container_elem_eq(va, vb)))
        }
        // Conversation 不支持相等比较——比较引用无意义
        _ => false,
    }
}

/// 容器**元素**的相等判定 —— v0.104.6 D391。
///
/// 取 `values_equal(..) || a == b` 的**并集**是有意为之，不是「保险起见」：
///
/// | 类型 | `values_equal` | `Value::eq` | 本函数 | 相对修前 |
/// |---|---|---|---|---|
/// | `Int(1)` ⊕ `Float(1.0)` | true | **false** | true | **补上数值塔**（本轮要修的）|
/// | `Cons` / `Code` / `Curry` / `Document` / `Tea*` | **false**（落 `_`）| true | true | **不变** |
/// | `Closure` / `Builtin` / `Task` | false | false | false | 不变 |
///
/// 之所以不能直接 `values_equal` 递归：`values_equal` 只覆盖
/// Nil/数值/String/Char/Bool/List/Dict，**没有** `Cons` 等变体的臂。
/// 若容器改走纯 `values_equal`，`[Cons{..}] == [Cons{..}]` 会从 true 变成
/// false —— 那是**收窄**既有行为，属于引入新缺陷。
fn container_elem_eq(a: &Value, b: &Value) -> bool {
    values_equal(a, b) || a == b
}

/// v0.104.6 D246：`Value` → 数字的**唯一**提取点。
///
/// D231 已在 `compress::json` 立过同样的规矩（「`Value` → `f64` 的唯一提取点」
/// /「新增数值提取**必须**走它，否则同样违约」），但它住在 `compress` 里，
/// **`interpreter` 够不着** —— 于是 `ai_helpers::extract_usage` 直接手写
/// `if let Value::Float(n)`，**违反了 D231 自己立的约束**。
///
/// ⇒ 收口必须放在**所有**需要它的地方都能到达的位置，而不只是最先发现它
/// 的那个模块。
///
/// 本仓数字有两个来源，**两侧必须都接受**，否则整列静默丢失：
/// - dict 字面量给 `Float`（D98）
/// - `json.parse` / 外部 API 响应给 `Int`（D129）
///
/// [`crate::compress::json::value_as_f64`] 现在转发到此，保持原路径可用。
pub fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(n) => Some(*n),
        Value::BigInt(b) => b.to_string().parse::<f64>().ok(),
        _ => None,
    }
}

/// 同 [`value_as_f64`]，但要求结果**非负且可表示为 `usize`**，否则 `None`。
///
/// 负数返回 `None`（而不是让 `as usize` 饱和成 0）—— 调用方据此报错或取
/// 显式默认值，**不替它猜**。`NaN` / `±inf` 同样返回 `None`。
///
/// **小数向零截断是本收口的既定约定**（`2.9 → 2`），由
/// `tests/value_extraction_saturation.rs` 显式钉住。理由：调用方都是
/// 「时长 / 步数 / 个数 / 超时」这类**计数**参数，小数没有物理意义，
/// 取整比报错更贴近「用户算错了但意思明确」的处境。
///
/// ⚠ 因此**不要**把它当成「必须是整数」的检查用 —— 错误消息写了
/// "integer" 的调用方必须**自己**补 `fract() == 0.0`，否则消息与行为矛盾。
/// v0.104.6 D329 正是因此只改了 `stats.histogram` 一处：它的签名
/// （`docs/mora-spec.md:981` 写 `list, int`）与错误消息都承诺 integer，
/// 而本收口不提供这个保证。
pub fn value_as_usize(v: &Value) -> Option<usize> {
    let n = value_as_f64(v)?;
    if !n.is_finite() || n < 0.0 {
        return None;
    }
    Some(n as usize)
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
// v0.104.6 D209：同时导出共享的 JSON 字符串转义器 —— 仓库里 6 处手写
// 转义链（ai_chat / ai_helpers / compress / http_server）改用它，
// 从此只有**一份**转义规则。
pub use json::{escape_json_string, json_to_value, value_to_json}; // 保持 flow::json_to_value 路径

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
        let v = numeric_op(l, r, |a, b| a + b, |a, b| a + b, &BinaryOp::Add).unwrap();
        assert_eq!(v, Value::Int(5));
    }

    /// v0.38: Float + Float = Float.
    #[test]
    fn numeric_tower_float_plus_float_yields_float() {
        let l = Value::Float(1.5);
        let r = Value::Float(2.5);
        let v = numeric_op(l, r, |a, b| a + b, |a, b| a + b, &BinaryOp::Add).unwrap();
        assert_eq!(v, Value::Float(4.0));
    }

    /// v0.103: Int op Float 提升为 Float（取代 v0.38 的 strict error）。
    /// 旧断言（`is_err`）与 typeck 的 Numeric 提升、spec §15.1、以及本文件
    /// 的 BigInt 混算分支三处矛盾；此处断言新的 tower 语义。
    #[test]
    fn numeric_tower_int_plus_float_promotes() {
        let l = Value::Int(2);
        let r = Value::Float(3.0);
        let v = numeric_op(l, r, |a, b| a + b, |a, b| a + b, &BinaryOp::Add).unwrap();
        assert_eq!(v, Value::Float(5.0), "Int + Float → Float(提升)");
    }

    /// v0.103: Float op Int 对称提升为 Float。
    #[test]
    fn numeric_tower_float_plus_int_promotes() {
        let l = Value::Float(2.0);
        let r = Value::Int(3);
        let v = numeric_op(l, r, |a, b| a + b, |a, b| a + b, &BinaryOp::Add).unwrap();
        assert_eq!(v, Value::Float(5.0), "Float + Int → Float(提升)");
    }

    /// v0.38: Float + Float → Float via numeric_op (补充用例：整数 Float)。
    #[test]
    fn numeric_tower_float_plus_float_integer_values() {
        let l = Value::Float(2.0);
        let r = Value::Float(3.0);
        let v = numeric_op(l, r, |a, b| a + b, |a, b| a + b, &BinaryOp::Add).unwrap();
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
        let v = numeric_cmp(Value::Int(1), Value::Int(2), |o| o == NumOrd::Less).unwrap();
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
        let v = numeric_cmp(Value::Float(1.5), Value::Float(1.5), |o| o == NumOrd::Equal).unwrap();
        assert_eq!(v, Value::Bool(true));
    }

    /// v0.103: numeric_cmp Int vs Float 提升比较（取代 v0.38 的 error）。
    #[test]
    fn numeric_cmp_int_float_promotes() {
        let v = numeric_cmp(Value::Int(1), Value::Float(2.0), |o| o == NumOrd::Less).unwrap();
        assert_eq!(v, Value::Bool(true));
        let v2 = numeric_cmp(Value::Float(2.0), Value::Int(1), |o| o == NumOrd::Less).unwrap();
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
