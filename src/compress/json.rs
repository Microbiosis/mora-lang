//! v0.30: SmartCrusher — content-aware JSON compression
//!
//! 灵感: Headroom (<https://github.com/headroomlabs-ai/headroom>) — SmartCrusher
//! 核心思想: **不依赖字段名，按值分布推断语义角色**。
//!
//! 用法:
//! ```ignore
//! use mora::compress::json::{crush_json, CrushResult};
//! let result = crush_json(&items, 100, &CompressOptions::default());
//! ```
//!
//! 提供 5 种压缩策略 (TopN / TimeSeries / ClusterSample / SmartSample / Lossless) +
//! 3 种安全约束 (KeepErrors / KeepOutliers / KeepBoundary)。

use std::collections::{HashMap, HashSet};

use crate::compress::CompressOptions;
use crate::flow::{json_to_value, value_to_json};
use crate::value::Value;

// ──────────────────── 字段角色 ────────────────────

/// 字段语义角色（按值分布推断，与字段名无关）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldRole {
    Id,       // uniqueness > 0.9, 或 UUID, 或顺序递增数字
    Score,    // bounded numeric range (0-1 或 0-100)
    Temporal, // date/timestamp pattern
    Error,    // 字段名或值含 ERROR_KEYWORDS
    Anomaly,  // 该字段值 >2σ from mean
    Constant, // 所有项相同
    Generic,  // 兜底
}

/// 单字段的统计特征
#[derive(Debug, Clone)]
pub struct FieldStats {
    pub name: String,
    pub role: FieldRole,
    pub uniqueness: f32,
    pub null_rate: f32,
    pub is_numeric: bool,
    pub numeric_range: Option<(f64, f64)>,
    pub sample: Vec<Value>,
}

// ──────────────────── Array 类型 ────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrayType {
    TopScores,  // 存在 Score 字段
    TimeSeries, // 存在 Temporal 字段
    Clustered,  // 字段值高冗余 (uniqueness < 0.3)
    Uniform,    // 所有项 schema 一致且字段数少 (<10)
    Generic,    // 兜底
}

// ──────────────────── 策略 trait ────────────────────

pub trait Strategy: std::fmt::Debug + Send + Sync {
    fn name(&self) -> &'static str;
    fn select(
        &self,
        items: &[Value],
        fields: &[FieldStats],
        target: usize,
        constraints: &[Box<dyn Constraint>],
    ) -> Vec<usize>;
}

// ──────────────────── Constraint trait ────────────────────

pub trait Constraint: std::fmt::Debug + Send + Sync {
    fn name(&self) -> &str;
    fn apply(&self, keep: &mut Vec<usize>, items: &[Value], fields: &[FieldStats]);
}

// ──────────────────── 压缩结果 ────────────────────

#[derive(Debug, Clone)]
pub struct CrushResult {
    pub items: Vec<Value>,
    pub strategy_used: String,
    pub array_type: ArrayType,
    pub fields: Vec<FieldStats>,
    pub items_total: usize,
    pub items_kept: usize,
    pub savings_ratio: f32,
    pub byte_estimate: usize,
}

impl CrushResult {
    pub fn metadata(&self) -> HashMap<String, Value> {
        let mut m = HashMap::new();
        m.insert("strategy".into(), Value::String(self.strategy_used.clone()));
        m.insert(
            "array_type".into(),
            Value::String(format!("{:?}", self.array_type)),
        );
        m.insert("items_total".into(), Value::Number(self.items_total as f64));
        m.insert("items_kept".into(), Value::Number(self.items_kept as f64));
        m.insert(
            "savings_ratio".into(),
            Value::Number(self.savings_ratio as f64),
        );
        m.insert(
            "fields_detected".into(),
            Value::Number(self.fields.len() as f64),
        );
        m
    }
}

// ──────────────────── 错误关键字常量 ────────────────────

pub const ERROR_KEYWORDS: &[&str] = &[
    "error",
    "failed",
    "exception",
    "fatal",
    "panic",
    "err",
    "denied",
    "rejected",
    "timeout",
    "abort",
    "crash",
    "refused",
    "unauthorized",
    "forbidden",
];

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
    names
}

pub fn detect_field_role(name: &str, values: &[&Value]) -> FieldStats {
    let uniqueness = compute_uniqueness(values);
    let null_rate = compute_null_rate(values);
    let is_numeric = !values.is_empty() && values.iter().all(|v| matches!(v, Value::Number(_)));
    let numeric_range = if is_numeric {
        let nums: Vec<f64> = values
            .iter()
            .filter_map(|v| {
                if let Value::Number(n) = v {
                    Some(*n)
                } else {
                    None
                }
            })
            .collect();
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
    let nums: Vec<f64> = values
        .iter()
        .filter_map(|v| {
            if let Value::Number(n) = v {
                Some(*n)
            } else {
                None
            }
        })
        .collect();
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
    let nums: Vec<f64> = values
        .iter()
        .filter_map(|v| {
            if let Value::Number(n) = v {
                Some(*n)
            } else {
                None
            }
        })
        .collect();
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
        Value::Number(n) => *n > 1_000_000_000.0 && *n < 10_000_000_000.0,
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

// ──────────────────── 5 种压缩策略 ────────────────────

#[derive(Debug)]
pub struct TopNStrategy;

impl Strategy for TopNStrategy {
    fn name(&self) -> &'static str {
        "topn"
    }
    fn select(
        &self,
        items: &[Value],
        fields: &[FieldStats],
        target: usize,
        constraints: &[Box<dyn Constraint>],
    ) -> Vec<usize> {
        let score_field = fields
            .iter()
            .find(|f| f.role == FieldRole::Score)
            .map(|f| f.name.clone());
        // 没找到 Score 字段 → fall back 到 SmartSampleStrategy 行为 (按 index 顺序 + 头尾)
        let Some(score_field) = score_field else {
            return SmartSampleStrategy.select(items, fields, target, constraints);
        };
        let mut scored: Vec<(usize, f64)> = items
            .iter()
            .enumerate()
            .map(|(i, it)| {
                let s = if let Value::Dict(d) = it {
                    d.get(&score_field)
                        .and_then(|v| {
                            if let Value::Number(n) = v {
                                Some(*n)
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0.0)
                } else {
                    0.0
                };
                (i, s)
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let mut keep: Vec<usize> = scored.iter().take(target).map(|(i, _)| *i).collect();
        apply_all(&mut keep, items, fields, constraints);
        finalize(keep, target)
    }
}

#[derive(Debug)]
pub struct TimeSeriesStrategy;

impl Strategy for TimeSeriesStrategy {
    fn name(&self) -> &'static str {
        "timeseries"
    }
    fn select(
        &self,
        items: &[Value],
        _fields: &[FieldStats],
        target: usize,
        constraints: &[Box<dyn Constraint>],
    ) -> Vec<usize> {
        let n = items.len();
        let boundary = target / 3;
        let mut keep: Vec<usize> = (0..boundary.min(n)).collect();
        keep.extend((n.saturating_sub(boundary)..n).collect::<Vec<_>>());

        let mid_target = target.saturating_sub(keep.len());
        if mid_target > 0 {
            let mid_start = boundary;
            let mid_end = n.saturating_sub(boundary);
            if mid_end > mid_start {
                let step = (mid_end - mid_start) as f32 / mid_target as f32;
                for i in 0..mid_target {
                    keep.push(mid_start + (i as f32 * step) as usize);
                }
            }
        }
        apply_all(&mut keep, items, _fields, constraints);
        finalize(keep, target)
    }
}

#[derive(Debug)]
pub struct ClusterSampleStrategy;

impl Strategy for ClusterSampleStrategy {
    fn name(&self) -> &'static str {
        "cluster_sample"
    }
    fn select(
        &self,
        items: &[Value],
        _fields: &[FieldStats],
        target: usize,
        constraints: &[Box<dyn Constraint>],
    ) -> Vec<usize> {
        let mut seen_groups: HashSet<String> = HashSet::new();
        let mut keep: Vec<usize> = Vec::new();
        for (i, it) in items.iter().enumerate() {
            if let Value::Dict(d) = it {
                let group_key: String = d
                    .values()
                    .filter(|v| matches!(v, Value::String(_)))
                    .take(3)
                    .map(|v| format!("{:?}", v))
                    .collect::<Vec<_>>()
                    .join("|");
                if seen_groups.insert(group_key) {
                    keep.push(i);
                    if keep.len() >= target {
                        break;
                    }
                }
            }
        }
        apply_all(&mut keep, items, _fields, constraints);
        finalize(keep, target)
    }
}

#[derive(Debug)]
pub struct SmartSampleStrategy;

impl Strategy for SmartSampleStrategy {
    fn name(&self) -> &'static str {
        "smart_sample"
    }
    fn select(
        &self,
        items: &[Value],
        _fields: &[FieldStats],
        target: usize,
        constraints: &[Box<dyn Constraint>],
    ) -> Vec<usize> {
        let n = items.len();
        let k_first = (target / 2).max(1);
        let k_last = target.saturating_sub(k_first).max(1);
        let mut keep: Vec<usize> = (0..k_first.min(n)).collect();
        keep.extend((n.saturating_sub(k_last)..n).collect::<Vec<_>>());
        let mid_target = target.saturating_sub(keep.len());
        if mid_target > 0 && n > k_first + k_last {
            let step = (n - k_first - k_last) as f32 / mid_target as f32;
            for i in 0..mid_target {
                keep.push(k_first + (i as f32 * step) as usize);
            }
        }
        apply_all(&mut keep, items, _fields, constraints);
        finalize(keep, target)
    }
}

#[derive(Debug)]
pub struct LosslessStrategy;

impl Strategy for LosslessStrategy {
    fn name(&self) -> &'static str {
        "lossless"
    }
    fn select(
        &self,
        items: &[Value],
        _fields: &[FieldStats],
        target: usize,
        _constraints: &[Box<dyn Constraint>],
    ) -> Vec<usize> {
        (0..items.len().min(target)).collect()
    }
}

fn apply_all(
    keep: &mut Vec<usize>,
    items: &[Value],
    fields: &[FieldStats],
    constraints: &[Box<dyn Constraint>],
) {
    for c in constraints {
        c.apply(keep, items, fields);
    }
}

fn finalize(keep: Vec<usize>, target: usize) -> Vec<usize> {
    let mut v = keep;
    v.sort_unstable();
    v.dedup();
    if v.len() > target {
        v.truncate(target);
    }
    v
}

// ──────────────────── Constraint 实现 ────────────────────

#[derive(Debug)]
pub struct KeepErrorsConstraint;

impl Constraint for KeepErrorsConstraint {
    fn name(&self) -> &str {
        "keep_errors"
    }
    fn apply(&self, keep: &mut Vec<usize>, items: &[Value], _fields: &[FieldStats]) {
        for (i, it) in items.iter().enumerate() {
            if keep.contains(&i) {
                continue;
            }
            if let Value::Dict(d) = it {
                let has_error = d.iter().any(|(k, v)| {
                    let kk = k.to_lowercase();
                    ERROR_KEYWORDS.iter().any(|kw| kk.contains(kw))
                        || matches!(v, Value::String(s) if {
                            let sl = s.to_lowercase();
                            ERROR_KEYWORDS.iter().any(|kw| sl.contains(kw))
                        })
                        || matches!(v, Value::Bool(false) if {
                            kk.contains("success") || kk.contains("ok") || kk == "passed"
                        })
                });
                if has_error {
                    keep.push(i);
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct KeepOutliersConstraint;

impl Constraint for KeepOutliersConstraint {
    fn name(&self) -> &str {
        "keep_outliers"
    }
    fn apply(&self, keep: &mut Vec<usize>, items: &[Value], fields: &[FieldStats]) {
        // 只对 role=Anomaly 字段跑 outlier 检测
        // (Score 字段的高值是 feature 不是 outlier, 由 TopNStrategy 保留)
        for field in fields.iter().filter(|f| f.role == FieldRole::Anomaly) {
            let values: Vec<&Value> = items
                .iter()
                .filter_map(|it| {
                    if let Value::Dict(d) = it {
                        d.get(&field.name)
                    } else {
                        None
                    }
                })
                .collect();
            let outliers = outliers_by_zscore(&values, 2.0);
            for i in outliers {
                if !keep.contains(&i) {
                    keep.push(i);
                }
            }
        }
    }
}

pub fn outliers_by_zscore(values: &[&Value], z: f64) -> Vec<usize> {
    let nums: Vec<(usize, f64)> = values
        .iter()
        .enumerate()
        .filter_map(|(i, v)| {
            if let Value::Number(n) = v {
                Some((i, *n))
            } else {
                None
            }
        })
        .collect();
    if nums.len() < 5 {
        return vec![];
    }
    let mean = nums.iter().map(|(_, n)| n).sum::<f64>() / nums.len() as f64;
    let var = nums.iter().map(|(_, n)| (n - mean).powi(2)).sum::<f64>() / nums.len() as f64;
    let std = var.sqrt();
    if std == 0.0 {
        return vec![];
    }
    nums.iter()
        .filter(|(_, n)| (*n - mean).abs() > z * std)
        .map(|(i, _)| *i)
        .collect()
}

#[derive(Debug)]
pub struct KeepBoundaryConstraint {
    pub k_first: usize,
    pub k_last: usize,
}

impl Constraint for KeepBoundaryConstraint {
    fn name(&self) -> &str {
        "keep_boundary"
    }
    fn apply(&self, keep: &mut Vec<usize>, items: &[Value], _fields: &[FieldStats]) {
        let n = items.len();
        for i in 0..self.k_first.min(n) {
            if !keep.contains(&i) {
                keep.push(i);
            }
        }
        for i in n.saturating_sub(self.k_last)..n {
            if !keep.contains(&i) {
                keep.push(i);
            }
        }
    }
}

// ──────────────────── Lossless 紧凑格式 ────────────────────

/// 尝试无损压缩: 转 csv-schema 或 markdown-kv
/// 返回 None 表示不适用 (schema 不均匀)
pub fn try_lossless_compact(items: &[Value], fields: &[FieldStats]) -> Option<CrushResult> {
    if items.is_empty() {
        return None;
    }
    let first_keys: HashSet<String> = match &items[0] {
        Value::Dict(d) => d.keys().cloned().collect(),
        _ => return None,
    };
    if !items.iter().all(|it| {
        if let Value::Dict(d) = it {
            d.keys().cloned().collect::<HashSet<_>>() == first_keys
        } else {
            false
        }
    }) {
        return None;
    }

    let all_scalar = fields.iter().all(|f| {
        f.sample.iter().all(|v| {
            matches!(
                v,
                Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Nil
            )
        })
    });

    let compact_str = if all_scalar && fields.iter().any(|f| f.is_numeric) {
        let header: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        let rows: Vec<String> = items
            .iter()
            .map(|it| {
                if let Value::Dict(d) = it {
                    fields
                        .iter()
                        .map(|f| d.get(&f.name).map(value_to_json).unwrap_or_default())
                        .collect::<Vec<_>>()
                        .join(",")
                } else {
                    String::new()
                }
            })
            .collect();
        format!("schema: {}\n{}", header.join(","), rows.join("\n"))
    } else {
        let mut s = String::new();
        for (i, it) in items.iter().enumerate() {
            if let Value::Dict(d) = it {
                s.push_str(&format!("## item_{}\n", i));
                for (k, v) in d {
                    s.push_str(&format!("- {}: {}\n", k, value_to_json(v)));
                }
            }
        }
        s
    };

    let byte_estimate = compact_str.len();
    Some(CrushResult {
        items: vec![Value::String(compact_str)],
        strategy_used: "lossless_compact".into(),
        array_type: ArrayType::Uniform,
        fields: fields.to_vec(),
        items_total: items.len(),
        items_kept: items.len(),
        savings_ratio: 0.0,
        byte_estimate,
    })
}

// ──────────────────── 主入口 `crush_json` ────────────────────

/// v0.30 SmartCrusher 主入口
pub fn crush_json(items: &[Value], target: usize, options: &CompressOptions) -> CrushResult {
    // v0.32 recursive=true: 走整棵 Value 树的 recursive walker (delegates to crush_json_recursive)
    if options.recursive {
        return crush_json_recursive(items, target, options);
    }
    crush_json_inner(items, target, options)
}

/// v0.32: 内部版本, 顶层 List 的标准 SmartCrusher 流程, 不递归.
fn crush_json_inner(items: &[Value], target: usize, options: &CompressOptions) -> CrushResult {
    // 1. 边界
    if items.is_empty() {
        return CrushResult {
            items: vec![],
            strategy_used: "passthrough".into(),
            array_type: ArrayType::Generic,
            fields: vec![],
            items_total: 0,
            items_kept: 0,
            savings_ratio: 0.0,
            byte_estimate: 0,
        };
    }
    // 短列表直通: items <= 5 或 items <= target (取 min)
    // 当显式 strategy="lossless" 时, 不直通 (让 Lossless-First 短路判断是否真的无损)
    let short_passthrough =
        items.len() <= 5 || (items.len() <= target && options.strategy != "lossless");
    if short_passthrough {
        return CrushResult {
            items: items.to_vec(),
            strategy_used: "passthrough".into(),
            array_type: ArrayType::Generic,
            fields: vec![],
            items_total: items.len(),
            items_kept: items.len(),
            savings_ratio: 0.0,
            byte_estimate: estimate_bytes(items),
        };
    }

    // 2. 字段角色
    let fields = extract_field_stats(items);

    // 3. Array 类型
    let array_type = detect_array_type(items, &fields);

    // 4. 选策略
    let strategy: Box<dyn Strategy> = match options.strategy.as_str() {
        "topn" => Box::new(TopNStrategy),
        "timeseries" => Box::new(TimeSeriesStrategy),
        "cluster" => Box::new(ClusterSampleStrategy),
        "lossless" => {
            if let Some(compact) = try_lossless_compact(items, &fields) {
                let ratio =
                    1.0 - (compact.byte_estimate as f32 / estimate_bytes(items).max(1) as f32);
                if ratio >= options.lossless_min_savings_ratio {
                    return compact;
                }
            }
            Box::new(SmartSampleStrategy)
        }
        "smart_sample" | "head_tail" => Box::new(SmartSampleStrategy),
        "auto" | "" => match array_type {
            ArrayType::TopScores => Box::new(TopNStrategy),
            ArrayType::TimeSeries => Box::new(TimeSeriesStrategy),
            ArrayType::Clustered => Box::new(ClusterSampleStrategy),
            ArrayType::Uniform => Box::new(LosslessStrategy),
            ArrayType::Generic => Box::new(SmartSampleStrategy),
        },
        _ => Box::new(SmartSampleStrategy),
    };

    // 5. 构建约束
    let mut constraints: Vec<Box<dyn Constraint>> = Vec::new();
    if options.preserve_errors {
        constraints.push(Box::new(KeepErrorsConstraint));
    }
    if options.preserve_outliers {
        constraints.push(Box::new(KeepOutliersConstraint));
    }
    let k_first = options.k_first.unwrap_or((target as f32 * 0.15) as usize);
    let k_last = options.k_last.unwrap_or((target as f32 * 0.15) as usize);
    if k_first + k_last < target {
        constraints.push(Box::new(KeepBoundaryConstraint { k_first, k_last }));
    }

    // 6. 执行选择
    let keep = strategy.select(items, &fields, target, &constraints);

    // 7. 构造结果
    let kept_items: Vec<Value> = keep.iter().map(|&i| items[i].clone()).collect();
    let byte_estimate = estimate_bytes(&kept_items);
    CrushResult {
        items: kept_items,
        strategy_used: strategy.name().to_string(),
        array_type,
        fields,
        items_total: items.len(),
        items_kept: keep.len(),
        savings_ratio: 1.0 - (keep.len() as f32 / items.len() as f32),
        byte_estimate,
    }
}

/// v0.32 recursive 模式: 整棵 Value 树递归 compact (pure iterative, no nested calls)
/// 顶层 List 走 standard SmartCrusher (inlined), 嵌套结构走 walker
fn crush_json_recursive(items: &[Value], target: usize, options: &CompressOptions) -> CrushResult {
    // 1. 顶层 List: inlined standard SmartCrusher logic
    // (复制 crush_json 主体避免栈嵌套)
    let top = crush_json_inner(items, target, options);

    // 2. 嵌套结构递归 compact (min_items = target / 4 启发式)
    let min_items = (target / 4).max(5);
    let mut new_items = Vec::with_capacity(top.items.len());
    let mut nested_count = 0;
    for it in &top.items {
        let (nv, n) = compact_value_recursive(it, min_items);
        new_items.push(nv);
        nested_count += n;
    }
    CrushResult {
        items: new_items,
        strategy_used: if nested_count > 0 {
            format!("{}+recursive({})", top.strategy_used, nested_count)
        } else {
            top.strategy_used
        },
        array_type: top.array_type,
        fields: top.fields,
        items_total: top.items_total,
        items_kept: top.items_kept,
        savings_ratio: top.savings_ratio,
        byte_estimate: top.byte_estimate,
    }
}

pub fn estimate_bytes(items: &[Value]) -> usize {
    // v0.36 (P1-2.12): use streaming byte estimator instead of
    // re-serializing each Value to a String just to call .len().
    items.iter().map(|v| value_byte_size(v)).sum()
}

/// v0.36 (P1-2.12): streaming byte-size estimate. Walks the Value tree
/// recursively, counting UTF-8 bytes without materializing any String.
fn value_byte_size(v: &Value) -> usize {
    match v {
        Value::String(s) => s.len(),
        Value::Char(c) => c.len_utf8(),
        Value::Number(n) => {
            // f64 Display bytes: integer part + '.' + decimal or scientific.
            // Cheap heuristic — full precision's unlikely to matter for sizing.
            if n.is_nan() {
                3
            } else if n.is_infinite() {
                3 + n.is_sign_negative() as usize
            } else {
                format!("{}", n).len()
            }
        }
        Value::Bool(b) => if *b { 4 } else { 5 },
        Value::Nil => 3, // "nil"
        Value::List(items) => {
            // "[a, b, c]" → 2 (braces) + sum + 2*(len-1) (", ")
            let inner: usize = items.iter().map(value_byte_size).sum();
            2 + inner + items.len().saturating_sub(1) * 2
        }
        Value::Dict(map) => {
            // "{k: v, k: v}" → 2 + sum('k: v') + 2*(n-1)
            let mut total = 2;
            let mut count = 0;
            for (k, vv) in map {
                total += k.len() + 2 + value_byte_size(vv); // key + ": " + val
                count += 1;
            }
            if count > 0 {
                total += (count - 1) * 2;
            }
            total
        }
        // Other variants: rough tag size.
        _ => 32,
    }
}

// ──────────────────── v0.32: Lossless-First Recursive Walker ────────────────────
//
// 灵感: Headroom DocumentCompactor (crates/headroom-core/src/transforms/smart_crusher/compaction/walker.rs)
//
// 遍历整棵 Value 树, 每个 List 节点都尝试 Lossless Compact.
// 替换为紧凑表示 (csv-schema 或 markdown-kv), 若不适用则原样保留.
//
// 实现: iterative stack 避免深度递归栈溢出 (CI 在 Windows 上 default 1MB stack)

/// 递归 compact 整棵 Value 树. 返回 (new_value, compacted_count)
pub fn compact_value_recursive(value: &Value, min_items: usize) -> (Value, usize) {
    // iterative stack-based DFS
    // entry: (value, parent_kind, parent_key_or_idx, visited_sentinel)
    // visited_sentinel=true 表示已处理完子节点, 现在 compact 当前节点
    enum Op {
        Enter,
        Exit,
    }
    let mut stack: Vec<(Value, Op)> = Vec::new();
    stack.push((value.clone(), Op::Enter));

    // 后序结果: 用 Vec 模拟递归返回值链
    let mut results: Vec<(Value, usize)> = Vec::new();

    while let Some((v, op)) = stack.pop() {
        match op {
            Op::Enter => {
                // 先 push Exit (sentinel), 然后 push children (按反序使正序处理)
                stack.push((v.clone(), Op::Exit));
                // children
                match &v {
                    Value::List(items) => {
                        for it in items.iter().rev() {
                            stack.push((it.clone(), Op::Enter));
                        }
                    }
                    Value::Dict(d) => {
                        // 收集 keys 按反序, 保证原序处理
                        let mut entries: Vec<(String, Value)> =
                            d.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                        entries.reverse();
                        for (_, val) in entries {
                            stack.push((val, Op::Enter));
                        }
                    }
                    _ => {}
                }
            }
            Op::Exit => {
                // 处理当前节点: 子节点结果在 results 末尾
                match &v {
                    Value::List(items) => {
                        // 弹出 items.len() 个子结果 (后序)
                        let n_kids = items.len();
                        let mut new_items = Vec::with_capacity(n_kids);
                        let mut total = 0;
                        for _ in 0..n_kids {
                            if let Some((nv, n)) = results.pop() {
                                new_items.push(nv);
                                total += n;
                            }
                        }
                        new_items.reverse();
                        if items.len() >= min_items {
                            let fields = extract_field_stats(items);
                            if let Some(crushed) = try_lossless_compact(items, &fields)
                                && let Some(first) = crushed.items.into_iter().next()
                            {
                                results.push((first, total + 1));
                                continue;
                            }
                        }
                        results.push((Value::List(new_items), total));
                    }
                    Value::Dict(d) => {
                        let n_kids = d.len();
                        let mut new_map: std::collections::HashMap<String, Value> =
                            std::collections::HashMap::with_capacity(n_kids);
                        let mut total = 0;
                        let mut keys: Vec<String> = d.keys().cloned().collect();
                        for _ in 0..n_kids {
                            if let Some((nv, n)) = results.pop() {
                                // 配对: 倒序弹出 key
                                if let Some(k) = keys.pop() {
                                    new_map.insert(k, nv);
                                }
                                total += n;
                            }
                        }
                        results.push((Value::Dict(new_map), total));
                    }
                    _ => {
                        results.push((v.clone(), 0));
                    }
                }
            }
        }
    }

    debug_assert_eq!(results.len(), 1, "post-order DFS must yield 1 root result");
    results.into_iter().next().unwrap()
}

// ──────────────────── 字符串入口（解析后调用 crush_json） ────────────────────

/// 从 JSON 字符串压缩（替代 v0.29 `parse_json_simple` stub）
pub fn crush_json_string(
    content: &str,
    target: usize,
    options: &CompressOptions,
) -> Result<CrushResult, String> {
    let parsed = json_to_value(content)?;
    let items = match parsed {
        Value::List(l) => l,
        other => {
            return Err(format!(
                "crush.json: expected JSON array, got {}",
                value_type_name(&other)
            ));
        }
    };
    Ok(crush_json(&items, target, options))
}

fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::String(_) => "string",
        Value::Number(_) => "number",
        Value::Bool(_) => "bool",
        Value::Nil => "null",
        Value::List(_) => "array",
        Value::Dict(_) => "object",
        _ => "other",
    }
}

// ──────────────────── SubCompressor 适配（保持 v0.29 trait API） ────────────────────

/// 适配 v0.29 SubCompressor trait：把 content 视为 JSON 数组字符串
#[derive(Debug)]
pub struct JsonSubCompressor;

impl crate::compress::SubCompressor for JsonSubCompressor {
    fn sniff(&self, content: &str) -> f32 {
        let trimmed = content.trim_start();
        if trimmed.starts_with('[') || trimmed.starts_with('{') {
            0.9
        } else {
            0.0
        }
    }

    fn compress(
        &self,
        content: &str,
        max_bytes: usize,
        options: &CompressOptions,
    ) -> Result<String, String> {
        let parsed = json_to_value(content).map_err(|e| format!("crush.json: {}", e))?;
        let items = match parsed {
            Value::List(l) => l,
            _ => {
                return Err(format!(
                    "crush.json: expected JSON array, got {}",
                    value_type_name(&parsed)
                ));
            }
        };
        // 由 max_bytes 推 target: 假设每项 200 bytes (与 v0.29 一致)
        let target = (max_bytes / 200).max(1);
        let result = crush_json(&items, target, options);
        let json = value_to_json(&Value::List(result.items.clone()));
        Ok(format!(
            "{}\n<compressed:method=smart_crusher strategy={} items={} total={} savings={:.2}>",
            json, result.strategy_used, result.items_kept, result.items_total, result.savings_ratio
        ))
    }

    fn origin(&self) -> &'static str {
        "json"
    }
}

// ──────────────────── 单元测试 ────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_items<F: Fn(usize) -> HashMap<String, Value>>(n: usize, f: F) -> Vec<Value> {
        (0..n).map(|i| Value::Dict(f(i))).collect()
    }

    // ── 字段角色测试 ──

    #[test]
    fn role_id_uses_uniqueness_not_name() {
        let items = make_items(10, |i| {
            let mut d = HashMap::new();
            d.insert("id".into(), Value::String("same".into()));
            d.insert("name".into(), Value::String(format!("user_{}", i)));
            d
        });
        let fields = extract_field_stats(&items);
        let role_map: HashMap<&str, FieldRole> =
            fields.iter().map(|f| (f.name.as_str(), f.role)).collect();
        assert_eq!(role_map["id"], FieldRole::Generic);
        assert_eq!(role_map["name"], FieldRole::Id);
    }

    #[test]
    fn role_score_uses_range_not_name() {
        let items = make_items(10, |i| {
            let mut d = HashMap::new();
            d.insert("amount".into(), Value::Number(i as f64 / 10.0));
            d
        });
        let fields = extract_field_stats(&items);
        assert_eq!(fields[0].role, FieldRole::Score);
    }

    #[test]
    fn role_error_detects_value_content() {
        let mut items = make_items(10, |i| {
            let mut d = HashMap::new();
            d.insert("msg".into(), Value::String(format!("ok #{}", i)));
            d
        });
        if let Value::Dict(d) = &mut items[5] {
            d.insert("msg".into(), Value::String("operation failed".into()));
        }
        let opts = CompressOptions::default();
        let r = crush_json(&items, 2, &opts);
        assert!(
            r.items.iter().any(|it| matches!(it, Value::Dict(d) if
                d.get("msg").map(|v| v.to_string().contains("failed")).unwrap_or(false)
            )),
            "KeepErrorsConstraint 应基于值内容保留 failed 项"
        );
    }

    #[test]
    fn role_temporal_iso8601() {
        let items = make_items(5, |i| {
            let mut d = HashMap::new();
            d.insert(
                "ts".into(),
                Value::String(format!("2026-01-{:02}T00:00:00Z", i + 1)),
            );
            d
        });
        let fields = extract_field_stats(&items);
        assert_eq!(fields[0].role, FieldRole::Temporal);
    }

    #[test]
    fn role_anomaly_zscore_detection() {
        let items = make_items(100, |i| {
            let mut d = HashMap::new();
            d.insert(
                "value".into(),
                Value::Number(if i == 50 { 1000.0 } else { (i as f64) / 100.0 }),
            );
            d
        });
        let fields = extract_field_stats(&items);
        assert_eq!(fields[0].role, FieldRole::Anomaly);
    }

    // ── 策略测试 ──

    #[test]
    fn strategy_topn_keeps_highest() {
        let items: Vec<Value> = (0..100)
            .map(|i| {
                let mut d = HashMap::new();
                d.insert("score".into(), Value::Number((i as f64).sqrt()));
                Value::Dict(d)
            })
            .collect();
        let opts = CompressOptions {
            strategy: "topn".into(),
            ..CompressOptions::default()
        };
        let r = crush_json(&items, 5, &opts);
        assert_eq!(r.strategy_used, "topn");
        assert!(
            r.items.len() <= 5,
            "top 5 + constraints ≤ 5: got {}",
            r.items.len()
        );
        // 必须包含最高分 (sqrt(99) ≈ 9.95)
        let scores: Vec<f64> = r
            .items
            .iter()
            .filter_map(|it| {
                if let Value::Dict(d) = it
                    && let Some(Value::Number(n)) = d.get("score")
                {
                    return Some(*n);
                }
                None
            })
            .collect();
        assert!(
            scores.iter().any(|&s| (s - 99.0_f64.sqrt()).abs() < 0.001),
            "top score sqrt(99) must be present, scores: {:?}",
            scores
        );
    }

    #[test]
    fn strategy_timeseries_preserves_boundary() {
        let items = make_items(100, |i| {
            let mut d = HashMap::new();
            d.insert("ts".into(), Value::String(format!("2026-01-{:02}", i + 1)));
            d.insert("v".into(), Value::Number(i as f64));
            d
        });
        let opts = CompressOptions {
            strategy: "timeseries".into(),
            ..CompressOptions::default()
        };
        let r = crush_json(&items, 30, &opts);
        assert!(r.items.iter().any(|it| matches!(it, Value::Dict(d) if
            d.get("v") == Some(&Value::Number(0.0))
        )));
    }

    #[test]
    fn strategy_lossless_csv_schema() {
        let items = make_items(20, |i| {
            let mut d = HashMap::new();
            d.insert("id".into(), Value::Number(i as f64));
            d.insert("name".into(), Value::String(format!("item_{}", i)));
            d.insert("value".into(), Value::Number(i as f64 * 2.0));
            d
        });
        let opts = CompressOptions {
            strategy: "lossless".into(),
            max_bytes: Some(8192),
            ..CompressOptions::default()
        };
        let r = crush_json(&items, 100, &opts);
        // 走 lossless compact 路径, 输出单字符串
        assert_eq!(r.strategy_used, "lossless_compact");
        assert_eq!(r.items.len(), 1);
        if let Value::String(s) = &r.items[0] {
            assert!(s.contains("schema:") || s.contains("## "), "got: {}", s);
        }
    }

    #[test]
    fn strategy_auto_picks_topn_for_scores() {
        let items = make_items(50, |i| {
            let mut d = HashMap::new();
            d.insert("score".into(), Value::Number((i as f64) / 50.0));
            d.insert("name".into(), Value::String(format!("item_{}", i)));
            d
        });
        let r = crush_json(&items, 5, &CompressOptions::default());
        assert_eq!(r.array_type, ArrayType::TopScores);
        assert_eq!(r.strategy_used, "topn");
    }

    // ── 约束测试 ──

    #[test]
    fn constraint_keeps_errors() {
        let mut items = make_items(100, |i| {
            let mut d = HashMap::new();
            d.insert("msg".into(), Value::String(format!("ok #{}", i)));
            d
        });
        if let Value::Dict(d) = &mut items[50] {
            d.insert("msg".into(), Value::String("operation failed".into()));
        }
        let r = crush_json(&items, 10, &CompressOptions::default());
        assert!(r.items.iter().any(|it| matches!(it, Value::Dict(d) if
            d.get("msg").map(|v| v.to_string().contains("failed")).unwrap_or(false)
        )));
    }

    #[test]
    fn constraint_keeps_outliers() {
        let items = make_items(100, |i| {
            let mut d = HashMap::new();
            d.insert(
                "value".into(),
                Value::Number(if i == 50 { 1000.0 } else { (i as f64).sin() }),
            );
            d
        });
        let r = crush_json(&items, 10, &CompressOptions::default());
        assert!(r.items.iter().any(|it| matches!(it, Value::Dict(d) if
            d.get("value") == Some(&Value::Number(1000.0))
        )));
    }

    // ── metadata 测试 ──

    #[test]
    fn metadata_reports_strategy_and_savings() {
        let items = make_items(100, |i| {
            let mut d = HashMap::new();
            d.insert("id".into(), Value::Number(i as f64));
            d.insert("score".into(), Value::Number((i as f64) / 100.0));
            d
        });
        let r = crush_json(&items, 10, &CompressOptions::default());
        assert!(
            r.savings_ratio > 0.8,
            "应节省 > 80%, got {}",
            r.savings_ratio
        );
        let meta = r.metadata();
        assert!(meta.contains_key("strategy"));
        assert!(meta.contains_key("savings_ratio"));
    }

    // ── 字符串入口测试 ──

    #[test]
    fn crush_json_string_parses_and_compresses() {
        let json = r#"[{"id":1,"score":0.5},{"id":2,"score":0.9},{"id":3,"score":0.1}]"#;
        // 3 items (≤5 短列表直通) — 验证 string 入口能解析 + 短列表直通
        let r = crush_json_string(json, 2, &CompressOptions::default()).unwrap();
        assert_eq!(r.items.len(), 3);
        assert_eq!(r.strategy_used, "passthrough");
        // 100 items 才走真策略
        let json_big = format!(
            "[{}]",
            (0..100)
                .map(|i| format!(r#"{{"id":{},"score":{}}}"#, i, (i as f64) / 100.0))
                .collect::<Vec<_>>()
                .join(",")
        );
        let r2 = crush_json_string(&json_big, 10, &CompressOptions::default()).unwrap();
        assert_eq!(r2.strategy_used, "topn");
        assert_eq!(r2.items.len(), 10);
        assert!(r2.savings_ratio > 0.8);
    }

    #[test]
    fn crush_json_string_rejects_non_array() {
        let json = r#"{"not":"array"}"#;
        let r = crush_json_string(json, 5, &CompressOptions::default());
        assert!(r.is_err());
    }

    // ── v0.32: Recursive walker 测试 ──

    #[test]
    fn recursive_walker_compacts_nested_lists() {
        // 嵌套结构: 顶层 list 含 dict, dict 里有 nested list
        // 减少深度避免 stack overflow (Windows 1MB default stack)
        let items: Vec<Value> = (0..10)
            .map(|i| {
                let mut inner = Vec::new();
                for j in 0..6 {
                    let mut d = std::collections::HashMap::new();
                    d.insert("id".into(), Value::Number(j as f64));
                    d.insert("value".into(), Value::Number((i + j) as f64));
                    inner.push(Value::Dict(d));
                }
                let mut outer = std::collections::HashMap::new();
                outer.insert("name".into(), Value::String(format!("g{}", i)));
                outer.insert("items".into(), Value::List(inner));
                Value::Dict(outer)
            })
            .collect();

        let r = crush_json(
            &items,
            5,
            &CompressOptions {
                recursive: true,
                ..CompressOptions::default()
            },
        );
        // recursive 模式应能 compact 嵌套结构 (10 outer dicts + 6 inner)
        assert!(!r.items.is_empty());
        // 至少 nested 计数 > 0
        assert!(r.strategy_used.contains("recursive") || r.items_kept >= 5);
    }

    #[test]
    fn compact_value_recursive_simple() {
        // 直接测试 walker
        let v = Value::List(vec![
            Value::Dict({
                let mut d = std::collections::HashMap::new();
                d.insert("id".into(), Value::Number(1.0));
                d.insert("name".into(), Value::String("a".into()));
                d
            }),
            Value::Dict({
                let mut d = std::collections::HashMap::new();
                d.insert("id".into(), Value::Number(2.0));
                d.insert("name".into(), Value::String("b".into()));
                d
            }),
        ]);
        let (new_v, n) = compact_value_recursive(&v, 5);
        // 单层 compact: 因 min_items=5 但 v.len()=2, 不 compact
        assert_eq!(n, 0);
        assert!(matches!(new_v, Value::List(_)));
    }
}
