//! v0.33+: Sandbox — path validation + pattern allow/deny + capability tokens
//!
//! 灵感:
//! - **MimiClaw** path validation: read_file/write_file 拒绝 `..` 路径
//!   (main/tools/tool_files.c:15-31)
//! - **AIOS** access manager: agent_id -> privilege_group (hashmap)
//! - **Puter** iframe sandbox: credentialless + capability-based URL params
//! - **loongclaw** (v0.42.0) Capability token system: token_id + allowed +
//!   expires_at + generation, PolicyEngine trait (issue/authorize/revoke)
//!
//! v0.33 实现:
//! - Path safety: 拒绝含 `..` 或绝对路径越界 (out of root) 的操作
//! - Pattern allow/deny: builtin 名 match (wildcard `*` segment, 与 event 一致)
//! - Privilege: thread-local sandbox context (类似 thread_local!)
//!
//! v0.42.0 增补:
//! - CapabilityStore: token-based 细粒度授权
//!   与 pattern-based allow/deny 并存 (后者用于 builtin dispatch, 前者用于
//!   runtime `sandbox.key { ... }` builtin 调用)

use std::path::{Component, Path, PathBuf};

// v0.42.0: Capability Token System (loongclaw-inspired)
// See capability.rs for full docs.
pub mod capability;
pub use capability::{Capability, CapabilityStore, CapabilityToken, SandboxError};

// v0.44.0: Container sandbox + REAL Docker orchestration (pi-mono inspired)
// See container.rs for full docs.
pub mod container;
pub use container::{
    ContainerBackend, ContainerHandle, ContainerSpec, MountSpec, NetworkMode, ResourceLimits,
    generate_container_name, spawn_container,
};

/// v0.33+: Sandbox 策略
#[derive(Debug, Clone, Default)]
pub struct SandboxPolicy {
    /// v0.36 (P1-3.10): BTreeSet for O(log N) membership checks
    /// (was Vec`<String>`, O(N) linear scan).
    pub allow: std::collections::BTreeSet<String>,
    /// 禁止的 builtin 模式 (优先于 allow)
    pub deny: std::collections::BTreeSet<String>,
    /// 文件操作根目录 (path validation 基准)
    pub fs_root: Option<PathBuf>,
    /// 超时秒数 (None = 无限制)
    pub timeout_s: Option<u64>,
    /// 内存限制 MB (None = 无限制)
    pub memory_limit_mb: Option<u64>,
    /// v0.42.0: Capability token store (loongclaw-inspired)
    /// 默认 new() 一个空 store, builtin 不可见; 业务代码通过 sandbox.key 发放
    pub capabilities: CapabilityStore,
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
            capabilities: CapabilityStore::new(),
        }
    }

    /// 创建一个开放 policy (允许一切 builtin, 全路径, 无限制)
    ///
    /// v0.104.6 D409：此 doc 此前与行为**不符**。
    ///
    /// `fs_root = Some("/")` 本意是「**不限制**」，但 `check_path` 把它当
    /// **真路径**处理 ⇒ `canonicalize("/")` 在 Windows 上落到**当前工作目录
    /// 所在的盘**（`\\?\D:\`）⇒ 「不限制」实际变成了「**只能访问当前盘**」。
    ///
    /// 实测：cwd 在 D: 时 `check_path("C:/Windows/win.ini")` 被拒。
    ///
    /// 这与两处明文矛盾：
    /// - 本 doc 的「全路径, 无限制」；
    /// - `docs/mora-spec.md` 17.1：「当前版本**无沙箱**。脚本可以读写文件系统」
    ///   （「文件系统访问白名单」列为 **v1.0 计划**）。
    ///
    /// ⇒ 修法在 [`Self::check_path`]：把字面量 `/` **显式**定义为「无限制」
    /// 哨兵，而不是让它参与根边界比较。POSIX 上 `/` 本就是全盘根，
    /// 该分支不改变任何行为。
    pub fn permissive() -> Self {
        let mut allow = std::collections::BTreeSet::new();
        allow.insert("*".to_string());
        Self {
            allow,
            deny: std::collections::BTreeSet::new(),
            fs_root: Some(PathBuf::from(UNRESTRICTED_FS_ROOT)),
            timeout_s: None,
            memory_limit_mb: None,
            capabilities: CapabilityStore::new(),
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
    ///
    /// v0.104.6 D335：修「**所有绝对路径都被拒**」。
    ///
    /// 修前两侧的形式**不一致**：`fs_root` 经 `canonicalize` 后在 Windows 上
    /// 是**逐字路径** `\\?\D:\`（逐字 = verbatim，`\\?\` 前缀），而用户传入的
    /// 绝对路径是**普通形式** `D:\Github\...`。`Path::starts_with` 按
    /// **组件逐个**比较，首个组件 `\\?\D:\` vs `D:\` 就不同 ⇒ **永远 false**。
    ///
    /// 实测后果（`fs_root = permissive() = PathBuf::from("/")`）：
    /// ```text
    /// file.exists("Cargo.toml")            → true   ← 相对路径 OK
    /// file.exists("D:/Github/.../Cargo.toml") → **sandbox denied**   ← 工作区自己的文件
    /// file.is_dir(file.cwd())              → **sandbox denied**      ← 自己返回的路径自己不能用
    /// file.exists(file.abs("Cargo.toml"))  → **sandbox denied**      ← 自己算的路径自己不能用
    /// file.is_dir("D:/")                   → **sandbox denied**      ← 盘根
    /// ```
    /// 而 `docs/mora-spec.md:1504` 明写「当前版本**无沙箱**。脚本可以读写文件系统」，
    /// 「文件系统访问白名单」是 **v1.0 计划** ⇒ 修前既违背 spec、又自相矛盾。
    ///
    /// 修法两条：
    /// ① **两侧归一到同一形式**再比：都剥掉 `\\?\` / `\\?\UNC\` 前缀；
    /// ② 对**已存在**的 `resolved` 也做 `canonicalize`（失败则退回原路径，
    ///    因为 `write_text` 的目标常常尚不存在）—— 这顺带堵住了
    ///    「沙箱内的符号链接 / junction 指向沙箱外」这条逃逸路径。
    ///
    /// v0.104.6 D409：修「D335 修完后 `permissive()` 在 Windows 上仍只覆盖
    /// **当前盘**」。`fs_root = "/"` 会被 `canonicalize` 解析成 `\\?\D:\`，
    /// 于是「无限制」退化成「当前盘」，与本 doc 及 spec 17.1 双重矛盾。
    /// 修法：把 `/` 当成**显式哨兵**（`UNRESTRICTED_FS_ROOT`），在 `canonicalize`
    /// 之前就判定「无限制」，不参与根边界比较。
    ///
    /// ⚠ 返回值说明：全仓**没有任何调用方**拿返回值去操作文件
    /// （`file.rs` 一律 `check_path(&path)?;` 后用原字符串），
    /// 消费方只有 `sandbox.check_path` builtin 的 `.is_ok()`。
    /// 故下面对**相对路径**的解析口径可以安全地改成「按 cwd」。
    pub fn check_path(&self, path: &str) -> Result<PathBuf, String> {
        let p = Path::new(path);
        // 1. 拒绝含 `..` component
        //
        // ⚠ 这是**明文决定**（模块头「Path safety: 拒绝含 `..` 或绝对路径越界
        // (out of root) 的操作」），且与 fs_root 边界**正交**：
        // 即便无限制，`..` 仍被拒。按 D397/D404 纪律**只报告不擅动** ——
        // 代价是 `permissive()` 并非字面意义的「一切路径皆可」，
        // `file.read_text("../x.txt")` 仍会报 `path traversal`。
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
        // D409：哨兵优先 —— 无限制时**不**做 canonicalize / 边界比较。
        // 否则 `canonicalize("/")` 会把「无限制」压成「当前盘」。
        if is_unrestricted(&root) {
            let resolved = if p.is_absolute() {
                p.to_path_buf()
            } else {
                // 无限制时按 **cwd** 解析。若沿用 `canonical_root.join(p)`，
                // Windows 上会把 `check_path("Cargo.toml")` 指到 `D:\Cargo.toml`
                // —— 盘根，而非进程工作目录。
                std::env::current_dir()
                    .map_err(|e| format!("path '{}' rejected: cannot resolve cwd: {}", path, e))?
                    .join(p)
            };
            return Ok(resolved);
        }
        let canonical_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        let resolved = if p.is_absolute() {
            p.to_path_buf()
        } else {
            canonical_root.join(p)
        };
        // 2a. 目标**已存在**时先 canonicalize —— 解开符号链接 / junction，
        //     否则「沙箱内 → 沙箱外」的链接会被当成「在沙箱内」放行。
        let resolved_canon = std::fs::canonicalize(&resolved).unwrap_or_else(|_| resolved.clone());

        // 2b. 两侧都剥掉 verbatim（`\\?\`）前缀后再比。
        //     不剥的话：root 是 `\\?\D:\`、resolved 是 `D:\...`，
        //     `starts_with` 按组件比，首组件就不同 ⇒ 一切绝对路径被拒。
        let root_cmp = strip_verbatim(&canonical_root);
        let resolved_cmp = strip_verbatim(&resolved_canon);
        if !resolved_cmp.starts_with(&root_cmp) {
            return Err(format!(
                "path '{}' escapes fs_root '{}'",
                resolved.display(),
                root_cmp.display()
            ));
        }
        Ok(resolved)
    }
}

/// 「**不限制**文件系统访问」的 `fs_root` 哨兵字面量。
///
/// v0.104.6 D409：见 [`SandboxPolicy::permissive`] 的 doc。
/// 单独提成常量，是为了让「这个字面量有特殊含义」这件事**只有一处**。
pub const UNRESTRICTED_FS_ROOT: &str = "/";

/// `fs_root` 是否表示「**不限制**文件系统访问」。
///
/// v0.104.6 D409。**在 `canonicalize` 之前**判定 —— 一旦 canonicalize，
/// Windows 上 `/` 会变成 `\\?\D:\`，与「用户显式设了 `D:\`」**无法区分**。
///
/// POSIX 上 `"/"` 本就是全盘根 ⇒ 命中此分支与走原逻辑**等价**，
/// 所以该分支不改变任何 POSIX 行为，只修正 Windows。
fn is_unrestricted(root: &Path) -> bool {
    root == Path::new(UNRESTRICTED_FS_ROOT)
}

/// 剥掉 Windows **逐字路径**前缀：`\\?\UNC\server\share` → `\\server\share`，
/// `\\?\C:\x` → `C:\x`。非逐字路径原样返回。
///
/// 为什么需要它：`std::fs::canonicalize` 在 Windows 上**总是**返回逐字形式，
/// 而调用方手里的路径通常不是。`Path::starts_with` 按**组件**比较，
/// 两种形式的首组件（`\\?\D:` vs `D:`）不同 ⇒ 一切绝对路径都会被误判为「逃逸」。
fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.as_os_str().to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p.to_path_buf()
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
