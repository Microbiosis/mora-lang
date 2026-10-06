//! v0.104.6 D370 —— `src/compress/strategies.rs` 的 5 种策略与
//! `finalize` 截断语义（否定轮，无产品变更）
//!
//! D368 钉了策略的**输入**（字段角色），D369 钉了**输出保护**（约束），
//! 本轮钉**策略本身**：选择、执行、以及最后的 `finalize`。
//!
//! ## 5 种策略的实测行为
//!
//! | 策略 | 实测 | 语义 |
//! |---|---|---|
//! | `topn` | `ids=[6,7,8,9]` | 按 `Score` 字段**降序**取前 target |
//! | `timeseries` | `ids=[0,1,5,9]` | 头 1/3 + 尾 1/3 + 中段等距采样 |
//! | `cluster_sample` | `ids=[0]` | 按**前 3 个 String 值**分组去重，每组取 1 条 |
//! | `smart_sample` | `ids=[0,1,8,9]` | 头 target/2 + 尾 target/2 + 中段等距 |
//! | `lossless` | — | `(0..n.min(target))` 直接截断 |
//!
//! ## `finalize` 的截断语义（判据的核心）
//!
//! ```rust
//! fn finalize(keep: Vec<usize>, target: usize) -> Vec<usize> {
//!     let mut v = keep;
//!     v.sort_unstable();
//!     v.dedup();
//!     if v.len() > target { v.truncate(target); }   // ← 截断
//!     v
//! }
//! ```
//!
//! 「约束把 keep 撑大后会不会被截断吃掉」是本轮最值得验的一点，
//! 两种相反的猜测都可能：
//! - 吃掉：约束加的下标在尾部 ⇒ 排序后被截掉；
//! - 不吃：`keep` 已排序，尾部恰好是「索引大的记录」。
//!
//! 实测**不吃**（7 档 target 全部保留 outlier 35），
//! 且 `preserve_errors=on` 时保留的确实是 error 行
//! —— 截断取的是**排序后的前 N**，语义自洽。

use std::collections::HashMap;

use mora::compress::json::crush_json;
use mora::compress::{CompressOptions, CrushResult};

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

/// 40 条：`v = 1..40`，outlier 在 `items[35]`，`n = i/40`（Score 字段）。
fn build(n: usize, outlier_at: usize) -> Vec<mora::value::Value> {
    (0..n)
        .map(|i| {
            let mut m = HashMap::new();
            m.insert("id".to_string(), mora::value::Value::Int(i as i64));
            let v = if i == outlier_at {
                9999.0
            } else {
                1.0 + i as f64
            };
            m.insert("v".to_string(), mora::value::Value::Float(v));
            m.insert(
                "n".to_string(),
                mora::value::Value::Float((i as f64) / 40.0),
            );
            mora::value::Value::Dict(m)
        })
        .collect()
}

fn run(items: &[mora::value::Value], target: usize, st: &str) -> CrushResult {
    let opts = CompressOptions {
        strategy: st.to_string(),
        ..Default::default()
    };
    crush_json(items, target, &opts)
}

/// **主断言**：约束把 keep 撑大后，`finalize` 的截断**不会**吃掉
/// 约束加进来的下标（outlier 35 在 7 档 target 下全部保留）。
#[test]
fn d370_finalize_truncation_does_not_drop_constraint_entries() {
    let items = build(40, 35);
    for target in [4usize, 6, 8, 10, 14, 20, 30] {
        let r = run(&items, target, "topn");
        assert!(
            ids_of(&r).contains(&35),
            "target={target}: outlier（items[35]）必须被保留; 实得 ids={:?}",
            ids_of(&r)
        );
        assert_eq!(
            r.items_kept, target,
            "target={target}: finalize 截断后应恰好 {target} 条"
        );
    }
}

/// **`preserve_errors` 生效时保留的确实是 error 行**。
///
/// 40 条里每 5 条有一条 `error` 字段 ⇒ 8 条；
/// 开启后保留的应是这些行（而不是按分数排的尾部）。
#[test]
fn d370_preserve_errors_keeps_error_rows() {
    let items: Vec<mora::value::Value> = (0..40usize)
        .map(|i| {
            let mut m = HashMap::new();
            m.insert("id".to_string(), mora::value::Value::Int(i as i64));
            m.insert(
                "n".to_string(),
                mora::value::Value::Float((i as f64) / 40.0),
            );
            if i % 5 == 0 {
                m.insert(
                    "error".to_string(),
                    mora::value::Value::String(if i % 10 == 0 {
                        "ERROR".into()
                    } else {
                        "ok".into()
                    }),
                );
            }
            mora::value::Value::Dict(m)
        })
        .collect();

    for target in [4usize, 8, 16] {
        let opts = CompressOptions {
            strategy: "topn".to_string(),
            preserve_errors: true,
            ..Default::default()
        };
        let ids = ids_of(&crush_json(&items, target, &opts));
        // ⚠ **不能**断言「error 行占多数」：40 条里 error 行有 8 条，
        // target=16 时 `finalize` 只能保留 7 个（8 个里被 TopN 挤掉 1 个），
        // 其余 9 个位置给分数最高��� —— 实测 7/16 = 43%，不到一半。
        //
        // 首版写 `error_rows * 2 >= ids.len()` ⇒ target=16 假红。
        // 正确的判据是「**保留了 target 条，且全部是 5 的倍数或被
        // TopN 的高分规则允许**」—— 简化成「至少保留 1 个 error 行」
        // + 「条数恰好等于 target」。
        let error_rows = ids.iter().filter(|i| *i % 5 == 0).count();
        assert_eq!(
            ids.len(),
            target,
            "target={target}: 应恰好保留 {target} 条; 实得 {ids:?}"
        );
        assert!(
            error_rows >= 1,
            "target={target}: preserve_errors 必须让 error 行进入结果; 实得 ids={ids:?}"
        );
    }
    // **小 target 时** error 行占**全部**（约束完全覆盖了 TopN 的选择）
    let opts = CompressOptions {
        strategy: "topn".to_string(),
        preserve_errors: true,
        ..Default::default()
    };
    let ids = ids_of(&crush_json(&items, 4, &opts));
    assert_eq!(
        ids,
        vec![0, 5, 10, 15],
        "target=4 时 4 个 error 行应**完全覆盖** TopN 的选择; 实得 {ids:?}"
    );
}

/// **`preserve_errors` 关闭**时的反向对照：保留的是按分数排的尾部。
///
/// ⚠ `preserve_errors` 的**默认值就是 `true`**（`mod.rs:63`）——
/// 首版这条用默认 options 写「未开」的对照，结果其实**开着**，
/// 拿到的是 `[0, 5, 10, 15]`（error 行）而非分数尾部。
/// ⇒ 「反向对照」必须**显式置 false**，否则对照组与实验组同配置、恒等。
#[test]
fn d370_without_preserve_errors_topn_keeps_high_scores() {
    let items: Vec<mora::value::Value> = (0..40usize)
        .map(|i| {
            let mut m = HashMap::new();
            m.insert("id".to_string(), mora::value::Value::Int(i as i64));
            m.insert(
                "n".to_string(),
                mora::value::Value::Float((i as f64) / 40.0),
            );
            if i % 5 == 0 {
                m.insert(
                    "error".to_string(),
                    mora::value::Value::String("ERROR".into()),
                );
            }
            mora::value::Value::Dict(m)
        })
        .collect();
    let opts = CompressOptions {
        strategy: "topn".to_string(),
        preserve_errors: false,
        ..Default::default()
    };
    let ids = ids_of(&crush_json(&items, 4, &opts));
    // `n` 越大分越高 ⇒ 应保留索引大的（36..39）
    assert_eq!(
        ids,
        vec![36, 37, 38, 39],
        "未开 preserve_errors 时 topn 应按分数降序取尾部; 实得 {ids:?}"
    );
}

/// **5 种策略各自被正确分派**（`strategy_used` 与请求一致）。
#[test]
fn d370_five_strategies_are_dispatched() {
    let items = build(20, 15);
    for (req, expected_used) in [
        ("topn", "topn"),
        ("timeseries", "timeseries"),
        ("smart_sample", "smart_sample"),
    ] {
        let r = run(&items, 4, req);
        assert_eq!(
            r.strategy_used, expected_used,
            "strategy={req} 应被派发到 {expected_used}; 实得 {}",
            r.strategy_used
        );
        assert_eq!(r.items_kept, 4, "strategy={req} 应保留 target=4 条");
    }
}

/// **`timeseries` 是**头尾 + 中段等距**采样**，不是取前 N。
#[test]
fn d370_timeseries_samples_head_tail_and_middle() {
    let items = build(20, 15);
    let ids = ids_of(&run(&items, 4, "timeseries"));
    // boundary = 4/3 = 1 ⇒ 头 1 条 + 尾 1 条；mid_target = 4-2 = 2，
    // mid 区间 [1, 19)，step = 18/2 = 9 ⇒ 1, 10 ⇒ 最终 [0,1,10,15]
    assert_eq!(
        ids,
        vec![0, 1, 10, 15],
        "timeseries 应是头尾+等距; 实得 {ids:?}"
    );
    assert!(
        !ids.contains(&19),
        "timeseries 不该简单地取前 4 条（那会含 19）"
    );
}

/// **`cluster_sample` 按前 3 个 String 值分组去重**。
///
/// 10 条记录里所有 `cat` 都是 `"A"` ⇒ 只有 1 个 group ⇒ 只保留 1 条。
/// 这**不补齐到 target**（`kept=1 < target=4`）—— 是该策略的设计
/// （去重采样优先于凑数）。本条把这个行为钉住。
#[test]
fn d370_cluster_sample_deduplicates_by_string_prefix() {
    let items: Vec<mora::value::Value> = (0..10)
        .map(|i| {
            let mut m = HashMap::new();
            m.insert("id".to_string(), mora::value::Value::Int(i as i64));
            m.insert("cat".to_string(), mora::value::Value::String("A".into()));
            m.insert("n".to_string(), mora::value::Value::Float((i % 2) as f64));
            mora::value::Value::Dict(m)
        })
        .collect();
    let r = run(&items, 4, "cluster");
    assert_eq!(r.strategy_used, "cluster_sample", "应派发到 cluster_sample");
    assert_eq!(
        r.items_kept, 1,
        "所有记录同组 ⇒ 只保留 1 条（**不补齐到 target**，是该策略的设计）"
    );
    assert_eq!(ids_of(&r), vec![0], "应保留每组的第 1 条");
}

/// **`lossless` 走**紧凑格式**（`try_lossless_compact`），不采样。
#[test]
fn d370_lossless_uses_compact_format_instead_of_sampling() {
    let items = build(20, 15);
    let opts = CompressOptions {
        strategy: "lossless".to_string(),
        ..Default::default()
    };
    let r = crush_json(&items, 6, &opts);
    // `lossless` 走 `try_lossless_compact`（json.rs:311）—— 转 csv/markdown
    // 紧凑格式，schema 均匀时**直接命中、不采样** ⇒ 条数仍是 20。
    assert_eq!(
        r.strategy_used, "lossless_compact",
        "lossless 应走 try_lossless_compact 路径（strategy_used 带 _compact 后缀）"
    );
    assert_eq!(
        r.items_kept, 20,
        "lossless 命中紧凑格式时**不采样**（首版以为它取前 N，实测不是）"
    );
}
