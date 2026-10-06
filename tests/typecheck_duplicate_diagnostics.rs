//! v0.104.6 D188：`mora --check` 把**同一个类型错误报两遍**，错误计数撒谎（已修）。
//!
//! ## 缺陷
//!
//! `let v: Int = 1.5` 修前的真实输出：
//!
//! ```text
//! Type error at line 1:14: type mismatch: expected `Int`, got `Float`
//!   expected: Int
//!   actual:   Float
//! Type error (位置未跟踪): Type mismatch: expected int, got float
//!   expected: int
//!   actual:   float
//! 2 type error(s) found.          ← 实际只有 1 个问题
//! ```
//!
//! 用户被告知有 **2 个**类型错误，实际只有 **1 个**。**错误计数是错的。**
//!
//! ## 根因：D128 的三元组去重对这条**失效**
//!
//! `check_program_witnesses_bidirectional` 跑两层：双向层 + HM 层，
//! D128 加了 `(line, expected, actual)` 三元组去重。但**两个**维度都不同：
//!
//! | | 位置 | `expected` / `actual` 文本 |
//! |---|---|---|
//! | 双向层 | `line 1:14` | `` `Int` `` / `` `Float` `` |
//! | HM 层 | `line 0` → 渲染成「位置未跟踪」 | `int` / `float` |
//!
//! - **位置不同**：HM 那条 `line == 0`。D126 已把「line 0」诚实渲染成
//!   「位置未跟踪」（`Constraint` 不携带 span 是**已记的**根因，D126 待办）。
//! - **措辞不同**：同一件事，两层用了两种写法（反引号 + 大写 vs 纯小写）。
//!
//! ## 修法：只对**无位置**的 HM 错误补一条按**规范化类型对**的去重
//!
//! 规范化 = 去反引号 + 转小写。
//!
//! **只影响本来就没有位置信息的那些** —— 那类错误（D126 判定为低信息）
//! 若描述的类型对已被一条**带位置**的诊断覆盖，就是同一件事的第二份表述。
//! 报两遍只会让错误计数撒谎。
//!
//! **不过度去重**（本文件的后两条判据专门守这个）：
//! 同一行/不同行的**不同**类型对仍各自保留；**带位置**的同类型对也各自保留
//! （`let a: Int = 1.5` 与 `let c: Int = 2.5` 必须报 2 个）。

use std::path::{Path, PathBuf};
use std::process::Command;

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d188_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建目录");
        WorkDir(d)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn script(&self, name: &str, body: &str) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, body).expect("写脚本");
        p
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn check(dir: &Path, file: &Path) -> (String, i32) {
    let out = Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/target/debug/mora.exe"
    ))
    .current_dir(dir)
    .arg("--check")
    .arg(file)
    .env_remove("OPENAI_API_KEY")
    .env_remove("MORA_AI_BASE_URL")
    .output()
    .expect("跑 mora --check");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (s, out.status.code().unwrap_or(-1))
}

/// 从 `N type error(s) found.` 里取 N。
fn reported_count(out: &str) -> Option<usize> {
    let at = out.find(" type error(s) found.")?;
    let head = &out[..at];
    let digits: String = head
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.chars().rev().collect::<String>().parse().ok()
}

/// 报出的 `Type error` 条数（按 `expected:` 行数算更稳，避免被标题行误计）。
fn diagnostics_count(out: &str) -> usize {
    out.lines()
        .filter(|l| l.trim_start().starts_with("Type error"))
        .count()
}

/// **主判据（有牙齿）**：**一个**类型错误只能报**一次**，计数必须是 1。
///
/// 修前：`2 type error(s) found.` 且两条诊断讲同一件事。
#[test]
fn d188_a_single_type_error_is_reported_once() {
    let dir = WorkDir::new("single");
    let f = dir.script("a.mora", "let v: Int = 1.5\nprint(v)\n");

    let (out, code) = check(dir.path(), &f);
    assert_eq!(code, 2, "有类型错误应 exit 2:\n{}", out);
    assert_eq!(
        reported_count(&out),
        Some(1),
        "一个类型错误被报成 N 个 —— 错误计数撒谎:\n{}",
        out
    );
    assert_eq!(diagnostics_count(&out), 1, "只应有一条诊断:\n{}", out);
    // 保留下来的是**带位置**的那条（HM 那条 `line==0` 的被滤掉）。
    assert!(out.contains("line 1:"), "应保留带精确位置的诊断:\n{}", out);
    assert!(
        !out.contains("位置未跟踪"),
        "重复的「位置未跟踪」那条应被去重:\n{}",
        out
    );
}

/// **不过度去重**：两个**不同**的类型对必须各报一次。
#[test]
fn d188_distinct_type_conflicts_are_all_reported() {
    let dir = WorkDir::new("distinct");
    let f = dir.script(
        "b.mora",
        "let a: Int = 1.5\nlet b: String = 2.0\nprint(a)\nprint(b)\n",
    );

    let (out, code) = check(dir.path(), &f);
    assert_eq!(code, 2, "有两个类型错误应 exit 2:\n{}", out);
    assert_eq!(
        reported_count(&out),
        Some(2),
        "两个**不同**的冲突必须都报出:\n{}",
        out
    );
    assert!(out.contains("line 1:"), "应含第 1 行的诊断:\n{}", out);
    assert!(out.contains("line 2:"), "应含第 2 行的诊断:\n{}", out);
}

/// **不过度去重**：**同一类型对**出现在两个**不同位置**（都带位置）时，
/// 必须各报一次 —— 那是两处独立的错误。
#[test]
fn d188_same_pair_at_two_positions_reports_both() {
    let dir = WorkDir::new("samepair");
    let f = dir.script("c.mora", "let a: Int = 1.5\nlet c: Int = 2.5\nprint(1)\n");

    let (out, code) = check(dir.path(), &f);
    assert_eq!(code, 2, "两处错误应 exit 2:\n{}", out);
    assert_eq!(
        reported_count(&out),
        Some(2),
        "同一类型对的两处独立冲突必须都报出:\n{}",
        out
    );
    assert!(
        out.contains("line 1:") && out.contains("line 2:"),
        "两处位置都应出现:\n{}",
        out
    );
}

/// **负对照**：合法程序仍应通过（去重不能吞掉真实错误，也不能造出错误）。
#[test]
fn d188_clean_program_still_passes() {
    let dir = WorkDir::new("clean");
    let f = dir.script(
        "d.mora",
        "let t = 0\nfor i in [1,2,3]\n  t = t + i\nend\nprint(t)\n",
    );

    let (out, code) = check(dir.path(), &f);
    assert_eq!(code, 0, "合法程序应 exit 0:\n{}", out);
    assert!(
        out.contains("No type errors found"),
        "应报告无类型错误:\n{}",
        out
    );
    assert_eq!(reported_count(&out), None, "不应出现错误计数行:\n{}", out);
}
