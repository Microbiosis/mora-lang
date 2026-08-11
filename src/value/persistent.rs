//! v0.78: HAMT-based persistent map (Clojure-style 32-way trie)。
//!
//! 设计目标：
//! - 不可变 `assoc` 返回新 HAMT，旧引用保持有效
//! - O(log32 N) ≈ 1 hop for N < 10M entries
//! - 5 bits per trie level
//!
//! 实现参考：clojure/src/jvm/clojure/lang/PersistentHashMap.java::BitmapIndexedNode
//!
//! v0.78 范围：单泛型参数 K = String（覆盖 Mora Environment 的核心场景）；
//! 后续 stage 扩展为多类型 key（用 phantom type 或 hash 适配）。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

const BITS: u32 = 5;
const MASK: u32 = (1 << BITS) - 1;
const MAX_DEPTH: u32 = u64::BITS / BITS;

fn hash_of(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

/// HAMT 节点（参考 PersistentHashMap.BitmapIndexedNode）。
#[derive(Debug, Clone)]
pub enum HamtNode {
    Empty,
    /// 单 entry：key (String) + value (V)
    Leaf(String, u64), // value 存 u64 hash（HAMT 导航用）
    /// BitmapIndexedNode：bitmap 标记 slot 使用，arr 是 packed (key, value)
    Bitmap(u32, Vec<HamtNode>),
    /// HashCollisionNode：哈希碰撞的链表
    Collision(u64, Vec<(String, u64)>),
}

impl Default for HamtNode {
    #[allow(clippy::derivable_impls)]
    fn default() -> Self {
        HamtNode::Empty
    }
}

/// HamtNode<V=String, K=u64-hash> typed wrapper.
pub type HamtMap = HamtNode;

impl HamtNode {
    fn mask(hash: u64, shift: u32) -> u32 {
        ((hash >> shift) & MASK as u64) as u32
    }
    fn bitpos(hash: u64, shift: u32) -> u32 {
        1 << Self::mask(hash, shift)
    }
    fn index(bitmap: u32, bit: u32) -> usize {
        (bitmap & (bit - 1)).count_ones() as usize
    }
    fn packed_len(bitmap: u32) -> usize {
        bitmap.count_ones() as usize
    }

    /// `assoc`: insert (key, val). Returns new node.
    pub fn assoc(&self, shift: u32, hash: u64, key: &str, val: u64) -> Self {
        match self {
            HamtNode::Empty => HamtNode::Leaf(key.to_string(), val),

            HamtNode::Leaf(k, v) => {
                if k == key {
                    HamtNode::Leaf(key.to_string(), val)
                } else {
                    // split: h1 来自旧 leaf 的 hash 重新计算（k 是 String）
                    let h1 = hash_of(k);
                    Self::assoc_two(shift, h1, k.clone(), *v, hash, key.to_string(), val)
                }
            }

            HamtNode::Bitmap(bmp, arr) => {
                let bit = Self::bitpos(hash, shift);
                let idx = Self::index(*bmp, bit);
                let packed_len = Self::packed_len(*bmp);

                if *bmp & bit != 0 {
                    let child_idx = idx * 2;
                    let new_child = arr[child_idx].assoc(shift + BITS, hash, key, val);
                    let mut new_arr = arr.clone();
                    new_arr[child_idx] = new_child;
                    HamtNode::Bitmap(*bmp, new_arr)
                } else {
                    let new_packed_len = packed_len + 1;
                    let mut new_arr: Vec<HamtNode> = Vec::with_capacity(new_packed_len * 2);
                    let insert_pos = idx * 2;
                    let mut inserted = false;
                    for (i, item) in arr.iter().enumerate() {
                        if i == insert_pos && !inserted {
                            new_arr.push(HamtNode::Leaf(key.to_string(), val));
                            new_arr.push(HamtNode::Empty);
                            inserted = true;
                        }
                        new_arr.push(item.clone());
                    }
                    if !inserted {
                        new_arr.push(HamtNode::Leaf(key.to_string(), val));
                        new_arr.push(HamtNode::Empty);
                    }
                    let new_bmp = *bmp | bit;
                    HamtNode::Bitmap(new_bmp, new_arr)
                }
            }

            HamtNode::Collision(_, kv) => {
                let mut new_kv: Vec<(String, u64)> = kv.clone();
                let mut found = false;
                for entry in new_kv.iter_mut() {
                    if entry.0 == key {
                        entry.1 = val;
                        found = true;
                        break;
                    }
                }
                if !found {
                    new_kv.push((key.to_string(), val));
                }
                HamtNode::Collision(hash, new_kv)
            }
        }
    }

    fn assoc_two(
        shift: u32,
        h1: u64,
        k1: String,
        v1: u64,
        h2: u64,
        k2: String,
        v2: u64,
    ) -> Self {
        if shift >= MAX_DEPTH * BITS {
            panic!("HAMT depth exhausted")
        }
        let bp1 = Self::bitpos(h1, shift);
        let bp2 = Self::bitpos(h2, shift);

        if bp1 == bp2 {
            let child = Self::assoc_two(shift + BITS, h1, k1, v1, h2, k2, v2);
            HamtNode::Bitmap(bp1, vec![child])
        } else {
            let bmp = bp1 | bp2;
            let packed_len = Self::packed_len(bmp);
            let mut arr: Vec<HamtNode> = Vec::with_capacity(packed_len * 2);
            arr.resize(packed_len * 2, HamtNode::Empty);
            let idx1 = Self::index(bmp, bp1);
            let idx2 = Self::index(bmp, bp2);
            arr[idx1 * 2] = HamtNode::Leaf(k1, v1);
            arr[idx2 * 2] = HamtNode::Leaf(k2, v2);
            HamtNode::Bitmap(bmp, arr)
        }
    }

    pub fn get(&self, shift: u32, hash: u64, key: &str) -> Option<u64> {
        match self {
            HamtNode::Empty => None,
            HamtNode::Leaf(k, v) => {
                if k == key {
                    Some(*v)
                } else {
                    None
                }
            }
            HamtNode::Bitmap(bmp, arr) => {
                let bit = Self::bitpos(hash, shift);
                if *bmp & bit == 0 {
                    None
                } else {
                    let idx = Self::index(*bmp, bit);
                    arr[idx * 2].get(shift + BITS, hash, key)
                }
            }
            HamtNode::Collision(_, kv) => kv
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| *v),
        }
    }

    pub fn size(&self) -> usize {
        match self {
            HamtNode::Empty => 0,
            HamtNode::Leaf(_, _) => 1,
            HamtNode::Bitmap(_, arr) => arr.iter().map(|n| n.size()).sum(),
            HamtNode::Collision(_, kv) => kv.len(),
        }
    }
}

/// HAMT-based persistent map（u64 → value，仅为 stage 1 测试）。
#[derive(Debug, Clone, Default)]
pub struct PersistentMap {
    root: HamtNode,
    size: usize,
}

impl PersistentMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// `assoc`: insert key → val. Returns new map.
    pub fn assoc(&self, key: &str, val: u64) -> Self {
        let key_hash = hash_of(key);
        let prev = self.root.get(0, key_hash, key);
        let new_root = self.root.assoc(0, key_hash, key, val);
        let new_size = if prev.is_some() { self.size } else { self.size + 1 };
        PersistentMap { root: new_root, size: new_size }
    }

    pub fn get(&self, key: &str) -> Option<u64> {
        let key_hash = hash_of(key);
        self.root.get(0, key_hash, key)
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn is_empty(&self) -> bool {
        self.size == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assoc_and_get() {
        let m = PersistentMap::default();
        let m = m.assoc("a", 1);
        let m = m.assoc("b", 2);
        assert_eq!(m.get("a"), Some(1));
        assert_eq!(m.get("b"), Some(2));
        assert_eq!(m.get("c"), None);
        assert_eq!(m.size(), 2);
    }

    #[test]
    fn persistent_old_version_stable() {
        let m = PersistentMap::default();
        let m1 = m.assoc("a", 1);
        let m2 = m1.assoc("a", 99);
        assert_eq!(m1.get("a"), Some(1));
        assert_eq!(m2.get("a"), Some(99));
        assert_eq!(m1.size(), 1);
        assert_eq!(m2.size(), 1);
    }

    #[test]
    fn assoc_updates_existing_key() {
        let m = PersistentMap::default();
        let m = m.assoc("a", 1);
        let m = m.assoc("a", 2);
        let m = m.assoc("a", 3);
        assert_eq!(m.get("a"), Some(3));
        assert_eq!(m.size(), 1);
    }

    #[test]
    fn many_entries() {
        let mut m = PersistentMap::default();
        for i in 0..100u64 {
            let key = format!("key_{}", i);
            m = m.assoc(&key, i);
        }
        assert_eq!(m.size(), 100);
        for i in 0..100u64 {
            let key = format!("key_{}", i);
            assert_eq!(m.get(&key), Some(i));
        }
    }

    #[test]
    fn empty_map_operations() {
        let m = PersistentMap::default();
        assert!(m.is_empty());
        assert_eq!(m.size(), 0);
        assert_eq!(m.get("anything"), None);
    }

    #[test]
    fn hamt_node_size_recursive() {
        let n = HamtNode::Empty;
        assert_eq!(n.size(), 0);
        let n = HamtNode::Leaf("k".to_string(), 42);
        assert_eq!(n.size(), 1);
        let n = HamtNode::Bitmap(
            0b101,
            vec![
                HamtNode::Leaf("a".to_string(), 1),
                HamtNode::Empty,
                HamtNode::Leaf("b".to_string(), 2),
                HamtNode::Empty,
            ],
        );
        assert_eq!(n.size(), 2);
    }
}