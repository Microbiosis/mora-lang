use mora::parser_v3::ParserV3;
use mora::value::Value;
use std::sync::Arc;

fn run_mora(source: &str) -> Result<Value, String> {
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    let (func, _witnesses) =
        ParserV3::compile(source).map_err(|e| format!("compile error: {}", e))?;
    let func_arc = Arc::new(func);
    mora::mir::vm::run_mir(&func_arc, &mut interp, &mut env, &mut mora::mir::effect::Effects::new())
        .map_err(|e| format!("run_mir error: {}", e))?;
    mora::mir::vm::run_main_task(&func_arc, &mut interp, &mut env, &mut mora::mir::effect::Effects::new())
        .map_err(|e| format!("run_main_task error: {}", e))?;
    env.get("__result")
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
    assert_eq!(result.to_string(), "22.0");
}

#[test]
fn diag_eval_fn_inner_call() {
    // eval "fn(f) f(21)" and call with fn(x) x * 2
    let src = r#"let f = eval("fn(f) f(21)")
let __result = f(fn(x) x * 2)"#;
    let result = run_mora(src).expect("eval inner call should succeed");
    println!("diag3: eval('fn(f) f(21)')(fn(x) x * 2) = {:?}", result);
    assert_eq!(result.to_string(), "42.0");
}
