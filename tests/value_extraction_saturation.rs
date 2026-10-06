//! v0.104.6 D246 —— `Value` → 数字的**唯一提取点**：`flow::value_as_f64` /
//! `value_as_usize`。
//!
//! ## 缺陷
//!
//! D231 已在 `compress::json` 立过同样的规矩（「`Value` → `f64` 的唯一提取点」
//! /「新增数值提取**必须**走它，否则同样违约」），但那个收口住在 **`compress`
//! 里，`interpreter` 够不着**。于是 `ai_helpers::extract_usage` 直接手写
//! `if let Value::Float(n)`，**违反了 D231 自己立的约束**。
//!
//! ## 后果：token 预算机制整体失效且零症状
//!
//! `track_tokens` 是 token 预算检查的**唯一执行者**（per_call 上限、总量预算、
//! 告警阈值、`ai.tokens().calls()` 全靠它），而 `ai_chat.rs` 两条 chat 响应
//! 路径都经由 `extract_usage` 喂数：
//!
//! ```text
//! {"usage":{"prompt_tokens":1500,"completion_tokens":250}}     → (0, 0)   ← 真实 API 形态
//! {"usage":{"prompt_tokens":1500.0,"completion_tokens":250.0}} → (1500, 250)  ← 现实不会发生
//! ```
//!
//! 真实 API 的 token 数是**整数** JSON 数字 ⇒ `json_to_value` 产出 `Int`
//! （D129）⇒ 落到 `_ => 0`。**用户设了 token 预算，但它永远不触发、
//! `ai.tokens()` 恒显示 0、告警永不打印。**
//!
//! ⇒ 教训：收口的**位置**和收口本身一样重要。放在只有一部分调用方能到达的
//! 地方，等于没有收口 —— 它只会挡住「已经看见它」的那部分调用方。

use mora::compress::json::value_as_f64 as compress_value_as_f64;
use mora::flow::{value_as_f64, value_as_usize};
use mora::value::Value;

/// ① 两侧都必须接受 —— 本仓数字有两个来源（D98 dict 字面量给 `Float`，
/// D129 `json.parse` / 外部 API 给 `Int`），只认一侧就丢整列。
#[test]
fn d246_both_int_and_float_are_accepted() {
    assert_eq!(value_as_f64(&Value::Int(1500)), Some(1500.0));
    assert_eq!(value_as_f64(&Value::Float(1500.0)), Some(1500.0));
    assert_eq!(value_as_f64(&Value::Int(-5)), Some(-5.0));
    assert_eq!(value_as_f64(&Value::Float(-0.5)), Some(-0.5));
    // 非数值类型仍返回 None（收口不放宽类型判定）。
    assert_eq!(value_as_f64(&Value::String("1500".into())), None);
    assert_eq!(value_as_f64(&Value::Bool(true)), None);
    assert_eq!(value_as_f64(&Value::Nil), None);
}

/// ② `value_as_usize` 要求**非负可表示**；负数必须返回 `None`，
/// 而不是让 `as usize` 饱和成 0 让调用方以为「真的是 0」。
#[test]
fn d246_value_as_usize_rejects_negative_and_non_finite() {
    assert_eq!(value_as_usize(&Value::Int(0)), Some(0));
    assert_eq!(value_as_usize(&Value::Int(1500)), Some(1500));
    assert_eq!(value_as_usize(&Value::Float(1500.0)), Some(1500));
    assert_eq!(value_as_usize(&Value::Float(2.9)), Some(2), "应向零取整");

    assert_eq!(value_as_usize(&Value::Int(-1)), None, "负整数必须 None");
    assert_eq!(value_as_usize(&Value::Float(-1.0)), None, "负浮点必须 None");
    assert_eq!(value_as_usize(&Value::Float(f64::NAN)), None);
    assert_eq!(value_as_usize(&Value::Float(f64::INFINITY)), None);
    assert_eq!(value_as_usize(&Value::String("5".into())), None);
}

/// ③ `compress::json::value_as_f64` 仍转发到同一实现（D231 的公开路径不变）。
#[test]
fn d246_compress_path_still_works() {
    assert_eq!(compress_value_as_f64(&Value::Int(42)), Some(42.0));
    assert_eq!(compress_value_as_f64(&Value::Float(4.5)), Some(4.5));
    assert_eq!(compress_value_as_f64(&Value::Nil), None);
}

/// ④ 对照组：钉住「整数 JSON 数字 → `Value::Int`」这条**前提**。
///
/// 若哪天 `json_to_value` 改成整数也给 `Float`，D246 的判据就需要重新审视
/// —— 但 D230 / D231 的成因分析依然成立（那两个来源必须都能被接受）。
#[test]
fn d246_control_group_json_int_parses_to_int_value() {
    use mora::flow::json_to_value;
    let v = json_to_value(r#"{"usage":{"prompt_tokens":1500}}"#).expect("parse");
    let Value::Dict(m) = &v else {
        panic!("expected dict, got {v:?}")
    };
    let Some(Value::Dict(usage)) = m.get("usage") else {
        panic!("expected usage dict, got {v:?}")
    };
    assert!(
        matches!(usage.get("prompt_tokens"), Some(Value::Int(1500))),
        "整数 JSON 应产出 Value::Int(1500)，实得 {:?}",
        usage.get("prompt_tokens")
    );
    // 对照：带小数点才是 Float。
    let v2 = json_to_value(r#"{"x":1500.5}"#).expect("parse");
    let Value::Dict(m2) = &v2 else { panic!() };
    assert!(matches!(m2.get("x"), Some(Value::Float(_))));
}
