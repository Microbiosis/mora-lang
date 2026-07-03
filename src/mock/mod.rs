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

/// v0.34: Mock handler 可以是 Rust 原生闭包，也可以是 Mora 脚本闭包
#[derive(Clone)]
pub enum MockHandler {
    /// Rust 原生 handler: 输入 Mora Value (List/Dict), 输出 Mora Value
    Native(Arc<dyn Fn(&Value) -> Value + Send + Sync + 'static>),
    /// Mora 脚本闭包 (Value::Closure), 需要 interpreter 执行
    Script(Value),
}

impl std::fmt::Debug for MockHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MockHandler::Native(_) => f.debug_tuple("Native").finish(),
            MockHandler::Script(v) => f.debug_tuple("Script").field(v).finish(),
        }
    }
}

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

    /// 取出 handler 的克隆，由调用方决定如何执行
    pub fn get(&self, name: &str) -> Option<MockHandler> {
        let map = self.handlers.lock().expect("mock registry mutex poisoned");
        map.get(name).cloned()
    }

    /// 调用 mock handler. 返回 None 如果未注册。
    /// 注意：Script handler 需要 interpreter，因此这里只执行 Native handler。
    ///
    /// v0.35 (P0-A3): clone-and-drop — drop the lock before invoking the
    /// handler so a Native handler that re-enters the registry on the
    /// same thread does NOT deadlock.
    pub fn call(&self, name: &str, args: &Value) -> Option<Value> {
        let handler = self
            .handlers
            .lock()
            .expect("mock registry mutex poisoned")
            .get(name)
            .cloned();
        match handler {
            Some(MockHandler::Native(f)) => Some(f(args)),
            _ => None,
        }
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
    fn register_and_call_native() {
        let r = MockRegistry::new();
        r.register(
            "ai.chat",
            MockHandler::Native(Arc::new(|args| {
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
            })),
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
    fn register_and_call_script() {
        // Script handler 只能 get 出来，不能直接用 call() 执行（需要 interpreter）
        let r = MockRegistry::new();
        let closure = Value::Closure {
            params: vec!["x".to_string()],
            env: Arc::new(Mutex::new(crate::value::Environment::new())),
            v2_node_id: None,
        };
        r.register("script.handler", MockHandler::Script(closure));
        assert_eq!(r.count(), 1);
        let names = r.names();
        assert_eq!(names, vec!["script.handler"]);
        if let Some(MockHandler::Script(_)) = r.get("script.handler") {
            // ok
        } else {
            panic!("expected Script handler");
        }
        // call() 对 Script handler 返回 None
        assert!(r.call("script.handler", &Value::Nil).is_none());
    }

    #[test]
    fn call_unregistered_returns_none() {
        let r = MockRegistry::new();
        assert!(r.call("nonexistent", &Value::Nil).is_none());
    }

    #[test]
    fn unregister_removes() {
        let r = MockRegistry::new();
        r.register(
            "x",
            MockHandler::Native(Arc::new(|_| Value::String("ok".into()))),
        );
        assert_eq!(r.count(), 1);
        r.unregister("x");
        assert_eq!(r.count(), 0);
        assert!(r.call("x", &Value::Nil).is_none());
    }

    #[test]
    fn multiple_handlers() {
        let r = MockRegistry::new();
        r.register(
            "a",
            MockHandler::Native(Arc::new(|_| Value::String("1".into()))),
        );
        r.register(
            "b",
            MockHandler::Native(Arc::new(|_| Value::String("2".into()))),
        );
        r.register(
            "c",
            MockHandler::Native(Arc::new(|_| Value::String("3".into()))),
        );
        assert_eq!(r.count(), 3);
        let mut names = r.names();
        names.sort();
        assert_eq!(names, vec!["a", "b", "c"]);
    }
}
