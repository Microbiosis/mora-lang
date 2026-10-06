//! v0.75.55: SmartCrusher 字段角色检测器（从 compress/json.rs 拆出）。
//! 语义角色推断（extract_field_stats / detect_field_role / detect_*）+ ArrayType
//! 判定。共享类型与 trait 定义在 super::json。

use std::collections::{HashMap, HashSet};

use super::json::{ArrayType, ERROR_KEYWORDS, FieldRole, FieldStats};
use crate::value::Value;

// ──────────────────── 字段角色检测器 ────────────────────

/// 主入口：对 items 所有字段跑检测
pub fn extract_field_stats(items: &[Value]) -> Vec<FieldStats> {
    let field_names = collect_field_names(items);
    field_names
        .into_iter()
        .map(|name| {
            let values: Vec<&Value> = items
                .iter()
                .filter_map(|it| {
                    if let Value::Dict(d) = it {
                        d.get(&name)
                    } else {
                        None
                    }
                })
                .collect();
            detect_field_role(&name, &values)
        })
        .collect()
}

fn collect_field_names(items: &[Value]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for it in items {
        if let Value::Dict(d) = it {
            for k in d.keys() {
                if seen.insert(k.clone()) {
                    names.push(k.clone());
                }
            }
        }
    }
    // v0.104.6 可复现性修复：按 key 排序。
    //
    // `Value::Dict` 是 `HashMap`，`d.keys()` 的序**每进程随机**；而本函数
    // 保留「首次出现」顺序，**把这个随机序原样传给下游** ——
    // `extract_field_stats` 返回的 `Vec<FieldStats>` 顺序随机，进而影响
    // `crush_json` 的策略选择（`detect_array_type` / `try_lossless_compact` /
    // 策略分派都吃这个列表）。
    //
    // 字段名来自 JSON 对象，按 RFC 8259 本就无序，故排序不损失任何信息。
    // 与 `flow::json::value_to_json` / `Value` 的 Display / `keys()`、
    // `values()` 统一为「按 key 排序」。
    names.sort();
    names
}

pub fn detect_field_role(name: &str, values: &[&Value]) -> FieldStats {
    let uniqueness = compute_uniqueness(values);
    let null_rate = compute_null_rate(values);
    // v0.104.6 D231：`Int` 与 `Float` 都算数值。
    // 修前 `matches!(v, Value::Float(_))` ⇒ `json.parse` 读入的整数列
    // 被判为**非数值**，角色推断 / range / outlier / 策略选择全部失准。
    let is_numeric = !values.is_empty()
        && values
            .iter()
            .all(|v| super::json::value_as_f64(v).is_some());
    let numeric_range = if is_numeric {
        let nums: Vec<f64> = super::json::values_as_f64(values.iter().copied());
        if nums.is_empty() {
            None
        } else {
            let lo = nums.iter().cloned().fold(f64::INFINITY, f64::min);
            let hi = nums.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            Some((lo, hi))
        }
    } else {
        None
    };

    // 检测顺序: Temporal → Error → Anomaly → Score → Id → Generic
    // (Temporal/Error/Anomaly 必须先于 Id, 否则高唯一性数值/字符串都被误判为 Id)
    let role = detect_temporal(values)
        .or_else(|| detect_error(name, values))
        .or_else(|| detect_anomaly(values))
        .or_else(|| detect_score(numeric_range, is_numeric))
        .or_else(|| detect_id(uniqueness, values))
        .unwrap_or(FieldRole::Generic);

    FieldStats {
        name: name.to_string(),
        role,
        uniqueness,
        null_rate,
        is_numeric,
        numeric_range,
        sample: values.iter().take(5).map(|v| (*v).clone()).collect(),
    }
}

fn detect_id(uniqueness: f32, values: &[&Value]) -> Option<FieldRole> {
    // UUID 模式或顺序递增数字 → Id
    if !values.is_empty() && values.iter().take(10).all(|v| is_uuid_pattern(v)) {
        return Some(FieldRole::Id);
    }
    if is_sequential_numeric(values) {
        return Some(FieldRole::Id);
    }
    // 字符串字段: 高 uniqueness 也算 Id (e.g. user_0, user_1, ...)
    if uniqueness > 0.9
        && !values.is_empty()
        && values.iter().all(|v| matches!(v, Value::String(_)))
    {
        return Some(FieldRole::Id);
    }
    None
}

fn detect_score(range: Option<(f64, f64)>, is_numeric: bool) -> Option<FieldRole> {
    if !is_numeric {
        return None;
    }
    let (lo, hi) = range?;
    let span = hi - lo;
    if (lo >= 0.0 && hi <= 1.0 && span > 0.01) || (lo >= 0.0 && hi <= 100.0 && span > 1.0) {
        Some(FieldRole::Score)
    } else {
        None
    }
}

fn detect_temporal(values: &[&Value]) -> Option<FieldRole> {
    if !values.is_empty() && values.iter().take(10).all(|v| is_timestamp_pattern(v)) {
        Some(FieldRole::Temporal)
    } else {
        None
    }
}

fn detect_error(name: &str, values: &[&Value]) -> Option<FieldRole> {
    let name_match = ERROR_KEYWORDS
        .iter()
        .any(|k| name.to_lowercase().contains(k));
    let value_match = values.iter().any(|v| match v {
        Value::String(s) => {
            let sl = s.to_lowercase();
            ERROR_KEYWORDS.iter().any(|k| sl.contains(k))
        }
        Value::Bool(false)
            if {
                let n = name.to_lowercase();
                n.contains("success") || n.contains("ok") || n == "passed"
            } =>
        {
            true
        }
        _ => false,
    });
    if name_match || value_match {
        Some(FieldRole::Error)
    } else {
        None
    }
}

fn detect_anomaly(values: &[&Value]) -> Option<FieldRole> {
    // 数值字段: 远离 mean > 3σ (更严格, 避免均匀分布的尾部被误判)
    // 且 outlier 数量少 (1-5% 范围, 不能 0 也不能太多)
    // v0.104.6 D231：数值提取统一走 `super::json::value_as_f64`（Int 侧也接受）
    let nums: Vec<f64> = super::json::values_as_f64(values.iter().copied());
    if nums.len() >= 5 {
        let mean = nums.iter().sum::<f64>() / nums.len() as f64;
        let var = nums.iter().map(|n| (n - mean).powi(2)).sum::<f64>() / nums.len() as f64;
        let std = var.sqrt();
        if std > 0.0 {
            let outlier_count = nums
                .iter()
                .filter(|n| (**n - mean).abs() > 3.0 * std)
                .count();
            // 至少 1 个 outlier, 且不超过 5% 项
            if outlier_count >= 1 && outlier_count * 20 <= nums.len() {
                return Some(FieldRole::Anomaly);
            }
        }
    }
    // 字符串字段: 低频 categorical (< 5%)
    let strs: Vec<&str> = values
        .iter()
        .filter_map(|v| {
            if let Value::String(s) = v {
                Some(s.as_str())
            } else {
                None
            }
        })
        .collect();
    if strs.len() >= 10 {
        let mut freq: HashMap<&str, usize> = HashMap::new();
        for s in &strs {
            *freq.entry(s).or_insert(0) += 1;
        }
        let rare = freq.values().filter(|&&c| c * 20 < strs.len()).count();
        if rare > 0 && rare * 5 < freq.len() {
            return Some(FieldRole::Anomaly);
        }
    }
    None
}

fn compute_uniqueness(values: &[&Value]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut seen: HashSet<String> = HashSet::new();
    for v in values {
        seen.insert(format!("{:?}", v));
    }
    seen.len() as f32 / values.len() as f32
}

fn compute_null_rate(values: &[&Value]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let nulls = values.iter().filter(|v| matches!(v, Value::Nil)).count();
    nulls as f32 / values.len() as f32
}

fn is_uuid_pattern(v: &Value) -> bool {
    if let Value::String(s) = v {
        let parts: Vec<&str> = s.split('-').collect();
        parts.len() == 5
            && parts[0].len() == 8
            && parts[1].len() == 4
            && parts[2].len() == 4
            && parts[3].len() == 4
            && parts[4].len() == 12
            && s.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
    } else {
        false
    }
}

fn is_sequential_numeric(values: &[&Value]) -> bool {
    // v0.104.6 D231：Int 序列 1,2,3,… 本该被识别为**顺序 Id**。
    // 修前只认 `Float`，`json.parse` 读入的整数列永远进不了 Id 角色。
    let nums: Vec<f64> = super::json::values_as_f64(values.iter().copied());
    if nums.len() < 3 {
        return false;
    }
    nums.windows(2).all(|w| (w[1] - w[0] - 1.0).abs() < 0.001)
}

fn is_timestamp_pattern(v: &Value) -> bool {
    match v {
        Value::String(s) => {
            let iso = s.len() >= 10
                && s.as_bytes().get(4) == Some(&b'-')
                && s.as_bytes().get(7) == Some(&b'-');
            let unix = s.len() >= 10 && s.len() <= 13 && s.chars().all(|c| c.is_ascii_digit());
            iso || unix
        }
        Value::Float(n) => *n > 1_000_000_000.0 && *n < 10_000_000_000.0,
        // v0.104.6 D264：Unix 秒级时间戳若来自 `json.parse` 就是 `Int`（D129），
        // 落 `_ => false` ⇒ 字段不被判为 Temporal ⇒ `ArrayType::TimeSeries`
        // 推断失败（实测落到 `Uniform`，压缩器完全不做针对性处理）。
        // 与 D231 修的 `is_numeric` 同族。
        //
        // ⚠ 范围要窄：**只补 `Int`**，不顺手改 `value_byte_size` 的保守估计
        // （那是 D227 硬上限的**安全方向**，改了会连带改变压缩行为 —— 本轮
        // 试过，连带把 `Float` 时间序列从 `TimeSeries` 打成 `Uniform`）。
        // 也不加 `BigInt`：时间戳不会是 BigInt，且 `to_f64` 需额外 import。
        Value::Int(n) => {
            let f = *n as f64;
            f > 1_000_000_000.0 && f < 10_000_000_000.0
        }
        _ => false,
    }
}

// ──────────────────── ArrayType 推断 ────────────────────

pub fn detect_array_type(_items: &[Value], fields: &[FieldStats]) -> ArrayType {
    if fields.iter().any(|f| f.role == FieldRole::Score) {
        return ArrayType::TopScores;
    }
    if fields.iter().any(|f| f.role == FieldRole::Temporal) {
        return ArrayType::TimeSeries;
    }
    if fields
        .iter()
        .any(|f| f.uniqueness < 0.3 && f.role != FieldRole::Constant)
    {
        return ArrayType::Clustered;
    }
    // Uniform: 字段少 (<10) 且全部是 Constant 或 Generic (即没有语义角色的纯数据)
    // 之前误把 is_numeric 全 true 的 Id 字段也算 Uniform, 改为排除 Id
    if fields.len() < 10
        && fields
            .iter()
            .all(|f| f.role == FieldRole::Constant || f.role == FieldRole::Generic)
    {
        return ArrayType::Uniform;
    }
    ArrayType::Generic
}
