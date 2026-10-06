//! v0.104.6 D356 —— 数字字面量的**位宽后缀被当成数值拼接**（已修）
//!
//! `src/lexer.rs` 是整条执行管线的最底层（`ParserV3 → MirExpr → lower
//! → MirInst → dag_interp` 全靠它切 token），此前从未系统测过。
//! 本轮从「数字字面量矩阵」入手，15 个用例翻出**一个真缺陷**。
//!
//! ## 缺陷：`1.5f32` 求值为 **1.532**
//!
//! ```text
//! let x = 1.5f32   →  1.532     ← 1.5 后面把 "32" 当小数位拼进去了
//! let x = 1f32     →  132.0
//! let x = 1i32     →  132
//! ```
//!
//! ## 根因
//!
//! `number_from` 消费位宽时是**追加进数值串**的：
//!
//! ```rust
//! // 修前（src/lexer.rs:706）
//! while self.peek().is_ascii_digit() {
//!     value.push(self.advance());   // ← "32" 进了 value
//! }
//! ```
//!
//! 而 `'f'` 分支是 **`value.parse()`**，**不做**任何截断 ⇒ 位宽被
//! 当成数值的一部分解析。`'i'` / `'u'` / `'n'` 分支恰好用了
//! `take_while(is_ascii_digit)` 才没出事，但那是**巧合**
//! （依赖位宽首字符是 ASCII 数字），不是设计。
//!
//! ## 危害
//!
//! 单精度字面量在真实代码里很常见（图形 / 科学计算 / 跨语言互操作）。
//! `1.5f32` 静默算出 `1.532` —— **不报错、不警告**，结果直接错。
//! 浮点误差还会沿表达式传播，`f32` 越靠后误差越隐蔽。
//!
//! ## 修法
//!
//! 位宽**只被消费、不混进 `value`**：Mora 不建模位宽
//! （`'u'` 分支注释原文：*mora doesn't model unsigned*），
//! 位宽既无类型语义也无运行期语义。
//!
//! ## 顺带钉住的行为（**不改**，只记录现状）
//!
//! | 行为 | 实测 | 是否要改 |
//! |---|---|---|
//! | 非 8/16/32/64 的位宽（`1i7`）| 照常消费，值 = 1 | ✅ 宽松合理（位宽不建模 ⇒ 无所谓）|
//! | `i64` 溢出（`9223372036854775808i`）| exit 2，Invalid integer literal | ✅ 正确 |
//! | `u` 后缀走 `i64` 解析 | `1u` = 1 | ✅ 注释已说明（不建模 unsigned）|

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// 判断某一行是否**只是**探针文件的绝对路径（CLI 偶发单独打印路径）。
///
/// ⚠ 不能用 `line.contains(path)` —— lexer 层诊断是
/// `<绝对路径>: <消息>` **同行**格式，`contains` 会把整条诊断一起滤掉。
/// 症状是「exit≠0 但 `got` 为空」，看着像产品静默失败（D357 首次发现）。
/// typeck 层诊断是**独立行**，所以这个收紧对既有判据无影响。
fn is_bare_path_line(line: &&str, path: &str) -> bool {
    **line == *path
}

fn slug(s: &str) -> String {
    let mut out = String::from("d356_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d356_{}_{}", n, slug(body)));
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
                // ⚠ **只忽略纯路径行**，不能 `contains(p)` ——
                // lexer 层的诊断格式是 `<绝对路径>: <消息>`，**消息与路径同行**；
                // 用 `contains` 会把整条诊断滤掉，症状是「exit≠0 但 got 为空」，
                // 看着像产品没报错（D357 首次发现）。
                // typeck 层的诊断是**独立行**，所以本条对既有判据无影响。
                && !is_bare_path_line(l, &path_str)
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

/// **装置自检**（D352 教训：exit 非 0 + 输出为空时**先怀疑采集器**）。
///
/// 本文件每条断言都建立在「`print` 的输出能被 `ev` 采到」之上。
/// 若这个前提为假，**全部 8 条会一起变红**，而且红得莫名其妙。
///
/// 首版这里写期望 `"42"`，实测是 `"42.0"` —— **mora 的裸数字字面量
/// 是 Float**（`1` 打印成 `1.0`，`1i` 才打印成 `1`）。
/// 装置本身一直是好的，错的是期望值 —— 这正是要先证装置的原因：
/// 「7 条一起红」和「1 条红」指向完全不同的排查方向。
#[test]
fn d356_harness_can_actually_collect_print_output() {
    let (code, got) = ev("print(42)\n");
    assert_eq!(code, 0, "探针应正常退出; 实得 exit={code} out=[{got}]");
    assert_eq!(
        got.trim(),
        "42.0",
        "采集器失效：`print(42)` 的 stdout 没被采到（本文件的断言全部依赖它）"
    );
}

/// 探针 = `let x = <字面量>` + `print(x)`。
///
/// ⚠ **必须带 `print`**：`let` 本身**不产生输出**，只写
/// `let x = 1.5f32` 时 `ev` 返回空串，`got.trim()` 恒为 `""`，
/// 于是 6 条断言一起红且红得莫名其妙（首版就踩了）。
/// 这与 D352 的「exit 非 0 + 输出为空 ⇒ 先怀疑采集器」同源，
/// 只是那次是采集器坏、这次是**探针本身没输出**。
fn lit_probe(lit: &str) -> String {
    format!("let x = {lit}\nprint(x)\n")
}

/// **主断言**：带位宽的浮点字面量必须**只取数值部分**。
///
/// 修前 `1.5f32` → `1.532`（把 "32" 当小数位）。三个宽度全测，
/// 因为修法对「位宽被消费」这条路径统一生效，但**不同宽度下
/// 拼出来的错值不同**（`1f32` → 132 而 `1.5f32` → 1.532），
/// 只测一个会漏掉「整数部分 + 位宽」那种形态。
#[test]
fn d356_width_suffix_is_not_appended_to_the_value() {
    let cases = [
        ("1.5f32", "1.5"),
        ("1f32", "1.0"),
        ("1.5f64", "1.5"),
        ("2.5f16", "2.5"),
        // 非 8/16/32/64 的位宽：位宽不建模 ⇒ 照常消费，值不变
        ("1.5f7", "1.5"),
    ];
    for (lit, expected) in cases {
        let (code, got) = ev(&lit_probe(lit));
        assert_eq!(code, 0, "`{lit}` 应正常求值; 实得 exit={code} out={got}");
        // 精确匹配而不是 contains —— "1.5" 是 "1.532" 的前缀，
        // 用 contains 会让这条判据**恒绿**（第一次就犯过）。
        assert_eq!(
            got.trim(),
            expected,
            "`{lit}` 的位宽不得被拼进数值（修前 1.5f32 → 1.532）"
        );
    }
}

/// **整数位宽**：修前 `1i32` → 132，靠 `take_while` **侥幸**没坏。
///
/// 单独钉住是为了防止将来有人把 `take_while` 去掉（那正是
/// 「巧合当设计」的典型改法 —— 有人会想「既然位宽已单独收集，
/// 这个截断是多余的」）。
#[test]
fn d356_integer_width_suffix_yields_the_bare_value() {
    for (lit, expected) in [
        ("1i7", "1"),
        ("1i8", "1"),
        ("1i16", "1"),
        ("1i32", "1"),
        ("1i64", "1"),
        ("42i32", "42"),
        ("1u32", "1"),
    ] {
        let (code, got) = ev(&lit_probe(lit));
        assert_eq!(code, 0, "`{lit}` 应正常求值; 实得 exit={code} out={got}");
        assert_eq!(
            got.trim(),
            expected,
            "`{lit}` 的位宽不得被拼进数值（修前 1i32 → 132）"
        );
    }
}

/// **BigInt 位宽**：`1n32` 修前也是 132（同一根因的第三条路径）。
#[test]
fn d356_bigint_width_suffix_yields_the_bare_value() {
    for (lit, expected) in [("1n32", "1n"), ("7n8", "7n"), ("1N64", "1n")] {
        let (code, got) = ev(&lit_probe(lit));
        assert_eq!(code, 0, "`{lit}` 应正常求值; 实得 exit={code} out={got}");
        assert_eq!(got.trim(), expected, "`{lit}` 的位宽不得被拼进数值");
    }
}

/// **无位宽的字面量必须完全不受影响** —— 修复不能动到主路径。
#[test]
fn d356_literals_without_width_are_unchanged() {
    for (lit, expected) in [
        ("1", "1.0"),
        ("1.5", "1.5"),
        ("1i", "1"),
        ("1u", "1"),
        ("1f", "1.0"),
        ("1n", "1n"),
        ("0.125", "0.125"),
        ("1000000", "1000000.0"),
    ] {
        let (code, got) = ev(&lit_probe(lit));
        assert_eq!(code, 0, "`{lit}` 应正常求值; 实得 exit={code} out={got}");
        assert_eq!(got.trim(), expected, "`{lit}` 的值被本轮修复改动了");
    }
}

/// **溢出仍必须报错** —— 修法不能顺手把溢出检查也放松掉。
#[test]
fn d356_i64_overflow_still_errors() {
    for lit in [
        "9223372036854775808i",  // i64::MAX + 1
        "99999999999999999999u", // 远超 u64
    ] {
        let (code, got) = ev(&lit_probe(lit));
        assert_eq!(
            code, 2,
            "`{lit}` 溢出 i64 必须报错; 实得 exit={code} out={got}"
        );
        // lexer 的诊断是 `<绝对路径>: <消息>` **同行**格式 ——
        // 这条断言顺带钉住「采集器没有把诊断整行滤掉」。
        // 首版采集器用 `contains(p)` 过滤路径，会把同行诊断一起吃掉，
        // 症状是 exit=2 但 got 为空（看着像产品静默）。
        assert!(
            got.contains("Invalid integer literal") && got.contains(lit),
            "`{lit}` 的诊断应点名这个字面量且说明原因; 实得: [{got}]"
        );
    }
}

/// **i64::MAX 边界仍合法** —— 上条的反向对照，防「一律拒绝」。
#[test]
fn d356_i64_max_still_parses() {
    let (code, got) = ev(&lit_probe("9223372036854775807i"));
    assert_eq!(code, 0, "i64::MAX 应合法; 实得 exit={code} out={got}");
    assert_eq!(got.trim(), "9223372036854775807");
}

/// **负数字面量**：`-` 走一元负号，不走 `number_from` 的位宽路径，
/// 但确认修复没让负数 + 位宽出问题。
#[test]
fn d356_negative_literals_with_width_are_correct() {
    for (lit, expected) in [
        ("-1.5f32", "-1.5"),
        ("-1i32", "-1"),
        ("-42.25", "-42.25"),
        ("-1.5f7", "-1.5"),
    ] {
        let (code, got) = ev(&lit_probe(lit));
        assert_eq!(code, 0, "`{lit}` 应正常求值; 实得 exit={code} out={got}");
        assert_eq!(got.trim(), expected, "`{lit}` 的值不对");
    }
}
