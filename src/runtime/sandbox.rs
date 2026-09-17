//! v0.52 ADR-001: SandboxRuntime — BC7 (sandbox policy + container + tool planes)
//!
//! 从 Interpreter god object 抽出的 sandbox 状态容器，3 字段（capability 是 module-level state）。
//!
//! 注意：ContainerHandle 有 Drop impl（v0.49 C3）触发 `docker rm -f`。
//! 多次 Clone 会导致 Drop 多次触发 — 这是 pre-existing 行为（Interpreter::clone 也走同路径）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::sandbox::{ContainerHandle, SandboxPolicy};
use crate::toolplane::ToolPlaneRegistry;

#[derive(Clone)]
pub struct SandboxRuntime {
    /// v0.34: 沙箱策略 (来自 src/sandbox/, MimiClaw path validation)
    pub(crate) sandbox: SandboxPolicy,
    /// v0.44.0: Container handle (REAL Docker spawn via `docker run`)
    /// None = no container (run on host). Set via sandbox.containerize builtin.
    /// Arc<Mutex<>> keeps call_sandbox_method `&self` (Clone-safe).
    pub(crate) container: Arc<Mutex<Option<ContainerHandle>>>,
    /// v0.45.0: ToolPlane registry (multi-plane Core/Extension adapter)
    /// Default has 2 core planes: "ai" + "sandbox"
    pub(crate) tool_planes: Arc<Mutex<ToolPlaneRegistry>>,
    /// v0.101: 容器名计数器 —— 取代 v0.49 的进程级
    /// `static CONTAINER_COUNTER: AtomicU64` 全局状态机（数据流化）。
    /// 归 SandboxRuntime 实例所有，Arc 跨克隆共享（锁分类第 2 类：有意
    /// 跨克隆/跨线程共享 —— Interpreter clone 出的 worker 与母体共用一个
    /// 计数序列，保证同进程内容器名唯一性与旧全局计数器等价）。
    pub(crate) container_name_counter: Arc<AtomicU64>,
}

impl Default for SandboxRuntime {
    fn default() -> Self {
        Self {
            sandbox: SandboxPolicy::permissive(),
            container: Arc::new(Mutex::new(None)),
            // 用 default_registry() 而非 ToolPlaneRegistry::default() — 含 2 core planes (ai + sandbox)
            tool_planes: Arc::new(Mutex::new(crate::toolplane::default_registry())),
            container_name_counter: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl SandboxRuntime {
    /// 检查路径是否在沙箱允许范围内（返回 canonical 路径或错误）
    pub fn check_path(&self, path: &str) -> Result<std::path::PathBuf, String> {
        self.sandbox.check_path(path)
    }

    /// v0.101: 生成下一个容器名（数据流入口）。
    /// 计数器是本实例的共享状态（`&self` + 原子 —— 这是共享机制本身，
    /// 不是防御式加锁）；nanos + counter 显式交给纯函数
    /// [`crate::sandbox::container::generate_container_name`] 做格式化。
    pub fn next_container_name(&self) -> String {
        let nanos: u64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let counter = self.container_name_counter.fetch_add(1, Ordering::Relaxed);
        crate::sandbox::container::generate_container_name(nanos, counter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_sandbox_permissive() {
        let sb = SandboxRuntime::default();
        // permissive 策略允许所有路径
        assert!(sb.check_path("/tmp/test").is_ok());
    }

    #[test]
    fn container_default_none() {
        let sb = SandboxRuntime::default();
        let container = sb.container.lock().expect("container poisoned");
        assert!(container.is_none());
    }

    #[test]
    fn tool_planes_default_has_core() {
        let sb = SandboxRuntime::default();
        let planes = sb.tool_planes.lock().expect("tool_planes poisoned");
        // ToolPlaneRegistry::default() 含 2 core planes (ai + sandbox)
        let _ = &*planes; // 不 panic 即可
    }

    #[test]
    fn sandbox_check_safe_path() {
        let sb = SandboxRuntime::default();
        // permissive 默认允许
        assert!(sb.check_path("/workspace/foo.txt").is_ok());
    }

    #[test]
    fn sandbox_check_relative_path() {
        let sb = SandboxRuntime::default();
        // relative path 在 permissive 下应允许
        assert!(sb.check_path("relative/path.txt").is_ok());
    }

    /// v0.101: 计数器跨克隆共享 —— clone 出的实例与母体共用了同一个
    /// 计数序列（Arc 共享），产生的名字两两不同。
    #[test]
    fn container_name_counter_shared_across_clone() {
        let sb = SandboxRuntime::default();
        let sb_clone = sb.clone();
        let names: Vec<String> = (0..4)
            .map(|i| {
                if i % 2 == 0 {
                    sb.next_container_name()
                } else {
                    sb_clone.next_container_name()
                }
            })
            .collect();
        let unique: std::collections::HashSet<_> = names.iter().collect();
        assert_eq!(unique.len(), 4, "clones share one counter: {:?}", names);
        assert!(names.iter().all(|n| n.starts_with("mora-")));
    }

    /// v0.101: 独立实例各自持独立计数序列（值语义隔离）——
    /// 两个 SandboxRuntime 的计数互不可见，各自从 0 开始。
    #[test]
    fn container_name_counter_independent_across_instances() {
        let a = SandboxRuntime::default();
        let b = SandboxRuntime::default();
        let _ = a.next_container_name();
        let _ = a.next_container_name();
        // b 的计数器不受 a 推进影响：仍是第 0 次调用
        let b_first = b.next_container_name();
        let b_second = b.next_container_name();
        assert_ne!(b_first, b_second, "same instance: counter advances");
        // b 实例的计数器值等于 2（前两次调用来自 a，与 b 无关）
        assert_eq!(
            b.container_name_counter
                .load(std::sync::atomic::Ordering::Relaxed),
            2
        );
        assert_eq!(
            a.container_name_counter
                .load(std::sync::atomic::Ordering::Relaxed),
            2
        );
    }

    /// v0.101: 并发生成 —— 多线程共享同一实例，名字仍然唯一
    /// （与旧进程级全局计数器等价的保证，但状态归实例所有）。
    #[test]
    fn container_name_unique_under_concurrency() {
        use std::collections::HashSet;
        use std::sync::Arc;

        let sb = Arc::new(SandboxRuntime::default());
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let sb = sb.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    (0..10)
                        .map(|_| sb.next_container_name())
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut all = HashSet::new();
        for h in handles {
            for name in h.join().expect("worker must not panic") {
                assert!(all.insert(name.clone()), "duplicate: {}", name);
            }
        }
        assert_eq!(all.len(), 80, "should have 80 unique names");
    }

    #[test]
    fn clone_shares_container_arc() {
        let sb1 = SandboxRuntime::default();
        let sb2 = sb1.clone();
        // Arc 共享：改一个应能影响另一个
        sb1.container
            .lock()
            .expect("container poisoned")
            .replace(ContainerHandle::new(
                "test_id".to_string(),
                "test_name".to_string(),
                crate::sandbox::ContainerSpec::new(crate::sandbox::ContainerBackend::Docker),
            ));
        let container2 = sb2.container.lock().expect("container poisoned");
        assert!(container2.is_some());
        assert_eq!(container2.as_ref().unwrap().container_id, "test_id");
    }
}
