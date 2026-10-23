//! v0.104.6 D468：`Int` 算术溢出**不再随构建配置漂移**，统一报 `MoraError`。
//!
//! ## 修复前的问题（D299 记录）
//!
//! ```mora
//! let a = 9223372036854775807i     // i64::MAX
//! print(a + 1i)
//! ```
//!
//! | 构建 | 退出码 | 输出 |
//! |---|---|---|
//! | `target/debug/mora.exe` | **101** | `thread 'mora-main' panicked at src\flow.rs:154:61` |
//! | `target/release/mora.exe` | **0** | **`-9223372036854775808`** ← `i64::MIN`，静默回绕 |
//!
//! ⇒ **同一个程序、同一个输入，值与退出码都随构建配置而变**。
//! release 那一侧是**静默的数值损坏**：用户拿到一个完全错误的结果，零诊断。
//!
//! 之所以**测试套件结构上看不见**：本项目 29 轮的 `cargo test` 全部跑在
//! debug ⇒ 任何断言都只能钉住其中一侧。而 CI 同时两条都跑（debug 测 +
//! release 构建），却**从不**用 release 跑测试 ⇒ release 的行为在 CI 里
//! 同样无人验证。
//!
//! ## D468 的修复
//!
//! 解释器的 `Int` 算术统一为 **checked i64**，溢出返回 `MoraError`：
//!
//! ```text
//! debug:   Runtime error (MIR): integer overflow in Add: ... （exit 1）
//! release: Runtime error (MIR): integer overflow in Add: ... （exit 1）
//! ```
//!
//! **两侧完全一致**，且都不再是 panic、不再是静默回绕。
//!
//! 顺带修掉的同族缺陷（此前**不可达**，因为 `Sub`/`Mul` 走 f64 造不出精确的
//! `i64::MIN`）：`i64::MIN / -1` 与 `i64::MIN % -1` 也会溢出 → 现在同样报错。
//!
//! ## 与 JIT 的关系
//!
//! JIT 的 `Int` 算术此前是 wrap（x86 `add` 天然回绕），与解释器「一起错」
//! 所以差分测试是绿的。D468 后 JIT 用 `jo` 检出溢出 → bail → 回落解释器，
//! 由解释器报 `MoraError`（见 `tests/jit_compile.rs::
//! jit_int_add_overflow_bails_instead_of_wrapping`）。

use mora::test_support::TempDir;
use std::process::Command;

/// i64::MAX + 1 —— 唯一能触发 i64 加法溢出的用户程序。
const OVERFLOW_SRC: &str = "let a = 9223372036854775807i\nprint(a + 1i)\n";

fn run(src: &str, tag: &str) -> (i32, String) {
    let dir = TempDir::new(tag);
    let p = dir.join("p.mora");
    std::fs::write(&p, src).expect("写探针");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe).arg(&p).output().expect("跑 mora");
    let s = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    (out.status.code().unwrap_or(-1), s)
}

/// **D468 判据**：`i64::MAX + 1` 报 `MoraError`，**不再 panic、不再回绕**。
///
/// 退出码是 1（Mora 的运行时错误），**不是** 101（Rust panic）。
#[test]
fn int_add_overflow_reports_mora_error_not_panic() {
    let (code, out) = run(OVERFLOW_SRC, "add_ovf");
    assert_eq!(
        code, 1,
        "D468：i64 加法溢出应报 Mora 级错误（exit 1），不得 panic（101）\
         也不得静默回绕（0）。\n\
         实得输出：\n{out}"
    );
    assert!(
        out.contains("overflow") && out.contains("i64"),
        "错误消息应指明是整数溢出；实得：\n{out}"
    );
    assert!(
        !out.contains("panicked"),
        "不得再出现 Rust panic；实得：\n{out}"
    );
    assert!(
        !out.contains("-9223372036854775808"),
        "不得静默回绕成 i64::MIN；实得：\n{out}"
    );
}

/// 对照组：**未溢出**的 `Int + Int` 必须正常，不得被守卫误伤。
///
/// 修溢出语义时最可能引入的回归是「把所有加法都挡掉」。
#[test]
fn int_add_without_overflow_is_unaffected() {
    let (code, out) = run("let a = 3i\nprint(a + 1i)\n", "ok_add");
    assert_eq!(code, 0, "普通整数加法必须成功；实得：\n{out}");
    assert!(out.contains('4'), "`3i + 1i` 应为 4；实得输出：\n{out}");
}

/// 对照组：**未溢出**的 `Int - Int` / `Int * Int` 同样必须正常。
///
/// v0.104.6 D468 之前，`Sub`/`Mul` 走 f64 round-trip，在 2^53 边界
/// 会静默丢最低位。此处用普通值做守卫，确保精确路径没有误伤常规运算。
#[test]
fn int_sub_mul_without_overflow_are_unaffected() {
    let (code, out) = run("print(10i - 3i)\nprint(4i * 5i)\n", "ok_submul");
    assert_eq!(code, 0, "普通整数减/乘必须成功；实得：\n{out}");
    assert!(
        out.contains('7') && out.contains("20"),
        "10-3=7, 4*5=20；实得：\n{out}"
    );
}

/// D468 的核心判据：`a - 0i` 与 `a + 0i` 必须相等（2^53 边界）。
///
/// 旧实现里 `Sub` 走 f64 round-trip、`Add` 走裸 i64 ⇒ 两者在
/// 2^53+1 处给出**不同**的值（同一个数，减法把它改了）。
#[test]
fn int_sub_zero_equals_add_zero_beyond_2p53() {
    let src = "let a = 9007199254740993i\nprint(a + 0i)\nprint(a - 0i)\n";
    let (code, out) = run(src, "p53");
    assert_eq!(code, 0, "2^53 边界上的加减必须成功；实得：\n{out}");
    let mut lines = out
        .lines()
        .filter(|l| l.trim().parse::<i64>().is_ok())
        .map(|l| l.trim().to_string());
    let plus = lines.next().expect("应有第一行输出");
    let minus = lines.next().expect("应有第二行输出");
    assert_eq!(
        plus, minus,
        "a + 0i 必须等于 a - 0i（2^53+1 不得被 f64 舍入改写）；\
         实得 +{plus} / -{minus}"
    );
    assert_eq!(
        plus, "9007199254740993",
        "2^53+1 必须精确保真；实得：\n{out}"
    );
}

/// `Int / Int` 与 `Int % Int` 的常规路径（截断向零）。
///
/// v0.104.6 D19 修过「四舍五入而非截断」与「除零静默返回垃圾」两处；
/// D468 保留了这两处语义（除零报错、截断向零），此处钉住不被回归。
#[test]
fn int_div_mod_truncate_toward_zero() {
    let (code, out) = run(
        "print(7i / 2i)\nprint(7i % 2i)\nprint(-7i / 2i)\n",
        "divmod",
    );
    assert_eq!(code, 0, "整数除/模的常规路径必须正常；实得：\n{out}");
    assert!(
        out.contains('3') && out.contains('1'),
        "7/2=3, 7%2=1；实得：\n{out}"
    );
}

/// 整数除零必须报错（不是静默返回 i64::MAX / 0）。
#[test]
fn int_div_by_zero_reports_error() {
    let (code, out) = run("print(1i / 0i)\n", "divzero");
    assert_eq!(code, 1, "整数除零必须报 Mora 级错误；实得：\n{out}");
    assert!(
        out.contains("division by zero"),
        "错误消息应指明除零；实得：\n{out}"
    );
}
