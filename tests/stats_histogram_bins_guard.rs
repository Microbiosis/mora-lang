//! v0.104.6 D284：`stats.histogram` 的 `bins` **回绕成 1.8e19** ⇒ 进程 panic（已修）
//!
//! ## 实测（修前）—— 两行普通 Mora 代码崩掉整个进程
//!
//! ```mora
//! let xs = json.parse("[1, 2, 3]")
//! let n  = json.parse("-1")
//! print(stats.histogram(xs, n))
//! ```
//!
//! ```text
//! thread 'mora-main' panicked at alloc/src/raw_vec/mod.rs:28:5:
//! capacity overflow
//! ```
//!
//! ## 因果链
//!
//! ```text
//! json.parse("-1")            → Value::Int(-1)
//! *n as usize                 → **回绕**成 1.8e19（不是负数、不是报错）
//! vec![0usize; bins]          → 1.8e19 × 8 字节 ⇒ capacity overflow panic
//! ```
//!
//! 另一半（更隐蔽、不崩）：负数 `Float` 饱和成 **0**，于是走
//! `if bins == 0` 分支**静默返回空列表** —— 用户拿到「0 个 bin 的直方图」
//! 而无任何诊断。
//!
//! ## ⚠ 只修回绕**不够**：超大正数会以完全相同的方式崩
//!
//! `json.parse("100000000000")` ⇒ `bins = 1e11` ⇒ 同样 `capacity overflow`。
//! 所以本条同时加了上界守卫，把「进程 panic」换成「一条可操作的错误」。
//!
//! ## 上界是**判断题**
//!
//! `HISTOGRAM_MAX_BINS = 1_000_000`（1e6 bin ≈ 8 MB）远超任何合理直方图用法。
//! 目的是**不崩**，不是规定合理值。若认为过严/过松，改 `stats.rs` 里那一个常量即可。

use mora::value::Value;

fn hist(bins: Value) -> Result<Value, String> {
    let xs = Value::List(mora::value::list::List::from_vec(vec![
        Value::Float(1.0),
        Value::Float(2.0),
        Value::Float(3.0),
        Value::Float(4.0),
    ]));
    mora::interpreter::builtins::stats::call_stats_method("histogram", &[xs, bins])
}

/// **主断言**：`Value::Int(-1)`（`json.parse("-1")` 的产物）必须被拒绝。
///
/// 修前它回绕成 1.8e19，本条**若在修前运行会直接 panic**，
/// 所以它同时是「这条路径曾经崩」的证据。
#[test]
fn d284_negative_int_bins_is_rejected_instead_of_wrapping() {
    let err = hist(Value::Int(-1)).expect_err("负数 bins 应被拒绝");
    assert!(
        err.contains("bins") && err.contains("integer"),
        "错误应点名 bins 且说明要求。实际：{err}"
    );
}

/// 负数 `Float`（字面量 `-1.0`）不得**静默变成 0 个 bin**。
///
/// 修前：饱和成 0 → `bins == 0` 分支 → 返回空列表、无任何诊断。
#[test]
fn d284_negative_float_bins_is_rejected_not_silently_zero() {
    let err = hist(Value::Float(-1.0)).expect_err("负数 bins 应被拒绝");
    assert!(err.contains("bins"), "错误应点名 bins。实际：{err}");
}

/// 超大正数同样必须被拒绝（不封顶的话它以**完全相同的方式**崩）。
#[test]
fn d284_huge_bins_is_rejected_instead_of_overflowing() {
    let err = hist(Value::Int(100_000_000_000))
        .expect_err("超上界的 bins 应被拒绝，而不是 capacity overflow");
    assert!(err.contains("bins"), "错误应点名 bins。实际：{err}");
}

/// **对照组 1**：正常 bins 仍给出正常结果（本条不能把 histogram 弄坏）。
#[test]
fn d284_normal_bins_still_works() {
    let v = hist(Value::Int(2)).expect("正常 bins 应成功");
    match v {
        Value::List(bins) => assert_eq!(bins.len(), 2, "bins=2 应得到 2 个 bin"),
        other => panic!("期望 list，实际：{other:?}"),
    }
}

/// **对照组 2**：`bins = 0` 的**既有行为**必须保留（返回空列表）。
///
/// 修前 `bins == 0` 走 `histogram()` 里的早返回分支。本条刻意**不改**它 ——
/// 负数不再被换算成 0，但用户**显式**传 0 仍是原来的语义。
/// 这条守住「只修换算、不动语义」的边界。
#[test]
fn d284_explicit_zero_bins_still_returns_empty_list() {
    let v = hist(Value::Int(0)).expect("显式 bins=0 仍应成功（既有行为）");
    match v {
        Value::List(bins) => assert!(bins.is_empty(), "bins=0 的既有行为是空列表，不该被本条改掉"),
        other => panic!("期望 list，实际：{other:?}"),
    }
}
