//! D241 判据：`reading_order` 的排序比较器必须满足**全序**。
//!
//! ## 缺陷背景
//!
//! `Strategy::TopToBottom` 的比较器是**混合**的：
//! 垂直**不重叠**时比 `y`（谁在上），**重叠**时比 `x`（谁靠左）。
//! 这不是全序 —— 可构造 `cmp(A,B)` 与 `cmp(B,C)` 由 `x` 决定、而
//! `cmp(A,C)` 由 `y` 决定的三元组，链式推论与直接比较**互相矛盾**。
//!
//! Rust 的 `sort_by` 在比较器非全序时**静默**产生错误结果
//! （官方文档："may panic or return nonsense"），不 panic、零诊断。
//!
//! ## 实测（穷举全部输入排列，bbox 用 `{x,y,w,h}` **dict**）
//!
//! | 块数 | 修前不同结果数 | 修后 |
//! |---|---|---|
//! | 3 | 1（该语料不触发） | 1 |
//! | **4** | **2**（`A,B,D,C` ×12 / `A,D,B,C` ×12） | **1**（`A,B,D,C` ×24） |
//!
//! 4 块时输出**依赖输入排列** —— 这就是「非全序」的可观测后果。
//!
//! ## 判据形态：**排列穷举不变式**
//!
//! 同一组 blocks 的**所有**输入排列必须产出**同一个**输出序列。
//! 这不依赖「正确顺序是什么」的先验，只要求「顺序是输入的函数之外的东西」——
//! 恰是全序的定义。
//!
//! ⚠ 4 块语料才触发：3 块语料修前也正常，**只测 3 块会漏掉这个缺陷**。

use mora::document::reading_order::{BBox, Strategy, assign_reading_order};
use mora::value::Value;
use std::collections::HashMap;

/// ⚠ `bbox` 必须是 **dict** `{x,y,w,h}` —— `BBox::from_value` 只认 dict
/// （`Value::List` 会让所有块被判为「无 bbox」⇒ 恒 `Ordering::Equal` ⇒
///  输出恒等于输入，让本判据**恒绿**、测不到任何东西）。
fn block(text: &str, bb: BBox) -> Value {
    let mut bbm = HashMap::new();
    bbm.insert("x".into(), Value::Float(bb.x));
    bbm.insert("y".into(), Value::Float(bb.y));
    bbm.insert("w".into(), Value::Float(bb.w));
    bbm.insert("h".into(), Value::Float(bb.h));
    let mut d = HashMap::new();
    d.insert("text".into(), Value::String(text.into()));
    d.insert("bbox".into(), Value::Dict(bbm));
    Value::Dict(d)
}

fn order_texts(blocks: Vec<Value>, strategy: Strategy) -> String {
    assign_reading_order(blocks, strategy)
        .iter()
        .map(|b| match b {
            Value::Dict(d) => match d.get("text") {
                Some(Value::String(s)) => s.clone(),
                _ => "?".into(),
            },
            _ => "?".into(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// 穷举 n 个元素的全部排列
fn perms<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
    fn go<T: Clone>(rest: &mut Vec<T>, acc: &mut Vec<T>, out: &mut Vec<Vec<T>>) {
        if rest.is_empty() {
            out.push(acc.clone());
            return;
        }
        for i in 0..rest.len() {
            let x = rest.remove(i);
            acc.push(x.clone());
            go(rest, acc, out);
            acc.pop();
            rest.insert(i, x);
        }
    }
    let mut rest = items.to_vec();
    let mut acc: Vec<T> = Vec::new();
    let mut out: Vec<Vec<T>> = Vec::new();
    go(&mut rest, &mut acc, &mut out);
    out
}

/// D241 主判据：4 块语料，穷举 24 种输入排列 ⇒ 必须只有 **1 种**输出。
///
/// 语料形状（4 块才触发，3 块不触发 —— 这是 D241 的取证要点）：
/// - `A`: y 0..10,  x=0    与 C 不重叠
/// - `B`: y 5..15,  x=100  与 A 重叠（按 x：A < B）
/// - `C`: y 20..30, x=0    与 B 重叠（按 x：C < B），与 A 不重叠（y）
/// - `D`: y 8..18,  x=100  与 A/B/C 的关系让混合比较器产生环
#[test]
fn d241_top_to_bottom_is_order_independent() {
    let a = block(
        "A",
        BBox {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        },
    );
    let b = block(
        "B",
        BBox {
            x: 100.0,
            y: 5.0,
            w: 10.0,
            h: 10.0,
        },
    );
    let c = block(
        "C",
        BBox {
            x: 0.0,
            y: 20.0,
            w: 10.0,
            h: 10.0,
        },
    );
    let d = block(
        "D",
        BBox {
            x: 100.0,
            y: 8.0,
            w: 10.0,
            h: 10.0,
        },
    );
    let base = vec![a, b, c, d];

    let mut outcomes: std::collections::BTreeMap<String, usize> = Default::default();
    for p in perms(&base.clone()) {
        *outcomes
            .entry(order_texts(p, Strategy::TopToBottom))
            .or_default() += 1;
    }

    assert_eq!(
        outcomes.len(),
        1,
        "D241: `TopToBottom` 的输出**依赖输入排列** —— 比较器不是全序。\n\
         4 块语料穷举 24 种输入排列，得到 {} 种不同结果：\n  {}\n\
         修前比较器混用两个 key（垂直不重叠比 y、重叠比 x），\
         Rust `sort_by` 在非全序比较器下**静默**产出依赖输入的结果。",
        outcomes.len(),
        outcomes
            .iter()
            .map(|(k, v)| format!("{k} x{v}"))
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

/// D241 对照组：修法选的 `(y, x)` 全序本身也必须是自洽的 ——
/// 按 y 升序、y 相同则按 x 升序。
#[test]
fn d241_top_to_bottom_follows_y_then_x_order() {
    let blocks = vec![
        block(
            "mid-right",
            BBox {
                x: 100.0,
                y: 50.0,
                w: 10.0,
                h: 10.0,
            },
        ),
        block(
            "top-left",
            BBox {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
        ),
        block(
            "top-right",
            BBox {
                x: 100.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
        ),
        block(
            "top-left2",
            BBox {
                x: 50.0,
                y: 0.0,
                w: 10.0,
                h: 10.0,
            },
        ),
        block(
            "bottom",
            BBox {
                x: 0.0,
                y: 200.0,
                w: 10.0,
                h: 10.0,
            },
        ),
    ];
    assert_eq!(
        order_texts(blocks, Strategy::TopToBottom),
        "top-left,top-left2,top-right,mid-right,bottom",
        "D241: 应按 y 升序、同 y 按 x 升序"
    );
}

/// D241 对照组：其余策略本就用单一全序 key，本条确保它们**也**满足排列不变式
/// （防止将来有人把混合比较器引进它们）。
#[test]
fn d241_other_strategies_are_also_order_independent() {
    let a = block(
        "A",
        BBox {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        },
    );
    let b = block(
        "B",
        BBox {
            x: 100.0,
            y: 5.0,
            w: 10.0,
            h: 10.0,
        },
    );
    let c = block(
        "C",
        BBox {
            x: 0.0,
            y: 20.0,
            w: 10.0,
            h: 10.0,
        },
    );
    let d = block(
        "D",
        BBox {
            x: 100.0,
            y: 8.0,
            w: 10.0,
            h: 10.0,
        },
    );
    let base = vec![a, b, c, d];

    for strategy in [
        Strategy::GapTree,
        Strategy::XyCut,
        Strategy::GroupBased,
        Strategy::XyCutPlusPlus,
    ] {
        let mut outcomes: std::collections::BTreeMap<String, usize> = Default::default();
        for p in perms(&base.clone()) {
            *outcomes.entry(order_texts(p, strategy)).or_default() += 1;
        }
        assert_eq!(
            outcomes.len(),
            1,
            "D241: {strategy:?} 的输出也依赖输入排列。\n  {outcomes:?}"
        );
    }
}

/// D241 防「判据空转」：确认语料**确实**带上了 bbox。
///
/// 若 `BBox::from_value` 认不出这些 dict（比如将来改了格式），
/// 所有块都变「无 bbox」⇒ 恒 `Equal` ⇒ 排列不变式**恒成立**、
/// 主判据**恒绿**却什么都没测。本条把那个前提显式钉住。
#[test]
fn d241_test_corpus_actually_has_usable_bboxes() {
    use mora::document::reading_order::BBox as BB;
    let v = block(
        "A",
        BB {
            x: 1.0,
            y: 2.0,
            w: 3.0,
            h: 4.0,
        },
    );
    assert!(
        BB::from_value(&v).is_some(),
        "D241: 测试语料的 bbox 必须能被 BBox::from_value 识别 —— \
         否则所有块都是「无 bbox」，排列不变式会**恒绿**而测不到任何东西"
    );
}
