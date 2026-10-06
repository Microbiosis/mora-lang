//! v0.104.6 D279：9 层路径与 `emit.rs` 路径的**行为等价性**（`quasiquote` 修好后首次全绿）
//!
//! ## 背景
//!
//! D275 量化出「9 层路径在 21% 的真实程序上被丢弃、**从未被验证过**」；
//! D277 在做 A/B 时撞出**第一个**实证分歧（`quasiquote` 的 `` `,,splice ``）；
//! D278 修掉了它。
//!
//! ## 全量扫描结果（真实 CLI，`examples/` + `tests/fixtures/e2e/` + `test_data/`）
//!
//! 用**修正后**的方法逐个 A/B（默认 = 9 层生效 vs `MORA_9LAYER=0` 强制
//! `emit.rs` 原路径）：
//!
//! | | stdout 相同 | stdout 不同 |
//! |---|---|---|
//! | D278 修前（**只比 stdout**，D276 教训） | 52 / 53 | 1（`quasiquote`） |
//! | D278 修前（**同时比退出码**） | — | 至少 2（`quasiquote` + `rel_*` 的 exit 1/0） |
//! | **D278 修后** | **57 / 57** | **0** |
//!
//! ⇒ 这是 9 层路径**第一次**有「两条路径行为等价」的正面证据。
//!
//! ## ⚠ 这个判据只比**抽样**，不是全量
//!
//! 57 个文件的全量扫描要跑约 2 分钟，放进 CI 太重。本文件取
//! **11 个代表性 fixture**（约 1 秒）：曾经出过问题的 `quasiquote`、
//! `rel_*`（D276 教训的主角）、tea、import、以及各类基础控制流。
//! 全量数字记在 CHANGELOG D279，需要复现时按那里的清单跑。
//!
//! ## 判定必须**同时**比 stdout 与退出码
//!
//! D276 的教训：`rel_*` 程序两条路径的 stdout **都是空**，只比 stdout 会
//! 判成「相等」，而**退出码 1 vs 0** 被整个漏掉 —— 那次差点据此认定
//! 修复安全。

use std::process::Command;

const EXE: &str = env!("CARGO_BIN_EXE_mora");

/// 跑一次 `mora run`，返回 `(stdout, exit_code)`。
///
/// ⚠ 本文件**只有一条测试**：`MORA_9LAYER` 是**进程全局**的环境变量，
/// 多条测试并行会互相踩。独立测试文件 = 独立进程 = 互不干扰。
fn run(env9: Option<&str>, path: &str) -> (String, i32) {
    // SAFETY: 本文件只有一条测试在跑，不存在与其他线程并发读环境变量。
    unsafe {
        match env9 {
            Some(v) => std::env::set_var("MORA_9LAYER", v),
            None => std::env::remove_var("MORA_9LAYER"),
        }
    }
    let out = Command::new(EXE)
        .arg("run")
        .arg(path)
        .output()
        .expect("应能执行");
    // SAFETY: 同上。
    unsafe { std::env::remove_var("MORA_9LAYER") };
    (
        String::from_utf8_lossy(&out.stdout).trim_end().to_string(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn d279_pipeline_and_emit_paths_agree_on_a_representative_sample() {
    let samples = [
        // 曾经出过值语义分歧的（D277/D278）
        "quasiquote.mora",
        // D276 的主角：差分护栏 + 两条路径的 stdout 都是空（退出码才是判别器）
        "rel_basic.mora",
        "rel_empty.mora",
        // import / 模块边界
        "import_handle_index_main.mora",
        "export_visibility.mora",
        // TEA
        "tea_standalone.mora",
        "tea_counter.mora",
        // 基础控制流
        "arithmetic.mora",
        "for_loop.mora",
        "match_guard.mora",
    ];

    let mut mismatches = Vec::new();
    for name in samples {
        let path = format!("tests/fixtures/e2e/{name}");
        assert!(
            std::path::Path::new(&path).exists(),
            "fixture 不存在：{path}"
        );
        let (out_a, code_a) = run(None, &path); // 默认：9 层生效
        let (out_b, code_b) = run(Some("0"), &path); // 强制 emit.rs
        if out_a != out_b || code_a != code_b {
            mismatches.push(format!(
                "{name}: stdout {} / exit {code_a} vs {code_b}",
                if out_a == out_b { "同" } else { "**异**" }
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "9 层路径与 emit.rs 路径在这些样本上行为不一致：\n  {}\n\
         （D278 修后全量 57/57 一致；若这里红，说明又出现新的值语义分歧）",
        mismatches.join("\n  ")
    );
}
