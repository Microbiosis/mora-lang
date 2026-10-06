//! v0.104.6 D329 —— **统计方法链**的穷尽矩阵（7 方法 × 边界形状，77 格）
//! —— 零 panic，契约钉死。
//!
//! D319 量了标量算术、D323 量了标量比较、D324 量了容器广播、D325/D326/D327
//! 量了值方法 / String·Dict 方法 / 命名内建函数。本轮量**统计方法链**：
//! `list.sum()/.mean()/.median()/.stddev()/.var()/.min()/.max()`
//! 与其自由函数孪生 `stats.*`。
//!
//! 统计方法的失败模式历来是「**静默返回一个数，但那个数不对**」
//! ——尤其空列表与单元素（D143 曾让 `stats.cov([],[])` 漏出 `nan`）。
//! 故本轮重点看：**空 / 单元素 / 混合类型 / 非数值元素**四类边界。
//!
//! ## 量出来的契约
//!
//! ### ① 七个统计方法与 `stats.*` 孪生**完全一致**，且**恒返回 Float**
//!
//! | 表达式 | 结果 |
//! |---|---|
//! | `[3,1,4,1,5].sum()` | `14.0` |
//! | `stats.sum([1,2,3])` | `6.0` |
//! | `type_of([1,2,3].sum())` | **`float`**（全 Int 输入也返回 Float）|
//! | `type_of([1,2].min())` | `float` |
//!
//! `stats.rs:132` 的 `sum_value` 注释写着「sum 保留 Int 当全是 Int 且不溢出；
//! 这里简化为 Float」——**两句自相矛盾**，实测是**恒 Float**。
//! 与 D324 量到的 `type_of(1 + 2) == float`（字面量 `1` 本身是 Float，
//! 见 `lexer.rs:797-800`）一致：本语言**没有 Int 字面量的整型语义**，
//! 所以「全是 Int」这个状态在 Mora 里**不可表达**，`sum_value` 那句
//! 「简化为 Float」是唯一的真实语义。本条把它钉死。
//!
//! ### ② 空列表：**七个方法一律返回 `0.0`**，无一报错
//!
//! | | `sum` | `mean` | `median` | `stddev` | `var` | `min` | `max` |
//! |---|---|---|---|---|---|---|---|
//! | `[]` | **`-0.0`** | `0.0` | `0.0` | `0.0` | `0.0` | `0.0` | `0.0` |
//!
//! `mean` / `median` / `min_f` / `max_f` / `variance` 都有
//! `if xs.is_empty() { return 0.0 }` 守卫（v0.104.6 引入并经语言作者确认）。
//! **`sum` 是唯一的例外**：`f64::iter().sum()` 的空迭代器初值是 `-0.0`
//! 而非 `0.0` —— 修 `-0.0` 就得在 `sum_value` 里特判，
//! 那会**改掉 D246 收口的既有行为**，属产品契约决定，故只钉不修。
//! （`stats.rs:154` 的注释已承认「`sum([])` 仍是 `-0.0` … 未在本次决定范围内」。）
//!
//! ### ③ 单元素：除 `var` / `stddev` 返回 `0.0` 外，其余**恒等于该元素**
//!
//! `[42].sum() / .mean() / .median() / .min() / .max()` 全是 `42.0`
//! （`variance` 的 `if xs.len() < 2 { return 0.0 }` 守卫，符合统计学定义：
//! 单样本的样本方差无定义，总体方差为 0）。
//!
//! ### ④ 非数值元素**干净报错**，无一静默
//!
//! | 表达式 | 结果 |
//! |---|---|
//! | `["a","b"].sum()` | exit 1 `stats.sum: non-numeric element` |
//! | `[[1],[2]].sum()` | exit 1 同上（嵌套 list 也被拒）|
//! | `stats.sum([1n,2n])` | exit 1 同上（**BigInt 被拒**）|
//! | `["a","b"].mean()` | exit 1 `stats.mean: non-numeric element` |
//!
//! 错误**逐方法点名**（`stats.sum` / `stats.mean` / …），不是笼统的「bad input」。
//!
//! ### ⑤ **混合元素被 typeck 挡在编译期**，根本到不了运行期
//!
//! | 表达式 | 结果 |
//! |---|---|
//! | `[1,"a"].sum()` | **exit 2** typeck：元素必须同质 |
//! | `[1,nil].mean()` | exit 2 同上 |
//! | `[true,1].sum()` | exit 2 同上 |
//!
//! ⇒ ④ 的「非数值元素」只在**全列表同质**时可达（如全 String / 全嵌套 list）。
//! 两条防线**不重叠**，本条两组都钉。
//!
//! ### ⑥ `quantile` / `histogram` / `corr` / `cov` 的边界
//!
//! | 表达式 | 结果 |
//! |---|---|
//! | `stats.quantile([1,2,3,4],0.0)` | `1.0`（闭区间端点）|
//! | `stats.quantile([1,2,3,4],1.0)` | `4.0` |
//! | `stats.quantile([],0.5)` | `0.0`（空列表守卫）|
//! | `stats.quantile([1,2,3],1.5)` | exit 1 `q must be in [0, 1]` |
//! | `stats.quantile([1,2,3])` | exit 1 `requires (list, q)` |
//! | `stats.quantile([1,2,3],"x")` | exit 1 同上 |
//! | `stats.corr([1,2,3],[1,2])` | exit 1 `lists must have equal length` |
//! | `stats.corr([],[])` | `0.0` |
//! | `stats.corr([1,1,1],[1,2,3])` | **`0.0`**（零方差 → 分母为 0 → 守卫返回 0）|
//! | `stats.cov([],[])` | `0.0`（v0.104.6 D143 已修 `nan`）|
//! | `stats.corr([1],[2])` | `0.0`（单元素零方差）|
//!
//! ### ⑦ `histogram` 的 `bins` 守卫（v0.104.6 D284 + **D329**）
//!
//! | `bins` | 结果 |
//! |---|---|
//! | `2` / `1` | 正常直方图 |
//! | `2.0` | **放行**（浮点写的整数）|
//! | `0` | `[]`（显式 0 的既有语义）|
//! | **`2.5` / `0.5`** | **exit 1** `bins must be an integer`（**D329 修**）|
//! | `-1` / `-0.5` | exit 1（D284 已挡）|
//! | `1000000` | 放行，耗时 **1.67 s**（不崩，见下）|
//! | `1000001` | exit 1（超出 `HISTOGRAM_MAX_BINS`）|
//!
//! ⚠ 上界 `1_000_000` 是个**判断题**（D284 原话「若认为过严/过松，改
//! `stats.rs` 里那一个常量即可」）。实测 `bins = 1e6` 产出 1e6 个 dict，
//! **1.67 s / 输出约 61 MB** —— 不 panic、不 OOM，但作为交互式调用的
//! 反馈是灾难性的。若将来要收紧，改 `stats.rs:67` 的那一个常量即可。

use std::process::Command;

fn run(src: &str, tag: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("mora_d329_stats_{}", slug(tag)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, src).expect("写探针");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe).arg(&p).output().expect("跑 mora");
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&out.stdout).into_owned()
        + "\n"
        + &String::from_utf8_lossy(&out.stderr);
    let first = text
        .lines()
        .map(str::trim)
        .find(|l| {
            !l.is_empty()
                && !l.starts_with("Mora v")
                && !l.starts_with("AI:")
                && !l.starts_with("AI 原语")
                && !l.starts_with("显式 API")
                && !l.starts_with("Trait 系统")
                && !l.starts_with("Built-in")
                && !l.starts_with("v0.15 CLI")
                && !l.starts_with('⚠')
                && !l.starts_with("[9layer]")
                && !l.contains(&p.to_string_lossy().to_string())
        })
        .unwrap_or("<empty>")
        .to_string();
    (out.status.code().unwrap_or(-1), first)
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn ev(e: &str) -> (i32, String) {
    run(&format!("print({e})\n"), e)
}

/// **契约 ①**：方法形态与 `stats.*` 孪生**逐字一致**，且**恒返回 Float**。
#[test]
fn d329_method_and_builtin_agree_and_always_return_float() {
    for (m, f, want) in [
        ("[3,1,4,1,5].sum()", "stats.sum([3,1,4,1,5])", "14.0"),
        ("[3,1,4,1,5].mean()", "stats.mean([3,1,4,1,5])", "2.8"),
        ("[3,1,4,1,5].median()", "stats.median([3,1,4,1,5])", "3.0"),
        ("[3,1,4,1,5].var()", "stats.var([3,1,4,1,5])", "2.56"),
        ("[3,1,4,1,5].stddev()", "stats.stddev([3,1,4,1,5])", "1.6"),
        ("[3,1,4,1,5].min()", "stats.min([3,1,4,1,5])", "1.0"),
        ("[3,1,4,1,5].max()", "stats.max([3,1,4,1,5])", "5.0"),
    ] {
        let (c1, o1) = ev(m);
        let (c2, o2) = ev(f);
        assert_eq!(c1, 0, "`{m}` 应成功; 实得 exit={c1} out={o1}");
        assert_eq!(c2, 0, "`{f}` 应成功; 实得 exit={c2} out={o2}");
        assert_eq!(o1, o2, "`{m}` 与 `{f}` 应一致; 实得 {o1} vs {o2}");
        assert_eq!(o1, want, "`{m}` 应得 {want}; 实得 {o1}");
    }
    // 类型恒为 float（**全 Int 输入也是**）
    for e in [
        "type_of([1,2,3].sum())",
        "type_of([1,2].min())",
        "type_of([1,2].max())",
        "type_of([1,2].mean())",
        "type_of(stats.sum([1,2]))",
    ] {
        let (c, o) = ev(e);
        assert_eq!(c, 0, "`{e}` 应成功; 实得 exit={c} out={o}");
        assert_eq!(
            o, "float",
            "`{e}` 应为 float（sum_value 恒转 Float）; 实得 {o}"
        );
    }
}

/// **契约 ②**：空列表一律返回 `0.0` —— **`sum` 是唯一例外，给 `-0.0`**。
///
/// `sum` 的 `-0.0` 是 `f64::iter().sum()` 空迭代器的初值。改它要动
/// D246 收口的既有行为（属产品契约决定），故本条**钉住现状**。
/// 若本条失败，说明有人改了 `sum([])` 的语义 —— 请先回 CHANGELOG
/// 找裁决依据，不要直接改判据。
#[test]
fn d329_empty_list_all_zero_except_sum_is_negative_zero() {
    for (e, want) in [
        ("[].mean()", "0.0"),
        ("[].median()", "0.0"),
        ("[].stddev()", "0.0"),
        ("[].var()", "0.0"),
        ("[].min()", "0.0"),
        ("[].max()", "0.0"),
        // 唯一例外：空迭代器初值
        ("[].sum()", "-0.0"),
    ] {
        let (c, o) = ev(e);
        assert_eq!(c, 0, "`{e}` 应成功; 实得 exit={c} out={o}");
        assert_eq!(o, want, "`{e}` 应得 {want}; 实得 {o}");
    }
}

/// **契约 ③**：单元素 —— 除 `var` / `stddev` 为 `0.0`，其余恒等于该元素。
#[test]
fn d329_single_element_equals_the_element_except_variance() {
    for (e, want) in [
        ("[42].sum()", "42.0"),
        ("[42].mean()", "42.0"),
        ("[42].median()", "42.0"),
        ("[42].min()", "42.0"),
        ("[42].max()", "42.0"),
        // 单样本方差无定义 ⇒ 0.0
        ("[42].var()", "0.0"),
        ("[42].stddev()", "0.0"),
    ] {
        let (c, o) = ev(e);
        assert_eq!(c, 0, "`{e}` 应成功; 实得 exit={c} out={o}");
        assert_eq!(o, want, "`{e}` 应得 {want}; 实得 {o}");
    }
}

/// **契约 ④**：非数值元素（**同质列表**才能到运行期）干净报错且**逐方法点名**。
#[test]
fn d329_non_numeric_elements_error_per_method() {
    for (e, want_ctx) in [
        ("[\"a\",\"b\"].sum()", "stats.sum"),
        ("[\"a\",\"b\"].mean()", "stats.mean"),
        ("[\"a\",\"b\"].median()", "stats.median"),
        ("[\"a\",\"b\"].var()", "stats.var"),
        ("[\"a\",\"b\"].min()", "stats.min"),
        ("[\"a\",\"b\"].max()", "stats.max"),
        // 嵌套 list 与 BigInt 同样被拒
        ("[[1],[2]].sum()", "stats.sum"),
        ("stats.sum([1n,2n])", "stats.sum"),
    ] {
        let (c, o) = ev(e);
        assert_eq!(c, 1, "`{e}` 应干净报错（exit 1）; 实得 exit={c} out={o}");
        assert!(
            o.contains("non-numeric element"),
            "`{e}` 的错误应说明是非数值元素; 实得: {o}"
        );
        assert!(
            o.contains(want_ctx),
            "`{e}` 的错误应点名 `{want_ctx}`（逐方法而非笼统 bad input）; 实得: {o}"
        );
    }
}

/// **契约 ⑤**：混合元素被 **typeck 在编译期**挡住 —— 与 ④ 两条防线不重叠。
#[test]
fn d329_mixed_elements_rejected_by_typeck_before_runtime() {
    for e in ["[1,\"a\"].sum()", "[1,nil].mean()", "[true,1].sum()"] {
        let (c, o) = ev(e);
        assert_eq!(
            c, 2,
            "`{e}` 应被 typeck 拒绝（exit 2）而不是运行期报错; 实得 exit={c} out={o}"
        );
        assert!(
            o.contains("同质") || o.contains("homogeneous"),
            "`{e}` 的错误应说明元素必须同质; 实得: {o}"
        );
    }
}

/// **契约 ⑥**：`quantile` / `corr` / `cov` 的边界。
#[test]
fn d329_quantile_corr_cov_boundaries() {
    // 闭区间端点 + 空列表守卫
    for (e, want) in [
        ("stats.quantile([1,2,3,4],0.0)", "1.0"),
        ("stats.quantile([1,2,3,4],1.0)", "4.0"),
        ("stats.quantile([1,2,3,4],0.5)", "2.5"),
        ("stats.quantile([],0.5)", "0.0"),
    ] {
        let (c, o) = ev(e);
        assert_eq!(c, 0, "`{e}` 应成功; 实得 exit={c} out={o}");
        assert_eq!(o, want, "`{e}` 应得 {want}; 实得 {o}");
    }
    // q 越界 / 缺参 / 类型错
    for e in [
        "stats.quantile([1,2,3],1.5)",
        "stats.quantile([1,2,3],-0.1)",
    ] {
        let (c, o) = ev(e);
        assert_eq!(c, 1, "`{e}` 应报错; 实得 exit={c} out={o}");
        assert!(o.contains("[0, 1]"), "`{e}` 应说明 q 范围; 实得: {o}");
    }
    for e in ["stats.quantile([1,2,3])", "stats.quantile([1,2,3],\"x\")"] {
        let (c, o) = ev(e);
        assert_eq!(c, 1, "`{e}` 应报错; 实得 exit={c} out={o}");
        assert!(
            o.contains("requires (list, q)"),
            "`{e}` 应说明需要 (list, q) 两个参数; 实得: {o}"
        );
    }
    // corr / cov：长度不等报错；空列表与零方差返回 0.0（D143）
    for (e, want) in [
        ("stats.corr([],[])", "0.0"),
        ("stats.cov([],[])", "0.0"),
        ("stats.corr([1,1,1],[1,2,3])", "0.0"),
        ("stats.corr([1],[2])", "0.0"),
        ("stats.cov([1],[2])", "0.0"),
    ] {
        let (c, o) = ev(e);
        assert_eq!(c, 0, "`{e}` 应成功; 实得 exit={c} out={o}");
        assert_eq!(o, want, "`{e}` 应得 {want}; 实得 {o}");
    }
    for e in ["stats.corr([1,2,3],[1,2])", "stats.cov([1,2,3],[1,2])"] {
        let (c, o) = ev(e);
        assert_eq!(c, 1, "`{e}` 应报错; 实得 exit={c} out={o}");
        assert!(
            o.contains("equal length"),
            "`{e}` 应说明长度必须相等; 实得: {o}"
        );
    }
}

/// **契约 ⑦**：`histogram` 的 `bins` 守卫 —— D284（负数 / 上界）+ D329（整数）。
///
/// 详细推导见 `stats_histogram_integer_bins.rs`；本条只从**源码**侧
/// 钉住上界常量不变，防止有人「顺手收紧」到破坏 1e6 的合法用法。
#[test]
fn d329_histogram_max_bins_constant_unchanged() {
    let src = include_str!("../src/interpreter/builtins/stats.rs");
    assert!(
        src.contains("const HISTOGRAM_MAX_BINS: usize = 1_000_000;"),
        "HISTOGRAM_MAX_BINS 应仍为 1_000_000（D284 判定为判断题，改动需走 CHANGELOG）"
    );
}

/// ⑦ 的行为侧：`bins = 0` 静默返回空列表是**既有合法语义**（D284 判据
/// `d284_explicit_zero_bins_still_returns_empty_list` 的孪生），
/// 非整数则是 D329 新增的拒绝面。两者在此并列，防止将来只改其一时
/// 把另一条悄悄带走。
#[test]
fn d329_histogram_zero_and_over_bound() {
    let (c, o) = ev("stats.histogram([1,2,3,4],0)");
    assert_eq!(c, 0, "bins=0 仍应成功; 实得 exit={c} out={o}");
    assert_eq!(o, "[]", "bins=0 的既有行为是空列表; 实得: {o}");

    let (c, o) = ev("stats.histogram([1,2,3,4],1000001)");
    assert_eq!(c, 1, "超上界应报错; 实得 exit={c} out={o}");
    assert!(o.contains("1000000"), "错误应写明上界; 实得: {o}");
}
