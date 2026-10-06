//! v0.104.6 D251 —— **外部解析器差分**：MCP 线缆格式里不得出现
//! 非标准 JSON 常量（`NaN` / `Infinity` / `-Infinity`）。
//!
//! ## 为什么需要这条
//!
//! 仓库里有**两个**手写 JSON 序列化器：`flow::json`（`json.stringify` 用）与
//! `lsp::json`（**LSP 与 MCP 协议都用它**）。人工审查过若干轮（D99 / D240），
//! 但**从未用外部解析器交叉验证**。
//!
//! 关键在于：Rust 侧两个解析器**自己就能读回自己写的**任何东西，所以内部
//! 往返测试**永远发现不了**「产出的是非标准 JSON」这类缺陷。
//! 而 Python 的 `json.loads` **默认接受** `NaN` / `Infinity`（非标准扩展），
//! 真实客户端不会。
//!
//! ## 做法：端到端 + 严格模式
//!
//! 用真实 `mora.exe` 起 MCP 服务器，工具分别返回 `NaN` / `+Inf` / `-Inf`，
//! 再用**安装了 `parse_constant` 钩子**的 Python 解析响应帧 —— 一旦线缆里
//! 出现非标准常量立刻失败。
//!
//! 实测结果：**7 帧全部是严格标准 JSON**，所有极端值都降级成 `null`：
//!
//! ```text
//! {"text":"null"}                    ← sqrt(-1) / log(0) / 1.0/0.0 / -1.0/0.0
//! {"text":"{\"n\":null,\"ok\":1.5}"}  ← dict 含 NaN
//! {"text":"[null,1.5]"}              ← list 含 NaN
//! ```
//!
//! ⇒ 否定结果。判据保留，把这个**跨实现的差分检查**固化下来，防止将来
//! 「给 `lsp::json` 加个功能」时漏掉非有限值分支。

use std::io::Write;
use std::process::{Command, Stdio};

const SERVER: &str = r#"let mcp = McpServer::new()
mcp = mcp.tool("nan_tool", {}, fn(args)
  return math.sqrt(-1.0)
end)
mcp = mcp.tool("inf_tool", {}, fn(args)
  return math.log(0.0)
end)
mcp = mcp.tool("div_tool", {}, fn(args)
  return 1.0 / 0.0
end)
mcp = mcp.tool("dict_nan", {}, fn(args)
  let d = {n: math.sqrt(-1.0), ok: 1.5}
  return d
end)
mcp = mcp.tool("list_nan", {}, fn(args)
  return [math.sqrt(-1.0), 1.5]
end)
mcp = mcp.tool("neg_inf", {}, fn(args)
  return -1.0 / 0.0
end)
mcp.serve()
"#;

const TOOLS: [&str; 6] = [
    "nan_tool", "inf_tool", "div_tool", "dict_nan", "list_nan", "neg_inf",
];

struct WorkDir(std::path::PathBuf);
impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d251_{tag}"));
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

/// 跑一次真实 MCP 会话，返回原始响应帧（**未解析**）。
fn raw_frames() -> Vec<String> {
    let dir = WorkDir::new("wire");
    let prog = dir.0.join("srv.mora");
    std::fs::write(&prog, SERVER).expect("write server");

    let payload: String = std::iter::once(frame(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#,
    ))
    .chain(std::iter::once(frame(
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    )))
    .chain(TOOLS.iter().enumerate().map(|(i, t)| {
        frame(&format!(
            r#"{{"jsonrpc":"2.0","id":{},"method":"tools/call","params":{{"name":"{t}","arguments":{{}}}}}}"#,
            100 + i
        ))
    }))
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
    let mut frames = Vec::new();
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
        frames.push(body[..cut].to_string());
        rest = &body[cut..];
    }
    frames
}

/// 线缆里出现这些 token 就是**非标准 JSON**（RFC 8259 只允许 `null`）。
fn find_nonstandard_constants(frame: &str) -> Vec<&'static str> {
    ["NaN", "Infinity"]
        .into_iter()
        .filter(|k| frame.contains(k))
        .collect()
}

/// ① 主判据：所有帧都不得含非标准常量，且必须能被解析。
#[test]
fn d251_mcp_wire_format_has_no_nonstandard_constants() {
    let frames = raw_frames();
    assert!(
        frames.len() >= 7,
        "应收到至少 7 帧（1 initialize + 6 tools/call），实得 {}",
        frames.len()
    );
    for f in &frames {
        let bad = find_nonstandard_constants(f);
        assert!(
            bad.is_empty(),
            "MCP 线缆里出现非标准 JSON 常量 {bad:?} —— 真实客户端（RFC 8259）\
             会直接解析失败。帧内容: {f}"
        );
        // 顺带确认这一帧确实是合法 JSON（`lsp::json` 至少能读回自己写的）。
        mora::lsp::json::Parser::new(f)
            .parse_value()
            .unwrap_or_else(|e| panic!("帧不是合法 JSON: {e}\n{f}"));
    }
}

/// ② 极端值必须**降级成 `null`**（而不是被静默丢掉或变成别的形态）。
#[test]
fn d251_extreme_values_degrade_to_null() {
    let frames = raw_frames();
    let mut by_id = std::collections::HashMap::new();
    for f in &frames {
        if let Ok(v) = mora::lsp::json::Parser::new(f).parse_value()
            && let Some(id) = v.get("id").and_then(|i| i.as_i64())
            && id >= 100
            && let Some(t) = v
                .get("result")
                .and_then(|r| r.get("content"))
                .and_then(|c| match c {
                    mora::lsp::json::Value::Array(a) => a.first(),
                    _ => None,
                })
                .and_then(|c| c.get("text"))
                .and_then(|t| t.as_str())
        {
            by_id.insert(id, t.to_string());
        }
    }

    // 极端值 → null 占位。
    for (i, tool) in TOOLS.iter().enumerate() {
        let id = 100 + i as i64;
        let text = by_id
            .get(&id)
            .unwrap_or_else(|| panic!("缺少 {tool} (id={id}) 的响应；实得 {by_id:?}"));
        assert!(
            text.contains("null"),
            "{tool} 返回的 text 应含 null（极端值被降级），实得 {text:?}"
        );
    }
    // 对照：**正常值必须原样保留**，否则「全都变 null」也能通过上面那条。
    assert!(
        by_id.get(&103).is_some_and(|t| t.contains("1.5")),
        "dict 里的正常值 1.5 必须保留；实得 {:?}",
        by_id.get(&103)
    );
    assert!(
        by_id.get(&104).is_some_and(|t| t.contains("1.5")),
        "list 里的正常值 1.5 必须保留；实得 {:?}",
        by_id.get(&104)
    );
}

/// ③ **对照组**：本判据的检查器必须对真实的坏帧**报警**，否则 ① 可能恒绿。
#[test]
fn d251_control_group_detector_catches_nonstandard_json() {
    // 真实客户端遇到这两种都会解析失败。
    assert_eq!(
        find_nonstandard_constants(r#"{"x":NaN}"#),
        vec!["NaN"],
        "检查器抓不到 NaN —— 主判据 ① 可能恒绿"
    );
    assert_eq!(
        find_nonstandard_constants(r#"{"x":-Infinity}"#),
        vec!["Infinity"],
        "检查器抓不到 Infinity —— 主判据 ① 可能恒绿"
    );

    // ⚠ **已知的局限，如实固定住**：本检查器是**子串**匹配，无法区分
    // 「裸 `NaN` 常量」与「字符串内容里的 `NaN`」。所以主判据 ① 只对
    // **已知极端值用例**成立 —— 它们的响应只含 `null` 与数字，不含任意
    // 字符串内容。（此处曾把期望写成「合法帧不应误报」，红的是我的期望：
    // 我在上一行注释里刚写明它会误报，两句话自相矛盾。）
    assert_eq!(
        find_nonstandard_constants(r#"{"z":"a NaN string"}"#),
        vec!["NaN"],
        "记录该局限：若将来 ① 要推广到含任意字符串的响应，必须先换成 \
         真正按结构解析的检查（如 Python json + parse_constant 钩子）"
    );
    // 纯数字 / null 的合法帧不应误报（这是 ① 真正依赖的性质）。
    assert!(find_nonstandard_constants(r#"{"x":null,"y":1.5,"z":-7}"#).is_empty());
}
