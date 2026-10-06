//! v0.104.6 D262 —— `crush_json` 的 `max` 实参必须同时接受 `Float` 与 `Int`。
//!
//! ## 缺陷
//!
//! `call_builtin_crush_json` 只匹配 `Value::Float`：
//!
//! ```text
//!   let max_items = match &args[1] {
//!       Value::Float(n) => { if *n < 0.0 { … } *n as usize }
//!       other => return Err(format!("crush_json: max must be a number, got {}", type_name(other))),
//!   };
//! ```
//!
//! 而本仓数字有**两个来源**（D98 / D129）：字面量给 `Float`，
//! `len()` 等运算给 `Int`。于是：
//!
//! ```text
//! crush_json([1,2,3,4,5], 2.0)   → OK     （字面量是 Float）
//! crush_json([1,2,3,4,5], 2)     → OK     （同上）
//! crush_json(xs, len(xs))         → ERR: "crush_json: max must be a number, got int"
//! ```
//!
//! **「int 明明是数字」** —— 与 D249 的 `with temperature` / `max_tokens`
//! 完全同型，但后果更直接：**用户无法用动态计算出来的上限**，
//! 而 `crush_json` 正是压缩功能的主入口。
//!
//! ## 为什么 `builtin_impls.rs` 里只有这一处
//!
//! 同文件的 `range`（`as_i64`，行 79-95）与 `int()`（行 216-227）都覆盖了
//! `Float` / `Int` / `BigInt` 三个分支，还带 `is_finite` 检查 ⇒ 不是这一带的
//! 系统性疏漏，是**单点遗漏**。判据 ③ 把这个「同族已达标」的现状钉住。
//!
//! ## 修法
//!
//! 改走 D246 立的收口 `flow::value_as_usize`，但**保留两种错误的区分**
//! （D259 教训：收口不该顺手抹掉诊断信息）—— 负数仍报 `non-negative`，
//! 非数值仍报 `must be a number, got <类型名>`。

use std::path::PathBuf;

const NOISE: &[&str] = &[
    "Mora v",
    "AI:",
    "AI ",
    "Built-in",
    "v0.15",
    "⚠",
    "AI 原语",
    "显式 API",
    "Trait",
];

/// `Drop` 守卫：assert 失败时也要清理。
struct WorkDir(PathBuf);
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(dir: &std::path::Path, src: &str) -> (i32, String, String) {
    let f = dir.join("a.mora");
    std::fs::write(&f, src).expect("write");
    let out = std::process::Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/target/debug/mora.exe"
    ))
    .arg("run")
    .arg(&f)
    .output()
    .expect("run mora");
    let keep = |b: &[u8]| -> String {
        String::from_utf8_lossy(b)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !NOISE.iter().any(|n| l.contains(n)))
            .collect::<Vec<_>>()
            .join(" | ")
    };
    (
        out.status.code().unwrap_or(-1),
        keep(&out.stdout),
        keep(&out.stderr),
    )
}

/// ① 主判据：三种「合法上界写法」都必须被接受 —— 尤其**第三种**
/// （`len()` 产 `Int`，修前报 "got int"）。
#[test]
fn d262_crush_json_accepts_float_int_and_len_valued_max() {
    let dir = std::env::temp_dir().join(format!("mora_d262_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let _work = WorkDir(dir.clone());

    for (label, src) in [
        ("float literal", "crush_json([1,2,3,4,5], 2.0)\n"),
        (
            "bare literal (also Float, D98)",
            "crush_json([1,2,3,4,5], 2)\n",
        ),
        (
            "len() -> Int",
            "let xs = [1,2,3,4,5]\ncrush_json(xs, len(xs))\n",
        ),
    ] {
        let (code, _, err) = run(&dir, src);
        assert_eq!(
            code, 0,
            "{label}: 合法上界被拒 —— 修前 `len(xs)` 会报 \
             「crush_json: max must be a number, got int」。stderr: {err}"
        );
    }
}

/// ② 不回归：两种错误必须**各自保持可区分**（D259 教训）。
#[test]
fn d262_crush_json_keeps_the_two_errors_distinguishable() {
    let dir = std::env::temp_dir().join(format!("mora_d262e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let _work = WorkDir(dir.clone());

    // 负数：报「非负」而不是「不是数字」
    for src in ["crush_json([1,2,3], -1)\n", "crush_json([1,2,3], -1.0)\n"] {
        let (code, _, err) = run(&dir, src);
        assert_ne!(code, 0, "负数上界应被拒：{err}");
        assert!(
            err.contains("non-negative"),
            "负数应报 non-negative，实得：{err}"
        );
    }
    // 非数值：报「不是数字」并**点名类型**
    let (code, _, err) = run(&dir, "crush_json([1,2,3], \"two\")\n");
    assert_ne!(code, 0, "字符串上界应被拒：{err}");
    assert!(
        err.contains("must be a number") && err.contains("string"),
        "非数值应报 `must be a number, got string`，实得：{err}"
    );
}

/// ③ 对照组：同文件的 `range` / `int()` 已覆盖 Int/Float/BigInt
/// —— 钉住「crush_json 是单点遗漏，不是这一带的系统性疏漏」。
#[test]
fn d262_control_group_siblings_already_accept_both_number_kinds() {
    let dir = std::env::temp_dir().join(format!("mora_d262c_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let _work = WorkDir(dir.clone());

    for (label, src) in [
        ("range(1, 3)", "print(len(range(1, 3)))\n"),
        ("int(len(...))", "print(int(len(range(1, 3))))\n"),
    ] {
        let (code, _, err) = run(&dir, src);
        assert_eq!(code, 0, "{label} 应正常工作（对照组失效）：{err}");
    }
}
