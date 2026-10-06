//! v0.104.6 D264 —— `is_timestamp_pattern` 必须接受 `Int`（`json.parse` 的产物）。
//!
//! ## 缺陷
//!
//! `compress::detect::is_timestamp_pattern` 只匹配 `Value::String` 与
//! `Value::Float`，`Value::Int` 落 `_ => false`。而 Unix 秒级时间戳经
//! `json.parse` 读入就是 `Int`（D129）⇒
//!
//! ```text
//! int ts (json.parse)  -> array_type=Uniform      ← 压缩器完全不做针对性处理
//! float ts (literal)   -> array_type=TimeSeries
//! ```
//!
//! 同一份数据，**写法不同 ⇒ 压缩策略差一个量级**。与 D231 修的 `is_numeric`
//! 同族（同一次「Int/Float 双来源」教训）。
//!
//! ## 修法：只补 `Int`，范围刻意窄
//!
//! - **不加 `BigInt`**：时间戳不会是 BigInt，且 `to_f64` 要额外 import。
//! - **不改 `value_byte_size` 的 `_ => 32`**：那是 D227 `max_bytes` 硬上限的
//!   **安全方向**（高估字节数宁可早压缩也不溢出），注释明写「rough tag size」。
//!   本轮一度把它当缺陷去改，属于**误判有意设计**。
//!
//! ## 本轮两次归因错误（都记在这里，因为它们才是本轮真正的教训）
//!
//! 1. 补 `Int` 后 `float ts` 从 `TimeSeries` 变 `Uniform` ⇒ 我归咎于
//!    `value_byte_size` 的改动，**并据此把它回退了**。真因是我自己把
//!    `10_000_000_000.0` 打成 `10_000_000.000.0`（多了两个零 ⇒ 阈值变成
//!    `< 10.0`，所有时间戳都超）—— **一个编译期就能拦下的笔误，被我包装成
//!    了「回归」并去追查无关模块**。
//! 2. 顺带发现：`_ => 32` 不是疏漏而是**刻意的保守估计**。

use mora::compress::{ArrayType, CompressOptions, crush_json};
use mora::value::Value;
use std::collections::HashMap;

fn obj(v: Value) -> Value {
    let mut m = HashMap::new();
    m.insert("ts".to_string(), v);
    Value::Dict(m)
}

fn array_type_of(items: &[Value]) -> ArrayType {
    crush_json(items, 5, &CompressOptions::default()).array_type
}

/// ① 主判据：`json.parse` 风格的 **Int** 时间戳必须与 Float 版本**同策略**。
#[test]
fn d264_int_timestamps_match_float_timestamps() {
    let int_ts: Vec<Value> = (0..20)
        .map(|i| obj(Value::Int(1_700_000_000i64 + i * 60)))
        .collect();
    let flt_ts: Vec<Value> = (0..20)
        .map(|i| obj(Value::Float(1_700_000_000.0 + (i * 60) as f64)))
        .collect();

    assert_eq!(
        array_type_of(&int_ts),
        ArrayType::TimeSeries,
        "Int 时间戳（`json.parse` 的产物）应与 Float 版本得到同样的 TimeSeries 策略；\
         D264 修前是 `Uniform`（压缩器完全不做针对性处理）"
    );
    assert_eq!(
        array_type_of(&int_ts),
        array_type_of(&flt_ts),
        "同一语义的 Int / Float 时间戳必须落在**同一** array_type —— \
         写法不同不该让压缩策略差一个量级"
    );
}

/// ② 反向护栏：小整数**不得**被误判成时间戳。
///
/// 若阈值写错（例如 D264 期间我自己打出的 `10_000_000.000.0` ⇒ `< 10.0`），
/// 这一条会先红 —— 它是「阈值被打错」的第一道警报。
#[test]
fn d264_small_integers_are_not_timestamps() {
    for n in [0i64, 1, 42, 999, 999_999] {
        let items = vec![obj(Value::Int(n))];
        assert_ne!(
            array_type_of(&items),
            ArrayType::TimeSeries,
            "小整数 {n} 被误判成时间序列 —— 时间戳阈值可能被打错了"
        );
    }
}

/// ③ 对照组：ISO-8601 字符串时间戳（本来就该是 TimeSeries）。
#[test]
fn d264_control_group_iso_string_timestamps_still_detected() {
    let items: Vec<Value> = (0..20)
        .map(|i| {
            obj(Value::String(format!(
                "2023-11-{:02}T00:00:00Z",
                (i % 28) + 1
            )))
        })
        .collect();
    assert_eq!(
        array_type_of(&items),
        ArrayType::TimeSeries,
        "ISO 字符串时间戳本来就该被识别（对照组失效）"
    );
}
