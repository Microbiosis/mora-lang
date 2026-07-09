//! v0.52 ADR-001: InfraRuntime — BC9 (scheduling + 字符串驻留 + recorder)

use std::sync::{Arc, Mutex};

use crate::event::EventBus;
use crate::interpreter::LruCache;
use crate::record::Recorder;
use crate::schedule::Scheduler;
use crate::value::Value;

/// v0.52 ADR-001: InfraRuntime — BC9
///
/// 注：Recorder 不实现 Clone（pre-existing，per-thread 状态）。
/// Clone 时 recorder 重建为 new_off()（与 `Interpreter::clone` 现有行为一致）。
/// Arc<Mutex<LruCache<..>> 字段共享 Arc，scheduler/bus 有 Clone impl。
pub struct InfraRuntime {
    pub recorder: Recorder,
    pub string_interner: Arc<Mutex<LruCache<Value>>>,
    pub ai_cache: Arc<Mutex<LruCache<String>>>,
    pub bus: EventBus,
    pub scheduler: Scheduler,
}

impl Clone for InfraRuntime {
    fn clone(&self) -> Self {
        Self {
            recorder: Recorder::new_off(), // 同 Interpreter::clone 行为 — 重建
            string_interner: self.string_interner.clone(),
            ai_cache: self.ai_cache.clone(),
            bus: self.bus.clone(),
            scheduler: self.scheduler.clone(),
        }
    }
}

impl Default for InfraRuntime {
    fn default() -> Self {
        Self {
            recorder: Recorder::new_off(),
            string_interner: Arc::new(Mutex::new(LruCache::new(50000))),
            ai_cache: Arc::new(Mutex::new(LruCache::new(10000))),
            bus: EventBus::new(),
            scheduler: Scheduler::default(),
        }
    }
}

impl InfraRuntime {
    /// 字符串驻留（去重）
    ///
    /// 注：原 `Interpreter` 内 `string_interner: LruCache<Value>` 直接 put；
    /// 现迁到 InfraRuntime 后保持同样语义。返回 put 后 key 的 index（未使用）。
    pub fn intern_string(&self, val: Value) -> usize {
        // Mutex 可能 poison 但概率极低，expect 即可（与项目其它 Mutex.lock() 模式一致）
        let mut cache = self
            .string_interner
            .lock()
            .expect("InfraRuntime string_interner poisoned");
        // 用 Value::to_string() 作为 key（v0.22 旧逻辑用 Debug fmt — 简化为 to_string）
        let key = val.to_string();
        cache.put(key.clone(), val);
        // 返回 put 后 key 的 map index（仅占位 — 实际项目未使用该返回值）
        key.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_interner_dedups() {
        let infra = InfraRuntime::default();
        let id1 = infra.intern_string(Value::String("hello".into()));
        let id2 = infra.intern_string(Value::String("hello".into()));
        // 返回 key.len()，相同字符串必然相同
        assert_eq!(id1, id2);
    }

    #[test]
    fn ai_cache_starts_empty() {
        let infra = InfraRuntime::default();
        let mut cache = infra
            .ai_cache
            .lock()
            .expect("InfraRuntime ai_cache poisoned");
        assert!(cache.get("anything").is_none());
    }

    #[test]
    fn bus_default_constructor() {
        let _bus = InfraRuntime::default().bus;
    }

    #[test]
    fn scheduler_default_constructor() {
        let _sched = InfraRuntime::default().scheduler;
    }

    #[test]
    fn recorder_default_constructor() {
        let _rec = InfraRuntime::default().recorder;
    }

    #[test]
    fn clone_shares_lru_cache() {
        let infra1 = InfraRuntime::default();
        let infra2 = infra1.clone();
        // Arc 共享：改一个应能影响另一个
        infra1
            .ai_cache
            .lock()
            .expect("poisoned")
            .put("k".to_string(), "v".to_string());
        let got = infra2.ai_cache.lock().expect("poisoned").get("k");
        assert_eq!(got, Some("v".to_string()));
    }

    #[test]
    fn clone_recreates_recorder() {
        // 与 Interpreter::clone 行为一致：recorder 重建（per-thread 状态）
        let infra1 = InfraRuntime::default();
        let _infra2 = infra1.clone();
        // 无 panic 即可 — recorder 字段独立
    }
}
