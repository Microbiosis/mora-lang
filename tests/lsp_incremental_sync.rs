//! v0.104.6 D194：LSP 声明 `textDocumentSync.change: 1`（**Incremental**），
//! 却把每条 `contentChanges` 当**全量替换** —— 敲一个键就丢掉整个文件（已修）。
//!
//! ## 缺陷
//!
//! `lsp/server.rs::parse_change_params` 原文：
//!
//! ```rust
//! // Full sync: 只取最后一条（按 LSP 规范 full sync 只发一条）
//! let last = changes.last()?;
//! let text = last.get("text")?.as_str()?.to_string();
//! Some((uri, version, text))
//! ```
//!
//! 但 capabilities 里声明的是 **`"change": 1`（Incremental）** —— 客户端
//! 会按规范发**带 `range`** 的增量变更。此时**每一条 change 的 `text`
//! 都成了整份文档**。
//!
//! 真实 `mora-lsp.exe` 走 stdio 实测：
//!
//! ```text
//! didOpen   "let a = 1\nprint(a)\n"
//! didChange range=(0,8)-(0,9) text="2"      ← 只把 '1' 改成 '2'
//!   → 文档实际变成 "2"
//!   → documentSymbol 返回 []      （编辑前返回 [a]）
//!   → hover 在 (0,4) 报 "variable 2"
//!   → formatting 返回 newText "2 \n"（把整份文档揭示了出来）
//! ```
//!
//! **全量变更（不带 `range`）一切正常** —— 所以它只在真实编辑里出现，
//! 任何只测「打开文档 → 请求一次」的测试都测不出来。
//!
//! 影响：任何按声明使用增量同步的编辑器，**敲第一个键就丢掉整个文件**，
//! 之后 hover / 符号 / 诊断 / 补全 / 格式化全部空转。
//!
//! ## 修法
//!
//! `apply_content_changes(base, changes)` 按规范逐条应用：
//! 带 `range` 的**按范围拼接**（复用 `position_to_offset`），不带 `range`
//! 的才是全量替换。调用方需先取当前文档文本作为 `base`。

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn frame(json: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{}", json.len(), json)
}

fn temp_dir() -> PathBuf {
    let d = std::env::temp_dir().join("mora_d194_lsp");
    std::fs::create_dir_all(&d).expect("建目录");
    d
}

/// 开文档 → 应用若干 didChange → 请求 `documentSymbol` 与 hover。
fn session(tag: &str, initial: &str, changes: &str, hover: (i64, i64)) -> String {
    let dir = temp_dir();
    let input = dir.join(format!("{tag}.bin"));
    let uri = format!("file:///tmp/{tag}.mora");
    let doc = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","languageId":"mora","version":1,"text":{}}}}}}}"#,
        json_string(initial)
    );
    let reqs = format!(
        "{}{}",
        frame(&format!(
            r#"{{"jsonrpc":"2.0","id":60,"method":"textDocument/documentSymbol","params":{{"textDocument":{{"uri":"{uri}"}}}}}}"#
        )),
        frame(&format!(
            r#"{{"jsonrpc":"2.0","id":61,"method":"textDocument/hover","params":{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":{},"character":{}}}}}}}"#,
            hover.0, hover.1
        ))
    );
    std::fs::write(
        &input,
        format!(
            "{}{}{}{}{}",
            frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}"#),
            frame(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#),
            frame(&doc),
            changes,
            reqs
        ),
    )
    .expect("写输入");

    let out = Command::new(env!("CARGO_BIN_EXE_mora-lsp"))
        .stdin(Stdio::from(std::fs::File::open(&input).expect("打开输入")))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .expect("跑 mora-lsp");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn part(raw: &str, id: u32) -> &str {
    raw.split("Content-Length")
        .find(|c| c.contains(&format!("\"id\":{id},")))
        .unwrap_or_else(|| panic!("没拿到 id={id} 的应答:\n{raw}"))
}

/// **主判据（有牙齿）**：增量编辑后，**文档其余部分必须还在**。
///
/// 修前：documentSymbol 返回 `[]`（文档已被替换成那一个字符）。
#[test]
fn d194_incremental_edit_preserves_the_rest_of_the_document() {
    let uri = "file:///tmp/inc.mora";
    // 把 `let a = 1` 里的 '1' 改成 '2'
    let chg = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didChange","params":{{"textDocument":{{"uri":"{uri}","version":2}},"contentChanges":[{{"range":{{"start":{{"line":0,"character":8}},"end":{{"line":0,"character":9}}}},"text":"2"}}]}}}}"#
    );
    let raw = session("inc", "let a = 1\nprint(a)\n", &frame(&chg), (0, 4));

    let sym = part(&raw, 60);
    assert!(
        sym.contains("\"name\":\"a\""),
        "增量编辑后 documentSymbol 应仍能看到 `a` —— 修前返回 []（整份文档被 \
         替换成了被编辑的那一个字符）:\n{}",
        sym
    );
    let hov = part(&raw, 61);
    assert!(
        hov.contains("let a"),
        "hover 应仍指向 `a`，而不是被编辑的数字:\n{}",
        hov
    );
}

/// 跨行增量编辑：改第 1 行的内容，第 0 行必须**毫发无损**。
///
/// 覆盖与上一条不同的偏移换算路径（多行文档）。
#[test]
fn d194_incremental_edit_across_lines_keeps_earlier_lines() {
    let uri = "file:///tmp/inc2.mora";
    let chg = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didChange","params":{{"textDocument":{{"uri":"{uri}","version":2}},"contentChanges":[{{"range":{{"start":{{"line":1,"character":6}},"end":{{"line":1,"character":7}}}},"text":"b"}}]}}}}"#
    );
    let raw = session("inc2", "let a = 1\nprint(a)\n", &frame(&chg), (0, 4));
    let sym = part(&raw, 60);
    assert!(
        sym.contains("\"name\":\"a\""),
        "改第 1 行不应毁掉第 0 行的 `a`:\n{}",
        sym
    );
}

/// **对照组**：不带 `range` 的**全量**变更必须照常工作（修前靠它碰巧正确）。
#[test]
fn d194_full_text_change_still_works() {
    let uri = "file:///tmp/full.mora";
    let chg = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didChange","params":{{"textDocument":{{"uri":"{uri}","version":2}},"contentChanges":[{{"text":"let z = 9\nprint(z)\n"}}]}}}}"#
    );
    let raw = session("full", "let a = 1\nprint(a)\n", &frame(&chg), (0, 4));
    let sym = part(&raw, 60);
    assert!(
        sym.contains("\"name\":\"z\""),
        "全量变更应把文档整个换掉（得到 `z`）:\n{}",
        sym
    );
}

/// 纯插入（`start == end`）也必须正确 —— 打字时最常见的形态。
#[test]
fn d194_pure_insertion_incremental_edit_works() {
    let uri = "file:///tmp/ins.mora";
    // 在 `let a = 1` 的 '=' 前插入 "2" → `let a = 21`
    let chg = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didChange","params":{{"textDocument":{{"uri":"{uri}","version":2}},"contentChanges":[{{"range":{{"start":{{"line":0,"character":8}},"end":{{"line":0,"character":8}}}},"text":"2"}}]}}}}"#
    );
    let raw = session("ins", "let a = 1\nprint(a)\n", &frame(&chg), (0, 4));
    let sym = part(&raw, 60);
    assert!(
        sym.contains("\"name\":\"a\""),
        "纯插入后文档仍应完好:\n{}",
        sym
    );
    let hov = part(&raw, 61);
    assert!(hov.contains("let a"), "hover 应仍指向 `a`:\n{}", hov);
}
