//! v0.104.6 D243 — XY-Cut++ 的**平移不变性**（translation invariance）。
//!
//! ## 缺陷
//!
//! `project_to_axis` 用 `坐标 as usize` 把 bbox 坐标映射到直方图下标，
//! `split_projection` 把段边界以**下标**形式回传；而
//! `recursive_xy_cut` 的段成员判定却把该下标直接当**原始 f64 中心坐标**
//! 与 `center_x()` / `center_y()` 比较。**两者不在同一坐标系。**
//!
//! Rust 的 `as usize` 对负数是**饱和**转换（`-5.0f64 as usize == 0`，实测）。
//! 负坐标（带 offset 的坐标系、以页面中心为原点的坐标系都很常见）因此
//! 整块塌进 0 号下标，而 `center_y()` 仍是负数 —— 永不相等 ⇒ 该块
//! 被段过滤**静默丢弃**，零诊断。
//!
//! ## 实测后果
//!
//! 4 块单列，相对几何完全相同，只改坐标系原点：
//!
//! ```text
//! y0=0     -> [title, p1, p2, footer]   正确
//! y0=-100  -> [p2, footer]              4 块输入只输出 2 块（静默丢数据）
//! y0=-300  -> [title, p1, p2, footer]   碰巧对
//! ```
//!
//! `y0=-300` 的「碰巧对」值得单独记住：此时所有下标都塌成 0、直方图全零
//! ⇒ 检不出 gap ⇒ 走「两轴都无法切分」的旁路，而那条旁路**不按段过滤**。
//! 它是**巧合正确**。只测这一个点会得到「否定结论」—— 这正是本判据
//! 必须**穷举多个平移量**（而不是抽一个负数）的原因。

use mora::document::reading_order::{Strategy, assign_reading_order};
use mora::value::Value;
use std::collections::HashMap;

fn block(text: &str, x: f64, y: f64, w: f64, h: f64) -> Value {
    let mut d = HashMap::new();
    d.insert("text".to_string(), Value::String(text.to_string()));
    let mut bb = HashMap::new();
    bb.insert("x".to_string(), Value::Float(x));
    bb.insert("y".to_string(), Value::Float(y));
    bb.insert("w".to_string(), Value::Float(w));
    bb.insert("h".to_string(), Value::Float(h));
    d.insert("bbox".to_string(), Value::Dict(bb));
    Value::Dict(d)
}

fn texts(out: &[Value]) -> Vec<String> {
    out.iter()
        .map(|v| match v {
            Value::Dict(d) => match d.get("text") {
                Some(Value::String(s)) => s.clone(),
                _ => "?".to_string(),
            },
            _ => "?".to_string(),
        })
        .collect()
}

/// 单列 4 块：title / p1 / p2 / footer，y0 是整块的纵向平移量。
fn single_column(y0: f64) -> Vec<Value> {
    vec![
        block("title", 0.0, y0, 200.0, 30.0),
        block("p1", 0.0, y0 + 40.0, 200.0, 100.0),
        block("p2", 0.0, y0 + 150.0, 200.0, 100.0),
        block("footer", 0.0, y0 + 260.0, 200.0, 20.0),
    ]
}

/// 双栏 4 块：左栏上下 + 右栏上下。
fn two_column(dx: f64, dy: f64) -> Vec<Value> {
    vec![
        block("L1", dx, dy, 90.0, 100.0),
        block("L2", dx, dy + 120.0, 90.0, 100.0),
        block("R1", dx + 110.0, dy, 90.0, 100.0),
        block("R2", dx + 110.0, dy + 120.0, 90.0, 100.0),
    ]
}

/// 一个横跨全宽的 cross-layout 块（页眉）+ 单列正文。
fn with_cross_layout(dx: f64, dy: f64) -> Vec<Value> {
    vec![
        block("header", dx, dy, 400.0, 20.0),
        block("p1", dx, dy + 40.0, 200.0, 100.0),
        block("p2", dx, dy + 150.0, 200.0, 100.0),
    ]
}

/// 判据主体：同一组相对几何，穷举平移量，阅读顺序必须完全一致。
fn assert_translation_invariant(
    name: &str,
    make: impl Fn(f64, f64) -> Vec<Value>,
    expected: &[&str],
) {
    // 平移量刻意覆盖：零、纯正、纯负、跨零、极端负。
    // 少任何一个都可能落进「巧合正确」的旁路（见模块注释 y0=-300）。
    const OFFSETS: &[(f64, f64)] = &[
        (0.0, 0.0),
        (7.0, 13.0),
        (0.0, 1000.0),
        (0.0, -100.0),
        (0.0, -300.0),
        (0.0, -1000.0),
        (-500.0, -100.0),
        (37.0, -7.0),
    ];

    let baseline = {
        let out = assign_reading_order(make(0.0, 0.0), Strategy::XyCutPlusPlus);
        let t = texts(&out);
        assert_eq!(t, expected, "{name}: 基线（未平移）顺序本身就错");
        // 守恒：基线不得丢块。
        assert_eq!(t.len(), expected.len(), "{name}: 基线丢块");
        t
    };

    for (dx, dy) in OFFSETS.iter().skip(1) {
        let out = assign_reading_order(make(*dx, *dy), Strategy::XyCutPlusPlus);
        let t = texts(&out);
        // ① 平移不变：坐标系原点不应影响阅读顺序。
        assert_eq!(
            t, baseline,
            "{name}: 平移 ({dx}, {dy}) 改变了阅读顺序\n  期望 {baseline:?}\n  实得 {t:?}"
        );
        // ② 守恒：无论怎么平移，块都不能消失。
        assert_eq!(
            t.len(),
            expected.len(),
            "{name}: 平移 ({dx}, {dy}) 静默丢块 —— 输入 {} 块，输出 {} 块\n  {t:?}",
            expected.len(),
            t.len()
        );
    }
}

#[test]
fn xy_cut_pp_translation_invariant_single_column() {
    assert_translation_invariant(
        "single_column",
        |_, dy| single_column(dy),
        &["title", "p1", "p2", "footer"],
    );
}

#[test]
fn xy_cut_pp_translation_invariant_two_column() {
    // 期望顺序是**行优先**：L1 与 R1 处在同一水平行（y 都是 0），
    // XY-Cut 先按 y 切出行段 `[0,100)` / `[120,220)`，段内再按 x 从左到右。
    //
    // 这里曾把期望写成「列优先」的 `[L1, L2, R1, R2]` 而红 —— 那是**我的期望
    // 写错了**，不是产品缺陷：XY-Cut++ 的语义本就是逐行横扫。
    // 判据的第一条断言是「基线顺序本身正确」，正是为了让这类错误在
    // 第一时间暴露成「期望错了」而不是「平移不变性被破坏」。
    assert_translation_invariant("two_column", two_column, &["L1", "R1", "L2", "R2"]);
}

#[test]
fn xy_cut_pp_translation_invariant_cross_layout() {
    assert_translation_invariant("cross_layout", with_cross_layout, &["header", "p1", "p2"]);
}

/// 对照组：把缺陷成因钉在**语言事实**上，而不是钉在产品代码上。
///
/// 修法收敛到了 `axis_origin`，但**判据不能钉在 `axis_origin` 上**
/// （D235 教训：判据钉在错误的代码上 ⇒ 回退时牙齿不响）。
/// 这里固定的是「`as usize` 对负数饱和」这条语言事实 —— 它是缺陷的前提，
/// 且完全独立于本仓任何实现。
#[test]
fn d243_control_group_negative_as_usize_saturates_to_zero() {
    // 若这条断言将来不成立，说明前提变了，D243 的判据需要重新审视。
    assert_eq!((-5.0f64) as usize, 0, "负 f64 as usize 应饱和为 0");
    assert_eq!((-0.5f64) as usize, 0, "负小数 as usize 应饱和为 0");
    assert_eq!((-100.0f64) as usize, 0, "大负数 as usize 应饱和为 0");
    // 正坐标不受影响 —— 这是对照组的一半：说明缺陷只在负侧。
    assert_eq!(0.0f64 as usize, 0);
    assert_eq!(37.0f64 as usize, 37, "正 f64 as usize 应向零取整");
    assert_eq!(37.9f64 as usize, 37);
}

/// 对照组：修复后，投影下标与坐标系原点**无关**。
///
/// 这一条是「修复为什么有效」的直接表述：平移后坐标恒非负，
/// `as usize` 不再饱和，于是下标就是纯粹的几何量。
#[test]
fn d243_control_group_shifted_coord_is_never_negative() {
    // 任意平移下，平移后的坐标恒非负（`axis_origin` 从 0.0 起步 fold min）。
    for y0 in [0.0, -1.0, -100.0, -300.0, -1e6, 1e6] {
        for (y, h) in [(0.0, 30.0), (40.0, 100.0), (150.0, 100.0), (260.0, 20.0)] {
            let lo = y + y0;
            let hi = y + h + y0;
            // 复刻 axis_origin 的语义：该轴所有端点的 min，下限 0。
            let origin = [lo, hi, 0.0].into_iter().fold(0.0f64, f64::min);
            assert!(lo - origin >= 0.0, "平移 {y0} 后起点 {lo} 仍为负");
            assert!(hi - origin >= 0.0, "平移 {y0} 后终点 {hi} 仍为负");
        }
    }
}
