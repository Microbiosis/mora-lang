//! v0.104.6 D390 —— `EventBus::emit` 对**空事件名**让 catch-all listener **触发两次**
//! （修复轮；并指出 D281 判据为何没能拦住它）
//!
//! ## 缺陷
//!
//! `emit` 的 prefix 走法分两步：
//!
//! ```rust
//! if let Some(handlers) = prefix.get("") { … }      // ① catch-all "*" 桶
//! for i in 0..parts.len() {
//!     let prefix_key = parts[..=i].join(".");
//!     if let Some(handlers) = prefix.get(&prefix_key) { … }   // ② 逐段前缀
//! }
//! ```
//!
//! 事件名是**空串**时：`"".split('.')` 产出 `[""]` ⇒ `parts.len() == 1`
//! ⇒ 走法 ② 在 `i = 0` 时算出 `parts[..=0].join(".") == ""`
//! ⇒ **与 ① 命中同一个 `prefix[""]` 桶** ⇒ catch-all handler 被投喂**两次**。
//!
//! 而 `matches("", "*")` 是 **true**（D281 已把这条钉成有意行为，
//! 见 `d281_catchall_still_matches_everything`）——
//! **谓词路径说「匹配」，索引路径投递「两次」**。
//!
//! ## 为什么 D281 的「两条路径一致」判据没拦住
//!
//! `d281_predicate_and_index_paths_agree` 的断言是
//! `fired > 0` 与 `matches(...)` 比 —— **布尔比较对重数完全不敏感**
//! （0 vs 1 会红，1 vs **2** 不会）。且它的用例表里没有空事件名。
//!
//! ⇒ 与 D389「往返断言恒真」同族：**断言的形状与被测性质不匹配**。
//! 本文件把它补成 `count == if matches {1} else {0}`。
//!
//! ## 当前为何不可观察（不夸大影响）
//!
//! 生产代码里**唯一**注册 handler 的地方是 `bus.subscribe`，
//! 而它装的是 **no-op handler**（`builtins/event.rs:52-57`）
//! ⇒ 脚本侧看不到重复触发。
//! 但该处注释明写「真实 handler 由上层 (LSP / HTTP / MCP) 通过更高级 API 提供」
//! ⇒ 这是**潜伏**缺陷，不是无害代码。
//!
//! `bus.emit("")` 本身**脚本可达**且不校验空串（`builtins/event.rs:13-21`
//! 只检查是不是 `Value::String`）。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use mora::event::{EventBus, matches};
use mora::value::Value;

/// 注册一个计数 handler，返回总线与计数器。
fn counted(pattern: &str) -> (EventBus, Arc<AtomicUsize>) {
    let bus = EventBus::new();
    let fired = Arc::new(AtomicUsize::new(0));
    let f = fired.clone();
    bus.on(
        pattern,
        Arc::new(move |_e: &str, _p: &Value| {
            f.fetch_add(1, Ordering::SeqCst);
        }),
    );
    (bus, fired)
}

/// **核心**：`emit("")` 时 catch-all 必须**恰好触发一次**。
///
/// 修前实测为 **2**（本条在修前红）。
#[test]
fn d390_catchall_fires_once_for_empty_event_name() {
    let (bus, fired) = counted("*");
    bus.emit("", &Value::Nil);
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "空事件名触发 catch-all 两次 —— `prefix.get(\"\")` 被走法 ① 与 ② 各命中一次"
    );
}

/// **谓词路径与索引路径在「重数」上也必须一致** —— D281 的加强版。
///
/// D281 只比 `> 0`；本条比 `count`，并**补上 D281 用例表里没有的空事件名**。
#[test]
fn d390_predicate_and_index_agree_on_multiplicity() {
    // D281 原表 10 条 + 空事件名 / 空段 / 尾点等边界
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
        // ↓ D281 未覆盖的边界
        ("", "*"),
        ("", "a.*"),
        ("a..b", "a.*.b"),
        ("a.", "a.*"),
        (".a", "*.a"),
        ("a.b", "a.b.*"),
        ("a", "*"),
    ];
    for (event, pattern) in cases {
        let (bus, fired) = counted(pattern);
        bus.emit(event, &Value::Nil);
        let by_index = fired.load(Ordering::SeqCst);
        let by_predicate = matches(event, pattern);
        let want = if by_predicate { 1 } else { 0 };
        assert_eq!(
            by_index, want,
            "事件 {event:?} × 模式 {pattern:?}：索引路径投递 {by_index} 次，\
             谓词路径 matches={by_predicate} ⇒ 应为 {want} 次 —— \
             **重复投递**与**漏投**都是缺陷"
        );
    }
}

/// **反向对照**：不该触发的组合**一次都不能触发**。
///
/// 若本条也一起红，说明上面的红是「什么都触发」而不是「重复触发」。
#[test]
fn d390_non_matching_pairs_never_fire() {
    let negatives: &[(&str, &str)] = &[
        ("a", "b.*"),
        ("x.y", "a.*"),
        ("a", "a.b.c"),
        ("a.b", "*.c"),
        ("x", ""), // 空模式无通配符 ⇒ Exact 桶，恒不匹配
    ];
    for (event, pattern) in negatives {
        let (bus, fired) = counted(pattern);
        bus.emit(event, &Value::Nil);
        assert_eq!(
            fired.load(Ordering::SeqCst),
            0,
            "事件 {event:?} × 模式 {pattern:?} 不应触发，却触发了 {} 次",
            fired.load(Ordering::SeqCst)
        );
    }
}

/// **同一模式注册多次 ⇒ 每个 handler 各触发一次**（不是被合并，也不是被放大）。
#[test]
fn d390_multiple_handlers_each_fire_once() {
    let bus = EventBus::new();
    let a = Arc::new(AtomicUsize::new(0));
    let b = Arc::new(AtomicUsize::new(0));
    let (fa, fb) = (a.clone(), b.clone());
    bus.on(
        "a.*",
        Arc::new(move |_e, _p| {
            fa.fetch_add(1, Ordering::SeqCst);
        }),
    );
    bus.on(
        "a.*",
        Arc::new(move |_e, _p| {
            fb.fetch_add(1, Ordering::SeqCst);
        }),
    );
    bus.emit("a.b", &Value::Nil);
    bus.emit("", &Value::Nil); // 非匹配，不应触发
    assert_eq!(a.load(Ordering::SeqCst), 1, "handler A 应恰好触发一次");
    assert_eq!(b.load(Ordering::SeqCst), 1, "handler B 应恰好触发一次");
}

/// **`bus.emit("")` 脚本可达但当前不可观察** —— 钉住「为什么这个缺陷潜伏至今」。
///
/// 生产侧唯一注册 handler 处是 `bus.subscribe`，且装的是 no-op。
/// 这条把该事实钉住：若将来 `subscribe` 接上真实回调，
/// 本条会红，提醒同步补上本文件其余判据的观测面。
#[test]
fn d390_only_production_handler_is_a_noop() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/interpreter/builtins/event.rs"),
    )
    .expect("读 event.rs");
    let on_calls = src.matches("bus.on(").count();
    assert_eq!(
        on_calls, 1,
        "`builtins/event.rs` 里的 `bus.on(` 应只有 subscribe 一处（no-op 占位）; \
         实得 {on_calls} 处 —— 若真实 handler 接入，重复投递将变为可观察"
    );
    assert!(
        src.contains("no-op: subscribe 占位"),
        "该 handler 应仍是 no-op 占位"
    );
}
