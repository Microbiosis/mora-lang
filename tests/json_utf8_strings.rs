//! v0.104.6 D205 + D206：`json.parse` 把**每个 UTF-8 字节当 Latin-1 码点**（长度
//! 也被改），并且**读不了标准的 `\uXXXX` 转义**（已修）。
//!
//! ## D205：字符串被拆成字节，**长度也错了**
//!
//! `flow/json.rs::parse_json_string` 的兜底分支原文：
//!
//! ```rust
//! c => result.push(c as char),   // c: u8
//! ```
//!
//! Rust 的 `u8 as char` 是 **Latin-1 解释**（把字节当成同数值的码点），
//! 不是 UTF-8 解码。真实 `mora run` 实测（修前）：
//!
//! ```text
//! json.parse("{\"greeting\": \"你好世界\"}")
//!   print(v["greeting"])  →  ä½ å¥½ä¸çå¥½
//!   print(len(...))      →  12        ← 正确的值是 4
//! ```
//!
//! 4 个汉字 = 12 个 UTF-8 字节 → 12 个码点。**长度被改了 3 倍**：
//! 任何按长度索引、切片、比较、哈希的下游全错，而且**看不出来** ——
//! 乱码至少是显眼的，错误的长度是隐形的（属 D198「静默给错数」）。
//!
//! ## D206：`\uXXXX` 直接报错 —— 读不了最常见的 JSON
//!
//! 修前的转义表只有 `\"` `\\` `\n` `\t` `\r` `\0`，其余一律
//! `Invalid escape`。而 **`\uXXXX` 是 JSON 标准的一部分**，且
//! **Python 的 `json.dumps()` 默认 `ensure_ascii=True` 就产出它**：
//!
//! ```text
//! json.parse("{\"g\": \"\u4f60\u597d\"}")
//!   → Runtime error (MIR): json.parse: Invalid escape: \u
//! ```
//!
//! 即**最常见的 JSON 生成方式**产出的含非 ASCII 文件，本语言根本读不了。
//! 同时补齐标准的 `\b` / `\f` / `\/`，以及代理对
//! （`😀` 在 ASCII 转义下是 `\uD83D\uDE00` 两个 UTF-16 码元）。
//!
//! ## 判据
//!
//! ① **长度必须等于字符数**（主判据，有牙齿：比内容判据更严，乱码长度也变了）；
//! ② 内容必须**逐字**还原；
//! ③ `\uXXXX` / 代理对必须能解析（D206 主判据）；
//! ④ ASCII 与既有转义**不回归**；⑤ `parse → stringify → parse` 往返不断链。

use std::path::PathBuf;
use std::process::Command;

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d205_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建目录");
        WorkDir(d)
    }

    /// 跑一份脚本，返回**程序自己的输出**（剥掉横幅）。
    fn run(&self, tag: &str, body: &str) -> String {
        let p = self.0.join(format!("{tag}.mora"));
        std::fs::write(&p, body).expect("写脚本");
        let out = Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/target/debug/mora.exe"
        ))
        .current_dir(&self.0)
        .arg(&p)
        .env_remove("OPENAI_API_KEY")
        .env_remove("MORA_AI_BASE_URL")
        .output()
        .expect("跑 mora");
        assert_eq!(
            out.status.code(),
            Some(0),
            "[{tag}] 应正常执行（修前 D206 会在此报 `Invalid escape: \\u`）:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
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
            .map(str::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 把一段文本变成 **Mora 普通字符串字面量**内部的转义形式。
///
/// ⚠ **Mora 源码里的字符串转义会吃掉一层反斜杠**：要让 JSON 里的 `\"`
/// 原样到达 JSON 解析器，Mora 源码里必须写 `\\\"`。这一层转义必须由本函数
/// 负责，否则测的就不是「JSON 解析对不对」而是「我写的 Mora 源码对不对」。
///
/// 我第一版没加它，测 `\"` 时拿到 `Expected ',' in dict` ——
/// 差点当成产品缺陷报上去。**D176「测试红了先问谁错了」的一次**。
fn mora_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// 造一个「解析某个 JSON 字符串值并打印内容与长度」的脚本。
///
/// `json_string_body` 是 JSON 字符串**内部**的原文（可含 JSON 转义）。
///
/// 长度那行是**主判据**：`len` 必须等于**字符数**。修前 4 个汉字得 12。
fn probe(json_string_body: &str) -> String {
    let json = format!("{{\"g\": \"{}\"}}", json_string_body);
    format!(
        "let j = \"{}\"\nlet v = json.parse(j)\nprint(v[\"g\"])\nprint(len(v[\"g\"]))\n",
        mora_escape(&json)
    )
}

/// **D205 主判据（有牙齿）**：原始 UTF-8 必须逐字还原，**且长度 = 字符数**。
#[test]
fn d205_raw_utf8_json_strings_round_trip_with_correct_length() {
    let dir = WorkDir::new("raw");
    // (JSON 里的值, 期望长度)
    let cases: &[(&str, usize)] = &[
        ("你好世界", 4),
        ("中", 1),
        ("é", 1),  // 2 字节的拉丁扩展
        ("€", 1),  // 3 字节
        ("😀", 1), // 4 字节（星平面）—— 修前会变成 4 个码点
        ("a中b", 3),
        ("  空格  ", 6),
        ("混合 mixed 123", 12),
    ];
    for (i, (val, want_len)) in cases.iter().enumerate() {
        let out = dir.run(&format!("raw{i}"), &probe(val));
        let mut lines = out.lines();
        let got_val = lines.next().unwrap_or_else(|| panic!("[{val}] 没有输出"));
        let got_len: usize = lines
            .next()
            .unwrap_or_else(|| panic!("[{val}] 没有输出长度行"))
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("[{val}] 长度行不是数字: {e}\n输出:\n{out}"));
        assert_eq!(
            got_val, *val,
            "[{val}] 内容在 `json.parse` 后**变了** —— UTF-8 被按 Latin-1 拆成了字节。\n输出:\n{out}"
        );
        assert_eq!(
            got_len, *want_len,
            "[{val}] **长度错了**（应为字符数 {want_len}，实得 {got_len}）—— \
             UTF-8 被按 Latin-1 拆成了字节。按长度切片/索引/比较的下游全会错。\n输出:\n{out}"
        );
    }
}

/// **D206 主判据**：`\uXXXX`（Python `json.dumps` 默认形态）必须能解析。
#[test]
fn d206_unicode_escapes_parse() {
    let dir = WorkDir::new("esc");
    let cases: &[(&str, &str, usize)] = &[
        (r"\u4f60\u597d\u4e16\u754c", "你好世界", 4), // Python json.dumps 默认输出
        (r"\u4e2d", "中", 1),
        (r"A\u0000B", "A\u{0000}B", 3), // NUL 也算一个字符
        (r"\ud83d\ude00", "😀", 1),     // 代理对 → 一个码点
        (r"caf\u00e9", "café", 4),      // 带重音的拉丁字母
        (r"\u4f60A", "你A", 2),         // 转义与原始字符混排
    ];
    for (i, (esc, want, want_len)) in cases.iter().enumerate() {
        let out = dir.run(&format!("esc{i}"), &probe(esc));
        let mut lines = out.lines();
        let got = lines.next().unwrap_or_else(|| panic!("[{esc}] 没有输出"));
        let got_len: usize = lines
            .next()
            .unwrap_or_else(|| panic!("[{esc}] 没有输出长度行"))
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("[{esc}] 长度行不是数字: {e}\n输出:\n{out}"));
        assert_eq!(got, *want, "[\\u{esc}] 解出的内容不对。\n输出:\n{out}");
        assert_eq!(
            got_len, *want_len,
            "[\\u{esc}] 长度不对（应 {want_len}，实得 {got_len}）:\n输出:\n{out}"
        );
    }
}

/// **不回归**：纯 ASCII 与修前就支持的转义，行为必须不变。
#[test]
fn d205_ascii_and_basic_escapes_do_not_regress() {
    let dir = WorkDir::new("ascii");
    let cases: &[(&str, &str, usize)] = &[
        (r"plain ascii", "plain ascii", 11),
        (r"tab\there", "tab\there", 8),
        (r"nl\nhere", "nl\nhere", 7), // 打印出来是两行，取第一行做内容断言
        // `\"` 解出的是 `q"q` —— 3 个字符（引号算 1 个）
        (r#"q\"q"#, r#"q"q"#, 3), // `\\` 解出的是 `back\slash` —— 10 个字符
        (r"back\\slash", r"back\slash", 10),
    ];
    for (i, (esc, want, want_len)) in cases.iter().enumerate() {
        let out = dir.run(&format!("ascii{i}"), &probe(esc));
        let lines: Vec<&str> = out.lines().collect();
        // 长度行恒为最后一行
        let got_len: usize = lines
            .last()
            .expect("没有输出")
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("[{esc}] 长度行不是数字: {e}\n输出:\n{out}"));
        assert_eq!(
            got_len, *want_len,
            "[{esc}] 长度变了（ASCII 路径不该受影响）:\n输出:\n{out}"
        );
        // 内容：把打印出的行重新拼起来应等于期望
        let got: String = lines[..lines.len() - 1].join("\n");
        assert_eq!(got, *want, "[{esc}] 内容不对:\n输出:\n{out}");
    }
}

/// `\b` / `\f` / `\/` 是 JSON 标准转义，此前也在「其余一律报错」里。
/// 长度用 `len` 钉住：修前这三种直接解析失败。
#[test]
fn d206_standard_escapes_backslash_formfeed_solidus_parse() {
    let dir = WorkDir::new("std");
    // `\f` / `\b` 是控制字符，打印出来不可见 —— 只钉长度 + 可打印部分
    for (i, (esc, want_len)) in [("a\\/b", 3), ("a\\bb", 3), ("a\\fb", 3)]
        .iter()
        .enumerate()
    {
        let json = format!("{{\"g\": \"{}\"}}", esc);
        let body = format!(
            "let j = \"{}\"\nlet v = json.parse(j)\nprint(len(v[\"g\"]))\n",
            mora_escape(&json)
        );
        let out = dir.run(&format!("std{i}"), &body);
        let got: usize = out
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("[{esc}] 输出不是数字: {e}\n输出:\n{out}"));
        assert_eq!(got, *want_len, "[{esc}] 长度不对:\n输出:\n{out}");
    }
}

/// `parse → stringify → parse` 往返不得断链（两边分别是 D205/D206 的对面）。
#[test]
fn d205_parse_stringify_parse_round_trip() {
    let dir = WorkDir::new("rt");
    let body = format!(
        "let a = json.parse(\"{}\")\nlet text = json.stringify(a)\nlet b = json.parse(text)\nprint(b[\"g\"])\nprint(len(b[\"g\"]))\n",
        mora_escape(r#"{"g": "你好世界"}"#)
    );
    let out = dir.run("rt", &body);
    let mut lines = out.lines();
    assert_eq!(lines.next(), Some("你好世界"), "往返后内容变了:\n{out}");
    assert_eq!(lines.next(), Some("4"), "往返后长度变了:\n{out}");
}

/// **负对照**：非法转义 / 落单的代理必须**明确报错**，
/// 不能静默产出半个字符（否则又是一份「看起来能用」的错数据）。
#[test]
fn d206_malformed_escapes_are_rejected_loudly() {
    let dir = WorkDir::new("bad");
    let mora = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    for (tag, json) in [
        ("trunc", "{\"g\": \"\\u12\"}"),
        ("nonnum", "{\"g\": \"\\uZZZZ\"}"),
        ("lone_hi", "{\"g\": \"\\ud83d\"}"),
        ("lone_lo", "{\"g\": \"\\ude00\"}"),
        ("badpair", "{\"g\": \"\\ud83d\\u0041\"}"),
    ] {
        let p = dir.0.join(format!("{tag}.mora"));
        let body = format!(
            "let j = \"{}\"\nlet v = json.parse(j)\nprint(v[\"g\"])\n",
            mora_escape(json)
        );
        std::fs::write(&p, body).expect("写脚本");
        let out = Command::new(mora)
            .current_dir(&dir.0)
            .arg(&p)
            .env_remove("OPENAI_API_KEY")
            .env_remove("MORA_AI_BASE_URL")
            .output()
            .expect("跑 mora");
        let err = String::from_utf8_lossy(&out.stderr);
        assert_ne!(
            out.status.code(),
            Some(0),
            "[{tag}] 畸形转义 `{json}` 应当**报错**，却成功了。输出:{}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            err.contains("escape"),
            "[{tag}] 报错信息应说明是转义问题，实际:\n{err}"
        );
    }
}
