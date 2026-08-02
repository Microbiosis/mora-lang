//! v0.59: DAG interpreter integration tests.
//!
//! Verifies that `run_mir_dag` can execute real programs without crashing.
//! Pure-computation programs use DAG execution; programs with `task main()`
//! delegate the task body to `run_mir` via `run_main_task`.

use mora::interpreter::Interpreter;
use mora::mir::lower::lower_mir_exprs;

fn run_dag_path(source: &str) -> Result<(), String> {
    let exprs = mora::interpreter::parse_code_v3(source)?;
    let func = lower_mir_exprs(&exprs)?;
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    // v0.75.9: 包裹 Arc（run_mir_dag 签名变更，走全局 DAG 缓存）
    mora::mir::vm::run_mir_dag(&std::sync::Arc::new(func), &mut interp, &mut env)?;
    Ok(())
}

#[test]
fn dag_pure_computation_no_crash() {
    run_dag_path("let x = 1 + 2\nlet y = x * 3").expect("pure computation via DAG");
    run_dag_path("let a = [10, 20, 30]\nlen(a)").expect("list + builtin via DAG");
    run_dag_path("let s = \"hello\"\nlet t = \"world\"\ns + \" \" + t")
        .expect("string concat via DAG");
}

#[test]
fn dag_task_with_main_no_crash() {
    // The task body is executed via run_mir (linear), not DAG.
    // This test verifies the full pipeline doesn't crash.
    run_dag_path("task main()\n  print(1 + 2)\nend").expect("task main via DAG pipeline");
}

#[test]
fn dag_compress_demo_no_crash() {
    let source = std::fs::read_to_string("examples/compress_demo.mora")
        .expect("should read compress_demo.mora");
    let exprs = mora::interpreter::parse_code_v3(&source).expect("parse");
    let func = lower_mir_exprs(&exprs).expect("lower");
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    // Just run the top-level DAG body, then main task via linear
    // v0.75.9: 包裹 Arc（run_mir_dag 签名变更）
    match mora::mir::vm::run_mir_dag(&std::sync::Arc::new(func), &mut interp, &mut env) {
        Ok(v) => eprintln!("DAG result: {:?}", v),
        Err(e) => eprintln!("DAG error (expected during compress mock): {}", e),
    }
    // compress_demo uses the `compress` builtin which may fail in mock mode;
    // we just care that the pipeline doesn't panic.
}

// ─── v0.75.28: 方向 2 行为守卫 — 变量级增量重算（输入值驱动）──────────

#[test]
fn memo_incremental_reruns_affected_dependencies_only() {
    // 变量级增量重算由 DagExecMemo 的「输入值相等跳过」实现：env 变量变化
    // → Var（非纯，每次重跑读 env）→ 受影响下游纯节点（BinaryOp）输入变
    // → 重算；未受影响下游输入相等 → memo 跳过。
    // 本例：b 链依赖外部 a；c/d 链独立。改 env 的 a 后第二次 run 应只重算
    // b 链、跳过 d 链的纯节点。
    use mora::mir::MirFunction;
    use mora::mir::cache::global_dag_cache;
    use mora::mir::vm::{DagExecMemo, run_dag_with_signal_memo};
    use mora::value::Value;
    use std::sync::Arc;

    let src = "print(a)\nlet b = a + 1\nprint(b)\nlet c = 5\nlet d = c + 1\nprint(d)";
    let exprs = mora::interpreter::parse_code_v3(src).expect("parse");
    let func: Arc<MirFunction> = Arc::new(lower_mir_exprs(&exprs).expect("lower"));
    let dag = global_dag_cache().get_or_build(&func);
    let mut memo = DagExecMemo::new();

    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    env.define("a".to_string(), Value::Float(1.0), false);

    run_dag_with_signal_memo(&dag, &func, &mut memo, &mut interp, &mut env).expect("first run");
    let first_executed = memo.executed_nodes;
    let first_skipped = memo.skipped_nodes;

    // 只改 b 链的依赖 a；c/d 链不受影响
    env.assign("a", Value::Float(10.0));

    run_dag_with_signal_memo(&dag, &func, &mut memo, &mut interp, &mut env).expect("second run");
    let delta_executed = memo.executed_nodes - first_executed;
    let delta_skipped = memo.skipped_nodes - first_skipped;

    // d 链的纯节点（BinaryOp(c+1)）输入相等 → 被 memo 跳过
    assert!(delta_skipped > 0, "未受影响下游应被 memo 跳过");
    // b 链（Var(a) 重读 → BinaryOp 重算）至少一个节点重执行
    assert!(delta_executed > 0, "受影响下游应重算");
}
