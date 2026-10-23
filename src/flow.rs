//! v0.20: 自由函数（自 interpreter.rs 抽出）。
//!
//! **Move-only refactor** — 代码自 src/interpreter.rs **迁移**（非复制）：
//! interpreter.rs 不再持有这些函数的副本，而是通过 `use crate::flow::*`
//! 重新导出。此处是唯一定义点。

use crate::common::{BinaryOp, Literal};
use crate::error::MoraError;
use crate::value::Value;

// v0.104.6 D468：数值塔自 flow.rs 拆出（数值语义自成一个模块，
// 见 `numeric/mod.rs` 的模块文档）。
//
// 拆分的理由不是"文件太大"，而是**数值语义需要独立的不变量**：
// 每个数值域（Int/Float/BigInt）必须拥有自己的精确算术，跨塔混算
// 必须经可表示性守卫。原先这些规则散落在 flow.rs 的各个 match 臂里，
// 且共用一条 `Fn(f64, f64) -> f64` 回调 —— **从签名上就不允许**
// Int 走精确路径，于是 `a - 0i` 与 `a + 0i` 在 2^53 边界给出不同结果。
//
// 拆出后 flow.rs 只保留**分流**职责：判断这是数值运算还是
// 字符串/列表运算；数值运算的具体语义全部委托给 `numeric`。
pub mod numeric;

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

pub fn eval_binary(left: Value, op: &BinaryOp, right: Value) -> Result<Value, MoraError> {
    match op {
        BinaryOp::Add => match (&left, &right) {
            // ── 数值加法：委托数值塔（D468）──
            //
            // 此前这一臂自己实现了一整套提升规则（Int/Float/BigInt 的
            // 六种组合 + `coerce_mixed`），与 `numeric_op` 的规则
            // **各写各的** —— 同一对操作数在 `Add` 与 `Sub` 下行为不同。
            // 现在两者共用数值塔的同一套分派。
            (Value::Int(_), Value::Int(_))
            | (Value::Float(_), Value::Float(_))
            | (Value::BigInt(_), Value::BigInt(_))
            | (Value::Int(_), Value::Float(_))
            | (Value::Float(_), Value::Int(_))
            | (Value::Int(_), Value::BigInt(_))
            | (Value::BigInt(_), Value::Int(_))
            | (Value::Float(_), Value::BigInt(_))
            | (Value::BigInt(_), Value::Float(_)) => numeric::eval(op, &left, &right),
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
                    //
                    // v0.104.6 D468：改用 `broadcast_with`（唯一实现）。
                    broadcast_with(
                        &BinaryOp::Add,
                        a.iter().zip(b.iter()).map(|(x, y)| (x.clone(), y.clone())),
                    )
                } else {
                    // v0.104.6：List 不可变，拼接走「取回 Vec → extend → 装回」
                    let mut merged = a.to_vec();
                    merged.extend(b.iter().cloned());
                    Ok(Value::List(merged.into()))
                }
            }
            // v0.17: 广播 - list + number
            //
            // v0.104.6 D22：逐元素递归回 `eval_binary` 派发。
            //
            // v0.104.6 D468：改用 `broadcast_with` —— 广播逻辑此前在
            // `Add` 分支与 `numeric_op` 里**各手写一份**（两份逻辑相同、
            // 注释各写一整套历史），现已收敛为**唯一实现**
            // `broadcast_with`（见下方）。
            //
            // 递归派发之后标量侧覆盖 Int/Float/BigInt，元素侧自动获得 `Add`
            // 支持的全部组合。真正无意义的元素对（如 `Bool + Float`）
            // 落 `unwrap_or(Nil)`，与既有约定一致。
            (Value::List(list), scalar @ (Value::Float(_) | Value::Int(_) | Value::BigInt(_))) => {
                broadcast_with(
                    &BinaryOp::Add,
                    list.iter().map(|i| (i.clone(), scalar.clone())),
                )
            }
            // v0.17: 广播 - number + list
            (scalar @ (Value::Float(_) | Value::Int(_) | Value::BigInt(_)), Value::List(list)) => {
                broadcast_with(
                    &BinaryOp::Add,
                    list.iter().map(|i| (scalar.clone(), i.clone())),
                )
            }
            _ => Err(MoraError::Other(
                "Operands must be two numbers, two strings, or two lists".to_string(),
            )),
        },
        // v0.104.6 D468：数值运算全部委托给数值塔模块。
        //
        // 此前 `Sub`/`Mul` 走 `numeric_op`（共享 f64 回调），`Div`/`Mod`
        // 走 `int_div`/`int_mod`（专用路径），`Add` 又是独立的一臂 ——
        // **三条路径、三套语义**。D468 统一为一条：数值塔按操作数
        // 类型分派到各自的域，域内保证精确性。
        //
        // **列表广播在委托之前处理** —— 广播是**容器语义**（逐元素
        // 递归回 `eval_binary`），不属于数值塔。数值塔只管标量。
        BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
            match (&left, &right) {
                // list op number
                (
                    Value::List(list),
                    scalar @ (Value::Float(_) | Value::Int(_) | Value::BigInt(_)),
                ) => broadcast_with(op, list.iter().map(|i| (i.clone(), scalar.clone()))),
                // number op list
                (
                    scalar @ (Value::Float(_) | Value::Int(_) | Value::BigInt(_)),
                    Value::List(list),
                ) => broadcast_with(op, list.iter().map(|i| (scalar.clone(), i.clone()))),
                // list op list（逐元素）
                (Value::List(a), Value::List(b)) => {
                    if a.len() != b.len() {
                        return Err(MoraError::Other(format!(
                            "List length mismatch: {} vs {}",
                            a.len(),
                            b.len()
                        )));
                    }
                    broadcast_with(
                        op,
                        a.iter().zip(b.iter()).map(|(x, y)| (x.clone(), y.clone())),
                    )
                }
                // 标量：委托数值塔
                _ => numeric::eval(op, &left, &right),
            }
        }
        BinaryOp::Equal => Ok(Value::Bool(values_equal(&left, &right))),
        BinaryOp::NotEqual => Ok(Value::Bool(!values_equal(&left, &right))),
        // v0.104.6 D468：比较同样委托给数值塔（跨塔精确）。
        //
        // 判据统一为「先求序，再由序推导关系」，因此
        // `>` / `>=` / `<` / `<=` 对同一组值**必然自洽**。
        // 旧实现里 `a > b` 为 false 而 `a >= b` 为 true
        // （f64 舍入导致），正是自洽性被破坏的表现。
        BinaryOp::Greater | BinaryOp::Less | BinaryOp::GreaterEqual | BinaryOp::LessEqual => {
            numeric::eval_order(op, &left, &right)
        }
    }
}

/// 逐元素广播：**唯一**的广播实现。
///
/// 每一对都回到 `eval_binary` 用**同一个 op** 走完整派发；
/// 无意义的元素对落 `Nil`（与既有约定一致）。
///
/// v0.104.6 D22：此前 `Add` 分支与 `numeric_op` 各手搓一份（两份逻辑
/// 相同、注释各写一整套历史）。
///
/// v0.104.6 D468：随着 `numeric_op` 删除，`Add` 分支也改用本函数 ——
/// 广播逻辑**全仓只剩这一处**。
///
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

pub fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Nil, Value::Nil) => true,
        // v0.104.6 D468：数值相等委托给数值塔（跨塔精确）。
        //
        // 判据与 `numeric::compare` 一致：装不下的 Int 与 Float
        // **必然不相等**（返回 false 而不是报错）—— 相等是布尔谓词，
        // 它的契约是「给一个答案」；而顺序比较是关系，它的契约是
        // 「给出正确的序」，装不下时报错才能维持 `>=` 与 `==` 的自洽。
        (Value::Int(_), Value::Int(_))
        | (Value::Float(_), Value::Float(_))
        | (Value::BigInt(_), Value::BigInt(_))
        | (Value::Int(_), Value::Float(_))
        | (Value::Float(_), Value::Int(_))
        | (Value::BigInt(_), Value::Float(_))
        | (Value::Float(_), Value::BigInt(_))
        | (Value::Int(_), Value::BigInt(_))
        | (Value::BigInt(_), Value::Int(_)) => numeric::equal(a, b),
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
        // v0.104.6 D484：Core 层闭包 —— 与 MIR 闭包**刻意区分**（两者函数体
        // 不同、执行器不同；混称 "closure" 会让「调用失败」难以定位）。
        Value::CoreClosure { .. } => "coreclosure",
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

    /// v0.38: Int < Int。
    #[test]
    fn eval_binary_int_lt() {
        let v = eval_binary(Value::Int(1), &BinaryOp::Less, Value::Int(2)).unwrap();
        assert_eq!(v, Value::Bool(true));
    }

    /// v0.75.44: eval_binary Equal(Int, Int) — values_equal 的 Int 分支
    /// （v0.38 引入 Int 变体时漏加，`4 == 4` 曾恒 false）。
    /// v0.103: 混合数值按 numeric tower 比较 —— `4 == 4.0` 为真（与
    /// `<=` 的结论一致；此前判否使 `<=` 与 `==` 矛盾）。
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

    /// v0.38: Float == Float。
    #[test]
    fn eval_binary_float_equal() {
        let v = eval_binary(Value::Float(1.5), &BinaryOp::Equal, Value::Float(1.5)).unwrap();
        assert_eq!(v, Value::Bool(true));
    }

    /// v0.103: Int vs Float 提升比较（取代 v0.38 的 error）。
    #[test]
    fn eval_binary_int_float_compares() {
        let v = eval_binary(Value::Int(1), &BinaryOp::Less, Value::Float(2.0)).unwrap();
        assert_eq!(v, Value::Bool(true));
        let v2 = eval_binary(Value::Float(2.0), &BinaryOp::Less, Value::Int(1)).unwrap();
        assert_eq!(v2, Value::Bool(false));
    }

    /// v0.104.6 D468：`eval_binary` 是**分流层** —— 数值运算必须与
    /// 数值塔模块给出**完全相同**的答案。
    ///
    /// 这条测试的作用是钉住「委托关系」：若将来有人绕过数值塔
    /// 在 `eval_binary` 里另写一套数值逻辑，这里会红。
    #[test]
    fn eval_binary_delegates_to_numeric_tower() {
        let cases: &[(Value, Value)] = &[
            (Value::Int(2), Value::Int(3)),
            (Value::Int(2), Value::Float(3.0)),
            (Value::Float(2.0), Value::Int(3)),
            (Value::Float(1.5), Value::Float(2.5)),
        ];
        for (l, r) in cases {
            for op in [
                BinaryOp::Add,
                BinaryOp::Sub,
                BinaryOp::Mul,
                BinaryOp::Div,
                BinaryOp::Mod,
            ] {
                let via_binary = eval_binary(l.clone(), &op, r.clone());
                let via_tower = numeric::eval(&op, l, r);
                assert_eq!(
                    via_binary.is_ok(),
                    via_tower.is_ok(),
                    "{op:?} on {l:?} / {r:?}: 委托层与数值塔的成败判定不一致"
                );
                if let (Ok(a), Ok(b)) = (&via_binary, &via_tower) {
                    assert_eq!(a, b, "{op:?} on {l:?} / {r:?}: 委托层与数值塔结果不一致");
                }
            }
        }
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
