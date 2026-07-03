//! v0.33: Sandbox — path validation + pattern allow/deny
//!
//! 灵感:
//! - **MimiClaw** path validation: read_file/write_file 拒绝 `..` 路径
//!   (main/tools/tool_files.c:15-31)
//! - **AIOS** access manager: agent_id -> privilege_group (hashmap)
//! - **Puter** iframe sandbox: credentialless + capability-based URL params
//!
//! v0.33 实现:
//! - Path safety: 拒绝含 `..` 或绝对路径越界 (out of root) 的操作
//! - Pattern allow/deny: builtin 名 match (wildcard `*` segment, 与 event 一致)
//! - Privilege: thread-local sandbox context (类似 thread_local!)

use std::path::{Component, Path, PathBuf};

/// v0.33: Sandbox 策略
#[derive(Debug, Clone, Default)]
pub struct SandboxPolicy {
    /// v0.36 (P1-3.10): BTreeSet for O(log N) membership checks
    /// (was Vec<String>, O(N) linear scan).
    pub allow: std::collections::BTreeSet<String>,
    /// 禁止的 builtin 模式 (优先于 allow)
    pub deny: std::collections::BTreeSet<String>,
    /// 文件操作根目录 (path validation 基准)
    pub fs_root: Option<PathBuf>,
    /// 超时秒数 (None = 无限制)
    pub timeout_s: Option<u64>,
    /// 内存限制 MB (None = 无限制)
    pub memory_limit_mb: Option<u64>,
}

impl SandboxPolicy {
    /// 创建一个空 policy (拒绝一切, 需显式 allow)
    pub fn strict() -> Self {
        Self {
            allow: std::collections::BTreeSet::new(),
            deny: std::collections::BTreeSet::new(),
            fs_root: None,
            timeout_s: None,
            memory_limit_mb: None,
        }
    }

    /// 创建一个开放 policy (允许一切 builtin, 全路径, 无限制)
    pub fn permissive() -> Self {
        let mut allow = std::collections::BTreeSet::new();
        allow.insert("*".to_string());
        Self {
            allow,
            deny: std::collections::BTreeSet::new(),
            fs_root: Some(PathBuf::from("/")),
            timeout_s: None,
            memory_limit_mb: None,
        }
    }

    /// 检查 builtin name 是否被允许
    pub fn check_builtin(&self, name: &str) -> Result<(), String> {
        // deny 优先
        for pattern in &self.deny {
            if crate::event::matches(name, pattern) {
                return Err(format!(
                    "builtin '{}' denied by pattern '{}'",
                    name, pattern
                ));
            }
        }
        // allow 必须显式
        if self.allow.is_empty() {
            return Err(format!(
                "builtin '{}' rejected: sandbox is strict (no allow patterns)",
                name
            ));
        }
        for pattern in &self.allow {
            if crate::event::matches(name, pattern) {
                return Ok(());
            }
        }
        Err(format!("builtin '{}' not in any allow pattern", name))
    }

    /// 检查 path 是否在 fs_root 之内 (MimiClaw 风格)
    pub fn check_path(&self, path: &str) -> Result<PathBuf, String> {
        let p = Path::new(path);
        // 1. 拒绝含 `..` component
        for comp in p.components() {
            if matches!(comp, Component::ParentDir) {
                return Err(format!(
                    "path '{}' rejected: contains '..' (path traversal)",
                    path
                ));
            }
        }
        // 2. 解析并检查 root 边界
        let root = match &self.fs_root {
            Some(r) => r.clone(),
            None => {
                return Err("sandbox has no fs_root; all file operations rejected".to_string());
            }
        };
        let canonical_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        let resolved = if p.is_absolute() {
            p.to_path_buf()
        } else {
            canonical_root.join(p)
        };
        // resolved 必须以 canonical_root 开头
        if !resolved.starts_with(&canonical_root) {
            return Err(format!(
                "path '{}' escapes fs_root '{}'",
                resolved.display(),
                canonical_root.display()
            ));
        }
        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mora_sandbox_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn strict_rejects_all_by_default() {
        let p = SandboxPolicy::strict();
        assert!(p.check_builtin("ai.chat").is_err());
    }

    #[test]
    fn permissive_allows_anything() {
        let p = SandboxPolicy::permissive();
        assert!(p.check_builtin("ai.chat").is_ok());
        assert!(p.check_builtin("anything.you.want").is_ok());
    }

    #[test]
    fn allow_pattern_matches() {
        let mut allow = std::collections::BTreeSet::new();
        allow.insert("memory.*".to_string());
        allow.insert("ai.chat".to_string());
        let p = SandboxPolicy {
            allow,
            ..SandboxPolicy::strict()
        };
        assert!(p.check_builtin("memory.store").is_ok());
        assert!(p.check_builtin("ai.chat").is_ok());
        assert!(p.check_builtin("ai.stream").is_err());
        assert!(p.check_builtin("file.read").is_err());
    }

    #[test]
    fn deny_overrides_allow() {
        let mut allow = std::collections::BTreeSet::new();
        allow.insert("*".to_string());
        let mut deny = std::collections::BTreeSet::new();
        deny.insert("dangerous.*".to_string());
        let p = SandboxPolicy {
            allow,
            deny,
            ..SandboxPolicy::default()
        };
        assert!(p.check_builtin("safe.op").is_ok());
        assert!(p.check_builtin("dangerous.op").is_err());
    }

    #[test]
    fn path_rejects_parent_traversal() {
        let dir = temp_dir();
        let p = SandboxPolicy {
            fs_root: Some(dir.clone()),
            ..SandboxPolicy::permissive()
        };
        assert!(p.check_path("../etc/passwd").is_err());
        assert!(p.check_path("a/../../b").is_err());
    }

    #[test]
    fn path_accepts_in_root() {
        let dir = temp_dir();
        let sub = dir.join("sub");
        let _ = std::fs::create_dir_all(&sub);
        let p = SandboxPolicy {
            fs_root: Some(dir.clone()),
            ..SandboxPolicy::permissive()
        };
        assert!(p.check_path("sub/file.txt").is_ok());
        assert!(p.check_path("file.txt").is_ok());

        // cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_rejects_absolute_escape() {
        let dir = temp_dir();
        let p = SandboxPolicy {
            fs_root: Some(dir.clone()),
            ..SandboxPolicy::permissive()
        };
        // absolute path 试图逃出 root
        #[cfg(unix)]
        let bad = "/etc/passwd";
        #[cfg(windows)]
        let bad = "C:\\Windows\\System32";
        assert!(p.check_path(bad).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_no_root_rejects_all() {
        let p = SandboxPolicy::strict();
        assert!(p.check_path("anywhere.txt").is_err());
    }
}
