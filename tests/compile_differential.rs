//! v0.75.39: ParserV3::compile 管线验证测试。
//!
//! 验证 compile 产出的 ControlProgram 具有正确的结构（entry 块非空、verify_program 通过）。
//! 旧 parse→lower 路径已删除（AGENTS.md §6 禁止兼容桥）。

use mora::parser_v3::ParserV3;

/// 验证 compile 产出的 ControlProgram 结构正确。
fn assert_compile_valid(source: &str) {
    let (prog, witnesses) = ParserV3::compile(source).expect("compile should succeed");
    assert!(
        mora::mir::control::verify_program(&prog).is_ok(),
        "compile 产出的 ControlProgram 必须通过 SSA 校验\nsource: {source}"
    );
    let entry_fn = prog.func(prog.entry);
    assert!(
        !entry_fn.blocks.is_empty(),
        "compile 产出的 ControlProgram entry 函数不应为空\nsource: {source}"
    );
    assert!(
        !witnesses.is_empty(),
        "compile 产出的 witnesses 不应为空\nsource: {source}"
    );
}

#[test]
fn compile_valid_literal() {
    assert_compile_valid("42");
    assert_compile_valid("\"hello\"");
    assert_compile_valid("3.14");
    assert_compile_valid("true");
}

#[test]
fn compile_valid_binary_and_variable() {
    assert_compile_valid("let x = 1\nlet y = x + 2\nprint(y)");
    assert_compile_valid("print(10 * 2 - 3)");
}

#[test]
fn compile_valid_control_flow() {
    assert_compile_valid("let n = 3\nif n > 0 { print(\"pos\") }");
    assert_compile_valid("let i = 0\nwhile i < 3\n  i = i + 1\nend\nprint(i)");
    assert_compile_valid(
        "let items = [1, 2, 3]\nlet sum = 0\nfor x in items\n  sum = sum + x\nend\nprint(sum)",
    );
}

#[test]
fn compile_valid_call_and_closure() {
    assert_compile_valid("let ops = {\"mul\": fn(x) x * 2 end}\nprint(ops.mul(5))");
    assert_compile_valid("print(len([1, 2, 3]))");
}

#[test]
fn compile_valid_match() {
    assert_compile_valid("match 42 {\n  _ => \"default\"\n}");
}

#[test]
fn compile_rejects_retired_prompt_and_accepts_string_concat() {
    assert!(
        ParserV3::compile("print(p\"hello {name}\")").is_err(),
        "v0.110.0 W2.6 retired p\"...\" literals from parser"
    );
    assert_compile_valid("let name = \"world\"\nprint(\"hello \" + name)");
    assert_compile_valid("let n = 10\nlet msg = \"score: \" + str(n) + \" points\"\nprint(msg)");
}

#[test]
fn compile_valid_nested_constructs() {
    assert_compile_valid(
        "let pick = fn(n)\n  if n > 0 {\n    1\n  } else {\n    0\n  }\nend\nprint(pick(3))",
    );
    assert_compile_valid("let n = 1\nfor i in [1, 2, 3] {\n  if i > n {\n    print(i)\n  }\n}");
    assert_compile_valid("match 0 {\n  0 => print(\"zero\"),\n  _ => print(\"other\"),\n}");
}

#[test]
fn compile_valid_top_level_task_and_if_value() {
    assert_compile_valid("task main()\n  print(1)\nend");
    assert_compile_valid("let pick = fn(n) if n > 0 { 1 } else { 0 } end\nprint(pick(3))");
}
