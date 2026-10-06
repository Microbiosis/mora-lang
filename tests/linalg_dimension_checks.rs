//! v0.104.6 D142：`linalg.dot` / `cross` / `matmul` 维度不匹配**静默给错值**（已修）。
//!
//! ## 缺陷
//!
//! | 调用 | 修复前 | 修复后 |
//! |---|---|---|
//! | `linalg.dot([1,2], [1,2,3])` | **`nan`**，exit 0，零诊断 | `linalg.dot: 向量维度不匹配（2 维 vs 3 维）`，exit 1 |
//! | `linalg.cross([1,2], [1,2])` | **`[nan, nan, nan]`**，exit 0 | `linalg.cross: 只支持 3D 向量…`，exit 1 |
//! | `linalg.matmul([[1,2]], [[1],[2],[3]])` | **`[]`**，exit 0 | `linalg.matmul: 维度不匹配 —— A 是 1×2，B 有 3 行…`，exit 1 |
//!
//! `dot` 当时的注释还写着「不在 builtin 抛错（**已在上层 `expect_f64_vec` 校验**）」——
//! 实测 `expect_f64_vec` **只校验是不是 List + 元素是数值**，**不看维度**。
//! 一条错误的前提让「静默兜底」看起来像是深思熟虑的设计。
//!
//! ## 为什么这三处危害大
//!
//! 维度不同的点积在数学上无意义，`NaN` 会顺着后续算术**静默污染整条表达式**
//! （`nan + 1`、`nan > 0` 都不报错）；而 `matmul` 返回 `[]` 时，调用方
//! **无法区分「结果就是空矩阵」与「参数写错了」**。

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

/// D142 主判据：三处维度不匹配都必须**明确报错**，不得静默给 `NaN` / `[]`。
#[test]
fn d142_linalg_dimension_mismatches_are_rejected() {
    for (name, src, needle) in [
        (
            "dot_mismatched",
            "linalg.dot([1.0, 2.0], [1.0, 2.0, 3.0])\n",
            "维度不匹配",
        ),
        (
            "cross_not_3d",
            "linalg.cross([1.0, 2.0], [1.0, 2.0])\n",
            "3D",
        ),
        (
            "matmul_mismatched",
            "linalg.matmul([[1.0, 2.0]], [[1.0], [2.0], [3.0]])\n",
            "维度不匹配",
        ),
    ] {
        let res = run(src);
        assert!(
            res.is_err(),
            "[{name}] 维度不匹配必须**报错**（修复前静默返回 NaN / []，exit 0）; 实际: {res:?}"
        );
        let e = res.unwrap_err();
        assert!(
            e.contains(needle),
            "[{name}] 错误信息应点明「{needle}」; 实际: {e}"
        );
    }
}

/// 反向对照：**合法**调用一个都不能被误伤。
#[test]
fn d142_valid_linalg_calls_still_work() {
    for (name, src, expect) in [
        (
            "dot",
            "linalg.dot([1.0, 2.0, 3.0], [4.0, 5.0, 6.0])\n",
            "32.0",
        ),
        (
            "cross",
            "linalg.cross([1.0, 0.0, 0.0], [0.0, 1.0, 0.0])\n",
            "[0.0, 0.0, 1.0]",
        ),
        ("norm", "linalg.norm([3.0, 4.0])\n", "5.0"),
        (
            "matmul",
            "linalg.matmul([[1.0, 2.0], [3.0, 4.0]], [[5.0, 6.0], [7.0, 8.0]])\n",
            "[[19.0, 22.0], [43.0, 50.0]]",
        ),
        (
            "transpose_non_square",
            "linalg.transpose([[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]])\n",
            "[[1.0, 4.0], [2.0, 5.0], [3.0, 6.0]]",
        ),
    ] {
        let got = run(src).unwrap_or_else(|e| panic!("[{name}] 合法调用不应报错: {e}"));
        assert_eq!(got, expect, "[{name}] 结果应保持不变");
    }
}

/// 对照组：**空输入**仍走原来的宽松路径（`dot`/`matmul` 对空输入返回 `0.0` / `[]`），
/// 本轮**只**收紧维度不匹配，不改空输入语义。
#[test]
fn d142_empty_inputs_keep_their_previous_behaviour() {
    assert_eq!(run("linalg.norm([])\n").unwrap(), "0.0");
    assert_eq!(run("linalg.matmul([], [])\n").unwrap(), "[]");
}
