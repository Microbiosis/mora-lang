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

/// v0.75.78: 差分等价 — 嵌套构造（end 终止闭包体内 if、for/while 体内 if、
/// 多行 brace 块、顶层 match）。
/// 修复前 compile 主路径缺嵌套上下文构造分发 + emit_block_w `{` 后不跳
/// 换行 → 这些源码 parse→lower 可解析、compile 解析失败（差分不等价）。
/// 注：只断言「两边交集」形态——旧 parse 路径对 brace 闭包体、闭包体内
/// match 亦不可解析（pre-existing 不对称，见 compile_run_nested_constructs
/// 的单侧运行回归）。
#[test]
fn compile_equivalent_nested_constructs() {
    assert_compile_equivalent(
        "let pick = fn(n)\n  if n > 0 {\n    1\n  } else {\n    0\n  }\nend\nprint(pick(3))",
    );
    assert_compile_equivalent(
        "let n = 1\nfor i in [1, 2, 3] {\n  if i > n {\n    print(i)\n  }\n}",
    );
    assert_compile_equivalent(
        "let n = 0\nwhile n < 2 {\n  if n == 0 {\n    n = n + 1\n  }\n}\nprint(n)",
    );
    assert_compile_equivalent("match 0 {\n  0 => print(\"zero\"),\n  _ => print(\"other\"),\n}");
}

/// v0.75.79: 差分等价 — 顶层 task 定义 + if-else 结果（Copy 指令）。
/// 修复 A 前：lower 为无 dst 的 TaskDef 分配死寄存器（n_regs 差 1）；
/// 修复 B 前：if 结果经 env 临时名 `__if_result` 传递（Assign 写未定义
/// 变量静默失败）→ 值语义与指令序列均不等价。
#[test]
fn compile_equivalent_top_level_task_and_if_value() {
    assert_compile_equivalent("task main()\n  print(1)\nend");
    assert_compile_equivalent(
        "task main()\n  let x = 5\n  if x > 3 {\n    print(\"big\")\n  }\nend",
    );
    assert_compile_equivalent("let pick = fn(n) if n > 0 { 1 } else { 0 } end\nprint(pick(3))");
}

/// v0.75.78: 回归测试 — compile 主路径解析并执行嵌套构造（run_mir 运行）。
/// 修复前：task 体内 if/let、闭包体 if、for 体内 if 均编译失败。
#[test]
fn compile_run_nested_constructs() {
    let src = "task main()\n  let x = 5\n  if x > 3 {\n    print(\"big\")\n  }\nend";
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
        .expect("run_mir should not fail (nested if in task body)");
    mora::mir::vm::run_main_task(&func_arc, &mut interp, &mut env)
        .expect("run_main_task should succeed");
}

/// v0.75.79: 回归测试 — if-else 表达式值经寄存器传递（Copy 指令，运行验证）。
/// 修复前：if 结果经 env 临时名 `__if_result` 传递（Assign 写未定义变量
/// 静默失败）→ `fn(n) if c {1} else {0} end` 的 else 值丢失（pick(0) 返回
/// Nil 而非 0）。修复后：分支值经 Copy 直写公共 dst，无 env 依赖。
#[test]
fn compile_run_if_value_passed_by_register() {
    let src = "let pick = fn(n) if n > 0 { 1 } else { 0 } end\nprint(pick(3))\nprint(pick(0))";
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
    mora::mir::vm::run_mir(&func_arc, &mut interp, &mut env).expect("run_mir should not fail");
    mora::mir::vm::run_main_task(&func_arc, &mut interp, &mut env)
        .expect("run_main_task should succeed (if-else value must not be lost)");
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

/// v0.75.81: 回归测试 — 事务块 commit 路径（compile 主路径运行）。
/// 前端：transaction 块 + commit 语句（spec 9.3）。commit 为 no-op，
/// body 正常执行（h_transaction run_isolated 得 Ok）。
#[test]
fn compile_run_transaction_commit() {
    let src = "transaction\n  print(\"tx-body\")\n  commit\nend\nprint(\"done\")";
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
    mora::mir::vm::run_mir(&func_arc, &mut interp, &mut env).expect("run_mir should not fail");
    mora::mir::vm::run_main_task(&func_arc, &mut interp, &mut env)
        .expect("run_main_task should succeed (commit path)");
}

/// v0.75.81: 回归测试 — 事务块 rollback + compensation 路径。
/// rollback 使 h_transaction 执行 compensation 后抛 "Transaction rolled back"。
#[test]
fn compile_run_transaction_rollback() {
    let src =
        "transaction\n  rollback\ncompensation\n  print(\"compensated\")\nend\nprint(\"after\")";
    let (func, _w) = ParserV3::compile(src).expect("compile should succeed");
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let func_arc = std::sync::Arc::new(func);
    // 顶层 transaction：Rollback 使 h_transaction 执行 compensation 后
    // 抛错，经 run_mir 上抛（run_main_task 不达）。
    let err = mora::mir::vm::run_mir(&func_arc, &mut interp, &mut env)
        .expect_err("rollback must surface Transaction rolled back");
    assert!(
        err.contains("Transaction rolled back"),
        "unexpected error: {}",
        err
    );
}

/// v0.75.81: 回归测试 — eval 断言语句（α.8 Eval 原语前端）。
/// 语法：`eval ["name"] given_expr, expect1, expect2, ...`。
/// 通过断言（given == expect）正常返回；失败断言返回错误。
#[test]
fn compile_run_eval_assertion() {
    let src = "eval \"sanity\" 2 + 2, 4\nprint(\"ok\")";
    let (func, _w) = ParserV3::compile(src).expect("compile should succeed");
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let func_arc = std::sync::Arc::new(func);
    mora::mir::vm::run_mir(&func_arc, &mut interp, &mut env).expect("run_mir should not fail");
    mora::mir::vm::run_main_task(&func_arc, &mut interp, &mut env)
        .expect("run_main_task should succeed (eval passes)");

    // 失败断言：given != expect → 错误上抛（顶层语句，经 run_mir 上抛）
    let src_bad = "eval \"bad\" 2 + 2, 5";
    let (func_bad, _w2) = ParserV3::compile(src_bad).expect("compile should succeed");
    let mut interp2 = mora::interpreter::Interpreter::new();
    let mut env2 = interp2.take_env();
    let arc2 = std::sync::Arc::new(func_bad);
    let err = mora::mir::vm::run_mir(&arc2, &mut interp2, &mut env2)
        .expect_err("failing eval must surface assertion error");
    assert!(
        err.contains("assertion failed"),
        "unexpected error: {}",
        err
    );
}
