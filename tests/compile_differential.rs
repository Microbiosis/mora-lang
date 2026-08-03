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
    // witness 同步产出（嵌套树）：每个顶层语句一个 witness，
    // 与 parse→lower 的顶层 expr 数一致（阶段 3 目标形态）。
    assert_eq!(
        witnesses.len(),
        exprs.len(),
        "compile 的顶层 witness 数应与顶层 expr 数一致\nsource: {source}"
    );
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

#[test]
fn compile_equivalent_prompt() {
    assert_compile_equivalent("print(p\"hello {name}\")");
    assert_compile_equivalent("let msg = p\"score: {n} points\"\nprint(msg)");
}

/// v0.75.76: 回归测试 — 顶层 `let f` 绑定 + 裸函数调用（compile 主路径）。
/// 修复前：take_env 移出 core.environment 后 h_define 写 run_mir 的 env 参数，
/// 而 call_function 兜底查 core（空壳）→ `f(5)` 报 "Undefined function or task"。
/// 修复：h_call 用执行 env 直查用户 callable（与 h_define 同容器），无锁、无死锁。
#[test]
fn compile_bare_user_function_call() {
    let src = "let f = fn(x) x * 2 end\nprint(f(5))";
    let (func, witnesses) = ParserV3::compile(src).expect("compile should succeed");
    let type_errors = mora::typeck::check_mir::check_program_witnesses(&witnesses);
    assert!(
        type_errors.is_empty(),
        "typeck should pass: {:?}",
        type_errors
    );

    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let func_arc = std::sync::Arc::new(func);
    mora::mir::vm::run_mir(&func_arc, &mut interp, &mut env)
        .expect("run_mir should not fail (no Undefined function panic)");
    mora::mir::vm::run_main_task(&func_arc, &mut interp, &mut env)
        .expect("run_main_task should succeed");
}

/// v0.75.77: 回归测试 — 闭包捕获顶层绑定（compile 主路径）。
/// 修复前：h_closure 用 interp.environment()（宿主全局槽）捕获闭包环境，
/// take_env 移空后捕获到空壳 → 闭包体 `x + base` 查不到 base（
/// "Operands must be two numbers..." / Undefined）。修复：h_closure 捕获
/// 执行 env 参数（与 h_define 同一容器，单一来源），无全局槽读取。
#[test]
fn compile_closure_captures_top_level_binding() {
    let src = "let base = 10\nlet offset = fn(x) x + base end\nprint(offset(5))";
    let (func, witnesses) = ParserV3::compile(src).expect("compile should succeed");
    let type_errors = mora::typeck::check_mir::check_program_witnesses(&witnesses);
    assert!(
        type_errors.is_empty(),
        "typeck should pass: {:?}",
        type_errors
    );

    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let func_arc = std::sync::Arc::new(func);
    mora::mir::vm::run_mir(&func_arc, &mut interp, &mut env)
        .expect("run_mir should not fail (closure must see captured base)");
    mora::mir::vm::run_main_task(&func_arc, &mut interp, &mut env)
        .expect("run_main_task should succeed");
}
