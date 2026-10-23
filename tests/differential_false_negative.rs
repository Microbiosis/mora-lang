//! v0.104.6 D278：quasiquote 的 `,,splice` 标记在 witness 层**丢失**
//! ⇒ 9 层路径把它渲染成 `List([…])`（**已修**）
//!
//! ## 原缺陷（D277/D278）
//!
//! `tests/fixtures/e2e/quasiquote.mora` 的 `` `,,items `` 在 9 层路径下输出
//! **`List([Float(1.0), Float(2.0), Float(3.0)])`**，而正确值是 **`1, 2, 3`**。
//!
//! 当时的差分判据**判它「通过」**（假阴性）：类别级差分只比指令类别，
//! 看不出 `Unquote` 与 `UnquoteSplice` 的差别。
//!
//! 根因（当时的三处）：
//!
//! | 位置 | 当时 |
//! |---|---|
//! | `parser_v3/emit.rs` | 标记**不产出** |
//! | `mir/witness_to_fcfg.rs` | 有消费约定（注释写着「UnquoteSplice 标记：`Literal(Boolean(true))`」）但**只跳过标记**、从不发射 `UnquoteSplice` |
//! | `mir/fcfg_lower.rs:498` | ✅ **已**正确处理 `UnquoteSplice` |
//! | `mir/ehir_to_core.rs:462` | ✅ **已**正确处理 `UnquoteSplice` |
//!
//! ## v0.104.6 D474：判据从「两条路径一致」改为「**期望输出**」
//!
//! D474 把 parser 改成**只产 witness**、MIR 全部由 9 层产出 ⇒ 「两条编译
//! 路径」这个对比**不再存在**（自己跟自己比恒等）。本文件因此把判据换成
//! **更强**的形式：直接断言**期望输出**。
//!
//! ⇒ 这比原判据更强：原判据只能发现「两条路径不一致」（两条一起错到同一个
//! 错值时反而绿），新判据直接钉住**正确值**。
//!
//! ## 修后
//!
//! `` let items = [1,2,3] `` / `` let q = `,,items `` ⇒ **`1, 2, 3`**，
//! 普通 unquote 形态（`` `10+,x `` ⇒ `10+5`）**不受影响**。

use std::process::Command;

const FIXTURE: &str = "tests/fixtures/e2e/quasiquote.mora";

/// 跑 `mora run` 并取 stdout（trim 尾部空白）。
fn run() -> String {
    let exe = env!("CARGO_BIN_EXE_mora");
    let out = Command::new(exe)
        .arg("run")
        .arg(FIXTURE)
        .output()
        .expect("应能执行 mora");
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

/// **D278 主断言**（D474 改为期望输出判据）：
///
/// ① quasiquote 的 `,,items` 展开为 `1, 2, 3`（**不是** List 的 Display
///    转储 —— 这正是 D277/D278 的缺陷形态）；
/// ② 其余三种形态（纯引号 / unquote / 带括号）**未受影响**。
#[test]
fn d278_quasiquote_splice_renders_expected_text() {
    let out = run();

    assert!(
        out.contains("code:1, 2, 3"),
        "quasiquote 的 `,,items` 应展开为 `1, 2, 3`（不是 List 的 Display 转储）。\n\
         实际输出：{out}"
    );

    // fixture 的 5 行输出依次是：
    //   30.0               ← eval(`10+20)
    //   15.0               ← eval(`10+,x)   = 10+5（普通 unquote 经求值）
    //   code:1, 2, 3       ← `` `,,items ``  ← 本条要保护的那一行
    //   code:hello world   ← 纯引号
    //   30.0               ← eval(`(10+20))
    assert!(
        out.contains("code:1, 2, 3")
            && out.contains("code:hello world")
            && out.contains("15.0")
            && out.contains("30.0"),
        "quasiquote 的四种形态（纯引号 / unquote / splice / 带括号）都应照常工作。\n\
         实际输出：{out}"
    );
}
