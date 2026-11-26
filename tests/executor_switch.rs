//! v0.90.3: 执行器切换验证 — 生产编译入口返回的程序必须可执行且语义正确。
//!
//! ⚠ **历史快照**：本文件头原描述的是「9 层差分绿时返回管线产出的
//! `MirFunction`、红时回落 `emit.rs` 直出」的双路径 —— 该 9 层 IR 与差分
//! 回落机制已于 **v0.110.0** 全部删除。今天**只有一条**编译路径：
//! `ParserV3::compile → MirWitness[] → mir::control::lower_program → ControlProgram`
//! → `optimize_program`。
//! 本测试走完整生产路径（`compile_and_opt` → `execute_program`）断言执行结果。

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
    let (top, globals) = execute_program(arc.clone(), &mut core).unwrap();
    let _ = run_main_task(arc.clone(), globals.clone(), &mut core);
    // break 触发则 total=10，未触发则=55；fixture 的末表达式是 `total + 1000i`，因此程序
    // 的返回值直接编码了这个事实（1010 / 1055）。
    //
    // 这里必须观察**返回值**而不是 `<main>` 全局槽：P6-1 的 Perceus 所有权转移会在全局的
    // 最后一次读取处把所有权搬出全局槽 —— 脚本的顶层绑定随 `<main>` 一同消亡。需要执行后
    // 仍能读到全局值的宿主，必须走保留全局的 lowering 路径编译。
    assert!(
        matches!(top, Value::Int(1010)),
        "while break: total should be 10 (break at i=5), got {:?}",
        top
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
