//! v0.104.6 D369 —— `KeepOutliersConstraint` 的**下标错位**（已修）
//!
//! D368 钉了字段角色判定，本轮钉它的**下游**：
//! `constraints.rs` 的三条安全约束。角色判对之后，
//! 约束把哪些记录**强制保留** —— 错了会**静默丢数据**。
//!
//! ## 缺陷：outlier 保护**保错了记录**
//!
//! `constraints.rs` 的 `KeepOutliersConstraint::apply` 修前：
//!
//! ```rust
//! let values: Vec<&Value> = items.iter()
//!     .filter_map(|it| if let Value::Dict(d) = it { d.get(&field.name) } else { None })
//!     .collect();                       // ← 只收 values，**丢掉了 items 下标**
//! let outliers = outliers_by_zscore(&values, 2.0);   // ← 返回 **values 下标**
//! for i in outliers {
//!     if !keep.contains(&i) { keep.push(i); }         // ← 当成 **items 下标**
//! }
//! ```
//!
//! 只要有**任何一条记录缺这个键**（JSON 里极常见），
//! `values` 比 `items` 短，之后所有下标**整体前移**。
//!
//! 实测（40 条记录，outlier 在 `items[25]`）：
//!
//! | 缺键数 | 修前保留 | 修后保留 |
//! |---|---|---|
//! | 0 | `25` ✅ | `25` ✅ |
//! | 2 | **`23`** ❌（= 25−2）| `25` ✅ |
//! | 5 | **`20`** ❌（= 25−5）| `25` ✅ |
//!
//! ⇒ 修前**保留了错误的记录**，而**真正的 outlier 反而被丢掉** ——
//! `preserve_outliers` 这条安全约束**恰好在它该生效的场景里失效**。
//!
//! ## 修法：收 `(items 下标, &Value)`，把下标一起传下去
//!
//! `value_to_item` 是一张「values 下标 → items 下标」的映射表。
//!
//! ## 前提：必须让字段**真的**判成 `Anomaly`
//!
//! `KeepOutliersConstraint` 只对 `role == Anomaly` 的字段跑。
//! 而 `detect_anomaly` 要求 outlier 占比 `count * 20 <= nums.len()`（≤5%）
//! —— **分母是「有值的条数」**，缺键多时占比超标，角色会降级为 `Generic`
//! ⇒ 约束整条不触发（这也是 D368 观察到的现象）。
//!
//! 所以判据用 40 条数据（缺 2/5 键时占比仍 2.6%/2.9% ≤ 5%），
//! 并配一个**缺 8 键**的对照（占比 1/32 = 3.1%… 实测需按实际值断言）。

use std::collections::HashMap;

use mora::compress::json::crush_json;
use mora::compress::{CompressOptions, CrushResult};

/// 40 条记录，`v = 1..40`，outlier 在 `items[outlier_at]`；
/// 前 `missing` 条**缺 `v` 键**。
fn build(n: usize, missing: usize, outlier_at: usize) -> Vec<mora::value::Value> {
    (0..n)
        .map(|i| {
            let mut m = HashMap::new();
            m.insert("id".to_string(), mora::value::Value::Int(i as i64));
            if i >= missing {
                let v = if i == outlier_at {
                    9999.0
                } else {
                    1.0 + i as f64
                };
                m.insert("v".to_string(), mora::value::Value::Float(v));
            }
            mora::value::Value::Dict(m)
        })
        .collect()
}

fn ids_of(r: &CrushResult) -> Vec<i64> {
    let mut v: Vec<i64> = r
        .items
        .iter()
        .filter_map(|it| match it {
            mora::value::Value::Dict(d) => match d.get("id") {
                Some(mora::value::Value::Int(i)) => Some(*i),
                _ => None,
            },
            _ => None,
        })
        .collect();
    v.sort();
    v
}

fn run(items: &[mora::value::Value], target: usize, po: bool) -> CrushResult {
    let opts = CompressOptions {
        strategy: "auto".to_string(),
        preserve_outliers: po,
        ..Default::default()
    };
    crush_json(items, target, &opts)
}

/// **主断言**：缺键时 outlier 仍被**正确**保留。
///
/// 修前保留的是 `outlier_at - missing`（错位的 values 下标）。
#[test]
fn d369_outlier_is_kept_even_when_some_records_lack_the_field() {
    for missing in [0usize, 2, 5] {
        let items = build(40, missing, 25);
        let r = run(&items, 8, true);
        let ids = ids_of(&r);
        assert!(
            ids.contains(&25),
            "缺 {missing} 个键时，outlier（items[25]）必须被保留；\
             修前保留的是错位的 {}。实得 ids={ids:?}",
            25i64 - missing as i64
        );
    }
}

/// **反向对照**：不缺键时保留 25（修前就对）—— 修复**不得**改变它。
#[test]
fn d369_complete_data_keeps_the_same_outlier_as_before() {
    let items = build(40, 0, 25);
    let r = run(&items, 8, true);
    assert!(ids_of(&r).contains(&25), "不缺键时修前就正确，行为不得变");
}

/// **反向对照 2**：`preserve_outliers = false` 时该约束**不生效**。
///
/// 若这条也保留 25，说明约束层整体失效（而不是本轮的映射问题）。
#[test]
fn d369_preserve_outliers_false_disables_the_constraint() {
    let items = build(40, 0, 25);
    let r = run(&items, 8, false);
    let ids = ids_of(&r);
    assert!(
        !ids.contains(&25),
        "preserve_outliers=false 时约束不生效；实得 ids={ids:?}"
    );
}

/// **前提断言**：`v` 必须真的判成 `Anomaly`，否则上面的判据**恒绿**。
///
/// 这是本文件的关键 —— 若角色降级为 `Generic`，约束整条不跑，
/// 「outlier 被保留」会是因为**别的机制**（topn 边界）而成立。
#[test]
fn d369_field_must_really_be_anomaly_for_the_assertions_to_mean_anything() {
    for missing in [0usize, 2, 5] {
        let items = build(40, missing, 25);
        let r = run(&items, 8, true);
        let v_role = r.fields.iter().find(|f| f.name == "v").map(|f| f.role);
        assert_eq!(
            v_role,
            Some(mora::compress::FieldRole::Anomaly),
            "缺 {missing} 个键时 `v` 应仍判 Anomaly；\
             若降级为 Generic，下游约束不触发，本文件的断言全部失效"
        );
    }
}

/// **边界**：缺键多到让 outlier 占比超标时，角色降级、`Anomaly` 约束不触发。
///
/// `detect_anomaly` 要求 `count * 20 <= nums.len()`。
/// 40 条缺 20 键 ⇒ `nums.len() = 20`，`1 * 20 <= 20` 仍成立。
/// 缺 25 键 ⇒ `nums.len() = 15`，`1 * 20 <= 15` **不成立** ⇒ 降级。
#[test]
fn d369_too_many_missing_keys_downgrades_role_and_skips_constraint() {
    let items = build(40, 25, 30); // 15 个有值，outlier 在其中
    let r = run(&items, 4, true);
    let v_role = r.fields.iter().find(|f| f.name == "v").map(|f| f.role);
    assert_eq!(
        v_role,
        Some(mora::compress::FieldRole::Generic),
        "缺 25 键时 outlier 占比 1/15 > 5% ⇒ 应降级为 Generic"
    );
}
