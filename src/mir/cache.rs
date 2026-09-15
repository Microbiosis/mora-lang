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

/// 缓存容量上限：超过则清空（防止长生命周期进程无限增长）。
const MAX_ENTRIES: usize = 128;

/// `MirFunction` → 优化后 `MirDag` 的缓存。
/// 缓存项持有 func 强引用（见模块注释，v0.75.11 指针复用修复）。
type DagCacheEntry = (Arc<MirFunction>, Arc<MirDag>);

/// MirFunction → 优化后 MirDag 的缓存（宿主单属主纯值，v1.00）。
#[derive(Clone)]
pub struct DagCache {
    /// 指针 key（快路径）：同一 `Arc` 直接命中，O(1)，无内容哈希开销。
    entries: HashMap<usize, DagCacheEntry>,
    /// 内容指纹 key（等价路径）：不同 `Arc` 包裹同一函数体时命中同一 DAG。
    ///
    /// **v0.103 修复**：此前只有指针 key，而多处调用方每轮新建
    /// `Arc::new((*body).clone())`（`h_call`/`run_isolated`/`Worker` 执行路径
    /// 等）—— 指针每轮不同 → 每轮都重建 DAG，且 `MAX_ENTRIES` 反复触发
    /// `clear()` 抖动。worker 内 task 含大循环时表现为**卡死**（实测 n=500
    /// 即不返回），顺序路径因复用同一 `Arc` 而掩盖了该缺陷。
    /// 内容 key 使缓存契约与调用方模式匹配（模块注释原写的「不同 Arc 各自
    /// 独立构建」是设计缺陷，非契约）。
    content_index: HashMap<u64, Arc<MirDag>>,
}

impl DagCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            content_index: HashMap::new(),
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
            return dag.clone();
        }
        // 等价路径：不同 Arc 同内容命中内容缓存
        let ckey = Self::content_key(func_arc);
        if let Some(dag) = self.content_index.get(&ckey) {
            // 回填指针 key，后续同 Arc 走快路径
            self.entries
                .insert(ptr_key, (func_arc.clone(), dag.clone()));
            return dag.clone();
        }
        let dag = Self::build(func_arc);
        // 容量上限：先清指针表（廉价可重建），内容表保留（价值更高）。
        // 此前单表满即 clear 全部 —— 循环内新 Arc 每轮触发清空，缓存
        // 命中率归零是 worker 卡死的直接成因。
        if self.entries.len() >= MAX_ENTRIES {
            self.entries.clear();
        }
        if self.content_index.len() >= MAX_ENTRIES {
            self.content_index.clear();
        }
        // v0.75.11: 持有 func 强引用 — key 指针永不复用（同指针必然同内容）。
        self.entries
            .insert(ptr_key, (func_arc.clone(), dag.clone()));
        self.content_index.insert(ckey, dag.clone());
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

    /// 缓存 DAG 与直建 DAG 等价执行（tier0 管线守卫）。
    #[test]
    fn cached_dag_runs_same_result() {
        let source = "let acc = 0\nfor i in [1, 2, 3]\n  acc = acc + i\nend\nreturn acc\n";
        let (func_raw, _witnesses) = ParserV3::compile(source).expect("compile");
        let func: Arc<MirFunction> = Arc::new(func_raw);

        // 直建路径（baseline）
        let mut interp = Interpreter::new();
        let mut env = interp.take_env();
        let baseline =
            crate::mir::vm::run_mir(&func, &mut interp, &mut env, &mut crate::mir::effect::Effects::new())
                .expect("baseline run should succeed");

        // 缓存路径
        let mut cache = DagCache::new();
        let dag = cache.get_or_build(&func);
        let mut interp = Interpreter::new();
        let mut env = interp.take_env();
        let (_, cached) =
            crate::mir::vm::run_dag_with_signal(
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
