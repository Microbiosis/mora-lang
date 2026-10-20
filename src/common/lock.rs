//! 锁中毒（poison）处理策略 —— 全仓唯一的锁获取入口约定。
//!
//! 锁中毒只说明「某个持锁线程 panic 了」，**不说明**被保护的状态还可不可信。
//! 是否可恢复取决于状态的**用途**，与它住在哪个模块无关（见 AGENTS.md §7：
//! 按「做什么」分类，不按「叫什么」分类）。因此本仓把锁分成两类：
//!
//! **A 类 · 遥测（telemetry）** —— trace collector、事件总线、mock 注册表、
//! AI 缓存。这些状态的唯一职责是「记录」。让*被记录的程序*因为
//! *记录器* panic 而死，是把观测手段升级成了可用性单点。→ [`lock_or_recover`]
//! 恢复 guard 并计数。
//!
//! **B 类 · 完整性状态（integrity）** —— 审计 hash 链、存活容器句柄、
//! 信号量。这类状态的**一致性本身是承重的**：中断在 `write()` 中途的
//! 审计日志，hash 链已经断裂，之后静默继续会产出「验证通过」的假结论
//! （`src/audit/mod.rs` 在 `verify_chain` 处已就 sync_all 失败写过同一条论证）。
//! 静默恢复在这里是**安全回归**，不是可用性改进。→ B 类保持显式失败，但
//! 调用路径已有 `Result` 的走类型化错误（`AuditError::Poisoned`），
//! 没有的才 panic，且 panic 文案必须**点名被破坏的不变量**，
//! 不能只写 "poisoned"。
//!
//! 为什么 A 类的降级记在这里而不是走审计 sink / 事件总线：那两者自己
//! 就持有 B 类锁，从锁恢复路径回调进它们会构成重入。计数因此是一个
//! **叶子**结构 —— 临界区里只有一次整数自增，不跑任何用户代码。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// 累计降级次数（全部子系统合计）。
static RECOVERIES: AtomicU64 = AtomicU64::new(0);

/// 按子系统拆分的降级次数。
///
/// 自身被毒化时可以安全恢复：唯一的写入路径是 `record_degradation`，
/// 临界区内只做一次 `BTreeMap` 自增，不执行用户代码、不分配 guard 状态。
static BY_SUBSYSTEM: Mutex<BTreeMap<&'static str, u64>> = Mutex::new(BTreeMap::new());

fn record_degradation(subsystem: &'static str) {
    RECOVERIES.fetch_add(1, Ordering::Relaxed);
    let mut map = BY_SUBSYSTEM
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *map.entry(subsystem).or_insert(0) += 1;
}

/// A 类 · `Mutex`：中毒时恢复 guard 并计数。
///
/// 恢复的前提是：守卫的数据允许「少记一条」或「读到一个略旧的值」。
/// 完整性状态不要用这里 —— 见模块文档的 B 类说明。
pub fn lock_or_recover<'a, T>(mutex: &'a Mutex<T>, subsystem: &'static str) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        record_degradation(subsystem);
        poisoned.into_inner()
    })
}

/// A 类 · `RwLock` 读锁：中毒时恢复 guard 并计数。
pub fn read_or_recover<'a, T>(
    lock: &'a RwLock<T>,
    subsystem: &'static str,
) -> RwLockReadGuard<'a, T> {
    lock.read().unwrap_or_else(|poisoned| {
        record_degradation(subsystem);
        poisoned.into_inner()
    })
}

/// A 类 · `RwLock` 写锁：中毒时恢复 guard 并计数。
pub fn write_or_recover<'a, T>(
    lock: &'a RwLock<T>,
    subsystem: &'static str,
) -> RwLockWriteGuard<'a, T> {
    lock.write().unwrap_or_else(|poisoned| {
        record_degradation(subsystem);
        poisoned.into_inner()
    })
}

/// 累计降级次数（全部子系统合计）。无降级时为 0。
pub fn recovery_count() -> u64 {
    RECOVERIES.load(Ordering::Relaxed)
}

/// 按子系统拆分的降级次数，按名称排序。
///
/// 零降级时返回空 vec。
pub fn recovery_breakdown() -> Vec<(&'static str, u64)> {
    let map = BY_SUBSYSTEM
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    map.iter().map(|(k, v)| (*k, *v)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// 这些计数器是进程级全局量，`cargo test` 下各测试并发运行。
    /// 因此每个测试只断言**自己独占的子系统名**下的计数，
    /// 不对全局总量做差分断言 —— 那样必然被并发写入搅成 flake。
    fn count_for(subsystem: &str) -> u64 {
        recovery_breakdown()
            .into_iter()
            .find(|(name, _)| *name == subsystem)
            .map(|(_, count)| count)
            .unwrap_or(0)
    }

    fn poison(mutex: &Arc<Mutex<Vec<u32>>>) {
        let inner = Arc::clone(mutex);
        let _ = std::thread::spawn(move || {
            let _guard = inner.lock().unwrap();
            panic!("poison the mutex");
        })
        .join();
        assert!(mutex.is_poisoned(), "前置条件：mutex 应处于中毒态");
    }

    #[test]
    fn clean_lock_does_not_record_degradation() {
        let m = Mutex::new(vec![1u32]);
        {
            let mut g = lock_or_recover(&m, "test.clean");
            g.push(2);
        }
        assert_eq!(*m.lock().unwrap(), vec![1, 2]);
        assert_eq!(count_for("test.clean"), 0, "未中毒时不应产生降级记录");
    }

    #[test]
    fn poisoned_mutex_is_recovered_and_counted() {
        let m = Arc::new(Mutex::new(vec![1u32, 2, 3]));
        poison(&m);

        {
            let mut g = lock_or_recover(&m, "test.poisoned_mutex");
            g.push(4);
        }

        assert_eq!(
            *lock_or_recover(&m, "test.poisoned_mutex"),
            vec![1, 2, 3, 4],
            "恢复后的 guard 必须可用"
        );
        assert_eq!(
            count_for("test.poisoned_mutex"),
            2,
            "写一次 + 断言回读一次 = 两次降级"
        );
    }

    #[test]
    fn poisoned_rwlock_read_and_write_are_recovered() {
        let rw = Arc::new(RwLock::new(vec![7u32]));
        let _ = std::thread::spawn({
            let rw = Arc::clone(&rw);
            move || {
                let _guard = rw.write().unwrap();
                panic!("poison the rwlock");
            }
        })
        .join();
        assert!(rw.is_poisoned(), "前置条件：rwlock 应处于中毒态");

        assert_eq!(*read_or_recover(&rw, "test.poisoned_rwlock"), vec![7]);
        write_or_recover(&rw, "test.poisoned_rwlock").push(8);
        assert_eq!(*read_or_recover(&rw, "test.poisoned_rwlock"), vec![7, 8]);

        assert_eq!(count_for("test.poisoned_rwlock"), 3, "读一次写一次读一次");
    }

    #[test]
    fn breakdown_separates_subsystems_and_is_sorted() {
        let m = Arc::new(Mutex::new(Vec::<u32>::new()));
        poison(&m);

        drop(lock_or_recover(&m, "test.breakdown.alpha"));
        drop(lock_or_recover(&m, "test.breakdown.alpha"));
        drop(lock_or_recover(&m, "test.breakdown.beta"));

        assert_eq!(count_for("test.breakdown.alpha"), 2);
        assert_eq!(count_for("test.breakdown.beta"), 1);

        let names: Vec<&str> = recovery_breakdown().iter().map(|(n, _)| *n).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "breakdown 应按子系统名排序");
    }

    #[test]
    fn total_is_never_below_the_breakdown_sum() {
        // record_degradation 先自增总量再更新分表，故任意快照下
        // 总量 >= 分表之和（并发交错只会让差距更大，不会反过来）。
        let sum: u64 = recovery_breakdown().iter().map(|(_, c)| *c).sum();
        assert!(
            recovery_count() >= sum,
            "总量 {} 不应低于分表之和 {}",
            recovery_count(),
            sum
        );
    }
}
