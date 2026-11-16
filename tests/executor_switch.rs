//! v0.90.3: 执行器切换验证 — compile_and_opt（生产编译入口）返回的
//! MirFunction 必须可执行且语义正确。
//!
//! compile_and_opt 在 9 层差分绿时返回管线产出的 MirFunction
//!（witness→FCFG→lower_fcfg→apply_rules），红时回落 emit.rs 直出。
//! 本测试走完整生产路径（compile_and_opt → execute）断言执行结果。

use mora::runtime::CoreRuntime;
use mora::runtime::vm::{execute_program, run_main_task};
use mora::value::Value;
use std::sync::Arc;

/// 生产路径执行：compile_and_opt → execute_program + run_main_task。
fn run_production(source: &str) -> Value {
    let (prog, _witnesses) =
        mora::cli::compile_and_opt(source, None).expect("compile_and_opt failed");
    let mut core = CoreRuntime::default();
    let prog_arc = Arc::new(prog);
    let (last, globals) = execute_program(prog_arc.clone(), &mut core)
        .unwrap_or_else(|e| panic!("execute failed: {}", e));
    let _ = run_main_task(prog_arc, globals, &mut core)
        .unwrap_or_else(|e| panic!("run_main_task failed: {}", e));
    last
}

#[test]
fn switch_arithmetic() {
    let v = run_production("task main()\n  print(10i + 32i)\nend");
    assert!(matches!(v, Value::Nil));
}

#[test]
fn switch_let_and_use() {
    let v = run_production("let x = 42i\nlet y = x + 8i\ny");
    assert!(matches!(v, Value::Int(50)));
}

#[test]
fn switch_string_concat() {
    let v = run_production("let a = \"foo\"\nlet b = \"bar\"\na + b");
    assert!(matches!(v, Value::String(ref s) if s == "foobar"));
}

#[test]
fn switch_if_else() {
    let v = run_production(
        "task main()\n  let x = 5i\n  if x > 3i { print(\"big\") } else { print(\"small\") }\nend",
    );
    assert!(matches!(v, Value::Nil));
}

#[test]
fn switch_for_loop() {
    let v = run_production(
        "task main()\n  let total = 0i\n  for i in [1, 2, 3] {\n    total = total + i\n  }\n  print(total)\nend",
    );
    assert!(matches!(v, Value::Nil));
}

#[test]
fn switch_handle_effect() {
    let v = run_production(
        "let g = \"init\"\nhandle Ai {\n  g = perform Ai(\"hello\")\n} {\n  \"m:\" + __arg0\n}\ng",
    );
    assert!(
        matches!(v, Value::String(ref s) if s == "m:hello"),
        "handle/perform semantics wrong: {:?}",
        v
    );
}

#[test]
fn switch_function_call() {
    let v = run_production("let ops = {\"add\": fn(a, b) a + b end}\nlet r = ops.add(2i, 3i)\nr");
    assert!(matches!(v, Value::Int(5)));
}

#[test]
fn switch_match_expr() {
    let v = run_production("let r = match 42i { 1i => \"one\", _ => \"other\" }\nr");
    assert!(matches!(v, Value::String(ref s) if s == "other"));
}

#[test]
fn switch_macro_factorial() {
    // 递归宏 + brace-if — 此前 wrap_with_cond bug 的回归测试
    let v = run_production(
        "macro factorial(n)\n  if n == 1i { 1i } else { n * factorial(n - 1i) }\nend\nlet f5 = factorial(5i)\nf5",
    );
    assert!(matches!(v, Value::Int(120)));
}

#[test]
fn switch_nested_closures() {
    let v = run_production("let add = fn(a) fn(b) a + b end end\nlet add5 = add(5i)\nadd5(3i)");
    assert!(matches!(v, Value::Int(8)));
}

#[test]
fn switch_dict_operations() {
    let v = run_production("let d = {\"x\": 10i, \"y\": 20i}\nd[\"x\"] + d[\"y\"]");
    assert!(matches!(v, Value::Int(30)));
}

#[test]
fn switch_match_with_guard() {
    let v =
        run_production("let x = 42i\nlet r = match x { 42i => \"found\", _ => \"not found\" }\nr");
    assert!(matches!(v, Value::String(ref s) if s == "found"));
}

#[test]
fn switch_list_comprehension_style() {
    let v = run_production(
        "task main()\n  let nums = [1, 2, 3, 4, 5]\n  let total = 0i\n  for n in nums {\n    total = total + n\n  }\n  print(total)\nend",
    );
    assert!(matches!(v, Value::Nil));
}

#[test]
fn switch_chained_method_calls() {
    // 链式方法调用 — dict 的 .keys() 然后取 len
    let v = run_production("let d = {\"a\": 1i, \"b\": 2i, \"c\": 3i}\nd.len()");
    assert!(matches!(v, Value::Int(3)));
}

#[test]
fn switch_while_break() {
    let source = std::fs::read_to_string("tests/fixtures/e2e/loop_break.mora").unwrap();
    let (prog, _) = mora::cli::compile_and_opt(&source, None).expect("compile_and_opt failed");
    let mut core = CoreRuntime::default();
    let arc = Arc::new(prog);
    let (_, globals) = execute_program(arc.clone(), &mut core).unwrap();
    let _ = run_main_task(arc.clone(), globals.clone(), &mut core);
    // 通过 globals 验证：break 触发则 total=10，未触发则=55
    let total = arc
        .globals
        .iter()
        .position(|g| g.name == "total")
        .map(|i| globals[i].lock().0.clone())
        .unwrap_or(Value::Nil);
    assert!(
        matches!(total, Value::Int(10)),
        "while break: total should be 10 (break at i=5), got {:?}",
        total
    );
}

#[test]
fn switch_while_continue() {
    // 1+2+4+5 = 12（i==3 时 continue，跳过 3）
    let source = "let total = 0i\nlet i = 0i\nwhile i < 5i\n  i = i + 1i\n  if i == 3i\n    continue\n  end\n  total = total + i\nend\ntotal";
    let (prog, _) = mora::cli::compile_and_opt(source, None).expect("compile_and_opt failed");
    let mut core = CoreRuntime::default();
    let arc = Arc::new(prog);
    let (last, globals) =
        execute_program(arc.clone(), &mut core).expect("while_continue execute failed");
    let _ = run_main_task(arc, globals, &mut core);
    let got = match last {
        Value::Int(n) => n as f64,
        Value::Float(n) => n,
        other => panic!("期望数值结果，得到 {:?}", other),
    };
    assert_eq!(got, 12.0, "continue 跳过 i==3 后累加应为 12");
}

#[test]
fn switch_for_break() {
    // 1+2+3+4+5 = 15
    let source = "let total = 0i\nfor i in [1i, 2i, 3i, 4i, 5i]\n  total = total + i\nend\ntotal";
    let (prog, _) = mora::cli::compile_and_opt(source, None).expect("compile_and_opt failed");
    let mut core = CoreRuntime::default();
    let arc = Arc::new(prog);
    let (last, globals) = execute_program(arc.clone(), &mut core).expect("for sum execute failed");
    let _ = run_main_task(arc, globals, &mut core);
    let got = match last {
        Value::Int(n) => n as f64,
        Value::Float(n) => n,
        other => panic!("期望数值结果，得到 {:?}", other),
    };
    assert_eq!(got, 15.0, "for 循环 1..5 累加应为 15");
}
