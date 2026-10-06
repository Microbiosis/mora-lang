//! v0.104.6 D410 —— `src/runtime/` 首次整模块外部覆盖（否定轮 + 1 处修复）
//!
//! ## 本轮范围
//!
//! `src/runtime/` 12 个文件（Kernel Services 层）。此前只被沙箱相关判据
//! 从旁碰到，**从未做过整模块外部覆盖**。
//!
//! 模块内单测覆盖普查（2026-10-06）：
//!
//! | 文件 | 行数 | 模块内单测 |
//! |---|---|---|
//! | ai.rs / ai_infra.rs / core.rs / effect.rs / infra.rs | 130 / 208 / 94 / 133 / 124 | 10 / 6 / 4 / 3 / 7 |
//! | orch.rs / persist.rs / random.rs / registry.rs / sandbox.rs | 109 / 102 / 260 / 107 / 170 | 6 / 5 / 6 / 7 / 9 |
//! | **types.rs** | **108** | **0** ← 唯一缺口 |
//!
//! ⇒ 覆盖缺口与本轮发现的缺陷**重合**：`types.rs`。
//!
//! ## 本轮唯一缺陷：`LruCache::put` 让 `cap = 0` 的缓存存进 1 条
//!
//! 修前淘汰判据是 `self.map.len() >= self.cap`，`cap = 0` 时**恒真**，
//! 于是走进淘汰分支 —— 但此刻 `order` 为空，`pop_front()` 返回 `None`
//! ⇒ **什么都没淘汰**；紧接着无条件 `push_back` + `insert`
//! ⇒ 条目被存了进去。
//!
//! 实测：`LruCache::new(0)` put 一条后 `len() == 1` 而 `cap() == 0`
//! ⇒ `len() > cap()`，**违反本类型自己 doc 承诺的「cap 上限」**。
//!
//! 修法：`cap == 0` 直接返回 —— 容量 0 的语义只能是「不缓存」。
//!
//! ⚠ **不关闭任何当前可观测的洞**：生产两处容量是
//! `STRING_INTERNER_CAPACITY = 50_000` 与 `AI_CACHE_CAPACITY = 10_000`，
//! 都 > 0。修的是 `pub` API 自身契约的不一致。
//!
//! ## 裁决依据（D404 纪律：两个检索都做过）
//!
//! ① 符号附近注释：doc 写「`cap` 上限, 超过 evict 最旧」，即 cap 是**上界**；
//!    全文无任何注释说「cap=0 存 1 条」。
//! ② 全仓判据：`tests/` 下搜 `LruCache` **零命中**；`src/stress_tests.rs`
//!    的三处 cap 是 1000 / 10000 / 50000，无一为 0。
//!
//! ⇒ 两项检索均未命中「已记录的决定」⇒ 可修。

use mora::interpreter::LruCache;

// ── ① 基本语义（对照组：确认装置本身能工作） ──

/// **标准 LRU 语义**：超容量淘汰最旧；`get` 命中会**刷新**访问顺序。
#[test]
fn d410_lru_basic_lru_semantics() {
    let mut c = LruCache::<i32>::new(3);
    c.put("a".into(), 1);
    c.put("b".into(), 2);
    c.put("c".into(), 3);
    assert_eq!(c.len(), 3, "未超容量时三条都在");
    assert_eq!(c.get("a"), Some(1), "命中应返回值");

    // `get("a")` 把 a 刷新成最新 ⇒ 此时最旧的是 b
    c.put("d".into(), 4);
    assert_eq!(c.len(), 3, "超容量后应仍为 3");
    assert_eq!(c.get("b"), None, "`b` 最久未用，应被淘汰");
    assert_eq!(c.get("a"), Some(1), "`a` 刚被 get 刷新，不该被淘汰");
    assert_eq!(c.get("c"), Some(3));
    assert_eq!(c.get("d"), Some(4));
}

/// **「LRU」与「FIFO」必须可区分** —— 否则上面的刷新逻辑其实没生效。
#[test]
fn d410_lru_is_lru_not_fifo() {
    let mut c = LruCache::<i32>::new(2);
    c.put("a".into(), 1);
    c.put("b".into(), 2);
    c.get("a"); // 刷新 a
    c.put("c".into(), 3);
    // LRU ⇒ 淘汰 b（a 被刷新过）；FIFO ⇒ 淘汰 a
    assert_eq!(c.get("b"), None, "应淘汰最久未访问的 b（LRU）");
    assert_eq!(c.get("a"), Some(1), "a 被 get 刷新过，应保留");
}

/// **重复 put 同一 key 只更新、不增长，且刷新顺序**。
#[test]
fn d410_lru_put_existing_key_updates_in_place() {
    let mut c = LruCache::<i32>::new(2);
    c.put("a".into(), 1);
    c.put("b".into(), 2);
    c.put("a".into(), 20); // 更新 a，并把它刷新成最新
    assert_eq!(c.len(), 2, "同 key 重复 put 不应增长");
    assert_eq!(c.get("a"), Some(20), "应保留新值");

    c.put("c".into(), 3); // 淘汰最旧 = b（a 刚被刷新）
    assert_eq!(c.get("b"), None, "b 最久未用，应被淘汰");
    assert_eq!(c.get("a"), Some(20));
}

// ── ② 核心不变式：`len() <= cap()`（本轮修复点） ──

/// **`len()` 永远不得超过 `cap()`** —— 类型自己 doc 承诺的「cap 上限」。
///
/// 这是本轮的**核心判据**，牙齿验证目标。
#[test]
fn d410_lru_len_never_exceeds_cap() {
    for cap in [0usize, 1, 2, 5] {
        let mut c = LruCache::<i32>::new(cap);
        for i in 0..20 {
            c.put(format!("k{i}"), i);
            assert!(
                c.len() <= cap,
                "cap={cap} 时 put #{i} 后 len={} 超过 cap={cap}",
                c.len()
            );
        }
    }
}

/// **容量 0 的缓存什么都不存**（`get` / `len` / `is_empty` 三者自洽）。
#[test]
fn d410_lru_zero_capacity_stores_nothing() {
    let mut c = LruCache::<i32>::new(0);
    assert_eq!(c.cap(), 0);
    c.put("a".into(), 1);
    assert_eq!(c.len(), 0, "cap=0 时不应存下任何条目");
    assert!(c.is_empty(), "cap=0 时应始终为空");
    assert_eq!(c.get("a"), None, "cap=0 时 get 必为 None");
}

// ── ③ 差分测试：对照参考实现（D392 做法） ──

/// 拿一个**朴素但显然正确**的参考 LRU 做随机差分。
///
/// 单条条目的测试容易漏掉交互（更新 + 刷新 + 淘汰的组合），
/// 差分能一次性覆盖。`rand` 是本仓已有依赖。
#[test]
fn d410_lru_matches_reference_implementation() {
    /// 参考实现：用 `Vec` 存 (key, value)，从头找 —— 慢但显然正确。
    /// recency: 0 = 最旧，末尾 = 最新。
    struct Ref {
        items: Vec<(String, i32)>, // 最旧在前
    }
    impl Ref {
        fn new() -> Self {
            Ref { items: Vec::new() }
        }
        fn touch(&mut self, key: &str) -> Option<i32> {
            let pos = self.items.iter().position(|(k, _)| k == key)?;
            let (_, v) = self.items.remove(pos);
            self.items.push((key.to_string(), v));
            Some(v)
        }
        fn put(&mut self, key: String, value: i32, cap: usize) {
            if cap == 0 {
                return;
            }
            if let Some(pos) = self.items.iter().position(|(k, _)| *k == key) {
                self.items.remove(pos);
            } else if self.items.len() >= cap {
                self.items.remove(0);
            }
            self.items.push((key, value));
        }
    }

    // 固定种子 + 小键空间 ⇒ 大量 key 重复 ⇒ 覆盖「更新 + 刷新 + 淘汰」组合。
    let mut state: u64 = 0x5DEECE66D;
    let mut next = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) as u32
    };

    for cap in [1usize, 2, 3, 7] {
        let mut real = LruCache::<i32>::new(cap);
        let mut oracle = Ref::new();
        for step in 0..400 {
            let key = format!("k{}", next() % 5); // 5 个键 vs cap ≤ 7 ⇒ 必触发淘汰
            if next() % 3 == 0 {
                // get 分支
                let a = real.get(&key);
                let b = oracle.touch(&key);
                assert_eq!(a, b, "cap={cap} step={step} get({key}) 分歧");
            } else {
                // put 分支
                let v = (next() % 1000) as i32;
                real.put(key.clone(), v);
                oracle.put(key.clone(), v, cap);
            }
            assert_eq!(
                real.len(),
                oracle.items.len(),
                "cap={cap} step={step} len 分歧: real={} oracle={}",
                real.len(),
                oracle.items.len()
            );
            assert!(real.len() <= cap, "cap={cap} step={step} 越界");
        }
    }
}
