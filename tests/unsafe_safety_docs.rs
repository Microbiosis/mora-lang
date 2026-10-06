//! v0.104.6 D252 —— 每个 `unsafe` **块**都必须带 SAFETY 论据。
//!
//! ## 背景：为什么先做可达性普查
//!
//! D250 的经验是「静态普查容易在噪声里漏掉真信号」，所以本轮先普查
//! **可达性**再普查文档：
//!
//! | 文件 | unsafe | 参与编译 | 真实执行 |
//! |---|---|---|---|
//! | `mir/jit.rs` | 10 | ✅ | **仅测试**：`LegacyJitBackend::try_compile` 是恒定 `CompileReject` 的占位实现（生产回落解释器），真正执行的是 `tests/jit_compile.rs` 的 19 个差分测试 |
//! | `mir/lmir_to_mir.rs` | 1 | ❌ **`mir/mod.rs` 未声明该模块** | 永不执行 |
//! | `sandbox/container.rs` / `interpreter/builtins/exec.rs` | 3 | ✅ | **`#[cfg(unix)]`，Windows 上不编译** |
//! | `document/backend/image.rs` | 3 | ✅ | 仅测试（`EnvGuard` 已有 SAFETY） |
//!
//! ⇒ 高危 unsafe 集中在 JIT，而 JIT 的**生产路径是关闭的**。
//! `lmir_to_mir.rs` 那处 `from_utf8_unchecked` 是**定时炸弹**：不参与编译所以
//! 现在无害，但一旦有人补 `mod lmir_to_mir;`，没有 null / 长度 / UTF-8 检查的
//! 裸指针解引用会**立刻变成真实 UB**。
//!
//! ## 本判据的作用
//!
//! 普查结果是「10 个真正的 `unsafe` 块缺 SAFETY 论据」，已逐一补齐
//! （论据均按实际代码逐处撰写，不是模板）。本判据把这条要求固化：
//! 将来新增 `unsafe` 而忘了写论据，立刻变红并点名文件:行号。
//!
//! **豁免两类**（它们不是 `unsafe` 块，不需要 SAFETY）：
//! - `unsafe extern "C" { … }` / `unsafe extern "system" { … }` —— **声明**，不执行；
//! - `fn … -> unsafe extern "C" fn(…)` —— `unsafe` 属于**返回类型**。

use std::path::{Path, PathBuf};

/// 一行是不是「`unsafe extern` 声明」或「返回类型里的 unsafe」。
fn is_exempt(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("unsafe extern") || t.contains("-> unsafe extern") || t.contains("-> unsafe fn")
}

fn collect_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    walk(&root.join("src"), &mut out);
    out.sort();
    out
}

/// 从 `lines[unsafe_idx]` 往上找 SAFETY 论据（跳过空行 / 属性 / 注释）。
fn has_safety_above(lines: &[&str], unsafe_idx: usize) -> bool {
    let mut j = unsafe_idx;
    while j > 0 {
        j -= 1;
        let t = lines[j].trim();
        if t.is_empty() || t.starts_with("#[") || t.starts_with("//") {
            if t.contains("SAFETY") {
                return true;
            }
            continue;
        }
        return false;
    }
    false
}

/// ① 主判据：每个真正的 `unsafe` 块都带 SAFETY 论据。
#[test]
fn d252_every_unsafe_block_has_safety_justification() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut missing: Vec<String> = Vec::new();
    let mut blocks = 0usize;

    for f in collect_files(root) {
        let src = std::fs::read_to_string(&f).expect("read source");
        let lines: Vec<&str> = src.lines().collect();
        // 跳过 #[cfg(test)] 之后的测试代码
        let start = lines
            .iter()
            .position(|l| l.trim() == "#[cfg(test)]")
            .unwrap_or(lines.len());
        for (i, line) in lines.iter().enumerate().take(start) {
            if !line.contains("unsafe") || is_exempt(line) {
                continue;
            }
            let t = line.trim();
            if t.starts_with("//") {
                continue;
            }
            blocks += 1;
            if !has_safety_above(&lines, i) {
                missing.push(format!(
                    "{}:{}  {t}",
                    f.strip_prefix(root).unwrap_or(&f).display(),
                    i + 1
                ));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "发现 {} 个 `unsafe` 块缺 SAFETY 论据（D252 补齐了 10 处，这条防止回退）：\n  - {}",
        missing.len(),
        missing.join("\n  - ")
    );
    assert!(
        blocks >= 8,
        "判据自身可能失效：只扫到 {blocks} 个 unsafe 块，扫描范围变了"
    );
}

/// ② 对照组：检查器必须**能抓到**缺论据的块，否则 ① 可能恒绿。
#[test]
fn d252_control_group_detector_finds_missing_safety() {
    let with_safety = vec!["// SAFETY: 因为 X", "let p = unsafe {"];
    assert!(
        has_safety_above(&with_safety, 1),
        "检查器抓不到紧邻上方的 SAFETY —— ① 可能恒绿"
    );

    // SAFETY 隔了几行（中间是普通代码）⇒ 不算
    let far = vec!["// SAFETY: 因为 X", "let a = 1;", "let p = unsafe {"];
    assert!(
        !has_safety_above(&far, 2),
        "检查器把远处的 SAFETY 当成了近邻 —— 规则写错了"
    );

    // 上方是普通注释、没有 SAFETY ⇒ 应判为缺
    let none = vec!["// 只是说明", "let p = unsafe {"];
    assert!(
        !has_safety_above(&none, 1),
        "检查器对无 SAFETY 的块误判为有"
    );

    // 豁免规则：`unsafe extern` 声明不算块
    assert!(is_exempt("            unsafe extern \"C\" {"));
    assert!(is_exempt(
        "        fn f(&self) -> unsafe extern \"C\" fn(*mut u8) -> u32 {"
    ));
    assert!(!is_exempt("        let p = unsafe {"));
}
