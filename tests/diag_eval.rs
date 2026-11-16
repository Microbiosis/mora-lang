use mora::parser_v3::ParserV3;
use mora::value::Value;
use std::sync::Arc;

fn run_mora(source: &str) -> Result<Value, String> {
    let mut core = mora::runtime::CoreRuntime::default();
    let (prog, _witnesses) =
        ParserV3::compile(source).map_err(|e| format!("compile error: {}", e))?;
    let prog_arc = Arc::new(prog);
    let (_, globals) = mora::runtime::vm::execute_program(Arc::clone(&prog_arc), &mut core)
        .map_err(|e| format!("execute error: {}", e))?;
    mora::runtime::vm::run_main_task(Arc::clone(&prog_arc), globals.clone(), &mut core)
        .map_err(|e| format!("run_main_task error: {}", e))?;
    prog_arc
        .globals
        .iter()
        .position(|g| g.name == "__result")
        .map(|i| globals[i].lock().0.clone())
        .ok_or_else(|| "result variable '__result' not found".to_string())
}

#[test]
fn diag_eval_simple_fn() {
    // Just eval a simple closure and see what it returns
    let src = r#"let __result = eval("fn(x) x + 1")"#;
    let result = run_mora(src).expect("simple eval should succeed");
    println!("diag1: eval('fn(x) x + 1') = {:?}", result);
    // Should be a closure
    assert!(matches!(
        &result,
        Value::Closure { .. } | Value::Task { .. } | Value::Partial(_, _)
    ));
}

#[test]
fn diag_eval_fn_and_call() {
    // eval a closure and call it
    let src = r#"let f = eval("fn(x) x + 1")
let __result = f(21)"#;
    let result = run_mora(src).expect("eval+call should succeed");
    println!("diag2: eval('fn(x) x + 1')(21) = {:?}", result);
    assert_eq!(result.to_string(), "22");
}

#[test]
fn diag_eval_fn_inner_call() {
    // eval "fn(f) f(21)" and call with fn(x) x * 2
    let src = r#"let f = eval("fn(f) f(21)")
let __result = f(fn(x) x * 2)"#;
    let result = run_mora(src).expect("eval inner call should succeed");
    println!("diag3: eval('fn(f) f(21)')(fn(x) x * 2) = {:?}", result);
    assert_eq!(result.to_string(), "42");
}
