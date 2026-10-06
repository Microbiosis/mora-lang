//! v0.75.51: builtin 模块聚合（P7，Rhai register_plugin/Koto workspace 思想）。
//!
//! 14 个 call_*_method + get_embedding 已按 domain 拆到独立文件：
//! file / event / sandbox / schedule / ai_tokens / ai / ccr / mock /
//! memory / exec / toolplane / skill / plan / mora。本文件仅以 mod 声明
//! 聚合 domain 文件。
//!
//! v0.92: 测试模块按 domain 拆到 `tests/` 子模块（P1.1 god module 拆分）。
//! 生产代码零残留，mod.rs 从 2260 行收缩为纯聚合声明 + 测试模块声明。

use super::*;
use crate::value::Value;

// ── v0.104.6 D153：**必选**数值 / 字符串实参的统一取值 ──
///
/// D150/D152 只覆盖了「可选」实参，**漏了值方法面**（`method_dispatch.rs`：
/// `xs.get(i)` / `xs.take(n)` / `xs.reshape(r,c)` / `xs.crush_json(max)` …）——
///
/// 那里的写法是
/// `.and_then(|v| match v { Value::Float(n) => …, _ => None }).ok_or("… requires …")`，
/// 于是 `Int` 实参落到 `_ => None`，报出一条**归因错误**的消息
/// （「requires a count argument」—— 实参明明传了，是个合法的 `Int`）。
//
///
/// 与可选版的区别只在**缺参**时的措辞：真的没传才说 requires。
pub(crate) fn required_num_arg(
    args: &[Value],
    idx: usize,
    callee: &str,
    what: &str,
) -> Result<f64, String> {
    match args.get(idx) {
        None | Some(Value::Nil) => Err(format!("{callee}: requires {what}")),
        Some(Value::Int(i)) => Ok(*i as f64),
        Some(Value::Float(n)) => Ok(*n),
        Some(other) => Err(format!(
            "{callee}: {what} must be a number, got {}",
            crate::compress::value_type_simple(other)
        )),
    }
}

/// v0.104.6 D153：必选**字符串**实参（`document.parse` 的 path）。
pub(crate) fn required_str_arg(
    args: &[Value],
    idx: usize,
    callee: &str,
    what: &str,
) -> Result<String, String> {
    match args.get(idx) {
        None | Some(Value::Nil) => Err(format!("{callee}: requires {what}")),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(other) => Err(format!(
            "{callee}: {what} must be a string, got {}",
            crate::compress::value_type_simple(other)
        )),
    }
}

// ── v0.104.6 D150：可选**数值**实参的统一取值 ──
///
/// 此前 6 处各写一遍
/// `if let Some(Value::Float(n)) = args.get(i) { … } else { 默认 }`，
/// 而 Mora 的数字**字面量**是 `Float`（D98）、`Value::Int` 只由 `len()` 等产生
/// —— 于是**合法的 `Int` 实参被静默丢弃**（exit 0、零诊断）。
/// `tea.run` 更是**反着来**（只认 `Int`），最自然的 `tea.run(a, 5)` 反而失效。
///
/// **必选**数值实参早就是对的：`math.rs::expect_number` 认 `Int`/`Float`/`BigInt`。
/// 缺陷只发生在**可选**实参上 —— 那里被迫写 `if let`，把「没传」与「类型不对」
/// 混成同一件事，于是漏掉了一侧。本助手把正确做法收成一处。
/// （`ai.rs` 的 `backoff_ms` 也早就两边都认，但它按 D59 **无 Mora 语法入口**。）
///
/// **缺失**（`None`）与 **`Value::Nil`** 都视为「没传」；其余类型报错并点名字段。
/// （`Nil` 放行是既有约定：`sandbox.containerize` 的 `cpu_cores` / `memory_mb`
/// 一直显式 `Value::Nil => {}`；`nil` 是 Mora 的 null，可选实参传 `nil` 是惯用法。）
/// 负数不在此处收紧 —— 各调用点的负数语义不同（见 CHANGELOG D150 记档）。
pub(crate) fn optional_num_arg(
    args: &[Value],
    idx: usize,
    callee: &str,
    what: &str,
) -> Result<Option<f64>, String> {
    match args.get(idx) {
        None | Some(Value::Nil) => Ok(None),
        Some(Value::Int(i)) => Ok(Some(*i as f64)),
        Some(Value::Float(n)) => Ok(Some(*n)),
        Some(other) => Err(format!(
            "{callee}: {what} must be a number, got {}",
            crate::compress::value_type_simple(other)
        )),
    }
}

// ── v0.104.6 D152：可选**字符串**实参的统一取值（D150 的对称件）──
///
/// `optional_num_arg` 收的是「默认同型」的情形（类型错就落回默认值）。
/// 但字符串实参更危险：有些调用点的 `else` 分支**根本不是默认值，
/// 而是另一个操作** —— 见 `plan.list`：传错类型的 name 会让「查某计划的步骤」
/// 静默变成「列出所有计划名」，exit 0、零诊断。
/// **缺失**（`None`）与 **`Value::Nil`** 都视为「没传」；其余类型报错（同 D150）。
pub(crate) fn optional_str_arg(
    args: &[Value],
    idx: usize,
    callee: &str,
    what: &str,
) -> Result<Option<String>, String> {
    match args.get(idx) {
        None | Some(Value::Nil) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(format!(
            "{callee}: {what} must be a string, got {}",
            crate::compress::value_type_simple(other)
        )),
    }
}

// ── 生产 domain 模块 ──
mod ai;
mod ai_tokens;
mod ccr;
mod event;
mod exec;
mod file;
pub mod linalg;
pub mod math;
mod memory;
mod mock;
mod mora;
mod plan;
mod sandbox;
mod schedule;
mod skill;
pub mod stats;
// v0.83: TEA runtime + transducer builtin
mod tea;
// v0.102: 声明式范式（逻辑式/关系式）目标原语
mod rel;
mod toolplane;
mod xform;

// ── 测试套件（按 domain 拆分，v0.92 P1.1）──
#[cfg(test)]
mod tests {
    mod ai; // v0.45 ai + v0.47 context
    mod audit; // v0.42.1 audit 持久化
    mod capability; // v0.42 capability (sandbox/key + sandbox.check_call)
    mod dag; // v0.47 dag
    mod heartbeat; // v0.47 heartbeat
    mod orchestrate; // v0.44 orchestrate block syntax
    mod plan; // v0.48 plan
    mod refine; // v0.48 refine
    mod sandbox; // v0.44 container_real
    mod skill; // v0.46 skill
    mod toolplane; // v0.45 toolplane
}
