//! v0.104.6 D195：LSP 声明 `triggerCharacters: [":"]`，却在**那个位置返回空**（已修：撤掉声明）。
//!
//! ## 缺陷
//!
//! capabilities 告诉编辑器「用户一打冒号就向我请求补全」。实测（真实
//! `mora-lsp.exe` 走 stdio）：
//!
//! | 位置 | 返回条目数 |
//! |---|---|
//! | 普通位置 `print(x)` 里的 → | **35** |
//! | 紧跟 `let x: ` | **0** |
//! | `with` 块内的缩进行 | **0** |
//!
//! **宣称会在某个位置提供补全，却在那里什么都不给** —— 与 D175 的
//! `methods_of` 空集、D186 的 MCP 名字目录同族。用户每打一个冒号
//! （类型标注 / dict 字面量 / `with` 块）都闪一个空列表。
//!
//! ## 修法：撤声明，而不是补实现
//!
//! 真正兑现这个触发需要一份「类型名清单」，而那必然是**第三份**要维护的
//! 名字表（已有 `Type` 枚举、`typeck` 的类型名映射），正是 D175/D189
//! 记过的那种漂移陷阱。**不宣称做不到的事**更诚实，也更小。
//!
//! ## 判据是**自适应**的
//!
//! 本测试**允许将来重新声明 `":"`** —— 条件是那时在冒号后**确实**返回条目。
//! 这样「补上这个功能」不需要改判据，而「只加回声明不补功能」会立刻红。

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
    let d = std::env::temp_dir().join("mora_d195_lsp");
    std::fs::create_dir_all(&d).expect("建目录");
    d
}

/// 跑一个会话，返回原始 stdout。
fn session(tag: &str, text: &str, requests: &str) -> String {
    let dir = temp_dir();
    let input = dir.join(format!("{tag}.bin"));
    let uri = format!("file:///tmp/{tag}.mora");
    let doc = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","languageId":"mora","version":1,"text":{}}}}}}}"#,
        json_string(text)
    );
    std::fs::write(
        &input,
        format!(
            "{}{}{}{}",
            frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}"#),
            frame(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#),
            frame(&doc),
            requests
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

/// 主判据：**声明的每个 triggerCharacter，都必须真的能换来补全。**
#[test]
fn d195_every_declared_trigger_character_actually_yields_completions() {
    // 1) 读 capabilities 里声明的 triggerCharacters
    let caps = session("caps", "let x = 1\nprint(x)\n", "");
    let triggers: Vec<String> = caps
        .split("Content-Length")
        .find(|c| c.contains("\"id\":1,"))
        .and_then(|c| {
            let a = c.find("\"triggerCharacters\":[")?;
            let b = c[a..].find(']')? + a;
            Some(c[a..b].to_string())
        })
        .map(|s| {
            s.trim_start_matches("\"triggerCharacters\":[")
                .trim_end_matches(']')
                .split(',')
                .filter(|t| !t.trim().is_empty())
                .map(|t| t.trim().trim_matches('"').to_string())
                .collect()
        })
        .unwrap_or_default();

    if triggers.is_empty() {
        // 当前状态：没有声明任何触发字符 —— 一致，无可指责。
        return;
    }

    // 2) 对每个声明的字符，在「刚打完它」的位置请求补全，必须拿到条目。
    //    ⚠ 探针必须**把该字符真的放进文档**、光标跟在它后面 ——
    //    否则位置落在别处，判据就成了摆设（我第一版正是如此：
    //    文档写的是 `let x = 1`，却去问 (0,7)，那里根本没有 `:`）。
    for t in triggers {
        let typed = format!("let x{t}\n");
        let col = "let x".chars().count() as i64 + t.chars().count() as i64;
        let open = format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"file:///tmp/trg.mora","languageId":"mora","version":1,"text":{}}}}}}}"#,
            json_string(&typed)
        );
        let req = format!(
            "{}{}",
            frame(&open),
            frame(&format!(
                r#"{{"jsonrpc":"2.0","id":80,"method":"textDocument/completion","params":{{"textDocument":{{"uri":"file:///tmp/trg.mora"}},"position":{{"line":0,"character":{}}}}}}}"#,
                col
            ))
        );
        let raw = session("trg", &typed, &req);
        let part = raw
            .split("Content-Length")
            .find(|c| c.contains("\"id\":80,"))
            .unwrap_or_else(|| panic!("没拿到补全应答:\n{}", raw));
        let n = part.matches("\"label\":").count();
        assert!(
            n > 0,
            "capabilities 声明了 `{}` 作为 triggerCharacter，但用户刚打完它时\
             服务器返回 **0 条**补全 —— 宣称能做却什么都不给，编辑器只会\
             闪一个空列表。\n实现该触发、或撤掉该声明。",
            t
        );
    }
}

/// **对照组**：普通位置的补全必须仍然正常 —— 修的是「空承诺」，
/// 不是把补全整体弄坏。
#[test]
fn d195_normal_position_completion_still_works() {
    let req = format!(
        "{}{}",
        frame(
            r#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///tmp/ok.mora","languageId":"mora","version":1,"text":"let x = 1\nprint(x)\n"}}}"#
        ),
        frame(
            r#"{"jsonrpc":"2.0","id":81,"method":"textDocument/completion","params":{"textDocument":{"uri":"file:///tmp/ok.mora"},"position":{"line":1,"character":6}}}"#
        )
    );
    let raw = session("ok", "let x = 1\nprint(x)\n", &req);
    let part = raw
        .split("Content-Length")
        .find(|c| c.contains("\"id\":81,"))
        .unwrap_or_else(|| panic!("没拿到补全应答:\n{}", raw));
    let n = part.matches("\"label\":").count();
    assert!(
        n > 10,
        "普通位置的补全必须照常工作（修的是「空承诺」，不是补全本身）; 实得 {} 条",
        n
    );
    assert!(
        part.contains("\"label\":\"x\""),
        "应包含当前作用域里的变量 `x`:\n{}",
        part.chars().take(400).collect::<String>()
    );
}
