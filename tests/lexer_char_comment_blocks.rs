//! v0.104.6 D357 —— `src/lexer.rs` 的**字符字面量 / 生命周期 / 注释 / 文档块**四张矩阵（否定轮，无产品变更）
//!
//! D356 从数字字面量切入，翻出「位宽被拼进数值」的真缺陷后，
//! 本轮扫 `src/lexer.rs` 的其余**未测面**。四个子面 **20+ 用例，零缺陷**。
//!
//! ## 为什么这四张矩阵值得单独钉
//!
//! `src/lexer.rs` 是整条执行管线的最底层
//! （`ParserV3 → MirExpr → lower → MirInst → dag_interp` 全靠它切 token），
//! 且 D354 刚动过 keyword 相关逻辑，lexer 侧是它的**对偶面**。
//!
//! 每张矩阵都按「**正例 + 边界 + 负例**」三段写，理由各不同：
//!
//! | 矩阵 | 最容易错的地方 | 用例数 |
//! |---|---|---|
//! | 字符字面量 | 转义表 / 多字节 / 长度校验 | 8 |
//! | 生命周期 vs 字符 | `'` 后一个字符的**歧义消解** | 3 |
//! | 行注释 `--` | 注释里含 `"` / `--` | 7 |
//! | `document` 块 | 内容里的 `--` / `{` / 连续多块 | 4 |
//!
//! ## 生命周期：`'a` 的歧义消解是**有意设计**（不是漏写）
//!
//! `lexer.rs:449-481` 用「`'` 后一个字符 + 第二个字符」区分：
//! 第二个字符是 `'` ⇒ **字符字面量** `'a'`；
//! 是 `>` / `)` / `,` / 空格 / 换行 / 非字母数字 ⇒ **生命周期** `'a`。
//!
//! 实测三种歧义形态**全部正确**：
//! - `let a = 'x` + 换行 ⇒ 被当成生命周期，**不**误报字符错误
//! - `let a = 'x >` ⇒ 同上
//! - `fn f<'a>(x) … end` ⇒ 生命周期语法本身**不被支持**（exit 2），
//!   但这是 trait 泛型的覆盖面问题，**不是 lexer 缺陷**
//!
//! ## `document` 的上下文退化（D355 已记，此处补行为面）
//!
//! 下一 token 是字符串 ⇒ `TokenType::Document`（块语句）；
//! 否则退化成 `Identifier`（模块名 `document.parse(…)`）。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d357_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d357_{}_{}", n, slug(body)));
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, body).expect("写探针");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("建 home");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe)
        .arg(&p)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .expect("跑 mora");
    let _ = std::fs::remove_dir_all(&dir);
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let path_str = p.to_string_lossy().into_owned();
    let kept: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("Mora v")
                && !l.starts_with("AI:")
                && !l.starts_with("AI 原语")
                && !l.starts_with("显式 API")
                && !l.starts_with("Trait 系统")
                && !l.starts_with("Built-in")
                && !l.starts_with("v0.15 CLI")
                && !l.starts_with('⚠')
                && !l.starts_with("[9layer]")
                // ⚠ **只忽略纯路径行**，不能 `contains(p)` ——
                // lexer 的诊断格式是 `<绝对路径>: <消息>`，**消息与路径同行**。
                // 用 `contains` 会把整条诊断滤掉（首版踩过），
                // 症状是「exit=2 但 got 为空」，看着像产品没报错。
                && !is_bare_path_line(l, &path_str)
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

/// 判断某一行是否**只是**探针文件的绝对路径。
///
/// ⚠ 不能用 `line.contains(path)` —— lexer 诊断是 `<绝对路径>: <消息>`
/// **同行**格式，`contains` 会把整条诊断一起滤掉。症状是
/// 「exit=2 但 `got` 为空」，看着像产品没报错 —— 本轮首次发现。
/// typeck 层诊断是**独立行**，所以这个收紧对既有判据无影响。
fn is_bare_path_line(line: &&str, path: &str) -> bool {
    **line == *path
}

/// **装置自检**（D356 教训：`exit 0` 但输出为空时，
/// 要先分清是「采集器坏」还是「探针没输出」）。
#[test]
fn d357_harness_collects_print_output() {
    let (code, got) = ev("print(7)\n");
    assert_eq!(code, 0, "探针应正常退出; 实得 exit={code} out=[{got}]");
    assert_eq!(got.trim(), "7.0", "采集器失效（本文件全部断言依赖它）");
}

// ─────────────────── 字符字面量 ───────────────────

/// 字符字面量正例：单字符、多字节（emoji）、四个转义。
///
/// 验转义用 **char 对 char 比较**，不用 `len()`：
/// - `len(char)` 报 *len() expects a list, string, or dict*（既有契约，**非缺陷**）
/// - `char == "str"` 被 typeck 拒（*expected Char, got String*）——
///   这也**正确**：typeck 严格区分 Char 与 String
///
/// 首版误以为「`len` 不接受 char」是缺陷，实测才知道那是 `len` 的
/// 契约（它只对 list/string/dict 有定义）。**先查既有契约再判缺陷**。
#[test]
fn d357_char_literals_round_trip() {
    // 简单字符：绑定后可打印
    for (lit, expected) in [("'a'", "a"), ("'Z'", "Z"), ("'_'", "_")] {
        let (code, got) = ev(&format!("let a = {lit}\nprint(a)\n"));
        assert_eq!(code, 0, "`{lit}` 应合法; 实得 exit={code} out={got}");
        assert_eq!(got.trim(), expected, "`{lit}` 的值不对; 实得: {got}");
    }
    // 五个转义：与**同形式**的期望字面量比较（不打印 —— 换行/tab 会被显示吞掉）
    for esc in ["\\n", "\\t", "\\r", "\\'", "\\\\"] {
        let body = format!("let a = '{esc}'\nprint(a == '{esc}')\n");
        let (code, got) = ev(&body);
        assert_eq!(code, 0, "转义 `'{esc}'` 应合法; 实得 exit={code} out={got}");
        assert_eq!(
            got.trim(),
            "true",
            "`'{esc}'` 应自等（转义被正确解析）; 实得: {got}"
        );
    }
    // 转义与别的字符**不等**（防「所有转义都变成同一坨」）
    let (code, got) = ev("let a = '\\n'\nprint(a == 'z')\n");
    assert_eq!(code, 0, "应合法; 实得 exit={code} out={got}");
    assert_eq!(got.trim(), "false", "`'\\n'` 不该等于 `'z'`; 实得: {got}");

    // emoji 必须按**一个字符**处理（多字节 UTF-8 但单 char）。
    // 用 char 自等 + 与 'z' 不等两条来验，不依赖 len。
    let (code, got) = ev("let a = '😀'\nprint(a == '😀')\nprint(a == 'z')\n");
    assert_eq!(
        code, 0,
        "emoji 字符字面量应合法; 实得 exit={code} out={got}"
    );
    assert_eq!(
        got.trim(),
        "true | false",
        "emoji 应自等且不等于 'z'; 实得: {got}"
    );
}

/// 字符字面量负例：多字符、空、转义表外、悬空反斜杠。
#[test]
fn d357_char_literals_reject_malformed_input() {
    for (lit, what) in [
        ("'xy'", "两个字符"),
        ("''", "空字面量"),
        ("'\\q'", "转义表外的 `\\q`"),
    ] {
        let (code, got) = ev(&format!("let a = {lit}\nprint(1)\n"));
        assert_eq!(
            code, 2,
            "`{lit}`（{what}）应报错; 实得 exit={code} out={got}"
        );
        // ⚠ 只钉**错误类型与退出码**，不钉措辞 —— 各处措辞不同，
        // 钉子串会在无关的措辞清理时假红。
        assert!(
            got.contains("Char") || got.contains("char") || got.contains("escape"),
            "`{lit}` 的诊断应指向字符字面量; 实得: {got}"
        );
    }
}

// ─────────────────── 生命周期 vs 字符 ───────────────────

/// `'` 的歧义消解：`'x` 后跟**非闭合引号**应被当成生命周期，
/// **不**误报成「字符字面量缺闭合引号」。
///
/// 这是 `lexer.rs:449-481` 的**有意设计**（Rust 风格生命周期），
/// 判据钉住它是为了防止将来有人「收紧 `'x` 的判定」时误伤。
#[test]
fn d357_lifetime_disambiguation_does_not_misfire() {
    // `'x` 后跟换行 + 后续语句 ⇒ 整体不应报「字符字面量」错误
    for (body, what) in [
        ("let a = 'x\nprint(1)\n", "换行"),
        ("let a = 'x >\nprint(1)\n", "空格 + `>`"),
    ] {
        let (code, got) = ev(body);
        assert_eq!(code, 2, "`'x` 后跟{what} 的后续解析应报错（`a` 未绑定）");
        assert!(
            !got.contains("Char literal must contain"),
            "`'x` 不该被误判成字符字面量（生命周期是有意设计）; 实得: {got}"
        );
    }
}

/// 反向对照：真正的字符字面量 `'x'` **必须**被认成字符。
#[test]
fn d357_closed_quote_is_always_a_char() {
    let (code, got) = ev("let a = 'x'\nprint(a)\n");
    assert_eq!(
        code, 0,
        "闭合引号的 `'x'` 必须是字符字面量; 实得 exit={code} out={got}"
    );
    assert_eq!(got.trim(), "x");
}

// ─────────────────── 行注释 `--` ───────────────────

/// 行注释：独立行、尾随、未到文件末尾。
#[test]
fn d357_line_comments_are_stripped() {
    let (code, got) = ev("-- 头部注释\nprint(1)\n-- 中间\nprint(2)\n");
    assert_eq!(code, 0, "注释应被剥离; 实得 exit={code} out={got}");
    assert_eq!(got.trim(), "1.0 | 2.0", "两行 print 都应执行; 实得: {got}");

    // 尾随注释
    let (code, got) = ev("print(1) -- 尾随\nprint(2)\n");
    assert_eq!(code, 0, "尾随注释应被剥离; 实得 exit={code} out={got}");
    assert_eq!(got.trim(), "1.0 | 2.0", "实得: {got}");

    // 注释在文件末尾且无换行
    let (code, got) = ev("let x = 5\n-- 文件末尾的注释");
    assert_eq!(code, 0, "末尾注释应被剥离; 实得 exit={code} out={got}");
}

/// 注释与字符串的交互：字符串**里**的 `--` 不是注释；
/// 注释**里**的 `"` 不开启字符串。
///
/// 这是注释实现最容易写错的两侧，各测一条。
#[test]
fn d357_comment_string_interaction() {
    // 字符串里的 `--`
    let (code, got) = ev("let s = \"-- 不是注释\"\nprint(s)\n");
    assert_eq!(
        code, 0,
        "字符串里的 `--` 应保留; 实得 exit={code} out={got}"
    );
    assert_eq!(got.trim(), "-- 不是注释", "实得: {got}");

    // 注释里的 `"`（成对与未成对各一条）
    for body in [
        "let a = 1\n-- 注释里有 \" 引号\nlet b = 2\nprint(a)\nprint(b)\n",
        "let a = 1\n-- 未闭合的 \" 在注释里\nlet b = 2\nprint(a)\nprint(b)\n",
    ] {
        let (code, got) = ev(body);
        assert_eq!(
            code, 0,
            "注释里的引号不应开启字符串; 实得 exit={code} out={got}"
        );
        assert_eq!(got.trim(), "1.0 | 2.0", "两行都应执行; 实得: {got}");
    }
}

// ─────────────────── `document` 块 ───────────────────

/// `document` 块：内容里的 `--` 与 `{` 都不该被误处理。
#[test]
fn d357_document_block_content_is_opaque() {
    for (what, body) in [
        (
            "内容含 `--`",
            "document \"内容里有 -- 双横杠\"\n  print(1)\nend\nprint(2)\n",
        ),
        (
            "内容含未闭合 `{`",
            "document \"内容里有 {未闭合的括号\"\n  print(1)\nend\nprint(2)\n",
        ),
    ] {
        let (code, got) = ev(body);
        assert_eq!(
            code, 0,
            "document 块{what} 应正常跑; 实得 exit={code} out={got}"
        );
        assert_eq!(got.trim(), "1.0 | 2.0", "块内与块后都应执行; 实得: {got}");
    }
}

/// 多个 `document` 块连续出现。
#[test]
fn d357_consecutive_document_blocks() {
    let (code, got) =
        ev("document \"第一段\" do\n  print(1)\nend\ndocument \"第二段\" do\n  print(2)\nend\n");
    assert_eq!(
        code, 0,
        "连续 document 块应正常; 实得 exit={code} out={got}"
    );
    assert_eq!(got.trim(), "1.0 | 2.0", "两个块都应执行; 实得: {got}");
}
