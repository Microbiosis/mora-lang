//! v0.104.6 D102 / D103：LSP `formatting` **产出不可解析的代码**；
//! `references` **静默忽略 `includeDeclaration`**。
//!
//! ## D102：格式化器把运算符换成枚举变体名
//!
//! `formatting.rs::token_text` 的兜底分支是 `format!("{:?}", tt).tolowercase()` ——
//! 那输出**枚举变体名**而非源码拼写：
//!
//! | 输入 | 修前格式化产物 |
//! |---|---|
//! | `let  x=1` | `let x assign 1` |
//! | `print( x +y )` | `print lparenx plus y rparen` |
//!
//! 产物**根本解析不了**（实测 `Parse error: Expected '=' in let binding at line 1`，
//! 而原文能跑出 `3.0`）。编辑器一旦接受这份 `newText` 并存盘，用户的代码就被毁了。
//!
//! 修法：穷举每个 `TokenType` 变体的源码拼写。
//!
//! ## D103：`references` 从不读 `context`
//!
//! 该 provider 此前**既不含 `context` 也不含 `includeDeclaration`** —— 标志被
//! 静默忽略；且 `collect_references_v3` 只从**表达式**收集，`let x = 1` 的
//! **声明处**（绑定）永远找不到。实测 `includeDeclaration: true` 只返回使用处，
//! 而同场景的 `rename` 走 `collect_definitions_v3` + `collect_references_v3`
//! **能找到两处** —— 两个 provider 行为不一致。
//!
//! 修法：按 LSP 规范支持 `includeDeclaration`（默认 false）与 `onlyDeclaration`。

use std::path::PathBuf;
use std::process::Command;

fn temp_dir() -> PathBuf {
    let d = std::env::temp_dir().join("mora_d102_fmt");
    std::fs::create_dir_all(&d).expect("create temp dir");
    d
}

fn frame(json: &str) -> String {
    let n = json.len();
    format!("Content-Length: {n}\r\n\r\n{json}")
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

/// 打开一个文档、发若干请求，返回 `(id, 结果 JSON)` 列表。
fn session(tag: &str, text: &str, requests: &[(&str, &str, &str)]) -> Vec<(String, String)> {
    let dir = temp_dir();
    let input = dir.join(format!("{tag}.bin"));
    let uri = format!("file:///tmp/{tag}.mora");

    let mut msgs = vec![
        frame(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}"#,
        ),
        frame(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#),
        frame(&format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","languageId":"mora","version":1,"text":{}}}}}}}"#,
            json_string(text)
        )),
    ];
    for (id, method, body) in requests {
        msgs.push(frame(&format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{body}}}"#
        )));
    }
    msgs.push(frame(
        r#"{"jsonrpc":"2.0","id":99,"method":"shutdown","params":null}"#,
    ));
    std::fs::write(&input, msgs.concat()).expect("write input");

    let out = Command::new(env!("CARGO_BIN_EXE_mora-lsp"))
        .stdin(std::process::Stdio::from(
            std::fs::File::open(&input).expect("open input"),
        ))
        .output()
        .expect("run mora-lsp");
    let _ = std::fs::remove_file(&input);

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    split_frames(&stdout)
        .into_iter()
        .filter_map(|f| {
            let id = f.find("\"id\":")? + 5;
            let rest = &f[id..];
            let end = rest.find(|c: char| !c.is_ascii_digit())?;
            let id = rest[..end].to_string();
            let r = f.find("\"result\":")? + 9;
            let result = f[r..].trim_end_matches('}').to_string();
            Some((id, result))
        })
        .collect()
}

const SRC: &str = "let  x=1\nlet y   =   2\nprint( x +y )\n";

/// D102 主断言：格式化结果必须是**可解析的**。
///
/// 判据不是「长得对」，而是把产物丢给 `mora run` —— 解析失败即缺陷。
#[test]
fn d102_formatting_output_still_parses_and_runs() {
    let res = session(
        "fmt",
        SRC,
        &[(
            "2",
            "textDocument/formatting",
            r#"{"textDocument":{"uri":"file:///tmp/fmt.mora"},"options":{"tabSize":2,"insertSpaces":true}}"#,
        )],
    );
    let (_, result) = res
        .iter()
        .find(|(id, _)| id == "2")
        .expect("formatting 响应");
    let raw = result
        .split("\"newText\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("响应里应有 newText");
    // 反转义：抽到的是 **JSON 字符串字面量**，`\n` 是两个字符（反斜杠 + n）。
    // 直接写进文件会得到「字面 \n」的单行文本，解析必失败 —— 那是探针 bug 不是缺陷。
    let new_text = raw
        .replace("\\n", "\n")
        .replace("\\r", "\r")
        .replace("\\t", "\t")
        .replace("\\\"", "\"")
        .replace("\\\\", "\\");

    // 不得出现枚举变体名。
    //
    // ⚠ 必须按**词边界**判定：`print` 里就含 `int`（pr-int），
    // 子串匹配会把正确产物误判为缺陷（本轮第一版就栽在这里）。
    let is_word = |s: &str, w: &str| {
        s.match_indices(w).any(|(i, _)| {
            let before_ok = i == 0
                || !s[..i]
                    .chars()
                    .next_back()
                    .map(char::is_alphanumeric)
                    .unwrap_or(false);
            let after = i + w.len();
            let after_ok = after >= s.len()
                || !s[after..]
                    .chars()
                    .next()
                    .map(char::is_alphanumeric)
                    .unwrap_or(false);
            before_ok && after_ok
        })
    };
    for bad in [
        "assign", "lparen", "rparen", "plus", "comma", "int", "bigint",
    ] {
        assert!(
            !is_word(&new_text, bad),
            "格式化产物含枚举变体名 `{bad}`：\n{new_text}"
        );
    }

    // 判据：产物必须能被编译器接受，且语义不变（1+2 = 3）
    let dir = temp_dir();
    let f = dir.join("formatted.mora");
    std::fs::write(&f, &new_text).expect("write formatted");
    let out = Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg("run")
        .arg(&f)
        .output()
        .expect("run mora");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "格式化产物解析/执行失败 —— 格式化把能跑的代码改坏了。\n产物:\n{new_text}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("3.0"),
        "格式化应只动空白，语义不变（期望 3.0）。产物:\n{new_text}\nstdout:\n{stdout}"
    );
    let _ = std::fs::remove_file(&f);
}

/// D103：`includeDeclaration: true` 必须**包含声明处**。
#[test]
fn d103_references_honours_include_declaration() {
    let uri = "file:///tmp/refs.mora";
    let res = session(
        "refs",
        SRC,
        &[(
            "2",
            "textDocument/references",
            &format!(
                r#"{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":0,"character":5}},"context":{{"includeDeclaration":true}}}}"#
            ),
        )],
    );
    let (_, result) = res
        .iter()
        .find(|(id, _)| id == "2")
        .expect("references 响应");
    let n = result.matches("\"uri\"").count();
    assert_eq!(
        n, 2,
        "includeDeclaration=true 时应返回 2 处（声明 line1 + 使用 line3）。得到 {n} 处：\n{result}"
    );
}

/// D103 反向对照：`includeDeclaration: false`（规范默认）时**只**返回使用处。
#[test]
fn d103_references_excludes_declaration_by_default() {
    let uri = "file:///tmp/refs2.mora";
    let res = session(
        "refs2",
        SRC,
        &[(
            "2",
            "textDocument/references",
            &format!(
                r#"{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":0,"character":5}}}}"#
            ),
        )],
    );
    let (_, result) = res
        .iter()
        .find(|(id, _)| id == "2")
        .expect("references 响应");
    let n = result.matches("\"uri\"").count();
    assert_eq!(
        n, 1,
        "默认（includeDeclaration 缺省 false）应只返回 1 处使用"
    );
}
