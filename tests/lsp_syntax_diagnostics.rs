//! v0.104.6 D101：LSP 对**语法错误**推送**零诊断** —— 等于告诉用户「代码没问题」。
//!
//! ## 实测（修前，真实 `mora-lsp` 子进程 + 文件重定向喂 stdin）
//!
//! | 源码 | `mora run` | LSP `publishDiagnostics` |
//! |---|---|---|
//! | `let = = =` | `Parse error: Expected variable name after 'let' at line 1`，exit 2 | **`diagnostics: []`** ❌ |
//! | `print(undefined_thing)` | `Unbound variable …`，exit 2 | 1 条 ✓ |
//! | `let n: Int = "hello"` | 2 条 type error | 2 条 ✓ |
//! | 正常源码 | 正常退出 | 0 条 ✓ |
//!
//! 根因：`server.rs::check_diagnostics` 里 `Err(_) => return Vec::new()` ——
//! parser 失败即返回空表。语法错误是语言服务器**最基本**的能力，
//! 这一吞让整条诊断链在此处失效。
//!
//! 修法：把 parser 的错误消息转成一条 `severity=1` / `source="mora-parser"`
//! 的诊断，行号从消息里的 `at line N` 提取。

use std::path::PathBuf;
use std::process::Command;

fn temp_dir() -> PathBuf {
    let d = std::env::temp_dir().join("mora_d101_lsp");
    std::fs::create_dir_all(&d).expect("create temp dir");
    d
}

fn frame(json: &str) -> String {
    let n = json.len();
    format!("Content-Length: {n}\r\n\r\n{json}")
}

/// 对单个文档跑一次完整的 LSP 会话，返回 `publishDiagnostics` 的诊断条数
/// 与首条 message 前缀。
fn open_document(tag: &str, text: &str) -> (usize, String, String) {
    let dir = temp_dir();
    let input = dir.join(format!("{tag}.bin"));
    let uri = format!("file:///tmp/{tag}.mora");

    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}"#;
    let doc = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","languageId":"mora","version":1,"text":{}}}}}}}"#,
        json_string(text)
    );
    let shutdown = r#"{"jsonrpc":"2.0","id":2,"method":"shutdown","params":null}"#;
    std::fs::write(
        &input,
        format!(
            "{}{}{}{}",
            frame(init),
            frame(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#),
            frame(&doc),
            frame(shutdown)
        ),
    )
    .expect("write input");

    let out = Command::new(env!("CARGO_BIN_EXE_mora-lsp"))
        .stdin(std::process::Stdio::from(
            std::fs::File::open(&input).expect("open input"),
        ))
        .output()
        .expect("run mora-lsp");
    let _ = std::fs::remove_file(&input);

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    // 取 publishDiagnostics 那一帧
    for f in split_frames(&stdout) {
        if let Some(rest) =
            f.strip_prefix(r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics""#)
        {
            let n = rest.matches(r#""severity""#).count();
            let src = rest
                .find(r#""source":""#)
                .and_then(|i| {
                    rest[i + 10..]
                        .find('"')
                        .map(|j| rest[i + 10..i + 10 + j].to_string())
                })
                .unwrap_or_default();
            let msg = rest
                .find(r#""message":""#)
                .and_then(|i| {
                    rest[i + 11..]
                        .find('"')
                        .map(|j| rest[i + 11..i + 11 + j].to_string())
                })
                .unwrap_or_default();
            return (n, src, msg);
        }
    }
    panic!("没有收到 publishDiagnostics 通知。stdout:\n{stdout}");
}

fn split_frames(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(idx) = rest.find("Content-Length: ") {
        let after = &rest[idx + 16..];
        let len: usize = match after.split_whitespace().next().and_then(|n| n.parse().ok()) {
            Some(n) => n,
            None => break,
        };
        let hdr_end = match rest.find("\r\n\r\n") {
            Some(i) => i,
            None => break,
        };
        let body = &rest[hdr_end + 4..];
        if body.len() < len {
            break;
        }
        out.push(body[..len].to_string());
        rest = &body[len..];
    }
    out
}

fn json_string(s: &str) -> String {
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

/// **D101 主断言**：语法错误必须产生诊断，且来源标为 parser。
#[test]
fn d101_syntax_error_produces_a_diagnostic() {
    let (n, src, msg) = open_document("parse_err", "let = = =\n");
    assert_eq!(
        n, 1,
        "语法错误必须产生 1 条诊断（修前为 0，等于告诉用户「没问题」）。得到 {n} 条，source={src}，msg={msg}"
    );
    assert_eq!(src, "mora-parser", "语法错误应标记来源为 parser");
}

/// 对照组：未定义变量仍走 typeck 通道，不受影响。
#[test]
fn d101_unbound_variable_still_comes_from_typeck() {
    let (n, src, _msg) = open_document("undef", "print(undefined_thing)\n");
    assert_eq!(n, 1, "未定义变量应有 1 条诊断");
    assert_eq!(src, "mora-typeck", "来源仍应是 typeck");
}

/// 对照组：类型错误数量 —— v0.104.6 D128 由 **2 条** 改为 **1 条**。
///
/// 此前断言「仍是 2 条（双向检查器报两条）」，把那两条当成了基线。
/// D128 查明：它们讲的是**同一件事**（同一个 `let` 标注冲突），只是位置不同：
///
/// ```text
/// Type error at line 1:14: type mismatch: expected `Int`, got `String`   ← bidirectional，指向值
/// Type error at line 1:1:  Type mismatch: expected Int, got String         ← HM，指向 let
/// ```
///
/// 原因是去重只按 (line, column) 过滤，而两层位置不同（值 vs 整条语句）。
/// D128 补了 (line, expected, actual) 三元组去重后只保留 bidirectional 那条 ——
/// 位置更精确、措辞更清楚。
///
/// 本条的**意图**（D101 的对照组职责）不变：确保 D101 加的 parser 诊断
/// 没有牵连 typeck 通道。这里同时把「1 条」钉成新基线。
#[test]
fn d101_type_errors_unchanged() {
    let (n, src, msg) = open_document("type_err", "let n: Int = \"hello\"\n");
    assert_eq!(
        n, 1,
        "类型错误应为 1 条（D128 去重后）; 实际 {n} 条, msg={msg}"
    );
    assert_eq!(src, "mora-typeck");
    assert!(
        msg.contains("Int") && msg.contains("String"),
        "诊断应仍包含两种类型; 实际: {msg}"
    );
}

/// 防「一刀切」：正常源码**不得**产生假诊断。
#[test]
fn d101_valid_source_produces_no_diagnostics() {
    let (n, _src, _msg) = open_document("good", "let n = 1\nprint(n)\n");
    assert_eq!(n, 0, "正常源码不得被报出诊断");
}
