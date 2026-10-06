//! v0.104.6 D174：`mora snapshot` **从不安装录制器** → 永久假绿（已修）。
//!
//! ## 缺陷
//!
//! `run_snapshot` 建 `Interpreter::new()` 之后**从不调 `replace_recorder`**。
//! 于是 `infra().recorder()` 是默认的 `Recorder::new_off()`（`Mode::Off`），
//! 而 `record_ai_chat` / `record_web_fetch` / `record_state_mutation` 的第一行都是
//!
//! ```text
//! if !self.mode.is_record() { return; }
//! ```
//!
//! → 一切事件被挡掉，`current_events` **恒空**。比对分支拿 `0 vs 0` 去
//! `diff_snapshot`，`max = 0` → 一个 diff 都不产生 → `mismatches` 空 →
//! 打印 `✓ passed`、exit 0。
//!
//! **这意味着 `mora snapshot` 永远不可能失败。** 实测（修复前）：
//!
//! ```text
//! $ mora snapshot p.mora s1 --update     # 脚本含 ai.chat
//! ✓ snapshot 's1' saved (0 events)      ← 明明该录到 ai.chat
//! $ mora snapshot p.mora s1
//! ✓ snapshot 's1' passed (0 events match)        exit 0
//! $ # 换成完全不同的 prompt：
//! ✓ snapshot 's1' passed (0 events match)        exit 0   ← 应当 FAILED
//! ```
//!
//! 一个不会失败的快照比对，等于没有比对 —— 比「没写这个命令」更糟，
//! 因为它会让人以为回归测试已经覆盖了 AI 调用路径。
//!
//! ## 修法
//!
//! 1. `Mode` 新增 `RecordMemory` 变体：`is_record()` 为真，但**不落盘**。
//!    `mora snapshot` 只需要 `events()` 与基线比对，不需要一份 JSONL 录像。
//!    （原来的 `new_record` 强制要一个真实路径，会凭空建目录再留垃圾。）
//! 2. `run_snapshot` 装上 `Recorder::new_record_memory()`。
//!
//! ## 残留面（已量化，但不在此处擅自定语义）
//!
//! 脚本若一条可录事件都没产生（整个文件只有 `print("hi")`），基线仍是 0 条，
//! 0 vs 0 恒 Match —— **判别力为零的快照**。
//! 现在会显式告警（`[warn] ... can never fail`），
//! 但**退出码仍是 0**：0 事件该不该判失败属于语义裁决，不在缺陷修复里定。
//!
//! ## 本测试守什么
//!
//! 核心是**反向对照**：换成完全不同的 prompt 后，快照比对**必须失败**。
//! 只验「同输入通过」的测试在缺陷存在时同样会绿（它就是原始的假绿形态），
//! 所以必须同时验「异输入必须失败」，这一条才是真正的判据。

use std::path::{Path, PathBuf};
use std::process::Command;

fn mora_exe() -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe").to_string()
}

/// 临时工作目录守卫 —— `Drop` 时删除。
///
/// 不能只在测试尾部写 `remove_dir_all`：断言失败会 panic，尾部代码根本不执行，
/// 于是**失败的测试反而留下垃圾目录**。`Drop` 才管得住所有出口。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mora_d174_{}", tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建工作目录");
        TempDir(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_script(dir: &Path, file: &str, src: &str) -> PathBuf {
    let p = dir.join(file);
    std::fs::write(&p, src).expect("写脚本");
    p
}

/// 跑 `mora <args...>`，返回 (stdout+stderr, exit code)。
fn mora(dir: &Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(mora_exe())
        .current_dir(dir)
        .args(args)
        .output()
        .expect("跑 mora");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (text, out.status.code().unwrap_or(-1))
}

/// 从输出里抠出 `(<n> events)` / `(<n> events match)` 的数字。
fn reported_events(output: &str) -> Option<i64> {
    let marker = if output.contains("events match") {
        "events match"
    } else {
        "events)"
    };
    let at = output.find(marker)?;
    let head = &output[..at];
    let open = head.rfind('(')?;
    // `(` 与数字之间隔着空格：`(0 events)` → 必须先 trim 再倒扫数字。
    let tail = head[open + 1..].trim_end();
    let digits: String = tail
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.chars().rev().collect::<String>().parse().ok()
}

const WITH_AI: &str = r#"let a = ai.chat(p"baseline prompt alpha")
print(a)
"#;

/// 基线：含 `ai.chat` 的脚本 `--update` 后**必须录到事件**（修复前是 0）。
#[test]
fn d174_snapshot_records_events_instead_of_saving_empty_baseline() {
    let dir = TempDir::new("save");
    let prog = write_script(dir.path(), "p.mora", WITH_AI);

    let (out, code) = mora(
        dir.path(),
        &["snapshot", prog.to_str().unwrap(), "s1", "--update"],
    );
    assert_eq!(code, 0, "snapshot --update 应成功: {}", out);
    assert!(out.contains("saved"), "应报告保存基线: {}", out);
    let n = reported_events(&out).expect("应能解析出事件数");
    assert!(
        n > 0,
        "含 ai.chat 的脚本快照基线不该是 0 事件（这正是 D174 的缺陷）: {}",
        out
    );

    // 基线文件里必须真的有事件行，而不只是 header。
    let snap = dir
        .path()
        .join(".mora")
        .join("snapshots")
        .join("s1.snap.jsonl");
    let body = std::fs::read_to_string(&snap).expect("读快照文件");
    let event_lines = body.lines().filter(|l| l.contains("\"kind\"")).count();
    assert!(
        event_lines > 0,
        "快照文件应含事件行，实际只有 header: {}",
        body
    );
    assert!(body.contains("ai.chat"), "快照应含 ai.chat 事件: {}", body);
}

/// 正对照：同输入比对应通过。
#[test]
fn d174_snapshot_same_input_passes() {
    let dir = TempDir::new("pass");
    let prog = write_script(dir.path(), "p.mora", WITH_AI);
    let p = prog.to_str().unwrap();

    let (out, code) = mora(dir.path(), &["snapshot", p, "s1", "--update"]);
    assert_eq!(code, 0, "基线保存失败: {}", out);
    let (out, code) = mora(dir.path(), &["snapshot", p, "s1"]);
    assert_eq!(code, 0, "同输入比对应通过: {}", out);
    assert!(out.contains("passed"), "应报告通过: {}", out);
    assert!(!out.contains("FAILED"), "不该报失败: {}", out);
}

/// **核心判据**：换成完全不同的 prompt，快照比对**必须失败**。
///
/// 缺陷存在时这一条会红（它会报 passed / exit 0），所以它是真正能抓住
/// D174 的那条断言 —— 只验「同输入通过」的测试在缺陷存在时同样是绿的。
#[test]
fn d174_snapshot_detects_changed_prompt() {
    let dir = TempDir::new("changed");
    let base = write_script(dir.path(), "p.mora", WITH_AI);
    let other = write_script(
        dir.path(),
        "p2.mora",
        "let a = ai.chat(p\"TOTALLY DIFFERENT PROMPT zebra 99999\")\nprint(a)\n",
    );

    let (out, code) = mora(
        dir.path(),
        &["snapshot", base.to_str().unwrap(), "s1", "--update"],
    );
    assert_eq!(code, 0, "基线保存失败: {}", out);

    // 改过的 prompt 对同一基线 —— 必须 FAILED
    let (out, code) = mora(dir.path(), &["snapshot", other.to_str().unwrap(), "s1"]);
    assert_eq!(code, 1, "换了 prompt 却仍然通过 = 快照没有判别力: {}", out);
    assert!(out.contains("FAILED"), "应报告失败: {}", out);
    assert!(
        out.contains("ai.chat"),
        "诊断应指出是 ai.chat 变了: {}",
        out
    );
}

/// 0 事件脚本：退出码仍为 0，但必须**明确告警**其判别力为零。
///
/// 守的是「不要再静悄悄地说 passed」—— 不是断言它该失败（那是语义裁决）。
#[test]
fn d174_snapshot_warns_when_zero_events() {
    let dir = TempDir::new("zero");
    let prog = write_script(dir.path(), "e.mora", "print(\"hi\")\n");
    let p = prog.to_str().unwrap();

    let (out, code) = mora(dir.path(), &["snapshot", p, "z0", "--update"]);
    assert_eq!(code, 0, "退出码不变（0 事件是否判失败属语义裁决）: {}", out);
    assert_eq!(reported_events(&out), Some(0), "应确为 0 事件: {}", out);
    assert!(
        out.contains("can never fail"),
        "0 事件快照必须显式告警，不能静默报通过: {}",
        out
    );

    let (out, code) = mora(dir.path(), &["snapshot", p, "z0"]);
    assert_eq!(code, 0, "退出码仍不变: {}", out);
    assert!(
        out.contains("can never fail"),
        "0 事件比对同样必须告警: {}",
        out
    );
}
