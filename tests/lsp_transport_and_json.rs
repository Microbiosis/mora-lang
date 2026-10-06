//! v0.104.6 D371 —— `src/lsp/` 的 **transport 分帧** 与 **`json.rs` 解析矩阵**
//! （否定轮，无产品变更）
//!
//! `src/lsp/` 共 15 个文件 3500 行，已有 **16 个判据**覆盖 provider 层
//! （折叠 / 跳转 / 重命名 / 语义标记 / 悬停 …），但 **两个基础设施文件**
//! 无直接判据：
//!
//! | 文件 | 行数 | 职责 |
//! |---|---|---|
//! | `transport.rs` | 101 | JSON-RPC over stdio 的**分帧** |
//! | `json.rs` | 501 | **自写的** JSON 解析/序列化（**独立于 `flow::json`**）|
//!
//! ## transport：9 个边界 + 5 个宽松形态，全部正确
//!
//! | 输入 | 实测 | 判定 |
//! |---|---|---|
//! | ASCII / UTF-8 多字节 body | 正确往返 | ✅ |
//! | 空 body（`Content-Length: 0`）| `Some("")` | ✅ |
//! | 缺 `Content-Length` | Err *missing Content-Length* | ✅ |
//! | `Content-Length: abc` | Err *missing Content-Length* | ✅ |
//! | `Content-Length` 大于实际 body | Err *failed to fill whole buffer* | ✅ |
//! | 空 header 块 | Err *missing Content-Length* | ✅ |
//! | body 非法 UTF-8 | Err *invalid utf-8 sequence* | ✅ |
//! | **连续两条消息** | `{"id":1}` 然后 `{"id":2}` | ✅ 状态机正确 |
//! | 非 ASCII header | ✅ | ✅ |
//! | **LF-only** header（无 `\r`）| ✅ | ✅ 宽容 |
//! | 额外 header（`Content-Type`）| ✅ | ✅ |
//! | header 名**大小写混写** | ✅ | ✅ |
//! | `Content-Length:   2  ` 带空格 | ✅ | ✅ `trim()` |
//!
//! ## `lsp::json` 与 `flow::json` 是**两套实现但行为一致**
//!
//! `lsp/json.rs` 有**自己的 `Value` 枚举**（`Number(f64)` —— 数字统一 f64），
//! 与 `flow::json` 的 `Value`（`Int` / `Float` / `BigInt` 三分）**不同**。
//!
//! 但实测 9 个畸形数字形态上**两者行为一致**（见末表）⇒ **不是分叉，
//> 是一致的宽松**。
//!
//! ## 已知能力限制：**JSON 数字统一 f64 ⇒ 大整数丢精度**
//!
//! ```text
//! parse("9007199254740993")      → Number(9007199254740992.0)   ← 2^53+1 被吞
//! parse("12345678901234567890")  → Number(1.2345678901234567e19)
//! ```
//!
//! **判定不修**：LSP 协议里 `Position.line` / `Position.character` 都是
//! **小整数**，2^53 远超编辑器实际行数；且 `to_string` 往返对整数仍是
//! `42` / `3.14` 这样的正常形式。改动涉及 `Value` 枚举与全部序列化点，
//! 收益为零。

use std::io::Cursor;

use mora::lsp::json::{Value, parse};
use mora::lsp::transport::{read_message, write_message};

fn roundtrip(body: &str) -> (Option<String>, bool) {
    let mut buf = Vec::new();
    if write_message(&mut buf, body).is_err() {
        return (None, false);
    }
    let mut c = Cursor::new(buf);
    match read_message(&mut c) {
        Ok(v) => (v, true),
        Err(_) => (None, false),
    }
}

fn read_raw(payload: &[u8]) -> Result<Option<String>, String> {
    let mut c = Cursor::new(payload);
    read_message(&mut c).map_err(|e| e.to_string())
}

/// **主断言 1**：标准与 UTF-8 多字节消息必须精确往返。
#[test]
fn d371_transport_roundtrips_ascii_and_utf8() {
    for body in [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
        r#"{"text":"你好世界"}"#,
        r#"{"emoji":"😀中文"}"#,
    ] {
        let (v, ok) = roundtrip(body);
        assert!(ok, "写入应成功: {body}");
        assert_eq!(v.as_deref(), Some(body), "往返必须逐字一致: {body}");
    }
}

/// **连续两条消息**：分帧状态机不能吞掉第二条。
#[test]
fn d371_transport_reads_consecutive_messages() {
    let mut buf = Vec::new();
    write_message(&mut buf, r#"{"id":1}"#).unwrap();
    write_message(&mut buf, r#"{"id":2}"#).unwrap();
    let mut c = Cursor::new(&buf[..]);
    assert_eq!(
        read_message(&mut c).unwrap().as_deref(),
        Some(r#"{"id":1}"#)
    );
    assert_eq!(
        read_message(&mut c).unwrap().as_deref(),
        Some(r#"{"id":2}"#)
    );
}

/// **畸形 frame 必须报错**，且不得返回半条消息。
#[test]
fn d371_transport_rejects_malformed_frames() {
    // 缺 Content-Length
    assert!(read_raw(b"Content-Type: application/json\r\n\r\n{}").is_err());
    // Content-Length 非数字
    assert!(read_raw(b"Content-Length: abc\r\n\r\n{}").is_err());
    // Content-Length 大于实际 body
    assert!(read_raw(b"Content-Length: 100\r\n\r\n{}").is_err());
    // 空 header 块
    assert!(read_raw(b"\r\n\r\n").is_err());
    // body 非法 UTF-8
    let mut bad = Vec::new();
    write_message(&mut bad, "{}").unwrap();
    let pos = bad.windows(2).position(|w| w == b"{}").unwrap();
    bad[pos] = 0xFF;
    let mut c = Cursor::new(&bad[..]);
    assert!(read_message(&mut c).is_err(), "非法 UTF-8 body 必须报错");
}

/// **空 body** 返回 `Some("")` 而不是 `None` / Err。
#[test]
fn d371_transport_handles_zero_length_body() {
    let (v, ok) = roundtrip("");
    assert!(ok);
    assert_eq!(v.as_deref(), Some(""), "Content-Length: 0 应得空串");
}

/// **宽松 header 形态**：LF-only、额外 header、大小写混写、带空格。
///
/// 这些是**真实客户端**会发的形态（VS Code / neovim 的行为不完全一致）。
#[test]
fn d371_transport_accepts_lenient_header_forms() {
    for (label, payload) in [
        ("LF-only", &b"Content-Length: 2\n\n{}"[..]),
        (
            "extra header",
            &b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: 2\r\n\r\n{}"[..],
        ),
        ("lower-case name", &b"content-length: 2\r\n\r\n{}"[..]),
        ("padded value", &b"Content-Length:   2  \r\n\r\n{}"[..]),
        ("utf-8 header", "Content-Length: 2\r\nX-注释: 值\r\n\r\n{}".as_bytes()),
    ] {
        match read_raw(payload) {
            Ok(Some(s)) => assert_eq!(s, "{}", "{label}: body 应正确读出"),
            other => panic!("{label}: 应成功读出 body，实得 {other:?}"),
        }
    }
}

/// **`lsp::json` 的标准形态解析**。
#[test]
fn d371_lsp_json_parses_standard_forms() {
    assert_eq!(parse("42").unwrap(), Value::Number(42.0));
    assert_eq!(parse("-7").unwrap(), Value::Number(-7.0));
    // ⚠ 浮点期望值**不能**用 π 的近似（`3.14` / `1.0e10`）——
    // clippy 的 `approx_constant` 会判为错误。用不接近 π 的值。
    assert_eq!(parse("2.5").unwrap(), Value::Number(2.5));
    // ⚠ 写成 `1.0e10` 会被 clippy 判为「approximate value of PI」——
    // 1e10 与 π/3 的 10 次方极其接近。改用普通字面量。
    assert_eq!(parse("1e10").unwrap(), Value::Number(10_000_000_000.0));
    assert_eq!(parse("1.5e-8").unwrap(), Value::Number(1.5e-8));
    assert_eq!(parse("true").unwrap(), Value::Bool(true));
    assert_eq!(parse("false").unwrap(), Value::Bool(false));
    assert_eq!(parse("null").unwrap(), Value::Null);
    assert_eq!(parse("[]").unwrap(), Value::Array(vec![]));
    assert_eq!(parse("{}").unwrap(), Value::Object(Default::default()));
    // 中文与代理对
    assert_eq!(parse(r#""你好""#).unwrap(), Value::String_("你好".into()));
    assert_eq!(parse(r#""\u4f60""#).unwrap(), Value::String_("你".into()));
    assert_eq!(
        parse(r#""\ud83d\ude00""#).unwrap(),
        Value::String_("😀".into())
    );
    // 转义
    assert_eq!(
        parse(r#""a\nb\tc\"d""#).unwrap(),
        Value::String_("a\nb\tc\"d".into())
    );
    // 深嵌套
    assert!(parse(r#"{"a":{"b":{"c":[1,[2,[3]]]}}}"#).is_ok());
}

/// **畸形 JSON 必须报错**（不该静默产生值）。
#[test]
fn d371_lsp_json_rejects_malformed_input() {
    for (label, src) in [
        ("尾逗号 obj", r#"{"a":1,}"#),
        ("尾逗号 arr", "[1,2,]"),
        ("单引号", "{'a':1}"),
        ("无引号键", "{a:1}"),
        ("前导 +", "+7"),
        ("裸小数点", ".5"),
        ("NaN", "NaN"),
        ("Infinity", "Infinity"),
        ("未闭合串", r#""abc"#),
        ("未闭合数组", "[1,2"),
        ("空输入", ""),
        ("纯空白", "   "),
    ] {
        assert!(
            parse(src).is_err(),
            "`{label}`（{src:?}）应报错; 实得 {:?}",
            parse(src)
        );
    }
}

/// **`lsp::json` 与 `flow::json` 行为一致**（畸形数字上）。
///
/// 这是本轮的关键结论：LSP 用**自己的** `Value`（`Number(f64)`），
/// 与 `flow::json` 的 `Int`/`Float`/`BigInt` 三分**不同**，
/// 但在 9 个畸形数字上**行为一致** ⇒ 不是分叉，是一致的宽松。
///
/// 若将来有人只改一侧，本条会红。
#[test]
fn d371_lsp_and_flow_json_agree_on_malformed_numbers() {
    for (label, src) in [
        ("前导零", "007"),
        ("hex", "0x1F"),
        ("双小数点", "1.2.3"),
        ("尾点", "5."),
        ("裸点", ".5"),
        ("前导 +", "+7"),
        ("大写指数", "1E5"),
        ("尾随垃圾", r#"{"a":1}xyz"#),
        ("两个数字", "1 2"),
    ] {
        let lsp_ok = parse(src).is_ok();
        let flow_ok = mora::flow::json_to_value(src).is_ok();
        assert_eq!(
            lsp_ok, flow_ok,
            "`{label}`（{src:?}）: lsp={lsp_ok} flow={flow_ok} —— 两套实现应一致"
        );
    }
}

/// **已知限制：JSON 数字统一 f64，大整数丢精度。**
///
/// **判定不修**：LSP 的 `Position.line` / `Position.character` 都是小整数，
/// 2^53 远超编辑器实际行数；`to_string` 往返对常规数值形式正常。
/// 本条把现状**钉住**，将来若改 `Value` 枚举，本条会提醒同步。
#[test]
fn d371_big_integers_lose_precision_because_number_is_f64() {
    // 2^53 + 1 ⇒ 被舍入成 2^53
    assert_eq!(
        parse("9007199254740993").unwrap(),
        Value::Number(9007199254740992.0),
        "LSP 的 `Number` 是 f64 ⇒ 2^53 以上丢精度（已知限制，非缺陷）"
    );
    // 常规整数往返正常
    assert_eq!(mora::lsp::json::to_string(&parse("42").unwrap()), "42");
    assert_eq!(mora::lsp::json::to_string(&parse("3.14").unwrap()), "3.14");
    // object 的 key 排序是确定的（BTreeMap）
    assert_eq!(
        mora::lsp::json::to_string(&parse(r#"{"b":1,"a":2}"#).unwrap()),
        r#"{"a":2,"b":1}"#,
        "Object 是 BTreeMap ⇒ 序列化按 key 排序（确定性）"
    );
}
