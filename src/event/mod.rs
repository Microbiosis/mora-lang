//! v0.32+: Event bus with wildcard pattern matching
//!
//! 灵感: Puter EventClient (src/backend/clients/event/EventClient.ts:62-67)
//! - emit("outer.gui.item.removed") 触发所有匹配 listener:
//!   * 精确 `outer.gui.item.removed`
//!   * `outer.gui.item.*` (single-segment wildcard)
//!   * `outer.gui.*`
//!   * `outer.*` (catch-all)
//! - Listener 注册通过 `on` 接受模式字符串 (含 `*` segment)
//!
//! 设计: 单线程同步, 用 `Arc<Mutex>` 共享. 避免 async runtime 依赖 (符合 Mora
//! "no async runtime" 红线).
//!
//! v0.41.0 (P0): O(segments) indexed matching 替代线性扫描 (灵感: Puter
//! EventClient 代码片段, 见 RESEARCH_PRIMITIVES_MASTER_v2.md §1.10)
//!
//! 索引结构 (双层):
//! - `exact`: 字面量键 (e.g. "ai.chat.completed") → Vec<Handler>
//! - `prefix`: 仅末尾 ".*" 模式 (e.g. "ai.chat.*") → Vec<Handler>, key 即 prefix
//! - `interior`: 中间含通配符的模式 (e.g. "ai.*.completed") → Vec<Handler>
//!   interior 用量极少, emit 时 fallback linear scan 即可; v0.41 不优化
//!
//! emit 路径:
//! 1. 字面量: O(1) HashMap lookup
//! 2. prefix: O(segments) 遍历 (a → a.b → a.b.c, 每步查 map["a.b.*"])
//! 3. interior: O(interior_patterns) 线性扫描

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::value::Value;

/// Listener 标识: 模式字符串 (e.g. "outer.*" 或 "ai.chat.completed")
pub type Pattern = String;

/// Listener handler: 接收 event_name 和 payload
pub type Handler = Arc<dyn Fn(&str, &Value) + Send + Sync + 'static>;

/// v0.32: Event bus
#[derive(Clone, Default)]
pub struct EventBus {
    /// v0.41: 双层索引, emit 走 O(segments) 路径
    /// 字面量模式 (无通配符) → handler 列表
    /// v0.50.0 (P0-10): Mutex → RwLock (emit 是 read-only, 多 reader 并发)
    exact: Arc<RwLock<HashMap<Pattern, Vec<Handler>>>>,
    /// 末尾通配符模式 ("a.b.*" 形式, key 即不带末尾 ".*" 的 prefix) → handler 列表
    /// v0.50.0 (P0-10): RwLock (same reason)
    prefix: Arc<RwLock<HashMap<Pattern, Vec<Handler>>>>,
    /// 中间通配符模式 (e.g. "a.*.c", "*.b.*") → handler 列表
    /// fallback linear scan; 用量极少
    /// v0.50.0 (P0-10): RwLock
    interior: Arc<RwLock<HashMap<Pattern, Vec<Handler>>>>,
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let exact = self.exact.read().map(|h| h.len()).unwrap_or(0);
        let prefix = self.prefix.read().map(|h| h.len()).unwrap_or(0);
        let interior = self.interior.read().map(|h| h.len()).unwrap_or(0);
        f.debug_struct("EventBus")
            .field("exact", &exact)
            .field("prefix", &prefix)
            .field("interior", &interior)
            .finish()
    }
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个 pattern + handler. 同 pattern 多次 on 会追加 handler.
    ///
    /// 自动路由到合适的索引桶:
    /// - 无 `*` → exact
    /// - 末尾 `.*` (且仅末尾) → prefix (key = 不带末尾 ".*" 的 prefix)
    /// - 其它 → interior (中间通配符)
    pub fn on(&self, pattern: &str, handler: Handler) {
        let bucket = classify_pattern(pattern);
        match bucket {
            PatternBucket::Exact => {
                // v0.50.0 (P0-10): write lock (on is a write op, rare)
                self.exact
                    .write()
                    .expect("event bus rwlock poisoned")
                    .entry(pattern.to_string())
                    .or_default()
                    .push(handler);
            }
            PatternBucket::Prefix(prefix_key) => {
                self.prefix
                    .write()
                    .expect("event bus rwlock poisoned")
                    .entry(prefix_key)
                    .or_default()
                    .push(handler);
            }
            PatternBucket::Interior => {
                self.interior
                    .write()
                    .expect("event bus mutex poisoned")
                    .entry(pattern.to_string())
                    .or_default()
                    .push(handler);
            }
        }
    }

    /// emit 一个 event, 触发所有匹配 pattern 的 handlers.
    /// matching 算法 (v0.41 P0):
    /// 1. 精确: O(1) HashMap 查 `exact[event]`
    /// 2. prefix: O(segments) 遍历 segments → 查 `prefix["seg0.seg1...segN"]`
    /// 3. interior: O(interior_count) fallback 扫描, 调 matches() 谓词
    ///
    /// v0.35 (P0-A2): clone-and-drop — drop the lock before invoking
    /// handlers so a handler that re-enters `bus.emit` on the same
    /// thread does NOT deadlock on the non-reentrant Mutex.
    pub fn emit(&self, event: &str, payload: &Value) {
        // Snapshot all matching handlers under locks, then drop locks, then invoke.
        let snapshot: Vec<Handler> = {
            let mut out: Vec<Handler> = Vec::new();

            // 1. Exact match (O(1))
            if let Ok(exact) = self.exact.read()
                && let Some(handlers) = exact.get(event)
            {
                out.extend(handlers.iter().cloned());
            }

            // 2. Prefix walks (O(segments))
            //    e.g. event = "outer.gui.item.removed"
            //    parts = ["outer", "gui", "item", "removed"]
            //    for i in 0..4: prefix_key = "outer", "outer.gui", "outer.gui.item", "outer.gui.item"
            //    but i == parts.len() - 1 we already covered in exact (handler is the literal form)
            //    so we walk i in 0..parts.len()-1 looking up "outer", "outer.gui", "outer.gui.item"
            //    matching prefix["outer.*"], prefix["outer.gui.*"], prefix["outer.gui.item.*"]
            //    BUT: prefix key 不带末尾 ".*", so we lookup "outer", "outer.gui", "outer.gui.item"
            if let Ok(prefix) = self.prefix.read() {
                let parts: Vec<&str> = event.split('.').collect();
                // Catch-all: prefix[""] corresponds to "*" pattern, always fires
                if let Some(handlers) = prefix.get("") {
                    out.extend(handlers.iter().cloned());
                }
                for i in 0..parts.len() {
                    let prefix_key = parts[..=i].join(".");
                    if let Some(handlers) = prefix.get(&prefix_key) {
                        out.extend(handlers.iter().cloned());
                    }
                }
            }

            // 3. Interior wildcard patterns (fallback linear scan, but rare)
            if let Ok(interior) = self.interior.read() {
                for (pattern, handlers) in interior.iter() {
                    if matches(event, pattern) {
                        out.extend(handlers.iter().cloned());
                    }
                }
            }

            out
        };

        for h in snapshot {
            h(event, payload);
        }
    }

    /// 取消注册一个 pattern (所有 handler)
    pub fn off(&self, pattern: &str) {
        let bucket = classify_pattern(pattern);
        match bucket {
            PatternBucket::Exact => {
                self.exact
                    .write()
                    .expect("event bus rwlock poisoned")
                    .remove(pattern);
            }
            PatternBucket::Prefix(prefix_key) => {
                self.prefix
                    .write()
                    .expect("event bus rwlock poisoned")
                    .remove(&prefix_key);
            }
            PatternBucket::Interior => {
                self.interior
                    .write()
                    .expect("event bus rwlock poisoned")
                    .remove(pattern);
            }
        }
    }

    /// 当前注册的 (exact + prefix + interior) 模式总数 (test helper)
    pub fn pattern_count(&self) -> usize {
        let exact = self.exact.read().map(|h| h.len()).unwrap_or(0);
        let prefix = self.prefix.read().map(|h| h.len()).unwrap_or(0);
        let interior = self.interior.read().map(|h| h.len()).unwrap_or(0);
        exact + prefix + interior
    }
}

/// v0.41: 模式分类桶
#[derive(Debug, Clone, PartialEq, Eq)]
enum PatternBucket {
    /// 无通配符, 精确字面量
    Exact,
    /// 末尾通配符 ("a.b.*"), 字段是不带末尾 ".*" 的 prefix 字符串
    /// e.g. pattern="outer.gui.*" → prefix_key="outer.gui"
    Prefix(String),
    /// 中间通配符 (e.g. "a.*.c", "*.b.*"), fallback linear scan
    Interior,
}

/// v0.41: 将 pattern 字符串路由到合适的桶
///
/// 规则:
/// - 无 `*` segment → Exact
/// - 仅最后一个 segment 是 `*` 且 pattern 末尾是 ".*" → Prefix (prefix_key = 不带末尾 ".*")
/// - 其它 (中间含 `*`) → Interior
///
/// Examples:
/// - "ai.chat" → Exact
/// - "outer.*" → Prefix("outer")
/// - "outer.gui.*" → Prefix("outer.gui")
/// - "*" → Prefix("") (catch-all, matches everything)
/// - "ai.*.completed" → Interior
/// - "*.b.*" → Interior
fn classify_pattern(pattern: &str) -> PatternBucket {
    let segments: Vec<&str> = pattern.split('.').collect();
    let star_count = segments.iter().filter(|s| **s == "*").count();

    if star_count == 0 {
        return PatternBucket::Exact;
    }

    // 仅末尾是 "*" → Prefix
    if star_count == 1 && segments.last() == Some(&"*") {
        // prefix_key = 不带末尾 ".*" 的字符串
        // pattern "outer.*" → "outer"
        // pattern "*" → ""
        // pattern "outer.gui.*" → "outer.gui"
        let prefix_key = if segments.len() == 1 {
            String::new() // "*" catch-all
        } else {
            segments[..segments.len() - 1].join(".")
        };
        return PatternBucket::Prefix(prefix_key);
    }

    PatternBucket::Interior
}

/// v0.32: 检查 event 是否匹配 pattern (Puter 风格)
/// - pattern 以 `*` 结尾 (e.g. "outer.*") 匹配所有以 `outer.` 开头的 events
/// - pattern 中间含 `*` 匹配单个 segment
/// - pattern 全 `*` 匹配所有
///
/// Example: "outer.*" 匹配 "outer.gui" "outer.foo" "outer.gui.item.removed"
///
/// v0.41: 此函数仅在 interior 桶的 fallback 扫描中使用; exact/prefix 桶走 O(1)/O(seg) 索引
pub fn matches(event: &str, pattern: &str) -> bool {
    let ev_segments: Vec<&str> = event.split('.').collect();
    let pa_segments: Vec<&str> = pattern.split('.').collect();

    // 模式以 ".*" 结尾表示 catch-all prefix, 不要求段数相等
    if let Some(last) = pa_segments.last()
        && *last == "*"
        && pa_segments.len() <= ev_segments.len() + 1
    {
        // check prefix match (last segment is "*", so we match pa_segments[..end] against ev[..end])
        let prefix_len = pa_segments.len() - 1;
        for (e, p) in ev_segments
            .iter()
            .take(prefix_len)
            .zip(pa_segments.iter().take(prefix_len))
        {
            if *p == "*" {
                continue;
            }
            if e != p {
                return false;
            }
        }
        return true;
    }

    // 精确段数匹配 (interior wildcards)
    if pa_segments.len() != ev_segments.len() {
        return false;
    }
    for (e, p) in ev_segments.iter().zip(pa_segments.iter()) {
        if *p == "*" {
            continue;
        }
        if e != p {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn matches_exact() {
        assert!(matches("a.b.c", "a.b.c"));
        assert!(!matches("a.b.c", "a.b"));
        assert!(!matches("a.b", "a.b.c"));
    }

    #[test]
    fn matches_single_segment_wildcard() {
        // "outer.*" matches any event starting with "outer."
        assert!(matches("outer.gui", "outer.*"));
        assert!(matches("outer.foo", "outer.*"));
        assert!(matches("outer.gui.item.removed", "outer.*"));
        assert!(!matches("other.gui", "outer.*"));
        // "outer.*.item" requires exactly 3 segments
        assert!(matches("outer.gui.item", "outer.*.item"));
        assert!(!matches("outer.gui", "outer.*.item"));
        assert!(!matches("outer.gui.item.x", "outer.*.item"));
    }

    #[test]
    fn matches_multiple_wildcards() {
        // "*.b.*.d" — interior wildcards require exact length
        assert!(matches("a.b.c.d", "*.b.*.d"));
        assert!(!matches("a.b.c", "*.b.*.d"));
        assert!(!matches("a.b.c.d.e", "*.b.*.d"));
    }

    #[test]
    fn matches_catchall() {
        // "*" matches everything
        assert!(matches("anything", "*"));
        assert!(matches("a", "*"));
        assert!(matches("a.b.c", "*"));
    }

    #[test]
    fn matches_puter_style_walk() {
        // Puter EventClient emits "outer.gui.item.removed"
        // listeners: "outer.*", "outer.gui.*", "outer.gui.item.*", exact
        // all should match
        assert!(matches("outer.gui.item.removed", "outer.*"));
        assert!(matches("outer.gui.item.removed", "outer.gui.*"));
        assert!(matches("outer.gui.item.removed", "outer.gui.item.*"));
        assert!(matches("outer.gui.item.removed", "outer.gui.item.removed"));
        // non-matching: different prefix
        assert!(!matches("inner.gui.item.removed", "outer.*"));
    }

    #[test]
    fn bus_emit_triggers_matching_handlers() {
        let bus = EventBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let c1 = counter.clone();
        bus.on(
            "outer.*",
            Arc::new(move |_, _| {
                c1.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let c2 = counter.clone();
        bus.on(
            "outer.gui.*",
            Arc::new(move |_, _| {
                c2.fetch_add(2, Ordering::SeqCst);
            }),
        );
        let c3 = counter.clone();
        bus.on(
            "outer.gui.item.added",
            Arc::new(move |_, _| {
                c3.fetch_add(4, Ordering::SeqCst);
            }),
        );

        bus.emit("outer.gui.item.added", &Value::Nil);
        // outer.* (1) + outer.gui.* (2) + exact (4) = 7
        assert_eq!(counter.load(Ordering::SeqCst), 7);
    }

    #[test]
    fn bus_emit_skips_non_matching() {
        let bus = EventBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        bus.on(
            "ai.*",
            Arc::new(move |_, _| {
                c.fetch_add(1, Ordering::SeqCst);
            }),
        );
        bus.emit("file.write", &Value::Nil);
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn bus_off_removes_pattern() {
        let bus = EventBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        bus.on(
            "ai.*",
            Arc::new(move |_, _| {
                c.fetch_add(1, Ordering::SeqCst);
            }),
        );
        bus.emit("ai.chat", &Value::Nil);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        bus.off("ai.*");
        bus.emit("ai.chat", &Value::Nil);
        assert_eq!(counter.load(Ordering::SeqCst), 1); // unchanged
    }

    // ===== v0.41.0 P0: O(segments) indexed matching tests =====

    #[test]
    fn classify_pattern_routes_correctly() {
        assert_eq!(classify_pattern("ai.chat"), PatternBucket::Exact);
        assert_eq!(
            classify_pattern("outer.*"),
            PatternBucket::Prefix("outer".to_string())
        );
        assert_eq!(
            classify_pattern("outer.gui.*"),
            PatternBucket::Prefix("outer.gui".to_string())
        );
        assert_eq!(classify_pattern("*"), PatternBucket::Prefix(String::new()));
        assert_eq!(classify_pattern("ai.*.completed"), PatternBucket::Interior);
        assert_eq!(classify_pattern("*.b.*"), PatternBucket::Interior);
        assert_eq!(classify_pattern("a.b.*.d"), PatternBucket::Interior);
    }

    #[test]
    fn bus_handlers_route_to_correct_buckets() {
        let bus = EventBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        // Exact pattern
        {
            let c = counter.clone();
            bus.on(
                "ai.chat.done",
                Arc::new(move |_, _| {
                    c.fetch_add(1, Ordering::SeqCst);
                }),
            );
        }
        // Prefix pattern (末端通配符)
        {
            let c = counter.clone();
            bus.on(
                "ai.*",
                Arc::new(move |_, _| {
                    c.fetch_add(1, Ordering::SeqCst);
                }),
            );
        }
        // Interior pattern (中间通配符)
        {
            let c = counter.clone();
            bus.on(
                "a.*.z",
                Arc::new(move |_, _| {
                    c.fetch_add(1, Ordering::SeqCst);
                }),
            );
        }

        // Debug output should show 1 exact + 1 prefix + 1 interior
        assert_eq!(bus.pattern_count(), 3);
    }

    #[test]
    fn bus_emit_literal_match_fires_handler() {
        let bus = EventBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        bus.on(
            "ai.chat.completed",
            Arc::new(move |_, _| {
                c.fetch_add(1, Ordering::SeqCst);
            }),
        );
        bus.emit("ai.chat.completed", &Value::Nil);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        // Non-matching literal event should not fire
        bus.emit("ai.chat.failed", &Value::Nil);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn bus_emit_wildcard_match_fires_handler() {
        let bus = EventBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        {
            let c = counter.clone();
            bus.on(
                "ai.*",
                Arc::new(move |_, _| {
                    c.fetch_add(1, Ordering::SeqCst);
                }),
            );
        }
        {
            let c = counter.clone();
            bus.on(
                "ai.chat.*",
                Arc::new(move |_, _| {
                    c.fetch_add(10, Ordering::SeqCst);
                }),
            );
        }
        bus.emit("ai.chat.completed", &Value::Nil);
        // ai.* (1) + ai.chat.* (10) = 11
        assert_eq!(counter.load(Ordering::SeqCst), 11);
    }

    #[test]
    fn bus_emit_with_no_subscribers_is_noop() {
        let bus = EventBus::new();
        bus.emit("nothing.here", &Value::Nil); // no panic, no fire
        assert_eq!(bus.pattern_count(), 0);
    }

    #[test]
    fn bus_emit_with_multiple_wildcards_fires_all() {
        let bus = EventBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        // Register 4 levels of Puter-style catch-alls
        bus.on(
            "a.*",
            Arc::new(move |_, _| {
                c.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let c = counter.clone();
        bus.on(
            "a.b.*",
            Arc::new(move |_, _| {
                c.fetch_add(10, Ordering::SeqCst);
            }),
        );
        let c = counter.clone();
        bus.on(
            "a.b.c.*",
            Arc::new(move |_, _| {
                c.fetch_add(100, Ordering::SeqCst);
            }),
        );
        let c = counter.clone();
        bus.on(
            "a.b.c.d",
            Arc::new(move |_, _| {
                c.fetch_add(1000, Ordering::SeqCst);
            }),
        );

        bus.emit("a.b.c.d", &Value::Nil);
        // 1 + 10 + 100 + 1000 = 1111
        assert_eq!(counter.load(Ordering::SeqCst), 1111);
    }

    #[test]
    fn bus_interior_wildcard_still_works() {
        // 中间通配符应走 interior bucket 并正确触发
        let bus = EventBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        bus.on(
            "a.*.c",
            Arc::new(move |_, _| {
                c.fetch_add(1, Ordering::SeqCst);
            }),
        );
        bus.emit("a.b.c", &Value::Nil);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        bus.emit("a.x.c", &Value::Nil);
        assert_eq!(counter.load(Ordering::SeqCst), 2);
        // Non-matching interior
        bus.emit("a.b.d", &Value::Nil);
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn bus_catchall_star_routes_to_prefix_empty() {
        let bus = EventBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        bus.on(
            "*",
            Arc::new(move |_, _| {
                c.fetch_add(1, Ordering::SeqCst);
            }),
        );
        bus.emit("anything", &Value::Nil);
        bus.emit("a", &Value::Nil);
        bus.emit("a.b.c", &Value::Nil);
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn bus_off_removes_from_correct_bucket() {
        let bus = EventBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        {
            let c = counter.clone();
            bus.on(
                "ai.*",
                Arc::new(move |_, _| {
                    c.fetch_add(1, Ordering::SeqCst);
                }),
            );
        }
        {
            let c = counter.clone();
            bus.on(
                "ai.chat",
                Arc::new(move |_, _| {
                    c.fetch_add(1, Ordering::SeqCst);
                }),
            );
        }
        {
            let c = counter.clone();
            bus.on(
                "a.*.z",
                Arc::new(move |_, _| {
                    c.fetch_add(1, Ordering::SeqCst);
                }),
            );
        }
        assert_eq!(bus.pattern_count(), 3);
        bus.off("ai.*");
        assert_eq!(bus.pattern_count(), 2);
        bus.off("ai.chat");
        assert_eq!(bus.pattern_count(), 1);
        bus.off("a.*.z");
        assert_eq!(bus.pattern_count(), 0);
    }

    /// v0.41.0 P0 性能基准: 1000 个订阅, 1000 次 emit, 验证 O(segments) 路径不退化
    /// v0.32 旧实现是 O(patterns × segments) per emit
    /// v0.41 新实现是 O(segments) per emit (固定 ~5 lookups)
    #[test]
    fn bus_emit_complexity_scales_with_segments_not_patterns() {
        let bus = EventBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        // 100 个不匹配的订阅 + 1 个匹配的
        for i in 0..100 {
            let c = counter.clone();
            bus.on(
                &format!("other.{}.*", i),
                Arc::new(move |_, _| {
                    c.fetch_add(1, Ordering::SeqCst);
                }),
            );
        }
        // 1 个匹配的精确订阅
        let c2 = counter.clone();
        bus.on(
            "target.event",
            Arc::new(move |_, _| {
                c2.fetch_add(1, Ordering::SeqCst);
            }),
        );

        let start = std::time::Instant::now();
        for _ in 0..1000 {
            bus.emit("target.event", &Value::Nil);
        }
        let elapsed = start.elapsed();

        // 1000 次 emit × 1 个匹配 handler = 1000 increments
        assert_eq!(counter.load(Ordering::SeqCst), 1000);
        // Sanity bound: even on slow debug build, 1000 emits with 100 patterns should < 200ms
        assert!(
            elapsed.as_millis() < 200,
            "emit too slow: {:?} for 1000 emits with 100 patterns",
            elapsed
        );
    }
}
