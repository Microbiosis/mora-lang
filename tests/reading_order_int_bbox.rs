//! D230 判据：`reading_order` 的 `BBox::from_value` 必须同时接受
//! `Int` 与 `Float` 两种数字表示。
//!
//! ## 缺陷背景
//!
//! `BBox::from_value` 此前只匹配 `Value::Float`。而本仓的数字有**两个**来源：
//! dict 字面量给 `Float`（D98），`json.parse` 给 `Int`（D129 实测）。
//! ⇒ 任何来自 `json.parse` 的 bbox 被**整个丢弃**（返回 `None`），
//! 该块被当作「无 bbox」，排序时走 `Ordering::Equal` 保持输入序。
//!
//! ## 后果（实测，非推断）
//!
//! 三块同列、**输入顺序与几何顺序相反**（两栏扫描常见）：
//!
//! | 中间块的 bbox | 结果顺序 |
//! |---|---|
//! | `Float` | `A_top, C, B_bottom` ✓ |
//! | `Int` | `B_bottom, C, A_top` ✗ **完全颠倒** |
//!
//! 零诊断。阅读顺序错 = 内容顺序错。
//!
//! ## 判据形态：**两条路径等价**，不写死具体顺序
//!
//! 写死「顺序必须是 A,C,B」在别的策略/几何下容易脆；
//! 「Int 版与 Float 版产出完全相同」是更强也更稳的不变式 ——
//! 它正是缺陷的本质（表示差异不得影响语义）。

use mora::document::reading_order::{BBox, Strategy, assign_reading_order};
use mora::value::Value;
use std::collections::HashMap;

fn float_bbox(x: f64, y: f64, w: f64, h: f64) -> Value {
    let mut d = HashMap::new();
    d.insert("x".into(), Value::Float(x));
    d.insert("y".into(), Value::Float(y));
    d.insert("w".into(), Value::Float(w));
    d.insert("h".into(), Value::Float(h));
    Value::Dict(d)
}

fn int_bbox(x: i64, y: i64, w: i64, h: i64) -> Value {
    let mut d = HashMap::new();
    d.insert("x".into(), Value::Int(x));
    d.insert("y".into(), Value::Int(y));
    d.insert("w".into(), Value::Int(w));
    d.insert("h".into(), Value::Int(h));
    Value::Dict(d)
}

fn block(text: &str, bbox: Option<Value>) -> Value {
    let mut d = HashMap::new();
    d.insert("text".into(), Value::String(text.into()));
    if let Some(b) = bbox {
        d.insert("bbox".into(), b);
    }
    Value::Dict(d)
}

fn order_texts(blocks: Vec<Value>, strategy: Strategy) -> Vec<String> {
    assign_reading_order(blocks, strategy)
        .iter()
        .map(|b| match b {
            Value::Dict(d) => match d.get("text") {
                Some(Value::String(s)) => s.clone(),
                _ => "?".into(),
            },
            _ => "?".into(),
        })
        .collect()
}

/// 判据 ①：`BBox::from_value` 对 `Int` 与 `Float` 产出**相同**的 `BBox`。
#[test]
fn d230_bbox_parses_int_and_float_identically() {
    let f = BBox::from_value(&float_bbox(10.0, 20.0, 30.0, 40.0));
    let i = BBox::from_value(&int_bbox(10, 20, 30, 40));
    assert!(
        f.is_some(),
        "Float bbox 必须能解析（这是修前就正常的对照组）"
    );
    assert!(
        i.is_some(),
        "D230: Int bbox 被静默丢弃（from_value 返回 None）—— \
         json.parse 产出的 bbox 全部失效"
    );
    assert_eq!(f, i, "D230: 同样的数值，Int 与 Float 必须解析成同一个 BBox");
}

/// 判据 ②：整条排序链上，Int 与 Float 产出**相同**的阅读顺序。
///
/// 覆盖 5 个策略 —— 缺陷不限于某一个策略（`bboxes` 在所有策略里共用
/// 同一个 `BBox::from_value`）。
#[test]
fn d230_int_and_float_bbox_give_same_reading_order() {
    for strategy in [
        Strategy::TopToBottom,
        Strategy::GapTree,
        Strategy::XyCut,
        Strategy::GroupBased,
        Strategy::XyCutPlusPlus,
    ] {
        // 输入顺序与几何顺序**相反** —— 两栏扫描的常见形状，
        // 且只有在这种情况下「被丢弃 → 保持输入序」才会显出差异。
        let float_blocks = vec![
            block("B_bottom", Some(float_bbox(0.0, 200.0, 100.0, 10.0))),
            block("C_mid", Some(float_bbox(0.0, 100.0, 100.0, 10.0))),
            block("A_top", Some(float_bbox(0.0, 0.0, 100.0, 10.0))),
        ];
        let int_blocks = vec![
            block("B_bottom", Some(int_bbox(0, 200, 100, 10))),
            block("C_mid", Some(int_bbox(0, 100, 100, 10))),
            block("A_top", Some(int_bbox(0, 0, 100, 10))),
        ];

        let got_float = order_texts(float_blocks, strategy);
        let got_int = order_texts(int_blocks, strategy);
        assert_eq!(
            got_float, got_int,
            "D230: strategy={strategy:?} 下 Int 与 Float 给出不同顺序 —— \
             Int bbox 被当作「无 bbox」，块保持输入序（阅读顺序错且零诊断）\n\
             Float: {got_float:?}\nInt:   {got_int:?}"
        );
    }
}

/// 判据 ③：对照组 —— 全 `Float` 时，**输入顺序与几何顺序相反**必须被纠正。
///
/// 防止判据 ② 因为「两条路径恰好都错」而一起绿。
#[test]
fn d230_float_only_control_group_is_actually_sorted() {
    let blocks = vec![
        block("B_bottom", Some(float_bbox(0.0, 200.0, 100.0, 10.0))),
        block("C_mid", Some(float_bbox(0.0, 100.0, 100.0, 10.0))),
        block("A_top", Some(float_bbox(0.0, 0.0, 100.0, 10.0))),
    ];
    let got = order_texts(blocks, Strategy::TopToBottom);
    assert_eq!(
        got,
        vec!["A_top", "C_mid", "B_bottom"],
        "对照组失效：全 Float 时也没排好 —— 判据 ② 的等价性会一起绿一起红"
    );
}

/// 判据 ④：非数字的 bbox 坐标仍应被拒绝（不是「来者不拒」）。
#[test]
fn d230_non_numeric_bbox_fields_are_still_rejected() {
    let mut d = HashMap::new();
    d.insert("x".into(), Value::String("10".into()));
    d.insert("y".into(), Value::Float(20.0));
    d.insert("w".into(), Value::Float(30.0));
    d.insert("h".into(), Value::Float(40.0));
    assert!(
        BBox::from_value(&Value::Dict(d)).is_none(),
        "D230: 字符串坐标必须仍然被拒绝（修法只应放宽 Int，不是放弃类型检查）"
    );
}
