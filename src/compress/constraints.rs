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
            // v0.104.6 D369：**必须记 items 下标，不能只收 `values`**。
            //
            // 修前只 `filter_map` 收 `values`，`outliers_by_zscore` 返回的
            // 是**values 的下标**，而下面 `keep.push(i)` 把它当 **items 下标**
            // 用 ⇒ 只要有任何一条记录**缺这个键**（JSON 里极常见），
            // 之后所有下标**全部错位**。
            //
            // 实测（20 条记录，前 3 条缺 `v`，outlier 在 items[15]）：
            //
            // ```text
            // items n=20, values n=17, outlier 在 values 里是 [12]
            // constraint 保留 items[12]（值 22.0）—— 错
            // 真正的 outlier items[15]（值 9999.0）—— 被丢掉
            // ```
            //
            // 修法：收 `(items 下标, &Value)`，并把 items 下标传下去。
            let indexed: Vec<(usize, &Value)> = items
                .iter()
                .enumerate()
                .filter_map(|(i, it)| {
                    if let Value::Dict(d) = it {
                        d.get(&field.name).map(|v| (i, v))
                    } else {
                        None
                    }
                })
                .collect();
            let values: Vec<&Value> = indexed.iter().map(|(_, v)| *v).collect();
            let value_to_item: Vec<usize> = indexed.iter().map(|(i, _)| *i).collect();
            let outliers = outliers_by_zscore(&values, 2.0);
            for pos in outliers {
                // `pos` 是 values 下标 ⇒ 映回 items 下标。
                let Some(&item_idx) = value_to_item.get(pos) else {
                    continue;
                };
                if !keep.contains(&item_idx) {
                    keep.push(item_idx);
                }
            }
        }
    }
}

pub fn outliers_by_zscore(values: &[&Value], z: f64) -> Vec<usize> {
    // v0.104.6 D231：数值提取统一走 `super::json::value_as_f64`。
    // 修前只认 `Float` ⇒ `json.parse` 读入的整数列**永远不产生任何 outlier**，
    // `preserve_outliers` 保护对整数列**静默失效**。
    let nums: Vec<(usize, f64)> = values
        .iter()
        .enumerate()
        .filter_map(|(i, v)| super::json::value_as_f64(v).map(|n| (i, n)))
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
