//! v0.104.6 D144：`random.rand_int` / `rand_float` 区间**写反**时静默给出错值（已修）。
//!
//! ## 缺陷
//!
//! ```mora
//! random.rand_int(5, 1)      -- 修复前：5.0，exit 0，零诊断
//! ```
//!
//! 根因：`next_i64_in` 的 `if max <= min { return min }` 把两件事混为一谈 ——
//! * `max == min` 是**单点区间**，返回 min **合理**
//! * `max < min` 是**参数写反**，返回一个确定值却**看不出出错**
//!
//! `rand_float` 同理：`min + (max - min) * rand` 在 `max < min` 时仍落在
//! 两数之间，只是方向反了，用户完全看不出来。
//!
//! 内部调用点（`rand_choice` / `shuffle`）传的都是 `(0, len)` 形式，
//! `max >= min` 恒成立，不受影响；`rand_choice` 另有空列表守卫。

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

/// D144 主判据：区间写反必须**报错**，且错误要说明「写反」。
#[test]
fn d144_reversed_rand_int_range_is_rejected() {
    let res = run("random.rand_int(5, 1)\n");
    assert!(
        res.is_err(),
        "区间写反必须报错（修复前得 5.0，exit 0，零诊断）; 实际: {res:?}"
    );
    let e = res.unwrap_err();
    assert!(
        e.contains("区间写反"),
        "错误信息应点明「区间写反」; 实际: {e}"
    );
}

/// `rand_float` 同样要拒（它此前静默产出**反向区间**内的随机值）。
#[test]
fn d144_reversed_rand_float_range_is_rejected() {
    let res = run("random.rand_float(5.0, 1.0)\n");
    assert!(res.is_err(), "rand_float 区间写反必须报错; 实际: {res:?}");
    assert!(
        res.unwrap_err().contains("区间写反"),
        "错误信息应点明「区间写反」"
    );
}

/// 反向对照：**单点区间**（`min == max`）必须仍可用 —— 它不是错误。
///
/// 这一条挡住「顺手把 `max <= min` 改成 `max < min`」的过度收紧。
#[test]
fn d144_degenerate_equal_range_still_works() {
    assert_eq!(
        run("random.rand_int(7, 7)\n").unwrap(),
        "7.0",
        "`rand_int(7, 7)` 是单点区间，应返回 7，不该被误拒"
    );
    assert_eq!(
        run("random.rand_float(2.5, 2.5)\n").unwrap(),
        "2.5",
        "`rand_float(2.5, 2.5)` 是单点区间，应返回 2.5"
    );
}

/// 反向对照：正常区间的结果必须落在 `[min, max)` 内（不越界）。
#[test]
fn d144_normal_ranges_stay_in_bounds() {
    for i in 0..30 {
        let v: f64 = run("random.seed(1)\nrandom.rand_int(3, 6)\n")
            .unwrap_or_else(|e| panic!("正常区间不应报错: {e}"))
            .parse()
            .expect("应为数字");
        assert!(
            (3.0..6.0).contains(&v),
            "第 {i} 次 `rand_int(3, 6)` 得 {v}，越出 [3, 6)"
        );
    }
    let f: f64 = run("random.rand_float(-1.0, 1.0)\n")
        .unwrap()
        .parse()
        .expect("应为数字");
    assert!(
        (-1.0..1.0).contains(&f),
        "`rand_float(-1, 1)` 得 {f}，越出 [-1, 1)"
    );
}

/// 对照组：`rand_choice` 的**空列表守卫本就存在**（本次未改动，钉住防回退）。
#[test]
fn d144_rand_choice_still_rejects_empty_list() {
    let res = run("random.rand_choice([])\n");
    assert!(res.is_err(), "空列表取样必须报错（`items[0]` 会越界）");
    assert!(res.unwrap_err().contains("empty list"), "错误应点明空列表");
}
