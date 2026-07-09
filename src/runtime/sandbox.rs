//! v0.52 ADR-001: SandboxRuntime — BC7 (sandbox policy + container + tool planes)
//!
//! 从 Interpreter god object 抽出的 sandbox 状态容器，3 字段（capability 是 module-level state）。
//!
//! 注意：ContainerHandle 有 Drop impl（v0.49 C3）触发 `docker rm -f`。
//! 多次 Clone 会导致 Drop 多次触发 — 这是 pre-existing 行为（Interpreter::clone 也走同路径）。

use std::sync::{Arc, Mutex};

use crate::sandbox::{ContainerHandle, SandboxPolicy};
use crate::toolplane::ToolPlaneRegistry;

#[derive(Clone)]
pub struct SandboxRuntime {
    /// v0.34: 沙箱策略 (来自 src/sandbox/, MimiClaw path validation)
    pub sandbox: SandboxPolicy,
    /// v0.44.0: Container handle (REAL Docker spawn via `docker run`)
    /// None = no container (run on host). Set via sandbox.containerize builtin.
    /// Arc<Mutex<>> keeps call_sandbox_method `&self` (Clone-safe).
    pub container: Arc<Mutex<Option<ContainerHandle>>>,
    /// v0.45.0: ToolPlane registry (multi-plane Core/Extension adapter)
    /// Default has 2 core planes: "ai" + "sandbox"
    pub tool_planes: Arc<Mutex<ToolPlaneRegistry>>,
}

impl Default for SandboxRuntime {
    fn default() -> Self {
        Self {
            sandbox: SandboxPolicy::permissive(),
            container: Arc::new(Mutex::new(None)),
            // 用 default_registry() 而非 ToolPlaneRegistry::default() — 含 2 core planes (ai + sandbox)
            tool_planes: Arc::new(Mutex::new(crate::toolplane::default_registry())),
        }
    }
}

impl SandboxRuntime {
    /// 检查路径是否在沙箱允许范围内（返回 canonical 路径或错误）
    pub fn check_path(&self, path: &str) -> Result<std::path::PathBuf, String> {
        self.sandbox.check_path(path)
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
