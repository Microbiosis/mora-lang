//! v0.104.6 D379 —— `src/record/analysis.rs` 的 **timeline / export 端到端**（否定轮，无产品变更）
//!
//! `src/record/` 覆盖相当充分（12 判据 + 48 自带单测），但
//! **`analysis.rs`（388 行，自带单测 0）**的 `build_timeline` 与
//! `export_recording` 此前无直接判据。
//!
//! ## 前提 ①：事件判别式是 **`"ai.chat"` / `"web.fetch"`**（带点）
//!
//! `serialization.rs:360-361` 的 match 键是 `ai.chat` / `web.fetch`
//! / `note` / `msg`。写成 `ai_chat` 会被解析器**静默丢弃**（`warn` 提示
//! 「N of M line(s) could not be parsed」）。
//!
//! ## 前提 ②：`Note` 的字段是 **`message`** 不是 `text`
//!
//! `record/mod.rs:106-110`：`Note { id, ts_ms, message }`。
//! 写 `text` 时反序列化器容错成 `message: ""`（**内容静默丢失**）。
//!
//! ## 前提 ③：export 的 format 是 **flag**，不是位置参数
//!
//! `mora record export <name> [--format jsonl|md]`（`main.rs:202`）。
//! 传位置参数 `md` 会被 `_ => {}`（`main.rs:220`）**静默忽略**。
//!
//! ⇒ 三条前提都是「**格式错了不报错，静默走兜底**」，
//! 与 D376「bbox 必须是字典」、D380「kind 必须带点」同族。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn mora(dir: &Path, args: &[&str]) -> (i32, String) {
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe)
        .current_dir(dir)
        .args(args)
        .output()
        .expect("跑 mora");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let kept: Vec<String> = text
        .lines()
        .map(str::trim_end)
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
                && !t.contains("不兼容 v0.03")
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join("\n"))
}

struct WorkDir(PathBuf);
impl WorkDir {
    fn new(tag: &str) -> Self {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("d379_{n}_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".mora").join("recordings")).expect("建目录");
        WorkDir(d)
    }
    fn write(&self, name: &str, lines: &[String]) {
        let p = self
            .0
            .join(".mora")
            .join("recordings")
            .join(format!("{name}.jsonl"));
        std::fs::write(p, lines.join("\n") + "\n").expect("写录像");
    }
    fn run(&self, args: &[&str]) -> (i32, String) {
        mora(&self.0, args)
    }
}
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 造一份含**全部 5 类事件**的录像（判别式与字段名**完全按实现**）。
fn full_recording() -> Vec<String> {
    vec![
        r#"{"kind":"ai.chat","id":1,"ts_ms":100,"model":"m1","prompt_hash":"h","prompt_preview":"p","response":"r","tokens_in":10,"tokens_out":20,"latency_ms":5,"error":null,"arg_signature":"()"}"#.into(),
        r#"{"kind":"web.fetch","id":2,"ts_ms":200,"url":"https://a","method":"GET","status":200,"body_len":100,"latency_ms":7,"error":null,"arg_signature":"()"}"#.into(),
        r#"{"kind":"note","id":3,"ts_ms":300,"message":"hello"}"#.into(),
        r#"{"kind":"msg","id":4,"ts_ms":400,"channel":"ch","payload":null,"prior_state_hash":0}"#.into(),
        r#"{"kind":"state_mutation","id":5,"ts_ms":500,"var":"x","old":null,"new":1.0}"#.into(),
        r#"{"kind":"ai.chat","id":6,"ts_ms":600,"model":"m2","prompt_hash":"h","prompt_preview":"p","response":"r","tokens_in":10,"tokens_out":20,"latency_ms":5,"error":null,"arg_signature":"()"}"#.into(),
    ]
}

/// **设备自检**：5 类事件**全部**被解析（无 warn 行）。
///
/// 这一条是本文件的**前提钉** —— 若判别式写错（如 `ai_chat`），
/// 解析器会**静默丢弃**它们，下面的断言就会拿到「少 3 条」的结果
/// 而误判成产品缺陷。
#[test]
fn d379_all_five_event_kinds_parse() {
    let wd = WorkDir::new("all5");
    wd.write("r1", &full_recording());
    let (code, out) = wd.run(&["record", "stats", "r1"]);
    assert_eq!(code, 0, "stats 应成功; out={out}");
    assert!(
        !out.contains("could not be parsed"),
        "5 类事件应**全部**解析成功（判别式/字段名写对）; 实得 warn:\n{out}"
    );
    assert!(out.contains("6 total"), "应有 6 个事件; 实得:\n{out}");
}

/// **子类之和 == total**（D225 的不变式）—— 写死具体数字做不到这件事。
#[test]
fn d379_stats_breakdown_sums_to_total() {
    let wd = WorkDir::new("stats");
    wd.write("r1", &full_recording());
    let (code, out) = wd.run(&["record", "stats", "r1"]);
    assert_eq!(code, 0);
    let num = |label: &str| -> i64 {
        out.lines()
            .find(|l| l.contains(label))
            .and_then(|l| l.split_whitespace().last())
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("`{label}` 那行解析不出数字:\n{out}"))
    };
    let total: i64 = out
        .lines()
        .find(|l| l.contains("Events:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
        .unwrap();
    let sum = [
        "ai.chat:",
        "web.fetch:",
        "notes:",
        "messages:",
        "state mut:",
    ]
    .iter()
    .map(|k| num(k))
    .sum::<i64>();
    assert_eq!(
        sum, total,
        "子类之和 {sum} 必须等于 total {total}（D225 不变式）"
    );
}

/// **`timeline` 逐条列出 5 类事件**，且带 token / latency / status。
#[test]
fn d379_timeline_lists_every_kind() {
    let wd = WorkDir::new("timeline");
    wd.write("r1", &full_recording());
    let (code, out) = wd.run(&["record", "timeline", "r1"]);
    assert_eq!(code, 0, "timeline 应成功; out={out}");
    assert!(out.contains("6 events"), "应报 6 个事件; 实得:\n{out}");
    for (kind, detail) in [
        ("ai.chat", "m1"),
        ("web.fetch", "https://a"),
        ("note", "hello"),
        ("msg", "ch"),
        ("state_mutation", "x"),
    ] {
        assert!(
            out.contains(kind),
            "timeline 应含 `{kind}` 行; 实得:\n{out}"
        );
        assert!(
            out.contains(detail),
            "timeline 的 `{kind}` 行应带 detail `{detail}`; 实得:\n{out}"
        );
    }
    // ai.chat 行应带 token 统计
    assert!(
        out.contains("10+20"),
        "ai.chat 行应带 token 数; 实得:\n{out}"
    );
    // web.fetch 行应带状态码
    assert!(out.contains("200"), "web.fetch 行应带状态码; 实得:\n{out}");
}

/// **`export --format jsonl` 逐行回放全部事件**（字段完整）。
#[test]
fn d379_export_jsonl_roundtrips_all_events() {
    let wd = WorkDir::new("expjsonl");
    wd.write("r1", &full_recording());
    let (code, out) = wd.run(&["record", "export", "r1", "--format", "jsonl"]);
    assert_eq!(code, 0, "export jsonl 应成功; out={out}");
    for frag in [
        r#""kind":"ai.chat""#,
        r#""kind":"web.fetch""#,
        r#""kind":"note""#,
        r#""kind":"msg""#,
        r#""kind":"state_mutation""#,
        r#""message":"hello""#,
        r#""prompt_hash":"h""#,
    ] {
        assert!(out.contains(frag), "导出应保留 `{frag}`; 实得:\n{out}");
    }
}

/// **`export --format md` 产出 Markdown 报告**（不是 JSONL）。
#[test]
fn d379_export_markdown_produces_markdown() {
    let wd = WorkDir::new("expmd");
    wd.write("r1", &full_recording());
    let (code, out) = wd.run(&["record", "export", "r1", "--format", "md"]);
    assert_eq!(code, 0, "export md 应成功; out={out}");
    assert!(
        out.contains("# Recording:"),
        "`--format md` 应产出 Markdown 报告; 实得:\n{out}"
    );
    assert!(
        !out.contains(r#""kind":"ai.chat""#),
        "Markdown 报告**不应**是 JSONL 原文; 实得:\n{out}"
    );
    // D225：markdown 报告也应列出全部 5 类
    for label in ["Events:", "AI calls", "Web calls", "Notes", "Messages"] {
        assert!(
            out.contains(label),
            "Markdown 报告应含 `{label}` 行; 实得:\n{out}"
        );
    }
}

/// **判别式写错 ⇒ 解析器**静默丢弃**并 warn**（不报错、不丢 exit）。
///
/// 本条把「静默丢弃」这个行为**钉住**：将来若改成硬报错，
/// 这条会红并提醒同步（那是更好的行为，但属于契约变更）。
#[test]
fn d379_wrong_discriminant_is_silently_dropped_with_warning() {
    let wd = WorkDir::new("badkind");
    wd.write(
        "r1",
        &[
            // ❌ 判别式缺点 ⇒ 解析器认不出
            r#"{"kind":"ai_chat","id":1,"ts_ms":100,"model":"m","prompt_hash":"h","prompt_preview":"p","response":"r","tokens_in":1,"tokens_out":2,"latency_ms":3,"error":null,"arg_signature":"()"}"#.to_string(),
            r#"{"kind":"note","id":2,"ts_ms":200,"message":"ok"}"#.to_string(),
        ],
    );
    let (code, out) = wd.run(&["record", "timeline", "r1"]);
    assert_eq!(code, 0, "解析失败**不改变退出码**（静默丢弃）; out={out}");
    assert!(
        out.contains("could not be parsed"),
        "应 warn「N of M line(s) could not be parsed」; 实得:\n{out}"
    );
    assert!(
        out.contains("1 events"),
        "只有 note 那条被解析出来; 实得:\n{out}"
    );
}
