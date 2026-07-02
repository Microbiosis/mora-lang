//! v0.32: Mock registry
//!
//! 灵感: OpenFugu MockWorld (train/train_trinity.py) + OpenInfer mock mode
//! 统一分散在 Mora 各模块的 mock response (compress/text.rs, ai_chat.rs),
//! 提供统一接口: mock.register("name", fn(args) -> result)
//!
//! 设计: 同步 in-process registry, 用 `Arc<Mutex>` 共享. 避免 async runtime.
//! 使用 Mora 自己的 Value 类型 (避免引入 serde_json).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::value::Value;

/// Mock handler 签名: 输入 Mora Value (List/Dict), 输出 Mora Value
pub type MockHandler = Arc<dyn Fn(&Value) -> Value + Send + Sync + 'static>;

/// v0.32: Mock registry
#[derive(Clone, Default)]
pub struct MockRegistry {
    handlers: Arc<Mutex<HashMap<String, MockHandler>>>,
}

impl std::fmt::Debug for MockRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.handlers.lock().map(|h| h.len()).unwrap_or(0);
        f.debug_struct("MockRegistry")
            .field("handlers", &count)
            .finish()
    }
}

impl MockRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个 mock handler by name
    pub fn register(&self, name: &str, handler: MockHandler) {
        let mut map = self.handlers.lock().expect("mock registry mutex poisoned");
        map.insert(name.to_string(), handler);
    }

    /// 注销
    pub fn unregister(&self, name: &str) {
        let mut map = self.handlers.lock().expect("mock registry mutex poisoned");
        map.remove(name);
    }

    /// 调用 mock handler. 返回 None 如果未注册
    pub fn call(&self, name: &str, args: &Value) -> Option<Value> {
        let map = self.handlers.lock().expect("mock registry mutex poisoned");
        map.get(name).map(|h| h(args))
    }

    /// 当前注册的 handler 数 (test helper)
    pub fn count(&self) -> usize {
        self.handlers
            .lock()
            .expect("mock registry mutex poisoned")
            .len()
    }

    /// 列出所有已注册 handler 名 (test helper)
    pub fn names(&self) -> Vec<String> {
        self.handlers
            .lock()
            .expect("mock registry mutex poisoned")
            .keys()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_dict(pairs: &[(&str, &str)]) -> Value {
        let mut d = HashMap::new();
        for (k, v) in pairs {
            d.insert(k.to_string(), Value::String(v.to_string()));
        }
        Value::Dict(d)
    }

    #[test]
    fn register_and_call() {
        let r = MockRegistry::new();
        r.register(
            "ai.chat",
            Arc::new(|args| {
                let prompt = if let Value::Dict(d) = args {
                    d.get("prompt").map(|v| v.to_string()).unwrap_or_default()
                } else {
                    String::new()
                };
                let mut out = HashMap::new();
                out.insert(
                    "text".to_string(),
                    Value::String(format!("[mock] {}", prompt)),
                );
                out.insert("model".to_string(), Value::String("mock".to_string()));
                Value::Dict(out)
            }),
        );
        let args = make_dict(&[("prompt", "hello")]);
        let result = r.call("ai.chat", &args).unwrap();
        if let Value::Dict(d) = result {
            assert_eq!(d.get("text").unwrap().to_string(), "[mock] hello");
            assert_eq!(d.get("model").unwrap().to_string(), "mock");
        } else {
            panic!("expected Dict");
        }
    }

    #[test]
    fn call_unregistered_returns_none() {
        let r = MockRegistry::new();
        assert!(r.call("nonexistent", &Value::Nil).is_none());
    }

    #[test]
    fn unregister_removes() {
        let r = MockRegistry::new();
        r.register("x", Arc::new(|_| Value::String("ok".into())));
        assert_eq!(r.count(), 1);
        r.unregister("x");
        assert_eq!(r.count(), 0);
        assert!(r.call("x", &Value::Nil).is_none());
    }

    #[test]
    fn multiple_handlers() {
        let r = MockRegistry::new();
        r.register("a", Arc::new(|_| Value::String("1".into())));
        r.register("b", Arc::new(|_| Value::String("2".into())));
        r.register("c", Arc::new(|_| Value::String("3".into())));
        assert_eq!(r.count(), 3);
        let mut names = r.names();
        names.sort();
        assert_eq!(names, vec!["a", "b", "c"]);
    }
}
