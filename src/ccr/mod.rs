//! v0.33: CCR (Compress-Cache-Retrieve) for lossy compression
//!
//! 灵感: Headroom CCR
//! (https://github.com/chopratejas/headroom)
//! (crates/headroom-core/src/transforms/smart_crusher/compaction/walker.rs)
//!
//! 核心思想: 即使 lossy 压缩丢数据, 也不真正丢:
//! - 原值存档到 CcrStore
//! - 压缩结果插入 marker: `<<ccr:HASH,KIND,SIZE>>` (12-char SHA-256 hex prefix)
//! - LLM 调 retrieve tool 拉回原值
//!
//! Mora v0.33 简化:
//! - CcrStore trait + InMemoryCcrStore impl
//! - Hash: 8-char hex from u64 counter (Headroom 用 SHA-256; Mora 简化)
//! - Marker format: `<<ccr:HASH,SIZE>>` (Mora 简化去掉 KIND)

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// v0.33: CCR stored payload
#[derive(Debug, Clone)]
pub struct CcrEntry {
    pub hash: String,
    pub size: usize,
    pub data: String,
}

/// v0.33: CcrStore trait
pub trait CcrStore: std::fmt::Debug + Send + Sync {
    /// 存储一个 entry. 返回 hash.
    fn put(&self, data: &str) -> String;
    /// 拉回一个 entry by hash
    fn get(&self, hash: &str) -> Option<CcrEntry>;
    /// 当前 entry 数 (test helper)
    fn len(&self) -> usize;
    /// 是否空 (test helper)
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// v0.33: Default in-memory store
#[derive(Debug, Default)]
pub struct InMemoryCcrStore {
    entries: Arc<Mutex<HashMap<String, CcrEntry>>>,
    counter: AtomicU64,
}

impl InMemoryCcrStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CcrStore for InMemoryCcrStore {
    fn put(&self, data: &str) -> String {
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        // 8-char hex from counter
        let hash = format!("{:08x}", n);
        let entry = CcrEntry {
            hash: hash.clone(),
            size: data.len(),
            data: data.to_string(),
        };
        self.entries
            .lock()
            .expect("ccr store mutex poisoned")
            .insert(hash.clone(), entry);
        hash
    }

    fn get(&self, hash: &str) -> Option<CcrEntry> {
        self.entries
            .lock()
            .expect("ccr store mutex poisoned")
            .get(hash)
            .cloned()
    }

    fn len(&self) -> usize {
        self.entries.lock().expect("ccr store mutex poisoned").len()
    }
}

/// v0.33: marker format
pub fn make_marker(hash: &str, size: usize) -> String {
    format!("<<ccr:{},{}>>", hash, size)
}

/// v0.33: extract hash from marker (returns None if not a marker)
pub fn extract_hash(marker: &str) -> Option<&str> {
    if marker.starts_with("<<ccr:") && marker.ends_with(">>") {
        let inner = &marker[6..marker.len() - 2];
        // 格式: hash,size — 取 hash 部分 (直到第一个 ',')
        Some(inner.split(',').next().unwrap_or(""))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_and_get() {
        let store = InMemoryCcrStore::new();
        let hash = store.put("hello world");
        assert_eq!(hash.len(), 8);
        let entry = store.get(&hash).unwrap();
        assert_eq!(entry.data, "hello world");
        assert_eq!(entry.size, 11);
        assert_eq!(entry.hash, hash);
    }

    #[test]
    fn put_unique_hashes() {
        let store = InMemoryCcrStore::new();
        let h1 = store.put("data1");
        let h2 = store.put("data2");
        let h3 = store.put("data3");
        assert_ne!(h1, h2);
        assert_ne!(h2, h3);
        assert_eq!(store.len(), 3);
    }

    #[test]
    fn get_unknown_hash_returns_none() {
        let store = InMemoryCcrStore::new();
        assert!(store.get("deadbeef").is_none());
    }

    #[test]
    fn marker_format_and_extract() {
        let marker = make_marker("abcd1234", 42);
        assert_eq!(marker, "<<ccr:abcd1234,42>>");
        assert_eq!(extract_hash(&marker), Some("abcd1234"));
    }

    #[test]
    fn extract_invalid_marker() {
        assert_eq!(extract_hash("not a marker"), None);
        assert_eq!(extract_hash("<<ccr:nocomma>>"), Some("nocomma"));
        assert_eq!(extract_hash("<<other:hash>>"), None);
    }

    #[test]
    fn empty_store() {
        let store = InMemoryCcrStore::new();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn ccr_lossy_recoverable() {
        // 模拟 lossy 压缩: 把 100KB 压成 marker, 然后 retrieve 拉回
        let store = InMemoryCcrStore::new();
        let big_data = "x".repeat(102_400); // 100 KB
        let hash = store.put(&big_data);
        let marker = make_marker(&hash, big_data.len());
        // LLM 看到的只是 marker
        assert!(marker.len() < 50);
        // retrieve 拿回原值
        let entry = store.get(&hash).unwrap();
        assert_eq!(entry.data.len(), 102_400);
    }
}
