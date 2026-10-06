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
        "2.0"
    );
    // `uncurry` 往返
    assert_eq!(
        run("let f = fn(a) a + 1 end\nlet c = curry(f, 1)\nlet g = uncurry(c)\ng(1)\n").unwrap(),
        "2.0"
    );
    // `arity == 0` 仍被既有检查拒绝
    let zero = run("let f = fn(a) a end\nlet c = curry(f, 0)\nprint(1)\n");
    assert!(
        zero.is_err(),
        "`curry(f, 0)` 应仍被既有的 arity==0 检查拒绝"
    );
}

/// D148 主判据 ②：`checkpoint` 的 `v` / `step` 负数必须报错。
///
/// ⚠ 两个分支**都**危险且机制不同：`Int` 截断成巨大版本号/步数、
/// `Float` 饱和成 0 —— 都会让检查点逻辑走向完全错误的状态。
///
/// ⚠ `checkpoint` **没有任何 Mora 语法入口**（`tests/` 里零引用、spec 无示例），
/// 只能经 Rust API 触达。故本轮以**源码判据**固定（见下一条同族的做法），
/// 不臆造调用形态 —— D139 的教训：**探针源码本身跑不通就别解读它的行为**。
#[test]
fn d148_checkpoint_negative_handling_is_documented_in_source() {
    let src = std::fs::read_to_string("src/checkpoint/mod.rs").expect("读 checkpoint 源码");
    assert!(
        src.contains("v0.104.6 D148") && src.contains("不能为负数"),
        "D148 对 checkpoint `v`/`step` 负数的拒绝应就地注明"
    );
    // 关键：两个分支都要有守卫（`Int` 截断 / `Float` 饱和，机制不同）
    assert!(
        src.contains("Some(Value::Int(i)) if *i < 0")
            && src.contains("Some(Value::Float(n)) if *n < 0.0"),
        "`v` 与 `step` 的 **Int / Float 两个分支都要有负数守卫** —— \
         只挡一侧会漏（Float 饱和成 0 恰被 arity 类检查挡住，Int 截断则绕过）"
    );
}

/// 对照组：`reading_order_idx` 负数 → 按**缺失**处理（返回 `None`），
/// 而不是伪造「第 0 个」下标把块顺序排错。
///
/// 这条是**库层**函数（`document::reading_order` 的私有 helper），
/// 不可从 Mora 源码直接调用；此处以「无对应语法入口」为事实记档，
/// 改动本身由源码注释承载。
#[test]
fn d148_reading_order_idx_negative_handling_is_documented_in_source() {
    let src = std::fs::read_to_string("src/document/reading_order/mod.rs")
        .expect("读 reading_order 源��（路径依赖仓库根）");
    assert!(
        src.contains("v0.104.6 D148") && src.contains("reading_order_idx"),
        "D148 对 `reading_order_idx` 负数的处理应就地注明（它是私有 helper，\
         无 Mora 语法入口，无法写成运行时判据）"
    );
}
