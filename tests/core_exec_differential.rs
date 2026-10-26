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
/// 返回 `(结果, 效应)` —— **效应始终返回**（哪怕结果报错）：有些构造的
/// 可观测性**只在效应里**（例：`transaction` 的补偿体在隔离环境里跑 ⇒ 它的
/// 环境改动被丢弃，但它的**效应**会向上传播）⇒ 只在成功路径上比效应会漏掉
/// 一整类缺陷。
fn exec_mir(src: &str) -> (Result<Value, String>, Effects) {
    let (func, witnesses) = match mora::parser_v3::ParserV3::compile(src) {
        Ok(v) => v,
        Err(e) => return (Err(e.to_string()), Effects::new()),
    };
    let mir = mora::mir::pipeline::compile_optimized(
        &func,
        &witnesses,
        mora::mir::ssa::OptLevel::default(),
    );
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let mut effects = Effects::new();
    let r = mora::mir::vm::execute(&Arc::new(mir), &mut interp, &mut env, &mut effects);
    (r, effects)
}

/// 走 **Core 执行器**执行（`mir::core_exec` 的块调度）。
fn exec_core(src: &str) -> (Result<Value, String>, Effects) {
    let (func, witnesses) = match mora::parser_v3::ParserV3::compile(src) {
        Ok(v) => v,
        Err(e) => return (Err(e.to_string()), Effects::new()),
    };
    let core = mora::mir::pipeline::compile_core(&func, &witnesses);
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let mut effects = Effects::new();
    let r = mora::mir::core_exec::execute(&core, &mut env, &mut interp, &mut effects);
    (r, effects)
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
    let (m, em) = exec_mir(src);
    let (c, ec) = exec_core(src);
    match (&m, &c) {
        (Ok(vm), Ok(vc)) => {
            assert_eq!(
                vm, vc,
                "[{tag}] 两个执行器的**值**不同\n程序：\n{src}\nMIR : {vm:?}\nCore: {vc:?}"
            );
            assert_eq!(
                em, ec,
                "[{tag}] 两个执行器的**效应**不同\n程序：\n{src}\nMIR : {em:?}\nCore: {ec:?}"
            );
        }
        (Err(_), Err(_)) => {
            // 两边都报错 ⇒ 值一致；**效应也要一致**（补偿体的效应在这里
            // 才看得见 —— 只在成功路径上比效应会漏掉一整类缺陷）。
            assert_eq!(
                em, ec,
                "[{tag}] 两个执行器都报错，但**效应**不同\n程序：\n{src}\nMIR : {em:?}\nCore: {ec:?}"
            );
        }
        (m, c) => {
            panic!("[{tag}] 一个成功一个失败 ⇒ 语义分叉\n程序：\n{src}\nMIR : {m:?}\nCore: {c:?}")
        }
    }
}

/// 对拍：两个执行器都应**报错**，且错误文案**都含同一句关键词**、**效应一致**。
///
/// 比 `assert_same` 的 `(Err, Err)` 分支**严格**：后者只判「都报错」，
/// ⇒ 「Core 侧根本不支持这个构造所以报错」也会被当成「一致」（**恒真陷阱**）。
fn assert_same_err(tag: &str, src: &str, keyword: &str) {
    assert_typechecks(src);
    let (m, em) = exec_mir(src);
    let (c, ec) = exec_core(src);
    let emsg = match &m {
        Ok(v) => panic!("[{tag}] MIR 侧应报错，实际成功：{v:?}"),
        Err(e) => e.clone(),
    };
    let ecsmsg = match &c {
        Ok(v) => panic!("[{tag}] Core 侧应报错，实际成功：{v:?}"),
        Err(e) => e.clone(),
    };
    assert!(
        emsg.contains(keyword),
        "[{tag}] MIR 侧错误应含 {keyword:?}（否则可能是「另一个原因」）：{emsg}"
    );
    assert!(
        ecsmsg.contains(keyword),
        "[{tag}] Core 侧错误应含 {keyword:?}（否则可能是「另一个原因」）：{ecsmsg}"
    );
    assert_eq!(
        em, ec,
        "[{tag}] 两边都报错，但**效应**不同（补偿体 / 效应通道的可观测性在这里）\
         \n程序：\n{src}\nMIR : {em:?}\nCore: {ec:?}"
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
        exec_mir(src).0.is_err(),
        "MIR 侧：rhs 未被求值（`false or xs[9] == 1` 不报错）⇒ 短路语义不对"
    );
    assert!(
        exec_core(src).0.is_err(),
        "Core 侧：rhs 未被求值 ⇒ 短路语义不对"
    );
}

/// **D484**：Core 层闭包 —— Core 执行器执行 **Core IR 的闭包体**。
///
/// `Value::Closure` 装的是 `Arc<MirFunction>`（MIR 的函数体）⇒ 装不下 Core
/// 闭包。D484 给语言值加了 `Value::CoreClosure` ⇒ Core 执行器能跑自己的闭包。
///
/// 覆盖三条路径：
/// - **具名调用** `f(1)` —— 走 `h_call` 的「**环境**查找」分支，查找结果
///   是 Core 闭包 ⇒ 宿主委派回 Core 执行器；
/// - **捕获外层** —— 闭包体读外层绑定（靠捕获的环境作父链）；
/// - **值调用**（管道 `|>`）—— Core 执行器内部的 `ClosureCall`。
#[test]
fn core_closures_execute_identically() {
    assert_same("闭包调用", "let f = fn(x) x + 1\nf(1)\n");
    assert_same(
        "闭包捕获外层",
        "let base = 10\nlet f = fn(x) x + base\nf(5)\n",
    );
    assert_same("闭包值调用", "let f = fn(x) x + 1\n1 |> f\n");
}

/// **D485**：区域构造 / 代数效果 / 编排执行 —— Core 执行器与 MIR 执行器
/// 的行为必须**完全一致**。
///
/// | 类别 | 覆盖点 |
/// |---|---|
/// | `transaction` | 体在**隔离**环境里跑；体失败 ⇒ 跑补偿 ⇒ 报「已回滚」 |
/// | `worker` | 隔离执行（环境变更不外泄） |
/// | `parallel` | 体在当前环境跑（worker 的合并按声明顺序 ⇒ 顺序执行等价） |
/// | `with_config` | 宿主 config 的设置与恢复 |
/// | `export` | 导出名标记 |
/// | `handle` / `perform` | handler 栈的 install / perform / restore |
/// | `orchestrate` | agent 图执行（结果变量回填环境） |
#[test]
fn regions_effects_and_orchestration_execute_identically() {
    // ── transaction：体成功 ⇒ 正常继续 ──
    assert_same("transaction", "transaction\n  1\nend\n2i\n");
    // ── transaction：体失败 ⇒ 跑补偿 ⇒ 报「已回滚」──
    //
    // ⚠ 用例的体**故意**报**别的**错（越界）—— 若体自己就报「已回滚」，
    // 这条断言就分不清「补偿跑过了」与「体错了直接冒泡」。
    assert_same_err(
        "transaction 回滚",
        "let xs = [1, 2]\ntransaction\n  xs[9]\nend\n1i\n",
        "rolled back",
    );
    // ── transaction：**补偿体确实跑了** ──
    //
    // 补偿体在**隔离**环境里跑 ⇒ 它的环境改动被丢弃 ⇒ **值上不可观测**；
    // 但它的**效应**会向上传播 ⇒ 补偿「没跑」在这里是**可观测**的。
    // （这正是把对拍改成「错误路径也比效应」的原因。）
    assert_same_err(
        "transaction 补偿体",
        "let xs = [1, 2]\ntransaction\n  xs[9]\ncompensation\n  aggregate c, 1\nend\n1i\n",
        "rolled back",
    );
    // ── worker：隔离执行 ──
    assert_same("worker", "worker w do\n  let q = 1\nend\n2i\n");
    // ── parallel ──
    assert_same(
        "parallel",
        "parallel\n  worker a do\n    let p = 1\n  end\nend\n2i\n",
    );
    // ── with_config：宿主 config ──
    assert_same("with_config", "with model = \"gpt-4o\"\n  1\nend\n2i\n");
    // ── export ──
    assert_same("export", "export let e = 1\n2i\n");
    // ── handle / perform：handler 栈 ──
    assert_same(
        "handle/perform",
        "handle Ai {\n  perform Ai(\"x\")\n} {\n  \"handled\"\n}\n",
    );
    // ── orchestrate：agent 图执行 ──
    assert_same(
        "orchestrate",
        "orchestrate graph input -> result\n  agent a => \"A\"\n  agent b => \"B\"\n  @start -> a\n  a -> b\nend\nresult\n",
    );
}

/// **D486**：section 构建 / 关系搜索 —— Core 执行器与 MIR 执行器行为一致。
///
/// - `prompt_section`：body 求值 ⇒ 建 `Value::PromptSection` ⇒ 绑到环境 ⇒
///   `compose_prompt` 能取到（**可观测**）；
/// - `document_section`：建带 `tag` 的 Dict ⇒ 绑到环境（**可观测**）；
/// - `solve`：关系搜索 ⇒ 解列表（**可观测**）。
#[test]
fn sections_and_relation_search_execute_identically() {
    // ── prompt section：值被 compose_prompt 消费 ──
    assert_same(
        "prompt section",
        "prompt \"p\" do\n  \"hello section\"\nend\ncompose_prompt(\"p\")\n",
    );
    // ── document section：值是带 tag 的 Dict，绑在名字上 ──
    assert_same(
        "document section",
        "document \"d\" do\n  \"doc section\"\nend\nd\n",
    );
    // ── solve：关系搜索的解列表 ──
    assert_same("solve", "rel edge(\"a\", \"b\")\nsolve { edge(?x, ?y) }\n");
}

/// **D488**：声明型构造（类型别名 / 枚举 / 结构体 / TEA model / TEA msg）——
/// Core 执行器与 MIR 执行器的行为必须**完全一致**。
///
/// 这几类的运行期语义都是**纯数据**（在环境里注册一个绑定）⇒ Core 侧复用
/// **同一份** handler ⇒ 两个执行器注册出的绑定**逐字相同**。
#[test]
fn declarations_execute_identically() {
    // ── model（含默认值）—— 名字是**可观测**的值（`Dict{__model_fields__}`）──
    assert_same(
        "model 声明",
        "model Counter\n  count: number = 0\nend\nCounter\n",
    );
    // ── 其余声明：名字**不进词法环境**（typeck 报未绑定）⇒ 这里只验
    //    「声明后程序仍正常」，载荷由**形状**判据核对
    //    （`core_fidelity::declarations_are_visible`）──
    assert_same(
        "msg 声明",
        "msg CounterMsg\n  Increment\n  Decrement\nend\n1\n",
    );
    assert_same("struct 声明", "struct P\n  x: number\nend\n1\n");
    assert_same("enum 声明", "enum E\n  A\n  B\nend\n1\n");
    assert_same("type 别名", "type Alias = number\n1\n");
}

/// 错误路径：两个执行器都应报错（不静默成功）。
#[test]
fn both_executors_report_the_same_error_paths() {
    let src = "let xs = [1, 2]\nxs[9]\n";
    assert_typechecks(src);
    assert!(exec_mir(src).0.is_err(), "MIR 侧应报错");
    assert!(exec_core(src).0.is_err(), "Core 侧应报错");
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
        let effects = r.1;
        r.0.expect("应成功");
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
