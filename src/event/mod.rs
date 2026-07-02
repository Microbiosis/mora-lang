//! v0.32: Event bus with wildcard pattern matching
//!
//! 灵感: Puter EventClient (src/backend/clients/event/EventClient.ts)
//! - emit("outer.gui.item.removed") 触发所有匹配 listener:
//!   * 精确 `outer.gui.item.removed`
//!   * `outer.gui.item.*` (single-segment wildcard)
//!   * `outer.gui.*`
//!   * `outer.*` (catch-all)
//! - Listener 注册通过 `on` 接受模式字符串 (含 `*` segment)
//!
//! 设计: 单线程同步, 用 Arc<Mutex> 共享. 避免 async runtime 依赖 (符合 Mora
//! "no async runtime" 红线).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Listener 标识: 模式字符串 (e.g. "outer.*" 或 "ai.chat.completed")
pub type Pattern = String;

/// Listener handler: 接收 event_name 和 payload
pub type Handler = Arc<dyn Fn(&str, &Value) + Send + Sync + 'static>;

/// v0.32: Event bus
#[derive(Clone, Default)]
pub struct EventBus {
    /// 模式 → Vec<Handler>; 模式按 "*" 切分, 存储在 trie-like HashMap
    /// (简化版: 单层 HashMap, emit 时 linear scan)
    handlers: Arc<Mutex<HashMap<Pattern, Vec<Handler>>>>,
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.handlers.lock().map(|h| h.len()).unwrap_or(0);
        f.debug_struct("EventBus")
            .field("patterns", &count)
            .finish()
    }
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个 pattern + handler. 同 pattern 多次 on 会追加 handler.
    pub fn on(&self, pattern: &str, handler: Handler) {
        let mut map = self.handlers.lock().expect("event bus mutex poisoned");
        map.entry(pattern.to_string()).or_default().push(handler);
    }

    /// emit 一个 event, 触发所有匹配 pattern 的 handlers.
    /// matching 算法: event "outer.gui.item.removed" 匹配
    /// - 精确: "outer.gui.item.removed"
    /// - wildcard: "outer.*", "outer.gui.*", "outer.gui.item.*"
    pub fn emit(&self, event: &str, payload: &Value) {
        let map = self.handlers.lock().expect("event bus mutex poisoned");
        for (pattern, handlers) in map.iter() {
            if matches(event, pattern) {
                for h in handlers {
                    h(event, payload);
                }
            }
        }
    }

    /// 取消注册一个 pattern (所有 handler)
    pub fn off(&self, pattern: &str) {
        let mut map = self.handlers.lock().expect("event bus mutex poisoned");
        map.remove(pattern);
    }

    /// 当前注册的模式数 (test helper)
    pub fn pattern_count(&self) -> usize {
        self.handlers
            .lock()
            .expect("event bus mutex poisoned")
            .len()
    }
}

/// v0.32: 检查 event 是否匹配 pattern (Puter 风格)
/// - pattern 以 `*` 结尾 (e.g. "outer.*") 匹配所有以 `outer.` 开头的 events
/// - pattern 中间含 `*` 匹配单个 segment
/// - pattern 全 `*` 匹配所有
///
/// Example: "outer.*" 匹配 "outer.gui" "outer.foo" "outer.gui.item.removed"
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

use crate::value::Value;

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
}
