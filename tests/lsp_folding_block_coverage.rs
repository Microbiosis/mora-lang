//! v0.104.6 D159：LSP `foldingRange` 的**覆盖面** —— 5 种块形态里只有 `task` 能折叠（已修）。
//!
//! ## D136 / D158 各修了一个维度，第三个维度没人碰
//!
//! - D136：补 `for` / `while` 的**覆盖面**（但没碰位置）
//! - D158：修 `startLine` / `endLine` 的**位置**（但覆盖面只到语句块）
//!
//! 声明块 / 配置块 / 可观测性块是**第三**个维度。实测（真实 LSP 会话，
//! 文档含 5 种块形态，源码行号 1-based）：
//!
//! | 形态 | 源码行 | 修复前 | 修复后 |
//! |---|---|---|---|
//! | `model Point … end` | 1–4 | ✗ | ✗（见下方「记档」） |
//! | `msg Move … end` | 6–9 | ✗ | ✗（见下方「记档」） |
//! | `task work() … end` | 11–15 | ✓ | ✓ |
//! | `with model = "m" … end` | 17–19 | ✗ | ✓ |
//! | `app Counter … end` | 21–27 | ✗ | ✓ |
//!
//! TEA 的 `app` 块动辄二十多行，不折叠等于在编辑器里完全收不起来。
//!
//! ## 记档（**未改**）：`model` / `msg` / `struct` / `enum` 仍不可折叠
//!
//! 这四种的字段是**纯字符串 / `TypeHint`**，witness 里**不带 span**
//! （`ModelDef { fields: Vec<(String, TypeHint)> }`、`EnumDef { variants: Vec<String> }`），
//! 结束行无从算起 —— 属 **parser 层缺口**（要么给字段记 span，要么给
//! 声明 witness 记 `end` 行）。本轮不动生产者，只在测试里把这条**写成
//! 显式 known-gap**（与 D138 记多行花括号 `match` 同样处理），免得后人
//! 以为「已覆盖全部形态」。

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
        .expect("写帧");
    let out =
        String::from_utf8_lossy(&child.wait_with_output().expect("等退出").stdout).into_owned();
    let i = out.find(r#""id":2"#).expect("拿到 foldingRange 响应");
    out[i..].to_string()
}

/// 抽出全部 `(startLine, endLine)`（**0-based**，LSP 规范值）。
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

/// D159 主判据 ①：`with` 块必须可折叠（修复前**完全不可折叠**）。
#[test]
fn d159_with_block_is_foldable() {
    let raw = folding("with model = \"m\"\n  print(1)\nend\n");
    let rs = ranges(&raw);
    assert_eq!(rs.len(), 1, "`with` 块应产生一个折叠区间; 实得: {raw}");
    assert_eq!(
        rs[0].0, 0,
        "`with` 在源码第 1 行 → startLine 应是 0（0-based）; 实得: {raw}"
    );
}

/// D159 主判据 ②：TEA `app` 块必须可折叠（修复前**完全不可折叠**）。
#[test]
fn d159_tea_app_block_is_foldable() {
    let src = "app Counter\n  model: Point\n  msg: Move\n  init: 0\n  \
               update: fn(msg, model) => model\n  view: fn(model) => model\nend\n";
    let raw = folding(src);
    let rs = ranges(&raw);
    assert_eq!(rs.len(), 1, "`app` 块应产生一个折叠区间; 实得: {raw}");
    assert_eq!(
        rs[0].0, 0,
        "`app` 在源码第 1 行 → startLine 应是 0; 实得: {raw}"
    );
    // 结束行应覆盖到 `view:` 那一行（源码第 6 行 → 0-based 5）
    assert_eq!(
        rs[0].1, 5,
        "endLine 应取三个子 witness 的最大行（`view:` 在第 6 行）; 实得: {raw}"
    );
}

/// D159 主判据 ③：命名 section 块（`prompt` / `document`）与 `parallel` 必须可折叠。
#[test]
fn d159_section_and_parallel_blocks_are_foldable() {
    for (name, src) in [
        (
            "prompt",
            "prompt \"system\" do\n  \"You are helpful.\"\nend\n",
        ),
        ("parallel", "parallel\n  let a = 1\n  let b = 2\nend\n"),
    ] {
        let raw = folding(src);
        let rs = ranges(&raw);
        assert_eq!(rs.len(), 1, "[{name}] 应可折叠; 实得: {raw}");
        assert_eq!(rs[0].0, 0, "[{name}] startLine 应是 0; 实得: {raw}");
    }
}

/// D159 主判据 ④：混合文档里**每种**块各出一个区间，且互不错位。
#[test]
fn d159_each_block_kind_yields_its_own_range() {
    let src = "model Point\n  x: number\n  y: number\nend\n\ntask work()\n  let a = 1\n  let b = 2\n  print(a + b)\nend\n\nwith model = \"m\"\n  print(1)\nend\n";
    let raw = folding(src);
    let rs = ranges(&raw);
    let starts: Vec<usize> = rs.iter().map(|r| r.0).collect();
    assert_eq!(
        starts,
        vec![5, 11],
        "应恰好两段：`task`（源码第 6 行 → 0-based 5）与 `with`（第 12 行 → 11）; 实得: {raw}"
    );
}

/// D159 反向对照：D136 补的循环块与 D158 修的 `task` / `if` 不得回退。
#[test]
fn d159_previously_working_blocks_not_regressed() {
    for (name, src) in [
        ("if", "if 1 == 1 then\n  print(1)\nend\n"),
        ("for", "for i in range(0, 3)\n  print(i)\nend\n"),
        ("while", "let i = 0\nwhile i < 3\n  print(i)\nend\n"),
        ("task", "task t()\n  let a = 1\n  print(a)\nend\n"),
    ] {
        let raw = folding(src);
        assert!(
            !ranges(&raw).is_empty(),
            "[{name}] 必须仍可折叠（D136/D158 的修复不得回退）; 实得: {raw}"
        );
    }
}

/// **已知缺口**（非缺陷断言）：`model` / `msg` 声明不可折叠 —— parser 层
/// 没有给字段记 span，结束行无从算起。
///
/// 写成**显式** known-gap 而不是「忘了测」，是为了让「覆盖面普查」的结论
/// 可查：把一个未覆盖项记成待办，比让它混在「已覆盖」里强。
/// 若将来 parser 补上字段 span，本条会红，提醒把 arm 补进 `folding.rs`。
#[test]
fn d159_model_and_msg_declarations_are_a_known_gap() {
    let src = "model Point\n  x: number\n  y: number\nend\n\nmsg Move\n  x\n  y\nend\n";
    let raw = folding(src);
    let rs = ranges(&raw);
    assert!(
        rs.is_empty(),
        "已知缺口：`model` / `msg` 尚不可折叠（witness 字段无 span）。\
         若本条转红，说明 parser 已补上字段 span —— 请在 `folding.rs` 为 \
         `ModelDef` / `MsgDef` / `StructDef` / `EnumDef` 补 arm 后再更新本条; 实得: {raw}"
    );
}
