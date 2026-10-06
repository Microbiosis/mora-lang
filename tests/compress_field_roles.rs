//! v0.104.6 D368 —— `src/compress/detect.rs` 的**字段角色判定矩阵**
//! （否定轮，无产品变更）
//!
//! D367 测了 `crush_json` 的**目标计算**与**预算收口**，
//! 本轮钉它的**前置**：`detect.rs` 的字段角色推断
//! —— 角色判错会导致选错压缩策略，且**不报错**。
//!
//! ## `detect` 模块是**私有**的，只能经 `CrushResult.fields` 观察
//!
//! `compress/mod.rs:226` 是 `mod detect;`（非 `pub mod`），
//! `detect_field_role` / `extract_field_stats` / `detect_array_type`
//! 都**无法从外部直接调用**。可观测路径只有
//! `json::crush_json` 返回的 `CrushResult { fields, array_type }`
//! （`mod.rs:234` 有 `pub use json::{... FieldRole, FieldStats ...}`）。
//!
//! ## 观测的前提：`target < items.len()`
//!
//! `json.rs:284` 有两个直通条件：
//!
//! ```rust
//! let short_passthrough =
//!     items.len() <= 5 || (items.len() <= target && options.strategy != "lossless");
//! ```
//!
//! 首版探针两次都踩空：
//! - 第一次只造 3-5 条 ⇒ `items.len() <= 5` 短路；
//! - 第二次造 12 条但 `target = n*10` ⇒ `items.len() <= target` 短路。
//!
//! ⇒ 两次都得到 `fields=[]`，看起来像「字段检测没实现」。
//! **必须**给 `target = n/2`。
//!
//! ## 判定顺序：`Temporal → Error → Anomaly → Score → Id → Generic`
//!
//! `Temporal`/`Error`/`Anomaly` **必须先于** `Id`，
//! 否则高唯一性数值/字符串都会被误判为 Id（`detect.rs:83-84` 注释）。

use std::collections::HashMap;

use mora::compress::json::crush_json;
use mora::compress::{CompressOptions, FieldRole};

/// 造 n 条记录（一个被测字段 + 一个 `pad` 填充字段），
/// 跑一次 `crush_json` 并返回「字段名 → 角色」。
fn roles_of(field: &str, vals: Vec<mora::value::Value>) -> Vec<(String, FieldRole)> {
    let n = vals.len();
    let items: Vec<mora::value::Value> = (0..n)
        .map(|i| {
            let mut m = HashMap::new();
            m.insert(field.to_string(), vals[i].clone());
            m.insert("pad".to_string(), mora::value::Value::Int(i as i64));
            mora::value::Value::Dict(m)
        })
        .collect();
    // target = n/2 ⇒ 避开「items <= 5」与「items <= target」两条直通
    let r = crush_json(&items, n / 2, &CompressOptions::default());
    // `FieldRole` 没实现 `Ord`，按字段名排序（`extract_field_stats`
    // 内部已按 key 排序，这里只是让断言输出稳定）。
    let mut out: Vec<(String, FieldRole)> =
        r.fields.iter().map(|f| (f.name.clone(), f.role)).collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn role_of(field: &str, vals: Vec<mora::value::Value>) -> FieldRole {
    roles_of(field, vals)
        .into_iter()
        .find(|(n, _)| n == field)
        .map(|(_, r)| r)
        .unwrap_or(FieldRole::Generic)
}

fn f(x: f64) -> mora::value::Value {
    mora::value::Value::Float(x)
}
fn i(x: i64) -> mora::value::Value {
    mora::value::Value::Int(x)
}
fn s(x: &str) -> mora::value::Value {
    mora::value::Value::String(x.to_string())
}

/// **ISO-8601 字符串** 与 **10 位秒级时间戳** ⇒ `Temporal`。
///
/// 后者是 D264 专门补的（`json.parse` 读入的整数列此前落 `_ => false`）。
#[test]
fn d368_iso_and_epoch_seconds_are_temporal() {
    let iso = role_of(
        "t",
        (0..12)
            .map(|k| s(&format!("2024-01-{:02}T00:00:00Z", k + 1)))
            .collect(),
    );
    assert_eq!(iso, FieldRole::Temporal, "ISO-8601 应判为 Temporal");

    let epoch_s = role_of("ts", (0..12).map(|k| i(1_700_000_000 + k)).collect());
    assert_eq!(
        epoch_s,
        FieldRole::Temporal,
        "10 位秒级时间戳（D264 补的分支）应判为 Temporal"
    );
}

/// **UUID 与高唯一性字符串** ⇒ `Id`。
#[test]
fn d368_uuid_and_high_uniqueness_strings_are_id() {
    let uuid = role_of(
        "id",
        (0..12)
            .map(|k| s(&format!("550e8400-e29b-41d4-a716-4466554400{k:02}")))
            .collect(),
    );
    assert_eq!(uuid, FieldRole::Id, "UUID 应判为 Id");

    let uniq = role_of("name", (0..12).map(|k| s(&format!("user_{k}"))).collect());
    assert_eq!(uniq, FieldRole::Id, "高唯一性字符串应判为 Id");
}

/// **错误字段**：名含关键字、或值含关键字 ⇒ `Error`。
///
/// 两条路径都要测 —— `detect_error`（`detect.rs:143-145`）是
/// 名字匹配 + 值匹配两条 `or`。
#[test]
fn d368_error_detection_covers_name_and_value() {
    let by_name = role_of("error_code", (0..12).map(i).collect());
    assert_eq!(by_name, FieldRole::Error, "字段名含 `error` 应判为 Error");

    let by_value = role_of(
        "msg",
        (0..12)
            .map(|k| {
                if k % 3 == 0 {
                    s("ERROR")
                } else {
                    s(&format!("ok{k}"))
                }
            })
            .collect(),
    );
    assert_eq!(by_value, FieldRole::Error, "值含 `ERROR` 应判为 Error");
}

/// **有界数值区间** ⇒ `Score`。
///
/// `detect.rs:127` 的规则：`(0..1 且 span > 0.01)` 或 `(0..100 且 span > 1)`。
#[test]
fn d368_bounded_numeric_ranges_are_score() {
    let unit = role_of("v", (0..12).map(|k| f(k as f64 / 20.0)).collect());
    assert_eq!(unit, FieldRole::Score, "0..1 区间应判为 Score");

    let pct = role_of("v", (0..12).map(|k| f(k as f64 * 8.0)).collect());
    assert_eq!(pct, FieldRole::Score, "0..100 区间应判为 Score");
}

/// **常数列 ⇒ `Generic`**（不是 `Constant`）。
///
/// `FieldRole::Constant` 变体存在（`json.rs:65` 注释「所有项相同」），
/// 但 `detect.rs:83-90` 的检测链里**没有**对应分支 ——
/// 常数列 `span = 0` ⇒ Score 不成立 ⇒ 落 Id（有 `is_sequential_numeric`
/// 但常数不递增）⇒ 落 `Generic`。
#[test]
fn d368_constant_columns_fall_through_to_generic() {
    let cn = role_of("v", (0..12).map(|_| i(7)).collect());
    assert_eq!(cn, FieldRole::Generic, "常数整数列落 Generic");

    let cf = role_of("v", (0..12).map(|_| f(0.5)).collect());
    assert_eq!(cf, FieldRole::Generic, "常数浮点列落 Generic");
}

/// **布尔列 ⇒ `Generic`**（非数值、非字符串）。
#[test]
fn d368_bool_column_is_generic() {
    let r = role_of(
        "flag",
        (0..12)
            .map(|k| mora::value::Value::Bool(k % 2 == 0))
            .collect(),
    );
    assert_eq!(r, FieldRole::Generic, "布尔列应落 Generic");
}

/// **Int 与 Float 混列仍算数值**（D231 的修复）。
///
/// 修前 `matches!(v, Value::Float(_))` ⇒ `json.parse` 读入的整数列被判
/// **非数值**，角色推断 / range / outlier / 策略选择全部失准。
#[test]
fn d368_int_and_float_mixed_column_is_still_numeric() {
    let r = role_of(
        "v",
        (0..12)
            .map(|k| if k % 2 == 0 { i(k) } else { f(k as f64) })
            .collect(),
    );
    // 0..11 的整数区间 ⇒ Score（证明 `is_numeric` 为真）
    assert_eq!(r, FieldRole::Score, "Int/Float 混列应仍被当作数值列");
}

/// **`Temporal` 必须先于 `Id`** —— 高唯一性时间戳不应被误判为 Id。
///
/// 这是 `detect.rs:83-84` 注释里写明的设计：
/// 「Temporal/Error/Anomaly 必须先于 Id, 否则高唯一性数值/字符串都被误判为 Id」。
#[test]
fn d368_temporal_wins_over_id_for_timestamps() {
    // 随机（不连续）的秒级时间戳 ⇒ is_sequential_numeric 为假，
    // 但 is_timestamp_pattern 为真 ⇒ 仍应是 Temporal
    let irregular = [
        1_700_000_007i64,
        1_600_000_000,
        1_750_000_123,
        1_450_000_456,
    ];
    let mut vals: Vec<mora::value::Value> = (0..12)
        .map(|k| i(irregular[k % 4] + (k as i64) * 7_919))
        .collect();
    vals.truncate(12);
    let r = role_of("ts", vals);
    assert_eq!(r, FieldRole::Temporal, "不连续的时间戳仍应判为 Temporal");
}

/// **13 位毫秒级时间戳落 `Id` 而非 `Temporal`** —— 记录**已知能力缺口**。
///
/// `is_timestamp_pattern`（`detect.rs:256-280`）的三个分支范围**不一致**：
///
/// | 分支 | 范围 |
/// |---|---|
/// | `String`（unix）| `len ∈ [10, 13]` —— **含 13 位毫秒** |
/// | `String`（iso）| `len >= 10` 且第 4/7 位是 `-` |
/// | `Float` | `1e9 < n < 1e10` —— 仅 10 位 |
/// | `Int`（D264 补）| `1e9 < n < 1e10` —— 仅 10 位 |
///
/// ⇒ `json.parse` 读入的**毫秒级整数时间戳**落 `Id`。
///
/// **判定为能力缺口，不修**：D264 的注释（`detect.rs:266-274`）明写
/// 「范围要窄：只补 `Int`」并记录了**试过放宽的后果** ——
/// 「连带把 `Float` 时间序列从 `TimeSeries` 打成 `Uniform`」。
/// 且 spec 对时间戳范围**零承诺**（grep `timestamp|TimeSeries|时间戳|Unix` 无命中）。
///
/// 本条把这个现状**钉住**：若将来有人扩了范围，本条会红 ——
/// 那是有意的行为变更，需同步更新本判据与 `detect.rs` 的注释。
#[test]
fn d368_millisecond_epoch_int_is_known_gap() {
    let ms = role_of("ts", (0..12).map(|k| i(1_700_000_000_000 + k)).collect());
    assert_eq!(
        ms,
        FieldRole::Id,
        "13 位毫秒级 Int 当前落 Id（`is_timestamp_pattern` 的 Int 分支只到 1e10）—— \
         这是 D264 刻意保持的窄范围；若将来放宽，本判据需同步更新"
    );

    // **反向对照**：13 位**字符串**形式是支持的（`len ∈ [10,13]`）
    let ms_str = role_of(
        "ts",
        (0..12)
            .map(|k| s(&format!("{:013}", 1_700_000_000_000u64 + k)))
            .collect(),
    );
    assert_eq!(
        ms_str,
        FieldRole::Temporal,
        "13 位毫秒级 **字符串** 是支持的（String 分支 len<=13）"
    );
}

/// **直通条件会让字段检测被跳过** —— 本轮踩了两次。
///
/// 本条把「两条直通条件」**显式钉住**：
/// - `items.len() <= 5` ⇒ 即使 target 很小也直通
/// - `items.len() <= target`（非 `lossless`）⇒ 直通
///
/// 直通时 `CrushResult.fields` 是**空 Vec**（不是「角色为 Generic」），
/// 这是**契约**（`json.rs:286-296` 显式构造 `fields: vec![]`）。
#[test]
fn d368_short_list_passthrough_yields_empty_fields() {
    let mk = |n: usize| {
        let items: Vec<mora::value::Value> = (0..n)
            .map(|k| {
                let mut m = HashMap::new();
                m.insert(
                    "t".to_string(),
                    s(&format!("2024-01-{:02}T00:00:00Z", k + 1)),
                );
                mora::value::Value::Dict(m)
            })
            .collect();
        items
    };
    // n=5 ⇒ `items.len() <= 5` 短路，即便 target 更小
    let r = crush_json(&mk(5), 1, &CompressOptions::default());
    assert_eq!(
        r.fields.len(),
        0,
        "n=5 时走「items.len() <= 5」短路，fields 应为空"
    );
    assert_eq!(r.strategy_used, "passthrough", "短路时策略是 passthrough");

    // n=12 但 target=12 ⇒ `items.len() <= target` 短路
    let r = crush_json(&mk(12), 12, &CompressOptions::default());
    assert_eq!(
        r.fields.len(),
        0,
        "target >= n 时走「items.len() <= target」短路，fields 应为空"
    );

    // **反向对照**：target < n ⇒ 字段检测真的发生
    let r = crush_json(&mk(12), 6, &CompressOptions::default());
    assert!(
        !r.fields.is_empty(),
        "target < n 时字段检测必须发生（否则本文件的其它判据全部恒绿）"
    );
}
