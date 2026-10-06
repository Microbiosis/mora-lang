//! v0.104.6 D196：LSP 格式化器把**缩进层级挂在了错误的 token 上** —— 循环体顶格、
//! 循环**外**的语句反而被缩进（已修）。
//!
//! ## 缺陷
//!
//! `lsp/providers/formatting.rs::simple_format` 的层级增减是：
//!
//! | 位置 | 修前 | 应该是 |
//! |---|---|---|
//! | `TokenType::End`（**闭合**关键字） | `depth += 1` | `depth -= 1` |
//! | `LBrace`（开花括号） | 不变 | `depth += 1` |
//! | `RBrace`（闭花括号） | `depth -= 1` | `depth -= 1` ✅ |
//! | `RParen` / `RBracket` | `depth -= 1` | **不变**（`(` `[` 不是块） |
//! | 块**开启**方（`for`/`task`/`if`/…） | 什么都不做 | `depth += 1` |
//!
//! 即**没有任何开启方会加层级，而闭合方在加** —— `depth` 只会朝错误方向走。
//!
//! 真实 `mora-lsp.exe` 实测（`for … end` + 两行体 + 一行顶层语句）：
//!
//! ```text
//! 修前：                          修后：
//! for i in [1 , 2 ]              for i in [1 , 2 ]
//! let x = i            ← 顶格     ␣␣let x = i
//! print (x )           ← 顶格     ␣␣print (x )
//! end                             end
//! ␣␣print (0 )         ← 顶层反而缩进  print (0 )
//! ```
//!
//! 语义没坏（该语言的 parser 不看行首空白），但**视觉嵌套与实际嵌套相反** ——
//! 而正确缩进正是格式化器最基本的职责。
//!
//! ## 一个连带发现：`while` / `worker` / `handle` … 不是关键字 token
//!
//! 它们在 `lexer.rs` 的 `TokenType` 枚举里**不存在**（与 `else` 同类，
//! 走 `Identifier`）。所以层级追踪必须**按文本**认这些开启方，
//! 否则 `while … end` 的体永远顶格。
//!
//! `then` / `else` **不开**层级：`if x then … else … end` 的开启方是 `if`。
//!
//! ## 判据
//!
//! ① 块体必须缩进、闭合行与块**外**语句必须顶格（主判据，含嵌套）；
//! ② **格式化不改变语义**：格式化后的文本运行结果必须与原文**逐字一致**。

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
    let d = std::env::temp_dir().join("mora_d196_lsp");
    std::fs::create_dir_all(&d).expect("建目录");
    d
}

/// 对一段源码跑 LSP 格式化，返回格式化后的文本。
fn format_source(tag: &str, text: &str) -> String {
    let dir = temp_dir();
    let input = dir.join(format!("{tag}.bin"));
    let uri = format!("file:///tmp/{tag}.mora");
    let open = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","languageId":"mora","version":1,"text":{}}}}}}}"#,
        json_string(text)
    );
    let fmt = format!(
        r#"{{"jsonrpc":"2.0","id":91,"method":"textDocument/formatting","params":{{"textDocument":{{"uri":"{uri}"}},"options":{{"tabSize":2,"insertSpaces":true}}}}}}"#
    );
    std::fs::write(
        &input,
        format!(
            "{}{}{}{}",
            frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}"#),
            frame(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#),
            frame(&open),
            frame(&fmt)
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
    let part = raw
        .split("Content-Length")
        .find(|c| c.contains("\"id\":91,"))
        .unwrap_or_else(|| panic!("没拿到格式化应答:\n{}", raw));
    let nt = part
        .split("\"newText\":\"")
        .nth(1)
        .and_then(|s| s.split("\",\"range\"").next())
        .unwrap_or_else(|| panic!("应答里没有 newText:\n{}", part));
    // 解 JSON 字符串转义
    let mut out = String::new();
    let mut esc = false;
    for c in nt.chars() {
        if esc {
            match c {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                other => out.push(other),
            }
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else {
            out.push(c);
        }
    }
    out
}

/// 该行在格式化结果里的**前导空格数**。
fn indent_of(formatted: &str, needle: &str) -> usize {
    indent_of_nth(formatted, needle, 0)
}

/// 第 `n` 个（0 起）以 `needle` 开头的行的前导空格数。
///
/// ⚠ 必须能指定**第几个**：嵌套块里有多行 `end`（内层缩进 2、外层顶格）。
/// 我第一版只取「第一个匹配」，于是把内层 `end` 当成了外层 —— 测试红了，
/// 而**格式化器是对的**。D176 的又一次。
fn indent_of_nth(formatted: &str, needle: &str, n: usize) -> usize {
    formatted
        .lines()
        .filter(|l| l.trim_start().starts_with(needle))
        .nth(n)
        .unwrap_or_else(|| {
            panic!(
                "格式化结果里没有第 {} 个以 `{}` 开头的行:\n{}",
                n, needle, formatted
            )
        })
        .chars()
        .take_while(|c| *c == ' ')
        .count()
}

/// **主判据（有牙齿）**：`for … end` 里，块体缩进、闭合行与块外语句顶格。
#[test]
fn d196_block_body_is_indented_and_outside_statement_is_not() {
    let src = "for i in [1,2]\n  let x = i\n  print(x)\nend\nprint(0)\n";
    let f = format_source("b1", src);
    assert_eq!(
        indent_of(&f, "let x"),
        2,
        "`for` 的块体应缩进 2 空格（修前是 0，顶格）:\n{}",
        f
    );
    assert_eq!(
        indent_of_nth(&f, "end", 0),
        0,
        "唯一的 `end` 应与开启方同级（修前 body 顶格、end 后面的顶层语句被缩进）:\n{}",
        f
    );
    assert_eq!(
        indent_of(&f, "print (0"),
        0,
        "块**外**的语句必须顶格（修前被缩进 2 —— 层级加错了地方）:\n{}",
        f
    );
}

/// `while` 不是关键字 token（走 `Identifier`）—— 层级追踪必须按文本认它。
#[test]
fn d196_identifier_keyword_blocks_are_indented_too() {
    let src = "let i = 0\nwhile i < 3\n  print(i)\n  i = i + 1\nend\nprint(9)\n";
    let f = format_source("b2", src);
    assert_eq!(
        indent_of(&f, "print (i"),
        2,
        "`while` 的块体应缩进 2（`while` 走 Identifier，不按文本认就会顶格）:\n{}",
        f
    );
    assert_eq!(
        indent_of_nth(&f, "end", 0),
        0,
        "唯一的 `end` 应顶格:\n{}",
        f
    );
    assert_eq!(indent_of(&f, "print (9"), 0, "块外语句必须顶格:\n{}", f);
}

/// 嵌套块逐层缩进（每层 +2）。
#[test]
fn d196_nested_blocks_indent_per_level() {
    let src =
        "for i in [1,2]\n  for j in [3,4]\n    print(i * j)\n  end\n  print(i)\nend\nprint(0)\n";
    let f = format_source("c1", src);
    assert_eq!(
        indent_of(&f, "print (i * j"),
        4,
        "第二层块体应缩进 4:\n{}",
        f
    );
    assert_eq!(
        indent_of_nth(&f, "end", 1),
        0,
        "**最外层**（第 2 个）`end` 应顶格:\n{}",
        f
    );
    assert_eq!(
        indent_of_nth(&f, "end", 0),
        2,
        "第一层 `end` 缩进 2:\n{}",
        f
    );
    assert_eq!(indent_of(&f, "print (0"), 0, "块外顶格:\n{}", f);
}

/// **正对照**：格式化**不改变语义** —— 格式化后的文本运行结果与原文逐字一致。
///
/// 缩进错乱本身不改语义（该语言 parser 不看行首空白），但一个格式化器
/// 若改坏语义，那是另一回事、也更严重；这条守住那条底线。
#[test]
fn d196_formatting_preserves_program_behavior() {
    let dir = temp_dir();
    let mora = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let src =
        "for i in [1,2]\n  for j in [3,4]\n    print(i * j)\n  end\n  print(i)\nend\nprint(0)\n";
    let f = format_source("c2", src);

    let orig_file = dir.join("c2_orig.mora");
    let fmt_file = dir.join("c2_fmt.mora");
    std::fs::write(&orig_file, src).expect("写原文");
    std::fs::write(&fmt_file, &f).expect("写格式化结果");

    let run = |p: &std::path::Path| {
        let out = Command::new(mora)
            .current_dir(&dir)
            .arg(p)
            .env_remove("OPENAI_API_KEY")
            .env_remove("MORA_AI_BASE_URL")
            .output()
            .expect("跑 mora");
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        // 只留程序自己的输出行（剥掉横幅）
        s.lines()
            .filter(|l| {
                let t = l.trim();
                !t.is_empty()
                    && !t.starts_with("Mora v")
                    && !t.starts_with("AI:")
                    && !t.starts_with("AI 原语")
                    && !t.starts_with("显式 API")
                    && !t.starts_with("Trait 系统")
                    && !t.starts_with("Built-in")
                    && !t.starts_with("v0.15 CLI")
                    && !t.starts_with('⚠')
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let a = run(&orig_file);
    let b = run(&fmt_file);
    assert_eq!(
        a, b,
        "格式化**改变了程序行为**:\n原文:\n{}\n格式化:\n{}",
        a, b
    );
    assert!(!a.is_empty(), "前提：程序应当有输出");
}
