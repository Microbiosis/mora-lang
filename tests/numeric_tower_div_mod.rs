//! v0.104.6 D319：`BigInt ⊗ Int` 的**除零 / 模零**是 Rust **panic**，不是 Mora 错误。
//!
//! ## 现象（修前）
//!
//! ```text
//! print(7i / 0i)   →  Runtime error (MIR): division by zero   exit 1    ✅
//! print(7n / 0n)   →  Runtime error (MIR): division by zero   exit 1    ✅
//! print(7n / 0i)   →  thread 'mora-main' panicked at
//!                     num-bigint-0.4.8/src/biguint/div.rs:
//!                     attempt to divide by zero                 exit 101  ❌
//! print(7i / 0n)   →  同上 panic                                                 ❌
//! ```
//!
//! `%` 的四种组合同理。**Rust panic 逃到用户面前**（还带
//! 「run with `RUST_BACKTRACE=1`」的提示），而同族的整数除零是干净的
//! `MoraError` —— 同一件事，取决于两个操作数**类型不同**。
//!
//! ## 根因
//!
//! `flow.rs::int_div` / `int_mod` 只对 `(Int, Int)` 与 `(BigInt, BigInt)`
//! 两条 arm 做零检查；`Int ⊗ BigInt` / `BigInt ⊗ Int` 落进 `_` arm →
//! `numeric_op`，在那里 Int 被提升成 BigInt 后直接 `a / b`，**绕过了零检查**
//! —— 而 num-bigint 对零除数是 panic 而非返回错误。
//!
//! ## 修法
//!
//! 补上两条混合 arm（`int_div` / `int_mod` 各两条）。**只新增此前会 panic 的
//! 那条路径，不改动任何既有行为** —— 既有 24 项数值结果逐条复验一致
//! （含负数向零截断、符号随被除数、以及 10^20 级别的大数精度）。
//!
//! ## 顺带查清但**不改**的两件事
//!
//! - `7i / 0` → `inf`、`7i % 0` → `nan`、exit **0**（静默）。
//!   这是**自洽**的：Int÷Float 提升为浮点走 IEEE，`7 / 0` 同样是 `inf`；
//!   而 Int÷Int 保持整数语义、才报错。不是缺陷。
//! - `2n == 2i` 被 typeck 拒绝（`expected BigInt, got Int`），而 `2n + 2i`
//!   可以算（→ `4n`）。**算术有提升、比较没有** —— 属**语义缺口**，
//!   修它要同时动 typeck + `Value::PartialEq` + `eval_binary` 三层，
//!   是**架构决定**。本文件把它钉成现状判据。

use std::process::Command;

/// 删除临时目录，**带重试**。
///
/// v0.104.6 D321：`mora.exe` 子进程在 `.output()` 返回后可能**尚未释放**
/// `p.mora` 的文件句柄；Windows 上此时 `remove_dir_all` 直接失败，而
/// `let _ =` 会把错误**静默吞掉** ⇒ 每跑一次判据就漏一个目录
/// （实测 `cargo test --no-fail-fast` 一次 +4 个）。
///
/// Windows 的句柄释放是异步的，重试即可覆盖；仍失败则**如实暴露**，
/// 不再伪装成「清理过了」。
fn cleanup_dir(dir: &std::path::Path) {
    for attempt in 0..8 {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) if attempt == 7 => {
                panic!(
                    "D321：临时目录 {dir:?} 清理失败（{e}）。\n\
                     句柄可能仍被 `mora.exe` 子进程占用；重试 8 次仍失败。\n\
                     该目录会逐次累积 —— 请勿忽略。"
                );
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(25)),
        }
    }
}

fn run(src: &str, tag: &str) -> (i32, Vec<String>) {
    // ⚠ 每条用例必须用**独立**目录：同文件的 `#[test]` 并行执行，
    //   共用目录会互相 remove_dir_all / 写同一个 p.mora（D317 踩过）。
    let dir = std::env::temp_dir().join(format!("mora_d319_num_{}", slug(tag)));
    cleanup_dir(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, src).expect("写探针");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe).arg(&p).output().expect("跑 mora");
    cleanup_dir(&dir);
    let text = String::from_utf8_lossy(&out.stdout).into_owned()
        + "\n"
        + &String::from_utf8_lossy(&out.stderr);
    let lines = text
        .lines()
        .map(|l| l.trim().to_string())
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
        .collect();
    (out.status.code().unwrap_or(-1), lines)
}

fn expr(e: &str) -> String {
    format!("print({e})\n")
}

/// 把用例名变成**合法的 Windows 目录名片段**。
///
/// ⚠ 表达式里含 `(` `)` `-` `>` `/` `%` 等字符，直接拼进 `%TEMP%` 路径
///   会得到 `Os { code: 123, kind: InvalidFilename }`（本轮第一版就这么炸的）。
fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// **主判据**：整数除法 / 取模的**全部 8 种类型组合**遇到零除数，都必须返回
/// `MoraError`（exit 1），**绝不允许 panic**（exit 101）。
///
/// **反向牙齿已验证**：删掉 `int_div` / `int_mod` 里新增的
/// `(Int, BigInt)` / `(BigInt, Int)` 两条 arm → 4 条立刻红（exit 101）。
#[test]
fn d319_integer_div_mod_by_zero_never_panics() {
    for op in ["/", "%"] {
        for (a, b, tag) in [
            ("7i", "0i", "int-int"),
            ("7n", "0n", "big-big"),
            ("7n", "0i", "big-int"),
            ("7i", "0n", "int-big"),
        ] {
            let (code, lines) = run(&expr(&format!("{a} {op} {b}")), &format!("{op}_{tag}"));
            assert_ne!(
                code, 101,
                "**D319**：`print({a} {op} {b})` 触发了 **Rust panic**（exit 101）。\n\
                 整数除零必须返回 `MoraError`（exit 1），与同族的 `7i {op} 0i` / \
                 `7n {op} 0n` 一致。\n  实际输出: {lines:?}"
            );
            assert_eq!(
                code, 1,
                "`print({a} {op} {b})` 应以 exit 1 报运行期错误; 实得 exit={code}\n  输出: {lines:?}"
            );
            assert!(
                lines.iter().any(|l| l.contains("zero")),
                "`print({a} {op} {b})` 的错误信息应指明除零/模零; 实得: {lines:?}"
            );
        }
    }
}

/// **正确性矩阵**：混合类型的除法 / 取模必须给出**正确值**，
/// 且既有的各条路径**不得回归**。
///
/// 期望值约定：向零截断（Rust `i64` 与 num-bigint 同约定）、符号随被除数、
/// BigInt 打印带 `n` 后缀。
#[test]
fn d319_mixed_type_div_mod_results_are_correct() {
    for (e, want) in [
        // 混合类型（修前会 panic 的那条路径，现在必须正常）
        ("7n / 2i", "3n"),
        ("7i / 2n", "3n"),
        ("7n % 2i", "1n"),
        ("7i % 2n", "1n"),
        ("8n / 2i", "4n"),
        ("8i / 2n", "4n"),
        // 负数：向零截断 / 符号随被除数
        ("(-7n) / 2i", "-3n"),
        ("(-7i) / 2n", "-3n"),
        ("(-7n) % 2i", "-1n"),
        ("(-7i) % 2n", "-1n"),
        ("7n / (-2i)", "-3n"),
        ("7n % (-2i)", "1n"),
        // 既有路径不得回归
        ("7i / 2i", "3"),
        ("7n / 2n", "3n"),
        ("7i % 2i", "1"),
        ("7n % 2n", "1n"),
        // Int ⊗ Float 提升为 IEEE 浮点、不截断
        ("7i / 2", "3.5"),
        ("7 / 2i", "3.5"),
        ("7 / 2", "3.5"),
        // 其它 BigInt 运算不受影响
        ("2n + 2i", "4n"),
        ("2i + 2n", "4n"),
        ("2n * 3i", "6n"),
        // 任意精度不得被截到 i64
        ("100000000000000000000n / 3i", "33333333333333333333n"),
        ("100000000000000000000n % 7i", "2n"),
    ] {
        let (code, lines) = run(&expr(e), &format!("val_{}", slug(e)));
        assert_eq!(code, 0, "`{e}` 应成功; 实得 exit={code} out={lines:?}");
        assert_eq!(
            lines.first().map(String::as_str),
            Some(want),
            "`print({e})` 的结果不对; 期望 {want}"
        );
    }
}

/// 浮点除零保持 IEEE 语义（`inf` / `nan`，exit 0）—— **这是自洽的，不是缺陷**。
///
/// 钉住它是为了防止有人「顺手」把浮点除零也改成报错（那会改变语言语义）。
#[test]
fn d319_float_division_by_zero_keeps_ieee_semantics() {
    for (e, want) in [("7 / 0", "inf"), ("7i / 0", "inf"), ("7 / 0.0", "inf")] {
        let (code, lines) = run(&expr(e), &format!("f_{}", slug(e)));
        assert_eq!(code, 0, "`{e}` 应成功（IEEE 语义）; 实得 exit={code}");
        assert_eq!(
            lines.first().map(String::as_str),
            Some(want),
            "`print({e})` 应得 {want}（Int÷Float 提升为浮点）；\
             ⚠ 若本条失败，说明有人把浮点除零改成了报错 —— 那是**语义变更**，\
             请先回到 CHANGELOG D319 记录裁决依据。"
        );
    }
    // `%` 0 得 nan（不是 inf）
    let (code, lines) = run(&expr("7i % 0"), "f_mod");
    assert_eq!(code, 0, "`7i % 0` 应成功; 实得 {code}");
    assert_eq!(
        lines.first().map(String::as_str),
        Some("nan"),
        "`print(7i % 0)` 应得 nan; 实得 {lines:?}"
    );
}

/// **现状判据**：BigInt 与 Int/Float 的**比较**被 typeck 拒绝，而**算术**可以。
///
/// 属**语义缺口**（算术有提升、比较没有），修它要同时动 typeck +
/// `Value::PartialEq` + `eval_binary` 三层 ⇒ **架构决定，只报告不实施**。
/// 本条保证这个缺口**保持可观察**，不被无意改动。
#[test]
fn d319_status_quo_bigint_comparison_is_rejected_but_arithmetic_is_not() {
    // 算术：允许（→ BigInt）
    for e in ["2n + 2i", "2i + 2n", "2n + 2"] {
        let (code, lines) = run(&expr(e), &format!("ar_{}", slug(e)));
        assert_eq!(
            code, 0,
            "`{e}` 算术应被允许; 实得 exit={code} out={lines:?}"
        );
        assert_eq!(
            lines.first().map(String::as_str),
            Some("4n"),
            "`print({e})` 应得 4n（任一含 BigInt ⇒ 结果 BigInt）; 实得 {lines:?}"
        );
    }
    // 比较：拒绝（typeck）
    for e in ["2n == 2i", "2n == 2", "2n < 2i", "2n > 2i"] {
        let (code, lines) = run(&expr(e), &format!("cmp_{}", slug(e)));
        assert_eq!(
            code, 2,
            "**D319 现状**：`{e}` 当前被 typeck 拒绝（BigInt 与 Int/Float 不可比）。\n\
             ⚠ 这是**语义缺口**（算术能提升、比较不能），不是已判定的缺陷。\n\
             若将来裁决为「应可比较」并实现（需同时改 typeck + `Value::PartialEq` \
             + `eval_binary`），本条会红并应翻转为 exit 0。\n  实得: {lines:?}"
        );
    }
    // BigInt 与 BigInt 之间可比较（对照组）
    let (code, lines) = run(&expr("2n == 2n"), "cmp_bb");
    assert_eq!(code, 0, "`2n == 2n` 应成功; 实得 {code}");
    assert_eq!(
        lines.first().map(String::as_str),
        Some("true"),
        "实得 {lines:?}"
    );
}
