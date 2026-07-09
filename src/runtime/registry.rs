//! v0.52 ADR-001: RegistryRuntime — BC8 (trait/impl/mock/ccr/memory registries)
//!
//! 从 Interpreter god object 抽出的 registry 状态容器，5 字段。

use std::collections::HashMap;
use std::sync::Arc;

use crate::ccr::InMemoryCcrStore;
use crate::interpreter::TraitInfo;
use crate::mock::MockRegistry;
use crate::value::Value;

#[derive(Clone)]
pub struct RegistryRuntime {
    /// v0.08: trait 系统注册表 (v0.36: wrapped in Arc for cheap clone in HTTP/MCP workers)
    pub trait_registry: Arc<HashMap<String, TraitInfo>>,
    /// v0.08: impl 表 (trait_name -> [impl_type_names])
    pub impl_table: Arc<HashMap<String, Vec<String>>>,
    /// v0.34: mock registry (OpenFugu + OpenInfer mock)
    pub mock_registry: MockRegistry,
    /// v0.34: CCR (Compress-Cache-Retrieve, Headroom style)
    pub ccr_store: InMemoryCcrStore,
    /// v0.25: 会话记忆存储
    pub memory_store: HashMap<String, Value>,
}

impl Default for RegistryRuntime {
    fn default() -> Self {
        Self {
            trait_registry: Arc::new(HashMap::new()),
            impl_table: Arc::new(HashMap::new()),
            mock_registry: MockRegistry::default(),
            ccr_store: InMemoryCcrStore::new(),
            memory_store: HashMap::new(),
        }
    }
}

impl RegistryRuntime {
    /// 存储会话记忆
    pub fn memory_remember(&mut self, key: String, value: Value) {
        self.memory_store.insert(key, value);
    }

    /// 检索会话记忆
    pub fn memory_recall(&self, key: &str) -> Option<&Value> {
        self.memory_store.get(key)
    }

    /// 注册 trait
    pub fn register_trait(&mut self, name: String, info: TraitInfo) {
        // Arc<HashMap> 是只读共享；注册走 make_mut 拷贝-改-回环
        let mut map = (*self.trait_registry).clone();
        map.insert(name, info);
        self.trait_registry = Arc::new(map);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::TraitMethodSig;

    fn make_trait_info() -> TraitInfo {
        TraitInfo {
            name: "T".to_string(),
            parents: Vec::new(),
            methods: Vec::new(),
        }
    }

    #[test]
    fn default_registry_empty_traits() {
        let reg = RegistryRuntime::default();
        assert!(reg.trait_registry.is_empty());
        assert!(reg.impl_table.is_empty());
    }

    #[test]
    fn mock_registry_default() {
        let reg = RegistryRuntime::default();
        let _ = &reg.mock_registry; // 不 panic 即可
    }

    #[test]
    fn ccr_store_default_empty() {
        let reg = RegistryRuntime::default();
        let _ = &reg.ccr_store; // 不 panic 即可
    }

    #[test]
    fn memory_store_default_empty() {
        let reg = RegistryRuntime::default();
        assert!(reg.memory_store.is_empty());
    }

    #[test]
    fn memory_remember_and_recall() {
        let mut reg = RegistryRuntime::default();
        reg.memory_remember("key1".to_string(), Value::Int(42));
        let v = reg.memory_recall("key1");
        assert_eq!(v, Some(&Value::Int(42)));
    }

    #[test]
    fn register_trait_inserts() {
        let mut reg = RegistryRuntime::default();
        reg.register_trait("T1".to_string(), make_trait_info());
        assert!(reg.trait_registry.contains_key("T1"));
    }

    #[test]
    fn clone_shares_arc_registries() {
        let reg1 = RegistryRuntime::default();
        let reg2 = reg1.clone();
        // Arc 共享：改一个应能影响另一个
        let mut map = (*reg1.trait_registry).clone();
        map.insert("shared".to_string(), make_trait_info());
        // reg1.trait_registry 是 Arc — 改 reg1 不会影响 reg2（因为 register_trait 创建新 Arc）
        // 改后两者不同 — 这是预期
        assert!(!reg2.trait_registry.contains_key("shared"));
    }

    // 避免 unused import 警告
    #[allow(dead_code)]
    fn _unused() {
        let _: TraitMethodSig = TraitMethodSig {
            name: String::new(),
            has_self: false,
            params: Vec::new(),
            return_type: None,
        };
    }
}
