//! D231 判据：`compress` 的数值分析必须把 `Int` 与 `Float` **同等对待**。
//!
//! ## 缺陷背景
//!
//! 修前全仓有 **5 处**各自手写 `if let Value::Float(n) = v`（`detect.rs` ×3、
//! `strategies.rs` ×1、`constraints.rs` ×1），**全部**只认 `Float`。而本仓
//! 数字有两个来源：dict 字面量给 `Float`（D98），`json.parse` 给 `Int`（D129）。
//!
//! ## 后果实测（经 `CrushResult.fields` 观察；`compress::detect` 是私有模块）
//!
//! | 语料 | 修前 `is_numeric` | 修前 `array_type` / 策略 | 修后 |
//! |---|---|---|---|
//! | `[{"id":1},…]`（Int） | **false** | `Uniform` / `lossless` | `true` / `TopScores` / `topn` |
//! | `[{"id":1.0},…]`（对照） | true | `TopScores` / `topn` | 同上 |
//! | 混排 `[{"n":1},{"n":2.5},…]` | **false** | `Uniform` / `lossless` | `true` / `TopScores` / `topn` |
//!
//! 即 `json.parse` 读入的整数列**完全不被识别为数值**：字段角色推断
//! （`Id` 角色检测不到）、`numeric_range`（`None`）、`TopN` 排序
//! （整数 score 全部退化成 `0.0` ⇒ 退化为稳定保序 ⇒ **根本没在排序**）、
//! outlier 保护全部失准，且**零诊断**。
//!
//! ## 判据形态：**两条表示路径等价**
//!
//! 与 D230 同一形态 —— 「表示差异不得影响语义」正是缺陷的本质。
//! 写死具体策略名在别的数据下会脆。

use mora::compress::CompressOptions;
use mora::compress::json::crush_json;
use mora::flow::json_to_value;
use mora::value::Value;

fn items(s: &str) -> Vec<Value> {
    match json_to_value(s).expect("json must parse") {
        Value::List(l) => l.to_vec(),
        other => panic!("expect array, got {other:?}"),
    }
}

/// `crush_json` 的一个字段观测：`(字段名, 是否数值型, 数值范围)`。
type FieldObs = (String, bool, Option<(f64, f64)>);

/// `analyze` 的返回：`(字段观测, 原始 JSON, 压缩后 JSON)`。
type Analysis = (Vec<FieldObs>, String, String);

fn analyze(s: &str) -> Analysis {
    let r = crush_json(&items(s), 3, &CompressOptions::default());
    let fields = r
        .fields
        .iter()
        .map(|f| (f.name.clone(), f.is_numeric, f.numeric_range))
        .collect();
    (
        fields,
        format!("{:?}", r.array_type),
        r.strategy_used.clone(),
    )
}

/// 判据 ①：整数列与浮点列识别出**相同**的字段特征。
#[test]
fn d231_int_and_float_columns_analyse_identically() {
    let int = analyze(
        r#"[{"id":1},{"id":2},{"id":3},{"id":4},{"id":5},{"id":6},{"id":7},{"id":8},
            {"id":9},{"id":10},{"id":11},{"id":12}]"#,
    );
    let float = analyze(
        r#"[{"id":1.0},{"id":2.0},{"id":3.0},{"id":4.0},{"id":5.0},{"id":6.0},{"id":7.0},{"id":8.0},
            {"id":9.0},{"id":10.0},{"id":11.0},{"id":12.0}]"#,
    );
    assert!(
        int.0.iter().any(|(_, n, _)| *n),
        "D231: 整数列被判为**非数值**（is_numeric=false, range=None）—— \
         json.parse 读入的整数完全不被识别。fields={:?}",
        int.0
    );
    assert_eq!(
        int, float,
        "D231: 整数列与浮点列应产出完全相同的分析结果。\n\
         Int:   {:?} / {} / {}\nFloat: {:?} / {} / {}",
        int.0, int.1, int.2, float.0, float.1, float.2
    );
}

/// 判据 ②：Int/Float **混排**的列（同列既有整数又有小数）也必须被识别。
///
/// 这是 `json.parse` 最常见的结果形态：整数列给 `Int`、小数给 `Float`。
/// 修前只要列里有一个 `Int`，整列就不是数值。
#[test]
fn d231_mixed_int_float_column_is_numeric() {
    let mixed =
        analyze(r#"[{"n":1},{"n":2.5},{"n":3},{"n":4.5},{"n":5},{"n":6.5},{"n":7},{"n":8.5}]"#);
    assert!(
        mixed.0.iter().all(|(_, n, r)| *n && r.is_some()),
        "D231: 混排列里有 Int 就整列不是数值 —— fields={:?}",
        mixed.0
    );
    let all_float = analyze(
        r#"[{"n":1.0},{"n":2.5},{"n":3.0},{"n":4.5},{"n":5.0},{"n":6.5},{"n":7.0},{"n":8.5}]"#,
    );
    assert_eq!(mixed, all_float, "混排与全 Float 应等价");
}

/// 判据 ③：`TopNStrategy` 必须真的按分数**选**。
///
/// 修前整数 score 全部 `unwrap_or(0.0)` 退化成全等分 ⇒ 稳定保序，
/// 于是「TopN 选分数最高的 N 个」变成「取前 N 个」—— 选出来的**不是**最高分。
///
/// ⚠ 判据形态：断言**选中集合**是最高分的 3 个，**不断言输出顺序**。
/// 第一版写死 `vec![9.0, 8.0, 7.0]`（降序）而实测得到 `[9.0, 7.0, 8.0]` ——
/// `TopNStrategy` 选出的是正确集合，但**输出未按分数重排**。
/// 那是与 D231 无关的另一个性质，不该混进本判据（会同时让 Int/Float
/// 两条路径一起红，掩盖真正的缺陷）。故改为**集合**比较。
#[test]
fn d231_topn_actually_selects_highest_int_scores() {
    // 分数故意乱序，且**全为整数**（json.parse 形态）
    let out = crush_json(
        &items(r#"[{"s":1},{"s":9},{"s":3},{"s":7},{"s":2},{"s":8},{"s":4},{"s":6},{"s":5}]"#),
        3,
        &CompressOptions {
            strategy: "topn".into(),
            ..Default::default()
        },
    );
    let mut scores: Vec<f64> = out
        .items
        .iter()
        .filter_map(|it| match it {
            Value::Dict(d) => match d.get("s") {
                Some(Value::Int(i)) => Some(*i as f64),
                Some(Value::Float(f)) => Some(*f),
                _ => None,
            },
            _ => None,
        })
        .collect();
    scores.sort_by(|a, b| b.partial_cmp(a).unwrap());
    assert_eq!(
        scores,
        vec![9.0, 8.0, 7.0],
        "D231: 整数 score 未被识别 ⇒ 全部退化成 0.0 ⇒ 排序退化为稳定保序，\
         选出的是「前 3 个」而非「分数最高的 3 个」。实得（降序后）{:?}",
        scores
    );
}

/// 判据 ④：对照组 —— 浮点 score 的 TopN 本来就选对。
///
/// 防止判据 ③ 因为「TopN 本来就没在按分数选」而一起绿。
#[test]
fn d231_topn_float_control_group() {
    let out = crush_json(
        &items(
            r#"[{"s":1.0},{"s":9.0},{"s":3.0},{"s":7.0},{"s":2.0},{"s":8.0},{"s":4.0},{"s":6.0},{"s":5.0}]"#,
        ),
        3,
        &CompressOptions {
            strategy: "topn".into(),
            ..Default::default()
        },
    );
    let mut scores: Vec<f64> = out
        .items
        .iter()
        .filter_map(|it| match it {
            Value::Dict(d) => match d.get("s") {
                Some(Value::Int(i)) => Some(*i as f64),
                Some(Value::Float(f)) => Some(*f),
                _ => None,
            },
            _ => None,
        })
        .collect();
    scores.sort_by(|a, b| b.partial_cmp(a).unwrap());
    assert_eq!(
        scores,
        vec![9.0, 8.0, 7.0],
        "对照组失效：浮点 score 也没选对 —— 判据 ③ 会一起绿"
    );
}

/// 判据 ⑤：顺序整数列应被识别为**顺序 Id**（`is_sequential_numeric` 的 Int 侧）。
///
/// 修前 `is_sequential_numeric` 只认 `Float`，所以 `json.parse` 读入的
/// `id: 1,2,3,…` 永远拿不到 `Id` 角色。
#[test]
fn d231_sequential_int_column_gets_id_role() {
    let (int_fields, _, _) = analyze(
        r#"[{"seq":1000},{"seq":1001},{"seq":1002},{"seq":1003},{"seq":1004},{"seq":1005},{"seq":1006},{"seq":1007}]"#,
    );
    let (float_fields, _, _) = analyze(
        r#"[{"seq":1000.0},{"seq":1001.0},{"seq":1002.0},{"seq":1003.0},{"seq":1004.0},{"seq":1005.0},{"seq":1006.0},{"seq":1007.0}]"#,
    );
    assert_eq!(
        int_fields, float_fields,
        "D231: 顺序整数列与顺序浮点列应得到相同的角色判定"
    );
}

/// 判据 ⑥：非数值类型**仍被拒绝**（修法只放宽 Int，不是放弃类型检查）。
#[test]
fn d231_non_numeric_still_rejected() {
    // 字符串列不应被判为数值
    let (fields, _, _) = analyze(
        r#"[{"t":"a"},{"t":"b"},{"t":"c"},{"t":"d"},{"t":"e"},{"t":"f"},{"t":"g"},{"t":"h"}]"#,
    );
    assert!(
        fields.iter().all(|(_, n, _)| !*n),
        "D231: 字符串列被误判为数值 —— fields={:?}",
        fields
    );
    // 混合列（数字 + 字符串）也不应是数值
    let (mixed, _, _) = analyze(r#"[{"m":1},{"m":"x"},{"m":3},{"m":4},{"m":5},{"m":6}]"#);
    assert!(
        mixed.iter().all(|(_, n, _)| !*n),
        "D231: 混合列被误判为数值 —— fields={:?}",
        mixed
    );
}

/// 判据 ⑧：整数列的 **z-score outlier** 必须被检出。
///
/// 首轮牙齿验证里 `outliers_by_zscore` 的回退报 **NO TEETH** ——
/// 判据 ①②只覆盖了 `is_numeric` / `array_type` 这一层，
/// 没有单独钉住「整数列也能产生 outlier」。
///
/// ⚠ 语料必须走**会应用约束**的策略。第一版用 `default()`（选中
/// `Uniform` → `LosslessStrategy`），而 `LosslessStrategy::select`
/// 的约束参数就叫 `_constraints` —— **它有意忽略**（lossless 语义上
/// 不该丢数据，属设计而非缺陷）。于是一侧断言「极端值必须被保留」
/// 在 Int/Float 两侧**同时**失败，测的是策略语义不是 D231。
/// 改用带 `score` 列 ⇒ `TopScores` → `TopNStrategy`，它会 `apply_all`。
#[test]
fn d231_integer_column_detects_zscore_outliers() {
    let extreme = 1000i64;
    let mk_int = |v: i64, score: f64| {
        let mut d = std::collections::HashMap::new();
        d.insert("v".into(), Value::Int(v));
        d.insert("score".into(), Value::Float(score));
        Value::Dict(d)
    };
    let mk_float = |v: f64, score: f64| {
        let mut d = std::collections::HashMap::new();
        d.insert("v".into(), Value::Float(v));
        d.insert("score".into(), Value::Float(score));
        Value::Dict(d)
    };
    let n_items = 25i64; // 1/25 = 4% <= 5%，满足 detect_anomaly 的 outlier 数量上限
    let int_items: Vec<Value> = (0..n_items)
        .map(|i| mk_int(if i == 7 { extreme } else { 10 + i }, (i as f64) / 10.0))
        .collect();
    let float_items: Vec<Value> = (0..n_items)
        .map(|i| {
            let v = if i == 7 { extreme } else { 10 + i };
            mk_float(v as f64, (i as f64) / 10.0)
        })
        .collect();

    let opts = CompressOptions::default();
    let r_int = crush_json(&int_items, 3, &opts);
    let r_float = crush_json(&float_items, 3, &opts);
    assert_eq!(
        r_int.array_type, r_float.array_type,
        "前置：两侧必须落在同一 array_type，否则约束路径不同、比较无意义"
    );
    assert_eq!(
        r_int.strategy_used, "topn",
        "前置：本判据需要走会应用约束的策略；实得 {}",
        r_int.strategy_used
    );

    let vals = |r: &mora::compress::CrushResult| -> Vec<i64> {
        r.items
            .iter()
            .filter_map(|it| match it {
                Value::Dict(d) => match d.get("v") {
                    Some(Value::Int(i)) => Some(*i),
                    Some(Value::Float(f)) => Some(*f as i64),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    };
    let int_kept = vals(&r_int);
    let float_kept = vals(&r_float);

    assert_eq!(
        int_kept, float_kept,
        "D231: 整数列与浮点列的 outlier 保留结果应相同。\nInt:   {:?}\nFloat: {:?}\n\
         修前 outliers_by_zscore 只认 Float ⇒ 整数列**永远不产生任何 outlier**，\
         preserve_outliers 保护对整数列静默失效",
        int_kept, float_kept
    );
    assert!(
        int_kept.contains(&extreme),
        "D231: 极端值 {extreme} 未被保留 —— outlier 保护失效。kept={int_kept:?}"
    );
}

/// 判据 ⑦：整数 Unix 时间戳应被识别为 **Temporal**。
///
/// `is_timestamp_pattern` 的数值分支修前只匹配 `Value::Float`，而
/// **Unix 秒几乎总是整数** —— `json.parse` 读入的时间戳因此
/// 永远拿不到 `Temporal` 角色，`TimeSeries` 策略无从触发。
///
/// 这条是首轮普查**漏掉**的：它写的是 `Value::Float(n) => …`（`match` 分支），
/// 而我第一遍的 grep 模式只覆盖 `if let Value::Float(…)`。
/// ⇒ 「普查」本身也要有普查。
#[test]
fn d231_integer_unix_timestamp_is_temporal() {
    // Unix 秒（约 1.7e9）—— 整数形态
    let int_ts = analyze(
        r#"[{"ts":1700000000,"v":1},{"ts":1700000060,"v":2},{"ts":1700000120,"v":3},
            {"ts":1700000180,"v":4},{"ts":1700000240,"v":5},{"ts":1700000300,"v":6}]"#,
    );
    // 对照组：同一批时间戳写成 Float
    let float_ts = analyze(
        r#"[{"ts":1700000000.0,"v":1},{"ts":1700000060.0,"v":2},{"ts":1700000120.0,"v":3},
            {"ts":1700000180.0,"v":4},{"ts":1700000240.0,"v":5},{"ts":1700000300.0,"v":6}]"#,
    );
    assert_eq!(
        int_ts, float_ts,
        "D231: 整数 Unix 时间戳与浮点版应得到相同的分析结果。\n\
         Int:   {:?} / {}\nFloat: {:?} / {}\n\
         修前整数时间戳拿不到 Temporal 角色 ⇒ TimeSeries 策略无从触发",
        int_ts.0, int_ts.1, float_ts.0, float_ts.1
    );
}
