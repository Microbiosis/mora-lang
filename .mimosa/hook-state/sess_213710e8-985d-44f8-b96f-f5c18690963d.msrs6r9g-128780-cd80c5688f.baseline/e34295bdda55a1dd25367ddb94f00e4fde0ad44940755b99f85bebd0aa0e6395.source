//! v0.75.55: SmartCrusher 3 种安全约束（从 compress/json.rs 拆出）。
//! KeepErrors / KeepOutliers / KeepBoundary + z-score 异常检测。
//! Constraint trait 定义在 super::json。

use super::json::{Constraint, ERROR_KEYWORDS, FieldRole, FieldStats};
use crate::value::Value;

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
            if let Value::Float(n) = v {
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
