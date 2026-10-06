//! v0.104.6 D193：LSP hover 对**每一个**符号都硬编码 `<inferred>` —— 信息量为零
//! （已修：**显式标注**现在会被报出来）。
//!
//! ## 缺陷
//!
//! `lsp/providers/hover.rs` 原本是：
//!
//! ```rust
//! let contents = format!("```mora\n{} {}: <inferred>\n```", kind, ident);
//! ```
//!
//! 字面量写死。实测（真实 `mora-lsp.exe` 走 stdio + Content-Length 帧）：
//!
//! ```text
//! let a: Int = 5   →  let a: <inferred>     ← 标注就在源码里
//! let b = 7.5      →  let b: <inferred>
//! let c: any = 1  →  let c: <inferred>
//! math.floor       →  variable floor: <inferred>
//! ```
//!
//! **连源码里明写着的类型标注都不报** —— hover 能告诉用户的只有「变量叫什么」
//! 和「它在哪一行」。用户悬停得不到任何新信息。
//!
//! ## 修法：只报**源码里确实写了**的类型
//!
//! 找 `let <ident>:` 后面的标注，报出来；找不到就仍回落 `<inferred>`。
//!
//! **刻意不接 HM 推断**：本语言有完整的 HM 推断与 `*_METHODS` 签名表
//! （D172/D175），把它们接进 LSP 是更大的工程；**猜出来的类型若与运行期
//! 不符，比 `<inferred>` 更糟** —— 它看起来可信。
//!
//! 匹配规则保守：必须是行首（可含缩进）的 `let`、名字**整词**匹配、
//! 标注取 `:` 之后到 `=` / `;` / 行尾之前。
//!
//! ## 判据
//!
//! ① 有**显式标注**的变量必须报出那个标注（主判据）；
//! ② 没有标注的**仍**回落 `<inferred>`（**不猜** —— 这是「诚实」这一半）；
//! ③ 名字**整词**匹配（`let ax` 不能匹配 `a`）—— 防止张冠李戴。

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
    let d = std::env::temp_dir().join("mora_d193_lsp");
    std::fs::create_dir_all(&d).expect("建临时目录");
    d
}

/// 打开一个文档，对若干位置取 hover，返回 `(id, hover 文本)` 列表。
fn hovers(tag: &str, text: &str, positions: &[(i64, i64)]) -> Vec<(i64, String)> {
    let dir = temp_dir();
    let input = dir.join(format!("{tag}.bin"));
    let uri = format!("file:///tmp/{tag}.mora");
    let doc = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","languageId":"mora","version":1,"text":{}}}}}}}"#,
        json_string(text)
    );
    let mut reqs = String::new();
    for (i, (line, ch)) in positions.iter().enumerate() {
        reqs.push_str(&frame(&format!(
            r#"{{"jsonrpc":"2.0","id":{},"method":"textDocument/hover","params":{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":{line},"character":{ch}}}}}}}"#,
            100 + i,
        )));
    }
    std::fs::write(
        &input,
        format!(
            "{}{}{}{}{}",
            frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}"#),
            frame(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#),
            frame(&doc),
            reqs,
            frame(r#"{"jsonrpc":"2.0","id":2,"method":"shutdown","params":null}"#),
        ),
    )
    .expect("写输入");

    let out = Command::new(env!("CARGO_BIN_EXE_mora-lsp"))
        .stdin(Stdio::from(std::fs::File::open(&input).expect("打开输入")))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .expect("跑 mora-lsp");
    let raw = String::from_utf8_lossy(&out.stdout).into_owned();

    let mut got = Vec::new();
    for chunk in raw.split("Content-Length") {
        if let Some(v) = chunk
            .split("\"value\":\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
        {
            let id = chunk
                .split("\"id\":")
                .nth(1)
                .and_then(|s| {
                    s.chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect::<String>()
                        .parse::<i64>()
                        .ok()
                })
                .unwrap_or(-1);
            got.push((id, v.replace("\\n", " ")));
        }
    }
    got
}

fn value_of(got: &[(i64, String)], id: i64) -> String {
    got.iter()
        .find(|(i, _)| *i == id)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| panic!("没拿到 id={} 的 hover", id))
}

/// **主判据（有牙齿）**：有**显式标注**的变量必须报出那个标注。
///
/// 修前一律 `<inferred>`。
#[test]
fn d193_hover_reports_explicit_annotations() {
    let text = "let a: Int = 5\nlet b = 7.5\nlet ax: String = \"x\"\nlet c: any = 1\n";
    let got = hovers("anno", text, &[(0, 4), (2, 4), (3, 4)]);

    // hover 的 value 外面包着 ```mora 围栏，故用 contains 而非 eq。
    // id 由 `hovers` 按位置序号从 100 递增：(0,4)→100, (2,4)→101, (3,4)→102。
    assert!(
        value_of(&got, 100).contains("let a: Int"),
        "`let a: Int = 5` 的标注就在源码里，hover 必须报出来（修前是 `<inferred>`）:\n{}",
        value_of(&got, 100)
    );
    assert!(
        value_of(&got, 101).contains("let ax: String"),
        "同上:\n{}",
        value_of(&got, 101)
    );
    assert!(
        value_of(&got, 102).contains("let c: any"),
        "同上:\n{}",
        value_of(&got, 102)
    );
}

/// **诚实那一半**：**没有**标注的**仍**报 `<inferred>` —— 不猜。
///
/// 猜出来的类型若与运行期不符，比 `<inferred>` 更糟：它看起来可信。
#[test]
fn d193_hover_still_says_inferred_when_there_is_no_annotation() {
    let text = "let b = 7.5\nprint(b)\n";
    let got = hovers("noanno", text, &[(0, 4)]);
    let v = value_of(&got, 100);
    assert!(
        v.contains("<inferred>"),
        "没有标注时必须诚实回落 `<inferred>`，不能猜一个类型:\n{}",
        v
    );
    assert!(
        !v.contains("Float") && !v.contains("float"),
        "不得凭空推断出具体类型:\n{}",
        v
    );
}

/// 名字必须**整词**匹配 —— `let ax: String` 不能被 `a` 认领。
#[test]
fn d193_hover_matches_the_whole_identifier() {
    let text = "let ax: String = \"x\"\nlet a = 1\n";
    // 悬停在**第二个** `a`（`let a = 1`，无标注）上。
    let got = hovers("whole", text, &[(1, 4)]);
    let v = value_of(&got, 100);
    assert!(
        v.contains("<inferred>"),
        "`a` 没有标注，不得被同文件里 `ax: String` 的标注张冠李戴:\n{}",
        v
    );
}
