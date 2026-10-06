//! D234 判据：`MemorySaver` 与 `SqliteSaver` 的**行为必须逐项一致**。
//!
//! `CheckpointSaver` 是 trait，两个内置实现此前在**三处**分叉：
//!
//! | 场景 | 修前 MemorySaver | 修前 SqliteSaver | 分叉性质 |
//! |---|---|---|---|
//! | `save(不可逆值)` | `Ok`（存原始对象，绕过 `to_json`） | **报错** | D233 的检查对前者**完全无效** |
//! | 同 `step` 取「最新」 | `b`（`max_by_key` 取最后匹配） | `a`（`ORDER BY step DESC` 取先吐出的） | 恢复到**不同状态** |
//! | 同 `id` 存两次 | `["dup","dup"]`（**重复**），load 得 step=1 | `["dup"]`，load 得 step=2 | `list()` 与 `load()` 不自洽 |
//!
//! 同 `step` 并不罕见：`pregel` 的 fault-retry 会重跑同一步，两条路径
//! 都往同一步写检查点。
//!
//! ## 判据形态
//!
//! **两实现逐项对照**（differential），而不是各自断言「看起来对」——
//! 分叉的本质是「同一个 trait 的两个实现给出不同结果」，单侧断言测不到。
//!
//! ⚠ `SqliteSaver` 在 feature `checkpoint-sqlite` 之后（**非默认**），
//! 故本文件整体 gate 住；`cargo test` 不带该 feature 时会被跳过。
//!
//! ⚠ 探针踩过的坑：`Checkpoint::new` 会**重新生成 UUID**、忽略传入的 `id`。
//! 本文件因此**直接构造结构体**，否则所有 id 断言都在比随机值。

#![cfg(feature = "checkpoint-sqlite")]

use mora::checkpoint::{Checkpoint, CheckpointSaver, MemorySaver, SqliteSaver};
use mora::value::Value;
use std::collections::HashMap;

/// 直接构造（`Checkpoint::new` 会重新生成 UUID，忽略传入 id）。
fn cp(id: &str, step: usize, ts: u128, ch: Option<Value>) -> Checkpoint {
    let mut channel_values = HashMap::new();
    if let Some(v) = ch {
        channel_values.insert("ch".to_string(), v);
    }
    let mut channel_versions = HashMap::new();
    channel_versions.insert("ch".to_string(), 1u64);
    Checkpoint {
        id: id.to_string(),
        v: 1,
        thread_id: "t".to_string(),
        step,
        channel_values,
        channel_versions,
        versions_seen: HashMap::new(),
        pending_sends: vec![],
        timestamp_ms: ts,
    }
}

/// 判据 ①：不可逆的值在**两个实现**里都必须报错。
///
/// D233 给 `to_json` 加了可逆性检查，但 `MemorySaver::save` 修前是
/// 无条件 `push`，**绕过**了它 ⇒ 检查对一个实现完全无效。
#[test]
fn d234_both_savers_reject_unrepresentable_values() {
    let bad = [
        ("char", Value::Char('中')),
        ("code", Value::Code("fn main() {}".into())),
    ];

    let mem = MemorySaver::new();
    let sq = SqliteSaver::new(":memory:").expect("sqlite");

    for (label, v) in bad {
        assert!(
            mem.save("t", &cp("bad", 1, 100, Some(v.clone()))).is_err(),
            "D234: MemorySaver 必须像 SqliteSaver 一样拒绝 {label} \
             （修前它无条件 push，完全绕过 D233 的检查）"
        );
        let err = sq
            .save("t", &cp("bad", 1, 100, Some(v.clone())))
            .expect_err("SqliteSaver 一直走 to_json，必须报错");
        assert!(
            err.to_string().contains(label),
            "错误应点名类型 {label}; 实得 {err}"
        );
    }
}

/// 判据 ②：可表示的值在**两个实现**里都必须被接受且往返恒等
/// （防「对齐时过度收紧」）。
#[test]
fn d234_both_savers_accept_representable_values() {
    let values = [
        Value::Int(1),
        Value::Float(1.5),
        Value::String("中文 🌍".into()),
        Value::List(vec![Value::Int(1), Value::String("a".into())].into()),
    ];
    let mem = MemorySaver::new();
    let sq = SqliteSaver::new(":memory:").expect("sqlite");
    for (i, v) in values.iter().enumerate() {
        let c = cp("ok", 1, 100 + i as u128, Some(v.clone()));
        mem.save("t", &c).expect("MemorySaver 应接受可表示的值");
        sq.save("t", &c).expect("SqliteSaver 应接受可表示的值");

        let m = mem.load("t", Some("ok")).expect("load").expect("some");
        let s = sq.load("t", Some("ok")).expect("load").expect("some");
        assert_eq!(m.channel_values.get("ch"), Some(v), "MemorySaver 往返失真");
        assert_eq!(s.channel_values.get("ch"), Some(v), "SqliteSaver 往返失真");
        assert_eq!(m, s, "D234: 两实现读回的检查点应完全相同");
    }
}

/// 判据 ③：同 `step` 时「最新」判定必须一致，**且**符合 `(step, ts, id)` 顺序。
///
/// 修前 memory 用 `max_by_key(step)`（并列取**最后**匹配），
/// sqlite 用 `ORDER BY step DESC LIMIT 1`（并列取先吐出的那行）
/// ⇒ 恢复出**不同状态**。现统一为 `(step, timestamp_ms, id)` 三级键。
///
/// ⚠ 只对照两个实现**不够**：若两边同时退化成「只按 step」，对照仍会绿
/// （首轮牙齿验证里 `latest-tiebreak-step-only` 那一处就是这样 NO TEETH）。
/// 故本条同时断言**绝对顺序**（timestamp 更晚的算最新），使单侧退化也变红。
#[test]
fn d234_latest_tiebreak_is_consistent() {
    // 两个 checkpoint 同 step、同 thread；timestamp 决定先后
    let a = cp("a", 5, 100, Some(Value::String("A".into())));
    let b = cp("b", 5, 200, Some(Value::String("B".into())));

    let mem = MemorySaver::new();
    let sq = SqliteSaver::new(":memory:").expect("sqlite");
    for c in [&a, &b] {
        mem.save("t", c).expect("save");
        sq.save("t", c).expect("save");
    }

    let m = mem.load("t", None).expect("load").expect("some");
    let s = sq.load("t", None).expect("load").expect("some");
    assert_eq!(
        m.id, s.id,
        "D234: 同 step 时两实现取到**不同**的检查点（修前 memory=b、sqlite=a）"
    );
    assert_eq!(
        m.id, "b",
        "D234: timestamp 更晚的应算最新。\
         修前 MemorySaver 的 `max_by_key(step)` 在并列时取**最后匹配**（碰巧也对），\
         但那是 Vec 插入顺序的巧合，不是规则 —— 换一组 id 就会分叉"
    );

    // 反向：把插入顺序反过来（先 b 后 a），绝对顺序仍必须给出 b。
    // 这一条才真正钉住「按 timestamp 而非插入序」。
    let mem2 = MemorySaver::new();
    let sq2 = SqliteSaver::new(":memory:").expect("sqlite");
    for c in [&b, &a] {
        mem2.save("t", c).expect("save");
        sq2.save("t", c).expect("save");
    }
    assert_eq!(
        mem2.load("t", None).expect("load").expect("some").id,
        "b",
        "D234: 无论插入顺序如何，timestamp 更晚的 b 必须算最新"
    );
    assert_eq!(
        sq2.load("t", None).expect("load").expect("some").id,
        "b",
        "D234: 无论插入顺序如何，timestamp 更晚的 b 必须算最新"
    );
}

/// 判据 ④：同 `id` 重复保存应**替换**，`list()` 不得出现重复。
///
/// 修前 `MemorySaver` 无条件 `push` ⇒ `list()` 返回 `["dup","dup"]`，
/// 且 `load(Some("dup"))` 取到**第一次**保存的（step=1）；
/// `SqliteSaver` 用 `INSERT OR REPLACE` ⇒ `["dup"]` + step=2。
/// 即同一个 id 在 `list()` 里出现两次，而 `load` 只认其中一个。
#[test]
fn d234_same_id_resaves_are_replaced_not_duplicated() {
    let mem = MemorySaver::new();
    let sq = SqliteSaver::new(":memory:").expect("sqlite");

    for saver_step in [1usize, 2] {
        let c = cp(
            "dup",
            saver_step,
            100 * saver_step as u128,
            Some(Value::Int(saver_step as i64)),
        );
        mem.save("t", &c).expect("save");
        sq.save("t", &c).expect("save");
    }

    let m_ids = mem.list("t").expect("list");
    let s_ids = sq.list("t").expect("list");
    assert_eq!(
        m_ids,
        vec!["dup".to_string()],
        "D234: MemorySaver 的 list() 出现重复 id（修前无条件 push）"
    );
    assert_eq!(m_ids, s_ids, "D234: 两实现的 list() 应完全相同");

    let m = mem.load("t", Some("dup")).expect("load").expect("some");
    let s = sq.load("t", Some("dup")).expect("load").expect("some");
    assert_eq!(m.step, 2, "应取到**后**保存的那份");
    assert_eq!(m, s, "D234: 两实现 load 同一 id 应得到相同检查点");
}

/// 判据 ⑤：`list()` 顺序在两个实现间一致（含同 step 的情况）。
#[test]
fn d234_list_order_is_consistent() {
    let mem = MemorySaver::new();
    let sq = SqliteSaver::new(":memory:").expect("sqlite");

    // 故意乱序插入，且含**同 step 不同 timestamp** 的两项
    let corpus = [
        ("a", 3usize, 100u128),
        ("b", 1, 200),
        ("c", 3, 50), // 与 a 同 step 但更早
        ("d", 2, 300),
    ];
    for (id, step, ts) in corpus {
        let c = cp(id, step, ts, Some(Value::Int(step as i64)));
        mem.save("t", &c).expect("save");
        sq.save("t", &c).expect("save");
    }

    let m = mem.list("t").expect("list");
    let s = sq.list("t").expect("list");
    assert_eq!(
        m, s,
        "D234: 两实现 list() 顺序不同。\n  memory={:?}\n  sqlite={:?}",
        m, s
    );
    // 期望顺序：(step, timestamp_ms, id) 升序 ⇒ b(1,200) d(2,300) c(3,50) a(3,100)
    assert_eq!(
        m,
        vec!["b", "d", "c", "a"],
        "D234: list() 应按 (step, timestamp_ms, id) 升序 —— \
         同 step 时按 timestamp 排（c 的 ts=50 早于 a 的 ts=100，\
         所以 c 在 a 之前；**不是**按 id 或插入顺序）"
    );
}

/// 判据 ⑥：thread 隔离在两个实现里都成立（防「对齐时改坏隔离」）。
#[test]
fn d234_thread_isolation_holds_in_both_savers() {
    let mem = MemorySaver::new();
    let sq = SqliteSaver::new(":memory:").expect("sqlite");
    for (tid, id) in [("t1", "x"), ("t2", "y")] {
        let mut c = cp(id, 1, 100, Some(Value::String(id.into())));
        c.thread_id = tid.to_string();
        mem.save(tid, &c).expect("save");
        sq.save(tid, &c).expect("save");
    }
    for (tid, want) in [("t1", "x"), ("t2", "y")] {
        let m = mem.load(tid, None).expect("load").expect("some");
        let s = sq.load(tid, None).expect("load").expect("some");
        assert_eq!(m.id, want, "MemorySaver: {tid} 应只看到自己的检查点");
        assert_eq!(s.id, want, "SqliteSaver: {tid} 应只看到自己的检查点");
    }
    assert!(mem.load("t3", None).expect("load").is_none());
    assert!(sq.load("t3", None).expect("load").is_none());
}
