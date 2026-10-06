//! v0.104.6 D176：`mora diff` 对 **state mutation 的值变化完全失明**（已修）。
//!
//! ## 缺陷
//!
//! `record::diff::summarize_event` 对 `Event::StateMutation` 只取 `var`（变量名），
//! 把 `old` / `new` **两个值整个丢掉**：
//!
//! ```text
//! Event::StateMutation { var, .. } => format!("state_mutation var={}", var),
//! ```
//!
//! `diff_recordings` 比的就是这个摘要串，于是**「同一变量名、值不同」被判成
//! `identical`**。真实 `mora diff` 实测（修前）：
//!
//! ```text
//! # s1.mora: let score = 42      →  录制 2 events
//! # s2.mora: let score = 99999   →  录制 2 events
//! $ mora diff d176s1 d176s2
//!   [#1] state_mutation var=score
//!   [#2] state_mutation var=__let_result
//! summary: identical=2 changed=0        ← 值差了一万倍，却「完全相同」
//! ```
//!
//! ## 为什么这比「多报几个 changed」严重
//!
//! 两点，都不是修辞：
//!
//! 1. **数据是齐的，是比对时自己扔的。** 两份 JSONL 里分别是
//!    `{"kind":"state_mutation","var":"score","old":null,"new":42.0}` 与
//!    `…"new":99999.0`。即**不是没录到**，是录到了却在 diff 层被丢弃。
//! 2. **state mutation 记录的就是「状态/记忆变了」** —— 它的**值变化本身
//!    就是被观测的结果**。对这类事件丢值，等于让 diff 面对最该发现的那一类
//!    差异失明，却照样打 `changed=0`。
//!
//! 这与 D174（`mora snapshot` 从不装录制器 → 永久假绿）同族：**一个本该能
//! 失败的比较，却结构性地报「一致」**。两者的区别是 D174 是「没数据」，
//! D176 是「有数据但没用」。
//!
//! ## 修法
//!
//! 摘要带上 `old -> new`，并按 `AiChat` 那个 `resp` 的**同一套 60 字符截断
//! 口径**处理（此处取 40），免得长 dict/list 把对齐的 diff 输出冲垮。
//!
//! ## 判据：两条方向都要
//!
//! - **正对照**：同一脚本录两次 → 必须 `changed=0`（修法不能引入误报）；
//! - **反向对照**：同一变量名、值不同 → 必须 `changed >= 1`（这条才有牙齿）。
//!
//! 只守正向的话，把摘要改成常量字符串也能过。

use std::path::{Path, PathBuf};
use std::process::Command;

/// 工作目录守卫 —— `Drop` 时删除。
///
/// 不能只在测试尾部 `remove_dir_all`：断言失败 panic 时尾部根本不执行，
/// 于是**失败的测试反而留下垃圾目录**。守卫管得住所有出口。
struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d176_{tag}"));
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
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn mora_exe() -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe").to_string()
}

/// 跑 `mora <args>`，返回 (stdout+stderr, exit code)。
///
/// ⚠ 必须给子进程设 `current_dir`：录制落 `.mora/recordings/`，**相对进程 CWD**
/// （D173 实测并记档）。不设就会污染仓库工作区。
fn mora(dir: &Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(mora_exe())
        .current_dir(dir)
        .args(args)
        .output()
        .expect("跑 mora");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (s, out.status.code().unwrap_or(-1))
}

/// 录一份，返回录制事件数（用于确认「确实录到了」而不是空文件对空文件）。
fn record(dir: &Path, script: &Path, name: &str) -> usize {
    let (out, code) = mora(dir, &["record", script.to_str().unwrap(), name]);
    assert_eq!(code, 0, "录制 {} 应成功: {}", name, out);
    // "✓ recorded N events -> …"
    let at = out.find("recorded ").expect("应有 recorded 输出") + "recorded ".len();
    let digits: String = out[at..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().expect("应能解析出事件数")
}

/// 从 `summary: identical=N changed=M …` 里取 `changed=` 的数字。
fn changed_count(diff_output: &str) -> i64 {
    let at = diff_output
        .find("changed=")
        .unwrap_or_else(|| panic!("diff 输出里没有 summary: {}", diff_output));
    let rest = &diff_output[at + "changed=".len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits
        .parse()
        .unwrap_or_else(|_| panic!("应能解析 changed 的数字: {}", diff_output))
}

/// **反向对照（有牙齿的那条）**：同一变量名、值不同 → diff 必须报出差异。
///
/// 缺陷存在时这里是 `changed=0`，即测试会红。
#[test]
fn d176_diff_detects_state_mutation_value_change() {
    let dir = WorkDir::new("value");
    let a = dir.script("s1.mora", "let score = 42\nprint(score)\n");
    let b = dir.script("s2.mora", "let score = 99999\nprint(score)\n");

    // 前置：两份都**确实录到了事件**（否则可能是空文件对空文件，比对无意义）。
    assert!(record(dir.path(), &a, "d_a") > 0, "A 应录到事件");
    assert!(record(dir.path(), &b, "d_b") > 0, "B 应录到事件");

    let (out, _) = mora(dir.path(), &["diff", "d_a", "d_b"]);
    assert!(
        changed_count(&out) >= 1,
        "同一变量名 score 的值从 42 变成 99999，diff 必须报出差异 —— \
         实得 changed=0: {}",
        out
    );
    // 差异还必须**看得见具体值**，不能只是个「有差异」的空壳结论。
    assert!(
        out.contains("99999") && out.contains("42"),
        "diff 输出应同时呈现两边的实际取值: {}",
        out
    );
}

/// **正对照**：同一脚本录两次 → 必须 `changed=0`。
///
/// 守的是「修法没有引入误报」—— 若把摘要改成常量字符串，①会红、这条也会红。
#[test]
fn d176_diff_identical_runs_stay_identical() {
    let dir = WorkDir::new("identical");
    let a = dir.script("s1.mora", "let score = 42\nprint(score)\n");
    let b = dir.script("s2.mora", "let score = 42\nprint(score)\n");

    assert!(record(dir.path(), &a, "d_a") > 0);
    assert!(record(dir.path(), &b, "d_b") > 0);

    let (out, _) = mora(dir.path(), &["diff", "d_a", "d_b"]);
    assert_eq!(
        changed_count(&out),
        0,
        "完全相同的两次运行不该报出差异（修法不得引入误报）: {}",
        out
    );
}

/// 事件数不同（一边多出事件）时也必须报出来 —— 走的是 `OnlyInA`/`OnlyInB`
/// 分支，**不是** `Changed` 分支。
///
/// ⚠ 这里**不能**断言 `changed >= 1`：多出来的事件被归入 `only_in_b`，
/// `changed=0` 是**正确**的。断言 `changed` 会把这个分支误判成缺陷 ——
/// 我第一版就是这么写错的（测试红而产品没错），判据跟着产品行为的错处走。
///
/// 顺带记下一个**真陷阱**（属报告项，未擅自改 CLI 输出格式）：
/// summary 的 `changed=` 列**只统计 `Changed` 行**，不含 `only_in_a/b`。
/// 「A 有 2 条、B 有 6 条」时是 `changed=0 only_in_b=4` ——
/// CI 脚本若只 grep `changed=`，会把「多了 4 个事件」判成「无变化」。
/// 本测试因此断言「**非 identical 的行必须逐条出现**」，而不是盯某一列。
#[test]
fn d176_diff_detects_added_events() {
    let dir = WorkDir::new("unequal");
    let one = dir.script("s1.mora", "let a = 1\nprint(a)\n");
    let two = dir.script("s2.mora", "let a = 1\nlet b = 2\nlet c = 3\nprint(c)\n");

    let n1 = record(dir.path(), &one, "d_a");
    let n2 = record(dir.path(), &two, "d_b");
    assert_ne!(n1, n2, "前提：两份的事件数应不同; 实得 {n1} vs {n2}");

    let (out, _) = mora(dir.path(), &["diff", "d_a", "d_b"]);
    let added = out
        .lines()
        .filter(|l| l.trim_start().starts_with("+ [#"))
        .count();
    assert_eq!(
        added,
        n2 - n1,
        "B 比 A 多 {} 个事件，diff 应逐条列出; 实得 {} 行:\n{}",
        n2 - n1,
        added,
        out
    );
    assert!(
        out.contains(&format!("only_in_d_b={}", n2 - n1)),
        "摘要的 only_in_d_b 应等于多出的事件数 {}:\n{}",
        n2 - n1,
        out
    );
}
