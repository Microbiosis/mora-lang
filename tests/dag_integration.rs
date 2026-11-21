//! W2.3: Control SSA VM integration tests (replacing the legacy DAG-interpreter
//! tests, whose execution stack was removed in v0.110.0).
//!
//! Verifies that `execute_program` and `run_main_task` execute real programs cleanly.

use std::sync::Arc;

use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::{execute_program, run_main_task};

fn run_dag_path(source: &str) -> mora::error::MoraResult<()> {
    let (prog, _witnesses) = ParserV3::compile(source)?;
    let arc = Arc::new(prog);
    let mut core = CoreRuntime::default();
    let (_, globals) = execute_program(arc.clone(), &mut core)?;
    let _ = run_main_task(arc, globals, &mut core)?;
    Ok(())
}

#[test]
fn dag_pure_computation_no_crash() {
    run_dag_path("let x = 1 + 2\nlet y = x * 3").expect("pure computation via VM");
    run_dag_path("let a = [10, 20, 30]\nlen(a)").expect("list + builtin via VM");
    run_dag_path("let s = \"hello\"\nlet t = \"world\"\ns + \" \" + t")
        .expect("string concat via VM");
}

#[test]
fn dag_task_with_main_no_crash() {
    run_dag_path("task main()\n  print(1 + 2)\nend").expect("task main via VM pipeline");
}

#[test]
fn dag_hm_basic_demo_no_crash() {
    let source = std::fs::read_to_string("examples/hm_basic_demo.mora")
        .expect("should read hm_basic_demo.mora");
    let (prog, _witnesses) = ParserV3::compile(&source).expect("compile");
    let mut core = CoreRuntime::default();
    let (v, _) = execute_program(Arc::new(prog), &mut core).expect("hm_basic_demo 应能跑通");
    eprintln!("VM result: {:?}", v);
}

#[test]
fn repeated_control_program_runs_keep_isolated_globals() {
    let src = "let c = 0i\nc = c + 1i\nc";
    let (prog, _witnesses) = ParserV3::compile(src).expect("compile");
    let arc = Arc::new(prog);

    let mut core1 = CoreRuntime::default();
    let (v1, _) = execute_program(arc.clone(), &mut core1).expect("first run");
    let mut core2 = CoreRuntime::default();
    let (v2, _) = execute_program(arc, &mut core2).expect("second run");

    assert_eq!(format!("{v1:?}"), "Int(1)");
    assert_eq!(format!("{v2:?}"), "Int(1)");
}
