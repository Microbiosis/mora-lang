//! v0.104.6 D198：`BigInt` 与 `Float` 混合运算**静默产生完全错误的结果**（已修）。
//!
//! ## 缺陷
//!
//! `flow.rs::eval_binary` 的 `Float ⊕ BigInt` 分支原文是「**把 BigInt 转成
//! f64 再算**」：
//!
//! ```text
//! b = 1000000000000000000000000000000n          （10^30）
//! b + 1.0   →  1000000000000000019884624838656.0   ← 前 18 位全错，零提示
//! 1 + b     →  1000000000000000019884624838656.0   ← 同上
//! 12345n+1  →  12346.0                              ← 值对，但**类型**静默退化成 Float
//! ```
//!
//! 而 `Int ⊕ BigInt`（紧邻的两臂）一直是**精确**的 —— 问题**只**出在 float 一侧。
//! `value.rs` 写明的推广规则是「任一含 BigInt 时结果为 BigInt（最小惊讶）」，
//! **实现与文档相反**。
//!
//! `BigInt ⊕ BigInt`（`b+b` / `b-1n` / `b*2n` / `b/2n`）实测**全部正确** ——
//! 所以这不是「BigInt 坏了」，是「**混合**路径坏了」。
//!
//! ## 修法：归一化 + **绝不静默给错数**
//!
//! `coerce_mixed(f, b)` 把一对操作数归一到同一类型：
//!
//! 1. float 侧是**整数值** → `(BigInt, BigInt)`，**精确**（顺带修回类型退化）；
//! 2. float 侧有小数、且 BigInt 能**无损**转 f64（字符串往返相等）→ `(f64, f64)`；
//! 3. 否则 → **报错**。因为「大整数 + 小数」在当前类型系统里**没有精确表示**
//!    （Float 装不下、BigInt 存不了小数）。**宁可报错，不返回一个错的数。**
//!
//! ## 判据
//!
//! ① 大 BigInt 与**整数值**相加必须**逐位**精确、且结果是 `BigInt`（主判据）；
//! ② 大 BigInt 与**小数**相加必须**报错**，不能返回错数（主判据的另一半）；
//! ③ **不回归**：小 BigInt + 小数仍按旧规则给 Float；`BigInt ⊕ BigInt` 不受影响。

use std::path::PathBuf;
use std::process::Command;

struct WorkDir(PathBuf);

impl WorkDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d198_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建目录");
        WorkDir(d)
    }
    fn run(&self, tag: &str, expr: &str) -> (String, i32) {
        let p = self.0.join(format!("{tag}.mora"));
        std::fs::write(&p, format!("print({expr})\n")).expect("写脚本");
        let out = Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/target/debug/mora.exe"
        ))
        .current_dir(&self.0)
        .arg(&p)
        .env_remove("OPENAI_API_KEY")
        .env_remove("MORA_AI_BASE_URL")
        .output()
        .expect("跑 mora");
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        let body = s
            .lines()
            .filter(|l| {
                let t = l.trim();
                !t.is_empty()
                    && !t.starts_with("Mora v")
                    && !t.starts_with("AI:")
                    && !t.starts_with("AI 原语")
                    && !t.starts_with("显式 API")
                    && !t.starts_with("Trait 系统")
                    && !t.starts_with("Built-in")
                    && !t.starts_with("v0.15 CLI")
                    && !t.starts_with('⚠')
            })
            .map(str::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        (body, out.status.code().unwrap_or(-1))
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const BIG: &str = "1000000000000000000000000000000n"; // 10^30

/// **主判据①**：大 BigInt 加**整数值**必须逐位精确，且**结果类型是 BigInt**。
///
/// 修前：`1000000000000000019884624838656.0`（前 18 位全错）或类型退化成 Float。
#[test]
fn d198_bigint_plus_integral_value_is_exact_bigint() {
    let dir = WorkDir::new("exact");
    for (tag, expr, want) in [
        (
            "l",
            &format!("{BIG} + 1"),
            "1000000000000000000000000000001n",
        ),
        (
            "r",
            &format!("1 + {BIG}"),
            "1000000000000000000000000000001n",
        ),
        (
            "f",
            &format!("{BIG} + 1.0"),
            "1000000000000000000000000000001n",
        ),
        (
            "s",
            &format!("{BIG} - 1n"),
            "999999999999999999999999999999n",
        ),
    ] {
        let (out, code) = dir.run(tag, expr);
        assert_eq!(code, 0, "[{tag}] 应成功执行:\n{}", out);
        assert_eq!(
            out.trim(),
            want,
            "[{tag}] `{expr}` 必须**逐位**精确、且结果是 BigInt（带 n 后缀）"
        );
    }
}

/// **主判据②**：大 BigInt 加**小数**必须**报错**，不能返回错数。
///
/// 修前：返回一个与真值差 18 位的 f64，且**零提示**。
#[test]
fn d198_bigint_plus_fraction_errors_instead_of_returning_a_wrong_number() {
    let dir = WorkDir::new("frac");
    for (tag, expr) in [
        ("a", &format!("{BIG} + 0.5")),
        ("b", &format!("0.5 + {BIG}")),
    ] {
        let (out, code) = dir.run(tag, expr);
        assert_ne!(
            code, 0,
            "[{tag}] `{expr}` 无精确表示，必须**报错**而不是返回错数:\n{}",
            out
        );
        assert!(
            out.contains("精确") || out.contains("损坏"),
            "[{tag}] 报错要说清「无法精确表示」:\n{}",
            out
        );
        assert!(
            !out.contains("1000000000000000019884624838656"),
            "[{tag}] 不得再出现修前那个静默的错值:\n{}",
            out
        );
    }
}

/// **不回归**：小 BigInt 与**真小数**混合仍给 Float（此处**确实无损**）。
#[test]
fn d198_small_bigint_mixed_arithmetic_still_works() {
    let dir = WorkDir::new("small");
    // 真正的**小数**（`fract() != 0`）：结果只能是 Float，
    // 而 12345 能无损装进 f64 → 走规则 2。
    let (out, code) = dir.run("f", "12345n + 0.5");
    assert_eq!(code, 0, "小值 + 真小数应照常:\n{}", out);
    assert_eq!(
        out.trim(),
        "12345.5",
        "小 BigInt + 小数仍给 Float:\n{}",
        out
    );

    // 且整数侧的混合现在**保持 BigInt**（修前会退化成 Float）。
    let (out, code) = dir.run("i", "12345n + 1");
    assert_eq!(code, 0, "整数侧混合应成功:\n{}", out);
    assert_eq!(
        out.trim(),
        "12346n",
        "整数值的 float + BigInt 应提升为 **BigInt**（修前退化成 `12346.0`）:\n{}",
        out
    );
}

/// **不回归**：`BigInt ⊕ BigInt` 本来就对，不能被改坏。
#[test]
fn d198_bigint_only_arithmetic_is_unaffected() {
    let dir = WorkDir::new("bb");
    let cases: [(&str, String, &str); 4] = [
        (
            "add",
            format!("{BIG} + {BIG}"),
            "2000000000000000000000000000000n",
        ),
        (
            "mul",
            format!("{BIG} * 2n"),
            "2000000000000000000000000000000n",
        ),
        (
            "div",
            format!("{BIG} / 2n"),
            "500000000000000000000000000000n",
        ),
        ("small", "2n + 3n".to_string(), "5n"),
    ];
    for (tag, expr, want) in cases {
        let (out, code) = dir.run(tag, &expr);
        assert_eq!(code, 0, "[{tag}] 应成功:\n{}", out);
        assert_eq!(out.trim(), want, "[{tag}] `{expr}` 结果被改坏了");
    }
}
