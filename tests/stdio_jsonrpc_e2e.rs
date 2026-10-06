//! v0.104.6 D100：stdio JSON-RPC 服务器（`McpServer.serve()`）的 **stdout 被启动横幅污染**。
//!
//! ## 缺陷
//!
//! `mora run` 可以在程序里启动 MCP 服务器（`McpServer::new()` + `.tool()` +
//! `.serve()`），协议是 **JSON-RPC 2.0 over stdin/stdout** + LSP 式
//! `Content-Length` 分帧 —— 即 **stdout 就是协议通道**。
//! 而 `main.rs::print_banner()` 在分派**之前**用 `println!`（stdout）打出
//! 9 行横幅，于是客户端先收到 9 行非帧文本，才收到协议帧。
//!
//! ```text
//! Mora v0.104.5          ← 不是协议帧
//!   AI: mock mode …      ← 含冒号，会被当 header 名 "AI"
//!   …共 9 行…
//!
//! Content-Length: 162
//!
//! {"id":1,"jsonrpc":"2.0",…}
//! ```
//!
//! 宽容的解析器可能跳过这些行恢复工作，严格的会直接失败。
//! `mora-lsp` 无此问题（它自己的 stdout 直接以 `Content-Length` 开头）。
//!
//! 对普通 `mora run` 而言，横幅同样是**元数据而非程序输出**，
//! 不该出现在 `mora run x.mora > out.txt` 的结果里。
//!
//! ## 本文件的另一半：证明**帧解析器本身是好的**
//!
//! 调查过程中一度怀疑 `lsp::transport::read_message` 解析有误。用内存里的
//! `Cursor` 喂标准分帧后全部通过 —— 于是「解析器 bug」被排除，
//! 缺陷只在 stdout 的污染与（早期 PowerShell 探针的假象）。
//! 下面这些用例保留下来，防止将来有人再怀疑解析器。

use mora::lsp::transport::read_message;
use std::io::Cursor;

fn frame(len: usize, body: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{}", len, body)
}

const BODY: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#;

/// 标准分帧（CRLF + 空行）必须正确解析。
#[test]
fn d100_standard_lsp_framing_parses() {
    let raw = frame(BODY.len(), BODY);
    let mut r = Cursor::new(raw.into_bytes());
    match read_message(&mut r) {
        Ok(Some(s)) => assert_eq!(s, BODY, "应还原出原始 body"),
        Ok(None) => panic!("解析器把一个完整分帧当成了 EOF"),
        Err(e) => panic!("解析器拒绝了标准分帧: {e}"),
    }
}

/// LF-only 分帧（部分客户端实现）也必须能解析。
#[test]
fn d100_lf_only_framing_parses() {
    let raw = format!("Content-Length: {}\n\n{}", BODY.len(), BODY);
    let mut r = Cursor::new(raw.into_bytes());
    assert_eq!(
        read_message(&mut r).ok().flatten().as_deref(),
        Some(BODY),
        "LF-only 分帧应能解析"
    );
}

/// 连续两条消息应被逐条读出。
#[test]
fn d100_two_messages_in_sequence() {
    let raw = format!("{}{}", frame(BODY.len(), BODY), frame(5, "\"end\""));
    let mut r = Cursor::new(raw.into_bytes());
    assert_eq!(read_message(&mut r).ok().flatten().as_deref(), Some(BODY));
    assert_eq!(
        read_message(&mut r).ok().flatten().as_deref(),
        Some("\"end\"")
    );
}

/// 真正的 EOF 才返回 None。
#[test]
fn d100_eof_returns_none() {
    let mut r = Cursor::new(Vec::new());
    assert_eq!(read_message(&mut r).ok().flatten(), None, "空输入应是 EOF");
}

/// **D100 主断言**：启动横幅不得出现在 stdout。
///
/// 用真实子进程 + **文件重定向**喂 stdin（PowerShell 的
/// `Process.StandardInput` 写入在本轮调查中制造过假象，见文件头注释）。
#[test]
fn d100_banner_does_not_pollute_stdout() {
    let dir = std::env::temp_dir().join("mora_d100_banner");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let prog = dir.join("p.mora");
    std::fs::write(&prog, "print(\"program-output\")\n").expect("write prog");
    let bin = env!("CARGO_BIN_EXE_mora");

    let out = std::process::Command::new(bin)
        .arg("run")
        .arg(&prog)
        .output()
        .expect("run mora");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        stdout.trim(),
        "program-output",
        "stdout 应只含程序输出。实际 stdout:\n{stdout}\n（stderr:\n{stderr}）"
    );
    assert!(
        stderr.contains("Mora v"),
        "横幅应出现在 stderr。实际 stderr:\n{stderr}"
    );
    let _ = std::fs::remove_file(&prog);
}
