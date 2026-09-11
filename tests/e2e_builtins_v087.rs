//! v0.87: Lisp homoiconicity 三件套 — gensym / read / macroexpand 测试
//!
//! 测试策略：inline 源码风格（不依赖 fixture），每条测试独立构造 Interpreter
//! 和 Environment。通过 ParserV3::compile → run_mir + run_main_task 执行，
//! 将结果存入模块级变量 __result，再从 env 中取出断言。
//!
//! 架构注意事项：
//! - run_mir 返回模块最后表达式的值（task def → Nil），不用其做断言
//! - run_main_task 返回 ()，不传播 main body 的最后表达式
//! - __result 变量由 Mora 顶层 let 绑定初始化后，run_mir 存入 env，run_main_task 后 env 可见
//! - DAG 优化器合并相同 Call 节点，因此 `[gensym(), gensym()]` 全部返回同一值
//!   （gensym 是"volatile" builtin，但 DAG 层尚不知）—— 用多个 let 绑定规避
//! - v0.84 JSON 对称性：2 + 3 结果为 Float(5.0)（+ 操作语义升级），用字符串比较断言

use mora::interpreter::Interpreter;
use mora::mir::vm::{run_main_task, run_mir};
use mora::parser_v3::ParserV3;
use mora::value::Value;
use std::sync::Arc;

fn run_mora(source: &str) -> Result<Value, String> {
    let (func, _witnesses) =
        ParserV3::compile(source).map_err(|e| format!("compile error: {}", e))?;
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let func_arc = Arc::new(func);
    run_mir(&func_arc, &mut interp, &mut env, &mut mora::mir::effect::Effects::new()).map_err(|e| format!("run_mir error: {}", e))?;
    run_main_task(&func_arc, &mut interp, &mut env, &mut mora::mir::effect::Effects::new())
        .map_err(|e| format!("run_main_task error: {}", e))?;
    env.get("__result")
        .ok_or_else(|| "result variable '__result' not found".to_string())
}

// ===================================================================
// 1. gensym() 测试
// ===================================================================

#[test]
fn v087_gensym_returns_string() {
    let src = r#"let __result = gensym()
task main()
  print("gensym=" + __result)
end"#;
    let result = run_mora(src).expect("gensym should succeed");
    assert!(
        matches!(result, Value::String(_)),
        "gensym should return Value::String, got {:?}",
        result
    );
}

#[test]
fn v087_gensym_returns_unique_values() {
    // DAG 合并规避：每个 gensym 调用用独立的 let 绑定，避免被识别为同一节点
    let src = r#"let g0 = gensym()
let g1 = gensym()
let g2 = gensym()
let __result = [g0, g1, g2]
task main()
  print("gensym trio=" + __result)
end"#;
    let result = run_mora(src).expect("gensym trio should succeed");
    let list = match result {
        Value::List(items) => items,
        other => panic!("expected list, got {:?}", other),
    };
    assert_eq!(list.len(), 3);
    assert_ne!(list[0], list[1]);
    assert_ne!(list[1], list[2]);
    assert_ne!(list[0], list[2]);
}

#[test]
fn v087_gensym_continuous_sequence() {
    // 用五个独立 let 绑定确保 DAG 不合并
    let src = r#"let g0 = gensym()
let g1 = gensym()
let g2 = gensym()
let g3 = gensym()
let g4 = gensym()
let __result = [g0, g1, g2, g3, g4]
task main()
  print("5 gensyms=" + __result)
end"#;
    let result = run_mora(src).expect("gensym sequence should succeed");
    let list = match result {
        Value::List(items) => items,
        other => panic!("expected list, got {:?}", other),
    };
    assert_eq!(list.len(), 5);
    for (i, item) in list.iter().enumerate() {
        assert_eq!(
            item.to_string(),
            format!("g{}", i),
            "gensym should produce sequential g{{i}} names"
        );
    }
}

#[test]
fn v087_gensym_rejects_args() {
    let src = r#"let __result = gensym("bad")
task main()
end"#;
    let err = run_mora(src).expect_err("gensym with args should fail");
    assert!(
        err.contains("expects no arguments"),
        "expected 'expects no arguments' error, got: {}",
        err
    );
}

// ===================================================================
// 2. read() 测试
// ===================================================================

#[test]
fn v087_read_returns_code() {
    let src = r#"let __result = read("2 + 3")
task main()
  print("read(2+3)=" + __result)
end"#;
    let result = run_mora(src).expect("read should succeed");
    match &result {
        Value::Code(s) => assert_eq!(s, "2 + 3", "read should preserve source text"),
        other => panic!("expected Value::Code, got {:?}", other),
    }
}

#[test]
fn v087_read_empty_string() {
    let src = r#"let __result = read("")
task main()
  print("read_empty=" + __result)
end"#;
    let result = run_mora(src).expect("read of empty string should succeed");
    match &result {
        Value::Code(s) => assert!(s.is_empty(), "read('') should return empty Code"),
        other => panic!("expected Value::Code, got {:?}", other),
    }
}

#[test]
fn v087_read_preserves_multiline() {
    let src = r#"let __result = read("let x = 1\nx + 1")
task main()
end"#;
    let result = run_mora(src).expect("read multiline should succeed");
    match &result {
        Value::Code(s) => assert!(s.contains("let x = 1"), "read should preserve multiline"),
        other => panic!("expected Value::Code, got {:?}", other),
    }
}

#[test]
fn v087_read_rejects_non_string() {
    let src = r#"let __result = read(42)
task main()
end"#;
    let err = run_mora(src).expect_err("read with non-string should fail");
    assert!(
        err.contains("expects a string or code argument"),
        "expected type error, got: {}",
        err
    );
}

// ===================================================================
// 3. eval(read(...)) roundtrip 测试
// ===================================================================

#[test]
fn v087_eval_read_roundtrip_arithmetic() {
    let src = r#"let __result = eval(read("2 + 3"))
task main()
  print("eval(read(2+3))=" + __result)
end"#;
    let result = run_mora(src).expect("eval(read(...)) should succeed");
    // v0.84: 2+3 = Float(5.0)（+ 操作语义升级）。用字符串比较。
    assert_eq!(
        result.to_string(),
        "5.0",
        "eval(read(\"2 + 3\")) should equal 5.0, got {:?}",
        result
    );
}

#[test]
fn v087_eval_read_conditional() {
    let src = r#"let __result = eval(read("if true { 99 } else { 0 }"))
task main()
  print("eval(read(if))=" + __result)
end"#;
    let result = run_mora(src).expect("eval(read(if)) should succeed");
    assert_eq!(
        result.to_string(),
        "99.0",
        "eval(read(if)) should return 99.0, got {:?}",
        result
    );
}

#[test]
fn v087_eval_read_task_definition() {
    let src = r#"let code = read("fn(f) f(21)")
let f = eval(code)
let __result = f(fn(x) x * 2)
task main()
  print("eval(read) result=" + __result)
end"#;
    let result = run_mora(src).expect("eval(read(task)) should succeed");
    assert_eq!(
        result.to_string(),
        "42.0",
        "eval(read(fn(f) f(21))) should return 42.0, got {:?}",
        result
    );
}

#[test]
fn v087_eval_read_roundtrip_list() {
    let src = r#"let __result = eval(read("[1, 2, 3]"))
task main()
  print("eval(read(list))=" + __result)
end"#;
    let result = run_mora(src).expect("eval(read(list)) should succeed");
    match &result {
        Value::List(items) => {
            assert_eq!(items.len(), 3);
            assert_eq!(items[0].to_string(), "1.0");
            assert_eq!(items[1].to_string(), "2.0");
            assert_eq!(items[2].to_string(), "3.0");
        }
        other => panic!("expected list, got {:?}", other),
    }
}

// ===================================================================
// 4. macroexpand() 测试
// ===================================================================

#[test]
fn v087_macroexpand_basic() {
    let src = r#"macro add(a, b)
  a + b
end
let __result = macroexpand("add", [1, 2])
task main()
  print("macroexpand(add,[1,2])=" + __result)
end"#;
    let result = run_mora(src).expect("macroexpand basic should succeed");
    assert_eq!(
        result.to_string(),
        "3.0",
        "macroexpand('add', [1,2]) should equal 3.0, got {:?}",
        result
    );
}

#[test]
fn v087_macroexpand_multi_param() {
    let src = r#"macro triple(a, b, c)
  a + b + c
end
let __result = macroexpand("triple", [10, 20, 30])
task main()
  print("macroexpand(triple,[10,20,30])=" + __result)
end"#;
    let result = run_mora(src).expect("macroexpand multi should succeed");
    assert_eq!(
        result.to_string(),
        "60.0",
        "macroexpand('triple', [10,20,30]) should equal 60.0, got {:?}",
        result
    );
}

#[test]
fn v087_macroexpand_zero_param() {
    let src = r#"macro hello()
  "hello, world"
end
let __result = macroexpand("hello", [])
task main()
  print("macroexpand(hello,[])=" + __result)
end"#;
    let result = run_mora(src).expect("macroexpand zero-param should succeed");
    assert_eq!(
        result,
        Value::String("hello, world".to_string()),
        "macroexpand('hello', []) should return hello, world, got {:?}",
        result
    );
}

#[test]
fn v087_macroexpand_undefined_macro() {
    let src = r#"let __result = macroexpand("nonexistent", [1, 2])
task main()
end"#;
    let err = run_mora(src).expect_err("macroexpand undefined should fail");
    assert!(
        err.contains("undefined macro"),
        "expected 'undefined macro' error, got: {}",
        err
    );
}

#[test]
fn v087_macroexpand_not_a_macro() {
    let src = r#"macro add(a, b)
  a + b
end
let x = 42
let __result = macroexpand("x", [1, 2])
task main()
end"#;
    let err = run_mora(src).expect_err("macroexpand non-macro should fail");
    assert!(
        err.contains("not a macro"),
        "expected 'not a macro' error, got: {}",
        err
    );
}

#[test]
fn v087_macroexpand_nested() {
    // square → mul → add 嵌套宏展开
    let src = r#"macro add(a, b)
  a + b
end
macro mul(a, b)
  add(a, a)
end
macro square(x)
  mul(x, x)
end
let __result = macroexpand("square", [7])
task main()
  print("macroexpand(square,[7])=" + __result)
end"#;
    let result = run_mora(src).expect("macroexpand nested should succeed");
    // square(7) = mul(7,7) = add(7,7) = 14.0
    assert_eq!(
        result.to_string(),
        "14.0",
        "macroexpand('square', [7]) should equal 14.0, got {:?}",
        result
    );
}

#[test]
fn v087_macroexpand_rejects_bad_name() {
    let src = r#"let __result = macroexpand(42, [1, 2])
task main()
end"#;
    let err = run_mora(src).expect_err("macroexpand with non-string name should fail");
    assert!(
        err.contains("expects a string name"),
        "expected 'expects a string name' error, got: {}",
        err
    );
}