//! v0.104.6 D373 —— `checkpoint::rewind` / `resume` 的**时间旅行语义**
//! （否定轮，无产品变更）
//!
//! `src/checkpoint/` 已有 3 个判据（负数守卫 / saver 一致性 / JSON 保真），
//! 但**恢复路径**（`rewind` / `resume`）此前无判据 —— 而它是
//! 「断点续跑」的正确性根基：错了会**静默恢复到错误的状态**。
//!
//! ## `rewind` 的 `>=` 与注释的 "before" 看似矛盾，实则一致
//!
//! ```rust
//! // mod.rs:481
//! /// This is the "time travel" primitive: after rewinding, the next
//! /// `resume` will load the last checkpoint **before** `before_step`.
//! pub fn rewind(saver, thread_id, before_step) {
//!     for id in ids {
//!         if let Some(cp) = saver.load(thread_id, Some(&id))?
//!             && cp.step >= before_step        // ← 删掉 step >= before_step
//!         { saver.delete(thread_id, &id)?; }
//!     }
//! }
//! ```
//!
//! 「last checkpoint **before** `before_step`」指的是**步骤 `before_step`
//! 执行之前的状态**（即 step < before_step 的最后一个），
//! 所以 `>=` 是**正确**的：删掉 `before_step` 及其之后，留下 `<` 的。
//!
//! 实测 `rewind(2)`（原 step 0..4）⇒ 剩 step 0、1，`resume` 得 **step 1** ✅
//!
//! ## `resume` 同 step 时**稳定取第一个**，不随机
//!
//! ```rust
//! // mod.rs:505
//! if let Some(cp) = … && latest.as_ref().is_none_or(|l| cp.step > l.step)
//! ```
//!
//! `>` 是**严格大于** ⇒ 同 step 时保留先遇到的那个。实测 3 条同 step
//! 的 checkpoint 连续 3 次 `resume` 都返回**同一个 id** ✅
//!
//! ## 边界：全部正确
//!
//! | 场景 | 实测 |
//! |---|---|
//! | 空 saver 的 `resume` | `None` ✅ |
//! | 空 saver 的 `rewind` | `Ok(())` ✅ |
//! | `resume` 不存在的 thread | `None` ✅ |
//! | `rewind(before=99)`（超过最大 step）| `Ok`，不删任何 ✅ |
//! | `rewind(before=0)` | 删**全部** ✅（符合「回到起点之前」）|

use std::collections::HashMap;
use std::sync::Arc;

use mora::checkpoint::{Checkpoint, CheckpointSaver, MemorySaver, resume, rewind};
use mora::value::Value;

fn mk(step: usize, tag: &str) -> Checkpoint {
    let mut cv = HashMap::new();
    cv.insert("tag".to_string(), Value::String(tag.to_string()));
    Checkpoint::new(
        "t".to_string(),
        step,
        cv,
        HashMap::new(),
        HashMap::new(),
        vec![],
    )
}

fn tag_of(cp: &Checkpoint) -> String {
    match cp.channel_values.get("tag") {
        Some(Value::String(s)) => s.clone(),
        other => format!("{other:?}"),
    }
}

fn setup(steps: &[usize]) -> Arc<MemorySaver> {
    let s = Arc::new(MemorySaver::new());
    for &st in steps {
        s.save("t", &mk(st, &format!("s{st}"))).unwrap();
    }
    s
}

/// **`rewind(before_step)` 删掉 `step >= before_step`，留下更早的**。
///
/// 注释说的 "before `before_step`" 指**该步骤执行之前的状态**，
/// 所以 `>=` 是正确的；本条把这个语义钉住。
#[test]
fn d373_rewind_removes_steps_at_or_after_before_step() {
    let saver = setup(&[0, 1, 2, 3, 4]);
    rewind(saver.as_ref(), "t", 2).expect("rewind");

    let mut left: Vec<usize> = saver
        .list("t")
        .unwrap()
        .iter()
        .filter_map(|id| saver.load("t", Some(id)).unwrap())
        .map(|c| c.step)
        .collect();
    left.sort_unstable();
    assert_eq!(
        left,
        vec![0, 1],
        "`rewind(2)` 应删掉 step>=2（2,3,4），留下 0、1; 实得 {left:?}"
    );
    assert_eq!(
        resume(saver.as_ref(), "t").unwrap().map(|c| c.step),
        Some(1),
        "`resume` 应得最后留下的那个（step 1）"
    );
}

/// **反向对照**：`rewind(0)` 删掉**全部**（回到起点之前）。
#[test]
fn d373_rewind_to_zero_removes_everything() {
    let saver = setup(&[0, 1]);
    rewind(saver.as_ref(), "t", 0).expect("rewind");
    assert!(saver.list("t").unwrap().is_empty(), "`rewind(0)` 应删全部");
    assert!(resume(saver.as_ref(), "t").unwrap().is_none());
}

/// **`before_step` 超过最大 step 时不删任何**。
#[test]
fn d373_rewind_beyond_max_step_is_a_noop() {
    let saver = setup(&[0, 1]);
    rewind(saver.as_ref(), "t", 99).expect("rewind");
    let mut left: Vec<usize> = saver
        .list("t")
        .unwrap()
        .iter()
        .filter_map(|id| saver.load("t", Some(id)).unwrap())
        .map(|c| c.step)
        .collect();
    left.sort_unstable();
    assert_eq!(left, vec![0, 1], "`rewind(99)` 不该删任何; 实得 {left:?}");
}

/// **`resume` 取最高 step**，与 `list` 的顺序无关。
#[test]
fn d373_resume_picks_the_highest_step() {
    // 逆序保存，验证不是「取最后一个」
    let saver = Arc::new(MemorySaver::new());
    for st in [4usize, 0, 3, 1, 2] {
        saver.save("t", &mk(st, &format!("s{st}"))).unwrap();
    }
    let r = resume(saver.as_ref(), "t")
        .unwrap()
        .expect("应有 checkpoint");
    assert_eq!(r.step, 4, "`resume` 应取最高 step 4; 实得 {}", r.step);
    assert_eq!(tag_of(&r), "s4");
}

/// **同 step 的多个 checkpoint：`resume` 稳定返回同一个**。
///
/// 实现用**严格大于**（`cp.step > l.step`）⇒ 同 step 保留先遇到的。
/// 本条钉住「**稳定**」这个性质 —— 若将来 list 顺序变成随机，
/// 这条会红，且那是**真缺陷**（恢复结果不确定）。
#[test]
fn d373_resume_is_stable_when_steps_tie() {
    let saver = Arc::new(MemorySaver::new());
    for t in ["a", "b", "c"] {
        saver.save("t", &mk(5, t)).unwrap();
    }
    let first = resume(saver.as_ref(), "t")
        .unwrap()
        .expect("应有 checkpoint");
    for _ in 0..4 {
        let again = resume(saver.as_ref(), "t")
            .unwrap()
            .expect("应有 checkpoint");
        assert_eq!(
            again.id, first.id,
            "同 step 时 `resume` 必须**稳定**返回同一个 id（恢复结果不确定 = 缺陷）"
        );
    }
}

/// **空 saver / 不存在的 thread**。
#[test]
fn d373_empty_and_missing_thread() {
    let saver = Arc::new(MemorySaver::new());
    assert!(
        resume(saver.as_ref(), "t").unwrap().is_none(),
        "空 saver → None"
    );
    assert!(
        resume(saver.as_ref(), "other").unwrap().is_none(),
        "不存在的 thread → None"
    );
    rewind(saver.as_ref(), "t", 0).expect("空 saver 的 rewind 不该报错");
    rewind(saver.as_ref(), "other", 3).expect("不存在的 thread 的 rewind 不该报错");
}

/// **thread 隔离**：`rewind` 只动指定的 thread。
///
/// ⚠ 首版断言写成「`t` 被清空」⇒ 假红 —— `rewind("t", 1)` 删的是
/// `step >= 1`，**留下 step 0**。清空要用 `before_step = 0`。
#[test]
fn d373_rewind_is_scoped_to_one_thread() {
    let saver = Arc::new(MemorySaver::new());
    for st in [0usize, 1, 2] {
        saver.save("t", &mk(st, &format!("t{st}"))).unwrap();
    }
    for st in [0usize, 1, 2] {
        let mut c = mk(st, &format!("o{st}"));
        c.thread_id = "other".to_string();
        saver.save("other", &c).unwrap();
    }
    rewind(saver.as_ref(), "t", 1).expect("rewind");

    let mut t_left: Vec<usize> = saver
        .list("t")
        .unwrap()
        .iter()
        .filter_map(|id| saver.load("t", Some(id)).unwrap())
        .map(|c| c.step)
        .collect();
    t_left.sort_unstable();
    assert_eq!(t_left, vec![0], "`rewind(\"t\", 1)` 应只留 step 0");
    assert_eq!(
        saver.list("other").unwrap().len(),
        3,
        "`other` thread 不该受影响"
    );

    // **反向对照**：清空 `t` 需要 `before_step = 0`
    rewind(saver.as_ref(), "t", 0).expect("rewind");
    assert!(
        saver.list("t").unwrap().is_empty(),
        "`rewind(\"t\", 0)` 应清空"
    );
    assert_eq!(saver.list("other").unwrap().len(), 3, "`other` 仍不受影响");
}
