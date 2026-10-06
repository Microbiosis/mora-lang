//! v0.104.6 D178：`load_jsonl` **静默丢弃**解析不了的行 —— 密钥扫描器在残缺数据上说「没有密钥」（已修）。
//!
//! ## 缺陷
//!
//! `record/serialization.rs::load_jsonl` 对每个非空行调 `parse_event_line`，
//! 失败就**直接丢掉**（原注释：`// 解析失败的行跳过 (前向兼容)`），
//! 不留任何痕迹。
//!
//! 这是**取证工具的数据入口** —— `replay` / `diff` / `stats` / `export` /
//! **`audit`** 全部建立在它之上。丢几行之后，每个下游命令都**照常报成功**。
//!
//! 实测（真实 CLI，把录制文件首行截成半行模拟「进程被 kill 写到一半」）：
//!
//! ```text
//! 原文件 3 events，首行含 ai.chat 的密钥
//! $ mora record audit full
//!   [（修前：零告警）
//!   ✓ No secrets found in recording 'full'     exit 0
//! $ mora record stats full
//!   Events: 2 total                              ← 3 变 2，零提示
//! ```
//!
//! **安全闸门被静音**：那份录像里明明有密钥，扫描器却给了「干净」的保证。
//! 这比没有扫描器更糟（D177 同源：**说「没有」比不检查更糟**）。
//!
//! ## 为什么「跳过」本身不是缺陷
//!
//! 容忍畸形行的意图是**对的**：`parse_event_line` 只抽取认识的字段，
//! 不认识的字段本来就忽略 —— 所以「新版本写的字段」根本不会让整行失败，
//! 失败的行是**真的畸形**（截断、非 JSON、被改坏）。
//!
//! 故本轮**不改行为**（仍跳过），只把「静默」改成「显式」。
//!
//! ## 修法
//!
//! - `load_jsonl` 额外返回 `Vec<SkippedLine>`（行号 + **限长**片段）；
//! - `Recorder.skipped_lines` 承载它；
//! - 8 个下游消费者一律经 `warn_skipped()` 报出来；
//! - **`audit` 单独硬失败**：它回答的是安全问题，数据不完整 ⇒ 无资格出结论。

use std::path::{Path, PathBuf};
use std::process::Command;

/// 工作目录守卫 —— `Drop` 时删除。
struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d178_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建工作目录");
        WorkDir(d)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn script(&self, file: &str, src: &str) -> PathBuf {
        let p = self.0.join(file);
        std::fs::write(&p, src).expect("写脚本");
        p
    }
    /// 录像文件路径（相对进程 CWD，D173 已记档）。
    fn recording(&self, name: &str) -> PathBuf {
        self.0
            .join(".mora")
            .join("recordings")
            .join(format!("{name}.jsonl"))
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
    .output()
    .expect("跑 mora");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (s, out.status.code().unwrap_or(-1))
}

fn record(dir: &Path, script: &Path, name: &str) {
    let (out, code) = mora(dir, &["record", script.to_str().unwrap(), name]);
    assert_eq!(code, 0, "录制 {} 应成功: {}", name, out);
    assert!(out.contains("recorded"), "应录到事件: {}", out);
}

/// 把录像的第一行截成半行，模拟「进程被 kill，写到一半」。
///
/// 保留原文件的**其它行**，于是事件数会真的少 1 —— 这正是原先静默的情形。
fn truncate_first_line(dir: &WorkDir, name: &str) -> String {
    let p = dir.recording(name);
    let body = std::fs::read_to_string(&p).expect("读录像");
    let mut lines: Vec<String> = body.lines().map(str::to_string).collect();
    assert!(lines.len() >= 2, "前提：录像应至少 2 行");
    let orig = lines[0].clone();
    // 切到 JSON 中段，保留 `{"kind":"ai.chat",...` 的开头。
    let cut = orig.len() / 2;
    let cut = floor_char_boundary(&orig, cut);
    lines[0] = orig[..cut].to_string();
    std::fs::write(&p, lines.join("\n")).expect("写坏录像");
    orig
}

/// 向下取到最近的字符边界（避免切在多字节字符中间）。
fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// **主判据（有牙齿）**：残缺的录像必须让 `audit` 判**无法出结论**并失败。
///
/// 修前这里输出 `✓ No secrets found` + exit 0。
#[test]
fn d178_audit_refuses_to_certify_a_truncated_recording() {
    let dir = WorkDir::new("trunc");
    let script = dir.script(
        "a.mora",
        "let x = ai.chat(p\"sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ012345\")\nprint(x)\n",
    );
    record(dir.path(), &script, "rec");
    truncate_first_line(&dir, "rec");

    let (out, code) = mora(dir.path(), &["record", "audit", "rec"]);
    assert_ne!(
        code, 0,
        "数据残缺却判「通过」= 假阴性，安全闸门被静音:\n{}",
        out
    );
    assert!(
        !out.contains("No secrets found"),
        "残缺数据上不得宣称「没有密钥」:\n{}",
        out
    );
    assert!(
        out.contains("could not be parsed"),
        "应说明有多少行没被检查:\n{}",
        out
    );
    assert!(
        out.contains("line 1"),
        "应指出是第几行（便于定位）:\n{}",
        out
    );
}

/// 汇报型命令（`stats`）也必须说出「只看到部分数据」。
///
/// 它们不硬失败（事件数本来就会打出来），但**不能沉默**。
#[test]
fn d178_stats_warns_about_partial_data() {
    let dir = WorkDir::new("stats");
    let script = dir.script("a.mora", "let x = 1\nprint(x)\n");
    record(dir.path(), &script, "rec");
    truncate_first_line(&dir, "rec");

    let (out, _) = mora(dir.path(), &["record", "stats", "rec"]);
    assert!(
        out.contains("could not be parsed") && out.contains("PARTIAL"),
        "stats 必须提示数据残缺:\n{}",
        out
    );
}

/// **正对照**：完好且无密钥的录像，仍应正常判「干净」+ exit 0。
///
/// 守的是「修复没有把所有 audit 都变成失败」。
#[test]
fn d178_audit_still_passes_on_a_clean_intact_recording() {
    let dir = WorkDir::new("clean");
    let script = dir.script(
        "a.mora",
        "let x = ai.chat(p\"just a normal question about rust\")\nprint(x)\n",
    );
    record(dir.path(), &script, "rec");

    let (out, code) = mora(dir.path(), &["record", "audit", "rec"]);
    assert_eq!(code, 0, "完好的干净录像应通过:\n{}", out);
    assert!(
        out.contains("No secrets found"),
        "应正常报告无密钥:\n{}",
        out
    );
    assert!(
        !out.contains("could not be parsed"),
        "完好文件不该有解析告警:\n{}",
        out
    );
}

/// **正对照**：完好但**确有密钥**的录像，仍应被报出并 exit 1。
#[test]
fn d178_audit_still_flags_secret_in_intact_recording() {
    let dir = WorkDir::new("secret");
    let script = dir.script(
        "a.mora",
        "let x = ai.chat(p\"sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ012345\")\nprint(x)\n",
    );
    record(dir.path(), &script, "rec");

    let (out, code) = mora(dir.path(), &["record", "audit", "rec"]);
    assert_eq!(code, 1, "有密钥应判失败:\n{}", out);
    assert!(
        out.contains("potential secret(s) found"),
        "应报出密钥:\n{}",
        out
    );
    assert!(
        !out.contains("INCONCLUSIVE"),
        "完好文件不该报「无法出结论」:\n{}",
        out
    );
}
