//! v0.104.6 D248 —— LSP 入站 `position` 的收口**必须覆盖全部 provider**。
//!
//! ## 缺陷：D245 修漏了 2/6 处，且漏的是最危险的两处
//!
//! D245 在 `parsed_doc_v3::pos_of` 建了唯一收口，并修了 `hover` /
//! `definition` / `formatting` / `server` 四处。但 `rename.rs` 与
//! `references.rs` **各写了一遍**同样的转换、**没有**跟着改：
//!
//! ```rust
//! let line = pos.get("line").and_then(|n| n.as_i64()).unwrap_or(0) as usize;
//! ```
//!
//! 真实探针（文档 `let alpha: Int = 1\nlet beta: Int = alpha\nbeta`）：
//!
//! | position | rename 实际重命名了什么 |
//! |---|---|
//! | `(1, 17)`（在 `alpha` 上） | `alpha` ✓ 正确 |
//! | **`(-1, 0)`** | **`beta` 两处** —— 客户端只需发一个负 position |
//!
//! 客户端照这份 WorkspaceEdit `apply` ⇒ **改坏用户文件**。这比 D245 严重
//! 一个量级：hover 只是**显示**错，rename 是**写入**。
//!
//! ## 这条判据为什么是「普查型」的
//!
//! D245 的行为判据只覆盖 `hover` 与 `definition` 两处，于是「另外四个里
//! 还有没有漏」完全无人过问 —— 事实是**还漏了两个**。
//!
//! ⇒ 本条直接扫源码：**任何**自己解析 `get("line")` / `get("character")`
//! 的地方都必须走 `pos_of`。它防的不是某个具体缺陷，而是**「漏改一处」这个
//! 失误模式本身**。
//!
//! 它是源码判据（D150 已论证 LSP 侧无法穷举运行时路径），但断言是
//! **全称量化**的 —— 「没有例外」，而不是「某处长什么样」，因此不会像
//! D150 那条一样随正确重构而失效。

use mora::lsp::json::{self, Value as J};
use mora::lsp::providers::{references_v3, rename_v3};
use mora::lsp::server::DocumentState;
use std::collections::HashMap;

const URI: &str = "file:///t.mora";
/// 文档以标识符 `beta` 结尾 —— 负 position 回绕到 `text.len()` 时**会命中它**。
const DOC: &str = "let alpha: Int = 1\nlet beta: Int = alpha\nbeta";

/// ① **普查型主判据**：没有任何模块可以自己解析 position。
#[test]
fn d248_no_module_parses_position_by_hand() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(root.join("src/lsp/providers"))
        .expect("读 providers 目录")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .collect();
    files.push(root.join("src/lsp/server.rs"));

    let mut violations: Vec<String> = Vec::new();
    let mut scanned = 0usize;

    for f in &files {
        let src = std::fs::read_to_string(f).expect("读源文件");
        for (i, line) in src.lines().enumerate() {
            // 只看真正在解析坐标的行；注释与文档行跳过。
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with("//!")
            {
                continue;
            }
            let parses_position =
                line.contains("get(\"line\")") || line.contains("get(\"character\")");
            if parses_position {
                scanned += 1;
                if !line.contains("pos_of") {
                    violations.push(format!(
                        "{}:{}: 自己解析了 position 却没走 `pos_of`\n    {}",
                        f.file_name().unwrap().to_string_lossy(),
                        i + 1,
                        trimmed
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "发现 {} 处绕过 `pos_of` 的手写 position 解析（D248 漏改的正是这种）：\n  - {}",
        violations.len(),
        violations.join("\n  - ")
    );
    assert!(
        scanned >= 5,
        "判据自身的扫描范围可能已失效：只扫到 {scanned} 个 position 解析点\
         （当前已知 6 处：hover / definition / rename / references / formatting / server）。\
         若确实有意合并了实现，请同步更新本判据的期望值。"
    );
}

fn docs() -> HashMap<String, DocumentState> {
    let mut m = HashMap::new();
    m.insert(
        URI.to_string(),
        DocumentState {
            uri: URI.to_string(),
            text: DOC.to_string(),
            version: 1,
            diagnostics: vec![],
        },
    );
    m
}

fn rename_params(line: i64, ch: i64) -> J {
    json::parse(&format!(
        r#"{{"textDocument":{{"uri":"{URI}"}},"position":{{"line":{line},"character":{ch}}},"newName":"HACKED"}}"#
    ))
    .expect("parse")
}

fn refs_params(line: i64, ch: i64) -> J {
    json::parse(&format!(
        r#"{{"textDocument":{{"uri":"{URI}"}},"position":{{"line":{line},"character":{ch}}}}}"#
    ))
    .expect("parse")
}

/// rename 会不会把 edits 里涉及的**行**都列出来（用来断言「没改 beta」）。
fn edited_lines(v: &J) -> Vec<i64> {
    let mut out = Vec::new();
    let Some(J::Object(changes)) = v.get("changes") else {
        return out;
    };
    for edits in changes.values() {
        if let J::Array(items) = edits {
            for it in items {
                if let Some(l) = it
                    .get("range")
                    .and_then(|r| r.get("start"))
                    .and_then(|s| s.get("line"))
                    .and_then(|n| n.as_i64())
                {
                    out.push(l);
                }
            }
        }
    }
    out.sort_unstable();
    out
}

/// ② **行为主判据**：负 position 不得产生**写操作**。
///
/// 退化到 `(0,0)` 之后该位置落在 `let` 关键字上，不是标识符 ⇒ 无编辑。
/// 修前会返回 `beta` 的两处编辑（行 1 与行 2）。
#[test]
fn d248_negative_position_produces_no_rename_edits() {
    let d = docs();
    // 确认对照组确实「有编辑」，否则本条会因恒真而空转。
    let baseline = rename_v3(&d, &rename_params(1, 17));
    assert!(
        !edited_lines(&baseline).is_empty(),
        "判据空转：在 alpha 上的 rename 竟没有编辑 —— 对照组失效"
    );

    for (label, line, ch) in [
        ("line=-1", -1i64, 0i64),
        ("char=-1", 0i64, -1i64),
        ("both", -1i64, -1i64),
    ] {
        let edits = edited_lines(&rename_v3(&d, &rename_params(line, ch)));
        assert!(
            edits.is_empty(),
            "负 position（{label}）竟产生了写入编辑，涉及行 {edits:?} —— \
             客户端 apply 之后就是**改坏用户文件**"
        );
    }
}

/// ③ references 侧同理：负 position 不得返回位置。
#[test]
fn d248_negative_position_produces_no_references() {
    let d = docs();
    let baseline = references_v3(&d, &refs_params(1, 17));
    assert!(
        matches!(&baseline, J::Array(a) if !a.is_empty()),
        "判据空转：在 alpha 上的 references 竟为空"
    );

    for (label, line, ch) in [
        ("line=-1", -1i64, 0i64),
        ("char=-1", 0i64, -1i64),
        ("both", -1i64, -1i64),
    ] {
        let r = references_v3(&d, &refs_params(line, ch));
        assert!(
            matches!(&r, J::Array(a) if a.is_empty()),
            "负 position（{label}）竟返回了引用位置：{r:?}"
        );
    }
}

/// ④ 对照组：钉住「负 i64 `as usize` 会回绕」这条语言事实（不钉 `pos_of`）。
#[test]
fn d248_control_group_negative_as_usize_wraps_to_max() {
    assert_eq!((-1i64) as usize, usize::MAX);
    assert_eq!(0i64 as usize, 0);
    assert_eq!(37i64 as usize, 37);
}
