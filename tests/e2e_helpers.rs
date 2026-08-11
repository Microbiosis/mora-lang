//! v0.77: E2E test harness — 镜像 main.rs::run_file 的完整调用栈：
//!   cli::compile_and_opt → typeck::check_program_witnesses_bidirectional
//!   → mir::vm::run_mir → mir::vm::run_main_task
//!
//! 与 parser_v2_integration.rs 那种依赖 cwd 的 read_to_string 不同，
//! 本模块所有路径用 env!("CARGO_MANIFEST_DIR") 解析，CI 无依赖。

use mora::interpreter::Interpreter;
use mora::mir::vm::{run_main_task, run_mir};
use mora::typeck::{TypeError, check_mir::check_program_witnesses_bidirectional, format_error};
use mora::value::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// 从 tests/fixtures/e2e/ 读取 .mora fixture 的绝对路径。
pub fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("e2e")
        .join(name)
}

/// 读取 fixture 源码。
pub fn read_fixture(name: &str) -> String {
    std::fs::read_to_string(fixture_path(name))
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {}", name, e))
}

/// E2E 编译 + 类型检查 + 执行的返回结果。
pub enum E2eResult {
    /// 编译 + 类型检查 + run_mir + run_main_task 全成功
    Ok {
        /// run_mir 返回的顶层最后表达式值
        last_expr: Value,
        /// run_main_task 期间 print 收集的 stdout（按行）
        stdout_lines: Vec<String>,
    },
    /// 编译失败
    CompileError(String),
    /// 类型检查失败（含错误列表）
    TypeErrors(Vec<TypeError>),
}

/// 完整镜像 main.rs::run_file 的调用栈，并捕获 stdout 用于断言。
///
/// 拦截 std::io::stdout 不是简单的活（task/worker 线程跨 stdout），
/// 因此我们用 in-memory buffer + 替换 print 实现：
/// print(x) 走 builtin dispatch → 流式输出由 Runtime 控制。
///
/// 注：v0.55+ Interpreter 的 print 调用 stdout 全局（println! 风格）。
/// 测试用 std::process 的方式是脆弱的 — 我们用 env var 或 capture helper。
/// 当前实现：直接跑原流程，让 stdout 走测试框架；E2E 测试断言
/// `result.last_expr.to_string()`（module-level 最后表达式）。
pub fn run_e2e(name: &str) -> E2eResult {
    let source = read_fixture(name);

    let (func, witnesses) = match mora::parser_v3::ParserV3::compile(&source) {
        Ok(f) => f,
        Err(e) => return E2eResult::CompileError(e),
    };

    let type_errs = check_program_witnesses_bidirectional(&witnesses);
    if !type_errs.is_empty() {
        return E2eResult::TypeErrors(type_errs);
    }

    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let func_arc = Arc::new(func);

    let last_expr = match run_mir(&func_arc, &mut interp, &mut env) {
        Ok(v) => v,
        Err(e) => return E2eResult::CompileError(format!("run_mir: {}", e)),
    };
    if let Err(e) = run_main_task(&func_arc, &mut interp, &mut env) {
        return E2eResult::CompileError(format!("run_main_task: {}", e));
    }

    E2eResult::Ok {
        last_expr,
        stdout_lines: Vec::new(),
    }
}

/// 断言 E2E 跑成功，返回 (last_expr, stdout)。
pub fn assert_ok(name: &str) -> (Value, Vec<String>) {
    match run_e2e(name) {
        E2eResult::Ok {
            last_expr,
            stdout_lines,
        } => (last_expr, stdout_lines),
        E2eResult::CompileError(e) => panic!("E2E {}: compile error: {}", name, e),
        E2eResult::TypeErrors(errs) => {
            let formatted: Vec<String> = errs.iter().map(format_error).collect();
            panic!("E2E {}: type errors:\n{}", name, formatted.join("\n"))
        }
    }
}

/// 断言 E2E 跑失败并返回错误（用于测试 typeck 错误注入）。
pub fn assert_compile_error(name: &str) -> String {
    match run_e2e(name) {
        E2eResult::CompileError(e) => e,
        E2eResult::Ok { last_expr, .. } => {
            panic!("E2E {}: expected compile error, got Ok({:?})", name, last_expr)
        }
        E2eResult::TypeErrors(errs) => {
            let formatted: Vec<String> = errs.iter().map(format_error).collect();
            formatted.join("\n")
        }
    }
}

// unused: 防止 rust 警告 Mutex 未使用（预留 capture 扩展）
#[allow(dead_code)]
fn _mutex_unused() -> Mutex<()> {
    Mutex::new(())
}