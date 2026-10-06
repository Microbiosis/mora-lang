//! v0.104.6 D158：LSP `foldingRange` 的**行号值**全错，且多语句块**整个不折叠**（已修）。
//!
//! ## 缺陷一：`startLine` / `endLine` 直接透传 1-based
//!
//! LSP 的 `FoldingRange.startLine` / `endLine` 是 **0-based**（与 `Position.line`
//! 同约定），而 `Span::line` 是 **1-based**。`make_range` 直接塞进去，
//! 折叠箭头整体**下移一行**。同一 LSP 里另外 5 个 provider
//! （diagnostics / definition / references / rename / documentSymbol）
//! **都**有 `saturating_sub(1)` —— 与 D133 记的「这三个 provider 漏了」同型，
//! folding 是漏网的第四个。
//!
//! ## 缺陷二：多语句块**根本不产生折叠**
//!
//! `parser_v3/emit.rs::block_witness` 给多条语句的 body 构造 `Sequence` 时，
//! 用的是**外层构造的 span**（即 `task` 那一行）。于是
//! `body.span.line == expr.span.line` → `body.span.line > expr.span.line` 为假
//! → 区间不产生。
//!
//! ```mora
//! task alpha()      ← 第 1 行，body.span.line 也是 1 → 1 > 1 为假
//!   let x = 1       ← 第 2 行
//!   print(x)        ← 第 3 行
//! end                ← 第 4 行
//! ```
//!
//! 单语句 body 走 `Call` 等 witness，span 正确，所以**没暴露** ——
//! 真实文档几乎每个 task 都不止一条语句。
//!
//! ## 实测（真实 `mora-lsp.exe` + stdio 协议，文档含两个 task）
//!
//! | | 折叠区间数 | `alpha`（源码第 1 行） | `beta`（源码第 6 行） |
//! |---|---|---|---|
//! | 修复前 | **1** | **完全没有** | `{startLine:6, endLine:7}` ❌ 应为 `{5,6}` |
//! | 修复后 | **2** | `{startLine:0, …}` ✅ | `{startLine:5, …}` ✅ |
//!
//! ## 为什么既有 6 条 folding 测试全绿却没抓到
//!
//! `tests/lsp_folding_coverage.rs` 全部只断言**存在性**与**条数**
//! （`res.contains("\"startLine\"")`、`matches("\"startLine\"").count()`），
//! **从不校验行号的值** —— 值错了它照样绿。本文件补上值断言。
//!
//! ## 记档（**未改**）：折叠结束行取「体内最后一条语句」而非 `end` 关键字
//!
//! `FnDef` witness 不携带 `end` 的行号，构造式参数里拿不到。
//! 故结束行取体内最后一条语句所在行 —— 比修复前（多语句时压根不折叠、
//! 单语句时只到首句）好得多，但比 `end` 行**早 1–N 行**。
//! 要彻底准确需给 `FnDef` 增字段，属结构改动，本轮不扩大范围。

use std::io::Write;
use std::process::{Command, Stdio};

fn frame(json: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{}", json.len(), json)
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

/// 起一次真实 LSP 会话，返回 `foldingRange` 的结果文本。
fn folding(src: &str) -> String {
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#;
    let open = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"file:///f.mora","languageId":"mora","version":1,"text":{}}}}}}}"#,
        json_string(src)
    );
    let ask = r#"{"jsonrpc":"2.0","id":2,"method":"textDocument/foldingRange","params":{"textDocument":{"uri":"file:///f.mora"}}}"#;
    let exit = r#"{"jsonrpc":"2.0","method":"exit","params":{}}"#;
    let payload = frame(init) + &frame(&open) + &frame(ask) + &frame(exit);

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
        .expect("写 LSP 帧");
    let out =
        String::from_utf8_lossy(&child.wait_with_output().expect("等退出").stdout).into_owned();
    let i = out.find(r#""id":2"#).expect("拿到 foldingRange 响应");
    out[i..].to_string()
}

/// 抽出全部 `(startLine, endLine)`。
///
/// ⚠ LSP **输出**里的行号是**裸数字**（`"startLine":6`），不是字符串；
/// 输入侧我构造的 `text` 才是字符串。两种都要小心。
fn ranges(raw: &str) -> Vec<(usize, usize)> {
    let num_after = |s: &str| -> usize {
        s.split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|d| d.parse::<usize>().ok())
            .expect("行号数字")
    };
    let mut res = Vec::new();
    let mut rest = raw;
    const END_KEY: &str = "\"endLine\":";
    const START_KEY: &str = "\"startLine\":";
    while let Some(i) = rest.find(END_KEY) {
        let end = num_after(&rest[i + END_KEY.len()..]);
        // 序列化键按字母序：`{"endLine":N,"startLine":M}` —— startLine 在**后**。
        let start_at = rest[i..]
            .find(START_KEY)
            .map(|p| i + p)
            .expect("startLine 键");
        let start = num_after(&rest[start_at + START_KEY.len()..]);
        res.push((start, end));
        rest = &rest[i + END_KEY.len()..];
    }
    res
}

/// D158 主判据 ①：**多语句** `task` 必须**也**产生折叠区间（修复前完全没有）。
#[test]
fn d158_multi_statement_task_is_foldable() {
    let src = "task alpha()\n  let x = 1\n  print(x)\nend\n\ntask beta()\n  print(2)\nend\n";
    let raw = folding(src);
    let rs = ranges(&raw);
    assert_eq!(
        rs.len(),
        2,
        "两个 task 都应可折叠（修复前多语句的 `alpha` **整个不折叠**）; 实得: {raw}"
    );
    // 第一个区间必须属于 `alpha`（源码第 1 行 → 0-based 0）
    assert_eq!(
        rs[0].0, 0,
        "`alpha` 的 startLine 应是 0（源码第 1 行，0-based）; 实得: {raw}"
    );
    // 第二个区间属于 `beta`（源码第 6 行 → 0-based 5）
    assert_eq!(
        rs[1].0, 5,
        "`beta` 的 startLine 应是 5（源码第 6 行，0-based；**不是** 6）; 实得: {raw}"
    );
}

/// D158 主判据 ②：`startLine` / `endLine` 必须是 **0-based**。
///
/// 独立文档里 task 在第 1 行、体在第 2 行 → 应为 `(0, 1)`。
/// **修复前**给的是 `(1, 2)` —— 整体下移一行，编辑器里的折叠箭头落在错误处。
#[test]
fn d158_folding_lines_are_zero_based() {
    let raw = folding("task beta()\n  print(2)\nend\n");
    let rs = ranges(&raw);
    assert_eq!(rs.len(), 1, "单语句 task 应有一个折叠区间; 实得: {raw}");
    assert_eq!(
        rs[0],
        (0, 1),
        "源码第 1/2 行 → 0-based 必须是 (0, 1)；修复前给的是 (1, 2)（整体下移一行）; 实得: {raw}"
    );
}

/// D158 反向对照：行号换算改的是**每一个**折叠区间，不得只对 `task` 生效。
#[test]
fn d158_if_block_lines_also_zero_based() {
    // `if` 在源码第 1 行，体在第 2 行 → 应为 (0, 1)
    let raw = folding("if 1 == 1 then\n  print(1)\nend\n");
    let rs = ranges(&raw);
    assert_eq!(rs.len(), 1, "`if` 应可折叠; 实得: {raw}");
    assert_eq!(
        rs[0].0, 0,
        "`if` 在第 1 行 → startLine 必须是 0（0-based）; 实得: {raw}"
    );
}

/// D158 对照组：循环块（D136 修的）不得回退，且行号也要 0-based。
#[test]
fn d158_loop_folding_not_regressed() {
    for (name, src, want_start) in [
        ("for", "for i in range(0, 3)\n  print(i)\nend\n", 0usize),
        ("while", "let i = 0\nwhile i < 3\n  print(i)\nend\n", 1usize),
    ] {
        let raw = folding(src);
        let rs = ranges(&raw);
        assert!(
            !rs.is_empty(),
            "[{name}] 循环块必须仍可折叠（D136 修复不得回退）; 实得: {raw}"
        );
        assert_eq!(
            rs[0].0, want_start,
            "[{name}] startLine 应是 0-based 的 {want_start}; 实得: {raw}"
        );
    }
}
