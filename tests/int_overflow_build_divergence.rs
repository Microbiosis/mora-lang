//! v0.104.6 D299：`Int + Int` 溢出是**构建配置相关**的 —— debug panic / release 静默回绕。
//!
//! ## 现象（实测，两个构建跑**同一段** Mora 源码）
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
//! ## 为什么测试套件**结构上**看不见
//!
//! 本项目 29 轮的 `cargo test` 全部跑在 debug（`Cargo.toml` 无 `[profile.*]`
//! 覆盖 ⇒ release 默认 `debug-assertions=off` + `overflow-checks=off`）。
//! 同一段源码在 debug 下**panic**，在 release 下**回绕**——
//! 所以**任何断言都只能钉住其中一侧**。这不是「测试写得不够」，是构建配置决定的。
//!
//! 而 CI 同时两条都跑（`ci.yml:70/73` 用 debug 测、`ci.yml:136/174` 构建 release），
//! 却**从不**用 release 跑测试 ⇒ release 的行为在 CI 里同样无人验证。
//!
//! ## 根因
//!
//! `src/flow.rs:154`：
//!
//! ```rust
//! (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),   // 裸 i64 加法
//! ```
//!
//! ⚠ **同一函数、紧邻上方 20 行**（D198）恰恰是这一类的显式守卫：
//! Float ⊕ BigInt 超出精确范围时返回带可读消息的 `MoraError`，
//! 并提示「整数运算请用 bigint 字面量」。即这个文件**知道**这类风险、
//! 也已有成例，但 `Int + Int` 这一条漏了。
//!
//! 另两处同形态（**当前不可达**，一并记档）：
//! `flow.rs:319` `Int / Int` → `a / b`（`i64::MIN / -1` 会溢出）、
//! `flow.rs:339` `Int % Int` → `a % b`（`i64::MIN % -1` 会溢出）。
//! 不可达的原因：`i64::MIN` 只能由减法造出，而 `Sub`/`Mul` 走
//! `numeric_op`（`Fn(f64, f64) -> f64`）会先转 f64 ⇒ 造不出精确的 `i64::MIN`。
//!
//! ## 另一处**既有记录**但至今未修
//!
//! `docs/audit/MIR_COMPILER_AUDIT_2026-07-25.md:1308` 把它列为 **P0**：
//!
//! ```text
//! | 6 | i64 加法 debug panic | flow.rs:103 | Runtime |
//! ```
//!
//! 建议是「用 `checked_add`/`saturating_add`，溢出返回 `Err`」。
//! 该条目自 2026-07-25 起**未进入 CHANGELOG 的 D 编号序列**，本轮才重新浮出。
//!
//! ## 为什么本轮只判据不修
//!
//! 修法有三种互不相同的**语言语义**，属产品契约决定：
//! ① 溢出报 `MoraError`（D198 的成例）② 饱和到 `i64::MAX/MIN` ③ 提升到 BigInt。
//! 未擅自实施，已上报裁决。
//!
//! 本文件是**现状判据**：钉住**当前**（debug）行为，并写明 release 的行为。
//! 修好之后本文件会红 —— 那时把 `assert_eq!(code, 101)` 改成断言「返回值正确」
//! 或「报 Mora 级错误」即可。

use std::process::Command;

/// i64::MAX + 1 —— 唯一能触发 i64 加法溢出的用户程序。
const OVERFLOW_SRC: &str = "let a = 9223372036854775807i\nprint(a + 1i)\n";

fn run(src: &str, tag: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("mora_d299_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
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

/// **现状判据**：debug 构建下 `i64::MAX + 1` 触发 Rust panic（exit 101）。
///
/// 修好之后本条会红。D299 的报告里写明了 release 那一侧的行为
/// （exit 0 + 静默回绕成 `i64::MIN`），改写时请一并把那条也改成断言。
#[test]
fn int_add_overflow_panics_in_debug_build_status_quo() {
    let (code, out) = run(OVERFLOW_SRC, "add_ovf");
    assert_eq!(
        code, 101,
        "**现状判据**：debug 构建下 i64 加法溢出会 panic（exit 101）。\n\
         D299 已上报：release 构建下同一程序 exit 0 并静默回绕为 i64::MIN。\n\
         若这里不再 panic，说明溢出已被守卫或改写 —— 请把本判据改成\n\
         断言正确行为（报 Mora 级错误 / 饱和 / 提升 BigInt），并更新 CHANGELOG D299。\n\
         实得输出：\n{out}"
    );
    assert!(
        out.contains("panicked") && out.contains("flow.rs"),
        "panic 应来自 `src/flow.rs`（裸 i64 加法那一行）；实得：\n{out}"
    );
}

/// 对照组：**未溢出**的 `Int + Int` 必须正常，不得被守卫误伤。
///
/// 修 `flow.rs:154` 时最可能引入的回归是「把所有加法都挡掉」。
#[test]
fn int_add_without_overflow_is_unaffected() {
    let (code, out) = run("let a = 3i\nprint(a + 1i)\n", "ok_add");
    assert_eq!(code, 0, "普通整数加法必须成功；实得：\n{out}");
    assert!(out.contains('4'), "`3i + 1i` 应为 4；实得输出：\n{out}");
}

/// 记档：`Int / Int` 与 `Int % Int` 是同形态的**潜伏**点。
///
/// 本条不做行为断言，只把「它们与 `Add` 同形」这件事钉在测试里，
/// 避免将来有人把 `flow.rs:319` / `:339` 当成「已经安全」。
#[test]
fn int_div_mod_share_the_same_unguarded_shape_note() {
    // `i64::MIN / -1` 与 `i64::MIN % -1` 在 debug 下会 panic，
    // 但 `i64::MIN` 目前**造不出来**（Sub/Mul 走 f64），故本轮不可达。
    // 一旦将来 `Sub` 改回整数路径或加了 checked 变体，本条的前提就变了，
    // 需要重新评估这两处。
    let (code, out) = run("print(7i / 2i)\nprint(7i % 2i)\n", "divmod");
    assert_eq!(code, 0, "整数除/模的常规路径必须正常；实得：\n{out}");
    assert!(
        out.contains('3') && out.contains('1'),
        "7/2=3, 7%2=1；实得：\n{out}"
    );
}
