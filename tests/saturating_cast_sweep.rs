//! v0.104.6 D148：把 D145–D147 的饱和转换普查**真正收口**（`checkpoint` / `curry` / `reading_order_idx`）。
//!
//! ## 最重要的一处：`curry` 的**两个分支行为不同**
//!
//! `curry` 本来就有 `arity == 0` 的检查，但负数路径**只挡住了一半**：
//!
//! | 实参 | `as usize` 的语义 | 结果 | 是否被 `arity == 0` 挡住 |
//! |---|---|---|---|
//! | `Value::Float(-1.0)` | **饱和** → `0` | `arity = 0` | ✓ 报错 |
//! | `Value::Int(-1)` | **截断** → `usize::MAX` | `arity = 18446744073709551615` | ✗ **绕过** |
//!
//! 于是 `curry(f, -1)` 返回一个**永远凑不齐参数**的 Curry —— 调用多少次
//! 都不产生返回值，**exit 0、零诊断**，即静默挂死。
//!
//! 同一个 `as` 表达式在 `Float` 上饱和、在 `Int` 上截断 —— **只测一侧会漏**。

use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use std::sync::Arc;

fn run(src: &str) -> Result<String, String> {
    let (prog, _w) = ParserV3::compile(src).map_err(|e| format!("COMPILE: {e}"))?;
    let mut core = CoreRuntime::default();
    let arc = Arc::new(prog);
    execute_program(arc, &mut core)
        .map(|(v, _)| format!("{v}"))
        .map_err(|e| e.to_string())
}

/// D148 主判据 ①：`curry` 的负数 arity（`Int` 与 `Float` **两侧**）都必须报错。
///
/// 两侧都要测 —— 只测 `Float` 会被既有的 `arity == 0` 检查「代为通过」，
/// 看不出 `Int` 那条截断路径的洞。
#[test]
fn d148_negative_curry_arity_is_rejected_on_both_sides() {
    for (name, n) in [("int", "-1"), ("float", "-1.0")] {
        let src = format!("let f = fn(a, b) a + b end\nlet c = curry(f, {n})\nprint(1)\n");
        let res = run(&src);
        assert!(
            res.is_err(),
            "[{name}] `curry(f, {n})` 必须报错（修复前 Int 侧绕过检查，\
             得到永远凑不齐参数的 Curry，调用多少次都不返回）; 实际: {res:?}"
        );
        assert!(
            res.unwrap_err().contains("不能为负数"),
            "[{name}] 错误应点明「不能为负数」"
        );
    }
}

/// 反向对照：`curry` 的正常 arity 与既有的 `0` 拒绝**都不得回退**。
///
/// ⚠ 形态：既有测试（`functional_builtins_signatures.rs::curry_family_still_usable`）
/// 用的是 `curry(F, 1)` + `c(1)` —— **分步**调用，不是 `c(1)(2)` 链式。
#[test]
fn d148_normal_curry_arity_still_works() {
    // arity 1 的柯里化：一次调用即得结果
    assert_eq!(
        run("let f = fn(a) a + 1 end\nlet c = curry(f, 1)\nc(1)\n").unwrap(),
        "2"
    );
    // `uncurry` 往返
    assert_eq!(
        run("let f = fn(a) a + 1 end\nlet c = curry(f, 1)\nlet g = uncurry(c)\ng(1)\n").unwrap(),
        "2"
    );
    // `arity == 0` 仍被既有检查拒绝
    let zero = run("let f = fn(a) a end\nlet c = curry(f, 0)\nprint(1)\n");
    assert!(
        zero.is_err(),
        "`curry(f, 0)` 应仍被既有的 arity==0 检查拒绝"
    );
}

// checkpoint / document removed in v0.110.0.
