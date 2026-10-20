//! v0.75.9: MirFunction → 优化后 DAG 的跨调用缓存。
//!
//! `run_mir_with_signal` 此前每次调用全量重建 DAG（`dag_analyze` +
//! `dag_optimize` + `prune_sequence_edges`）。同一函数体多次调用
//! （Closure/Task/循环内 WithConfig/pregel 每超步 agent）重复重建，
//! 分析开销与分配都无谓。
//!
//! 缓存 key 有两级（v0.103 起）：
//! 1. **指针 key**（快路径）：`Arc<MirFunction>` 地址 —— 同一 Arc 即同一内容，
//!    O(1) 命中。
//! 2. **内容指纹 key**（等价路径）：`params + n_regs + body Debug 表示` 的
//!    哈希 —— 不同 Arc 包裹同一函数体时命中同一 DAG。
//!
//! 两级表容量各为 `MAX_ENTRIES`，超出时**淘汰最久未用的一项**（LRU）。
//! 此前是「超出即 `clear()` 整表清空」—— 工作集一旦超过 128，每插入一项
//! 就丢掉全部 128 项，命中率崩到接近 0。MCP 长会话（`Interpreter` 会话级
//! 存活，`vm/engine.rs` 的热路径每次工具调用都查缓存）必然踩到这个悬崖。
//!
//! v0.103 修复：此前只有指针 key，且明确写下「不同 Arc 包裹同一内容则各自
//! 独立构建 — 内容相等性不在缓存契约内」。但项目内多处每轮新建
//! `Arc::new((*body).clone())`（`h_call` plan 路径、`h_transaction`、
//! `Worker` 执行、`h_parallel` 的段执行等），使该「契约」在实践中退化为
//! 「循环内每轮重建 DAG + `MAX_ENTRIES` 反复清空」—— worker 内 task 含
//! 大循环时直接卡死（实测 n=500 不返回）。内容 key 恢复了缓存的本来
//! 目的（同一函数体不重复分析），并使容量淘汰分表进行（清指针表不牺牲
//! 内容表）。
//!
//! v0.75.11: 指针缓存项**同时持有 `Arc<MirFunction>` 强引用**（二元组
//! `(Arc<MirFunction>, Arc<MirDag>)`）。此前只存 dag，key 指针在 func_arc
//! drop 后会被 allocator 复用 → 不同函数撞同地址 → 命中错误 DAG（pregel
//! 并行单元测试全量并发时暴露：`Const(42)` 的 body 被 `Const(10)` 的调用
//! 命中）。持有强引用后指针永不复用，同指针必然同内容。
//!
//! v1.00 数据流化：缓存从进程级 `static OnceLock<DagCache>`（内部
//! `Mutex<HashMap>` 防御式加锁）迁为 **MirHost 单属主纯值** —— 缓存的
//! 计算是对同一 `Arc<MirFunction>` 确定的纯函数，单属主 `&mut self`
//! 线性访问无需锁；Clone 按值复制（memo 透明：命中与否只影响耗时，
//! 不影响结果）—— Pregel worker 克隆继承 master 预热条目后独立演化，
//! 并发从「共享一把进程级锁」变成「值拷贝即隔离」。全局单例
//! `DAG_CACHE` / `global_dag_cache()` 已删除（无兼容层）。

use std::collections::HashMap;
use std::sync::Arc;

use super::MirFunction;
use super::dag::{MirDag, dag_analyze};
use super::optimize::dag_optimize;

/// 缓存容量上限：超过则淘汰最久未用的一项（防止长生命周期进程无限增长）。
const MAX_ENTRIES: usize = 128;

/// 带容量上限的映射，淘汰**最久未用**的一项。
///
/// **不使用 `runtime::types::LruCache`**：它的 key 是 `String`，且刷新访问
/// 顺序用的是 `order.iter().position()`（线性扫描）。本缓存的 key 是指针
/// 整数 / 内容哈希，复用它就得为每次查找 `to_string()` 一次分配 —— 那
/// 是**每次 DAG 查找**的热路径，代价比它省下的还大。
///
/// 用「递增 tick + 淘汰时取最小」而非侵入式链表：淘汰只在**超出容量时**
/// 发生，每次 O(cap)，cap=128 时可忽略；换���不引入任何链表状态机
/// （那类代码的 bug 率远高于它省下的常数）。
struct LruMap<K, V> {
    map: HashMap<K, (u64, V)>,
    /// 单调递增的访问时戳，存活条目的 tick 互不相同。
    tick: u64,
    cap: usize,
}

impl<K: std::hash::Hash + Eq + Copy, V: Clone> LruMap<K, V> {
    fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            tick: 0,
            cap,
        }
    }

    /// 命中时刷新时戳（这正是 LRU 语义与普通 HashMap 的唯一差别）。
    fn get(&mut self, key: &K) -> Option<V> {
        let tick = self.tick;
        self.tick += 1;
        match self.map.get_mut(key) {
            Some(slot) => {
                slot.0 = tick;
                Some(slot.1.clone())
            }
            None => None,
        }
    }

    /// 插入或更新；超出容量时淘汰时戳最小的一项。
    fn put(&mut self, key: K, value: V) {
        // cap == 0 的语义只能是「不缓存」—— 与 LruCache D410 同一条：
        // 淘汰判据在此之前成立，而此刻无可淘汰项。
        if self.cap == 0 {
            return;
        }
        let tick = self.tick;
        self.tick += 1;
        if !self.map.contains_key(&key)
            && self.map.len() >= self.cap
            && let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, (t, _))| *t)
                .map(|(k, _)| *k)
        {
            self.map.remove(&oldest);
        }
        self.map.insert(key, (tick, value));
    }

    fn len(&self) -> usize {
        self.map.len()
    }

    fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    fn clear(&mut self) {
        self.map.clear();
    }
}

impl<K: std::hash::Hash + Eq + Copy, V: Clone> Clone for LruMap<K, V> {
    fn clone(&self) -> Self {
        Self {
            map: self.map.clone(),
            tick: self.tick,
            cap: self.cap,
        }
    }
}

/// `MirFunction` → 优化后 `MirDag` 的缓存。
/// 缓存项持有 func 强引用（见模块注释，v0.75.11 指针复用修复）。
type DagCacheEntry = (Arc<MirFunction>, Arc<MirDag>);

/// MirFunction → 优化后 MirDag 的缓存（宿主单属主纯值，v1.00）。
#[derive(Clone)]
pub struct DagCache {
    /// 指针 key（快路径）：同一 `Arc` 直接命中，O(1)，无内容哈希开销。
    entries: LruMap<usize, DagCacheEntry>,
    /// 内容指纹 key（等价路径）：不同 `Arc` 包裹同一函数体时命中同一 DAG。
    ///
    /// **v0.103 修复**：此前只有指针 key，而多处调用方每轮新建
    /// `Arc::new((*body).clone())`（`h_call`/`run_isolated`/`Worker` 执行路径
    /// 等）—— 指针每轮不同 → 每轮都重建 DAG。worker 内 task 含大循环时
    /// 表现为**卡死**（实测 n=500 即不返回），顺序路径因复用同一 `Arc`
    /// 而掩盖了该缺陷。内容 key 使缓存契约与调用方模式匹配（模块注释原写的
    /// 「不同 Arc 各自独立构建」是设计缺陷，非契约）。
    ///
    /// 本表此前超出 `MAX_ENTRIES` 走的是 `clear()`（整表清空），内容 key
    /// 只把「每轮重建」降级成「每轮重建 + 周期性全丢」，**工作集 >128 时
    /// 命中率仍然崩掉**。现改为 LRU 逐项淘汰。
    content_index: LruMap<u64, Arc<MirDag>>,
}

impl DagCache {
    pub fn new() -> Self {
        Self {
            entries: LruMap::new(MAX_ENTRIES),
            content_index: LruMap::new(MAX_ENTRIES),
        }
    }

    /// 函数体内容指纹（结构稳定：params + body 的 Debug 表示 + n_regs）。
    ///
    /// 用 `DefaultHasher` 对 `Debug` 输出哈希 —— 不引入 `MirInst: Hash`
    /// 的侵入式 derive（`MirInst` 含 `Value`/`MirFunction` 递归字段，
    /// 派生 Hash 成本与维护面都大），而 `Debug` 表示对同一结构确定。
    fn content_key(func: &MirFunction) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        func.params.hash(&mut h);
        func.n_regs.hash(&mut h);
        format!("{:?}", func.body).hash(&mut h);
        h.finish()
    }

    /// 构建 DAG（分析 → 优化 → 剪边），三条路径共用。
    fn build(func_arc: &Arc<MirFunction>) -> Arc<MirDag> {
        let mut dag = dag_analyze(func_arc);
        dag_optimize(&mut dag);
        dag.prune_sequence_edges();
        Arc::new(dag)
    }

    /// 获取或构建缓存 DAG。`func_arc` 是调用方持有的 `Arc<MirFunction>`。
    /// 构建路径 = `dag_analyze → dag_optimize → prune_sequence_edges`，
    /// 与 `run_mir_with_signal` 原本的构建路径一致。
    pub fn get_or_build(&mut self, func_arc: &Arc<MirFunction>) -> Arc<MirDag> {
        // 快路径：同一 Arc（指针）直接命中
        let ptr_key = Arc::as_ptr(func_arc) as usize;
        if let Some((_, dag)) = self.entries.get(&ptr_key) {
            return dag;
        }
        // 等价路径：不同 Arc 同内容命中内容缓存
        let ckey = Self::content_key(func_arc);
        if let Some(dag) = self.content_index.get(&ckey) {
            // 回填指针 key，后续同 Arc 走快路径
            self.entries.put(ptr_key, (func_arc.clone(), dag.clone()));
            return dag;
        }
        let dag = Self::build(func_arc);
        // v0.75.11: 持有 func 强引用 — key 指针永不复用（同指针必然同内容）。
        self.entries.put(ptr_key, (func_arc.clone(), dag.clone()));
        self.content_index.put(ckey, dag.clone());
        dag
    }

    /// 当前缓存项数（测试/诊断用）。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 当前缓存是否为空（测试/诊断用）。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 内容缓存项数（测试/诊断用）。
    pub fn content_len(&self) -> usize {
        self.content_index.len()
    }

    /// 测试辅助：清空缓存。
    pub fn clear(&mut self) {
        self.entries.clear();
        self.content_index.clear();
    }
}

impl Default for DagCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::Interpreter;
    use crate::parser_v3::ParserV3;

    fn sample_func(src: &str) -> Arc<MirFunction> {
        let (func, _witnesses) = ParserV3::compile(src).expect("compile should succeed");
        Arc::new(func)
    }

    /// 同一 Arc 命中缓存（不重建），不同 Arc 各自构建。
    #[test]
    fn same_arc_hits_different_arc_reuses_by_content() {
        let mut cache = DagCache::new();
        let f1 = sample_func("let x = 1 + 2\nprint(x)\n");
        let d1 = cache.get_or_build(&f1);
        let d2 = cache.get_or_build(&f1);
        assert!(Arc::ptr_eq(&d1, &d2), "same Arc must reuse cached DAG");
        assert_eq!(cache.len(), 1);

        // v0.103: 不同 Arc 但**同内容** → 复用同一 DAG（内容指纹命中）。
        // 此前契约是「不同 Arc 必然重建」，但多处调用方每轮新建
        // `Arc::new((*body).clone())`（h_call/run_isolated/Worker/h_parallel
        // 段执行），使该契约退化为「循环内每轮重建 + 缓存反复清空」。
        let f2 = sample_func("let x = 1 + 2\nprint(x)\n");
        let d3 = cache.get_or_build(&f2);
        assert!(
            Arc::ptr_eq(&d1, &d3),
            "different Arc with identical content must reuse the DAG"
        );

        // 内容不同 → 必然重建
        let f3 = sample_func("let y = 9 * 9\nprint(y)\n");
        let d4 = cache.get_or_build(&f3);
        assert!(
            !Arc::ptr_eq(&d1, &d4),
            "different content must build a distinct DAG"
        );
        assert!(cache.content_len() >= 2);
    }

    /// clear 后重新构建（测试辅助语义）。
    #[test]
    fn clear_forces_rebuild() {
        let mut cache = DagCache::new();
        let f1 = sample_func("let x = 42\nprint(x)\n");
        let d1 = cache.get_or_build(&f1);
        cache.clear();
        assert_eq!(cache.len(), 0);
        let d2 = cache.get_or_build(&f1);
        assert!(!Arc::ptr_eq(&d1, &d2), "clear must evict cached DAG");
    }

    /// v1.00: Clone 按值复制 —— 克隆与母本独立演化（memo 透明），
    /// 继承母本已预热条目（Pregel worker 继承 master 预构建 DAG）。
    #[test]
    fn clone_inherits_and_diverges() {
        let mut cache = DagCache::new();
        let f1 = sample_func("let x = 1 + 2\nprint(x)\n");
        let d1 = cache.get_or_build(&f1);
        let mut cloned = cache.clone();
        assert_eq!(cloned.len(), 1);
        // 克隆命中母本已缓存的同一 Arc。
        let d2 = cloned.get_or_build(&f1);
        assert!(Arc::ptr_eq(&d1, &d2), "clone must inherit warmed entries");
        // 克隆插入新条目不影响母本。
        let f2 = sample_func("let y = 3\nprint(y)\n");
        let _ = cloned.get_or_build(&f2);
        assert_eq!(cloned.len(), 2);
        assert_eq!(cache.len(), 1);
    }

    /// 容量上限改为**淘汰最久未用**后的回归。
    ///
    /// 修前是 `if len >= MAX { clear() }` —— 超出容量就把**整张表**清空。
    /// 于是任何工作集 >128 的调用方（模块注释记录的 n=500 卡死就是这个）
    /// 每插入一项就丢掉全部 128 项，命中率崩到接近 0；worker 内含大循环的
    /// task 直接表现为「不返回」。
    ///
    /// 修后超出容量只淘汰**一项**，且 len 恒等于 cap。
    #[test]
    fn exceeding_capacity_evicts_one_entry_instead_of_clearing() {
        let mut m: LruMap<usize, u32> = LruMap::new(4);
        for k in 0..4u32 {
            m.put(k as usize, k);
        }
        assert_eq!(m.len(), 4);
        m.put(99, 99);
        assert_eq!(
            m.len(),
            4,
            "超出容量后必须只淘汰一项；清空会让 len 归零（修前行为）"
        );
    }

    /// 淘汰的必须是**最久未用**的那一项，而不是插入顺序里的第一项。
    ///
    /// 这条才区分「真 LRU」与「FIFO / 随便挑一项」：命中会刷新时戳，
    /// 所以刚被读过的 key 不该是下一个被淘汰的。
    #[test]
    fn eviction_targets_the_least_recently_used_entry() {
        let mut m: LruMap<usize, u32> = LruMap::new(3);
        m.put(1, 1);
        m.put(2, 2);
        m.put(3, 3);
        // 触碰 1 ⇒ 它成为最新，2/3 变旧
        assert_eq!(m.get(&1), Some(1));
        m.put(4, 4);
        assert!(m.get(&1).is_some(), "刚访问过的 1 不该被淘汰");
        assert!(m.get(&4).is_some());
        // 2 与 3 谁更旧取决于触碰顺序：触碰 1 之后 2 是最旧的
        assert!(
            m.get(&2).is_none() || m.get(&3).is_none(),
            "必须至少淘汰一项最旧条目"
        );
    }

    /// cap == 0 的语义只能是「不缓存」（与 `LruCache` D410 同一条判据：
    /// 淘汰判据在此之前成立，而此刻无可淘汰项）。
    #[test]
    fn zero_capacity_caches_nothing() {
        let mut m: LruMap<usize, u32> = LruMap::new(0);
        m.put(1, 1);
        assert_eq!(m.len(), 0, "cap=0 存进任何一条都违反本类型自己的契约");
        assert!(m.get(&1).is_none());
    }

    /// DagCache 层：内容表超出容量后仍保有 cap 项，而不是被清空。
    ///
    /// 直接用 `clear()` 版本时，这个断言在第 129 个不同函数体处就会炸。
    #[test]
    fn dag_cache_stays_populated_beyond_capacity() {
        let mut cache = DagCache::new();
        // 造 MAX_ENTRIES + 8 个内容互异的函数体
        for i in 0..(MAX_ENTRIES + 8) {
            let f = sample_func(&format!(
                "let x = {}
print(x)
",
                i * 7 + 1
            ));
            let _ = cache.get_or_build(&f);
        }
        assert_eq!(
            cache.content_len(),
            MAX_ENTRIES,
            "超出容量后内容表应保持在 cap（修前此处会被 clear 成 0）"
        );
    }

    /// 缓存 DAG 与直建 DAG 等价执行（tier0 管线守卫）。
    #[test]
    fn cached_dag_runs_same_result() {
        // v0.104.6 D42：原先这里写的是顶层 `return acc`，而 D42 判定
        // 「程序顶层的 return」为缺陷（它会静默终止整个程序、吞掉后续
        // 语句且退出码 0），已改为编译期拒绝。本测试的意图是
        // 「**缓存路径与直建路径结果一致**」，与顶层 `return` 无关 ——
        // 故把累加与 `return` 一起放进 task，形状与原用例等价。
        let source = "task go()\n  let acc = 0\n  for i in [1, 2, 3]\n    acc = acc + i\n  end\n  return acc\nend\ngo()\n";
        let (func_raw, _witnesses) = ParserV3::compile(source).expect("compile");
        let func: Arc<MirFunction> = Arc::new(func_raw);

        // 直建路径（baseline）
        let mut interp = Interpreter::new();
        let mut env = interp.take_env();
        let baseline = crate::mir::vm::execute(
            &func,
            &mut interp,
            &mut env,
            &mut crate::mir::effect::Effects::new(),
        )
        .expect("baseline run should succeed");

        // 缓存路径
        let mut cache = DagCache::new();
        let dag = cache.get_or_build(&func);
        let mut interp = Interpreter::new();
        let mut env = interp.take_env();
        let (_, cached) = crate::mir::vm::run_dag_with_signal(
            dag.as_ref(),
            func.as_ref(),
            &mut interp,
            &mut env,
            &mut crate::mir::effect::Effects::new(),
        )
        .expect("cached run should succeed");
        assert_eq!(cached, baseline);
    }
}
