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
//! 容量上限 128：满则整体清空（简单可预测的 LRU 近似，防无限增长）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use super::MirFunction;
use super::dag::{MirDag, dag_analyze};
use super::optimize::dag_optimize;

/// 缓存容量上限：超过则清空（防止长生命周期进程无限增长）。
const MAX_ENTRIES: usize = 128;

/// 全局 DAG 缓存（进程级，线程安全 — pregel 并行 worker 共享同一缓存）。
pub static DAG_CACHE: OnceLock<DagCache> = OnceLock::new();

/// 获取进程级全局缓存实例。
pub fn global_dag_cache() -> &'static DagCache {
    DAG_CACHE.get_or_init(DagCache::new)
}

/// `MirFunction` → 优化后 `MirDag` 的缓存。
pub struct DagCache {
    entries: Mutex<HashMap<usize, Arc<MirDag>>>,
}

impl DagCache {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// 获取或构建缓存 DAG。`func_arc` 是调用方持有的 `Arc<MirFunction>`。
    /// 构建路径 = `dag_analyze → dag_optimize → prune_sequence_edges`，
    /// 与 `run_mir_with_signal` 原本的构建路径一致。
    pub fn get_or_build(&self, func_arc: &Arc<MirFunction>) -> Arc<MirDag> {
        let key = Arc::as_ptr(func_arc) as usize;
        let mut entries = self.entries.lock().expect("DagCache entries poisoned");
        if let Some(dag) = entries.get(&key) {
            return dag.clone();
        }
        let mut dag = dag_analyze(func_arc);
        dag_optimize(&mut dag);
        dag.prune_sequence_edges();
        let dag = Arc::new(dag);
        if entries.len() >= MAX_ENTRIES {
            entries.clear();
        }
        entries.insert(key, dag.clone());
        dag
    }

    /// 当前缓存项数（测试/诊断用）。
    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .expect("DagCache entries poisoned")
            .len()
    }

    /// 测试辅助：清空缓存。
    pub fn clear(&self) {
        self.entries
            .lock()
            .expect("DagCache entries poisoned")
            .clear();
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
    use crate::interpreter::{Interpreter, parse_code_v3};
    use crate::mir::lower::lower_mir_exprs;

    fn sample_func(src: &str) -> Arc<MirFunction> {
        let exprs = parse_code_v3(src).expect("parse should succeed");
        Arc::new(lower_mir_exprs(&exprs).expect("lower should succeed"))
    }

    /// 同一 Arc 命中缓存（不重建），不同 Arc 各自构建。
    #[test]
    fn same_arc_hits_different_arc_rebuilds() {
        let cache = DagCache::new();
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
        let cache = DagCache::new();
        let f1 = sample_func("let x = 42\nprint(x)\n");
        let d1 = cache.get_or_build(&f1);
        cache.clear();
        assert_eq!(cache.len(), 0);
        let d2 = cache.get_or_build(&f1);
        assert!(!Arc::ptr_eq(&d1, &d2), "clear must evict cached DAG");
    }

    /// 缓存 DAG 与直建 DAG 等价执行（tier0 管线守卫）。
    #[test]
    fn cached_dag_runs_same_result() {
        let source = "let acc = 0\nfor i in [1, 2, 3]\n  acc = acc + i\nend\nreturn acc\n";
        let exprs = parse_code_v3(source).expect("parse");
        let func: Arc<MirFunction> = Arc::new(lower_mir_exprs(&exprs).expect("lower"));

        // 直建路径（baseline）
        let mut interp = Interpreter::new();
        let mut env = interp.take_env();
        let baseline = crate::mir::interp::run_mir(&func, &mut interp, &mut env)
            .expect("baseline run should succeed");

        // 缓存路径
        let cache = DagCache::new();
        let dag = cache.get_or_build(&func);
        let mut interp = Interpreter::new();
        let mut env = interp.take_env();
        let (_, cached) = crate::mir::dag_interp::run_dag_with_signal(
            dag.as_ref(),
            func.as_ref(),
            &mut interp,
            &mut env,
        )
        .expect("cached run should succeed");
        assert_eq!(cached, baseline);
    }
}
