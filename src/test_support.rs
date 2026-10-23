//! 测试用临时目录 / 临时文件 —— **本仓唯一的临时文件创建入口**。
//!
//! # 为什么要有这个模块
//!
//! 修复前，全仓有 **163 处**「在 `%TEMP%` 下建目录 / 写文件」的动作，散落在
//! **145 个文件**里，每个站点**各写各的**：
//!
//! | 维度 | 修复前的形态 | 数量 |
//! |---|---|---|
//! | 命名 | `mora_d198_{tag}` / `mora_audit_{pid}_{nanos}` / `d363_{n}_{slug}` / `mora_sandbox_{pid}` / `mora_schema_test_{name}` … | **5 种以上** |
//! | 回收 | `Drop` 守卫 / 开头清一次 / 结尾清一次 / **完全不清** | **4 种** |
//! | 根目录 | 全部 `%TEMP%` | 1 种（但没有主人） |
//!
//! 后果是 `%TEMP%` 里累积了 **9 205 个空目录**（最早 2026-08-15）——
//! 根因不是「某几处忘了删」，而是**机制本身是碎的**：每加一个测试就要
//! 重新发明一次命名与回收，而「忘了删」只是默认结局。
//!
//! ⇒ 本模块把三件事**收敛成一处**：
//!
//! 1. **根目录**：优先用**仓库内**的 `target/test-scratch/`（gitignore 覆盖、
//!    `cargo clean` 顺带清掉）；仓库不可写时（如 crate 作为依赖被测试）
//!    退回 `%TEMP%/mora_test_scratch/`。
//! 2. **命名**：一处决定，带 pid + 纳秒时间戳 ⇒ 并行测试**天然不撞名**
//!    （修复前 `mora_sandbox_{pid}` 这类**按 pid 命名**的目录会被同一
//!    进程内的多个并行测试共享）。
//! 3. **回收**：`Drop` 时 `remove_dir_all` **整个目录** ⇒ panic（unwind）
//!    路径也不漏。
//!
//! # 用法
//!
//! ```ignore
//! use mora::test_support::TempDir;
//!
//! #[test]
//! fn something() {
//!     let dir = TempDir::new("audit");           // 独占目录，结束即回收
//!     let p = dir.path().join("x.jsonl");
//!     std::fs::write(&p, "…").unwrap();
//!     // … 用 p …
//! } // ← dir 在这里被 Drop，整个目录（含子文件）被删掉
//! ```
//!
//! `TempDir` 实现了 `Deref<Target = Path>`，所以 `dir.join("a")`、
//! `&dir` 当 `&Path` 用都可以**一字不改**。
//!
//! # 纪律一：**持有路径的测试必须同时持有守卫**
//!
//! 守卫是**唯一**的回收者 —— 它一析构，目录连同里面的文件当场消失。
//! 于是：
//!
//! - ❌ helper 返回裸路径（守卫随 helper 返回而析构 ⇒ 文件在用之前就没了）；
//! - ❌ `let f = temp_dir().join("a.mora");`（守卫是语句内的临时值 ⇒ 立刻析构）；
//! - ❌ `let md = md.to_string_lossy();`（同名遮蔽把守卫覆盖掉）。
//!
//! ✅ helper 返回 `(TempDir, PathBuf)`，调用方 `let (_guard, path) = …`；
//! ✅ 需要单个**文件**时用 [`TempFile`]（它只建父目录，文件本身不建 ——
//! 「测一个不存在的文件」正好用它）。
//!
//! # 纪律二：命名是机制的事，调用方不必自己 slug
//!
//! `TempDir::new` 会对 tag 做**文件名安全**规整（见 [`sanitize_tag`]），
//! 所以调用方可以随便传 tag（含表达式、空格、标点）。
//!
//! # 纪律
//!
//! **不要在别处直接调 `std::env::temp_dir()`。**
//! `tests/test_tempdir_hygiene.rs` 会扫全仓 `src/` + `tests/`，发现
//! `src/test_support.rs` 之外的直接调用就判失败 —— 碎片化**不许重新长出来**。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// 同一进程内的**单调计数**。
///
/// 纳秒时间戳已经足够唯一，但两台机器的时钟可能相同、且 `SystemTime` 在
/// Windows 上的分辨率有限 —— 再叠一个进程内计数，杜绝同毫秒内的碰撞。
static SEQ: AtomicU64 = AtomicU64::new(0);

/// 临时目录的**根** —— 本仓所有测试临时产物的唯一落点。
///
/// 优先仓库内 `target/test-scratch/`：
/// - 落在**构建产物**里 ⇒ 不会被系统清理策略扫掉，也不会污染用户目录；
/// - `target/` 已被 gitignore ⇒ 不会进版本库；
/// - `cargo clean` 顺带清掉 ⇒ 即使某次异常退出留下残留，下次构建就带走。
///
/// 仓库不可写时（crate 作为**依赖**被别处测试时 `CARGO_MANIFEST_DIR`
/// 指向只读的 registry 缓存）退回 `%TEMP%/mora_test_scratch/`。
fn scratch_root() -> PathBuf {
    let repo_local = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-scratch");
    if std::fs::create_dir_all(&repo_local).is_ok() {
        return repo_local;
    }
    std::env::temp_dir().join("mora_test_scratch")
}

/// 把 tag 规整成**文件名安全**的片段。
///
/// ## 为什么机制要管这件事
///
/// D475 之前，每个测试站点都得自己把 tag 规整好才能拼进路径 —— 于是全仓
/// 长出了一批各写各的 `slug()` 辅助函数（41 处复制品），而**漏掉一处**就是
/// 一个 `Os { code: 123, InvalidFilename }`（Windows 路径非法字符）。
///
/// ⇒ 命名既然是**机制**的事，规整也必须是机制的事：调用方随便传什么 tag，
/// 机制保证拼出来的路径**合法**。
///
/// 保留 `[A-Za-z0-9_-]`，其余（`:` `*` `?` `"` `<` `>` `|`、空白、路径分隔符
/// 等）一律替换为 `_`；并截断到 48 字符（留出后缀空间，也避免超长路径）。
fn sanitize_tag(tag: &str) -> String {
    let mut out = String::with_capacity(tag.len().min(48));
    for ch in tag.chars() {
        if out.len() >= 48 {
            break;
        }
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

/// 造一个**独占**的临时目录名。
fn unique_name(tag: &str) -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{}_{pid}_{nanos}_{seq}", sanitize_tag(tag))
}

/// 一个**独占**的临时目录，生命周期结束时**整树回收**。
///
/// 回收发生在 [`Drop`] —— Rust 的析构在 unwind（测试 panic）时同样执行，
/// 故**异常路径也不漏**。
///
/// [`Drop`]: std::ops::Drop
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// 建一个独占的临时目录。
    ///
    /// `tag` 只用于**人读**（在 `%TEMP%` / `target/test-scratch/` 里一眼看出
    /// 是哪个测试留下的），不参与唯一性。
    pub fn new(tag: &str) -> Self {
        let path = scratch_root().join(unique_name(tag));
        std::fs::create_dir_all(&path).expect("建测试临时目录");
        Self { path }
    }

    /// 目录路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 目录下的某个文件路径（**不创建**该文件）。
    pub fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

/// `AsRef<Path>` —— `Command::current_dir(&dir)` 这类**泛型边界**不会走
/// deref 强制转换，必须显式实现。
impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// `AsRef<OsStr>` —— `Path::new(&dir)` / `PathBuf::from(&dir)` 需要它。
impl AsRef<std::ffi::OsStr> for TempDir {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.path.as_os_str()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // 整树删除：测试常在目录里散落多个文件，只删目录会漏文件。
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// 一个**独占**的临时文件，生命周期结束时回收。
///
/// 与 [`TempDir`] 同源：同一条根目录、同一套命名、同一套回收策略。
pub struct TempFile {
    path: PathBuf,
}

impl TempFile {
    /// 在临时根目录下造一个**独占**的临时文件路径（文件本身**不**创建）。
    pub fn new(tag: &str, name: &str) -> Self {
        let path = scratch_root().join(unique_name(tag)).join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("建测试临时目录");
        }
        Self { path }
    }

    /// 文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl std::ops::Deref for TempFile {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

/// `AsRef<Path>` —— `fs::remove_file(&file)` 这类**泛型边界**不会走 deref
/// 强制转换，必须显式实现。
impl AsRef<Path> for TempFile {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// `AsRef<OsStr>` —— `Path::new(&file)` / `PathBuf::from(&file)` 需要它。
impl AsRef<std::ffi::OsStr> for TempFile {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.path.as_os_str()
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
}
