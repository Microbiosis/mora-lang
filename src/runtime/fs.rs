//! v0.110.0: 文件系统隔离策略（`FsPolicy`）与不可绕过路径凭证（`CheckedPath`）。
//!
//! 所有经过运行时执行的文件操作（`file.*` 与 `ControlInst::Import`）必须先经由
//! [`FsContext::resolve_path`] 验证并构造 [`CheckedPath`]，再交由
//! [`crate::runtime::reactor::AsyncIoReactor`] 在非阻塞线程池上执行：
//! - 拒绝 NUL 字节与空路径，消除路径截断漏洞；
//! - `virtual_cwd` 由每个 [`crate::runtime::CoreRuntime`] 独立持有，彻底替代会污染
//!   全进程的 `std::env::set_current_dir`（解决并发竞态与 `chdir` 沙箱逃逸）；
//! - 在 `FsPolicy::Confined` 模式下逐段展开 `.` / `..` 与符号链接（含悬空符号链接），
//!   阻止任何词法 traversal 或 symlink 越狱。

use std::collections::VecDeque;
use std::path::{Component, Path, PathBuf};

use crate::error::{MoraError, MoraResult};

/// 符号链接展开最大跳数，防止自指符号链接死循环。
const MAX_SYMLINK_HOPS: usize = 40;

/// 运行时文件系统访问策略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsPolicy {
    /// 宿主 CLI 默认模式：保留全部路径安全校验与独立 `virtual_cwd`，不限制沙箱根目录。
    Unrestricted,
    /// 严格受限沙箱：所有路径解析与符号链接展开结果必须位于规范化 `root` 之内。
    Confined {
        /// 规范化后的沙箱根目录。
        root: PathBuf,
    },
}

/// 经 [`FsContext::resolve_path`] 校验通过后的路径凭证（内部字段私有，防止未检构造）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedPath {
    resolved: PathBuf,
}

impl CheckedPath {
    /// 读取已验证的目标绝对路径（供宿主机文件 I/O 与模块加载使用）。
    pub fn as_path(&self) -> &Path {
        &self.resolved
    }
}

/// 每个 [`crate::runtime::CoreRuntime`] 持有的文件系统策略与隔离工作目录。
#[derive(Debug, Clone)]
pub struct FsContext {
    policy: FsPolicy,
    virtual_cwd: PathBuf,
}

impl FsContext {
    /// 构造默认未限根的文件系统上下文（`virtual_cwd` 取当前进程工作目录）。
    pub fn unrestricted() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let virtual_cwd = cwd.canonicalize().unwrap_or(cwd);
        Self {
            policy: FsPolicy::Unrestricted,
            virtual_cwd,
        }
    }

    /// 构造严格限制在 `root` 目录内的文件系统上下文（`virtual_cwd` 固定为规范化 `root`）。
    pub fn confined(root: impl AsRef<Path>) -> MoraResult<Self> {
        let raw_root = root.as_ref();
        let canon_root = raw_root.canonicalize().map_err(|e| {
            MoraError::Io(format!(
                "failed to initialize confined fs_root '{}': {e}",
                raw_root.display()
            ))
        })?;
        if !canon_root.is_dir() {
            return Err(MoraError::Io(format!(
                "fs_root '{}' is not a directory",
                canon_root.display()
            )));
        }
        Ok(Self {
            policy: FsPolicy::Confined {
                root: canon_root.clone(),
            },
            virtual_cwd: canon_root,
        })
    }

    /// 返回当前文件系统策略。
    pub fn policy(&self) -> &FsPolicy {
        &self.policy
    }

    /// 返回当前运行时的隔离工作目录（不读取或污染全局 `std::env::set_current_dir`）。
    pub fn cwd(&self) -> &Path {
        &self.virtual_cwd
    }

    /// 更新当前运行时的隔离工作目录（先校验目标存在且为合法目录，受限模式下必须处于 `fs_root` 内）。
    pub fn chdir(&mut self, raw: &str, op: &str) -> MoraResult<()> {
        let checked = self.resolve_path(raw, op)?;
        let meta = std::fs::metadata(checked.as_path())
            .map_err(|e| MoraError::Io(format!("{op}: cannot chdir to '{}': {e}", raw)))?;
        if !meta.is_dir() {
            return Err(MoraError::Io(format!("{op}: '{}' is not a directory", raw)));
        }
        let canon = checked
            .as_path()
            .canonicalize()
            .unwrap_or_else(|_| checked.as_path().to_path_buf());
        if let FsPolicy::Confined { root } = &self.policy
            && !canon.starts_with(root)
        {
            return Err(MoraError::Runtime(format!(
                "sandbox denied: {op} path '{raw}' escapes fs_root"
            )));
        }
        self.virtual_cwd = canon;
        Ok(())
    }

    /// 返回宿主机主目录（在 [`FsPolicy::Confined`] 下显式拒绝，避免泄露宿主环境）。
    pub fn home_dir(&self, op: &str) -> MoraResult<String> {
        if matches!(self.policy, FsPolicy::Confined { .. }) {
            return Err(MoraError::Runtime(format!(
                "sandbox denied: {op} is disabled under confined FsPolicy"
            )));
        }
        std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map_err(|_| MoraError::Runtime(format!("{op}: HOME/USERPROFILE not set")))
    }

    /// 解析并校验路径，返回不可伪造的 [`CheckedPath`] 凭证。
    pub fn resolve_path(&self, raw: &str, op: &str) -> MoraResult<CheckedPath> {
        if raw.contains('\0') {
            return Err(MoraError::Runtime(format!(
                "{op}: invalid NUL byte in path"
            )));
        }
        if raw.is_empty() {
            return Err(MoraError::Runtime(format!("{op}: path must not be empty")));
        }
        let input_path = Path::new(raw);
        let mut comps = input_path.components().peekable();
        if let Some(Component::Prefix(_)) = comps.peek() {
            let mut peek_iter = input_path.components();
            let _ = peek_iter.next();
            if !matches!(peek_iter.next(), Some(Component::RootDir)) {
                return Err(MoraError::Runtime(format!(
                    "{op}: drive-relative paths are not supported: '{raw}'"
                )));
            }
        }

        match &self.policy {
            FsPolicy::Unrestricted => {
                let joined = if input_path.is_absolute() {
                    input_path.to_path_buf()
                } else {
                    self.virtual_cwd.join(input_path)
                };
                Ok(CheckedPath {
                    resolved: lexical_normalize(&joined),
                })
            }
            FsPolicy::Confined { root } => {
                let resolved = resolve_confined(&self.virtual_cwd, root, raw, input_path, op)?;
                Ok(CheckedPath { resolved })
            }
        }
    }
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(
                    out.components().next_back(),
                    None | Some(Component::RootDir | Component::Prefix(_))
                ) {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn resolve_confined(
    virtual_cwd: &Path,
    canon_root: &Path,
    raw: &str,
    input_path: &Path,
    op: &str,
) -> MoraResult<PathBuf> {
    let was_relative = !input_path.is_absolute();
    let mut resolved = if was_relative {
        virtual_cwd.to_path_buf()
    } else {
        PathBuf::new()
    };

    let mut queue: VecDeque<PathBuf> = input_path
        .components()
        .map(|c| PathBuf::from(c.as_os_str()))
        .collect();
    let mut symlink_hops = 0usize;

    while let Some(step) = queue.pop_front() {
        let Some(comp) = Path::new(&step).components().next() else {
            continue;
        };
        match comp {
            Component::Prefix(p) => {
                resolved.push(p.as_os_str());
            }
            Component::RootDir => {
                resolved.push(comp.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !resolved.pop() {
                    return Err(MoraError::Runtime(format!(
                        "sandbox denied: {op} path '{raw}' escapes fs_root"
                    )));
                }
                if was_relative && !resolved.starts_with(canon_root) {
                    return Err(MoraError::Runtime(format!(
                        "sandbox denied: {op} path '{raw}' escapes fs_root"
                    )));
                }
            }
            Component::Normal(seg) => {
                resolved.push(seg);
                // 仅对存在的组件做符号链接/junction 规范化展开；对悬空 symlink 同样展开目标以拦逃逸。
                if let Ok(meta) = std::fs::symlink_metadata(&resolved) {
                    if meta.file_type().is_symlink() {
                        symlink_hops += 1;
                        if symlink_hops > MAX_SYMLINK_HOPS {
                            return Err(MoraError::Io(format!(
                                "{op}: too many levels of symbolic links resolving '{raw}'"
                            )));
                        }
                        let target = std::fs::read_link(&resolved).map_err(|e| {
                            MoraError::Io(format!("{op}: cannot read symlink '{raw}': {e}"))
                        })?;
                        resolved.pop();
                        if target.is_absolute() {
                            resolved.clear();
                        }
                        for c in target.components().rev() {
                            queue.push_front(PathBuf::from(c.as_os_str()));
                        }
                    } else {
                        match resolved.canonicalize() {
                            Ok(canon) => resolved = canon,
                            Err(e) => {
                                return Err(MoraError::Io(format!(
                                    "{op}: cannot resolve path '{}': {e}",
                                    raw
                                )));
                            }
                        }
                    }
                }
            }
        }
    }

    if !resolved.starts_with(canon_root) {
        return Err(MoraError::Runtime(format!(
            "sandbox denied: {op} path '{raw}' escapes fs_root"
        )));
    }
    Ok(resolved)
}
