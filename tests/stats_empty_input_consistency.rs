//! v0.104.6 D143：`stats.cov([] , [])` 静默得 `NaN` —— 与**同文件**其余聚合函数不一致（已修）。
//!
//! ## 缺陷
//!
//! ```mora
//! stats.cov([], [])     →  nan        （exit 0，零诊断）
//! ```
//!
//! 而**同文件**的 `mean` / `var` / `median` / `min` / `max` / `corr` 一律返回
//! `0.0` —— 这个约定就写在 `mean` 上方的注释里（v0.104.6 引入）。
//! `cov` 是同族里**唯一**漏掉空输入守卫的（它直接 `cov / a.len()`）。
//!
//! ## 对照：D142 的姊妹案例
//!
//! D142 修 `linalg.dot`/`matmul` 的维度不匹配时，留下一条「`stats.mean([])`
//! 属同类问题」的判断 —— **本轮查明那是错的**：`stats` 的空输入行为是
//! **有意设计**（有注释声明），不是被错误前提掩盖的缺陷。
//! 真正破例的只有 `cov` 一个。

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use std::sync::Arc;

fn run(src: &str) -> Result<String, String> {
    let (func, _w) = ParserV3::compile(src).map_err(|e| format!("COMPILE: {e}"))?;
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )
    .map(|v| format!("{v}"))
}

/// D143 主判据：`cov` 的空输入必须与**同族其余函数**一致返回 `0.0`。
#[test]
fn d143_cov_of_empty_lists_matches_its_siblings() {
    assert_eq!(
        run("stats.cov([], [])\n").unwrap(),
        "0.0",
        "`stats.cov` 空输入必须返回 0.0（与 mean/var/median/min/max/corr 一致）; \
         修复前得 `nan`"
    );
}

/// 反向对照：**同族全部**聚合函数的空输入都应是 `0.0` —— 这条把「一致性」
/// 本身钉成契约，而不是逐个函数各记一条。
#[test]
fn d143_all_stats_aggregates_agree_on_empty_input() {
    for (name, src) in [
        ("mean", "stats.mean([])\n"),
        ("var", "stats.var([])\n"),
        ("stddev", "stats.stddev([])\n"),
        ("median", "stats.median([])\n"),
        ("min", "stats.min([])\n"),
        ("max", "stats.max([])\n"),
        ("corr", "stats.corr([], [])\n"),
        ("cov", "stats.cov([], [])\n"),
    ] {
        let got = run(src).unwrap_or_else(|e| panic!("[{name}] 空输入不应报错: {e}"));
        assert_eq!(
            got, "0.0",
            "[{name}] 空输入应返回 0.0（v0.104.6 的同族约定）"
        );
    }
}

/// 反向对照：**非空**输入的结果一个都不能变（防止「一刀切」改坏正常路径）。
#[test]
fn d143_non_empty_results_are_unchanged() {
    for (name, src, expect) in [
        ("mean", "stats.mean([1.0, 2.0, 3.0])\n", "2.0"),
        ("median", "stats.median([1.0, 2.0, 3.0])\n", "2.0"),
        ("min", "stats.min([3.0, 1.0, 2.0])\n", "1.0"),
        ("max", "stats.max([3.0, 1.0, 2.0])\n", "3.0"),
        // cov([1,2,3],[2,4,6]) = 1.3333…（总体协方差）
        (
            "cov",
            "stats.cov([1.0, 2.0, 3.0], [2.0, 4.0, 6.0])\n",
            "1.3333333333333333",
        ),
        // 常量序列的相关系数数学上未定义，实现约定返回 0.0（`denom == 0.0` 分支）
        (
            "corr_constant",
            "stats.corr([1.0, 1.0, 1.0], [1.0, 2.0, 3.0])\n",
            "0.0",
        ),
        // q 越界必须**报错**（本就正确，钉住防回退）
    ] {
        let got = run(src).unwrap_or_else(|e| panic!("[{name}] 不应报错: {e}"));
        assert_eq!(got, expect, "[{name}] 非空结果应保持不变");
    }
}

/// 对照组：`quantile` 的 q 越界**本就**明确报错（不是静默），钉住防回退。
#[test]
fn d143_quantile_rejects_out_of_range_q() {
    for src in [
        "stats.quantile([1.0, 2.0, 3.0, 4.0], 1.5)\n",
        "stats.quantile([1.0, 2.0, 3.0, 4.0], -0.1)\n",
    ] {
        let res = run(src);
        assert!(res.is_err(), "q 越界应报错; 实际: {res:?}");
        assert!(
            res.unwrap_err().contains("[0, 1]"),
            "错误信息应给出合法区间"
        );
    }
}
