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

use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::typeck::check_mir::check_program_witnesses;

/// 编译 → typeck → execute，返回末值。
fn run(src: &str) -> String {
    let (prog, witnesses) =
        ParserV3::compile(src).unwrap_or_else(|e| panic!("compile: {e}\n{src}"));
    let errs = check_program_witnesses(&witnesses);
    assert!(
        errs.is_empty(),
        "被 typeck 拒绝 —— 断言的是用户写不出来的代码。\n源：{src}\n错误：{:?}",
        errs.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
    );
    let arc = Arc::new(prog);
    let mut core = CoreRuntime::default();
    match execute_program(arc, &mut core) {
        Ok((v, _)) => format!("{v:?}"),
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
    expect_all(&[("(fn(x) x * 2 end)(5)", "(fn(x) x * 2 end)(5)\n", "Int(10)")]);
}

/// 绑定后经括号调用（等价形态，确认不是靠裸标识符走捷径）。
#[test]
fn bound_closure_called_via_parens() {
    expect_all(&[(
        "let d = fn(x) x * 3 end\n(d)(7)",
        "let d = fn(x) x * 3 end\n(d)(7)\n",
        "Int(21)",
    )]);
}

/// 列表索引取出可调用值后立即调用 —— 跨两种后缀（`[` 接 `(`）。
#[test]
fn indexed_closure_called_directly() {
    expect_all(&[(
        "let fs = [fn(x) x * 4 end]\nfs[0](3)",
        "let fs = [fn(x) x * 4 end]\nfs[0](3)\n",
        "Int(12)",
    )]);
}

/// 旧行为必须不再出现：D123 修的是「静默算错」——`mk(1)(2)` 曾得出 `2`
/// 而非 `3`，且**退出码 0、零诊断**。这里用等价的单参形态钉住正确值。
#[test]
fn no_silent_wrong_value() {
    expect_all(&[(
        "(fn(x) x + 1 end)(1) == 2",
        "(fn(x) x + 1 end)(1)\n",
        "Int(2)",
    )]);
}

/// 二元闭包的部分应用（少传参数）仍被 typeck 拒绝；
/// 而显式柯里化单参函数 `fn(x) fn(y) x + y end end` 的 1 参调用合法返回内层闭包。
#[test]
fn curried_partial_application_still_rejected() {
    let src_partial = "let add = fn(x, y) x + y end\nadd(1)\n";
    let (_func, witnesses) = ParserV3::compile(src_partial).expect("compile");
    let errs = check_program_witnesses(&witnesses);
    assert!(
        !errs.is_empty(),
        "二元闭包少传实参的部分应用应被 typeck 拒绝"
    );

    let src_curried = "let mk = fn(x) fn(y) x + y end end\nmk(1)(2)\n";
    let (_func2, witnesses2) = ParserV3::compile(src_curried).expect("compile");
    assert!(
        check_program_witnesses(&witnesses2).is_empty(),
        "单参柯里化闭包完整调用 mk(1)(2) 应合法通过"
    );
}
