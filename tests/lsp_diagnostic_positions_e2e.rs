//! v0.104.6 D157：把 D156 的诊断改进**验到用户看得见的那一层**（LSP）。
//!
//! ## 为什么需要这一条
//!
//! D156 修的是 `TypeError.line/column` 从 0 变成真实位置。但 `TypeError`
//! 有**两个**消费者：CLI `--check` 与 LSP。D156 的测试只钉了前者。
//! 若 LSP 那条路把 line 用错（D132–D135 恰好在这一层修过 LSP 位置），
//! D156 对编辑器用户就等于没修。
//!
//! ## 实测（真实 `mora-lsp.exe` 进程 + stdio JSON-RPC）
//!
//! 文档（错误在**第 2 行**）：
//!
//! ```mora
//! let xs = [1, 2, 3]
//! let a = xs.take("one")
//! let ok = 1
//! ```
//!
//! | | 条数 | LSP `range`（0-based） | 指向 | `expected` |
//! |---|---|---|---|---|
//! | D156 前 | **2**（重复） | `line 0, character 0` | 源码**第 1 行第 1 列** ❌ | `int`（欠报） |
//! | D156 后 | **1** | `line 1, character 16` | 源码**第 2 行第 17 列** ✅ | `int \| float` |
//!
//! 即修复前 VS Code 会在**错误所在行的下一行**画两条一模一样的波浪线。
//!
//! ## 本文件怎么测
//!
//! 走**真实二进制 + 真实 stdio 协议**（`mora-lsp.exe` + `Content-Length` 分帧），
//! 而不是调内部函数 —— `check_diagnostics` 是私有的，且只有真起进程才能
//! 验证「stdout 就是协议通道」这一前提（与 D100 的教训同源）。

use std::io::Write;
use std::process::{Command, Stdio};

/// 组一个 `Content-Length` 帧（字节数按 UTF-8 计）。
fn frame(body: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{}", body.len(), body)
}

/// 逃出 JSON 字符串用的最小转义（本文档只含 `"` 与换行）。
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

/// 起一次真实的 `mora-lsp` 会话，返回原始 stdout。
fn lsp_session(doc: &str) -> String {
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#;
    let open = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"file:///doc.mora","languageId":"mora","version":1,"text":"{}"}}}}}}"#,
        json_escape(doc)
    );
    let payload =
        frame(init) + &frame(&open) + &frame(r#"{"jsonrpc":"2.0","method":"exit","params":{}}"#);

    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora-lsp.exe");
    let mut child = Command::new(exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("起 mora-lsp.exe（先 cargo build）");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("写入 LSP 帧");
    let out = child.wait_with_output().expect("等 mora-lsp 退出");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// 从 publishDiagnostics 帧里抽出每条诊断的 `(line, character)` 与 message。
///
/// 序列化后的键按字母序：`{"message":…,"range":{"end":{…},"start":{…}},…}`，
/// 故每条诊断以 `{"message":"` 开头，以下一个 `{"message":"`（或 `],"uri"`）为止。
fn diagnostics(stdout: &str) -> Vec<(usize, usize, String)> {
    let frame = match stdout.find("publishDiagnostics") {
        Some(i) => &stdout[i..],
        None => return Vec::new(),
    };
    let mut res = Vec::new();
    let mut rest = frame;
    while let Some(i) = rest.find(r#"{"message":"#) {
        rest = &rest[i..];
        let body_end = rest[1..]
            .find(r#"{"message":"#)
            .map(|p| p + 1)
            .unwrap_or(rest.len());
        let body = &rest[..body_end];

        // range.start = {"character":N,"line":M}
        let range = match body.find(r#""start":{"#) {
            Some(p) => &body[p..],
            None => {
                rest = &rest[body_end..];
                continue;
            }
        };
        let num = |key: &str| -> usize {
            range
                .find(key)
                .and_then(|p| {
                    range[p + key.len()..]
                        .split(|c: char| !c.is_ascii_digit())
                        .next()
                        .and_then(|d| d.parse::<usize>().ok())
                })
                .unwrap_or(usize::MAX)
        };
        let character = num(r#""character":"#);
        let line = num(r#""line":"#);
        let message = body
            .find(r#""message":""#)
            .map(|p| body[p + 11..].split('"').next().unwrap_or("").to_string())
            .unwrap_or_default();
        res.push((line, character, message));
        rest = &rest[body_end..];
    }
    res
}

const DOC: &str = "let xs = [1, 2, 3]\nlet a = xs.take(\"one\")\nlet ok = 1\n";

/// D157 主判据：诊断必须落在**真正出错的那一行**（0-based line 1 = 源码第 2 行）。
#[test]
fn d157_lsp_diagnostic_points_at_the_offending_argument() {
    let out = lsp_session(DOC);
    let ds = diagnostics(&out);
    assert_eq!(
        ds.len(),
        1,
        "一个错误应只推一条诊断（D156 前是 2 条重复）; 实得: {ds:?}"
    );
    let (line, ch, msg) = &ds[0];
    assert_eq!(
        (*line, *ch),
        (1, 16),
        "应指向源码第 2 行第 17 列那个 `\"one\"` 字面量（LSP 是 0-based）; 实得: {ds:?}"
    );
    assert!(
        msg.contains("int | float") && msg.contains("string"),
        "消息应如实写出 Union 与实参类型; 实得: {msg}"
    );
}

/// D157 反向对照：无错的文档必须推**零**条诊断。
///
/// ⚠ 额外断言「会话确实跑起来了」：否则解析器一旦失配（本文件第一版就栽在
/// `"start":"` vs `"start":{`），两条判据会**同时空过** —— 全绿但什么都没验。
#[test]
fn d157_lsp_clean_document_has_no_diagnostics() {
    let out = lsp_session("let xs = [1, 2, 3]\nlet a = xs.take(2)\nlet ok = 1\n");
    assert!(
        out.contains("\"serverInfo\""),
        "会话必须真的握手成功（否则下面的「零诊断」是空过，不是真通过）; 实得: {out}"
    );
    let ds = diagnostics(&out);
    assert!(ds.is_empty(), "合法文档不得推诊断; 实得: {ds:?}");
}

/// D157 对照组：语法错误仍走 parse_error 诊断路径（D101 修的），不得被本轮改动影响。
#[test]
fn d157_lsp_syntax_error_still_reported() {
    let out = lsp_session("let = = =\n");
    assert!(
        out.contains("\"serverInfo\""),
        "会话必须真的握手成功; 实得: {out}"
    );
    let ds = diagnostics(&out);
    assert!(
        !ds.is_empty(),
        "语法错误必须被推成诊断（D101：`Err(_) => return Vec::new()` 曾让它静默）; 实得: {ds:?}"
    );
}
