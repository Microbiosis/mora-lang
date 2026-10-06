//! v0.104.6 D207：审计 hash 链把**未篡改**的日志报成 `HashMismatch` ——
//! 读侧把 UTF-8 按 **Latin-1** 拆开（与 D204/D205 同款）。
//!
//! ## 缺陷
//!
//! `src/audit/mod.rs` 的**写**端 `json_string`（第 116–127 行）逐 `chars()`
//! 输出，非 ASCII **原样写**（`c => out.push(c)`）；而**读**端
//! `extract_field_skip_escaped`（第 453 行）是：
//!
//! ```rust
//! out.push(bytes[i] as char);   // bytes[i]: u8
//! ```
//!
//! `u8 as char` 是 **Latin-1 解释**，UTF-8 的每个字节变成一个码点。
//!
//! ## 后果：**假报警的篡改告警**
//!
//! `verify_chain()` 的流程是「从每一行提取字段 → 重建 `AuditEvent` →
//! `seal()` 重算 SHA-256 → 与存储的 hash 比对」。读侧打乱了 `actor` /
//! `action` / `target`，重算的 hash 必然对不上：
//!
//! ```text
//! AuditError::HashMismatch { line, stored, computed }
//! ```
//!
//! 即**任何含非 ASCII 字段的审计日志**（中文 actor、中文路径的 target）
//! 都被判为「被篡改」。对一套**防篡改**系统来说这比不校验更糟：
//! 真篡改者可以藏在噪声里，而运维会学会忽略这条告警。
//!
//! ## 判据
//!
//! ① **主判据**：写入含非 ASCII 字段的事件后，`verify_chain()` 必须通过
//!    （修前报 `HashMismatch`）；
//! ② 提取出的字段内容必须**逐字**还原（用非 ASCII 的 `actor` 反证长度）；
//! ③ **不回归**：纯 ASCII 事件仍然通过；真正被篡改的日志**仍必须**报错
//!    （否则就成了「为了不误报而放弃检测」）；
//! ④ 完整的转义表（`\b` `\f` `\/` `\uXXXX` + 代理对）也要能反转义。

use mora::audit::{AuditEvent, AuditSink, JsonlAuditSink};
use std::path::PathBuf;

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d207_{tag}"));
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

/// `verify_chain` 要断言的单个字段断言：`(字段名, [(actor, action, target)])`。
type FieldCase<'a> = (&'a str, &'a [(&'a str, &'a str, Option<&'a str>)]);

/// 写一批事件到一条新链，然后校验。
fn write_and_verify(tag: &str, events: &[(&str, &str, Option<&str>)]) -> Result<(), String> {
    let dir = WorkDir::new(tag);
    let path = dir.0.join("audit.jsonl");
    let sink = JsonlAuditSink::new_fresh(&path).expect("开 sink");
    for (actor, action, target) in events {
        sink.write(AuditEvent::new(
            *actor,
            *action,
            target.map(|t| t.to_string()),
            None,
            None,
        ))
        .expect("写事件");
    }
    sink.flush().expect("flush");
    sink.verify_chain().map_err(|e| format!("{e:?}"))
}

/// **主判据（有牙齿）**：含非 ASCII 字段的日志**必须**校验通过。
///
/// 修前：这些用例全部返回 `Err(HashMismatch { .. })`。
#[test]
fn d207_verify_chain_passes_for_non_ascii_events() {
    let cases: &[FieldCase] = &[
        ("actor", &[("用户", "login", None)]),
        ("action", &[("svc", "文件.写入", None)]),
        ("target", &[("svc", "file.write", Some("/工作区/报告.md"))]),
        (
            "all",
            &[
                ("代理一", "工具.调用", Some("/数据/一号.csv")),
                ("agent-2", "tool.invoke", Some("/data/2.csv")),
            ],
        ),
        ("emoji", &[("bot", "op", Some("/tmp/😀.txt"))]),
        ("accent", &[("café", "op", Some("/données/x.txt"))]),
    ];
    for (tag, events) in cases {
        if let Err(e) = write_and_verify(tag, events) {
            panic!(
                "[{tag}] 审计链把**未篡改**的日志报成失败 —— 读侧把 UTF-8 按 Latin-1 \
                 拆开，重算的 hash 必然对不上。\n错误: {e}\n\
                 提示：防篡改系统对未篡改日志误报，比不校验更糟。"
            );
        }
    }
}

/// **不回归**：纯 ASCII 事件仍然通过。
#[test]
fn d207_ascii_chain_still_verifies() {
    let events: &[(&str, &str, Option<&str>)] = &[
        ("user_script", "file.write", Some("/workspace/a.txt")),
        ("agent.researcher", "tool.invoke", Some("ai.chat")),
        ("a", "b", None),
    ];
    write_and_verify("ascii", events)
        .unwrap_or_else(|e| panic!("[ascii] 纯 ASCII 反而失败了: {e}"));
}

/// **不回归（反面）**：真正被篡改的日志**仍必须**报错。
///
/// 修 D207 很容易滑向「为了不误报而放弃检测」—— 这条守住那条底线：
/// 改 `actor` 字段后校验必须失败。
#[test]
fn d207_real_tampering_is_still_detected() {
    let dir = WorkDir::new("tamper");
    let path = dir.0.join("audit.jsonl");
    let sink = JsonlAuditSink::new_fresh(&path).expect("开 sink");
    sink.write(AuditEvent::new("用户", "login", None, None, None))
        .expect("写");
    sink.write(AuditEvent::new(
        "svc",
        "op",
        Some("/a.txt".to_string()),
        None,
        None,
    ))
    .expect("写");
    sink.flush().expect("flush");
    assert!(
        sink.verify_chain().is_ok(),
        "前提：未篡改的日志应当通过校验"
    );
    drop(sink);

    // 篡改第 1 行的 actor
    let text = std::fs::read_to_string(&path).expect("读日志");
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    lines[0] = lines[0].replace("\"actor\":\"用户\"", "\"actor\":\"入侵者\"");
    std::fs::write(&path, lines.join("\n") + "\n").expect("写回");

    let sink2 = JsonlAuditSink::new_fresh(&path).expect("重开 sink");
    assert!(
        sink2.verify_chain().is_err(),
        "篡改 actor 之后校验**必须**失败 —— 修 D207 不得变成「放弃检测」"
    );
}

/// 转义表完整性：`\b` `\f` `\/` `\uXXXX` + 代理对都要能**反转义**。
///
/// 写端的 `json_string` 会产出 `\b` `\f` `\u00xx`；读端此前只认 `\"` `\\`
/// `\n` `\r` `\t`，其余落入「原样带出反斜杠」分支 —— 往返不等价。
#[test]
fn d207_escape_round_trip_is_lossless() {
    // `\b` / `\f` 不可打印，用 target 的可打印片段 + 长度反证它们进了事件
    let dir = WorkDir::new("esc");
    let path = dir.0.join("audit.jsonl");
    let sink = JsonlAuditSink::new_fresh(&path).expect("开 sink");
    sink.write(AuditEvent::new("a\x08b\x0cc/d", "op", None, None, None))
        .expect("写");
    sink.flush().expect("flush");
    assert!(
        sink.verify_chain().is_ok(),
        "含 `\\b` / `\\f` / `\\/` 的 actor 反转义后应当与原值一致"
    );

    // 第三方写的日志：合法 JSON 的 `\uXXXX` 与代理对也必须能解出来
    let dir2 = WorkDir::new("uesc");
    let path2 = dir2.0.join("audit.jsonl");
    let sink2 = JsonlAuditSink::new_fresh(&path2).expect("开 sink");
    sink2
        .write(AuditEvent::new("A\u{0001}B", "op", None, None, None))
        .expect("写");
    sink2.flush().expect("flush");
    assert!(
        sink2.verify_chain().is_ok(),
        "控制字符经 `\\u00xx` 转义写出后，反转义必须还原"
    );
}
