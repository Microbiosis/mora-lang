//! v0.104.6 D380 —— CLI **flag 解析的静默兜底**全称普查（否定轮钉现状 + 一项待裁决）
//!
//! D379 在测 `record export` 时发现：`main.rs` 的 flag 解析用
//! `_ => {}` 兜底（3 处），**任何未知 flag 都被静默忽略**。
//! 这与 D344「`builtins/**` 的静默兜底普查」是**同一族**，但发生在 **CLI 层**。
//!
//! ## 现状：7 种错误用法**全部 exit 0 静默回落**，零诊断
//!
//! | 用法 | 期望 | 实测 |
//! |---|---|---|
//! | `record export r1 --format md` | Markdown | **Markdown** ✅ |
//! | `record export r1 --formt md`（**错拼**）| 报错 | **JSONL，exit 0，零诊断** ❌ |
//! | `record export r1 --bogus x`（未知）| 报错 | **JSONL，exit 0** ❌ |
//! | `record export r1 md`（位置参数）| 报错 | **JSONL，exit 0** ❌ |
//! | `record export r1 --format BOGUS` | 报错 | **JSONL，exit 0** ❌ |
//! | `record export r1 --format`（缺值）| 报错 | **JSONL，exit 0** ❌ |
//! | `record export r1`（无 format）| JSONL | JSONL ✅ |
//!
//! **危害比 D343（`plan.create` 非法 status）更直接**：
//! 用户要 Markdown 报告（给人看），拿到 JSONL（机器格式），
//! 而**退出码 0 让他以为成功了**。
//!
//! ## 三处静默兜底的位置
//!
//! | 位置 | 上下文 |
//! |---|---|
//! | `main.rs:123` | `--version` / `--help` 之后的 `_ => {}` |
//! | `main.rs:220` | `record export` 的 `--format` / `--output` |
//! | `main.rs:269` | `record snapshot` 的 `--baseline` / `--verify` / `--output` |
//!
//! （`cli/record.rs:591` 的 `_ => {}` 是 `SnapshotDiff` 的 match 分支，
//! **不是**参数解析，不属此列。）
//!
//! ## 与已有防护的关系
//!
//! D189 建的 `cli::reject_option_as_path`（`cli/mod.rs:193`）解决的是
//! **另一类**问题：把 `--opt=2` 这种**看起来是路径**的参数当成文件读，
//! 导致错误归因错位。它在 6 处被调用，**不覆盖**未知 flag 被忽略。
//!
//! ⇒ 两者互补，都留着。

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

struct WorkDir(PathBuf);
impl WorkDir {
    fn new(tag: &str) -> Self {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("d380_{n}_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".mora").join("recordings")).expect("建目录");
        std::fs::write(
            d.join(".mora").join("recordings").join("r1.jsonl"),
            "{\"kind\":\"note\",\"id\":1,\"ts_ms\":100,\"message\":\"hi\"}\n",
        )
        .expect("写录像");
        WorkDir(d)
    }
    fn run(&self, args: &[&str]) -> (i32, String) {
        let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
        let out = Command::new(exe)
            .current_dir(&self.0)
            .args(args)
            .output()
            .expect("跑 mora");
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        s.push('\n');
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), s)
    }
}
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn is_markdown(s: &str) -> bool {
    s.lines().any(|l| l.trim_start().starts_with("# Recording"))
}
fn is_jsonl(s: &str) -> bool {
    s.lines().any(|l| l.trim_start().starts_with('{'))
}

/// **正常用法必须正常** —— 这是本文件的前提钉。
#[test]
fn d380_correct_usage_works() {
    let wd = WorkDir::new("ok");
    let (code, out) = wd.run(&["record", "export", "r1", "--format", "md"]);
    assert_eq!(code, 0, "正常用法应成功; out={out}");
    assert!(
        is_markdown(&out),
        "`--format md` 应产出 Markdown; out={out}"
    );

    // 不给 format ⇒ 默认 jsonl
    let (code, out) = wd.run(&["record", "export", "r1"]);
    assert_eq!(code, 0, "缺省应成功; out={out}");
    assert!(is_jsonl(&out), "缺省应是 JSONL; out={out}");
}

/// **错拼的 flag 静默回落**（现状钉住）。
///
/// ⚠ 这是**已知的可疑行为**，判据把它**钉住**而不是「修掉」：
/// 若将来改成硬报错，本条会红 —— 那是有意的行为变更，
/// 需同步更新本判据与 CHANGELOG 的待裁决项。
#[test]
fn d380_misspelled_flag_silently_falls_back() {
    let wd = WorkDir::new("misspell");
    let (code, out) = wd.run(&["record", "export", "r1", "--formt", "md"]);
    assert_eq!(code, 0, "错拼 flag 当前**静默回落**（exit 0）; out={out}");
    assert!(is_jsonl(&out), "回落目标是 JSONL; out={out}");
    assert!(
        !is_markdown(&out),
        "错拼的 `--formt` 绝不能**碰巧**生效; out={out}"
    );
}

/// **未知 flag 静默忽略**。
#[test]
fn d380_unknown_flag_is_silently_ignored() {
    let wd = WorkDir::new("unknown");
    for args in [
        vec!["record", "export", "r1", "--bogus", "x"],
        vec!["record", "export", "r1", "md"],       // 位置参数
        vec!["record", "export", "r1", "--format"], // 缺值
        vec!["record", "export", "r1", "--format", "BOGUS"], // 未知 format
    ] {
        let (code, out) = wd.run(&args);
        assert_eq!(
            code,
            0,
            "`{}` 当前静默回落（exit 0）; out={out}",
            args.join(" ")
        );
        assert!(
            is_jsonl(&out) && !is_markdown(&out),
            "`{}` 应静默回落到 JSONL; out={out}",
            args.join(" ")
        );
    }
}

/// **源码层断言**：三处 `_ => {}` 兜底仍在位，且 `reject_option_as_path`
/// **不覆盖**它们（两者互补）。
#[test]
fn d380_the_three_silent_fallbacks_exist_in_source() {
    let main = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"))
        .expect("读 main.rs");
    let count = main.lines().filter(|l| l.trim() == "_ => {}").count();
    assert!(
        count >= 3,
        "main.rs 至少应有 3 处 `_ => {{}}` 静默兜底（:123 / :220 / :269）; 实得 {count}"
    );

    // `reject_option_as_path` 存在但**不覆盖**未知 flag —— 两者互补
    let mod_rs = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/cli/mod.rs"))
        .expect("读 cli/mod.rs");
    assert!(
        mod_rs.contains("fn reject_option_as_path"),
        "D189 建的 `reject_option_as_path` 应仍在（它解决的是**另一类**问题：\
         把 `--opt=2` 当路径读）"
    );
}
