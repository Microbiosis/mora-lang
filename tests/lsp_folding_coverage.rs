//! v0.104.6 D136：LSP `foldingRange` 对 `for` / `while` **完全不产生折叠范围**（已修）。
//!
//! ## 缺陷
//!
//! `folding.rs::collect_folds` 此前只处理 `If` / `Match` / `FnDef` / `Closure`
//! 四种 witness kind。实测（真实 LSP 会话）：
//!
//! | 块构造 | 修复前 | 修复后 |
//! |---|---|---|
//! | `task` | ✓ | ✓ |
//! | `if` | ✓ | ✓ |
//! | `task` 套 `if` | ✓ 两层 | ✓ 两层 |
//! | **`for`** | **`[]`** ✗ | ✓ |
//! | **`while`** | **`[]`** ✗ | ✓ |
//! | `for` 套 `if` | **`[]`** ✗ | ✓ 两层 |
//!
//! 循环块恰恰是编辑器里最需要折叠的（长循环会淹没视线），却一个都不折叠。
//!
//! ## 一个容易踩空的细节
//!
//! **`for` 在 witness 层是 `Loop`（`{ var, iterable, body }`）而**不是** `For`
//! —— 按 `For` 写 match 分支永远匹配不到（我最初就是这么写的）。

use std::path::PathBuf;
use std::process::Command;

fn temp_dir() -> PathBuf {
    let d = std::env::temp_dir().join("mora_d136_fold");
    std::fs::create_dir_all(&d).expect("create");
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

fn folding(tag: &str, src: &str) -> String {
    let dir = temp_dir();
    let input = dir.join(format!("{tag}.bin"));
    let uri = format!("file:///tmp/f_{tag}.mora");
    let msgs = [
        frame(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}"#,
        ),
        frame(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#),
        frame(&format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","languageId":"mora","version":1,"text":{}}}}}}}"#,
            json_string(src)
        )),
        frame(&format!(
            r#"{{"jsonrpc":"2.0","id":3,"method":"textDocument/foldingRange","params":{{"textDocument":{{"uri":"{uri}"}}}}}}"#
        )),
        frame(r#"{"jsonrpc":"2.0","id":99,"method":"shutdown","params":null}"#),
    ]
    .concat();
    std::fs::write(&input, msgs).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_mora-lsp"))
        .stdin(std::process::Stdio::from(
            std::fs::File::open(&input).expect("open"),
        ))
        .output()
        .expect("run");
    let _ = std::fs::remove_file(&input);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    split_frames(&stdout)
        .into_iter()
        .find(|f| f.contains("\"id\":3"))
        .unwrap_or_default()
}

/// D136 主判据：`for` 与 `while` 都必须产生折叠范围。
#[test]
fn d136_for_and_while_produce_folding_ranges() {
    for (name, src) in [
        ("for", "for i in [1, 2]\n  print(i)\nend\n"),
        ("while", "let n = 0\nwhile n < 2\n  n = n + 1\nend\n"),
    ] {
        let res = folding(name, src);
        assert!(
            res.contains("\"startLine\""),
            "[{name}] 必须产生折叠范围（修复前恒为 `[]`）; 实际: {res}"
        );
    }
}

/// 对照组 ①：嵌套时**两层**都要出（证明递归进了循环体，不只是顶层）。
#[test]
fn d136_nested_blocks_yield_nested_ranges() {
    let res = folding(
        "nested",
        "for i in [1, 2]\n  if i == 1 then\n    print(i)\n  end\nend\n",
    );
    assert_eq!(
        res.matches("\"startLine\"").count(),
        2,
        "`for` 套 `if` 应产生**两层**折叠范围; 实际: {res}"
    );
}

/// 对照组 ②：原本就正常的 `if` / `task` **不得回退**。
#[test]
fn d136_preexisting_if_and_task_folding_still_work() {
    for (name, src) in [
        ("if", "if 1 == 1 then\n  print(1)\nend\n"),
        ("task", "task d()\n  print(1)\nend\n"),
    ] {
        let res = folding(name, src);
        assert!(
            res.contains("\"startLine\""),
            "[{name}] 原本就正常，不得回退; 实际: {res}"
        );
    }
}

/// D138 边界：花括号形态 `match` 的 arm body **只能单行表达式**（块体非法）。
///
/// ```mora
/// let r = match v {
///   1 -> {          ← 语法非法：`Expected '}' at line 3`
///     print(1)
///   }
///   _ -> print(2)
/// }
/// ```
///
/// `emit_match_w` 的注释（v0.104.3）已写明两种形态「仅**体终止符**
/// （`end` / `}`）不同」，即花括号 arm 体就是单行 expr。
///
/// 这条判据的作用是**防止后人再拿一个非法形态去测 folding** ——
/// D136 与 D137 各因此误判过一次，都以为发现了缺口。
#[test]
fn d138_brace_match_rejects_a_block_body() {
    let dir = temp_dir();
    let src =
        "let v = 1\nlet r = match v {\n  1 -> {\n    print(1)\n  }\n  _ -> print(2)\n}\nprint(r)\n";
    let f = dir.join("bad_brace_block.mora");
    std::fs::write(&f, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg("--check")
        .arg(&f)
        .output()
        .expect("run");
    let _ = std::fs::remove_file(&f);
    assert!(
        !out.status.success(),
        "花括号 arm body 是块体时应**解析失败**（`Expected '}}'`）—— \
         若本测试失败，说明语法已放宽，届时可以重新测该形态的 foldingRange"
    );
}

/// D138 查明：**合法**的花括号形态（arm 跨两行）也不折叠 —— 已知次要缺口。
///
/// ```mora
/// let r = match v { 1 -> 10
///   _ -> 20 }
/// ```
/// arm body 在 line 2 / line 3、`expr.span.line = 2`，按
/// `end_line(3) > expr.span.line(2)` **本应**产出折叠范围，实测 `[]`。
///
/// 本测试锁住**现状**而非正确行为：若日后修好了（`res` 含 `startLine`），
/// 本测试会红并提醒更新 CHANGELOG D138 的「仍存的次要缺口」。
#[test]
fn d138_legal_multiline_brace_match_still_not_folded_known_gap() {
    let res = folding(
        "brace_gap",
        "let v = 1\nlet r = match v { 1 -> 10\n  _ -> 20 }\nprint(r)\n",
    );
    if res.contains("\"startLine\"") {
        panic!(
            "合法的多行花括号 match 现在**已可折叠**了 —— \
             请更新 CHANGELOG D138 的「仍存的次要缺口」段落。实际响应: {res}"
        );
    }
}
///
/// `match` 的 **spec §14.2 主流形态**（`with … end`）必须可折叠。
///
/// D136 一度把 `match` 记成「根因未定位的缺口」，D137 查明**那是探针的错**：
/// 我测的是**跨行 arm**（`match v` ⏎ `  1 -> print(1)` ⏎ …），而 `match` 的
/// arm 体**不能跨行** —— 实测报 `Expected '{' or 'with' after match subject`，
/// 解析失败 → `parsed_doc_v3` 返回 `None` → 折叠 `[]`。
///
/// 这条判据的作用是**把主流形态钉住**，让后来者不必再重新推导。
#[test]
fn d137_match_with_form_is_foldable() {
    let res = folding(
        "match_with",
        "let v = 1\nmatch v with\n  1 -> print(1)\n  _ -> print(2)\nend\n",
    );
    assert!(
        res.contains("\"startLine\""),
        "spec 的 `match v with … end` 形态必须产生折叠范围; 实际: {res}"
    );
}
