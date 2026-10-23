//! v0.104.6 D475：临时目录**卫生守卫** —— 全仓**唯一入口**之外不得直接调
//! `std::env::temp_dir()`。
//!
//! ## 为什么要有这条
//!
//! D475 之前，全仓有 **163 处**「在 `%TEMP%` 下建目录/写文件」的动作散落在
//! **145 个文件**里，每个站点**各写各的**命名与回收策略（5 种以上命名、4 种
//! 回收方式：RAII 守卫 / 开头清一次 / 结尾清一次 / **完全不清**）。
//!
//! 后果不是「某几处忘了删」，而是**机制本身是碎的** —— 每加一个测试就要重新
//! 发明一次命名与回收，「忘了删」只是默认结局。实测 `%TEMP%` 里累积了
//! **9 205 个空目录**（最早 2026-08-15）。
//!
//! D475 把它们收敛到 `mora::test_support::TempDir`（唯一根目录、唯一命名、
//! 唯一回收策略）。**但收敛本身会回退** —— 下一个测试作者完全可能再写一次
//! `std::env::temp_dir().join(...)`。
//!
//! ⇒ 本条把「不许绕过」变成**机器可判**的红：CI 一旦看到入口之外的新调用
//! 就失败。这才是系统性修复的闭环 —— 机制 + 入口 + **防回退**。
//!
//! ## 允许清单
//!
//! 少数站点确实需要**系统临时目录本身**（而不是 scratch 空间）：
//! 它们只把 `%TEMP%` 当作**子进程的工作目录**或**只读路径**，不创建任何东西。
//! 逐条列在 [`ALLOWED`] 里，每条都写明理由 —— 新增例外必须在这里登记，
//! 否则 CI 会红。

use std::path::{Path, PathBuf};

/// 允许直接调用 `temp_dir()` 的文件 —— 逐条写明理由。
///
/// 每条要么把 `%TEMP%` 当**子进程的工作目录**（不创建任何东西），
/// 要么只是**读一个既有路径**，要么就是那唯一入口本身。
const ALLOWED: &[(&str, &str)] = &[
    (
        "src/test_support.rs",
        "唯一入口本身：它决定 scratch 根目录（仓库内 target/test-scratch，退回 %TEMP%）",
    ),
    (
        "tests/test_tempdir_hygiene.rs",
        "守卫自身：它的文档与断言里必须提到 temp_dir() 这个名字",
    ),
    (
        "src/interpreter/builtins/ai.rs",
        "HOME 兜底（读用户主目录），不是 scratch 空间",
    ),
    (
        "tests/mcp_tool_catalog_honesty.rs",
        "把 %TEMP% 当子进程 cwd（只跑命令，不创建文件）",
    ),
    (
        "tests/skill_load_sandbox_guard.rs",
        "把 %TEMP% 当子进程 cwd（只跑命令，不创建文件）",
    ),
    (
        "tests/repl_eof_exit.rs",
        "把 %TEMP% 当子进程 cwd（只跑 REPL，不创建文件）",
    ),
    (
        "tests/document_methods_introspection.rs",
        "只读一个既有路径（document 解析输入），不创建",
    ),
    (
        "tests/spec_builtin_surface.rs",
        "只读一个既有路径（builtin 面盘点），不创建",
    ),
];

/// 递归收集目录下的所有 `.rs` 文件。
fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_rs(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// **主判据**：入口之外不得有直接的 `temp_dir()` 调用。
#[test]
fn no_temp_dir_calls_outside_the_shared_entry_point() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    collect_rs(&root.join("src"), &mut files);
    collect_rs(&root.join("tests"), &mut files);

    let mut violations: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for f in &files {
        let rel = f
            .strip_prefix(root)
            .unwrap_or(f)
            .to_string_lossy()
            .replace('\\', "/");
        let Ok(text) = std::fs::read_to_string(f) else {
            continue;
        };
        let hits: Vec<usize> = text
            .lines()
            .enumerate()
            .filter(|(_, l)| {
                let t = l.trim();
                // 只看**代码行**（跳过注释与文档）
                !t.starts_with("//")
                    && (t.contains("std::env::temp_dir()") || t.contains("env::temp_dir()"))
            })
            .map(|(i, _)| i + 1)
            .collect();
        if hits.is_empty() {
            continue;
        }
        checked += 1;

        if ALLOWED.iter().any(|(a, _)| *a == rel) {
            continue;
        }
        for h in hits {
            violations.push(format!("  {rel}:{h}"));
        }
    }

    assert!(
        violations.is_empty(),
        "发现 {} 处**绕过唯一入口**的 temp_dir() 调用 —— 临时目录必须走 \
         `mora::test_support::TempDir`（唯一根目录 / 唯一命名 / 唯一回收策略）。\n\
         直接调 `std::env::temp_dir()` 会重新引入「各站点各写各的」的碎片化 —— \
         D475 之前那 9 205 个 %TEMP% 残留就是这么来的。\n\
         若此处确有必要，请改用 `TempDir`，或把它登记进本文件的 `ALLOWED`（并写明理由）。\n{}",
        violations.len(),
        violations.join("\n")
    );
    assert!(
        checked > 0,
        "一个 temp_dir() 调用都没扫到 —— 扫描器本身坏了（会让本条**恒绿**）"
    );
}

/// **允许清单自身不许腐化**：登记的例外必须**仍然存在**且**仍然合理**。
///
/// 清单里若留了一条已经改掉的路径，那条例外就悄悄失效了 —— 它不再被检查，
/// 但也没人知道。⇒ 本条钉住「清单里的每一条都还在那个文件里、且那里确实
/// 还有 temp_dir() 调用」。
#[test]
fn allowlist_entries_are_still_actual_temp_dir_users() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut stale = Vec::new();
    for (rel, why) in ALLOWED {
        let p = root.join(rel);
        if !p.exists() {
            stale.push(format!("  {rel} —— 文件已不存在（理由：{why}）"));
            continue;
        }
        let text = std::fs::read_to_string(&p).unwrap_or_default();
        if !(text.contains("std::env::temp_dir()") || text.contains("env::temp_dir()")) {
            stale.push(format!(
                "  {rel} —— 里面已没有 temp_dir() 调用了，这条例外已失效（理由：{why}）"
            ));
        }
    }
    assert!(
        stale.is_empty(),
        "允许清单里有 {} 条已经失效的例外 —— 请删除它们：\n{}",
        stale.len(),
        stale.join("\n")
    );
}
