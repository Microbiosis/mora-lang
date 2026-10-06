//! v0.104.6 D202 + D203：LSP 格式化器**静默删掉源码内容**，区间格式化返回
//! **与自己的 `range` 对不上**的 `newText`（已修）。
//!
//! ## D202：格式化把注释与空行删光
//!
//! `lsp/providers/formatting.rs::simple_format` 是**从 token 流重建整份文档**的。
//! 而 lexer 遇到 `--` 只前进、**不产出任何 token**（`lexer.rs` 的 `'-'` 分支），
//! `Newline` 又有「连续换行只出一个」的去重 —— 于是：
//!
//! | 输入 | 修前输出 | 后果 |
//! |---|---|---|
//! | 5 行 / 3 处 `--` | **4 行 / 0 处 `--`** | 注释全没了 |
//! | 6 行（含 2 空行 + 1 纯空白行） | **4 行** | 空行全没了 |
//!
//! 服务器声明了 `documentFormattingProvider: true`，而编辑器普遍支持
//! 「保存时格式化」—— 一按保存，用户的注释和空行就被清空。**静默的数据丢失。**
//!
//! 修法：lexer 加 `keep_comments` 开关（**默认关**，parser 那条路径逐字不变），
//! `Newline` 改为**无条件**换行。顺带得到一条**保行数不变式**
//! （输出与输入的 `\n` 个数相等），D203 依赖它。
//!
//! ## D203：`rangeFormatting` 的 `newText` 是整份文档，`range` 只有子区间
//!
//! LSP 对 `TextEdit` 的定义是「a text edit **replaces a range**」——
//! `newText` 是 range 的**内容**。修前 `simple_format` 的 `range` 形参被
//! `_range` **忽略**：客户端只要第 2–3 行，服务端却把**整份文档**放进一个
//! **只覆盖第 2–3 行**的 `range` 里：
//!
//! ```text
//! 请求 range = 行 1..2（0-based，文档共 4 行）
//! 修前应答 newText = 4 行整份文档,  range = 行 1..2
//! ```
//!
//! 客户端照 `range` 应用 → 把 4 行内容**替换进那 2 行**，文件凭空多出 2 行、
//! 代码被复制。修法：整份文档只格式化一次，再切出被请求的那几行，
//! 并把区间**吸附到整行**，使 `newText` 与 `range` 严格对应。
//!
//! ## 判据
//!
//! D202 主判据：**每一条注释的正文都必须原样出现在格式化结果里**；
//! 支撑不变式：保行数 + 程序行为不变。
//!
//! D203 主判据：**协议不变式** —— 把返回的 edit 应用到源码后，
//! 行数不变、**区间外的行逐字不变**、区间内的行恰为 `newText` 的各行。
//! 这条**不依赖本实现怎么写**，修前必红（会多出 2 行）。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// ===================================================================
// harness：真实 `mora-lsp.exe` + Content-Length 分帧
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
    // Content-Length 是**字节**长度
    format!("Content-Length: {}\r\n\r\n{}", json.len(), json)
}

/// 按 `Content-Length` 逐帧切出 JSON 正文。
///
/// ⚠ 不能简单 `split("\r\n\r\n")`：一份 stdout 里有多个帧，
/// 上一帧的正文后面紧跟着下一帧的 `Content-Length:` 头。
fn frame_bodies(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = raw;
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
        out.push(body[..cut].to_string());
        rest = &body[cut..];
    }
    out
}

/// 一条 `TextEdit`（只取本测试需要的字段）。
#[derive(Debug, Clone)]
struct Edit {
    start_line: usize,
    start_char: usize,
    end_line: usize,
    end_char: usize,
    new_text: String,
}

fn num(v: &mora::lsp::json::Value, key: &str) -> usize {
    v.get(key)
        .and_then(|n| n.as_i64())
        .unwrap_or_else(|| panic!("应答里没有数字字段 `{key}`"))
        .max(0) as usize
}

/// 每个测试一个**独立子目录** + Drop 守卫。
///
/// ⚠ 不能让所有测试共用一个目录再整体删除：`cargo test` 在同一进程里
/// **并行**跑测试，一个测试的 Drop 会把另一个**正在用**的目录删掉。
/// `lsp_formatting_indent.rs` 那几个测试至今没装守卫正是这个原因 ——
/// 本轮给自己这份补上，靠「每测试独立子目录」绕开并行互删。
struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d202_{tag}"));
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

/// 对一段源码发一条格式化请求，返回服务端给的 edit 列表。
///
/// `range` 为 `None` → `textDocument/formatting`；`Some((l0, l1))` →
/// `textDocument/rangeFormatting`（`end.character` 故意给一个**行中间**的
/// 值，模拟真实客户端）。
fn format_request(
    dir: &WorkDir,
    tag: &str,
    text: &str,
    range: Option<(usize, usize)>,
) -> Vec<Edit> {
    let input = dir.0.join(format!("{tag}.bin"));
    let uri = format!("file:///tmp/{tag}.mora");
    let open = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","languageId":"mora","version":1,"text":{}}}}}}}"#,
        json_string(text)
    );
    let (method, params) = match range {
        None => (
            "textDocument/formatting",
            format!(
                r#"{{"textDocument":{{"uri":"{uri}"}},"options":{{"tabSize":2,"insertSpaces":true}}}}"#
            ),
        ),
        Some((l0, l1)) => (
            "textDocument/rangeFormatting",
            format!(
                r#"{{"textDocument":{{"uri":"{uri}"}},"range":{{"start":{{"line":{l0},"character":0}},"end":{{"line":{l1},"character":3}}}},"options":{{"tabSize":2,"insertSpaces":true}}}}"#
            ),
        ),
    };
    let req = format!(r#"{{"jsonrpc":"2.0","id":91,"method":"{method}","params":{params}}}"#);
    std::fs::write(
        &input,
        format!(
            "{}{}{}{}",
            frame(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}"#
            ),
            frame(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#),
            frame(&open),
            frame(&req)
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
    let body = frame_bodies(&raw)
        .into_iter()
        .find(|b| b.contains("\"id\":91,"))
        .unwrap_or_else(|| panic!("没拿到格式化应答:\n{raw}"));

    // 用**服务器自己的 JSON 解析器**读应答（而不是字符串手术）
    let v = mora::lsp::json::Parser::new(&body)
        .parse_value()
        .unwrap_or_else(|e| panic!("应答不是合法 JSON: {e}\n{body}"));
    let edits = v
        .get("result")
        .and_then(|r| r.as_array())
        .unwrap_or_else(|| panic!("应答没有 result 数组:\n{body}"));
    edits
        .iter()
        .map(|e| {
            let r = e.get("range").expect("edit 没有 range");
            let (s, en) = (
                r.get("start").expect("range.start"),
                r.get("end").expect("range.end"),
            );
            Edit {
                start_line: num(s, "line"),
                start_char: num(s, "character"),
                end_line: num(en, "line"),
                end_char: num(en, "character"),
                new_text: e
                    .get("newText")
                    .and_then(|t| t.as_str())
                    .expect("edit 没有 newText")
                    .to_string(),
            }
        })
        .collect()
}

fn fmt(dir: &WorkDir, tag: &str, text: &str) -> String {
    let e = format_request(dir, tag, text, None);
    assert_eq!(
        e.len(),
        1,
        "整份格式化应当只回一条 edit（修 range 切片时别发多条）"
    );
    e[0].new_text.clone()
}

// ===================================================================
// 协议不变式的应用侧：把 edit 真的贴回源码
// ===================================================================

/// LSP 位置 → 源码字节偏移（`character` 按 char 计，与服务端同约定）。
fn offset_of(text: &str, line: usize, ch: usize) -> usize {
    let mut off = 0usize;
    for (i, l) in text.split_inclusive('\n').enumerate() {
        if i == line {
            return off + l.chars().take(ch).map(|c| c.len_utf8()).sum::<usize>();
        }
        off += l.len();
    }
    text.len()
}

/// 真实客户端会做的事：把 edit 贴回文档。
fn apply_edits(src: &str, edits: &[Edit]) -> String {
    let mut s = src.to_string();
    for e in edits {
        let a = offset_of(&s, e.start_line, e.start_char);
        let b = offset_of(&s, e.end_line, e.end_char);
        s.replace_range(a..b, &e.new_text);
    }
    s
}

/// **D203 主判据**：`newText` 必须恰是 `range` 的替换文本。
///
/// 逐行核对，不依赖本实现怎么组织格式化：
/// - 行数不变（既没复制、也没丢行）
/// - 区间**外**的行逐字不变
/// - 区间**内**的行等于 `newText` 的对应行
fn assert_edit_is_self_consistent(src: &str, edits: &[Edit], what: &str) {
    let got = apply_edits(src, edits);
    let src_lines: Vec<&str> = src.split('\n').collect();
    let got_lines: Vec<&str> = got.split('\n').collect();
    for e in edits {
        assert_eq!(
            got_lines.len(),
            src_lines.len(),
            "{what}: 应用 edit 后**行数变了** —— 说明 `newText` 与 `range` 对不上，\
             客户端会把内容复制或删掉。\nrange = 行 {}..{}\nnewText:\n{}\n结果:\n{}",
            e.start_line,
            e.end_line,
            e.new_text,
            got
        );
        for i in 0..src_lines.len() {
            if i >= e.start_line && i <= e.end_line {
                let want = e.new_text.split('\n').nth(i - e.start_line).unwrap_or("");
                assert_eq!(
                    got_lines[i],
                    want,
                    "{what}: 第 {i} 行应等于 newText 的第 {} 行。\nnewText:\n{}\n结果:\n{}",
                    i - e.start_line,
                    e.new_text,
                    got
                );
            } else {
                assert_eq!(
                    got_lines[i], src_lines[i],
                    "{what}: 第 {i} 行在区间 {}..{} **之外**，不该被改动。\n结果:\n{}",
                    e.start_line, e.end_line, got
                );
            }
        }
    }
}

/// 跑一份 Mora 源码，返回「程序自己的输出」（剥掉横幅）。
fn run_program(dir: &WorkDir, src: &str, name: &str) -> String {
    let f = dir.0.join(format!("{name}.mora"));
    std::fs::write(&f, src).expect("写源码");
    run_file(&f)
}

fn run_file(p: &Path) -> String {
    let mora = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(mora)
        .current_dir(p.parent().unwrap_or(Path::new(".")))
        .arg(p)
        .env_remove("OPENAI_API_KEY")
        .env_remove("MORA_AI_BASE_URL")
        .output()
        .expect("跑 mora");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
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
}

// ===================================================================
// D202：注释与空行不能被吞
// ===================================================================

/// 含 5 处 `--` 注释（文件头 / 行尾 / 独立行 / 块内 / 空注释）与 2 个空行的源码。
const SAMPLE: &str = "\
-- 文件头注释
let a = 1  -- 行尾注释

-- 独立注释行
for i in [1, 2]
  -- 块内注释
  let b = i * a  -- 块内行尾注释
  print(b)
end
--
print(a)
";

#[test]
fn d202_every_comment_survives_formatting() {
    let _dir = WorkDir::new("cmt");
    let f = fmt(&_dir, "cmt", SAMPLE);
    for body in [
        "文件头注释",
        "行尾注释",
        "独立注释行",
        "块内注释",
        "块内行尾注释",
    ] {
        assert!(
            f.contains(body),
            "注释 `{body}` 在格式化后**消失了** —— 格式化静默删掉了用户的注释。\n\
             修前：5 行 3 处注释 → 4 行 0 处注释。\n格式化结果:\n{f}"
        );
    }
    // 注释条数不得减少（`--` 出现次数 = 注释条数）
    assert_eq!(
        f.matches("--").count(),
        SAMPLE.matches("--").count(),
        "格式化后 `--` 的条数变了 —— 有注释被删或被复制。\n原文 {} 处 / 格式化后 {} 处:\n{f}",
        SAMPLE.matches("--").count(),
        f.matches("--").count()
    );
}

/// **空注释** `--` 本身也必须留住（它不带正文，靠上面那条按内容匹配抓不到）。
#[test]
fn d202_bare_double_dash_comment_survives() {
    let src = "let a = 1\n--\nlet b = 2\nprint(a + b)\n";
    let _dir = WorkDir::new("bare");
    let f = fmt(&_dir, "bare", src);
    assert_eq!(
        f.matches("--").count(),
        src.matches("--").count(),
        "空注释 `--` 被吞了:\n{f}"
    );
}

/// **支撑不变式（承重）**：输出与输入的 `\n` 个数必须相等。
///
/// D203 的区间切片依赖它 —— 一旦格式化让文档变短，「第 k 行」在两侧就对不上。
#[test]
fn d202_formatting_preserves_line_count() {
    let _dir = WorkDir::new("lcount");
    for (tag, src) in [
        ("lc1", SAMPLE),
        ("lc2", "let a = 1\n\n\nlet b = 2\n\nprint(a + b)\n"),
        ("lc3", "let a = 1\n   \n\t\nlet b = 2\nprint(a + b)\n"),
        ("lc4", "let a = 1"),
        ("lc5", "let a = 1\n"),
        ("lc6", "\n\n"),
        (
            "lc7",
            "for i in [1,2]\n  for j in [3,4]\n    print(i * j)\n  end\n  print(i)\nend\nprint(0)\n",
        ),
    ] {
        let f = fmt(&_dir, tag, src);
        assert_eq!(
            f.matches('\n').count(),
            src.matches('\n').count(),
            "[{tag}] 格式化改变了行数（修前空行被吞：6 行 → 4 行）:\n原文:\n{src}\n格式化:\n{f}"
        );
    }
}

/// **空行必须留在原处**（不只是总数相等）。
#[test]
fn d202_blank_lines_stay_in_place() {
    let src = "let a = 1\n\nlet b = 2\n   \nprint(a + b)\n";
    let _dir = WorkDir::new("blanks");
    let f = fmt(&_dir, "blanks", src);
    let s: Vec<&str> = src.split('\n').collect();
    let g: Vec<&str> = f.split('\n').collect();
    assert_eq!(s.len(), g.len(), "行数变了:\n{src}\n{f}");
    for i in 0..s.len() {
        if s[i].trim().is_empty() {
            assert_eq!(
                g[i].trim(),
                "",
                "第 {i} 行原本是空行，格式化后变成了 `{}`（修前空行被整个吞掉）:\n{f}",
                g[i]
            );
        }
    }
}

/// **正对照**：格式化**不改变语义** —— 运行结果逐字一致。
///
/// 注释被吞不改变语义，所以这条守不住主判据；它守的是另一条底线：
/// 格式化产物不能把能跑的代码改成跑不了的。
#[test]
fn d202_formatting_still_produces_runnable_code() {
    let _dir = WorkDir::new("runnable");
    let a = run_program(&_dir, SAMPLE, "d202_before");
    assert!(!a.is_empty(), "前提：样本程序应当有输出");
    let f = fmt(&_dir, "run", SAMPLE);
    let b = run_program(&_dir, &f, "d202_after");
    assert_eq!(
        a, b,
        "格式化**改变了程序行为**:\n原文:\n{a}\n格式化:\n{}\n跑出:\n{b}",
        f
    );
}

// ===================================================================
// D204：LSP 的 JSON 读取把**每个非 ASCII 字节**当 Latin-1 码点
// ===================================================================

/// 含中文**字符串字面量**的源码 —— 比注释更能说明后果：
/// 字符串内容变了，程序的输出就变了。
const SAMPLE_ZH: &str = "\
-- 中文注释：你好世界
let s = \"你好世界\"
print(s)
";

/// **D204 主判据（有牙齿）**：非 ASCII 文本必须**逐字**穿过 LSP 往返。
///
/// `lsp/json.rs::parse_string` 的兜底分支是 `out.push(c as char)`，
/// 而 `c: u8` —— Rust 的 `u8 as char` 做的是**Latin-1 解释**
/// （把字节当成同数值的码点），不是 UTF-8 解码。实测：
///
/// ```text
/// 原文   你好世界 = E6 96 87 E4 BB B6 …
/// 读进来 'æ' 'Ģ' 'ħ' …（每个字节一个码点），再写出去变成 C3 A6 C2 96 …
/// ```
///
/// 于是**任何含非 ASCII 的文件**，经格式化后中文全变成 `æä»¶` 这类乱码。
/// 注释里的乱码只是难看；**字符串字面量**里的乱码等于**改了程序**。
#[test]
fn d204_non_ascii_text_survives_the_lsp_round_trip() {
    let _dir = WorkDir::new("zh");
    let f = fmt(&_dir, "zh", SAMPLE_ZH);
    for piece in ["中文注释", "你好世界"] {
        assert!(
            f.contains(piece),
            "`{piece}` 在 LSP 往返后变成了乱码 —— 服务端的 JSON 读取把每个 UTF-8 \
             字节当成了 Latin-1 码点。\n格式化结果:\n{f}"
        );
    }
    // 程序行为也必须不变（字符串字面量被改 = 程序被改）
    let a = run_program(&_dir, SAMPLE_ZH, "d204_before");
    assert_eq!(a, "你好世界", "前提：样本程序应原样打印中文字符串");
    let b = run_program(&_dir, &f, "d204_after");
    assert_eq!(
        a, b,
        "格式化**改掉了字符串字面量的内容**:\n原文跑出: {a}\n格式化后跑出: {b}\n格式化:\n{f}"
    );
}

// ===================================================================
// D210：字符串字面量里的**控制字符**被原样吐出 —— 破坏保行数不变式
// ===================================================================

/// 含各种转义的字符串字面量的源码。
///
/// 注意这里的 `\n` 是**两个字符**（反斜杠 + n），即源码里写的就是转义序列；
/// lexer 会把它解码成真换行后才交给 `token_text`。
const SAMPLE_ESC: &str = "\
let a = \"x\\ny\"
let p   =   1
let q   =   2
print(len(a) + p + q)
";

/// **主判据（有牙齿）**：源码里的 `\n` / `\t` / `\\` / `\"` **不得**在格式化后
/// 变成**裸控制字符**。
///
/// 修前 `token_text` 只转义 `\` 与 `"`，于是 lexer 解码出的真换行被原样吐出
/// —— 输出里凭空多出一行。
#[test]
fn d210_string_literal_escapes_survive_formatting() {
    let _dir = WorkDir::new("esc");
    let f = fmt(&_dir, "esc", SAMPLE_ESC);
    // 断言 1：不得出现裸换行以外的任何裸控制字符（换行本身是分隔符，单独查行数）
    for (i, l) in f.lines().enumerate() {
        for c in l.chars() {
            assert!(
                (c as u32) >= 0x20,
                "[D210] 第 {i} 行含**裸控制字符** U+{:04X} —— 修前 `token_text` \
                 只转义 `\\` 与 `\"`，lexer 解码出的控制字符被原样吐出。\n格式化结果:\n{f}",
                c as u32
            );
        }
    }
    // 断言 2：`\n` 必须**仍然是转义序列**（两个字符），而不是真换行
    assert!(
        f.contains(r#""x\ny""#),
        "源码里的 `\\n` 应在格式化后仍是**转义序列**，修前变成了真换行。\n格式化结果:\n{f}"
    );
}

/// **保行数不变式**在含转义的源码上同样成立 —— D202 立的这条是 D203 的承重前提。
#[test]
fn d210_line_count_preserved_with_escapes() {
    let _dir = WorkDir::new("esclc");
    let f = fmt(&_dir, "esclc", SAMPLE_ESC);
    assert_eq!(
        f.matches('\n').count(),
        SAMPLE_ESC.matches('\n').count(),
        "[D210] 格式化**改变了行数** —— 字符串字面量里的 `\\n` 变成了真换行。\n\
         这一条一旦破了，D203 的区间切片就会取错行。\n原文:\n{SAMPLE_ESC}\n格式化:\n{f}"
    );
}

/// **D203 的承重后果**：区间格式化必须仍取到被请求的那几行。
///
/// 修前：保行数被破 → 映射错位 → 客户端要第 2–3 行，服务端返回
/// `y"` / `let p = 1`（第一行的字符串尾巴 + 第二行）—— 应用即损坏文件。
#[test]
fn d210_range_formatting_still_picks_the_right_lines() {
    let _dir = WorkDir::new("escrng");
    let edits = format_request(&_dir, "escrng", SAMPLE_ESC, Some((1, 2)));
    assert_eq!(edits.len(), 1);
    assert_edit_is_self_consistent(SAMPLE_ESC, &edits, "含转义源码的 rangeFormatting(1,2)");
    assert!(
        edits[0].new_text.contains("let p = 1") && edits[0].new_text.contains("let q = 2"),
        "[D210] 区间切片取错了行 —— 源码第 2–3 行是 `let p   =   1` / `let q   =   2`。\n\
         返回:\n{}",
        edits[0].new_text
    );
    assert!(
        !edits[0].new_text.contains("y\""),
        "[D210] 区间切片里混进了**上一行字符串的尾巴** `y\"`。\n返回:\n{}",
        edits[0].new_text
    );
}

/// **正对照**：格式化结果**仍能跑**，且输出与原文一致。
///
/// 修前结果其实也能跑（lexer 允许字符串跨行）—— 据实记录，那不是本条的缺陷；
/// 本条守的是行数与切片。
#[test]
fn d210_escaped_source_still_runs_after_formatting() {
    let _dir = WorkDir::new("escrun");
    let a = run_program(&_dir, SAMPLE_ESC, "d210_before");
    let f = fmt(&_dir, "escrun", SAMPLE_ESC);
    let b = run_program(&_dir, &f, "d210_after");
    assert_eq!(
        a, b,
        "[D210] 格式化**改变了程序行为**。\n原文跑出: {a}\n格式化后跑出: {b}\n格式化:\n{f}"
    );
    assert!(!a.is_empty(), "前提：样本程序应当有输出");
}

// ===================================================================
// D203：区间格式化的 edit 必须与自己的 range 对得上
// ===================================================================

/// **主判据（有牙齿）**：只格式化中间两行，**不得复制或丢失任何行**。
///
/// 修前：请求 `行 1..2`，`newText` 却是**全部 4 行** → 客户端把那 4 行
/// 替换进第 2–3 行 → 结果变成 6 行，`let a = 1` / `print(...)` 各多出一份。
#[test]
fn d203_range_formatting_does_not_duplicate_or_lose_lines() {
    let src = "let a = 1\nlet b   =   2\nlet c = 3\nprint(a + b + c)\n";
    let _dir = WorkDir::new("rng1");
    let edits = format_request(&_dir, "rng1", src, Some((1, 2)));
    assert_eq!(edits.len(), 1, "应当只回一条 edit");
    assert_edit_is_self_consistent(src, &edits, "rangeFormatting(1,2)");
    assert_eq!(
        edits[0].new_text.matches('\n').count() + 1,
        2,
        "请求 2 行，`newText` 就该是 2 行（修前是整份 4 行）:\n{}",
        edits[0].new_text
    );
    assert_eq!(
        (edits[0].start_line, edits[0].end_line),
        (1, 2),
        "`range` 应与请求的行区间一致（修前 echo 了请求的 range，却配了整份 newText）"
    );
}

/// 单行区间：`newText` 必须只有 1 行。
#[test]
fn d203_single_line_range_returns_one_line() {
    let src = "let a = 1\nlet b   =   2\nlet c = 3\nprint(a + b + c)\n";
    let _dir = WorkDir::new("rng2");
    let edits = format_request(&_dir, "rng2", src, Some((1, 1)));
    assert_eq!(edits.len(), 1);
    assert_eq!(
        edits[0].new_text.matches('\n').count(),
        0,
        "请求 1 行，`newText` 里不该有换行:\n{}",
        edits[0].new_text
    );
    assert_edit_is_self_consistent(src, &edits, "rangeFormatting(1,1)");
}

/// 整份文档格式化也必须满足同一条不变式（`range` 取全文，不能是 `(0,0)-(0,0)`）。
#[test]
fn d203_whole_document_edit_is_also_self_consistent() {
    let _dir = WorkDir::new("whole");
    for (tag, src) in [
        ("wd1", SAMPLE),
        ("wd2", "let a = 1\nlet b = 2\nprint(a + b)\n"),
        ("wd3", "let a = 1"),
        ("wd4", ""),
    ] {
        let edits = format_request(&_dir, tag, src, None);
        assert_eq!(edits.len(), 1, "[{tag}] 应当只回一条 edit");
        assert_edit_is_self_consistent(src, &edits, &format!("formatting({tag})"));
    }
}

/// 区间含注释时，切片必须把注释一起带上（否则区间格式化会吃掉注释）。
#[test]
fn d203_range_slice_carries_comments() {
    let _dir = WorkDir::new("rng3");
    let edits = format_request(&_dir, "rng3", SAMPLE, Some((0, 1)));
    assert_eq!(edits.len(), 1);
    assert!(
        edits[0].new_text.contains("文件头注释"),
        "区间切片丢了注释:\n{}",
        edits[0].new_text
    );
    assert_edit_is_self_consistent(SAMPLE, &edits, "rangeFormatting(0,1)");
}

/// 越界行号：不崩、返回结构合法（空 edit 列表是可以接受的答案，
/// 返回一条**范围错乱**的 edit 不是）。
#[test]
fn d203_out_of_range_line_is_rejected_cleanly() {
    let src = "let a = 1\nlet b = 2\nprint(a + b)\n";
    let _dir = WorkDir::new("rng4");
    let edits = format_request(&_dir, "rng4", src, Some((99, 120)));
    assert!(
        edits.is_empty(),
        "越界区间应回空 edit 列表（没有可格式化的行），却回了 {:?}",
        edits
    );
}

/// `end.line < start.line` 的畸形请求：不得 panic、不得返回错乱范围。
#[test]
fn d203_inverted_range_is_handled() {
    let src = "let a = 1\nlet b = 2\nlet c = 3\nprint(a + b + c)\n";
    let _dir = WorkDir::new("rng5");
    let edits = format_request(&_dir, "rng5", src, Some((2, 0)));
    for e in &edits {
        assert!(
            e.start_line <= e.end_line,
            "返回了 start > end 的 range: {}..{}",
            e.start_line,
            e.end_line
        );
    }
    assert_edit_is_self_consistent(src, &edits, "rangeFormatting(2,0)");
}
