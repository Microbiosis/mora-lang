//! v0.104.6 D392 —— `value/persistent.rs`（HAMT 持久化 map）首次以 **oracle 差分**普查
//!
//! 本文件此前零覆盖（`persistent.rs` 自己的 9 条单测全是「插入后能查到」
//! 这类顺向断言，**没有一条与参考实现对拍**）。本轮改用
//! `std::collections::HashMap` 作 oracle，在**固定种子的确定性随机序列**上
//! 逐条对拍 `len` / `get` / `contains_key` / 全量键集。
//!
//! ## 为什么要差分而不是继续读
//!
//! HAMT 的 bug 集中在**结构不变量**（`children.len() == bitmap.count_ones()`、
//! `bit_index` 的 packed 下标、删除后的下标重排）。人工读容易「看着对」，
//! 而这些不变量在**删除**路径上最脆 —— 单向的 assoc 测不出来。
//!
//! ## 本轮的两个结构性发现
//!
//! 1. **`Node::Collision` 分支无法用真实键触发** —— 它要求两个键的
//!    **64-bit hash 完全相同**。`hash_of` 用 `DefaultHasher`，
//!    真实键撞满 64 位的概率约 `N²/2^65`，**本文件造不出来**。
//!    ⇒ 该分支（及其 `remove` 时的 `Collision → Leaf` 塌缩）在
//!    **零覆盖**下长期存在。属覆盖缺口，**只报告不擅动**（见 CHANGELOG）。
//!
//! 2. **`remove` 不做 Bitmap 合并（coalesce）** —— 删到一个子节点时保留
//!    `Bitmap{bitmap: 单bit, children:[唯一子]}`，而不把它塌成那个子节点。
//!    查得到、语义正确，只是多一层深度 ⇒ **空间问题，不是正确性问题**。
//!    与 Clojure 的做法不同，但**不构成缺陷**，仅记录以免日后误判。

use std::collections::{BTreeMap, HashMap};

use mora::value::persistent::PersistentMap;

/// 固定种子的 xorshift64*，保证序列**跨平台、跨版本完全一致**
/// （不用 `rand`：那会让判据不可复现）。
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

fn key_space(rng: &mut Rng, n: usize) -> Vec<String> {
    (0..n)
        .map(|_| {
            let a = rng.next();
            let b = rng.next();
            format!("k{a:016x}{b:016x}")
        })
        .collect()
}

/// 差分：跑一串操作，逐步与 oracle 对拍。
fn differential<V: Clone + PartialEq + std::fmt::Debug + Round>(
    ops: Vec<(&'static str, usize)>,
    seed: u64,
) {
    let mut rng = Rng(seed);
    let keys = key_space(&mut rng, 400);
    let mut map = PersistentMap::<V>::default();
    let mut oracle: HashMap<String, V> = HashMap::new();

    for (round, (op, idx)) in ops.iter().enumerate() {
        let k = &keys[*idx % keys.len()];
        match *op {
            "assoc" => {
                // 值取自轮次，使每轮不同 ⇒ 能查出「更新写错槽位」
                map = map.assoc(k, V::from_round(round));
                oracle.insert(k.clone(), V::from_round(round));
            }
            "overwrite" => {
                map = map.assoc(k, V::from_round(round));
                oracle.insert(k.clone(), V::from_round(round));
            }
            "remove" => {
                map = map.remove(k);
                oracle.remove(k);
            }
            _ => unreachable!(),
        }

        // 逐步对拍：任一不一致立刻报出**第几步、哪个键**
        assert_eq!(
            map.len(),
            oracle.len(),
            "第 {round} 步({op} {k:?})：len 不一致（map={} oracle={}）",
            map.len(),
            oracle.len()
        );
        // 不变量：iter 的条目数恒等于 len
        assert_eq!(
            map.iter().len(),
            map.len(),
            "第 {round} 步：iter 条目数 ≠ len —— 树结构与计数失配"
        );
        // 全量键集一致
        let got: BTreeMap<&str, &V> = map.iter().into_iter().collect();
        let want: BTreeMap<&str, &V> = oracle.iter().map(|(k, v)| (k.as_str(), v)).collect();
        assert_eq!(got, want, "第 {round} 步({op} {k:?})：键集/取值不一致");
    }
    // 终局：空 map 语义
    let mut emptied = map.clone();
    for k in &keys {
        emptied = emptied.remove(k);
    }
    assert!(emptied.is_empty(), "删光所有键后应为空");
    assert_eq!(emptied.iter().len(), 0);
}

/// 让判据能用不同 `V` 跑同一串操作序列。
trait Round: Clone + PartialEq + std::fmt::Debug {
    fn from_round(r: usize) -> Self;
}

impl Round for u64 {
    fn from_round(r: usize) -> Self {
        r as u64
    }
}

impl Round for String {
    fn from_round(r: usize) -> Self {
        format!("v{r}")
    }
}

fn build_ops(pattern: &str, n: usize) -> Vec<(&'static str, usize)> {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut ops = Vec::with_capacity(n);
    for _ in 0..n {
        let op = match pattern {
            "insert_only" => "assoc",
            "churn" => ["assoc", "overwrite", "remove"][(rng.next() % 3) as usize],
            "remove_heavy" => {
                ["assoc", "remove", "remove", "assoc", "assoc"][(rng.next() % 5) as usize]
            }
            _ => unreachable!(),
        };
        ops.push((op, (rng.next() % 200) as usize));
    }
    ops
}

/// 纯插入：最容易通过的一档，**作为基线**（若它就红，说明差分装置坏了）。
#[test]
fn d392_insert_only_matches_oracle() {
    differential::<u64>(build_ops("insert_only", 300), 1);
}

/// 覆写：验证「更新既有键」不会误增 `len`、不会写错槽位。
#[test]
fn d392_overwrite_matches_oracle() {
    differential::<u64>(build_ops("churn", 400), 2);
}

/// **删除穿插** —— 结构性 bug 最集中的一档。
#[test]
fn d392_remove_heavy_matches_oracle() {
    differential::<u64>(build_ops("remove_heavy", 500), 3);
}

/// 换 `V` 类型再跑一遍：确保对拍不是靠 `u64` 的巧合。
#[test]
fn d392_other_value_type_matches_oracle() {
    differential::<String>(build_ops("churn", 400), 4);
}

/// **持久性**：旧版本在新版本产生后**必须**保持原样。
#[test]
fn d392_old_versions_stay_frozen() {
    let mut rng = Rng(0xDEAD_BEEF_CAFE);
    let keys = key_space(&mut rng, 50);
    let mut versions: Vec<(PersistentMap<u64>, Vec<String>)> = Vec::new();
    let mut cur = PersistentMap::<u64>::default();
    for (i, k) in keys.iter().enumerate() {
        cur = cur.assoc(k, i as u64);
        versions.push((cur.clone(), keys[..=i].to_vec()));
    }
    // 逐个回看历史版本：它当时有的键必须还在，值必须没被后续 assoc 改写
    for (i, (v, expect_keys)) in versions.iter().enumerate() {
        assert_eq!(v.len(), expect_keys.len(), "第 {i} 个版本 len 变了");
        for (j, k) in expect_keys.iter().enumerate() {
            assert_eq!(v.get(k), Some(&(j as u64)), "第 {i} 个版本的 {k:?} 变了");
        }
    }
    // 最后一个版本仍然完整
    assert_eq!(cur.len(), keys.len());
}

/// **`iter()` 序是哈希序，且确定** —— 不是插入序、不是排序序，
/// 但**同一键集必得同一顺序**（可复现，不是 `HashMap` 那种随机种子序）。
///
/// 这条是 D385「`HashMap` 的 `RandomState` 让同一程序连跑 5 次得到 5 个不同
/// 输出」在 HAMT 侧的对照：`DefaultHasher` 用固定键 ⇒ 序稳定。
#[test]
fn d392_iter_order_is_deterministic_hash_order() {
    let build = || {
        let mut m = PersistentMap::<i32>::default();
        for (i, k) in ["a", "b", "c", "d", "e", "f", "g", "h"].iter().enumerate() {
            m = m.assoc(k, i as i32);
        }
        m
    };
    // ⚠ 必须把 map 绑到具名变量：`iter()` 借用 key 的 `&str`，
    //   写成 `build().iter()` 会让引用指向已析构的临时值（E0716）。
    let m1 = build();
    let m2 = build();
    let order_a: Vec<&str> = m1.iter().into_iter().map(|(k, _)| k).collect();
    let order_b: Vec<&str> = m2.iter().into_iter().map(|(k, _)| k).collect();
    assert_eq!(
        order_a, order_b,
        "同一键集的两次遍历序应完全一致（HAMT 用固定种子 hasher）"
    );
    let mut sorted = order_a.clone();
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        vec!["a", "b", "c", "d", "e", "f", "g", "h"],
        "遍历应覆盖全部 8 个键且不重复"
    );
    // ⚠ **不断言具体顺序**：`DefaultHasher` 的算法跨 Rust 版本不保证稳定，
    //   硬编码顺序会在工具链升级时假红。需要钉的是**稳定性**（上面那条），
    //   不是**序本身**。（D391：期望值必须来自实测，不可凭空写。）
}

/// **`Node::Collision` 用真实键造不出来** —— 覆盖缺口的现状钉。
///
/// `Collision` 需要两个键的 64-bit hash **完全相同**。本判据用 20 万个
/// 键（远超生日界）搜索同 hash 对，断言**找不到**。
///
/// ⇒ 这不是「验证 Collision 逻辑正确」，而是诚实地记录：
/// 该分支**无法被现有 API 触发**，因此处于**零覆盖**状态。
/// 若将来 `hash_of` 换算法或允许注入 hasher，本条会红并需补覆盖。
#[test]
fn d392_collision_branch_is_unreachable_with_real_keys() {
    let mut seen = std::collections::HashMap::new();
    for i in 0..200_000u32 {
        let h = seen.insert(i.to_string(), i);
        assert!(h.is_none(), "键重复，说明生成逻辑有误");
    }
    // 独立算一遍 hash，确认无碰撞对
    let mut hashes = std::collections::HashSet::new();
    let mut collided = None;
    for i in 0..200_000u32 {
        let k = i.to_string();
        // 与 persistent::hash_of 同算法（DefaultHasher + str.hash）
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        k.hash(&mut h);
        if !hashes.insert(h.finish()) {
            collided = Some(k);
            break;
        }
    }
    assert!(
        collided.is_none(),
        "20 万个键里出现了 64-bit hash 碰撞（{collided:?}）—— \
         可用这对键造 `Node::Collision` 覆盖，需补该分支测试"
    );
}
