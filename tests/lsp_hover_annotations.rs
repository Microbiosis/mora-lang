//! v0.104.6 D193 / Wave 2：LSP hover 对**显式标注**和 **HM 推断类型**的真实响应。
//!
//! - 有显式标注（如 `let a: Int = 5`）时保留源码标注；
//! - 无标注（如 `let b = 7.5`、`fn add(x: Int, y: Int) -> Int`）时经统一 HM
//!   (`typeck::check_mir::infer_program_types`) 报出真实推断类型；
//! - 无法解决或存在错误时诚实回落 `<inferred>`，绝不猜测；
//! - 名字严格整词匹配，同名 shadowing 按作用域就近解析。

use mora::test_support::TempDir;
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

fn temp_dir() -> TempDir {
    TempDir::new("mora_d193_lsp")
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

/// 无标注变量/函数由 HM 给出真实推断类型；无法解决的标识符诚实回落 `<inferred>`。
#[test]
fn d193_hover_infers_unannotated_bindings_and_honestly_falls_back() {
    let text = "let b = 7.5\nprint(b)\ntask add(x: Int, y: Int)\n  return x + y\nend\n";
    let got = hovers("noanno", text, &[(0, 4), (2, 5)]);
    let v_b = value_of(&got, 100);
    assert!(
        v_b.contains("let b: float"),
        "HM 应为 `let b = 7.5` 推断出 float:\n{}",
        v_b
    );
    let v_add = value_of(&got, 101);
    assert!(
        v_add.contains("fn add: fn (int) -> fn (int) -> int"),
        "HM 应为无显式返回标注的 `task add` 推断出真实签名:\n{}",
        v_add
    );

    let err_text = "let broken = missing_var\n";
    let err_got = hovers("unresolved", err_text, &[(0, 4), (0, 15)]);
    assert!(
        value_of(&err_got, 100).contains("<inferred>"),
        "类型错误表达式的绑定必须回落 `<inferred>`:\n{}",
        value_of(&err_got, 100)
    );
    assert!(
        value_of(&err_got, 101).contains("<inferred>"),
        "未定义符号必须诚实回落 `<inferred>`:\n{}",
        value_of(&err_got, 101)
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
        v.contains("let a: int") && !v.contains("String"),
        "`a` 应按 HM 推断为 int，绝不得被同文件里 `ax: String` 的标注张冠李戴:\n{}",
        v
    );
}
