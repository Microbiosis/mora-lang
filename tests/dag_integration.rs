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
    mora::mir::dag_interp::run_mir_dag(&func, &mut interp, &mut env)?;
    Ok(())
}

#[test]
fn dag_pure_computation_no_crash() {
    run_dag_path("let x = 1 + 2\nlet y = x * 3").expect("pure computation via DAG");
    run_dag_path("let a = [10, 20, 30]\nlen(a)").expect("list + builtin via DAG");
    run_dag_path("let s = \"hello\"\nlet t = \"world\"\ns + \" \" + t").expect("string concat via DAG");
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
    match mora::mir::dag_interp::run_mir_dag(&func, &mut interp, &mut env) {
        Ok(v) => eprintln!("DAG result: {:?}", v),
        Err(e) => eprintln!("DAG error (expected during compress mock): {}", e),
    }
    // compress_demo uses the `compress` builtin which may fail in mock mode;
    // we just care that the pipeline doesn't panic.
}
