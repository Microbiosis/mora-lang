//! v0.104.6 D247 —— 一次「我差点证明了一个不存在的缺陷」的**否定结果**归档。
//!
//! ## 我原本要报什么
//!
//! `mcp_server.rs::mora_to_json` 只匹配 `Value::Float`、**没有 `Value::Int`
//! 分支**（对比 `http_server.rs::value_to_json` 是有的），于是推断「所有整数
//! 都被兜底编成 JSON 字符串，客户端拿到的类型错了」—— 这与 D245
//! （`server.rs::pos_of` 有守卫、三个 provider 没有）形状完全相同。
//!
//! ## 实测证明它不成立
//!
//! 真实 MCP stdio 会话：
//!
//! ```text
//! tools/call int_tool   → {"content":[{"text":"42",   "type":"text"}]}
//! tools/call float_tool → {"content":[{"text":"42.5", "type":"text"}]}
//! ```
//!
//! 原因在 `mcp_server.rs:437-454`：`result_json` 的两个分支**产出同构**的
//! `{type:"text", text:<字符串>}`，只是 `text` 来源不同。所以 `Int` 被兜底成
//! `String_` 与走 `Number` 分支，输出**逐字节相同**。
//!
//! ## 我自己又犯了一次「先推理、后取证」
//!
//! 第一版判据的理由是「补上 `Int` 分支会让 `42` 变成 `"42.0"`」。我把它
//! 当成「显然的破坏」去验牙齿 —— **结果判据仍然全绿**。交叉验证排除了装置
//! 问题（`mora.exe` 的时间戳确实比源文件新，二进制被重建了），于是查了
//! 真正的序列化实现，发现：
//!
//! ```text
//! lsp/json.rs:83   Number(42.0) → "42"     ← 整数按 i64 输出，不补小数点
//! flow/json.rs:387 Value::Float(42.0) → "42.0"  ← D99 刻意补小数点
//! ```
//!
//! ⇒ 补 `Int` 分支确实**无害**，我的理由是错的。**本条判据因此重写**：
//! 主判据改钉真正脆弱的地方（`lsp::json` 的整数格式化），端到端那条
//! 降级为**行为快照**并如实标注它不是护栏。
//!
//! ⇒ 教训：**注释与形状像缺陷，不等于它就是缺陷**。必须先看下游怎么消费，
//! 再决定修不修；写下护栏后必须真的验一次牙齿，`绿` 有时意味着「你要防的
//! 东西根本不存在」。

use std::io::Write;
use std::process::{Command, Stdio};

/// ① **主判据（有牙齿）**：`lsp::json` 序列化整数**不得**补小数点。
///
/// LSP 协议规定 `line` / `character` 是 `uinteger`；输出 `"42.0"` 会让严格
/// 客户端解析失败。MCP 也复用 `lsp::json`（`json_to_string`），影响面更大。
///
/// 真实脆弱点：若有人为了「和 `flow::json` 对齐」把这里也改成补小数点
/// （D99 在 `flow::json` 做过这个修复），本条立即变红。
#[test]
fn d247_lsp_json_integer_has_no_decimal_point() {
    use mora::lsp::json::{Value as J, to_string};

    assert_eq!(
        to_string(&J::Number(42.0)),
        "42",
        "lsp::json 把 42.0 输出成了带小数点的形式 —— LSP 的 line/character 是\
         uinteger，严格客户端会拒绝"
    );
    assert_eq!(to_string(&J::Number(0.0)), "0");
    assert_eq!(to_string(&J::Number(-7.0)), "-7");
    // 真正的非整数必须保留小数点。
    assert_eq!(to_string(&J::Number(42.5)), "42.5");
    assert_eq!(to_string(&J::Number(-0.25)), "-0.25");
}

/// ② 对照组：`flow::json` 对 `Float` **刻意**补小数点，与 ① 的行为**不同**。
///
/// 钉住这个差异是**设计决定**（D99：往返后不能丢类型），不是疏忽。
/// 两条判据一起把「两个 JSON 实现为何不同」固定下来。
#[test]
fn d247_control_group_flow_json_float_keeps_decimal_point() {
    use mora::flow::value_to_json;
    use mora::value::Value;

    assert_eq!(
        value_to_json(&Value::Float(42.0)),
        "42.0",
        "flow::json 对 Float 必须补小数点（D99：否则读回变 Int，类型降级不可逆）"
    );
    // 对照：Int 仍是裸整数。
    assert_eq!(value_to_json(&Value::Int(42)), "42");
    // 于是往返对称。
    assert_eq!(
        mora::flow::json_to_value("42.0").expect("parse"),
        Value::Float(42.0)
    );
    assert_eq!(
        mora::flow::json_to_value("42").expect("parse"),
        Value::Int(42)
    );
}

// ── 以下是端到端行为快照 ──────────────────────────────────────────────
// ⚠ 牙齿边界（实测，别照抄本注释就当它是护栏）：
//   对「`lsp::json` 被改成补小数点」这个破坏：**有牙齿**（验过，会红）。
//   对「给 `mora_to_json` 补 `Value::Int` 分支」这个破坏：**没有**（验过，仍绿）。
//   真正的护栏是 ①。

const SERVER: &str = r#"let mcp = McpServer::new()
mcp = mcp.tool("int_tool", {}, fn(args)
  return 42
end)
mcp = mcp.tool("float_tool", {}, fn(args)
  return 42.5
end)
mcp = mcp.tool("str_tool", {}, fn(args)
  return "hi"
end)
mcp = mcp.tool("list_tool", {}, fn(args)
  return [1, 2, 3]
end)
mcp.serve()
"#;

struct WorkDir(std::path::PathBuf);
impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d247_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mkdir");
        WorkDir(d)
    }
}
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn frame(body: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{}", body.len(), body)
}

fn call_all() -> Vec<(String, String)> {
    let dir = WorkDir::new("numeric");
    let prog = dir.0.join("srv.mora");
    std::fs::write(&prog, SERVER).expect("write server");

    let payload: String = std::iter::once(frame(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#,
    ))
    .chain(std::iter::once(frame(
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    )))
    .chain(
        ["int_tool", "float_tool", "str_tool", "list_tool"]
            .iter()
            .enumerate()
            .map(|(i, name)| {
                frame(&format!(
                    r#"{{"jsonrpc":"2.0","id":{},"method":"tools/call","params":{{"name":"{name}","arguments":{{}}}}}}"#,
                    i + 10
                ))
            }),
    )
    .collect();

    let mut child = Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/target/debug/mora.exe"
    ))
    .arg("run")
    .arg(&prog)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("spawn mora");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    assert!(
        out.status.success(),
        "服务器应正常退出:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let raw = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut rest = raw.as_str();
    let mut found = Vec::new();
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
            .unwrap_or_else(|e| panic!("帧不是合法 JSON: {e}\n{}", &body[..cut]));
        if let Some(item) = v
            .get("result")
            .and_then(|r| r.get("content"))
            .and_then(|c| match c {
                mora::lsp::json::Value::Array(a) => a.first(),
                _ => None,
            })
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str())
        {
            let name = match v.get("id").and_then(|i| i.as_i64()) {
                Some(10) => "int_tool",
                Some(11) => "float_tool",
                Some(12) => "str_tool",
                Some(13) => "list_tool",
                _ => "init",
            };
            found.push((name.to_string(), item.to_string()));
        }
        rest = &body[cut..];
    }
    found
}

/// ③ 行为快照 + **半承重**护栏：MCP 工具结果的实测形态。
///
/// 牙齿验证实测（把 `lsp::json` 改成补小数点）：
/// - 本条 **FAILED** ⇒ 它对「`lsp::json` 补小数点」这个破坏**确实有牙齿**；
/// - 但它对**我最初声称的**那个破坏（给 `mora_to_json` 补 `Value::Int` 分支）
///   **没有**牙齿 —— 那次验证时它仍然全绿。
///
/// ⇒ 「有牙齿」不是一个布尔值，要说清**对哪个破坏有**。
///
/// 它的另一层作用：若将来有人把 content 改成 `structuredContent`
/// （MCP 规范推荐），本条会立刻暴露形态变化，那时才需要重新判断
/// 「`Int` 走兜底」是缺陷还是无害。
#[test]
fn d247_mcp_tool_result_text_shapes_snapshot() {
    let got = call_all();
    let get = |n: &str| {
        got.iter()
            .find(|(name, _)| name == n)
            .map(|(_, t)| t.clone())
            .unwrap_or_else(|| panic!("缺少 {n} 的响应；实得 {got:?}"))
    };
    assert_eq!(get("int_tool"), "42");
    assert_eq!(get("float_tool"), "42.5");
    assert_eq!(get("str_tool"), "hi");
    assert_eq!(get("list_tool"), "[1,2,3]");
}
