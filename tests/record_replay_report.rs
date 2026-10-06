//! v0.104.6 D182：`mora replay` 报的是「**加载了几条事件**」，不是「重放了几次」——
//! 一次都没命中也照样打 `✓`（已修）。
//!
//! ## 缺陷
//!
//! 修前的收尾一行是：
//!
//! ```text
//! println!("✓ replayed {} events from {}", recorder().events().len(), path);
//! ```
//!
//! `events().len()` 是**从文件加载进来多少条**。三处让它虚高：
//!
//! 1. **命中与否它不管。** 一次都没匹配上，照样打 `✓`。
//! 2. **不是所有事件都能被重放。** 能重放的只有 `ai.chat` / `web.fetch`
//!    （`event_to_replay_entry` 建的索引只含这两类）；`state_mutation` /
//!    `msg` / `note` **没有条目**。而一次普通 `ai.chat` 录制就有
//!    2 条 `state_mutation` 搭头 —— 「3 条事件」里真正可重放的只有 1 条。
//! 3. 于是那个 `✓` 被读成「重放成功了」，而实际可能**什么都没复现**。
//!
//! 修前实测（真实 CLI，录像确有 1 个 ai.chat）：
//!
//! ```text
//! $ mora replay miss.mora rr        # miss.mora 用的是一个从未录过的 prompt
//! [Mock response for: a prompt that was NEVER recorded anywhere]
//! ✓ replayed 3 events from …rr.jsonl         ← 一次都没命中，仍报「成功」
//! ```
//!
//! ## 为什么这在 D181 之后更要紧
//!
//! D181 已证实「没命中」不是理论风险：prompt 不一致、model 不一致、
//! 签名漂移，任一都会静默回落。而回落的目标**可能是真实 API**
//! （有 `OPENAI_API_KEY` 时）—— 用戶以为自己在离线重放，其实发了网络请求。
//! 命令却报 `✓`。
//!
//! ## 修法
//!
//! - `Recorder` 增加 `replay_hits` / `replay_misses`，两个 lookup
//!   命中与未命中时各自累加；
//! - `run_replay` 报 `✓/⚠ replayed <hits>/<replayable> recorded call(s)`，
//!   **并按命中数切换符号**（0 命中不给 `✓`）；
//! - 0 命中但录像里确有可重放条目 → 明确说「NOTHING was replayed」，
//!   并提醒「设了 key 的话这些调用可能打了真实 API」。
//!
//! **退出码未改**（仍为 0）：「0 命中是否该判失败」属 CLI 契约决定，
//! 与 D176 记录的 `mora diff` 恒 exit 0 是同一类待裁决项，**仅报告不擅改**。

use std::path::{Path, PathBuf};
use std::process::Command;

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d182_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建目录");
        WorkDir(d)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn write(&self, file: &str, body: &str) -> PathBuf {
        let p = self.0.join(file);
        std::fs::write(&p, body).expect("写文件");
        p
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn mora(dir: &Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/target/debug/mora.exe"
    ))
    .current_dir(dir)
    .args(args)
    .env_remove("OPENAI_API_KEY")
    .env_remove("MORA_AI_BASE_URL")
    .output()
    .expect("跑 mora");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (s, out.status.code().unwrap_or(-1))
}

const RECORDED: &str = "DISTINCTIVE_RECORDED_VALUE_12345";

/// 录一份含 1 个 `ai.chat` 的录像（另外会带 2 条 `state_mutation`）。
fn record_recording(dir: &WorkDir, name: &str) {
    let script = dir.write(
        "rec.mora",
        &format!(
            "with mock_llm = [\"{RECORDED}\"]\n  let a = ai.chat(p\"find me\")\n  print(a)\nend\n"
        ),
    );
    let (out, code) = mora(dir.path(), &["record", script.to_str().unwrap(), name]);
    assert_eq!(code, 0, "录制应成功: {}", out);
}

/// **主判据（有牙齿）**：一次都没命中时，**不得**报 `✓ replayed`，且必须
/// 明说「什么都没重放」。
///
/// 修前这里输出 `✓ replayed 3 events`。
#[test]
fn d182_zero_hit_replay_does_not_claim_success() {
    let dir = WorkDir::new("zero");
    record_recording(&dir, "rr");
    let player = dir.write(
        "miss.mora",
        "let a = ai.chat(p\"a prompt that was NEVER recorded anywhere\")\nprint(a)\n",
    );

    let (out, _) = mora(dir.path(), &["replay", player.to_str().unwrap(), "rr"]);
    assert!(
        !out.contains("✓ replayed"),
        "一次都没命中却打 ✓ —— 命令跑完了 ≠ 重放成功了:\n{}",
        out
    );
    assert!(out.contains("replayed 0/"), "必须报出 0 命中:\n{}", out);
    assert!(
        out.contains("NOTHING was replayed"),
        "必须明确说「什么都没重放」:\n{}",
        out
    );
    assert!(
        out.contains("LIVE API"),
        "0 命中时应提醒可能打了真实 API（D181 证实的回退方向）:\n{}",
        out
    );
}

/// 正对照：全部命中时必须报 `✓ replayed 1/1`，且**不得**出现告警。
#[test]
fn d182_full_hit_replay_reports_accurate_counts() {
    let dir = WorkDir::new("full");
    record_recording(&dir, "rr");
    let player = dir.write("play.mora", "let a = ai.chat(p\"find me\")\nprint(a)\n");

    let (out, code) = mora(dir.path(), &["replay", player.to_str().unwrap(), "rr"]);
    assert_eq!(code, 0, "命中时不应报错: {}", out);
    assert!(
        out.contains("✓ replayed 1/1"),
        "全部命中应报 `✓ replayed 1/1`:\n{}",
        out
    );
    assert!(out.contains(RECORDED), "应返回录像里的响应:\n{}", out);
    assert!(
        !out.contains("NOTHING was replayed") && !out.contains("did NOT match"),
        "全部命中时不该有告警:\n{}",
        out
    );
}

/// **数字必须诚实**：`state_mutation` 不可重放，不得被算进「可重放」分母。
///
/// 修前那句 `replayed 3 events` 把 2 条 `state_mutation` 也算了进去。
#[test]
fn d182_replayable_count_excludes_non_replayable_events() {
    let dir = WorkDir::new("counts");
    record_recording(&dir, "rr");
    let player = dir.write("play.mora", "let a = ai.chat(p\"find me\")\nprint(a)\n");

    let (out, _) = mora(dir.path(), &["replay", player.to_str().unwrap(), "rr"]);
    assert!(
        out.contains("1/1"),
        "可重放条目只有 1 个（1 个 ai.chat），分母应是 1:\n{}",
        out
    );
    // 加载的条数可以如实带出（那是另一个事实），但要与「可重放」分开说。
    assert!(
        out.contains("event(s) loaded"),
        "应区分「加载了几条」与「可重放几条」:\n{}",
        out
    );
    assert!(
        !out.contains("replayed 1 events"),
        "不得再用「加载条数」冒充「重放次数」:\n{}",
        out
    );
}
