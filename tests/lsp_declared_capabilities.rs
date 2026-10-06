//! v0.104.6 D372 —— LSP **端到端 stdio**：`initialize` 声明的能力
//! **全部可调用**，未声明的方法正确报 MethodNotFound（否定轮，无产品变更）
//!
//! D371 钉了传输层与 JSON，本轮钉**请求分发**（`server.rs`，672 行、20 个方法）。
//! provider 层的各方法已有 16 个判据覆盖，但**「声明了什么」与
//! 「实现了什么」是否一致**此前无端到端判据。
//!
//! ## 为什么必须走真实 stdio
//!
//! `initialize` 的 `capabilities` 是**对外承诺**。若声明了某个能力
//! 却没有对应 handler，编辑器会正常调用并收到 `MethodNotFound` ——
//! 这是**能力协商层**的分叉，单元测试测不到。
//!
//! ## 入口是 `mora-lsp`，不是 `mora lsp`
//!
//! `src/main.rs:425-427` 的注释明写：banner 会污染 `mora run` 的 stdout，
//! 而**独立二进制 `mora-lsp` 的 stdout 直接以 `Content-Length` 开头**。
//! 首版探针用 `mora lsp` ⇒ 被当成文件名读（*系统找不到指定的文件*）。
//!
//! ## 判据的关键：**必须在字节层解帧**
//!
//! `Content-Length` 是**字节数**。首版探针先把 stdout `decode` 成 `str`
//! 再按 `n` 切片 ⇒ **多字节 UTF-8 让下标错位**，只收到 8/14 条响应，
//! 看起来像「formatting 之后服务器挂了」。
//! 用字节切片后 **15 帧全收齐**（含 `publishDiagnostics` 通知）。
//!
//! ⇒ 与 D371「`read_message` 用 `read_exact` 读 body」同源：
//! **LSP 的长度是字节数，不是字符数**。

use std::io::Write;
use std::process::{Command, Stdio};

/// 拼一条 `Content-Length` 帧（**字节数**）。
fn frame(body: &str) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

/// 驱动 LSP：发一批完整 JSON 行，返回**解帧后的响应体**。
///
/// ⚠ **必须在字节层解帧**（见文件头）—— `Content-Length` 是字节数。
fn run_lines(lines: &[&str]) -> Vec<String> {
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora-lsp.exe");
    let mut payload: Vec<u8> = Vec::new();
    for l in lines {
        payload.extend(frame(l));
    }
    let mut child = Command::new(exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("启动 mora-lsp");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(&payload)
        .expect("写请求");
    let out = child.wait_with_output().expect("等退出");
    let raw = out.stdout;

    // ⚠ **字节层**解帧（见文件头）
    let mut bodies = Vec::new();
    let mut i = 0usize;
    while let Some(rel) = raw[i.min(raw.len())..]
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
    {
        let he = i + rel;
        let head = String::from_utf8_lossy(&raw[i..he]).to_string();
        let n: usize = head
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or_else(|| panic!("帧头缺 Content-Length: {head:?}"));
        let start = he + 4;
        let end = (start + n).min(raw.len());
        bodies.push(String::from_utf8_lossy(&raw[start..end]).to_string());
        i = end;
    }
    bodies
}

const DOC: &str = "let Counter = 1\nlet f = fn(x) x + 1 end\nprint(Counter)\n";

fn probe_lines(extra: &[&str]) -> Vec<String> {
    let mut lines: Vec<String> = vec![
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}"#.into(),
        r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#.into(),
        format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"file:///d372doc.mora","languageId":"mora","version":1,"text":{}}}}}}}"#,
            serde_escape(DOC)
        ),
    ];
    lines.extend(extra.iter().map(|s| s.to_string()));
    lines.push(r#"{"jsonrpc":"2.0","id":9999,"method":"shutdown","params":{}}"#.into());
    lines.push(r#"{"jsonrpc":"2.0","method":"exit","params":{}}"#.into());

    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    run_lines(&refs)
}

fn serde_escape(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn find(bodies: &[String], id: u32) -> Option<String> {
    bodies.iter().find_map(|b| {
        if b.contains(&format!("\"id\":{id}")) {
            Some(b.clone())
        } else {
            None
        }
    })
}

/// **主断言**：`initialize` 声明的 **9 个能力**逐一调用，**都不得**是
/// `method not supported`。
#[test]
fn d372_every_declared_capability_is_callable() {
    let td = r#""textDocument":{"uri":"file:///d372doc.mora"}"#;
    let pos = r#""position":{"line":0,"character":5}"#;
    let reqs: Vec<String> = vec![
        format!(
            r#"{{"jsonrpc":"2.0","id":10,"method":"textDocument/hover","params":{{{td},{pos}}}}}"#
        ),
        format!(
            r#"{{"jsonrpc":"2.0","id":11,"method":"textDocument/completion","params":{{{td},"position":{{"line":0,"character":0}}}}}}"#
        ),
        format!(
            r#"{{"jsonrpc":"2.0","id":12,"method":"textDocument/definition","params":{{{td},{pos}}}}}"#
        ),
        format!(
            r#"{{"jsonrpc":"2.0","id":13,"method":"textDocument/references","params":{{{td},{pos},"context":{{"includeDeclaration":true}}}}}}"#
        ),
        format!(
            r#"{{"jsonrpc":"2.0","id":14,"method":"textDocument/documentSymbol","params":{{{td}}}}}"#
        ),
        format!(
            r#"{{"jsonrpc":"2.0","id":15,"method":"textDocument/formatting","params":{{{td},"options":{{"tabSize":2,"insertSpaces":true}}}}}}"#
        ),
        format!(
            r#"{{"jsonrpc":"2.0","id":16,"method":"textDocument/rangeFormatting","params":{{{td},"range":{{"start":{{"line":0,"character":0}},"end":{{"line":2,"character":0}}}},"options":{{"tabSize":2,"insertSpaces":true}}}}}}"#
        ),
        format!(
            r#"{{"jsonrpc":"2.0","id":17,"method":"textDocument/rename","params":{{{td},{pos},"newName":"Renamed"}}}}"#
        ),
        format!(
            r#"{{"jsonrpc":"2.0","id":18,"method":"textDocument/foldingRange","params":{{{td}}}}}"#
        ),
    ];
    let refs: Vec<&str> = reqs.iter().map(String::as_str).collect();
    let bodies = probe_lines(&refs);
    assert!(
        bodies.len() >= reqs.len(),
        "应至少收到 {} 条响应（含通知），实得 {}; bodies={:?}",
        reqs.len(),
        bodies.len(),
        bodies
            .iter()
            .map(|b| b.chars().take(50).collect::<String>())
            .collect::<Vec<_>>()
    );
    for (i, _) in reqs.iter().enumerate() {
        let id = 10 + i as u32;
        let b = find(&bodies, id).unwrap_or_else(|| panic!("没收到 id={id} 的响应"));
        assert!(
            !b.contains("method not supported"),
            "id={id} 是 `initialize` **已声明**的能力，不该报 not supported: {b}"
        );
    }
}

/// **反向对照**：**未声明**的方法必须报 `-32601/32603`，
/// 而不是静默无响应或 panic。
#[test]
fn d372_undeclared_methods_report_not_supported() {
    let bodies = probe_lines(&[
        r#"{"jsonrpc":"2.0","id":20,"method":"textDocument/codeAction","params":{}}"#,
        r#"{"jsonrpc":"2.0","id":21,"method":"no/such/method","params":{}}"#,
    ]);
    for id in [20u32, 21] {
        let b = find(&bodies, id).unwrap_or_else(|| panic!("没收到 id={id} 的响应"));
        assert!(
            b.contains("method not supported"),
            "未声明的方法必须明确报错; 实得: {b}"
        );
    }
}

/// **`exit` 必须让进程正常退出**（`shutdown` 之后的收尾）。
#[test]
fn d372_exit_terminates_the_process() {
    let bodies = probe_lines(&[]);
    assert!(
        find(&bodies, 9999).is_some(),
        "`shutdown` 必须有响应; 实得帧={:?}",
        bodies.len()
    );
    // `run_lines` 用 `wait_with_output` ⇒ 能返回就说明进程**没有挂死**。
}

/// **`publishDiagnostics` 通知**在 `didOpen` 后发出（它是通知，无 `id`）。
#[test]
fn d372_did_open_publishes_diagnostics() {
    let bodies = probe_lines(&[]);
    assert!(
        bodies
            .iter()
            .map(String::as_str)
            .any(|b| b.contains("publishDiagnostics")),
        "`didOpen` 后应推送诊断（可能有也可能没有内容，但通知本身应发出）; 实得帧={:?}",
        bodies.len()
    );
}
