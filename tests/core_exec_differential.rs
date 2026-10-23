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
///
/// 返回 `(值, 效应)` —— **效应也要对拍**：`aggregate` 的贡献走的是效应数据
/// 通道（`Effect::Contribute`），只比返回值的话它**完全不可观测**。
fn exec_mir(src: &str) -> Result<(Value, Effects), String> {
    let (func, witnesses) = mora::parser_v3::ParserV3::compile(src).map_err(|e| e.to_string())?;
    let mir = mora::mir::pipeline::compile_optimized(
        &func,
        &witnesses,
        mora::mir::ssa::OptLevel::default(),
    );
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let mut effects = Effects::new();
    let v = mora::mir::vm::execute(&Arc::new(mir), &mut interp, &mut env, &mut effects)?;
    Ok((v, effects))
}

/// 走 **Core 执行器**执行（`mir::core_exec` 的块调度）。
fn exec_core(src: &str) -> Result<(Value, Effects), String> {
    let (func, witnesses) = mora::parser_v3::ParserV3::compile(src).map_err(|e| e.to_string())?;
    let core = mora::mir::pipeline::compile_core(&func, &witnesses);
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let mut effects = Effects::new();
    let v = mora::mir::core_exec::execute(&core, &mut env, &mut interp, &mut effects)?;
    Ok((v, effects))
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

/// 对拍：两个执行器的**值**与**效应**必须**完全一致**。
fn assert_same(tag: &str, src: &str) {
    assert_typechecks(src);
    let m = exec_mir(src);
    let c = exec_core(src);
    match (&m, &c) {
        (Ok((vm, em)), Ok((vc, ec))) => {
            assert_eq!(
                vm, vc,
                "[{tag}] 两个执行器的**值**不同\n程序：\n{src}\nMIR : {vm:?}\nCore: {vc:?}"
            );
            assert_eq!(
                em, ec,
                "[{tag}] 两个执行器的**效应**不同\n程序：\n{src}\nMIR : {em:?}\nCore: {ec:?}"
            );
        }
        (Err(_), Err(_)) => {} // 两边都报错 ⇒ 一致（错误文案由专门判据核对）
        (m, c) => {
            panic!("[{tag}] 一个成功一个失败 ⇒ 语义分叉\n程序：\n{src}\nMIR : {m:?}\nCore: {c:?}")
        }
    }
}

/// 对拍：两个执行器都应**报错**，且错误文案**都含同一句关键词**。
///
/// 比 `assert_same` 的 `(Err, Err)` 分支**严格**：后者只判「都报错」，
/// ⇒ 「Core 侧根本不支持这个构造所以报错」也会被当成「一致」（**恒真陷阱**）。
fn assert_same_err(tag: &str, src: &str, keyword: &str) {
    assert_typechecks(src);
    let m = exec_mir(src);
    let c = exec_core(src);
    let em = match &m {
        Ok(v) => panic!("[{tag}] MIR 侧应报错，实际成功：{v:?}"),
        Err(e) => e.clone(),
    };
    let ec = match &c {
        Ok(v) => panic!("[{tag}] Core 侧应报错，实际成功：{v:?}"),
        Err(e) => e.clone(),
    };
    assert!(
        em.contains(keyword),
        "[{tag}] MIR 侧错误应含 {keyword:?}（否则可能是「另一个原因」）：{em}"
    );
    assert!(
        ec.contains(keyword),
        "[{tag}] Core 侧错误应含 {keyword:?}（否则可能是「另一个原因」）：{ec}"
    );
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

/// **D483**：曾经「有损」的 6 类构造，现在两个执行器的行为必须**完全一致**。
///
/// 其中 5 类（`eval` / `aggregate` / quasiquote / `commit` / `rollback`）
/// Core 执行器支持 ⇒ 可直接对拍；`orchestrate` 的执行语义（agent 图 / BSP
/// 超步）在**执行层** ⇒ Core 执行器**响亮报错**（不是「假装能跑」）⇒ 它由
/// **形状**判据守住（`core_fidelity::formerly_lossy_constructs_are_now_faithful`）。
///
/// ⚠ 用例是**精选**的：每一条都恰好能区分「忠实」与「D481/D482 的占位」：
/// - `eval "n" 1`：值 = **given 的值**（不是 Nil）—— 占位路径会把它覆盖成 Nil；
/// - `aggregate acc, 1`：贡献进**效应通道** ⇒ 值与效应都要一致；
/// - quasiquote：片段重组为 Code（占位路径下片段全丢 ⇒ 值是 Nil）。
#[test]
fn formerly_lossy_constructs_execute_identically() {
    // 断言通过：语句的值 = **given 的值**（不是 Nil）。
    assert_same("eval 通过", "eval \"n\" 1\n");
    // 断言失败：两个执行器都应报错，且错误**都**含「assertion failed」。
    assert_same_err("eval 失败", "eval \"n\" 1, 2\n", "assertion failed");
    // 聚合：贡献进**效应通道** ⇒ 值与**效应**都要一致。
    assert_same("aggregate", "aggregate acc, 1\n");
    // ⭐ 效应通道**显式**核对：光靠「两边相等」不够 —— 若两边都空，
    // 判据会**恒真**。这里直接看贡献本身。
    for (tag, r) in [
        ("MIR", exec_mir("aggregate acc, 1\n")),
        ("Core", exec_core("aggregate acc, 1\n")),
    ] {
        let effects = r.expect("应成功").1;
        assert_eq!(
            effects.contributions.len(),
            1,
            "[{tag}] 应恰好有一条聚合贡献，实际 {:?}",
            effects.contributions
        );
        assert_eq!(
            effects.contributions[0].name, "acc",
            "[{tag}] 贡献的聚合器名不对"
        );
    }
    // quasiquote：片段重组为 Code。
    assert_same("quasiquote", "let x = 5\n`10+,x\n");
    // 提交：unit 语句（两边都是空操作）。
    assert_same("commit", "commit\n");
    // 回滚：两个执行器都应报「已回滚」（不是静默放行）。
    assert_same_err("rollback", "rollback\n", "rolled back");
}
