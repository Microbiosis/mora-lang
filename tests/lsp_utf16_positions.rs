//! v0.104.6 D211：LSP `Position.character` 是 **UTF-16 码元**偏移，代码全程当
//! **char 索引** —— 含星平面字符（emoji）的行上**改坏用户文件**（已修）。
//!
//! ## 缺陷
//!
//! 规范原文：`Position.character` = "Character offset on a line in a text
//! document **(zero-based)**"，单位是 **UTF-16 code unit**（因为主流编辑器
//! 内部就是 UTF-16）。BMP 内 UTF-16 码元与 char 一一对应，故此前**从未暴露**。
//!
//! 真实 `mora-lsp.exe` 会话实测（客户端发的是**正确**的 UTF-16 位置，
//! 源码第 2 行 `print("😀", total)`，`total` 在 char 索引 11 / UTF-16 12）：
//!
//! | 路径 | 修前实际 | 期望 |
//! |---|---|---|
//! | `rename` total→sum | `print("😀",suml)` | `print("😀", sum)` |
//! | `didChange` 在 `total` 前插入 `X` | `print("😀", tXotal)` | `print("😀", Xtotal)` |
//!
//! `rename` 那条**把文件改坏了**（`suml`）；`didChange` 那条更狠 ——
//! **每敲一个键，服务器自己的文档缓冲就被写坏一次**，而 hover / definition /
//! 诊断全部基于这个缓冲，损伤会累积。
//!
//! ## 修法：一对转换函数，两个方向各一处入口
//!
//! - 入站：`position_to_offset` 先把 `col` 从 UTF-16 换算成 char 索引。
//!   它是**全部入站位置的唯一入口**（hover / definition / references /
//!   rename / completion / **增量同步**都走它）⇒ 改这一处即修全部入站。
//! - 出站：新增 `char_to_utf16_col`，凡是向客户端发 `character` 的地方
//!   都过这一层（hover / definition / references / documentSymbol /
//!   rename / formatting 的 range）。
//!
//! ## 判据
//!
//! **主判据写成「真实客户端会做的事」**：把返回的 edit 按 **UTF-16** 贴回
//! 文档，结果必须与源码语义一致。修前必红，且失败信息直接复现被改坏的文本。
//!
//! **正对照**：纯 BMP 行必须**逐字不变**（修前也正确，不能被本次修复带坏）。

use std::path::PathBuf;
use std::process::{Command, Stdio};

// ===================================================================
// harness：真实 mora-lsp.exe + Content-Length 分帧
// ===================================================================

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

struct WorkDir(PathBuf);
impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d211_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建目录");
        WorkDir(d)
    }
}
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 发一批消息，返回 id=91 的 `result`（用**服务器自己的 JSON 解析器**读）。
fn talk(tag: &str, msgs: &[String]) -> Option<mora::lsp::json::Value> {
    let dir = WorkDir::new(tag);
    let input = dir.0.join(format!("{tag}.bin"));
    let uri = format!("file:///tmp/{tag}.mora");
    let mut all: Vec<String> = vec![
        frame(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}"#,
        ),
        frame(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#),
    ];
    let _ = uri;
    for m in msgs {
        all.push(frame(m));
    }
    std::fs::write(&input, all.join("")).expect("写输入");
    let out = Command::new(env!("CARGO_BIN_EXE_mora-lsp"))
        .stdin(Stdio::from(std::fs::File::open(&input).expect("打开输入")))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .expect("跑 mora-lsp");
    let raw = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut rest = raw.as_str();
    while let Some(hp) = rest.find("\r\n\r\n") {
        let len: usize = rest[..hp]
            .lines()
            .find_map(|l| l.strip_prefix("Content-Length: "))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        let body = &rest[hp + 4..];
        let mut cut = len.min(body.len());
        while cut > 0 && !body.is_char_boundary(cut) {
            cut -= 1;
        }
        let v = mora::lsp::json::Parser::new(&body[..cut])
            .parse_value()
            .ok()?;
        if v.get("id").and_then(|i| i.as_i64()) == Some(91) {
            return v.get("result").cloned();
        }
        rest = &body[cut..];
    }
    None
}

fn did_open(uri: &str, text: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","languageId":"mora","version":1,"text":{}}}}}}}"#,
        json_string(text)
    )
}

/// 某子串的 **char 索引**。
///
/// ⚠ `str::find` 返回的是**字节**偏移，而本文件的样本里含 emoji（4 字节），
/// 直接当 char 索引用会偏 3 —— 我第一版就栽在这里，一度以为产品没修好。
/// （Python 的 `str.index` 是字符索引，所以脚本侧一直是对的。）
fn char_index_of(line: &str, needle: &str) -> usize {
    let b = line.find(needle).expect("样本含该子串");
    line[..b].chars().count()
}
/// char 索引 → UTF-16 码元偏移（**真实客户端**会发/会读的值）。
fn to_u16(line: &str, char_idx: usize) -> usize {
    line.chars().take(char_idx).map(char::len_utf16).sum()
}

/// 把一份 `TextEdit` 列表按 **UTF-16** 贴回文档 —— 真实客户端的做法。
///
/// 修前这条就是「文件被改坏」的地方，故判据必须**真的这么算**，
/// 而不是断言某个数字（断言数字只锁住了一部分）。
fn apply_edits_utf16(text: &str, edits: &[(usize, usize, usize, usize, String)]) -> String {
    let mut s = text.to_string();
    for (sl, sc, el, ec, new) in edits.iter().rev() {
        let lines: Vec<&str> = s.split('\n').collect();
        let base16: usize = lines[..*sl]
            .iter()
            .map(|l| l.chars().map(char::len_utf16).sum::<usize>() + 1)
            .sum();
        let a16 = base16 + sc;
        let base16b: usize = lines[..*el]
            .iter()
            .map(|l| l.chars().map(char::len_utf16).sum::<usize>() + 1)
            .sum();
        let b16 = base16b + ec;
        let u = s.encode_utf16().collect::<Vec<u16>>();
        let mut v: Vec<u16> = u[..a16].to_vec();
        v.extend(new.encode_utf16());
        v.extend(u[b16..].iter().copied());
        s = String::from_utf16(&v).expect("UTF-16 往返");
    }
    s
}

/// 从 rename 的应答里抽出 `(startLine, startChar, endLine, endChar, newText)`。
fn rename_edits(result: &mora::lsp::json::Value) -> Vec<(usize, usize, usize, usize, String)> {
    let mut out = Vec::new();
    let Some(changes) = result.get("changes") else {
        return out;
    };
    for list in changes.as_object().expect("changes 是对象").values() {
        for e in list.as_array().expect("edit 列表") {
            let r = e.get("range").expect("edit.range");
            let g = |p: &str, k: &str| {
                r.get(p)
                    .and_then(|x| x.get(k))
                    .and_then(|n| n.as_i64())
                    .unwrap_or(0) as usize
            };
            out.push((
                g("start", "line"),
                g("start", "character"),
                g("end", "line"),
                g("end", "character"),
                e.get("newText")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string(),
            ));
        }
    }
    out
}

// ===================================================================
// D211 判据
// ===================================================================

/// 源码里**有 emoji** 的样本。`total` 在 char 索引 11 / UTF-16 12。
const SRC_EMOJI: &str = "let total = 7\nprint(\"😀\", total)\n";
/// **正对照**：纯 BMP，`total` 的 char 索引与 UTF-16 偏移**相同**（11）。
const SRC_BMP: &str = "let total = 7\nprint(\"ab\", total)\n";

fn rename_probe(tag: &str, src: &str) -> String {
    let uri = format!("file:///tmp/{tag}.mora");
    let line1 = src.lines().nth(1).unwrap_or("");
    let ci = char_index_of(line1, "total");
    let pos = to_u16(line1, ci);
    let msgs = vec![
        did_open(&uri, src),
        format!(
            r#"{{"jsonrpc":"2.0","id":91,"method":"textDocument/rename","params":{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":1,"character":{pos}}},"newName":"sum"}}}}"#
        ),
    ];
    let r = talk(tag, &msgs).expect("拿到 rename 应答");
    let edits = rename_edits(&r);
    assert!(!edits.is_empty(), "[{tag}] rename 没有产生任何 edit");
    apply_edits_utf16(src, &edits)
}

/// **主判据（有牙齿）**：`rename` 后的文档必须语义正确。
///
/// 修前 `print("😀", total)` 会被改成 `print("😀",suml)` —— 名字被截了一个字符。
#[test]
fn d211_rename_on_a_line_with_emoji_does_not_corrupt_the_file() {
    let _d = WorkDir::new("rename");
    let got = rename_probe("rn_emoji", SRC_EMOJI);
    assert_eq!(
        got, "let sum = 7\nprint(\"😀\", sum)\n",
        "[D211] rename 把**含 emoji 的行改坏了** —— `character` 按 char 索引发出，\
         客户端按 UTF-16 应用，落在错误字符上。\n期望: let sum = 7 / print(\"😀\", sum)\n实际: {got}"
    );
}

/// **主判据**：增量同步（`didChange`）不得写坏**服务器自己的缓冲**。
///
/// 修前：客户端在 `total` 前插入 `X`，缓冲里变成 `tXotal`。
/// 客户端每敲一个键都会这样 —— 而 hover / 诊断全都基于这个缓冲。
#[test]
fn d211_incremental_change_on_emoji_line_keeps_buffer_intact() {
    let _d = WorkDir::new("chg");
    let uri = "file:///tmp/chg.mora";
    let line = SRC_EMOJI.trim_end().lines().nth(1).unwrap_or("");
    let pos = to_u16(line, char_index_of(line, "total"));
    // 先 didChange，再用一个请求把缓冲内容回显出来（格式化会原样重排）

    let msgs = vec![
        did_open(uri, SRC_EMOJI),
        format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didChange","params":{{"textDocument":{{"uri":"{uri}","version":2}},"contentChanges":[{{"range":{{"start":{{"line":1,"character":{pos}}},"end":{{"line":1,"character":{pos}}}}},"text":"X"}}]}}}}"#
        ),
        format!(
            r#"{{"jsonrpc":"2.0","id":91,"method":"textDocument/hover","params":{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":1,"character":{}}}}}}}"#,
            pos + 1
        ),
    ];
    let r = talk("chg_emoji", &msgs).expect("拿到 hover 应答");
    let txt = format!("{:?}", r);
    assert!(
        txt.contains("Xtotal"),
        "[D211] 增量同步把**服务器缓冲**改坏了 —— 期望 `Xtotal`，\
         实际 hover 报出的是别的东西。\n原始结果: {txt}\n\
         注意：客户端每敲一个键都会这样，而 hover / 诊断全基于这个缓冲。"
    );
}

/// **出站 range 必须是 UTF-16**：客户端按 UTF-16 读到的范围要正好是 `total`。
///
/// ⚠ 光标放在标识符**末尾**：`hover` 的 range 形状是 `[col - ident.len(), col]`，
/// 即**假定光标在标识符末尾**。光标在开头时它会给出「前 5 个字符」——
/// 那是**另一条与 UTF-16 无关的既有问题**（纯 BMP 行同样错），不在本条范围，
/// 已单独记档。把光标放末尾，本判据就只测 UTF-16 这一件事。
#[test]
fn d211_outbound_ranges_are_utf16_offsets() {
    let _d = WorkDir::new("rng");
    let uri = "file:///tmp/rng.mora";
    let line = SRC_EMOJI.trim_end().lines().nth(1).unwrap_or("");
    let ci = char_index_of(line, "total");
    let pos = to_u16(line, ci + "total".chars().count());
    for (method, tag) in [
        ("textDocument/hover", "hover"),
        ("textDocument/references", "refs"),
    ] {
        let msgs = vec![
            did_open(uri, SRC_EMOJI),
            format!(
                r#"{{"jsonrpc":"2.0","id":91,"method":"{method}","params":{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":1,"character":{pos}}},"context":{{"includeDeclaration":true}}}}}}"#
            ),
        ];
        let r = talk(&format!("rng_{tag}"), &msgs).expect("拿到应答");
        // 把 range 换算回源码：**按 range 自己所在的那一行**解码
        // （`references` 会返回 `total` 的**所有**出现位置，含第 0 行的声明，
        //  拿它们都去对第 1 行解码只会得到乱码 —— 我第一版就是这么写错的）
        let all_lines: Vec<&str> = SRC_EMOJI.trim_end().split('\n').collect();
        for (sl, sc, el, ec) in collect_ranges(&r) {
            let slice: String = if sl == el && sl < all_lines.len() {
                let u16s: Vec<u16> = all_lines[sl].encode_utf16().collect();
                String::from_utf16(&u16s[sc.min(u16s.len())..ec.min(u16s.len())])
                    .unwrap_or_else(|_| "<非法 UTF-16>".into())
            } else {
                "<跨行或越界>".into()
            };
            assert_eq!(
                slice, "total",
                "[D211] {method} 发出的 range 按 **UTF-16** 解读得到 `{slice}`，\
                 应为 `total`。服务端把 char 索引当 UTF-16 发出了。"
            );
        }
    }
}

fn collect_ranges(v: &mora::lsp::json::Value) -> Vec<(usize, usize, usize, usize)> {
    let mut out = Vec::new();
    if let Some(arr) = v.as_array() {
        for e in arr {
            push_range(e, &mut out);
        }
    }
    if let Some(r) = v.get("range") {
        push_range_from(r, &mut out);
    }
    out
}

fn push_range(e: &mora::lsp::json::Value, out: &mut Vec<(usize, usize, usize, usize)>) {
    if let Some(r) = e.get("range") {
        push_range_from(r, out);
    }
    if let Some(r) = e.get("location").and_then(|loc| loc.get("range")) {
        push_range_from(r, out);
    }
}

fn push_range_from(r: &mora::lsp::json::Value, out: &mut Vec<(usize, usize, usize, usize)>) {
    let g = |p: &str, k: &str| {
        r.get(p)
            .and_then(|x| x.get(k))
            .and_then(|n| n.as_i64())
            .unwrap_or(0) as usize
    };
    out.push((
        g("start", "line"),
        g("start", "character"),
        g("end", "line"),
        g("end", "character"),
    ));
}

/// **正对照（不回归）**：纯 BMP 行的 rename 结果必须与修前**逐字一致**。
#[test]
fn d211_bmp_only_line_is_untouched_by_the_fix() {
    let _d = WorkDir::new("bmp");
    let got = rename_probe("rn_bmp", SRC_BMP);
    assert_eq!(
        got, "let sum = 7\nprint(\"ab\", sum)\n",
        "[D211] 纯 BMP 行被本次修复带坏了（修前它是对的）:\n{got}"
    );
}

/// **转换函数本身的不变式**（不依赖 LSP 会话，最快的一层）。
#[test]
fn d211_utf16_conversion_helpers_are_inverses_on_real_lines() {
    use mora::lsp::providers::{char_to_utf16_col, utf16_to_char_col};
    for line in [
        "let a = 1",
        "print(\"😀\", total)",
        "😀😀😀",
        "中文字符串测试",
        "a😀b中c",
        "",
    ] {
        let n = line.chars().count();
        for ci in 0..=n {
            let u = char_to_utf16_col(line, ci);
            assert_eq!(
                utf16_to_char_col(line, u),
                ci,
                "[D211] 对 `{line}` 的 char 索引 {ci}：char_to_utf16 → {u}，\
                 再 utf16_to_char 回来却不是 {ci}"
            );
        }
        // 越过行尾必须**夹到行尾**而不是 panic
        assert_eq!(utf16_to_char_col(line, 9999), n, "col 越界应夹到行尾");
    }
}
