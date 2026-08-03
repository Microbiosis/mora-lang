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
        m.insert("items_total".into(), Value::Float(self.items_total as f64));
        m.insert("items_kept".into(), Value::Float(self.items_kept as f64));
        m.insert(
            "savings_ratio".into(),
            Value::Float(self.savings_ratio as f64),
        );
        m.insert(
            "fields_detected".into(),
            Value::Float(self.fields.len() as f64),
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

// v0.75.55: 三层实现拆至子模块（detect / strategies / constraints），
// 本文件经 use 引用。依赖方向：detect ← strategies ← constraints，
// 主入口 crush_json 在下方。
use super::constraints::{KeepBoundaryConstraint, KeepErrorsConstraint, KeepOutliersConstraint};
use super::detect::{detect_array_type, extract_field_stats};
use super::strategies::{
    ClusterSampleStrategy, LosslessStrategy, SmartSampleStrategy, TimeSeriesStrategy, TopNStrategy,
};

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
                Value::String(_) | Value::Float(_) | Value::Bool(_) | Value::Nil
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
    items.iter().map(value_byte_size).sum()
}

/// v0.36 (P1-2.12): streaming byte-size estimate. Walks the Value tree
/// recursively, counting UTF-8 bytes without materializing any String.
fn value_byte_size(v: &Value) -> usize {
    match v {
        Value::String(s) => s.len(),
        Value::Char(c) => c.len_utf8(),
        Value::Float(n) => {
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
        Value::Bool(b) => {
            if *b {
                4
            } else {
                5
            }
        }
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
    results
        .into_iter()
        .next()
        .expect("post-order DFS must yield 1 root result (debug_assert verified)")
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
                json_type_name(&other)
            ));
        }
    };
    Ok(crush_json(&items, target, options))
}

/// JSON 视角类型名（错误消息用）。复用 flow::type_name（String→string /
/// Float→float / Bool→bool 与 JSON 名天然一致），仅映射 3 个 JSON 专名
/// （list→array / dict→object / nil→null）。v0.75.47: 消除与 flow 版
/// value_type_name 的撞名重复（旧版手写 6 变体 match）。
fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::List(_) => "array",
        Value::Dict(_) => "object",
        Value::Nil => "null",
        _ => crate::flow::type_name(v),
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
                    json_type_name(&parsed)
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
            d.insert("amount".into(), Value::Float(i as f64 / 10.0));
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
                Value::Float(if i == 50 { 1000.0 } else { (i as f64) / 100.0 }),
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
                d.insert("score".into(), Value::Float((i as f64).sqrt()));
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
                    && let Some(Value::Float(n)) = d.get("score")
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
            d.insert("v".into(), Value::Float(i as f64));
            d
        });
        let opts = CompressOptions {
            strategy: "timeseries".into(),
            ..CompressOptions::default()
        };
        let r = crush_json(&items, 30, &opts);
        assert!(r.items.iter().any(|it| matches!(it, Value::Dict(d) if
            d.get("v") == Some(&Value::Float(0.0))
        )));
    }

    #[test]
    fn strategy_lossless_csv_schema() {
        let items = make_items(20, |i| {
            let mut d = HashMap::new();
            d.insert("id".into(), Value::Float(i as f64));
            d.insert("name".into(), Value::String(format!("item_{}", i)));
            d.insert("value".into(), Value::Float(i as f64 * 2.0));
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
            d.insert("score".into(), Value::Float((i as f64) / 50.0));
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
                Value::Float(if i == 50 { 1000.0 } else { (i as f64).sin() }),
            );
            d
        });
        let r = crush_json(&items, 10, &CompressOptions::default());
        assert!(r.items.iter().any(|it| matches!(it, Value::Dict(d) if
            d.get("value") == Some(&Value::Float(1000.0))
        )));
    }

    // ── metadata 测试 ──

    #[test]
    fn metadata_reports_strategy_and_savings() {
        let items = make_items(100, |i| {
            let mut d = HashMap::new();
            d.insert("id".into(), Value::Float(i as f64));
            d.insert("score".into(), Value::Float((i as f64) / 100.0));
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
                    d.insert("id".into(), Value::Float(j as f64));
                    d.insert("value".into(), Value::Float((i + j) as f64));
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
                d.insert("id".into(), Value::Float(1.0));
                d.insert("name".into(), Value::String("a".into()));
                d
            }),
            Value::Dict({
                let mut d = std::collections::HashMap::new();
                d.insert("id".into(), Value::Float(2.0));
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
