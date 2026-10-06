//! v0.104.6 D146：`take(-1)` 得**空列表**、`drop(-1)` 原样返回 —— 饱和转换的第二处（已修）。
//!
//! ## 缺陷
//!
//! ```mora
//! let xs = [1, 2, 3, 4, 5, 6]
//! xs.take(-1)      -- 修复前：[]          （用户以为「取最后 1 个」）
//! xs.drop(-1)      -- 修复前：全部原样返回
//! ```
//!
//! 两者都 **exit 0、零诊断**。根因与 D145 同源：`Value::Float(n) as usize`
//! 是**饱和转换**，`-1.0 as usize == 0`。
//!
//! `take(-1)` 比 D145 那条更糟：D145 是「压缩结果丢内容」，
//! 这条是**直接返回空列表** —— 调用方无从判断是「列表本来就空」
//! 还是「参数写反了」。
//!
//! ## 同族正确的样板
//!
//! `crush_json(max)` 与 `tail(max)` 早已有 `if *n < 0.0 { return Err(…) }`，
//! `take`/`drop` 漏了。同批还修了 `compress` 的 `k_first` / `k_last`
//! （与 D145 的 `max_bytes` **同函数同型**）。

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

/// D146 主判据：`take` / `drop` 的负数 count 必须报错。
#[test]
fn d146_negative_take_drop_count_is_rejected() {
    for (name, src) in [
        ("take", "[1,2,3].take(-1)\n"),
        ("drop", "[1,2,3].drop(-1)\n"),
        ("take_frac", "[1,2,3].take(-0.5)\n"),
    ] {
        let res = run(src);
        assert!(
            res.is_err(),
            "[{name}] 负数 count 必须报错（修复前 take(-1) 静默返回 []）; 实际: {res:?}"
        );
        assert!(
            res.unwrap_err().contains("不能为负数"),
            "[{name}] 错误信息应点明「不能为负数」"
        );
    }
}

/// 同批：`compress` 的 `k_first` / `k_last` 负数（压缩会**静默失效**）。
#[test]
fn d146_negative_compress_boundary_counts_are_rejected() {
    for key in ["k_first", "k_last"] {
        let src = format!("compress([1,2,3,4,5], \"head_tail\", {{{key}: -5}})\n");
        let res = run(&src);
        assert!(
            res.is_err(),
            "[{key}: -5] 必须报错（修复前**完全不压缩**却 exit 0）; 实际: {res:?}"
        );
    }
}

/// 反向对照：正常 count 的结果**逐字节不变**。
#[test]
fn d146_normal_take_drop_results_are_unchanged() {
    for (name, src, expect) in [
        ("take_2", "[1,2,3,4,5,6].take(2)\n", "[1.0, 2.0]"),
        ("drop_4", "[1,2,3,4,5,6].drop(4)\n", "[5.0, 6.0]"),
        ("take_0", "[1,2,3].take(0)\n", "[]"),
        ("drop_0", "[1,2,3].drop(0)\n", "[1.0, 2.0, 3.0]"),
        ("take_all", "[1,2,3].take(3)\n", "[1.0, 2.0, 3.0]"),
        ("drop_all", "[1,2,3].drop(3)\n", "[]"),
    ] {
        let got = run(src).unwrap_or_else(|e| panic!("[{name}] 正常参数不应报错: {e}"));
        assert_eq!(got, expect, "[{name}] 结果应保持不变");
    }
}

/// 对照组：同族的 `crush_json(max)` / `tail(max)` 早已拒绝负数 —— 钉住防回退。
///
/// v0.104.6 D336 更正：原断言只有 `res.is_err()`，而 `run()` 把
/// **编译错误**也映射成 `Err("COMPILE: …")` ⇒ 任何编译期失败都能满足它。
///
/// 原 `tail` 用例写的是 `tail([1,2,3], 5, -1)` —— 首参是 list、还多给了一个参数，
/// 与 `tail(path, max)` 的签名不符，实测报的是**类型不匹配**（编译期），
/// **根本没走到**负数检查。⇒ 注释里「`tail(max)` 早已拒绝负数」是**未被验证**的。
///
/// 现改为三件事同时做：
/// ① 断言错误**不是** `COMPILE:`（否则就是编译期失败蒙对了）；
/// ② 断言错误**提到**非负校验（这才是在验证那条守卫）；
/// ③ `tail` 改用**真实签名** `tail(path, max)`，path 指向工作区内确实存在的文件。
#[test]
fn d146_siblings_that_already_reject_negatives_still_do() {
    for (name, src, needle) in [
        ("crush_json", "crush_json([1,2,3], -1)\n", "non-negative"),
        // ⚠ 真实签名是 `tail(path, max)` —— 首参必须是**字符串路径**。
        // 修前写的 `tail([1,2,3], 5, -1)` 是三参 + list，编译期就挂了。
        ("tail", "tail(\"Cargo.toml\", -1)\n", "non-negative"),
    ] {
        let res = run(src);
        assert!(
            res.is_err(),
            "[{name}] 本就有的非负校验不得回退（它们是 take/drop 的正确样板）"
        );
        let err = res.unwrap_err();
        assert!(
            !err.starts_with("COMPILE:"),
            "[{name}] 不该是**编译错误** —— 那说明用例本身写错了（签名/元数/类型），\
             蒙对了断言。实际: {err}"
        );
        assert!(
            err.contains(needle),
            "[{name}] 错误信息应点明 `{needle}`（这才是验证那条守卫）; 实际: {err}"
        );
    }
}
