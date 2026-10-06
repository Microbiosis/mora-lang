//! v0.104.6 D132：LSP 位置**基���错一半** + `rename` 会**改坏用户的代码**（已修）
//!
//! ## 缺陷（三层，从轻到重）
//!
//! ① **行号基数错**：LSP 规范要求 `line` / `character` **都是 0-based**，而
//!    `Span::line` 是 **1-based**（`column` 本来就是 0-based）。`definition` /
//!    `references` / `symbols` / `rename` 四个 provider **全部直接透传**，
//!    「跳转到定义」整体偏移一行，编辑器把光标落到**下一行**。
//!    `server.rs` 的诊断路径一直有 `saturating_sub(1)` —— 只有这四个漏了。
//!
//! ② **每个结果重复两遍**：`collect_definitions_in_expr` 先用
//!    `match expr.kind` 记一次**根节点**，紧接着 `walk_witness(expr, …)`
//!    **又遍历根节点**。实测 `definition` 返回两条完全相同的 location。
//!
//! ③ **重命名改坏代码**（最严重）：span 的起点是**整个 let 语句 / 表达式**
//!    的起点（`let x = 1` 落在 `l`，column 0），而 `rename` 用
//!    `column + name.len()` 算 end —— 把它当成标识符位置。实测（真实 LSP 会话
//!    + **真的把 edits 应用到源码**）：
//!
//!    ```text
//!    let x = 1      →   lzt x = 1        ← `let` 的 `e` 被改成 `z`
//!    let y = x + 1  →   let y = xz+ 1
//!    ```
//!
//!    `mora --check` 报 3 个类型错误 —— **用户按一次 F2，代码就毁了**。
//!
//! ## 修法
//!
//! ①② 各改一处（4 个 provider 的 `line` 加 `saturating_sub(1)`；
//! `collect_definitions_in_expr` 删掉重复的那次 match）。
//! ③ `rename` 新增 `locate` 闭包：从 `span.column` 起**双向就近**搜索
//!    `old_name` 的**完整词**（前后不是标识符字符），取实际位置；两头都找不到
//!    就**放弃这一处**（宁可不改，也不要改错字符）。
//!
//! ## 为什么 ③ 必须**真的**把 edits 应用到源码
//!
//! 只断言「返回的 range 等于预期值」会漏掉「预期值本身就是错的」这类情况。
//! 本文件的核心判据是**把 LSP 返回的 edits 施加到源码，再把产物丢给
//! `mora --check`** —— 判据是「产物仍可解析」，与 rename 的实现细节无关。

use std::path::PathBuf;
use std::process::Command;

fn temp_dir() -> PathBuf {
    let d = std::env::temp_dir().join("mora_d132_apply");
    std::fs::create_dir_all(&d).expect("create temp dir");
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

/// 发一次 LSP 请求并返回 `id:2` 的响应体。
///
/// ⚠ `tag` **必须**每个测试各不相同：本文件三个测试若共用同一个输入文件，
/// `cargo test` 的并发执行会互相覆盖 `in.bin`，表现为「单跑绿、一起跑红」。
/// （隔离粒度必须匹配被测系统的状态粒度 —— 这里的状态是那个临时文件。）
fn lsp_request(tag: &str, src: &str, method: &str, params: &str) -> String {
    let dir = temp_dir();
    let input = dir.join(format!("{tag}.bin"));
    let uri = format!("file:///tmp/{tag}.mora");
    let params = params.replace("URI_PLACEHOLDER", &uri);
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
            r#"{{"jsonrpc":"2.0","id":2,"method":"{method}","params":{params}}}"#
        )),
        frame(r#"{"jsonrpc":"2.0","id":99,"method":"shutdown","params":null}"#),
    ]
    .concat();
    std::fs::write(&input, msgs).expect("write input");
    let out = Command::new(env!("CARGO_BIN_EXE_mora-lsp"))
        .stdin(std::process::Stdio::from(
            std::fs::File::open(&input).expect("open"),
        ))
        .output()
        .expect("run mora-lsp");
    let _ = std::fs::remove_file(&input);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    split_frames(&stdout)
        .into_iter()
        .find(|f| f.contains("\"id\":2"))
        .unwrap_or_default()
}

/// 从 `s` 的第 `from` 字节处起取第一段连续数字，返回 `(值, 数字之后的偏移)`。
fn num_at(s: &str, from: usize) -> Option<(usize, usize)> {
    let i = from + s.get(from..)?.find(|c: char| c.is_ascii_digit())?;
    let n: String = s[i..].chars().take_while(|c| c.is_ascii_digit()).collect();
    Some((n.parse().ok()?, i + n.chars().count()))
}

/// 把 rename 的 edits 施加到源码上。
fn apply_edits(src: &str, result_json: &str) -> String {
    let mut lines: Vec<String> = src.lines().map(|s| s.to_string()).collect();
    // 每个 edit：`"newText":"<txt>","range":{"end":{"character":E,"line":EL},"start":{"character":S,"line":SL}}`
    let mut edits: Vec<(usize, usize, usize, usize, String)> = Vec::new();
    let mut at = 0usize;
    while let Some(p) = result_json.get(at..).and_then(|s| s.find("\"newText\":\"")) {
        let txt_at = at + p + "\"newText\":\"".len();
        let txt_end = txt_at
            + result_json[txt_at..]
                .find('"')
                .expect("newText 应以引号结束");
        let new_text = result_json[txt_at..txt_end].to_string();
        // 之后依次是 end.character、end.line、start.character、start.line
        // （BTreeMap 按键排序，`character` < `line`）
        let (e_ch, a) = num_at(result_json, txt_end).expect("end.character");
        let (e_line, b) = num_at(result_json, a).expect("end.line");
        let (s_ch, c) = num_at(result_json, b).expect("start.character");
        let (s_line, d) = num_at(result_json, c).expect("start.line");
        edits.push((e_line, e_ch, s_line, s_ch, new_text));
        at = d;
    }
    // 逆序应用
    for (el, ec, sl, sc, nt) in edits.into_iter().rev() {
        if el != sl {
            continue;
        }
        let chars: Vec<char> = lines[sl].chars().collect();
        let start = sc.min(chars.len());
        let end = ec.min(chars.len());
        if start > end {
            continue;
        }
        let mut new: String = chars[..start].iter().collect();
        new.push_str(&nt);
        new.extend(chars[end..].iter());
        lines[sl] = new;
    }
    lines.join("\n")
}

fn parses_ok(src: &str) -> (bool, String) {
    let f = temp_dir().join("applied.mora");
    std::fs::write(&f, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg("--check")
        .arg(&f)
        .output()
        .expect("run mora --check");
    let _ = std::fs::remove_file(&f);
    let code = out.status.code().unwrap_or(-1);
    let last = String::from_utf8_lossy(&out.stderr)
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or("")
        .to_string();
    (code == 0, last)
}

const SRC: &str = "let x = 1\nlet y = x + 1\nprint(y)\n";

#[test]
fn d132_rename_edits_keep_the_source_parsable() {
    let res = lsp_request(
        "ren",
        SRC,
        "textDocument/rename",
        r#"{"textDocument":{"uri":"URI_PLACEHOLDER"},"position":{"line":1,"character":8},"newName":"z"}"#,
    );
    eprintln!("rename 响应: {res}");
    let applied = apply_edits(SRC, &res);
    eprintln!("--- 原源码\n{SRC}--- 应用 rename 后\n{applied}");
    let (ok, msg) = parses_ok(&applied);
    eprintln!("mora --check → ok={ok}  {msg}");
    assert!(ok, "应用 rename 后源码应仍可解析，实际产物:\n{applied}");

    // 精确判据：定义处与使用处**都要**改成新名字（`let z = 1` / `let y = z + 1`）。
    // 修复前只改对了一半（定义处对、使用处漏）——「产物可解析」这一条抓不到
    // 「漏改」，因为漏改的代码仍然合法，只是引用了旧名字。
    assert!(
        applied.contains("let z = 1"),
        "定义处应被改名，实际产物:\n{applied}"
    );
    assert!(
        applied.contains("let y = z + 1"),
        "使用处也应被改名（修复前漏改），实际产物:\n{applied}"
    );
}

/// D132 判据 ②：`definition` **不得**把同一条结果返回两遍。
#[test]
fn d132_definition_is_not_duplicated() {
    let res = lsp_request(
        "defn_dup",
        SRC,
        "textDocument/definition",
        r#"{"textDocument":{"uri":"URI_PLACEHOLDER"},"position":{"line":1,"character":8}}"#,
    );
    eprintln!("definition 响应: {res}");
    let n = res.matches("\"uri\"").count();
    assert_eq!(
        n, 1,
        "光标在唯一的定义上，`definition` 应返回**1** 条；返回 {n} 条说明 \
         `collect_definitions_in_expr` 把根节点记了两遍（D132）:\n{res}"
    );
}

/// D134 续：`documentSymbol` 的 range 同样要落在**符号**上。
///
/// 它与 `references` 同型（直接透传 1-based 的 `span.column`），
/// 但定位条件更精确 —— 这里的 `name` **就是**标识符本身，无需再从光标反推。
#[test]
fn d134_document_symbol_columns_land_on_the_identifier() {
    let src = "let x = 1\nlet y = x + 1\n";
    let res = lsp_request(
        "syms",
        src,
        "textDocument/documentSymbol",
        r#"{"textDocument":{"uri":"URI_PLACEHOLDER"}}"#,
    );
    eprintln!("documentSymbol 响应: {res}");
    // `x` 在 line0 char4（长度 1 → char4-5），`y` 在 line1 char4
    assert!(
        res.contains("\"character\":4") && res.contains("\"line\":0"),
        "`x` 的 range 应是 line0 char4（符号起点），而非 let 语句起点; 实际:\n{res}"
    );
    assert!(
        res.contains("\"character\":5"),
        "`x` 长度为 1，range 终点应是 char5; 实际:\n{res}"
    );
    // 去重后应恰好 2 个符号（x 与 y 各一个）
    assert_eq!(
        res.matches("\"name\":").count(),
        2,
        "去重后应恰好 2 个符号（x / y）; 实际:\n{res}"
    );
}
/// D134：`references` 的**列号**同样要落在标识符上。
///
/// 追加发现：不仅行号，`column` 也是 **1-based**（实测：源码 0-based char 16 的
/// `abc` 在诊断里报 `column 17`），直接透传会**整体偏移 1 列**。
/// 修复前 `let x = 1` / `let y = x + 1` 的 `references` 返回 `char 1-2` 与
/// `char 9-10`，而 `x` 实际在 `char 4` 与 `char 8`。
#[test]
fn d134_references_columns_land_on_the_identifier() {
    let src = "let x = 1\nlet y = x + 1\n";
    let res = lsp_request(
        "refs",
        src,
        "textDocument/references",
        r#"{"textDocument":{"uri":"URI_PLACEHOLDER"},"position":{"line":0,"character":4},"context":{"includeDeclaration":true}}"#,
    );
    eprintln!("references 响应: {res}");
    // 声明处：`let x = 1` 的 x 在 line0 char4
    assert!(
        res.contains("\"start\":{\"character\":4,\"line\":0}")
            || res.contains("\"line\":0,\"start\":{\"character\":4")
            || (res.contains("\"character\":4") && res.contains("\"line\":0")),
        "声明处的 range 应是 line0 char4（`x` 的位置）; 实际:\n{res}"
    );
    // 使用处：`let y = x + 1` 的 x 在 line1 char8
    assert!(
        res.contains("\"character\":8") && res.contains("\"line\":1"),
        "使用处的 range 应是 line1 char8（`x` 的位置）; 实际:\n{res}"
    );
    // 不得出现修复前的偏移值
    assert!(
        !res.contains("\"character\":9") || !res.contains("\"line\":1}\""),
        "不应返回偏移 1 的列号（char 9）; 实际:\n{res}"
    );
}
/// D132 判据 ①：行号与列号都必须是 **0-based**（LSP 规范）。
///
/// D134 补正：`Span::column` 也是 1-based（实测确证），不是 D132 初稿里
/// 写的「已是 0-based」—— 正确性由 provider 的 `locate` 承担。
#[test]
fn d132_definition_line_is_zero_based() {
    let res = lsp_request(
        "defn_base",
        SRC,
        "textDocument/definition",
        r#"{"textDocument":{"uri":"URI_PLACEHOLDER"},"position":{"line":1,"character":8}}"#,
    );
    // `let x = 1` 在源码第 1 行 → LSP 0-based 下应是 line 0。
    // 修复前透传 1-based 的 `Span::line`，编辑器会跳到下一行。
    assert!(
        res.contains("\"line\":0"),
        "`let x = 1` 在 0-based 下应是 line 0（`Span::line` 是 1-based，\
         provider 必须 `saturating_sub(1)`）; 实际响应:\n{res}"
    );
    assert!(
        !res.contains("\"line\":1"),
        "不应返回 1-based 的行号:\n{res}"
    );

    // 列号同样要落在**标识符**上，而不是 let 语句的起点。
    // `let x = 1` 的 `x` 在 char 4；span 起点是 `l`（char 0）。
    // 修复前返回 char 1-2（`let` 的 `e`），跳转停在错误位置。
    assert!(
        res.contains("\"start\":{\"character\":4"),
        "range 起点应落在标识符 `x`（char 4）上，而非 let 语句起点（char 0）; 实际:\n{res}"
    );
    assert!(
        res.contains("\"end\":{\"character\":5"),
        "range 终点应为 char 5（`x` 长 1）; 实际:\n{res}"
    );
}
