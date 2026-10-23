//! v0.104.6 D474：代表性 fixture 的**行为契约** —— 逐条钉住期望输出。
//!
//! ## 与 D279 版的差别
//!
//! D279 版用「9 层路径 vs `emit.rs` 路径的 A/B」作判据（D278 修好
//! `quasiquote` 之后首次全绿）。**D474 把差分机制整个退役** —— parser 不再
//! 发射 MirInst（只产 witness），MIR 全部由 9 层产出 ⇒「参考实现」这一侧
//! **已不存在**，那次 A/B 会变成**自己跟自己比**（恒真）。
//!
//! ⇒ 判据换成**钉死的期望输出**。这比原判据**更强** —— 原判据只能发现
//! 「两条路径不一致」（两条一起错到同一个错值时反而绿），新判据直接钉住
//! **正确值**。
//!
//! ## 为什么只取 11 个样本
//!
//! 全部 57 个 e2e fixture 的「跑得通」由
//! `tests/nine_layer_unblocked.rs::every_e2e_fixture_runs_successfully`
//! 覆盖；本文件只负责**逐值**钉住**代表性**样本 —— 曾经出过值语义分歧的
//! （`quasiquote`）、D276 教训的主角（`rel_*`）、模块边界（import/export）、
//! TEA、以及各类基础控制流。
//!
//! ## 判定必须**同时**看 stdout 与退出码
//!
//! D276 的教训：`rel_*` 程序两条路径的 stdout **都是空**，只比 stdout 会
//! 判成「相等」，而**退出码 1 vs 0** 被整个漏掉。

use std::process::Command;

const EXE: &str = env!("CARGO_BIN_EXE_mora");

/// 跑一次 `mora run`，返回 `(stdout, exit_code)`。
fn run(path: &str) -> (String, i32) {
    let out = Command::new(EXE)
        .arg("run")
        .arg(path)
        .output()
        .expect("应能执行");
    (
        String::from_utf8_lossy(&out.stdout)
            .replace("\r\n", "\n")
            .trim_end()
            .to_string(),
        out.status.code().unwrap_or(-1),
    )
}

/// 代表性样本 → 期望 stdout（**逐行**）。
const SAMPLES: &[(&str, &str)] = &[
    // 曾经出过值语义分歧的（D277/D278）：`` `,,splice `` 此前被渲染成
    // `List([…])`（正确值是 `1, 2, 3`）。
    (
        "quasiquote.mora",
        "30.0\n15.0\ncode:1, 2, 3\ncode:hello world\n30.0",
    ),
    // D276 的主角：stdout 都是空，**退出码**才是判别器。
    ("rel_basic.mora", ""),
    ("rel_empty.mora", ""),
    // import / 模块边界
    ("import_handle_index_main.mora", "20.0"),
    ("export_visibility.mora", "hi\n1.0"),
    // TEA
    ("tea_standalone.mora", "dict\nlist\nclosure\ntea_app"),
    ("tea_counter.mora", ""),
    // 基础控制流
    ("arithmetic.mora", "42"),
    ("for_loop.mora", "15"),
    (
        "match_guard.mora",
        "[negative, middle, seven]\npositive\nzero\nzero\nlong\nsmall\none\nenough",
    ),
];

#[test]
fn d474_representative_samples_produce_their_expected_output() {
    let mut mismatches = Vec::new();
    for (name, want) in SAMPLES {
        let path = format!("tests/fixtures/e2e/{name}");
        assert!(
            std::path::Path::new(&path).exists(),
            "fixture 不存在：{path}"
        );
        let (out, code) = run(&path);
        // 退出码与 stdout **同时**判（D276 教训）。
        if code != 0 || out != *want {
            mismatches.push(format!(
                "{name}: exit={code}\n    期望 stdout: {want:?}\n    实得 stdout: {out:?}"
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "代表性样本的运行结果与期望不符（{} 个）：\n  {}",
        mismatches.len(),
        mismatches.join("\n  ")
    );
}
