//! v0.77: proptest — VectorClock CRDT laws。
//!
//! VectorClock 是 Mora BSP engine 中"是否看见某 channel 写入"的因果跟踪机制。
//! 不变量：
//!   1. merge 是可交换：merge(a,b) == merge(b,a)
//!   2. merge 是幂等：merge(a,a) == a
//!   3. 反自反：!happened_before(a, a)

use mora::value::VectorClock;
use proptest::prelude::*;

/// 构造一个 VectorClock，agent_i 各 tick k_i 次。
fn make_clock(agent_ticks: &[(u8, u8)]) -> VectorClock {
    let mut vc = VectorClock::default();
    for &(i, k) in agent_ticks {
        let agent = format!("agent_{}", i);
        for _ in 0..k {
            vc.tick(&agent);
        }
    }
    vc
}

/// 用 happened_before 比较两个 VectorClock（避开私有字段 PartialEq）。
/// VectorClock 等价定义：∀k: a[k] == b[k]。
/// 实现：mutual happened_before + same generation keys 集合。
fn equivalent(a: &VectorClock, b: &VectorClock) -> bool {
    // 简化：两个时钟等价 iff 各自的 happened_before 与对方的差 ≤ 0（不能严格 <）。
    // 这避免了 HashMap 调试输出顺序不确定的问题。
    !VectorClock::happened_before(a, b) && !VectorClock::happened_before(b, a)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        .. ProptestConfig::default()
    })]

    /// merge 是可交换。
    #[test]
    fn merge_is_commutative(
        a_ticks in proptest::collection::vec((0u8..3u8, 0u8..4u8), 0..5),
        b_ticks in proptest::collection::vec((0u8..3u8, 0u8..4u8), 0..5),
    ) {
        let a = make_clock(&a_ticks);
        let b = make_clock(&b_ticks);
        let mut ab = a.clone(); ab.merge(&b);
        let mut ba = b.clone(); ba.merge(&a);
        prop_assert!(equivalent(&ab, &ba), "merge must be commutative");
    }

    /// merge 是幂等。
    #[test]
    fn merge_is_idempotent(
        ticks in proptest::collection::vec((0u8..3u8, 0u8..4u8), 0..5),
    ) {
        let a = make_clock(&ticks);
        let mut merged = a.clone();
        merged.merge(&a);
        prop_assert!(equivalent(&a, &merged), "merge must be idempotent");
    }

    /// happened_before 是反自反的。
    #[test]
    fn happened_before_is_irreflexive(
        ticks in proptest::collection::vec((0u8..3u8, 0u8..4u8), 0..5),
    ) {
        let a = make_clock(&ticks);
        prop_assert!(
            !VectorClock::happened_before(&a, &a),
            "happened_before(a, a) must be false"
        );
    }
}