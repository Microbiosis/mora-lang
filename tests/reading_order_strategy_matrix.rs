//! v0.104.6 D376 —— `reading_order` 的 **6 种策略矩阵**（否定轮，无产品变更）
//!
//! `src/document/reading_order/` 共 1333 行，有 3 个既有判据
//! （comparator 全序 / int bbox / xy-cut 平移不变性）+ 17 条自带单测。
//! 此前未覆盖的是 **`assign_reading_order` 的 6 种 `Strategy` 的实际输出**。
//!
//! ## 前提：bbox 必须是 `{x, y, w, h}` **字典**，不是列表
//!
//! `BBox::from_value`（`mod.rs:36`）只接受字典（block dict 的 `"bbox"`
//! 字段，或 block 自身含 x/y/w/h）。用 `[x, y, w, h]` **列表**时
//! `bbox` 全是 `None` ⇒ **全部策略都保持输入顺序**。
//!
//! ⇒ 首版探针因此得出「6 种策略都不排序」的**错误结论**（差一步就写成
//! 「双栏布局不被支持」的重大缺陷）。修正后结论完全反过来。
//!
//! 与 D368「钉住 `target < n` 前提」同构：**前提错了，全部结论都反**。
//!
//! ## 策略矩阵（双栏交错 + 字典 bbox）
//!
//! | 策略 | 输出 | 判定 |
//! |---|---|---|
//! | `InputOrder` | `L1,R1,L2,R2,L3,R3` | 按定义不排 ✅ |
//! | `TopToBottom` | 同上 | 按 y 再 x 排；本例 y 已升序 ⇒ 不变 ✅ |
//! | `GapTree` | 同上 | ✅ |
//! | `XyCut` | 同上 | 注释明写「**简化** XY-cut」，本就不分栏 ✅ |
//! | **`GroupBased`** | **`L1,L2,L3,R1,R2,R3`** | **唯一真正做双栏** ✅ |
//! | `XyCutPlusPlus` | 同上 | 见下 |
//!
//! ## `XyCutPlusPlus` 在等宽双栏上不切栏，是**算法的固有权衡**
//!
//! `compute_prefer_horizontal`（`xy_cut.rs:91`）按**密度比**决定首次切分
//! 方向：`x_density > BETA * y_density` ⇒ 横向优先。
//!
//! 等宽双栏必然 `sum_w / x_span > sum_h / y_span`（本例 270/95 = 2.84
//! vs 60/40 = 1.5）⇒ **选横向优先** ⇒ 退化为按 y 排。
//!
//! ⇒ **不是缺陷，是算法设计**。`GroupBased` 才按 x 重叠聚类实现双栏
//! （`mod.rs:226` 注释：「按 x 重叠聚类」）。
//!
//! ## 别名与兜底：15/15 全对
//!
//! `Strategy::from_str` 的 6 组别名 + 未知/空串 → 兜底 `TopToBottom`。

use mora::document::reading_order::{Strategy, assign_reading_order};
use mora::value::Value;

fn block(x: f64, y: f64, w: f64, h: f64, text: &str) -> Value {
    let mut bb = std::collections::HashMap::new();
    bb.insert("x".to_string(), Value::Float(x));
    bb.insert("y".to_string(), Value::Float(y));
    bb.insert("w".to_string(), Value::Float(w));
    bb.insert("h".to_string(), Value::Float(h));
    let mut d = std::collections::HashMap::new();
    d.insert("bbox".to_string(), Value::Dict(bb));
    d.insert("text".to_string(), Value::String(text.to_string()));
    Value::Dict(d)
}

fn plain(text: &str) -> Value {
    let mut d = std::collections::HashMap::new();
    d.insert("text".to_string(), Value::String(text.to_string()));
    Value::Dict(d)
}

/// 读 `reading_order_idx`（**同时接受 `Int` 与 `Float`**）。
///
/// ⚠ 写回侧（`mod.rs:278`）写的是 **`Value::Float`**，
/// 私有辅助 `reading_order_idx`（`mod.rs:309`）显式同时接受两种
/// （D230 修的）。首版只认 `Int` ⇒ 拿到**空列表**、误以为字段没写。
fn idx_of(v: &Value) -> Option<i64> {
    match v {
        Value::Dict(d) => match d.get("reading_order_idx") {
            Some(Value::Int(i)) => Some(*i),
            Some(Value::Float(f)) => Some(*f as i64),
            _ => None,
        },
        _ => None,
    }
}

fn order_text(items: &[Value]) -> String {
    let mut v: Vec<(i64, String)> = items
        .iter()
        .map(|it| {
            let d = match it {
                Value::Dict(d) => d,
                _ => return (0, String::new()),
            };
            let idx = idx_of(it).unwrap_or(-1);
            let t = match d.get("text") {
                Some(Value::String(s)) => s.clone(),
                _ => String::new(),
            };
            (idx, t)
        })
        .collect();
    v.sort_by_key(|(i, _)| *i);
    v.into_iter().map(|(_, t)| t).collect::<Vec<_>>().join(",")
}

const ALL: [Strategy; 6] = [
    Strategy::InputOrder,
    Strategy::TopToBottom,
    Strategy::GapTree,
    Strategy::XyCut,
    Strategy::GroupBased,
    Strategy::XyCutPlusPlus,
];

/// 双栏交错输入。
fn two_column() -> Vec<Value> {
    vec![
        block(0.0, 0.0, 45.0, 10.0, "L1"),
        block(50.0, 0.0, 45.0, 10.0, "R1"),
        block(0.0, 15.0, 45.0, 10.0, "L2"),
        block(50.0, 15.0, 45.0, 10.0, "R2"),
        block(0.0, 30.0, 45.0, 10.0, "L3"),
        block(50.0, 30.0, 45.0, 10.0, "R3"),
    ]
}

/// **反向对照**：单栏但几何顺序与输入顺序**相反**。
///
/// 数据构造（**注意输入数组顺序就是 A,B,C**，y 从大到小）：
///
/// ```text
/// A: y=30   B: y=20   C: y=10
/// ```
///
/// ⇒ `InputOrder` 保持 A,B,C；5 种几何策略必须**重排成 C,B,A**。
///
/// 这条证明策略**确实在工作**（不是恒等变换），
/// 并让「双栏不切栏」那部分**不是**「策略完全没实现」。
#[test]
fn d376_geometric_strategies_reorder_single_column() {
    let data = vec![
        block(0.0, 30.0, 40.0, 10.0, "A"),
        block(0.0, 20.0, 40.0, 10.0, "B"),
        block(0.0, 10.0, 40.0, 10.0, "C"),
    ];
    // `InputOrder` 按定义不排
    assert_eq!(
        order_text(&assign_reading_order(data.clone(), Strategy::InputOrder)),
        "A,B,C",
        "`InputOrder` 必须原样返回输入序"
    );
    // 其余 5 种都必须重排成「y 升序」= C,B,A
    for st in ALL.iter().filter(|s| **s != Strategy::InputOrder) {
        assert_eq!(
            order_text(&assign_reading_order(data.clone(), *st)),
            "C,B,A",
            "{st:?} 在 y 倒序的单栏上必须重排（否则说明策略没生效）"
        );
    }
}

/// **`GroupBased` 是唯一真正做双栏的策略**。
#[test]
fn d376_group_based_is_the_only_real_two_column_strategy() {
    assert_eq!(
        order_text(&assign_reading_order(two_column(), Strategy::GroupBased)),
        "L1,L2,L3,R1,R2,R3",
        "`GroupBased`（按 x 重叠聚类）应产出真正的双栏顺序"
    );
}

/// **`XyCut` 是「简化版」**（`mod.rs:210` 注释原文）：
/// 只按 y 再 x 排，**不**先按 x 分栏。
#[test]
fn d376_xy_cut_is_the_simplified_variant() {
    assert_eq!(
        order_text(&assign_reading_order(two_column(), Strategy::XyCut)),
        "L1,R1,L2,R2,L3,R3",
        "`XyCut` 是简化版（按 y 再 x），双栏上不切栏是**设计**（mod.rs:210 注释明写）"
    );
}

/// **`XyCutPlusPlus` 在等宽双栏上不切栏** —— 密度比决策的**固有结果**。
///
/// `compute_prefer_horizontal`（`xy_cut.rs:91`）按
/// `x_density > BETA * y_density` 选首次切分方向。等宽双栏必然
/// `sum_w/x_span > sum_h/y_span` ⇒ 选横向优先 ⇒ 退化为按 y 排。
///
/// 本条把这个**行为**钉住，并显式记录**它不是缺陷**。
#[test]
fn d376_xy_cut_plus_plus_picks_horizontal_first_on_equal_columns() {
    assert_eq!(
        order_text(&assign_reading_order(two_column(), Strategy::XyCutPlusPlus)),
        "L1,R1,L2,R2,L3,R3",
        "等宽双栏下 `compute_prefer_horizontal` 选横向优先 ⇒ 退化为按 y 排（算法权衡，非缺陷）"
    );
}

/// **无 bbox ⇒ 保持输入顺序**（所有策略）。
#[test]
fn d376_missing_bbox_keeps_input_order() {
    let data = vec![plain("A"), plain("B"), plain("C")];
    for st in ALL {
        assert_eq!(
            order_text(&assign_reading_order(data.clone(), st)),
            "A,B,C",
            "{st:?}：无 bbox 时应保持输入顺序"
        );
    }
}

/// **空输入 / 单块**不得 panic。
#[test]
fn d376_empty_and_single_are_safe() {
    for st in ALL {
        assert_eq!(
            assign_reading_order(vec![], st).len(),
            0,
            "{st:?}：空输入应得空结果"
        );
        assert_eq!(
            order_text(&assign_reading_order(vec![plain("X")], st)),
            "X",
            "{st:?}：单块应原样返回"
        );
    }
}

/// **`Strategy::from_str` 的 6 组别名 + 兜底**（15 个输入全对）。
#[test]
fn d376_strategy_aliases_and_fallback() {
    for (s, expected) in [
        ("input", Strategy::InputOrder),
        ("input_order", Strategy::InputOrder),
        ("top_to_bottom", Strategy::TopToBottom),
        ("ttb", Strategy::TopToBottom),
        ("gap_tree", Strategy::GapTree),
        ("gap", Strategy::GapTree),
        ("xy_cut", Strategy::XyCut),
        ("xy", Strategy::XyCut),
        ("group_based", Strategy::GroupBased),
        ("group", Strategy::GroupBased),
        ("xy_cut_plus_plus", Strategy::XyCutPlusPlus),
        ("xy++", Strategy::XyCutPlusPlus),
        ("xy_cut_pp", Strategy::XyCutPlusPlus),
        // 兜底
        ("BOGUS", Strategy::TopToBottom),
        ("", Strategy::TopToBottom),
    ] {
        assert_eq!(
            Strategy::from_str(s),
            expected,
            "from_str({s:?}) 应得 {expected:?}"
        );
    }
}

/// **`reading_order_idx` 必须**从 0 连续编号**。
///
/// 判据不能只比顺序文本 —— 还要确认下标是**连续**的
/// （`0,1,2,…` 而非 `0,2,4`），否则「按 idx 排序」会掩盖编号错误。
#[test]
fn d376_reading_order_idx_is_contiguous_from_zero() {
    for st in ALL {
        let r = assign_reading_order(two_column(), st);
        let mut idxs: Vec<i64> = r.iter().filter_map(idx_of).collect();
        idxs.sort_unstable();
        assert_eq!(
            idxs,
            vec![0, 1, 2, 3, 4, 5],
            "{st:?}：`reading_order_idx` 必须 0..5 连续; 实得 {idxs:?}"
        );
    }
}
