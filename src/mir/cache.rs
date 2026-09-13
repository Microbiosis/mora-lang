//! v0.75.9: MirFunction → 优化后 DAG 的跨调用缓存。
//!
//! `run_mir_with_signal` 此前每次调用全量重建 DAG（`dag_analyze` +
//! `dag_optimize` + `prune_sequence_edges`）。同一函数体多次调用
//! （Closure/Task/循环内 WithConfig/pregel 每超步 agent）重复重建，
//! 分析开销与分配都无谓。
//!
//! 缓存 key = `Arc<MirFunction>` 的指针地址。前提：`MirFunction` body 在
//! 构造后不可变（项目内无 `Arc::get_mut` 改写），同一 Arc 即同一 DAG。
//! 不同 Arc 包裹同一内容（如 SSA 优化产物每次新建）则各自独立构建 —
//! 内容相等性不在缓存契约内。
//!
//! v0.75.11 修复：缓存项**同时持有 `Arc<MirFunction>` 强引用**（二元组
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
    entries: HashMap<usize, DagCacheEntry>,
}

impl DagCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// 获取或构建缓存 DAG。`func_arc` 是调用方持有的 `Arc<MirFunction>`。
    /// 构建路径 = `dag_analyze → dag_optimize → prune_sequence_edges`，
    /// 与 `run_mir_with_signal` 原本的构建路径一致。
    pub fn get_or_build(&mut self, func_arc: &Arc<MirFunction>) -> Arc<MirDag> {
        let key = Arc::as_ptr(func_arc) as usize;
        if let Some((_, dag)) = self.entries.get(&key) {
            return dag.clone();
        }
        let mut dag = dag_analyze(func_arc);
        dag_optimize(&mut dag);
        dag.prune_sequence_edges();
        let dag = Arc::new(dag);
        if self.entries.len() >= MAX_ENTRIES {
            self.entries.clear();
        }
        // v0.75.11: 持有 func 强引用 — key 指针永不复用（同指针必然同内容）。
        self.entries.insert(key, (func_arc.clone(), dag.clone()));
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

    /// 测试辅助：清空缓存。
    pub fn clear(&mut self) {
        self.entries.clear();
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
    fn same_arc_hits_different_arc_rebuilds() {
        let mut cache = DagCache::new();
        let f1 = sample_func("let x = 1 + 2\nprint(x)\n");
        let d1 = cache.get_or_build(&f1);
        let d2 = cache.get_or_build(&f1);
        assert!(Arc::ptr_eq(&d1, &d2), "same Arc must reuse cached DAG");
        assert_eq!(cache.len(), 1);

        // 不同 Arc（同内容）：构建出新 DAG，互不影响。
        let f2 = sample_func("let x = 1 + 2\nprint(x)\n");
        let d3 = cache.get_or_build(&f2);
        assert!(!Arc::ptr_eq(&d1, &d3), "different Arc must rebuild");
        assert_eq!(cache.len(), 2);
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
