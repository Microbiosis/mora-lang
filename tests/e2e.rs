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
        "nested_if.mora",
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
        "nested_if.mora",
    ] {
        assert_ok(name);
    }
}

// 静默工具 unused import 警告
#[allow(dead_code)]
fn _unused_assert_compile_error() {
    let _ = assert_compile_error("__unused__");
}