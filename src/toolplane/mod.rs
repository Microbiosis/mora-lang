//! v0.45.0: ToolPlane — Core/Extension Adapter (loongclaw-inspired)
//!
//! 灵感: loongclaw `crates/kernel/src/tool.rs:25-67`
//! - `ToolPlane` struct: name + kind + tools HashMap (one per plane)
//! - `PlaneKind::Core` — built-in/standard plane (e.g. "ai", "memory")
//! - `PlaneKind::Extension` — user/plugin-defined planes
//! - `ToolSpec { name, description, parameters }` + `ToolDef` with handler
//!
//! v0.45.0 设计:
//! - Multi-plane registry (vs v0.34 single HashMap tool_registry)
//! - Core/Extension 分桶, 调度通过 `tool.plane.dispatch(plane, name, args)`
//! - 保留 `tool_registry` field (向后兼容), 新加 `tool_planes` field
//! - builtin `tool.plane.*` 操作 plane
//!
//! 与 master doc §6.5 区别: master 提议 `ToolPlane 替代 tool_registry`,
//! v0.45.0 保守共存 (additive), 留 v0.46+ 再做完全替代。

use std::collections::HashMap;
use std::sync::Arc;

/// v0.45.0: Plane kind — Core (built-in) vs Extension (user/plugin)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaneKind {
    /// Built-in / standard plane (e.g. "ai", "memory", "sandbox")
    Core,
    /// User-defined / plugin plane
    Extension,
}

impl PlaneKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "core" => Some(Self::Core),
            "extension" | "ext" => Some(Self::Extension),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Extension => "extension",
        }
    }
}

/// v0.45.0: ToolSpec — 工具元数据 (无 handler)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema-style 参数描述 (字符串简化, 不依赖外部 crate)
    pub parameters: String,
}

/// v0.45.0: ToolPlane — 一个 plane 包含一组 tools
#[derive(Debug, Clone)]
pub struct ToolPlane {
    pub name: String,
    pub kind: PlaneKind,
    pub tools: HashMap<String, ToolSpec>,
    pub created_at: std::time::Instant,
}

impl ToolPlane {
    pub fn new(name: String, kind: PlaneKind) -> Self {
        Self {
            name,
            kind,
            tools: HashMap::new(),
            created_at: std::time::Instant::now(),
        }
    }

    /// 注册一个 tool (返回 Err 如果已存在同名 tool)
    pub fn register(&mut self, spec: ToolSpec) -> Result<(), String> {
        if self.tools.contains_key(&spec.name) {
            return Err(format!(
                "tool '{}' already exists in plane '{}'",
                spec.name, self.name
            ));
        }
        self.tools.insert(spec.name.clone(), spec);
        Ok(())
    }

    pub fn unregister(&mut self, tool_name: &str) -> Option<ToolSpec> {
        self.tools.remove(tool_name)
    }

    pub fn get(&self, tool_name: &str) -> Option<&ToolSpec> {
        self.tools.get(tool_name)
    }

    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }
}

/// v0.45.0: ToolPlaneRegistry — multi-plane container
///
/// 单线程同步 (Arc<Mutex<>> for shared access if needed later).
#[derive(Debug, Default, Clone)]
pub struct ToolPlaneRegistry {
    planes: HashMap<String, ToolPlane>,
}

impl ToolPlaneRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create_plane(&mut self, name: String, kind: PlaneKind) -> Result<(), String> {
        if name.is_empty() {
            return Err("plane name cannot be empty".to_string());
        }
        if self.planes.contains_key(&name) {
            return Err(format!("plane '{}' already exists", name));
        }
        self.planes.insert(name.clone(), ToolPlane::new(name, kind));
        Ok(())
    }

    pub fn get_plane(&self, name: &str) -> Option<&ToolPlane> {
        self.planes.get(name)
    }

    pub fn get_plane_mut(&mut self, name: &str) -> Option<&mut ToolPlane> {
        self.planes.get_mut(name)
    }

    pub fn remove_plane(&mut self, name: &str) -> Option<ToolPlane> {
        self.planes.remove(name)
    }

    pub fn plane_count(&self) -> usize {
        self.planes.len()
    }

    pub fn list_planes(&self) -> Vec<String> {
        let mut names: Vec<String> = self.planes.keys().cloned().collect();
        names.sort();
        names
    }

    /// 在指定 plane 中查找 tool (返回 (plane_name, &ToolSpec))
    pub fn find_tool(&self, plane_name: &str, tool_name: &str) -> Option<&ToolSpec> {
        self.planes.get(plane_name)?.get(tool_name)
    }
}

/// v0.45.0: 全局默认 planes — 注册两个 core planes (ai, sandbox)
pub fn default_registry() -> ToolPlaneRegistry {
    let mut reg = ToolPlaneRegistry::new();
    reg.create_plane("ai".to_string(), PlaneKind::Core).unwrap();
    reg.create_plane("sandbox".to_string(), PlaneKind::Core)
        .unwrap();
    reg
}

/// v0.45.0: 把 ToolPlaneRegistry 包装成 Arc<Mutex<>> 便于 Interpreter 持有
pub type SharedRegistry = Arc<std::sync::Mutex<ToolPlaneRegistry>>;

pub fn shared_default_registry() -> SharedRegistry {
    Arc::new(std::sync::Mutex::new(default_registry()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plane_kind_parse_roundtrip() {
        for kind in [PlaneKind::Core, PlaneKind::Extension] {
            assert_eq!(PlaneKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(PlaneKind::parse("unknown"), None);
    }

    #[test]
    fn plane_register_and_lookup() {
        let mut plane = ToolPlane::new("test".to_string(), PlaneKind::Core);
        plane
            .register(ToolSpec {
                name: "echo".to_string(),
                description: "echo back".to_string(),
                parameters: r#"{"type":"object"}"#.to_string(),
            })
            .unwrap();
        assert_eq!(plane.tool_count(), 1);
        assert!(plane.get("echo").is_some());
        assert!(plane.get("nope").is_none());
    }

    #[test]
    fn plane_register_duplicate_fails() {
        let mut plane = ToolPlane::new("test".to_string(), PlaneKind::Core);
        plane
            .register(ToolSpec {
                name: "x".to_string(),
                description: "".to_string(),
                parameters: "{}".to_string(),
            })
            .unwrap();
        let err = plane
            .register(ToolSpec {
                name: "x".to_string(),
                description: "".to_string(),
                parameters: "{}".to_string(),
            })
            .unwrap_err();
        assert!(err.contains("already exists"));
    }

    #[test]
    fn plane_unregister() {
        let mut plane = ToolPlane::new("test".to_string(), PlaneKind::Core);
        plane
            .register(ToolSpec {
                name: "x".to_string(),
                description: "".to_string(),
                parameters: "{}".to_string(),
            })
            .unwrap();
        let removed = plane.unregister("x");
        assert!(removed.is_some());
        assert_eq!(plane.tool_count(), 0);
    }

    #[test]
    fn registry_create_plane() {
        let mut reg = ToolPlaneRegistry::new();
        reg.create_plane("p1".to_string(), PlaneKind::Core).unwrap();
        reg.create_plane("p2".to_string(), PlaneKind::Extension)
            .unwrap();
        assert_eq!(reg.plane_count(), 2);
    }

    #[test]
    fn registry_create_duplicate_plane_fails() {
        let mut reg = ToolPlaneRegistry::new();
        reg.create_plane("p".to_string(), PlaneKind::Core).unwrap();
        let err = reg
            .create_plane("p".to_string(), PlaneKind::Extension)
            .unwrap_err();
        assert!(err.contains("already exists"));
    }

    #[test]
    fn registry_empty_plane_name_fails() {
        let mut reg = ToolPlaneRegistry::new();
        let err = reg
            .create_plane("".to_string(), PlaneKind::Core)
            .unwrap_err();
        assert!(err.contains("empty"));
    }

    #[test]
    fn registry_find_tool_across_planes() {
        let mut reg = ToolPlaneRegistry::new();
        reg.create_plane("p1".to_string(), PlaneKind::Core).unwrap();
        reg.create_plane("p2".to_string(), PlaneKind::Extension)
            .unwrap();
        reg.get_plane_mut("p1")
            .unwrap()
            .register(ToolSpec {
                name: "x".to_string(),
                description: "".to_string(),
                parameters: "{}".to_string(),
            })
            .unwrap();
        reg.get_plane_mut("p2")
            .unwrap()
            .register(ToolSpec {
                name: "x".to_string(),
                description: "".to_string(),
                parameters: "{}".to_string(),
            })
            .unwrap();

        assert!(reg.find_tool("p1", "x").is_some());
        assert!(reg.find_tool("p2", "x").is_some());
        assert!(reg.find_tool("p1", "y").is_none());
    }

    #[test]
    fn default_registry_has_core_planes() {
        let reg = default_registry();
        assert!(reg.get_plane("ai").is_some());
        assert!(reg.get_plane("sandbox").is_some());
        assert_eq!(reg.get_plane("ai").unwrap().kind, PlaneKind::Core);
        assert_eq!(reg.get_plane("sandbox").unwrap().kind, PlaneKind::Core);
    }

    #[test]
    fn list_planes_returns_sorted() {
        let mut reg = ToolPlaneRegistry::new();
        reg.create_plane("z".to_string(), PlaneKind::Core).unwrap();
        reg.create_plane("a".to_string(), PlaneKind::Core).unwrap();
        reg.create_plane("m".to_string(), PlaneKind::Core).unwrap();
        let list = reg.list_planes();
        assert_eq!(list, vec!["a", "m", "z"]);
    }

    #[test]
    fn remove_plane_returns_plane() {
        let mut reg = ToolPlaneRegistry::new();
        reg.create_plane("p".to_string(), PlaneKind::Core).unwrap();
        let removed = reg.remove_plane("p");
        assert!(removed.is_some());
        assert_eq!(reg.plane_count(), 0);
    }
}
