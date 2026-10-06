//! v0.104.6 D359 —— **Float ⊗ BigInt 混算在 float 超出 2^53 时静默算错**（已修）
//!
//! D358 修了 `is_truthy` 漏 BigInt 分支后，本轮扫 `src/flow.rs` 的
//! 数值混算面。`eval_binary`（282 行）+ `numeric_op`（Sub/Mul/Div/Mod）
//! 合计 8 个运算符 × Float/BigInt 组合，翻出**一个静默错值**缺陷。
//!
//! ## 缺陷：`f - 1n` 的 `1` 凭空消失
//!
//! ```text
//! let f = 100000000000000000000     // 1e20，Float
//! print(f + 1n)   →  100000000000000000000.0   ← 加的 1 消失
//! print(f - 1n)   →  100000000000000000000.0   ← 减的 1 消失
//! print(1n - f)   →  -100000000000000000000.0  ← 符号被吞
//! print(f * 2n)   →  200000000000000000000.0   ← 量级对、精度错
//! print(2n / f)   →  0.00000000000000000002
//! ```
//!
//! **全部不报错、不警告。** `f * 2n` 尤其危险：量级正确，
//! 低位精度丢失，肉眼几乎无法察觉。
//!
//! ## 根因：**同一个类型对，两个运算符，两种行为**
//!
//! - `Add` 走 `eval_binary` → `coerce_mixed`，其中**有**往返校验
//!   （`BigInt::from(bf as i128) != b` ⇒ 报错），
//!   所以 `1e20 + 99999999999999999999n` 明确报错；
//! - Sub/Mul/Div/Mod 走 `numeric_op` → `bigint_to_f64_lossy`，
//!   **无任何守卫**，BigInt 侧无条件降级成 f64。
//!
//! 而 `coerce_mixed` 自己的守卫**只查 BigInt 侧**，
//! 完全没查 float 侧 ⇒ `1e20 + 1n`（BigInt 侧极小）也静默。
//!
//! **静默错 + 明确报错并存，是最坏的组合** ——
//! 用户会误以为「BigInt 混 Float 已经有保护了」。
//!
//! ## 判据
//!
//! f 是**整数值**（`f.fract() == 0.0`）但 `|f| > 2^53`
//! ⇒ 该整数 **f64 装不下** ⇒ 无论 BigInt 侧多小，结果都无法精确表示 ⇒ 报错。
//!
//! 抽成共用函数 `check_float_exact_int`，`coerce_mixed` 与 `numeric_op`
//! **共用同一判据**，避免再分叉。
//!
//! ## 边界是精确的
//!
//! `2^53` **本身仍精确**（`9007199254740992 + 1n` = `9007199254740993n`），
//! 拦的是 `> 2^53` 而非 `>= 2^53`。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d359_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d359_{}_{}", n, slug(body)));
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, body).expect("写探针");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("建 home");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe)
        .arg(&p)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .expect("跑 mora");
    let _ = std::fs::remove_dir_all(&dir);
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let path_str = p.to_string_lossy().into_owned();
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
                && !l.starts_with('⚠')
                && !l.starts_with("[9layer]")
                && !is_bare_path_line(l, &path_str)
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

fn is_bare_path_line(line: &&str, path: &str) -> bool {
    **line == *path
}

/// **装置自检**（D356 教训：先证装置有效，再看它测出的数据）。
#[test]
fn d359_harness_collects_print_output() {
    let (code, got) = ev("print(1)\n");
    assert_eq!(code, 0, "探针应正常退出; 实得 exit={code} out=[{got}]");
    assert_eq!(got.trim(), "1.0", "采集器失效（本文件全部断言依赖它）");
}

/// **主断言**：**全部 8 个** Float ⊗ BigInt 组合在 float 超 2^53 时必须报错。
///
/// 修前只有 `+` 的**部分**形态报错，Sub/Mul/Div/Mod **全部**静默错。
/// 逐个写死是为了防止将来只修到某一个运算符。
#[test]
fn d359_all_eight_mixed_operators_reject_out_of_range_float() {
    for expr in [
        "f + 1n", "1n + f", "f - 1n", "1n - f", "f * 2n", "2n * f", "f / 2n", "2n / f",
    ] {
        let body = format!("let f = 100000000000000000000\nprint({expr})\n");
        let (code, got) = ev(&body);
        assert_eq!(
            code, 1,
            "`{expr}` 的 float 侧超 2^53，必须报错（修前静默算错）; 实得 exit={code} out={got}"
        );
        // 只钉「错误类型 + 关键信息」，不钉完整措辞。
        assert!(
            got.contains("f64") && got.contains("bigint"),
            "`{expr}` 的诊断应说明是 f64 精度问题; 实得: {got}"
        );
    }
}

/// **`f * 2n` 单独钉住** —— 它是修前**最隐蔽**的一个：
/// 量级对（2e20）、低位丢，肉眼几乎看不出错。
#[test]
fn d359_multiplication_by_out_of_range_float_is_rejected() {
    let (code, got) = ev("let f = 100000000000000000000\nprint(f * 2n)\n");
    assert_eq!(
        code, 1,
        "`1e20 * 2n` 修前静默返回 200000000000000000000.0（精度已损）; 实得 exit={code} out={got}"
    );
    assert!(
        !got.contains("200000000000000000000"),
        "绝不能返回那个量级正确、精度错误的值"
    );
}

/// **2^53 边界仍精确** —— 上条的反向对照，防「一律拒绝」。
///
/// `2^53` 是 f64 能精确表示的最大整数，`2^53 + 1` 也能精确，
/// 所以 `9007199254740992 + 1n` 必须得出 `9007199254740993n`。
#[test]
fn d359_two_to_the_53_boundary_is_still_exact() {
    let (code, got) = ev("let f = 9007199254740992\nprint(f + 1n)\n");
    assert_eq!(code, 0, "2^53 应仍精确; 实得 exit={code} out={got}");
    assert_eq!(
        got.trim(),
        "9007199254740993n",
        "2^53 + 1n 必须精确; 实得: {got}"
    );

    let (code, got) = ev("let f = 9007199254740991\nprint(f + 1n)\n");
    assert_eq!(code, 0, "2^53-1 应精确; 实得 exit={code} out={got}");
    assert_eq!(got.trim(), "9007199254740992n", "实得: {got}");
}

/// **正常范围内的混算必须照常工作** —— 修法不能把合法的混算也拒了。
#[test]
fn d359_in_range_mixed_arithmetic_still_works() {
    for (body, expected) in [
        ("print(1.5 + 1n)\n", "2.5"),
        ("print(3 - 1n)\n", "2.0"),
        ("print(1.5 * 2n)\n", "3.0"),
        ("print(3.0 / 2n)\n", "1.5"),
        ("print(3 + 1n)\n", "4n"),
        // 整数值 float 提升为 BigInt（D198 的「最小惊讶」规则）⇒ 结果 BigInt。
        // 首版这里写期望 `200.0`，实测是 `200n` —— 期望写错，不是产品错。
        ("print(100.0 + 100n)\n", "200n"),
    ] {
        let (code, got) = ev(body);
        assert_eq!(code, 0, "`{body}` 应正常; 实得 exit={code} out={got}");
        assert_eq!(got.trim(), expected, "`{body}` 的值不对; 实得: {got}");
    }
}

/// **纯同类型运算完全不受影响** —— Float⊗Float / BigInt⊗BigInt / Int⊗Int。
#[test]
fn d359_same_type_arithmetic_is_untouched() {
    for (body, expected) in [
        ("print(1.5 - 0.5)\n", "1.0"),
        ("print(2.0 * 3.0)\n", "6.0"),
        ("print(1.0 / 4.0)\n", "0.25"),
        ("print(10n - 1n)\n", "9n"),
        ("print(10n * 10n)\n", "100n"),
        ("print(10n / 3n)\n", "3n"),
        ("print(10 - 1)\n", "9.0"),
        ("print(10 * 3)\n", "30.0"),
    ] {
        let (code, got) = ev(body);
        assert_eq!(code, 0, "`{body}` 应正常; 实得 exit={code} out={got}");
        assert_eq!(got.trim(), expected, "`{body}` 的值被本轮修复改动了");
    }
}

/// **BigInt 侧超范围时仍按既有契约报错** —— D198 的行为不得回退。
#[test]
fn d359_out_of_range_bigint_side_still_errors() {
    for expr in ["f + 99999999999999999999n", "f - 99999999999999999999n"] {
        let body = format!("let f = 100000000000000000000\nprint({expr})\n");
        let (code, got) = ev(&body);
        assert_eq!(
            code, 1,
            "`{expr}` 的 BigInt 侧超范围应报错（D198 契约）; 实得 exit={code} out={got}"
        );
    }
}

/// **除零守卫不得被本轮修复放松**。
#[test]
fn d359_division_by_zero_still_errors() {
    let (code, got) = ev("print(10n / 0n)\n");
    assert_eq!(code, 1, "BigInt 除零必须报错; 实得 exit={code} out={got}");
}
