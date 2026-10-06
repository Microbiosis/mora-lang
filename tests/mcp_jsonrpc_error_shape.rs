//! v0.104.6 D213：MCP 的**每一条错误路径**都被包进 `result` —— 客户端把
//! **失败当成成功**（已修）。
//!
//! ## 缺陷
//!
//! `mcp_server.rs::dispatch` 只有一个 `Option<JsonValue>` 通道，
//! **错误与结果载荷挤在一起**，于是所有错误都被 `wrap_response` 裹成
//! `{"jsonrpc","id","result": <错误>}`。
//!
//! JSON-RPC 2.0 规定 `error` 必须与 `result` **同级**（且互斥）。
//! 客户端找顶层 `error` 找不到 ⇒ **把失败当成成功返回**。
//!
//! 真实 MCP stdio 会话实测（修前，4 条错误路径**全中**）：
//!
//! ```text
//! 未知 method (resources/list) → result = {"error":{…},"id":4,"jsonrpc":"2.0"}
//! 未知 method (ping)            → result = {"error":{…},"id":7,"jsonrpc":"2.0"}
//! tools/call 未知工具名          → result = {"code":-32602,"message":"Unknown tool: …"}
//! tools/call 缺 name            → result = {"code":-32602,"message":"Missing 'name' in params"}
//! ```
//!
//! 两种形状还**互不一致**：前者嵌的是**整份应答**（连 `jsonrpc` / `id` 都
//! 重复了一遍），后者只回 `{code, message}`。`resources/list` / `ping` /
//! `prompts/list` 都是标准 MCP 方法而 Mora 未实现 —— 客户端**永远学不到**
//! 「这个服务器不支持它」。
//!
//! ## 修法：用**类型**把两者分开
//!
//! ```rust
//! enum Reply { Result(JsonValue), Error(i64, String) }
//! ```
//!
//! 写回处按变体分派：`Reply::Error` → `wrap_error`（顶层 `error`），
//! `Reply::Result` → `wrap_response`（顶层 `result`）。二者互斥由类型保证，
//! 不再可能像修前那样在同一个通道里混。
//!
//! ## 判据
//!
//! 判据直接写成**协议要求**（`error` 在顶层、与 `result` 互斥、code 正确），
//! 而不是断言某个具体字符串 —— 后者会随实现变化而失效。
//!
//! 另有正常路径的形状校验（`initialize` 三字段、`tools/list` 每项必须有
//! `name` + `inputSchema`、`tools/call` 的 `content` 必须是数组）作为**不回归**。

use std::io::Write;
use std::process::{Command, Stdio};

/// 一个**真实**的 MCP 服务器脚本：注册两个工具并阻塞在 stdio 上。
///
/// ⚠ `mcp.serve()` 必须**当语句调用** —— 它返回 `Nil`，
/// 写成 `mcp = mcp.serve()` 会因类型不匹配报错（我第一版就栽在这）。
const SERVER: &str = r#"let mcp = McpServer::new()
mcp = mcp.tool("search", {query: "string", max_results: "number"}, fn(args)
  let q = args["query"]
  return "found for: " + q
end)
mcp = mcp.tool("calculate", {expression: "string"}, fn(args)
  return 42
end)
mcp.serve()
"#;

struct WorkDir(std::path::PathBuf);
impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d213_{tag}"));
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

fn frame(body: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{}", body.len(), body)
}

/// 跑一次完整握手，返回 `(id, response JSON)` 列表。
fn handshake(tag: &str, msgs: &[String]) -> Vec<mora::lsp::json::Value> {
    let dir = WorkDir::new(tag);
    let prog = dir.0.join("srv.mora");
    std::fs::write(&prog, SERVER).expect("写服务器脚本");
    let payload: String = std::iter::once(frame(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#,
    ))
    .chain(std::iter::once(frame(
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    )))
    .chain(msgs.iter().map(|m| frame(m)))
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
    .expect("起 mora");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("写 stdin");
    let out = child.wait_with_output().expect("等 mora");
    assert!(
        out.status.success(),
        "[{tag}] 服务器应正常退出:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let raw = String::from_utf8_lossy(&out.stdout).into_owned();

    // 按 Content-Length 逐帧切出
    let mut rest = raw.as_str();
    let mut bodies = Vec::new();
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
            .unwrap_or_else(|e| panic!("[{tag}] 帧不是合法 JSON: {e}\n{}", &body[..cut]));
        bodies.push(v);
        rest = &body[cut..];
    }
    bodies
}

/// 协议要求：`error` 必须在**顶层**，且与 `result` **互斥**。
fn assert_toplevel_error(tag: &str, id: i64, bodies: &[mora::lsp::json::Value], want_code: i64) {
    let o = bodies
        .iter()
        .find(|v| v.get("id").and_then(|i| i.as_i64()) == Some(id))
        .unwrap_or_else(|| panic!("[{tag}] 没有收到 id={id} 的应答"));
    assert_eq!(
        o.get("jsonrpc").and_then(|v| v.as_str()),
        Some("2.0"),
        "[{tag}] id={id} 的 jsonrpc 字段不对: {o:?}"
    );
    assert!(
        !o.get("result").is_some(),
        "[{tag}] id={id} 竟带 `result` —— 错误被包进 result 后，客户端找不到顶层 \
         `error`，于是**把失败当成成功**。实际: {o:?}"
    );
    let e = o
        .get("error")
        .unwrap_or_else(|| panic!("[{tag}] id={id} 缺顶层 `error`: {o:?}"));
    assert_eq!(
        e.get("code").and_then(|c| c.as_i64()),
        Some(want_code),
        "[{tag}] id={id} 的 error.code 应为 {want_code}: {o:?}"
    );
    assert!(
        e.get("message")
            .and_then(|m| m.as_str())
            .is_some_and(|m| !m.is_empty()),
        "[{tag}] id={id} 的 error.message 为空: {o:?}"
    );
}

/// **主判据（有牙齿）**：4 条错误路径都必须返回**顶层** `error`。
///
/// 修前这 4 条全部把错误塞进 `result`。
#[test]
fn d213_every_error_path_returns_a_toplevel_jsonrpc_error() {
    let _d = WorkDir::new("err");
    let bodies = handshake(
        "err",
        &[
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#.to_string(),
            r#"{"jsonrpc":"2.0","id":4,"method":"resources/list"}"#.to_string(),
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"no_such_tool","arguments":{}}}"#.to_string(),
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{}}"#.to_string(),
            r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#.to_string(),
        ],
    );
    assert_toplevel_error("resources/list", 4, &bodies, -32601);
    assert_toplevel_error("ping", 7, &bodies, -32601);
    assert_toplevel_error("tools/call 未知工具", 5, &bodies, -32602);
    assert_toplevel_error("tools/call 缺 name", 6, &bodies, -32602);
}

/// **不回归**：正常路径的形状必须仍符合 MCP 规范。
#[test]
fn d213_happy_path_shapes_stay_conformant() {
    let _d = WorkDir::new("ok");
    let bodies = handshake(
        "ok",
        &[
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#.to_string(),
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search","arguments":{"query":"abc"}}}"#.to_string(),
        ],
    );
    let get = |id: i64| -> mora::lsp::json::Value {
        bodies
            .iter()
            .find(|v| v.get("id").and_then(|i| i.as_i64()) == Some(id))
            .cloned()
            .unwrap_or_else(|| panic!("缺 id={id} 的应答"))
    };

    // initialize
    let init = get(1);
    assert!(init.get("error").is_none(), "initialize 不应报错: {init:?}");
    let r = init.get("result").expect("initialize 应有 result");
    for k in ["protocolVersion", "capabilities", "serverInfo"] {
        assert!(
            r.get(k).is_some(),
            "initialize 的 result 缺 `{k}`（MCP 规范要求）: {r:?}"
        );
    }

    // tools/list
    let tools = get(2);
    let arr = tools
        .get("result")
        .and_then(|x| x.get("tools"))
        .and_then(|x| x.as_array())
        .unwrap_or_else(|| panic!("tools/list 的 result.tools 应是数组: {tools:?}"));
    assert!(!arr.is_empty(), "注册了 2 个工具，tools/list 不应为空");
    for t in arr {
        assert!(t.get("name").is_some(), "tool 缺 name: {t:?}");
        assert!(
            t.get("inputSchema").and_then(|s| s.as_object()).is_some(),
            "tool `{:?}` 的 inputSchema 应是 JSON Schema 对象（MCP 规范要求）: {t:?}",
            t.get("name").and_then(|n| n.as_str()).unwrap_or("?")
        );
    }

    // tools/call
    let call = get(3);
    let content = call
        .get("result")
        .and_then(|x| x.get("content"))
        .and_then(|x| x.as_array())
        .unwrap_or_else(|| {
            panic!("tools/call 的 result.content 应是数组（MCP 规范要求）: {call:?}")
        });
    assert_eq!(content.len(), 1);
    let c = &content[0];
    assert_eq!(
        c.get("type").and_then(|t| t.as_str()),
        Some("text"),
        "content item 的 type 应为 text: {c:?}"
    );
    assert!(
        c.get("text")
            .and_then(|t| t.as_str())
            .is_some_and(|t| t.contains("abc")),
        "content.text 应含 handler 的返回值: {c:?}"
    );
}

/// notification（无 id）**不得**产生任何应答。
#[test]
fn d213_notifications_produce_no_response() {
    let _d = WorkDir::new("note");
    let bodies = handshake(
        "note",
        &[r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#.to_string()],
    );
    // 本文件固定前置 initialize(id=1) + initialized(notification)。
    // 若 notification 也被回包，帧数会多于 2。
    assert_eq!(
        bodies.len(),
        2,
        "应只有 initialize 与 tools/list 两条应答（notification 不得有回包），\
         实得 {}: {bodies:?}",
        bodies.len()
    );
}
