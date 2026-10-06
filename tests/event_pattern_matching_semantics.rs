//! v0.104.6 D281：事件通配符 `X.*` **也匹配裸 `X`** —— 语义**未文档化、未测试**
//!
//! ## 本文件是**语义钉子**，不是「正确性断言」
//!
//! `matches("outer", "outer.*")` 返回 **true**。这个行为此前既没写进文档、
//! 也没有任何测试覆盖。本轮**不改它**（改它属产品策略决定），
//! 只把它**写明并钉住**，免得将来无意中漂移、或被误当成 bug「顺手修掉」。
//!
//! 如果将来决定改成「`X.*` 只匹配 `X` 之下」，本文件会**变红** ——
//! 那正是改动的信号，不是失败。
//!
//! ## 两条实现**一致**（不是「两套实现打架」）
//!
//! | 路径 | 对 `emit("outer")` / `matches("outer", "outer.*")` |
//! |---|---|
//! | 谓词 `matches()` | true（`pa.len() <= ev.len()+1` 的 `+1` 余量） |
//! | 索引 `emit()` | `classify_pattern("outer.*")` → 存 `prefix["outer"]`；`emit` 的 `for i in 0..parts.len()` 正好查到该键 ⇒ 触发 |
//!
//! ## 影响面已核：目前**无可观察后果**
//!
//! - `sandbox::check_builtin`（`event::matches` 的生产消费者之一）是**查询型**
//!   builtin：`sandbox.check_builtin(name)` 的 `name` 由**用户传**，
//!   执行路径上**没有任何调用者**（全仓只有该 builtin 与单测）。
//!   ⇒ 「安全绕过」的框架不成立，它只是**查询结果偏宽**。
//! - 事件总线的消费者都是内部代码，且没有代码 emit 裸 `X` 形态的事件。
//!
//! ⚠ 若将来把沙箱 policy 接到**执行闸门**上，本钉子描述的「偏宽」就会变成
//! 真实的越权面，届时**必须**重新评估这条语义。

use mora::event::{EventBus, matches};
use mora::value::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// `X.*` 匹配裸 `X` —— 钉住既有语义。
#[test]
fn d281_prefix_pattern_also_matches_the_bare_name() {
    assert!(
        matches("outer", "outer.*"),
        "既有语义：`X.*` 也匹配裸 `X`（`+1` 余量所致）"
    );
    // 与「之下」的常规形态并存
    assert!(matches("outer.gui", "outer.*"));
    assert!(matches("outer.gui.item.removed", "outer.*"));
}

/// 对照：**不**同名的裸名不得被匹配（钉住 `+1` 余量没有放宽过头）。
#[test]
fn d281_prefix_pattern_does_not_match_other_top_level_names() {
    assert!(!matches("other", "outer.*"));
    assert!(!matches("other.gui", "outer.*"));
    assert!(!matches("outerx", "outer.*"));
}

/// 钉住前缀的**层级**语义：更深的 `X.Y.*` 同样匹配裸 `X.Y`，但不匹配 `X`。
#[test]
fn d281_deeper_prefix_has_the_same_shape() {
    assert!(matches("a.b", "a.b.*"));
    assert!(!matches("a", "a.b.*"));
    assert!(matches("a.b.c.d", "a.b.*"));
}

/// **两条实现必须给出一致答案** —— 谓词路径与索引路径。
///
/// 这是本文件最有价值的一条：若将来只改其中一处（例如为了「修」裸名匹配
/// 而动了 `matches` 却忘了索引路径，或反之），本条会立刻变红。
#[test]
fn d281_predicate_and_index_paths_agree() {
    let cases: &[(&str, &str)] = &[
        ("outer", "outer.*"),
        ("outer.gui", "outer.*"),
        ("outer.gui.item", "outer.gui.*"),
        ("other.gui", "outer.*"),
        ("a.b", "a.b.*"),
        ("a", "a.b.*"),
        ("a.b.c.d", "a.b.*"),
        ("anything", "*"),
        ("a", "*"),
        ("a.b.c", "a.b.c"),
    ];
    for (event, pattern) in cases {
        let bus = EventBus::new();
        let fired = Arc::new(AtomicUsize::new(0));
        let f = fired.clone();
        bus.on(
            pattern,
            Arc::new(move |_e: &str, _p: &Value| {
                f.fetch_add(1, Ordering::SeqCst);
            }),
        );
        bus.emit(event, &Value::Nil);
        let by_index = fired.load(Ordering::SeqCst) > 0;
        let by_predicate = matches(event, pattern);
        assert_eq!(
            by_index, by_predicate,
            "事件 {event:?} × 模式 {pattern:?}：索引路径={by_index}，\
             谓词路径={by_predicate} —— 两条实现必须一致"
        );
    }
}

/// 对照：catch-all `*` 匹配一切（既有行为，防回归）。
#[test]
fn d281_catchall_still_matches_everything() {
    assert!(matches("anything", "*"));
    assert!(matches("", "*"));
}
