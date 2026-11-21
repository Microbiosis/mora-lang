//! v0.110.1: `vendor/` 包目录的**存储登记与卫生清扫**。
//!
//! ## `mora.lock` —— 「哪些文件是本工具装出来的」的唯一事实源
//!
//! 安装登记表，每行 `name = "url"`。它让清扫能把**工具装出来的包**与
//! **项目自带的 vendored 文件**（如本仓 `vendor/hello.mora` 这类样例输入）区分开。
//!
//! ## 清扫判据（历史残留 = 旧版 `install` 留下的 HTTP 错误页）
//!
//! v0.110.1 之前的 `mora install` 不校验 HTTP 状态 ⇒ 404 / 502 的**错误页响应体**
//! 被原样写进 `vendor/<pkg>.mora`，并在 `mora.lock` 里登记了该包 ——
//! 这些文件在磁盘上与「正常安装的包」**无从区分**，直到有人真的 import 它。
//!
//! 判定为残留需**同时**满足两条：
//!
//! 1. **不是合法的 Mora 程序**（`ParserV3::compile` 失败）—— 装出来的包必须能编译；
//! 2. **是本工具装出来的**（登记在 `mora.lock`）**或**内容带 HTTP 错误页特征。
//!
//! 第 1 条保证**项目自带的样例包不会被误删**（能编译即保留）；第 2 条保证
//! 「用户手写但写坏的包」不会被无端删除（既不在登记表、也不像错误页 ⇒ 一律保留）。
//!
//! 判定用 `compile`（解析 + 降低）而**不是**类型检查：包 `import` 兄弟模块时
//! 类型检查会因模块缺失报错，但那是**环境问题**、不是文件损坏。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::parser_v3::ParserV3;

/// `mora.lock` 的内容：`包名 → URL`。
#[derive(Debug, Default, Clone)]
pub(crate) struct LockFile {
    entries: BTreeMap<String, String>,
}

impl LockFile {
    /// 读取 `mora.lock`；文件不存在或读不动即视为**空登记表**。
    pub(crate) fn load(path: &Path) -> Self {
        let Ok(text) = fs::read_to_string(path) else {
            return Self::default();
        };
        Self::parse(&text)
    }

    /// 解析 `mora.lock` 文本（每行 `name = "url"`）。
    pub(crate) fn parse(text: &str) -> Self {
        let mut entries = BTreeMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((name, url)) = line.split_once('=') else {
                continue;
            };
            let name = name.trim();
            let url = url.trim().trim_matches('"').trim();
            if name.is_empty() {
                continue;
            }
            entries.insert(name.to_string(), url.to_string());
        }
        Self { entries }
    }

    /// 包名是否已登记。
    pub(crate) fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    /// 登记一个包（已存在则**不覆盖** —— 与旧实现语义一致）。
    pub(crate) fn insert(&mut self, name: &str, url: &str) {
        self.entries
            .entry(name.to_string())
            .or_insert_with(|| url.to_string());
    }

    /// 移除一个包，返回是否真的移除了。
    pub(crate) fn remove(&mut self, name: &str) -> bool {
        self.entries.remove(name).is_some()
    }

    /// 写回磁盘（按包名排序，保证输出稳定）。
    pub(crate) fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut text = String::new();
        for (name, url) in &self.entries {
            text.push_str(&format!("{name} = \"{url}\"\n"));
        }
        fs::write(path, text)
    }
}

/// 清扫结果。
#[derive(Debug, Default)]
pub(crate) struct VendorSweep {
    /// 被删除的残留文件（包名）。
    pub removed: Vec<String>,
    /// 被一并清除的 `mora.lock` 条目（包名）。
    pub lock_removed: Vec<String>,
}

impl VendorSweep {
    /// 是否清掉了东西。
    pub(crate) fn is_empty(&self) -> bool {
        self.removed.is_empty()
    }
}

/// 扫描 `vendor_dir`，删除历史残留，并同步清理 `mora.lock` 条目。
///
/// 判据见模块文档。返回被删除的包名与被清除的登记条目。
pub(crate) fn sweep_vendor(vendor_dir: &Path, lock: &mut LockFile) -> VendorSweep {
    let mut sweep = VendorSweep::default();
    let Ok(rd) = fs::read_dir(vendor_dir) else {
        return sweep;
    };
    let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.extension().and_then(|s| s.to_str()) != Some("mora") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let installed_by_tool = lock.contains(name);
        if !installed_by_tool && !looks_like_http_error_page(&text) {
            continue;
        }
        if compiles_as_mora(&text) {
            continue;
        }
        if fs::remove_file(&path).is_ok() {
            sweep.removed.push(name.to_string());
            if installed_by_tool && lock.remove(name) {
                sweep.lock_removed.push(name.to_string());
            }
        }
    }
    sweep
}

/// 内容是否是**可编译的 Mora 程序**（解析 + 降低成功）。
///
/// 只看语法与结构：包 `import` 兄弟模块时类型检查会因模块缺失报错，
/// 但那是**环境问题**，不该把包判成残留。
fn compiles_as_mora(text: &str) -> bool {
    ParserV3::compile(text).is_ok()
}

/// 内容是否带 HTTP 错误页特征（**只看开头**）。
///
/// 只认「文件**本身就是**错误页」：HTML 文档以 `<!DOCTYPE html` / `<html` 开头，
/// 网关的纯文本错误体以 `502 Bad Gateway` 这类状态短语开头。
/// 正文中**提到**这些字样的正常包不算（`print("404 Not Found")` 不是错误页）。
///
/// 本函数只提供**线索** —— 最终是否删除还要过「不是合法 Mora 程序」那一关。
fn looks_like_http_error_page(text: &str) -> bool {
    let head: String = text.chars().take(512).collect();
    let head = head
        .trim_start()
        .trim_start_matches('\u{FEFF}')
        .to_ascii_lowercase();
    const MARKERS: &[&str] = &[
        "<!doctype html",
        "<html",
        "<head>",
        "<body",
        "<?xml",
        "403 forbidden",
        "404 not found",
        "500 internal server error",
        "502 bad gateway",
        "503 service unavailable",
        "504 gateway timeout",
    ];
    MARKERS.iter().any(|m| head.starts_with(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_parsing_reads_name_and_url() {
        let lock = LockFile::parse("alpha = \"http://h/a.mora\"\nbeta = \"http://h/b.mora\"\n");
        assert!(lock.contains("alpha"));
        assert!(lock.contains("beta"));
        assert!(!lock.contains("gamma"));
    }

    #[test]
    fn lock_parsing_tolerates_junk_lines() {
        let lock = LockFile::parse("# 注释\n\nnot-an-entry\n  spaced  =  \"http://h/x.mora\"  \n");
        assert!(lock.contains("spaced"));
        assert_eq!(lock.entries.len(), 1);
    }

    #[test]
    fn lock_insert_does_not_overwrite_and_remove_reports() {
        let mut lock = LockFile::default();
        lock.insert("pkg", "http://h/1");
        lock.insert("pkg", "http://h/2");
        assert!(lock.remove("pkg"));
        assert!(!lock.remove("pkg"));
    }

    #[test]
    fn error_page_signature_matches_html_and_status_phrases() {
        assert!(looks_like_http_error_page(
            "<!DOCTYPE html><html><body>404 Not Found</body></html>"
        ));
        assert!(looks_like_http_error_page("<html><body>oops</body></html>"));
        assert!(looks_like_http_error_page("502 Bad Gateway\n"));
        // 正常 Mora 源码不该被误判。
        assert!(!looks_like_http_error_page(
            "export task hello()\n  return \"hi\"\nend\n"
        ));
        assert!(!looks_like_http_error_page("print(\"404 Not Found\")\n"));
    }

    #[test]
    fn valid_mora_compiles_and_error_page_does_not() {
        assert!(compiles_as_mora(
            "export task hello()\n  return \"hi\"\nend\n"
        ));
        assert!(!compiles_as_mora("<html><body>404 Not Found</body></html>"));
        assert!(!compiles_as_mora("export task broken(\n"));
    }
}
