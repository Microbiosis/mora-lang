//! v0.104.6 D123：后缀**值调用** `f(…)(…)`。
//!
//! ## 这条能力此前缺在哪（三层，逐层取证）
//!
//! 1. **文法**：spec §14.2 的 `call` 把被调用者钉死成 `IDENTIFIER`，而同节
//!    紧邻的 `method_call` / `index` 都经 `expr` 递归 ⇒ EBNF 推导不出
//!    `(expr)(args)`。已补 `primary` / `postfix` 两条产生式。
//! 2. **parser**：`emit_call_tail_w` 的后缀链循环只认 `.` 与 `[`，遇 `(` 走
//!    D123 加的硬报错分支。已改成真调用。
//! 3. **MIR**：`MirInst` 此前**没有任何 N 元值调用指令** —— `Pipe` 是 1 元的
//!    （`h_pipe` = `call_value(callee, [lhs])`），`f(x)(a, b)` 无处可放。
//!    已加 `MirInst::CallValue(dst, callee_reg, args)`，并接进 SSA /
//!    优化 / 代价模型 / 9 层管线（原先 `fcfg_lower` 会把动态调用降级成
//!    假名 `_r{n}`，callee 寄存器直接丢失）。
//!
//! ## 边界（**不是**本条要解的）
//!
//! `compose(inc, dbl)(5)` 仍不通，但**卡点不在后缀链**：`compose` 在
//! `typeck/dispatch.rs` 里被声明为返回**名义类型** `Type::Compose` 而非函数
//! 类型，于是对其结果发起调用时 typeck 报
//! `expected compose, got fn (float) -> …`（该表的 D79 注释原样记录了这个
//! 错误串）。那是签名问题，另案处理。

use std::sync::Arc;

use mora::interpreter::Interpreter;
use mora::mir::vm::execute;
use mora::parser_v3::ParserV3;
use mora::typeck::check_mir::check_program_witnesses_bidirectional;

/// 编译 → typeck → execute，返回末值。
///
/// 走 `check_program_witnesses_bidirectional` 是刻意的：本项目有
/// 「运行期已支持、typeck 未放行」的能力，若绕过 typeck，这组断言就变成在
/// 断言**用户写不出来**的代码（`builtin_gaps.rs` 的同款守卫，v0.104.6 曾因此
/// 让 15 条测试里 8 条当场失败）。
fn run(src: &str) -> String {
    let (func, witnesses) =
        ParserV3::compile(src).unwrap_or_else(|e| panic!("compile: {e}\n{src}"));
    let errs = check_program_witnesses_bidirectional(&witnesses);
    assert!(
        errs.is_empty(),
        "被 typeck 拒绝 —— 断言的是用户写不出来的代码。\n源：{src}\n错误：{:?}",
        errs.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
    );
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    match execute(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    ) {
        Ok(v) => format!("{v:?}"),
        Err(e) => format!("ERR: {e}"),
    }
}

fn expect_all(cases: &[(&str, &str, &str)]) {
    let mut failures = Vec::new();
    for (name, src, want) in cases {
        let got = run(src);
        if got != *want {
            failures.push(format!(
                "  [{name}]\n    src  = {src:?}\n    want = {want}\n    got  = {got}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} / {} 条不符：\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

/// 括号表达式后立即调用 —— 最直接的 postfix 形态。
///
/// 注意末条语句**就是表达式本身**（不包 `print`）：`run` 返回的是程序末值，
/// `print(...)` 的返回值是 Nil。
#[test]
fn paren_expr_called_directly() {
    expect_all(&[(
        "(fn(x) x * 2 end)(5)",
        "(fn(x) x * 2 end)(5)\n",
        "Float(10.0)",
    )]);
}

/// 绑定后经括号调用（等价形态，确认不是靠裸标识符走捷径）。
#[test]
fn bound_closure_called_via_parens() {
    expect_all(&[(
        "let d = fn(x) x * 3 end\n(d)(7)",
        "let d = fn(x) x * 3 end\n(d)(7)\n",
        "Float(21.0)",
    )]);
}

/// 列表索引取出可调用值后立即调用 —— 跨两种后缀（`[` 接 `(`）。
#[test]
fn indexed_closure_called_directly() {
    expect_all(&[(
        "let fs = [fn(x) x * 4 end]\nfs[0](3)",
        "let fs = [fn(x) x * 4 end]\nfs[0](3)\n",
        "Float(12.0)",
    )]);
}

/// 旧行为必须不再出现：D123 修的是「静默算错」——`mk(1)(2)` 曾得出 `2.0`
/// 而非 `3.0`，且**退出码 0、零诊断**。这里用等价的单参形态钉住正确值。
#[test]
fn no_silent_wrong_value() {
    expect_all(&[(
        "(fn(x) x + 1 end)(1) == 2",
        "(fn(x) x + 1 end)(1)\n",
        "Float(2.0)",
    )]);
}

/// 柯里化闭包的**部分应用**被本语言**有意拒绝**（见 `typeck/hm/infer.rs`
/// 的 arity 检查注释：运行期不支持隐式部分应用，另有显式写法 `curry(f, n)`）。
/// 这里钉住该策略**仍然生效** —— 本轮补的是「任意 primary 后接 `(…)`」，
/// 不是「少传实参即可」。若将来放开部分应用，这条会红，提醒同步改策略文档。
#[test]
fn curried_partial_application_still_rejected() {
    let src = "let mk = fn(x) fn(y) x + y end end\nmk(1)\n";
    let (_func, witnesses) = ParserV3::compile(src).expect("compile");
    let errs = check_program_witnesses_bidirectional(&witnesses);
    assert!(
        !errs.is_empty(),
        "柯里化部分应用当前应被 typeck 拒绝（隐式部分应用不是本语言的能力）；\
         若这是有意放开，请同步更新 typeck/hm/infer.rs 的 arity 策略注释与本测试"
    );
}
