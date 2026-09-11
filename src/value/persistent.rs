//! Persistent HAMT map（Clojure-style 32-way trie，Arc 路径复制）。
//!
//! 设计目标（v0.94 重写）：
//! - **不可变**：`assoc`/`remove` 返回新 map，旧值与旧 map 保持有效。
//! - **结构共享**：未修改的子树用 `Arc` 共享，`assoc` 只复制根到叶的路径，
//!   复杂度 O(log32 N)，`clone()` 仅做引用计数递增。
//! - **泛型**：`PersistentMap<V>` 可承载任意 `V`（`Environment` 用它存 `Value`，
//!   取代过去的每绑定 `Arc<Mutex<Value>>` —— 无内部可变性，天然可并发共享）。
//!
//! 这两条性质正是「用数据流代替状态机」的底层原语：环境是不可变值，
//! `assoc` 产生新版本，闭包捕获/并行 worker 拿到的是真正的快照而非共享锁。
//!
//! 实现参考：clojure/src/jvm/clojure/lang/PersistentHashMap.java::BitmapIndexedNode

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

const BITS: u32 = 5;
const MASK: u64 = (1 << BITS) - 1;
/// u64 有 64 位，每次消费 5 位；到 64 已无位可分，剩余只能是整 hash 碰撞。
const MAX_SHIFT: u32 = 64;

fn hash_of(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

fn bitpos(hash: u64, shift: u32) -> u32 {
    1u32 << ((hash >> shift) & MASK)
}

/// bitmap 中低 `bit` 之前的置位数 = packed children 的下标。
fn bit_index(bitmap: u32, bit: u32) -> usize {
    (bitmap & (bit - 1)).count_ones() as usize
}

/// HAMT 节点。未修改子树以 `Arc` 共享。
#[derive(Debug)]
enum Node<V> {
    /// 单条 entry。
    Leaf { hash: u64, key: String, val: V },
    /// bitmap 索引节点：`children.len() == bitmap.count_ones()`，按 bit 升序 packed。
    Bitmap { bitmap: u32, children: Vec<Arc<Node<V>>> },
    /// 整 hash 碰撞（同一 64-bit hash 的多条 entry）。
    Collision { hash: u64, entries: Vec<(String, V)> },
}

fn collect_entries<V: Clone>(node: &Node<V>, out: &mut Vec<(String, V)>) {
    match node {
        Node::Leaf { key, val, .. } => out.push((key.clone(), val.clone())),
        Node::Bitmap { children, .. } => {
            for c in children {
                collect_entries(c, out);
            }
        }
        Node::Collision { entries, .. } => out.extend(entries.iter().cloned()),
    }
}

/// 合并两棵「代表 hash」分别为 `a_hash`/`b_hash` 的树。用于插入时遇到不同 entry
/// 需要按 5-bit 分叉的场景。
fn merge_nodes<V: Clone>(
    shift: u32,
    a: Arc<Node<V>>,
    a_hash: u64,
    b: Arc<Node<V>>,
    b_hash: u64,
) -> Arc<Node<V>> {
    if shift >= MAX_SHIFT {
        // 已无位可分：整 hash 碰撞，合并 entries。
        let mut entries = Vec::new();
        collect_entries(&a, &mut entries);
        collect_entries(&b, &mut entries);
        return Arc::new(Node::Collision { hash: a_hash, entries });
    }
    let bp_a = bitpos(a_hash, shift);
    let bp_b = bitpos(b_hash, shift);
    if bp_a == bp_b {
        let child = merge_nodes(shift + BITS, a, a_hash, b, b_hash);
        Arc::new(Node::Bitmap { bitmap: bp_a, children: vec![child] })
    } else {
        let bitmap = bp_a | bp_b;
        let ia = bit_index(bitmap, bp_a);
        let ib = bit_index(bitmap, bp_b);
        let (first, second) = if ia < ib { (a, b) } else { (b, a) };
        Arc::new(Node::Bitmap { bitmap, children: vec![first, second] })
    }
}

/// 插入/更新，路径复制，返回新根。
fn assoc_node<V: Clone>(
    node: Option<&Arc<Node<V>>>,
    shift: u32,
    hash: u64,
    key: &str,
    val: V,
) -> Arc<Node<V>> {
    match node {
        None => Arc::new(Node::Leaf { hash, key: key.to_string(), val }),
        Some(n) => match &**n {
            Node::Leaf { hash: h, key: k, val: _ } => {
                if k == key {
                    Arc::new(Node::Leaf { hash, key: key.to_string(), val })
                } else {
                    let new_leaf = Arc::new(Node::Leaf { hash, key: key.to_string(), val });
                    merge_nodes(shift, Arc::clone(n), *h, new_leaf, hash)
                }
            }
            Node::Bitmap { bitmap, children } => {
                let bit = bitpos(hash, shift);
                if bitmap & bit != 0 {
                    let idx = bit_index(*bitmap, bit);
                    let child = assoc_node(Some(&children[idx]), shift + BITS, hash, key, val);
                    let mut new_children = children.clone();
                    new_children[idx] = child;
                    Arc::new(Node::Bitmap { bitmap: *bitmap, children: new_children })
                } else {
                    let idx = bit_index(*bitmap, bit);
                    let leaf = Arc::new(Node::Leaf { hash, key: key.to_string(), val });
                    let mut new_children: Vec<Arc<Node<V>>> =
                        Vec::with_capacity(children.len() + 1);
                    for (i, c) in children.iter().enumerate() {
                        if i == idx {
                            new_children.push(Arc::clone(&leaf));
                        }
                        new_children.push(Arc::clone(c));
                    }
                    if idx >= children.len() {
                        new_children.push(leaf);
                    }
                    Arc::new(Node::Bitmap { bitmap: *bitmap | bit, children: new_children })
                }
            }
            Node::Collision { hash: h, entries } => {
                if *h == hash {
                    let mut new_entries = entries.clone();
                    if let Some(slot) = new_entries.iter_mut().find(|(k, _)| k == key) {
                        slot.1 = val;
                    } else {
                        new_entries.push((key.to_string(), val));
                    }
                    Arc::new(Node::Collision { hash: *h, entries: new_entries })
                } else {
                    let new_leaf = Arc::new(Node::Leaf { hash, key: key.to_string(), val });
                    merge_nodes(shift, Arc::clone(n), *h, new_leaf, hash)
                }
            }
        },
    }
}

/// 删除，路径复制；返回 `None` 表示该子树被清空。
fn remove_node<V: Clone>(
    node: &Arc<Node<V>>,
    shift: u32,
    hash: u64,
    key: &str,
) -> Option<Arc<Node<V>>> {
    match &**node {
        Node::Leaf { key: k, .. } if k == key => None,
        Node::Leaf { .. } => Some(Arc::clone(node)),
        Node::Collision { hash: h, entries } if *h == hash => {
            let mut new_entries: Vec<(String, V)> =
                entries.iter().filter(|(k, _)| k != key).cloned().collect();
            match new_entries.len() {
                0 => None,
                1 => {
                    let (k, v) = new_entries.pop().expect("len==1 已保证非空");
                    Some(Arc::new(Node::Leaf { hash, key: k, val: v }))
                }
                _ => Some(Arc::new(Node::Collision { hash: *h, entries: new_entries })),
            }
        }
        Node::Collision { .. } => Some(Arc::clone(node)),
        Node::Bitmap { bitmap, children } => {
            let bit = bitpos(hash, shift);
            if bitmap & bit == 0 {
                return Some(Arc::clone(node));
            }
            let idx = bit_index(*bitmap, bit);
            match remove_node(&children[idx], shift + BITS, hash, key) {
                Some(new_child) => {
                    let mut new_children = children.clone();
                    new_children[idx] = new_child;
                    Some(Arc::new(Node::Bitmap { bitmap: *bitmap, children: new_children }))
                }
                None => {
                    let new_bitmap = bitmap & !bit;
                    if new_bitmap == 0 {
                        None
                    } else {
                        let new_children: Vec<Arc<Node<V>>> = children
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| *i != idx)
                            .map(|(_, c)| Arc::clone(c))
                            .collect();
                        Some(Arc::new(Node::Bitmap { bitmap: new_bitmap, children: new_children }))
                    }
                }
            }
        }
    }
}

fn get_node<'a, V>(node: &'a Node<V>, shift: u32, hash: u64, key: &str) -> Option<&'a V> {
    match node {
        Node::Leaf { key: k, val, .. } => {
            if k == key {
                Some(val)
            } else {
                None
            }
        }
        Node::Bitmap { bitmap, children } => {
            let bit = bitpos(hash, shift);
            if bitmap & bit == 0 {
                None
            } else {
                let idx = bit_index(*bitmap, bit);
                get_node(&children[idx], shift + BITS, hash, key)
            }
        }
        Node::Collision { hash: h, entries } => {
            if *h == hash {
                entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
            } else {
                None
            }
        }
    }
}

fn collect_refs<'a, V>(node: &'a Node<V>, out: &mut Vec<(&'a str, &'a V)>) {
    match node {
        Node::Leaf { key, val, .. } => out.push((key.as_str(), val)),
        Node::Bitmap { children, .. } => {
            for c in children {
                collect_refs(c, out);
            }
        }
        Node::Collision { entries, .. } => {
            for (k, v) in entries {
                out.push((k.as_str(), v));
            }
        }
    }
}

/// 持久化不可变 map：`assoc`/`remove` 返回新版本，旧版本保持有效。
#[derive(Debug)]
pub struct PersistentMap<V> {
    root: Option<Arc<Node<V>>>,
    len: usize,
}

impl<V> Default for PersistentMap<V> {
    fn default() -> Self {
        PersistentMap { root: None, len: 0 }
    }
}

// 手写 Clone：Arc 引用计数递增，不复制树。
impl<V> Clone for PersistentMap<V> {
    fn clone(&self) -> Self {
        PersistentMap { root: self.root.clone(), len: self.len }
    }
}

impl<V> PersistentMap<V> {
    pub fn new() -> Self {
        PersistentMap { root: None, len: 0 }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 不可变查找。
    pub fn get(&self, key: &str) -> Option<&V> {
        let hash = hash_of(key);
        self.root.as_ref().and_then(|r| get_node(r, 0, hash, key))
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
}

impl<V: Clone> PersistentMap<V> {
    /// 插入/更新，返回新版本。旧版本不受影响。
    pub fn assoc(&self, key: &str, val: V) -> Self {
        let hash = hash_of(key);
        let existed = self.contains_key(key);
        let root = assoc_node(self.root.as_ref(), 0, hash, key, val);
        PersistentMap { root: Some(root), len: if existed { self.len } else { self.len + 1 } }
    }

    /// 删除，返回新版本。key 不存在时返回与自身等值的浅拷贝。
    pub fn remove(&self, key: &str) -> Self {
        if !self.contains_key(key) {
            return self.clone();
        }
        let hash = hash_of(key);
        let root = self.root.as_ref().and_then(|r| remove_node(r, 0, hash, key));
        PersistentMap { root, len: self.len - 1 }
    }

    /// 遍历当前版本的所有 (key, value) 引用。
    pub fn iter(&self) -> Vec<(&str, &V)> {
        let mut out = Vec::with_capacity(self.len);
        if let Some(root) = &self.root {
            collect_refs(root, &mut out);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assoc_and_get() {
        let m = PersistentMap::<i32>::default();
        let m = m.assoc("a", 1);
        let m = m.assoc("b", 2);
        assert_eq!(m.get("a"), Some(&1));
        assert_eq!(m.get("b"), Some(&2));
        assert_eq!(m.get("c"), None);
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn old_version_stable_after_assoc() {
        let m0 = PersistentMap::<i32>::default();
        let m1 = m0.assoc("a", 1);
        let m2 = m1.assoc("a", 99);
        assert_eq!(m1.get("a"), Some(&1));
        assert_eq!(m2.get("a"), Some(&99));
        assert_eq!(m0.get("a"), None);
        assert_eq!(m1.len(), 1);
        assert_eq!(m2.len(), 1);
    }

    #[test]
    fn assoc_updates_existing_key_keeps_len() {
        let m = PersistentMap::<i32>::default();
        let m = m.assoc("a", 1);
        let m = m.assoc("a", 2);
        let m = m.assoc("a", 3);
        assert_eq!(m.get("a"), Some(&3));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn many_entries_roundtrip() {
        let mut m = PersistentMap::<String>::default();
        for i in 0..500u64 {
            m = m.assoc(&format!("key_{i}"), format!("val_{i}"));
        }
        assert_eq!(m.len(), 500);
        for i in 0..500u64 {
            assert_eq!(m.get(&format!("key_{i}")).map(String::as_str), Some(format!("val_{i}")).as_deref());
        }
        assert_eq!(m.get("missing"), None);
    }

    #[test]
    fn remove_restores_previous_version() {
        let m = PersistentMap::<i32>::default();
        let m = m.assoc("a", 1);
        let m = m.assoc("b", 2);
        let removed = m.remove("a");
        assert_eq!(m.get("a"), Some(&1), "旧版本不受 remove 影响");
        assert_eq!(removed.get("a"), None);
        assert_eq!(removed.get("b"), Some(&2));
        assert_eq!(removed.len(), 1);
    }

    #[test]
    fn remove_missing_is_noop() {
        let m = PersistentMap::<i32>::default().assoc("a", 1);
        let m2 = m.remove("nope");
        assert_eq!(m2.len(), 1);
        assert_eq!(m2.get("a"), Some(&1));
    }

    #[test]
    fn iter_visits_all_entries() {
        let m = PersistentMap::<i32>::default()
            .assoc("x", 10)
            .assoc("y", 20)
            .assoc("z", 30);
        let mut got: Vec<(&str, i32)> = m.iter().into_iter().map(|(k, v)| (k, *v)).collect();
        got.sort();
        assert_eq!(got, vec![("x", 10), ("y", 20), ("z", 30)]);
    }

    #[test]
    fn empty_map_operations() {
        let m = PersistentMap::<i32>::default();
        assert!(m.is_empty());
        assert_eq!(m.len(), 0);
        assert_eq!(m.get("anything"), None);
        assert!(m.iter().is_empty());
    }

    #[test]
    fn clone_shares_and_versions_diverge() {
        let base = PersistentMap::<i32>::default().assoc("a", 1);
        let cloned = base.clone();
        let diverged = cloned.assoc("b", 2);
        assert_eq!(base.get("b"), None, "分叉后原版本不含新键");
        assert_eq!(diverged.get("a"), Some(&1), "结构共享保留原有键");
        assert_eq!(diverged.get("b"), Some(&2));
    }

    #[test]
    fn high_collision_volume_stays_correct() {
        // 大量键：覆写 + 删除穿插，验证路径复制与计数一致。
        let mut m = PersistentMap::<u32>::default();
        for i in 0..300u32 {
            m = m.assoc(&format!("k{i}"), i);
        }
        for i in (0..300u32).step_by(3) {
            m = m.remove(&format!("k{i}"));
        }
        assert_eq!(m.len(), 200);
        for i in 0..300u32 {
            if i % 3 == 0 {
                assert_eq!(m.get(&format!("k{i}")), None);
            } else {
                assert_eq!(m.get(&format!("k{i}")), Some(&i));
            }
        }
    }
}
