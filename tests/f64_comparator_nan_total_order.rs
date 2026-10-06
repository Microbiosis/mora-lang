//! D242 判据：`f64` 比较器必须用**全序**（`total_cmp`），不能 `partial_cmp + Equal`。
//!
//! ## 缺陷背景
//!
//! `a.partial_cmp(b).unwrap_or(Ordering::Equal)` 遇 NaN 时 `partial_cmp`
//! 返回 `None` → `Equal` ⇒ **NaN 与一切相等**，而它与别的数的实际大小关系
//! 又不一致 ⇒ 违反传递性。Rust `sort_by` 在非全序比较器下**静默**产出
//! **依赖输入顺序**的结果，不 panic、零诊断。
//!
//! ## 实测（`total_cmp` 的穷举对照）
//!
//! `[1.0, NaN, 3.0, 2.0]` 的 24 种输入排列：
//!
//! | 比较器 | 不同结果数 |
//! |---|---|
//! | `partial_cmp + Equal` | **8** |
//! | `total_cmp` | **1** |
//!
//! ## 影响（真实 CLI，`mora run`）
//!
//! | 调用 | 修前（仅换输入顺序） | 修后 |
//! |---|---|---|
//! | `stats.median` | `1.5` / **`2.5`** | 恒 `1.5` |
//! | `stats.quantile(_, 0.75)` | `2.25` / **`nan`** | 恒 `2.25` |
//! | `xs.sort()` | 3 种不同结果 | 恒 `[nan,1,2,3]` |
//!
//! **中位数/分位数是统计值** —— 顺序依赖意味着**同一组数据给出不同答案**。
//!
//! ## 判据形态
//!
//! **排列穷举不变式**（D241 同款）+ **真实 Mora 源码**端到端。

use mora::interpreter::Interpreter;
use mora::mir::effect::Effects;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use std::sync::Arc;

/// 穷举 n 个 f64 的全部排列
fn perms(xs: &[f64]) -> Vec<Vec<f64>> {
    fn go(rest: &mut Vec<f64>, acc: &mut Vec<f64>, out: &mut Vec<Vec<f64>>) {
        if rest.is_empty() {
            out.push(acc.clone());
            return;
        }
        for i in 0..rest.len() {
            let x = rest.remove(i);
            acc.push(x);
            go(rest, acc, out);
            acc.pop();
            rest.insert(i, x);
        }
    }
    let mut rest = xs.to_vec();
    let mut acc = Vec::new();
    let mut out = Vec::new();
    go(&mut rest, &mut acc, &mut out);
    out
}

/// D242 主判据：`total_cmp` 对同一组数给出**唯一**结果。
#[test]
fn d242_total_cmp_is_order_independent() {
    let xs = vec![1.0, f64::NAN, 3.0, 2.0];
    let mut outcomes: std::collections::BTreeSet<Vec<u64>> = Default::default();
    for p in perms(&xs) {
        let mut v = p.clone();
        v.sort_by(|a, b| a.total_cmp(b));
        outcomes.insert(v.iter().map(|f| f.to_bits()).collect());
    }
    assert_eq!(
        outcomes.len(),
        1,
        "D242: `total_cmp` 应给出唯一结果（NaN 统一排末尾），实得 {} 种",
        outcomes.len()
    );
}

/// D242 对照组：证明**旧写法确实是坏的**（否则主判据无判别力）。
///
/// 这条不是「测产品」，而是**固定住缺陷的成因**：若哪天有人认为
/// `partial_cmp + Equal` 也没问题，本条会立刻给出反例。
#[test]
fn d242_partial_cmp_plus_equal_is_the_broken_form() {
    let xs = vec![1.0, f64::NAN, 3.0, 2.0];
    let mut outcomes: std::collections::BTreeSet<Vec<u64>> = Default::default();
    for p in perms(&xs) {
        let mut v = p.clone();
        // 修前就是这个写法
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        outcomes.insert(v.iter().map(|f| f.to_bits()).collect());
    }
    assert!(
        outcomes.len() > 1,
        "D242: `partial_cmp + Equal` 应对 NaN 产生**顺序依赖**（多种结果），\
         实得 {} 种 —— 若这里只有 1 种，说明「NaN 破坏全序」的前提不成立，\
         本文件的其余判据都要重新审视",
        outcomes.len()
    );
}

fn run(source: &str) -> String {
    let (func, _w) = ParserV3::compile(source).expect("compile");
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    match run_mir(&arc, &mut interp, &mut env, &mut Effects::new()) {
        Ok(v) => format!("{v:?}"),
        Err(e) => format!("ERR:{e}"),
    }
}

/// D242 端到端：真实 Mora 源码下 `stats.median` / `quantile` / `list.sort`
/// 不得因输入顺序不同而给出不同答案。
#[test]
fn d242_stats_and_sort_are_order_independent_end_to_end() {
    // 同一组 4 个数，三种输入顺序
    let a = run("let z = 0.0 / 0.0\nstats.median([1.0, z, 3.0, 2.0])\n");
    let b = run("let z = 0.0 / 0.0\nstats.median([z, 3.0, 1.0, 2.0])\n");
    let c = run("let z = 0.0 / 0.0\nstats.median([2.0, 1.0, 3.0, z])\n");
    assert_eq!(a, b, "D242: median 依赖输入顺序。\n  a={a}\n  b={b}");
    assert_eq!(a, c, "D242: median 依赖输入顺序。\n  a={a}\n  c={c}");

    let qa = run("let z = 0.0 / 0.0\nstats.quantile([1.0, z, 3.0, 2.0], 0.75)\n");
    let qc = run("let z = 0.0 / 0.0\nstats.quantile([2.0, 1.0, 3.0, z], 0.75)\n");
    assert_eq!(
        qa, qc,
        "D242: quantile 依赖输入顺序。\n  qa={qa}\n  qc={qc}"
    );

    let sa = run("let z = 0.0 / 0.0\n[1.0, z, 3.0, 2.0].sort()\n");
    let sb = run("let z = 0.0 / 0.0\n[z, 3.0, 1.0, 2.0].sort()\n");
    let sc = run("let z = 0.0 / 0.0\n[2.0, 1.0, 3.0, z].sort()\n");
    assert_eq!(sa, sb, "D242: sort 依赖输入顺序。\n  sa={sa}\n  sb={sb}");
    assert_eq!(sa, sc, "D242: sort 依赖输入顺序。\n  sa={sa}\n  sc={sc}");
}

/// D242 对照组：**无 NaN** 时行为不变（防「过度修改」）。
#[test]
fn d242_finite_values_behave_normally() {
    assert_eq!(
        run("stats.median([3.0, 1.0, 2.0])\n"),
        "Float(2.0)",
        "D242: 无 NaN 时中位数应正常（不应被 total_cmp 改动影响）"
    );
    // ⚠ `[3.0, 1.0, 2.0]` 的字面量元素是 **Float** 不是 Int
    //   （D98：dict / 列表字面量给 Float）。我第一版写死 `Int(..)` 判红，
    //   那是**我的期望错了**，不是产品错。
    assert_eq!(
        run("[3.0, 1.0, 2.0].sort()\n"),
        "List([Float(1.0), Float(2.0), Float(3.0)])",
        "D242: 无 NaN 时 sort 应正常升序"
    );
}
