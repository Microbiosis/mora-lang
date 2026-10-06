//! v0.104.6 D358 —— `src/flow.rs::is_truthy` 的 **`BigInt(0)` 被判为 truthy**（已修）
//!
//! `is_truthy` 是全语言真值语义的**唯一权威**（`flow.rs:17`），
//! 11 个调用点（`filter` / 分支 / pregel 条件 / DAG 跳转）全部走它。
//! 函数头注释写着「已收敛为单一实现」—— 本轮**实测确认这句是真的**
//! （全仓 `grep` 无第二份定义，零分叉）。
//!
//! ## 缺陷：同一个「零」，三种数值类型里 BigInt 独异
//!
//! ```text
//! [0i, 1i, 2i].filter(fn(x) x end)   →  [1, 2]        ✅ 0 被剔除
//! [0.0, 1.0, 2.0].filter(fn(x) x end) →  [1.0, 2.0]    ✅ 0 被剔除
//! [0n, 1n, 2n].filter(fn(x) x end)   →  [0n, 1n, 2n]  ❌ 0 **被保留**
//! ```
//!
//! ## 根因
//!
//! `is_truthy` 的 `match` 覆盖 `Int` / `Float` / `String` / `List` / `Dict`，
//! **唯独漏了 `BigInt`** ⇒ 落进兜底分支 `_ => true` ⇒ 恒真。
//!
//! ## 为什么是缺陷（不是「BigInt 另有语义」）
//!
//! ① **同函数内的类型一致性**：`Int(0)` 与 `Float(0.0)` 都 falsy，
//!    三者同为「数值零」却行为不同 —— 这是**漏写**，不是设计。
//! ② `Value::BigInt` 就在 enum 里，`is_truthy` 显式列了它**上面**的
//!    `Int` / `Float`，却跳过它 —— 不可能是「有意不处理」。
//! ③ 危害具体：任何以值为条件的 `filter` / 分支，
//!    对 BigInt 列表会把「零」当「非零」处理。
//!
//! ## 修法
//!
//! 补一条 `Value::BigInt(b) => *b != BigInt::from(0)`。
//!
//! ## 顺带查明、**不擅动**的两处
//!
//! | 现象 | 判定 |
//! |---|---|
//! | `bool('\0')` → `true` | 兜底分支的**语义选择**（NUL 是否算空？），无契约，**只报告** |
//! | 条件位置 typeck 强制 `Bool` | ✅ 正确（`if z` z 是数值直接被拒），`is_truthy` 在条件位只接 Bool |

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d358_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d358_{}_{}", n, slug(body)));
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
fn d358_harness_collects_print_output() {
    let (code, got) = ev("print(1)\n");
    assert_eq!(code, 0, "探针应正常退出; 实得 exit={code} out=[{got}]");
    assert_eq!(got.trim(), "1.0", "采集器失效（本文件全部断言依赖它）");
}

/// **主断言**：三种数值类型的 `0` 在 `filter` 里必须**行为一致**。
///
/// 用 `filter` 而非 `bool()`，是因为它是**用户最容易撞上**的路径
/// （谓词直接返回元素值，而 typeck 允许 Bool 以外的值过 `filter`）。
///
/// 修前：`[0n, 1n, 2n]` 原样返回（0 保留），另两个类型都剔除 0。
#[test]
fn d358_zero_is_falsy_for_all_three_numeric_types() {
    let cases = [
        ("Int", "[0i, 1i, 2i].filter(fn(x) x end)", "[1, 2]"),
        ("Float", "[0.0, 1.0, 2.0].filter(fn(x) x end)", "[1.0, 2.0]"),
        ("BigInt", "[0n, 1n, 2n].filter(fn(x) x end)", "[1n, 2n]"),
    ];
    for (name, expr, expected) in cases {
        let (code, got) = ev(&format!("print({expr})\n"));
        assert_eq!(code, 0, "`{name}` 应正常跑; 实得 exit={code} out={got}");
        assert_eq!(
            got.trim(),
            expected,
            "`{name}` 的 0 必须被 filter 剔除（修前 BigInt 的 0 被保留）"
        );
    }
}

/// **`bool()` 直接验真值**：三种数值零一律 falsy，非零一律 truthy。
#[test]
fn d358_bool_coercion_matches_across_numeric_types() {
    for (expr, expected) in [
        ("bool(0n)", "false"),
        ("bool(1n)", "true"),
        ("bool(0i)", "false"),
        ("bool(1i)", "true"),
        ("bool(0.0)", "false"),
        ("bool(1.0)", "true"),
    ] {
        let (code, got) = ev(&format!("print({expr})\n"));
        assert_eq!(code, 0, "`{expr}` 应正常跑; 实得 exit={code} out={got}");
        assert_eq!(
            got.trim(),
            expected,
            "`{expr}` 的真值不对（BigInt 必须与 Int/Float 同语义）"
        );
    }
}

/// **非数值类型的 falsy 现状不得被本轮修复改动**。
///
/// 这是**反向对照**：只验「BigInt 现在对」的话，
/// 一条「把所有 falsy 都改成 truthy」的判据也能全绿。
#[test]
fn d358_other_falsy_values_are_unchanged() {
    for (expr, expected) in [
        ("bool(nil)", "false"),
        ("bool(false)", "false"),
        ("bool(\"\")", "false"),
        ("bool([])", "false"),
        ("bool({})", "false"),
        ("bool(true)", "true"),
        ("bool(\"a\")", "true"),
        ("bool([1])", "true"),
    ] {
        let (code, got) = ev(&format!("print({expr})\n"));
        assert_eq!(code, 0, "`{expr}` 应正常跑; 实得 exit={code} out={got}");
        assert_eq!(got.trim(), expected, "`{expr}` 的真值被本轮修复改动了");
    }
}

/// **BigInt 的其它零值形态**：负零不存在，但大数与极小值仍应 truthy。
#[test]
fn d358_bigint_edge_values() {
    for (expr, expected) in [
        ("bool(0n)", "false"),
        ("bool(-1n)", "true"),
        ("bool(170141183460469231731687303715884105727n)", "true"),
        ("bool(999999999999999999999999999999999999999999n)", "true"),
    ] {
        let (code, got) = ev(&format!("print({expr})\n"));
        assert_eq!(code, 0, "`{expr}` 应正常跑; 实得 exit={code} out={got}");
        assert_eq!(got.trim(), expected, "`{expr}` 的真值不对; 实得: {got}");
    }
}

/// **filter 谓词返回显式 Bool 时，BigInt 与其它类型仍一致**。
///
/// 上两条用「谓词直接返回元素值」，这条用 `x != 0n` 显式比较 ——
/// 防「只有值返回路径被修好了、显式 Bool 路径反而坏了」。
#[test]
fn d358_filter_with_explicit_bool_predicate() {
    for (name, expr, expected) in [
        ("BigInt", "[0n, 5n].filter(fn(x) x != 0n end)", "[5n]"),
        ("Int", "[0i, 5i].filter(fn(x) x != 0i end)", "[5]"),
    ] {
        let (code, got) = ev(&format!("print({expr})\n"));
        assert_eq!(code, 0, "`{name}` 应正常跑; 实得 exit={code} out={got}");
        assert_eq!(got.trim(), expected, "`{name}` 的过滤结果不对; 实得: {got}");
    }
}

/// **条件位置 typeck 强制 Bool** —— 这条钉住「`if 0n` 被拒」是
/// **正确行为**，防止将来有人放宽成隐式真值后无人察觉。
#[test]
fn d358_condition_position_requires_bool() {
    for (expr, what) in [("if 0n", "BigInt"), ("if 0i", "Int")] {
        let body = format!("let z = 1\n{expr}\n  print(1)\nend\n");
        let (code, got) = ev(&body);
        assert_eq!(
            code, 2,
            "`{expr}`（{what}）应被 typeck 拒（条件位只接受 Bool）; 实得 exit={code} out={got}"
        );
    }
}
