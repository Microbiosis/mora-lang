//! v0.75.55: SmartCrusher 5 种压缩策略（从 compress/json.rs 拆出）。
//! TopN / TimeSeries / ClusterSample / SmartSample / Lossless + 约束应用辅助
//! apply_all/finalize。Strategy/Constraint trait 定义在 super::json。

use std::collections::HashSet;

use super::json::{Constraint, FieldRole, FieldStats, Strategy};
use crate::value::Value;

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
                            if let Value::Float(n) = v {
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
