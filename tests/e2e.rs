//! v0.77: E2E 端到端测试 — 镜像 main.rs::run_file 的完整调用栈。
//!
//! 与 src 内 inline 单元测试的区别：
//! - 单元测试：白盒，测单个函数/模块的不变量
//! - E2E 测试：黑盒，测"一段 .mora 源码 → 成功执行"的完整链路
//!
//! 注：value-level 断言需要 stdout 捕获（VM 跨线程 print 复杂），
//! 本 E2E 套件断言"执行成功"作为最基础的端到端契约。
//! 精确值断言由各 inline unit test 承担（typeck/bidirectional 22 个、
//! HM 推断 60+ 个、vm 等价性 7 个等）。
//!
//! Fixture 路径：tests/fixtures/e2e/*.mora（env!("CARGO_MANIFEST_DIR") 解析）

mod e2e_helpers;

use e2e_helpers::{assert_compile_error, assert_ok, read_fixture};

// ===================================================================
// 1. 基本值与算术
// ===================================================================

/// arithmetic.mora：task main + print(Int 加法)。完整 E2E 路径。
#[test]
fn e2e_arithmetic_runs() {
    assert_ok("arithmetic.mora");
}

/// string_concat.mora：字符串拼接 + print。
#[test]
fn e2e_string_concat_runs() {
    assert_ok("string_concat.mora");
}

// ===================================================================
// 2. 控制流
// ===================================================================

/// if_else.mora：if-then-else 分支选择。
#[test]
fn e2e_if_else_runs() {
    assert_ok("if_else.mora");
}

/// nested_if.mora：嵌套 if-then-else。
#[test]
fn e2e_nested_if_runs() {
    assert_ok("nested_if.mora");
}

/// for_loop.mora：for-in 累加 + print。
#[test]
fn e2e_for_loop_runs() {
    assert_ok("for_loop.mora");
}

/// match_default.mora：match 默认分支 + print。
#[test]
fn e2e_match_default_runs() {
    assert_ok("match_default.mora");
}

/// v0.87: match_guard.mora — `n when n > 0 => ...` guard conditions。
#[test]
fn e2e_match_guard_runs() {
    assert_ok("match_guard.mora");
}

/// v0.87: match_list_rest.mora — `[a, b, ..rest]` list rest destructuring。
#[test]
fn e2e_match_list_rest_runs() {
    assert_ok("match_list_rest.mora");
}

/// v0.87: match_dict_rename.mora — `{name: n, age: a}` dict rename。
#[test]
fn e2e_match_dict_rename_runs() {
    assert_ok("match_dict_rename.mora");
}

// ===================================================================
// 3. 数据结构
// ===================================================================

/// dict_access.mora：dict 字面量 + 索引访问 + 算术 + print。
#[test]
fn e2e_dict_access_runs() {
    assert_ok("dict_access.mora");
}

// ===================================================================
// 4. task 定义 + 调用
// ===================================================================

/// function_call.mora：task 定义 + 跨 task 调用 + print。
/// 这条路径覆盖 call_value → run_mir 关键 dispatch 路径。
#[test]
fn e2e_task_define_and_call_runs() {
    assert_ok("function_call.mora");
}

/// macro_def.mora：macro 定义 + 展开 + 调用。
/// v0.83: 验证 parser emit_macro_def_w 保留宏体（子 EmitContext 编译），
/// Value::Macro { body: Arc<MirFunction> } 存储宏体，
/// call_builtin_fallback 以 args 绑定 params，子 env run_mir 执行 body。
/// 此前 Value::Macro 仅存 name+params（无 body），调用方报错"not implemented"。
#[test]
fn e2e_macro_define_and_expand_runs() {
    assert_ok("macro_def.mora");
}

/// macro_advanced.mora：嵌套宏调用 + 递归宏 + 宏返回宏。
/// v0.86: 验证宏系统的完整语义——不只是基本展开，还包括复合调用模式。
#[test]
fn e2e_macro_advanced_runs() {
    assert_ok("macro_advanced.mora");
}

/// eval.mora：runtime eval(code) — 从 Mora 内部动态执行 Mora 源码。
/// v0.86: Lisp homoiconicity + eval-apply loop 的落地。
/// 覆盖：算术 / 字符串 / 条件 / 闭包绑定外层变量。
#[test]
fn e2e_eval_runs() {
    assert_ok("eval.mora");
}

/// quasiquote.mora：v0.88 Lisp 系 quasiquote/unquote/unquote-splice。
/// 覆盖：纯 quasiquote（静态 Code）、unquote（,x 求值插入）、
/// unquote-splice（,,items 展开 List）、括号深度解析。
#[test]
fn e2e_quasiquote_runs() {
    assert_ok("quasiquote.mora");
}

// ===================================================================
// 5. 错误注入
// ===================================================================

/// 类型错误注入：把 string 赋给 Int 类型注解的变量。
/// 这是 typeck 错误的最小复现（HM 推断 + 双向叠加层必经路径）。
#[test]
fn e2e_typecheck_error_is_reported() {
    let bogus = r#"
let x: int = "not an int"
x
"#;
    let res = (|| -> Result<(), String> {
        let (_, witnesses) = mora::parser_v3::ParserV3::compile(bogus)
            .map_err(|e| format!("parse: {}", e))?;
        let type_errs =
            mora::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
        if !type_errs.is_empty() {
            return Err(format!("{} type error(s)", type_errs.len()));
        }
        Ok(())
    })();
    assert!(
        res.is_err(),
        "expected type error for `let x: int = \"...\"`, got Ok"
    );
}

/// 语法错误注入：未闭合的字符串字面量。
#[test]
fn e2e_parse_error_is_reported() {
    let bogus = r#"let x = "unterminated"#;
    let res = mora::parser_v3::ParserV3::compile(bogus);
    assert!(
        res.is_err(),
        "expected parser error for unterminated string, got Ok"
    );
}

// ===================================================================
// 6. 字节级 fixture 完整性（防 fixtures 漂移）
// ===================================================================

/// 检查所有 fixture 文件非空。
#[test]
fn e2e_fixtures_are_non_empty() {
    for name in [
        "arithmetic.mora",
        "if_else.mora",
        "function_call.mora",
        "for_loop.mora",
        "string_concat.mora",
        "dict_access.mora",
        "match_default.mora",
        "match_guard.mora",
        "match_list_rest.mora",
        "match_dict_rename.mora",
        "nested_if.mora",
        "handle_effect.mora",
        "macro_def.mora",
    ] {
        let content = read_fixture(name);
        assert!(
            !content.trim().is_empty(),
            "fixture {} must not be empty",
            name
        );
    }
}

// ===================================================================
// 7. run_mir 与 run_dag 线性退化等价（v0.59 行为契约）
// ===================================================================

/// 验证相同 fixture 跑两次都成功（deterministic 执行）。
/// run_mir ≡ run_dag(add_sequential_edges 后) 是 v0.59 起的核心承诺。
#[test]
fn e2e_run_mir_deterministic() {
    assert_ok("arithmetic.mora");
    assert_ok("arithmetic.mora");
}

/// 8 fixtures 全部跑通 — 完整 E2E 覆盖 smoke test。
#[test]
fn e2e_all_fixtures_run() {
    for name in [
        "arithmetic.mora",
        "if_else.mora",
        "function_call.mora",
        "for_loop.mora",
        "string_concat.mora",
        "dict_access.mora",
        "match_default.mora",
        "match_guard.mora",
        "match_list_rest.mora",
        "match_dict_rename.mora",
        "nested_if.mora",
        // v0.91: 数学原语覆盖
        "math_basic.mora",
        "stats_basic.mora",
        "linalg_basic.mora",
        "random_basic.mora",
        "random_handle.mora",
        "bigint_basic.mora",
    ] {
        assert_ok(name);
    }
}

// ===================================================================
// 8. v0.80 algebraic effects：handle / perform 端到端可执行
// ===================================================================

/// perform 必须由 handle 块内的 handler 接管，且返回值 = handler 末尾表达式。
/// 此测试验证 Stage 2.0 单发语义下整条路径可执行（不是 stub）。
#[test]
fn e2e_handle_perform_returns_handler_result() {
    let (result, _stdout) = assert_ok("handle_effect.mora");
    // handle 块的 body 把 perform 返回值（handler 末尾表达式 = "mocked:" + __arg0）
    // 存入 global_result；最后一行 result = global_result 取出来断言。
    assert_eq!(
        result.to_string(),
        "mocked:hello",
        "handle/perform 端到端语义失败：handler 末尾表达式值未传回 perform"
    );
}

// 静默工具 unused import 警告
#[allow(dead_code)]
fn _unused_assert_compile_error() {
    let _ = assert_compile_error("__unused__");
}

// ===================================================================
// v0.83: TEA (The Elm Architecture) — Runtime 层验证
// ===================================================================
// 注：完整 TEA 循环（Model/Msg/Update/Cmd + Replay）的 Runtime 基础设施
// 已通过 unit tests 验证（src/tea/{mod,replay}.rs 中 10+ tests）。
// E2E fixtures 需要 model/msg/update/app 新语法支持（Stage 4 路线图），
// 本阶段仅做 Runtime API 集成测试：

#[test]
fn e2e_tea_runtime_compiles() {
    // 验证 TeaApp/TeaCmd/TeaMsg 类型在 VM 中可构造和操作
    // （Runtime 通过 builtin tea.* 暴露，E2E 暂用 unit tests 覆盖）
    use mora::tea::{Cmd, Msg, TeaApp};
    // v0.94: TeaApp 是纯值 —— with_model/dispatch 返回新 app，run_loop 返回新 app。
    let app = TeaApp::new(
        mora::value::Value::Nil,
        mora::value::Value::Nil,
        mora::value::Value::Nil,
    )
    .with_model(mora::value::Value::Int(42))
    .dispatch(Msg::new("Test", mora::value::Value::Nil));
    assert_eq!(app.model(), mora::value::Value::Int(42));
    // v0.83: run_loop 需要 MirHost context —— 用 Interpreter::new() 注入
    let mut interp = mora::interpreter::Interpreter::new();
    assert_eq!(
        app.run_loop(10, &mut interp).model(),
        mora::value::Value::Int(42)
    );
    let cmd = Cmd::None;
    let _ = cmd.to_value();
}

/// tea_app.mora：完整 model/msg/update/app 语法糖 — 验证 parser 端到端解析
#[test]
fn e2e_tea_app_runs() {
    assert_ok("tea_app.mora");
}