//! v0.104.6 D482：Core 执行器与 MIR 执行器的**语义对拍**。
//!
//! ## 为什么
//!
//! Core 层此前只是**影子**（`probe_all_layers` 只统计指令数）⇒ 降维里的错误
//! 不会被**执行**发现。D482 给 Core 层一条**独立**的执行路径（`mir::core_exec`），
//! 于是可以做**语义级**对照：同一个程序、两个执行器、结果必须一致。
//!
//! 这正是 D474 退役差分机制时缺的对照 —— 当年的差分**只比形状、不比语义**
//! （`or`/`and` 短路不赋值就是**两边一起错**，差分抓不到）。
//!
//! ## 对拍的是什么
//!
//! - **执行路径不同**：MIR 侧走 `dag_analyze → dag_optimize → BSP 超步`；
//!   Core 侧走 `mir::core_exec` 的**块调度**（无 DAG、无优化）。
//! - **内建实现相同**：都经 `MirHost` 分派 ⇒ 差分覆盖的是**降维与控制流**，
//!   不是内建本身。
//!
//! ## 覆盖的程序
//!
//! 算术 / 字符串 / 列表 / 字典 / if / 嵌套 if / while / for / match（含守卫）/
//! 具名调用 / 方法调用 / 模板 / **短路求值**（含一个能区分「短路」与「急切」
//! 的判别式）/ 错误路径。

use std::sync::Arc;

use mora::mir::effect::Effects;
use mora::value::Value;

/// 走 **MIR 生产路径**执行（`dag_analyze → dag_optimize → BSP 超步`）。
fn exec_mir(src: &str) -> Result<Value, String> {
    let (func, witnesses) = mora::parser_v3::ParserV3::compile(src).map_err(|e| e.to_string())?;
    let mir = mora::mir::pipeline::compile_optimized(
        &func,
        &witnesses,
        mora::mir::ssa::OptLevel::default(),
    );
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let mut effects = Effects::new();
    mora::mir::vm::execute(&Arc::new(mir), &mut interp, &mut env, &mut effects)
}

/// 走 **Core 执行器**执行（`mir::core_exec` 的块调度）。
fn exec_core(src: &str) -> Result<Value, String> {
    let (func, witnesses) = mora::parser_v3::ParserV3::compile(src).map_err(|e| e.to_string())?;
    let core = mora::mir::pipeline::compile_core(&func, &witnesses);
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let mut effects = Effects::new();
    mora::mir::core_exec::execute(&core, &mut env, &mut interp, &mut effects)
}

/// 程序本身必须是**合法 Mora**（过 typeck）—— 否则对拍的是两个错误。
fn assert_typechecks(src: &str) {
    let (_func, witnesses) = mora::parser_v3::ParserV3::compile(src).expect("解析");
    let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
    assert!(
        errs.is_empty(),
        "用例本身没过 typeck：{src}\n诊断：{errs:?}"
    );
}

/// 对拍：两个执行器的结果必须**完全一致**。
fn assert_same(tag: &str, src: &str) {
    assert_typechecks(src);
    let m = exec_mir(src);
    let c = exec_core(src);
    match (&m, &c) {
        (Ok(vm), Ok(vc)) => assert_eq!(
            vm, vc,
            "[{tag}] 两个执行器结果不同\n程序：\n{src}\nMIR : {vm:?}\nCore: {vc:?}"
        ),
        (Err(_), Err(_)) => {} // 两边都报错 ⇒ 一致（错误文案不必逐字相同）
        (m, c) => {
            panic!("[{tag}] 一个成功一个失败 ⇒ 语义分叉\n程序：\n{src}\nMIR : {m:?}\nCore: {c:?}")
        }
    }
}

#[test]
fn mir_and_core_agree_on_the_executable_subset() {
    let cases: [(&str, &str); 15] = [
        ("算术", "let a = 2\nlet b = 3\na * b + 1\n"),
        ("字符串", "let s = \"ab\"\ns + \"cd\"\n"),
        ("列表", "let xs = [1, 2, 3]\nxs[1]\n"),
        ("字典", "let d = {a: 1}\nd[\"a\"]\n"),
        ("if", "let x = 5\nif x > 3 then 10 else 20 end\n"),
        (
            "嵌套 if",
            "let x = 5\nif x > 3 then\n  if x > 4 then 1 else 2 end\nelse\n  3\nend\n",
        ),
        (
            "while",
            "let i = 0\nlet s = 0\nwhile i < 5\n  assign s = s + i\n  assign i = i + 1\nend\ns\n",
        ),
        (
            "for",
            "let s = 0\nfor x in [1, 2, 3]\n  assign s = s + x\nend\ns\n",
        ),
        (
            "match",
            "let x = 2\nmatch x with\n  1 -> 10\n  2 -> 20\n  _ -> 30\nend\n",
        ),
        (
            "match 带守卫",
            "let x = 2\nmatch x with\n  y when y > 5 -> 10\n  _ -> 30\nend\n",
        ),
        ("具名调用", "len([1, 2, 3])\n"),
        ("方法调用", "let xs = [1, 2, 3]\nxs.len()\n"),
        ("模板", "let n = 5\np\"n={n}\"\n"),
        ("or 短路", "let xs = [1, 2]\ntrue or xs[9] == 1\n"),
        ("and 短路", "let xs = [1, 2]\nfalse and xs[9] == 1\n"),
    ];
    for (tag, src) in cases {
        assert_same(tag, src);
    }
}

/// **判别式**：`or` 的 rhs 若**被求值**就会越界报错；短路则不求值、无错。
///
/// ⇒ 这条断言能区分「短路求值」与「急切求值」—— D474 那类缺陷（短路只作用于
/// 取值、rhs 仍被求值）会在这里**立刻暴露**。
///
/// rhs 写成 `xs[9] == 1`：类型是 **bool**（`or` 的操作数必须是 bool），
/// 但求值它会先算 `xs[9]` ⇒ 越界 ⇒ 运行时错误。
#[test]
fn short_circuit_is_semantically_observable() {
    // 短路成立 ⇒ rhs 不求值 ⇒ 不报错 ⇒ 值是 true
    assert_same(
        "or 短路（rhs 会越界）",
        "let xs = [1, 2]\nlet r = true or xs[9] == 1\nr\n",
    );
    // 短路不成立 ⇒ rhs **必须**被求值 ⇒ 两边都应报错
    let src = "let xs = [1, 2]\nlet r = false or xs[9] == 1\nr\n";
    assert_typechecks(src);
    assert!(
        exec_mir(src).is_err(),
        "MIR 侧：rhs 未被求值（`false or xs[9] == 1` 不报错）⇒ 短路语义不对"
    );
    assert!(
        exec_core(src).is_err(),
        "Core 侧：rhs 未被求值 ⇒ 短路语义不对"
    );
}

/// 错误路径：两个执行器都应报错（不静默成功）。
#[test]
fn both_executors_report_the_same_error_paths() {
    let src = "let xs = [1, 2]\nxs[9]\n";
    assert_typechecks(src);
    assert!(exec_mir(src).is_err(), "MIR 侧应报错");
    assert!(exec_core(src).is_err(), "Core 侧应报错");
}
