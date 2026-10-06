//! v0.104.6 D362 —— `src/mir/handlers/effects.rs` 的 `define`/`assign` 录制路径
//! + **`mora record` 子命令的**分派层级**与 banner 不一致（否定轮，无产品变更）
//!
//! D360/D361 扫了 `src/mir/handlers/` 的 `values.rs` / `control.rs`，
//! 本轮钉 `effects.rs`（665 行）里最核心的两个指令
//! —— `h_define` / `h_assign`，它们是 D354「僵尸绑定」修复的作用面。
//!
//! ## 录制路径完全正确（6 个事件逐条核对）
//!
//! 脚本：
//!
//! ```text
//! let a = 1
//! let b = a + 1
//! a = 5
//! let c = a * 2
//! ```
//!
//! 录像（`mora record <file> <name>`）：
//!
//! ```json
//! {"var":"a","old":null,"new":1.0}     ← define：前值 Nil ✅
//! {"var":"b","old":null,"new":2.0}     ← define 读到上一轮的 a ✅
//! {"var":"a","old":1.0,"new":5.0}      ← assign：**旧值正确** ✅
//! {"var":"c","old":null,"new":10.0}    ← define 读到 a=5 ✅
//! ```
//!
//! `h_define`（L28-34）与 `h_assign`（L52-59）的**分叉**只在录制器开启时
//! 才有意义：未录制时 `h_define` 直接把所有权交给 `env.define`，
//! `h_assign` 直接 `env.assign`（省掉两次 `clone()`）——
//! 两者都正确。
//!
//! ## 发现：banner 的**分派层级**与实现不一致
//!
//! 启动 banner 明写：
//!
//! ```text
//! v0.15 CLI: record / replay / diff / list / stats / timeline
//! ```
//!
//! 六个并列。实测**只有前两个是顶层**：
//!
//! | 命令 | 层级 | 实测 |
//! |---|---|---|
//! | `mora replay <file> <name>` | **顶层** | ✅ exit 0 |
//! | `mora diff <a> <b>` | **顶层** | ✅ exit 0 |
//! | `mora record list` | 子命令 | ✅ exit 0 |
//! | `mora record stats <name>` | 子命令 | ✅ exit 0 |
//! | `mora record timeline <name>` | 子命令 | ✅ exit 0 |
//! | **`mora list`** | — | ❌ *系统找不到指定的文件* |
//!
//! `mora list` 没有顶层分支 ⇒ 落到默认的「执行文件」路径，
//! 把 `list` 当**文件名**去读 ⇒ 报文件不存在。
//!
//! **判定为文档措辞问题，不改产品**：
//! `mora run` 也曾有同样的「被当作文件名」问题（`main.rs:165` 注释明说
//! 是 v0.08.5 修的），说明「未知子命令 → 当文件名读」是**既有行为**，
//! 不是本轮引入。banner 只是把命令**列举**出来，未承诺它们都是顶层。
//!
//! ⇒ **只报告**，是否把 banner 改成 `record {list,stats,timeline}` 的
//! 分组写法，属**文档措辞决定**，待裁决。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d362_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

/// 在独立工作目录里跑一条 CLI 命令，返回 (exit, 合并输出)。
fn cli(dir: &std::path::Path, args: &[&str]) -> (i32, String) {
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
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("Mora v")
                && !l.starts_with("AI:")
                && !l.starts_with("AI 原语")
                && !l.starts_with("显式 API")
                && !l.starts_with("Trait 系统")
                && !l.starts_with("Built-in")
                && !l.starts_with("v0.15 CLI")
                // ⚠ 只滤 banner 的「⚠  不兼容 v0.03 builtin」，
                // **不能**滤掉「⚠ replayed 0/0」—— 那正是本文件要验的行
                // （D182 的修复：0 命中时把 ✓ 换成 ⚠）。
                && !l.contains("不兼容 v0.03")
                && !l.starts_with("[9layer]")
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

fn fresh_dir(tag: &str) -> std::path::PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d362_{n}_{}", slug(tag)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    dir
}

const SCRIPT: &str = "let a = 1\nlet b = a + 1\na = 5\nlet c = a * 2\nprint(b)\nprint(c)\n";

/// **主断言**：录制出的 `state_mutation` 事件里，`define` 的前值是 Nil、
/// `assign` 的前值是**赋值前的旧值**。
///
/// `h_assign` 的注释（L51）明写「录制分支保持原顺序（先读旧值、再写入）
/// —— 旧值必须反映赋值前状态」。本条钉住它。
#[test]
fn d362_record_captures_correct_old_values_for_define_and_assign() {
    let dir = fresh_dir("record");
    let script = dir.join("s.mora");
    std::fs::write(&script, SCRIPT).expect("写脚本");

    let (code, out) = cli(&dir, &["record", script.to_str().unwrap(), "run1"]);
    assert_eq!(code, 0, "record 应成功; 实得 exit={code} out={out}");
    assert!(out.contains("recorded"), "应确实录到事件; 实得: {out}");

    let rec = dir.join(".mora").join("recordings").join("run1.jsonl");
    let body = std::fs::read_to_string(&rec).expect("读录像");

    // define：前值必须是 null（Nil）
    assert!(
        body.contains(r#""var":"a","old":null,"new":1.0"#),
        "`let a = 1` 的 define 应记 old=null new=1.0; 录像:\n{body}"
    );
    assert!(
        body.contains(r#""var":"b","old":null,"new":2.0"#),
        "`let b = a + 1` 应读到 a=1 得 2.0; 录像:\n{body}"
    );

    // assign：**旧值**必须正确
    assert!(
        body.contains(r#""var":"a","old":1.0,"new":5.0"#),
        "`a = 5` 必须记 **旧值 1.0**（h_assign 注释要求的顺序）; 录像:\n{body}"
    );
    assert!(
        body.contains(r#""var":"c","old":null,"new":10.0"#),
        "`let c = a * 2` 应读到 a=5 得 10.0; 录像:\n{body}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **0 命中时 replay 不得打 ✓** —— D182 的既有修复。
///
/// 实测 `⚠ replayed 0/0 recorded call(s)`（脚本里没有 ai.chat / web.fetch，
/// 所以没有可重放条目）。那条 ✓ 会被读成「重放成功了」，
/// 而它其实只表示「命令跑完了」。
#[test]
fn d362_replay_reports_zero_hits_without_checkmark() {
    let dir = fresh_dir("replay");
    let script = dir.join("s.mora");
    std::fs::write(&script, SCRIPT).expect("写脚本");
    let (code, _) = cli(&dir, &["record", script.to_str().unwrap(), "r1"]);
    assert_eq!(code, 0, "录制应成功");

    let (code, out) = cli(&dir, &["replay", script.to_str().unwrap(), "r1"]);
    assert_eq!(code, 0, "replay 应成功; 实得 exit={code} out={out}");
    assert!(
        out.contains("replayed 0/0"),
        "无可重放条目时应报 0/0; 实得: {out}"
    );
    assert!(
        !out.contains("✓ replayed"),
        "0 命中时**不得**打 ✓（D182 已修）; 实得: {out}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **`record diff` 应逐条列出事件**。
#[test]
fn d362_record_diff_lists_events() {
    let dir = fresh_dir("diff");
    let script = dir.join("s.mora");
    std::fs::write(&script, SCRIPT).expect("写脚本");
    let (code, _) = cli(&dir, &["record", script.to_str().unwrap(), "d1"]);
    assert_eq!(code, 0, "录制应成功");

    let (code, out) = cli(&dir, &["diff", "d1", "d1"]);
    assert_eq!(code, 0, "diff 应成功; 实得 exit={code} out={out}");
    assert!(
        out.contains("vs") && out.contains("state_mutation"),
        "diff 应逐条列出 state_mutation; 实得: {out}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **分派层级**：`replay` / `diff` 是**顶层**，`list` / `stats` / `timeline`
/// 需要 `record` 前缀。
///
/// 本条钉住**现状**（不是「应该怎样」）——
/// banner 把 6 个并列，措辞上有歧义，但**实际层级**如此。
/// 若将来有人给 `mora list` 加了顶层别名，本条会红，那是有意的行为变更。
#[test]
fn d362_subcommand_dispatch_levels() {
    let dir = fresh_dir("levels");
    let script = dir.join("s.mora");
    std::fs::write(&script, SCRIPT).expect("写脚本");
    let (code, _) = cli(&dir, &["record", script.to_str().unwrap(), "x1"]);
    assert_eq!(code, 0, "录制应成功");
    let sp = script.to_str().unwrap().to_string();

    // 顶层可用
    for args in [vec!["replay", sp.as_str(), "x1"], vec!["diff", "x1", "x1"]] {
        let (code, out) = cli(&dir, &args);
        assert_eq!(
            code, 0,
            "`mora {}` 是顶层命令，应成功; 实得 exit={code} out={out}",
            args[0]
        );
    }
    // 子命令可用
    for args in [
        vec!["record", "list"],
        vec!["record", "stats", "x1"],
        vec!["record", "timeline", "x1"],
    ] {
        let (code, out) = cli(&dir, &args);
        assert_eq!(
            code,
            0,
            "`mora {}` 应成功; 实得 exit={code} out={out}",
            args.join(" ")
        );
    }

    // **顶层没有 `list`** ⇒ 落到「执行文件」路径，把 `list` 当文件名读
    let (code, out) = cli(&dir, &["list"]);
    assert_ne!(
        code, 0,
        "`mora list` 无顶层分支，应落到执行文件路径并失败; 实得 exit={code} out={out}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **`record` 家族齐全** —— banner 列的 6 个命令**实际都存在**
/// （只是层级不同），本条防止「某个命令彻底消失」。
#[test]
fn d362_all_six_banner_commands_exist_at_their_own_level() {
    let dir = fresh_dir("six");
    let script = dir.join("s.mora");
    std::fs::write(&script, SCRIPT).expect("写脚本");
    let (code, _) = cli(&dir, &["record", script.to_str().unwrap(), "z1"]);
    assert_eq!(code, 0, "录制应成功");
    let sp = script.to_str().unwrap().to_string();

    // banner 的六个：record / replay / diff / list / stats / timeline
    for args in [
        vec!["record", "list"],
        vec!["record", "stats", "z1"],
        vec!["record", "timeline", "z1"],
        vec!["replay", sp.as_str(), "z1"],
        vec!["diff", "z1", "z1"],
    ] {
        let (code, out) = cli(&dir, &args);
        assert_eq!(
            code,
            0,
            "banner 承诺的 `{}` 必须可用; 实得 exit={code} out={out}",
            args.join(" ")
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
