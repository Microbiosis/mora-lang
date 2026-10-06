//! v0.104.6 D327：`range(n)` **单参形态**让 `for` 循环体完全跳过（已修）。
//!
//! ## 现象（修前）
//!
//! ```text
//! let t = 0
//! for i in range(5)
//!   t = t + i
//! end
//! print(t)                     →  0.0      （期望 10）
//!
//! for i in range(3) { print(i) } →  一次都不打印
//! len(range(5))                 →  0
//! ```
//!
//! **退出码 0、零诊断** —— 与 D312 / D313 / D315 同族的「静默失败」，
//! 且比那几条更基础：`for i in range(n)` 是最常见的循环写法。
//!
//! ## 根因
//!
//! `builtin_impls.rs::call_builtin_range`：
//!
//! ```text
//! start = args.first()          // 单参时 = n
//! end   = args.get(1) ?? start  // 单参时 = n
//! ```
//!
//! ⇒ 单参时 **start == end** ⇒ `while i < end` 恒假 ⇒ 静默返回空列表。
//!
//! ## 查证（吸取 D320 教训：**肯定断言也需要自己的验证**）
//!
//! | 出处 | 内容 |
//! |---|---|
//! | `docs/mora-spec.md` | **`range` 零命中** —— 规范从未定义它 |
//! | 既有判据 | **全部**用多参形态（`range(0,4)` / `range(0,n,1)` / `range(3,0,-1)` / `range(0,5,0)`），**无一条覆盖单参** |
//! | `builtin_return_types.rs:153` | 「range 声明 **3 参**却常被 2 参调用」⇒ 签名是 3 参，1 参落在签名之外 |
//!
//! ⇒ 单参是**未文档化、未钉住的漏掉方向**（与 D325 的 `reshape` 截断同形）。
//! Python / JS / Rust 的 `range(n)` 均为 `[0, n)`。
//!
//! ## 修法
//!
//! 单参时 `start = 0`、`end = n`。**多参形态逐字未变**，负步长分支（D- 轮已修）
//! 未动。既有 6 个相关测试目标 65 条全绿。

use std::process::Command;

fn run(src: &str, tag: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("mora_d327_{}", slug(tag)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, src).expect("写探针");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe).arg(&p).output().expect("跑 mora");
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&out.stdout).into_owned()
        + "\n"
        + &String::from_utf8_lossy(&out.stderr);
    let all: Vec<String> = text
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
                && !l.starts_with('⚠')
                && !l.starts_with("[9layer]")
        })
        .map(|l| {
            l.replace(&p.to_string_lossy().to_string(), "<TMP>")
                .to_string()
        })
        .collect();
    (out.status.code().unwrap_or(-1), all.join(" | "))
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn pr(e: &str) -> (i32, String) {
    run(&format!("print({e})\n"), e)
}

/// **主判据**：单参 `range(n)` 必须等价于 `range(0, n)`。
///
/// **反向牙齿已验证**：把 `1 => (0, as_i64(&args[0], "end")…)` 改回
/// `end = args.get(1) ?? start` 即红。
#[test]
fn d327_single_arg_range_means_zero_to_n() {
    for (e, want) in [
        ("range(3)", "[0.0, 1.0, 2.0]"),
        ("range(1)", "[0.0]"),
        ("range(5)", "[0.0, 1.0, 2.0, 3.0, 4.0]"),
        ("range(0)", "[]"),
        ("range(-1)", "[]"),
        // 浮点上界向零截断（2.9 → 2），与 `as_i64` 一致
        ("range(2.9)", "[0.0, 1.0]"),
        // `2i` / `2n` 上界同样接受
        ("range(3i)", "[0.0, 1.0, 2.0]"),
        ("range(3n)", "[0.0, 1.0, 2.0]"),
    ] {
        let (code, got) = pr(e);
        assert_eq!(code, 0, "`{e}` 应成功; 实得 exit={code} out={got}");
        assert_eq!(
            got, want,
            "`print({e})` 应得 `{want}`（单参 = `range(0, n)`）; 实得 `{got}`\n\
             修前单参时 start == end ⇒ 静默返回 `[]`，导致 `for i in range(n)` \
             的**循环体一次都不执行**。"
        );
    }
}

/// **可观察后果**：`for i in range(n)` 的循环体必须真的执行 n 次。
///
/// 这一条不检查 `range` 本身，只检查**缺陷的实际后果**，
/// 因此对任何修法都成立。
#[test]
fn d327_for_loop_over_single_arg_range_actually_runs() {
    let (code, got) = run(
        "let t = 0\nfor i in range(5)\n  t = t + i\nend\nprint(t)\n",
        "for_sum",
    );
    assert_eq!(code, 0, "for 累加应成功; 实得 {got}");
    assert_eq!(
        got, "10.0",
        "`for i in range(5) {{ t = t + i }}` 之后 t 必须是 **10.0**。\n\
         修前循环体一次都不执行、t 恒为 0.0，**退出码仍是 0、零诊断**。"
    );

    let (code, got) = run(
        "for i in range(3)\n  print(i)\nend\nprint(\"after\")\n",
        "for_print",
    );
    assert_eq!(code, 0, "for 打印应成功; 实得 {got}");
    assert_eq!(
        got, "0.0 | 1.0 | 2.0 | after",
        "`for i in range(3) {{ print(i) }}` 必须打印 0/1/2 再打印 after; 实得 `{got}`"
    );

    let (code, got) = pr("len(range(5))");
    assert_eq!(code, 0, "len(range(5)) 应成功; 实得 {got}");
    assert_eq!(got, "5", "`len(range(5))` 必须是 **5**（修前是 0）");
}

/// **多参形态必须逐字未变**（本修复只动单参）。
#[test]
fn d327_multi_arg_range_unchanged() {
    for (e, want) in [
        ("range(0, 3)", "[0.0, 1.0, 2.0]"),
        ("range(1, 3)", "[1.0, 2.0]"),
        ("range(0, 6, 2)", "[0.0, 2.0, 4.0]"),
        ("range(-1, 3)", "[-1.0, 0.0, 1.0, 2.0]"),
        ("range(3, 1)", "[]"),
        ("range(1, 2, 3)", "[1.0]"),
        // 负步长（D- 轮已修）
        ("range(3, 0, -1)", "[3.0, 2.0, 1.0]"),
        ("range(0, 3, -1)", "[]"),
        ("range(3, 3)", "[]"),
        ("range(0, 0)", "[]"),
    ] {
        let (code, got) = pr(e);
        assert_eq!(code, 0, "`{e}` 应成功; 实得 exit={code} out={got}");
        assert_eq!(
            got, want,
            "`print({e})` 应得 `{want}`; 实得 `{got}`\n\
             ⚠ D327 只动单参形态 —— 若本条失败，说明有人改动了多参语义。"
        );
    }
    // 步长为 0 仍报错（既有契约）
    let (code, got) = pr("range(0, 5, 0)");
    assert_ne!(code, 0, "`range(0,5,0)` 步长为 0 必须报错（会永不终止）");
    assert!(
        got.contains("step must not be 0"),
        "报错应指明 step; 实得: {got}"
    );
}
