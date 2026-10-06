//! v0.104.6 D393 —— `value/display.rs`：**atom 内容用 `Debug` 渲染**，
//! 且**完全绕过深度限制**（否定轮 + 一项待裁决 + 一处注释修正）
//!
//! ## ① atom 的内容用 `Debug` 而非 `Display` —— 与全语言不一致
//!
//! 实测（真实 CLI）：
//!
//! ```text
//! print([1, 2])        → [1.0, 2.0]
//! print(atom([1, 2]))  → <atom List([Float(1.0), Float(2.0)])>
//! print(atom("hi"))    → <atom String("hi")>
//! ```
//!
//! 同一个列表，**装进 atom 就变成 Rust 枚举语法**（`List` / `Float` / `String`）。
//! `display.rs:131` 与 `:201` 都用 `{:?}`。
//!
//! ## ② 深度限制**根本没覆盖 Atom** —— 文档声称的防护不存在
//!
//! `fmt_inner` 的 doc comment 写着：
//! 「stops at MAX_DEPTH (default 16) to prevent stack overflow on
//! recursive/cyclic structures (**e.g. Atom containing self**)」。
//!
//! 但 `Atom` 分支走 `{:?}`，**根本不经过 `fmt_inner` 的深度检查** ——
//! 递归由派生 `Debug` 完成，`MAX_DEPTH` 对它**无效**。
//!
//! ## ③ 那为什么自引用 atom **不会**崩？—— 靠 parking_lot 自己的 Debug
//!
//! 实测：`swap(a, fn(x) => a end)` 把 atom 指向自身后 `print(a)` 得到
//!
//! ```text
//! after=<atom Atom(Mutex { data: <locked> })>
//! ```
//!
//! **`<locked>`** 是 parking_lot 的 `Mutex: Debug` 在 `try_lock()` 失败时的输出
//! ⇒ 重入同一把锁时不再下钻，**意外**形成环断开器。
//!
//! ⚠ **牙齿验证推翻了我的第一版归因**：我原以为断开环的是
//! `display.rs:130` 的 `let v = arc.lock();`。把该处改成
//! `arc.lock().clone()`（**解锁后**再格式化）后，自引用 atom 判据
//! **照样全绿** —— 真正取锁的是 parking_lot 的 Debug 自己。
//!
//! 可佐证：非环的 `atom(0)` 会显示 `Float(0.0)`（说明锁可取时它渲染数据），
//! 只有重入取不到才退化成 `<locked>`。
//!
//! ⇒ **深度限制覆盖 List/Dict，环安全覆盖 Atom**，二者机制不同，
//! 且环安全依赖的是**第三方 crate 的 Debug 实现细节**。
//! 判据把「自引用 atom 必须终止」钉死 —— 它才是真正要守的东西。
//!
//! ## ④ 为什么**不**直接把它改成 `Display`
//!
//! ⚠ 若把 `{:?}` 换成 `{}`（`Display`）：第 130 行 `arc.lock()` 持有的锁，
//! 会**在同一把 `parking_lot::Mutex` 上重入**（自引用 atom 的 Display 臂
//! 再次 `arc.lock()`）⇒ **死锁**。
//!
//! ⇒ 这是**设计取舍**（一致性 vs 环安全），按 D382 先例**只报告不擅动**。
//! 本判据把「**安全**」这条现有保证钉死：任何改动都必须先解决环安全。
//!
//! （死锁路径是**代码层推断**，依据 `display.rs:130` 持锁 + 同臂重入；
//!   **刻意未实测** —— 实测会让测试进程永久挂死。）

use std::sync::Arc;

use mora::value::Value;
use mora::value::list::List;

/// 自引用 atom：内容指向自身。
///
/// 用法：`let arc = Arc::new(Mutex::new(Value::Nil));`
/// 然后 `*arc.lock() = Value::Atom(arc.clone())`。
fn self_referential_atom() -> Value {
    let arc: Arc<parking_lot::Mutex<Value>> = Arc::new(parking_lot::Mutex::new(Value::Nil));
    *arc.lock() = Value::Atom(arc.clone());
    Value::Atom(arc)
}

/// **自引用 atom 必须终止**（不挂死、不栈溢出），且带 `<locked>` 占位。
///
/// 这是本文件最核心的一条：它是 atom 唯一的**环安全**保证。
#[test]
fn d393_self_referential_atom_terminates_with_locked_placeholder() {
    let s = format!("{}", self_referential_atom());
    assert!(
        s.contains("<locked>"),
        "自引用 atom 应显示 `<locked>` 占位（锁守卫的效果）; 实得: {s}"
    );
    assert!(s.starts_with("<atom"), "实得: {s}");
}

/// **环内嵌在列表里**也必须终止（`atom → [atom]` 闭环）。
#[test]
fn d393_cyclic_atom_through_list_terminates() {
    let arc: Arc<parking_lot::Mutex<Value>> = Arc::new(parking_lot::Mutex::new(Value::Nil));
    // 先让 atom 指向自身，再造一个含该 atom 的列表，最后让 atom 指向该列表
    // ⇒ atom → list → atom（同一条 Arc）构成真环。
    *arc.lock() = Value::Atom(arc.clone());
    let lst = Value::List(List::from(vec![Value::Atom(arc.clone())]));
    *arc.lock() = lst;
    let s = format!("{}", Value::Atom(arc));
    assert!(s.contains("<locked>"), "环应被锁守卫断开; 实得: {s}");
}

/// **现状钉：atom 内容走 `Debug`**（Rust 枚举语法）。
///
/// 若将来改成 `Display`（与全语言一致），本条会红 —— 那需要**先**解决
/// ④ 的死锁问题（环安全），不是顺手能改的。
#[test]
fn d393_atom_body_uses_debug_syntax_today() {
    let cases: &[(&str, Value, &[&str])] = &[
        (
            "列表",
            Value::Atom(Arc::new(parking_lot::Mutex::new(Value::List(List::from(
                vec![Value::Float(1.0), Value::Float(2.0)],
            ))))),
            &["List", "Float"],
        ),
        (
            "字符串",
            Value::Atom(Arc::new(parking_lot::Mutex::new(Value::String(
                "hi".into(),
            )))),
            &["String"],
        ),
        (
            "浮点",
            Value::Atom(Arc::new(parking_lot::Mutex::new(Value::Float(42.0)))),
            &["42"],
        ),
    ];
    for (why, v, expect) in cases {
        let s = format!("{}", v);
        for e in expect.iter() {
            assert!(
                s.contains(e),
                "{why}：atom 内容应含 `{e}`（Debug 形态）; 实得: {s}"
            );
        }
    }
}

/// **对照**：非 atom 的同一值走 `Display`（无 `List`/`Float` 枚举名）。
#[test]
fn d393_non_atom_values_use_display_not_debug() {
    let lst = Value::List(List::from(vec![Value::Float(1.0), Value::Float(2.0)]));
    let s = format!("{}", lst);
    assert_eq!(s, "[1.0, 2.0]", "裸列表应走 Display 形态");
    assert!(
        !s.contains("Float"),
        "Display 不应带 Rust 枚举名; 实得: {s}"
    );
}

/// **深度限制确实覆盖嵌套列表**（与 atom 形成对照）。
///
/// 本条证明 `MAX_DEPTH` 是**有效**的 —— 进一步说明 atom 绕过它属
/// 遗漏而非设计。
#[test]
fn d393_depth_limit_applies_to_nested_lists() {
    // 手工造 25 层嵌套（> DISPLAY_MAX_DEPTH = 16）
    let mut v = Value::Float(1.0);
    for _ in 0..25 {
        v = Value::List(List::from(vec![v]));
    }
    let s = format!("{}", v);
    assert!(
        s.contains('…'),
        "25 层嵌套应被深度限制截断（出现 `…`）; 实得: {s}"
    );
    assert!(
        s.len() < 200,
        "截断后长度应远小于未截断; 实得 {} 字节",
        s.len()
    );
}

/// **Float 显示：`{:.1}` 给的是 f64 的**精确值**，不是精度损失**（否定结果）。
///
/// 首版怀疑 `{:.1}` 会把大整数 float 截断成短形式。实测否掉：
/// `1234567890123456789.0` → `1234567890123456768.0`，
/// 那是该 f64 的**精确整数值**（`{}` 的最短往返反而会给 `…800`）。
#[test]
// `3.14` 触发 `clippy::approx_constant`（它近似 π）—— 但这里**刻意**要用
// 一个「有小数部分」的 float 来验证 `{:.1}` 分支的 else 路径，
// 换成别的值会改变被测性质。故按测试意图豁免。
#[allow(clippy::approx_constant)]
fn d393_float_display_gives_exact_value_for_large_integrals() {
    let s = format!("{}", Value::Float(1234567890123456789.0));
    assert_eq!(
        s, "1234567890123456768.0",
        "大整数 float 应显示其 f64 精确值"
    );
    // 常规值
    assert_eq!(format!("{}", Value::Float(1000000.0)), "1000000.0");
    assert_eq!(format!("{}", Value::Float(3.14)), "3.14");
    // 非有限值不得 panic（v0.36 约束）
    assert_eq!(format!("{}", Value::Float(f64::NAN)), "nan");
    assert_eq!(format!("{}", Value::Float(f64::INFINITY)), "inf");
    assert_eq!(format!("{}", Value::Float(f64::NEG_INFINITY)), "-inf");
}
