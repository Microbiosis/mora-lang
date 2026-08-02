//! v0.75.39: ParserV3::compile vs parse→lower 差分测试。
//!
//! 阶段 3（ParserV3 融合 lower 直接 emit MirInst）的核心守卫：compile
//! 是目标形态的入口（最终直接 emit 指令 + 并行产出 witness），parse→lower
//! 是旧路径。两者对同一源码必须产出**指令序列等价**的 MirFunction。
//! 融合过程中，此测试持续锁定「新路径不改变语义」。

use mora::mir::lower::lower_mir_exprs;
use mora::parser_v3::ParserV3;

/// 同一源码：compile 与 parse→lower 的 body 指令序列必须一致。
fn assert_compile_equivalent(source: &str) {
    // 旧路径：parse → lower
    let exprs = mora::interpreter::parse_code_v3(source).expect("parse should succeed");
    let old_func = lower_mir_exprs(&exprs).expect("lower should succeed");

    // 新路径：compile（阶段 3 目标形态）
    let (new_func, witnesses) = ParserV3::compile(source).expect("compile should succeed");

    assert_eq!(
        new_func.body, old_func.body,
        "compile 与 parse→lower 指令序列不等价\nsource: {source}"
    );
    assert_eq!(
        new_func.n_regs, old_func.n_regs,
        "compile 与 parse→lower 寄存器数不等价\nsource: {source}"
    );
    assert_eq!(
        new_func.params, old_func.params,
        "compile 与 parse→lower 参数不等价\nsource: {source}"
    );
    // witness 同步产出（阶段 3 中间态：emit 路径扁平 push，含嵌套叶子；
    // 阶段 4 精化为嵌套树后此断言收紧为顶层计数相等）。
    assert!(
        !witnesses.is_empty(),
        "compile 应产出 witness\nsource: {source}"
    );
    // body 指令等价是核心守卫（语义锁定）；witness 数仅要求非空。
    let _ = exprs.len();
}

#[test]
fn compile_equivalent_literal() {
    assert_compile_equivalent("42");
    assert_compile_equivalent("\"hello\"");
    assert_compile_equivalent("3.14");
    assert_compile_equivalent("true");
}

#[test]
fn compile_equivalent_binary_and_variable() {
    assert_compile_equivalent("let x = 1\nlet y = x + 2\nprint(y)");
    assert_compile_equivalent("print(10 * 2 - 3)");
}

#[test]
fn compile_equivalent_control_flow() {
    assert_compile_equivalent("let n = 3\nif n > 0 { print(\"pos\") }");
    assert_compile_equivalent("let i = 0\nwhile i < 3\n  i = i + 1\nend\nprint(i)");
    assert_compile_equivalent(
        "let items = [1, 2, 3]\nlet sum = 0\nfor x in items\n  sum = sum + x\nend\nprint(sum)",
    );
}

#[test]
fn compile_equivalent_call_and_closure() {
    assert_compile_equivalent("let ops = {\"mul\": fn(x) x * 2 end}\nprint(ops.mul(5))");
    assert_compile_equivalent("print(len([1, 2, 3]))");
}

#[test]
fn compile_equivalent_orchestrate() {
    assert_compile_equivalent(
        "orchestrate sequential input -> result\n  agent a => \"hello\"\nend\nprint(result)",
    );
}

#[test]
fn compile_equivalent_match() {
    assert_compile_equivalent("match 42 {\n  _ => \"default\"\n}");
}
