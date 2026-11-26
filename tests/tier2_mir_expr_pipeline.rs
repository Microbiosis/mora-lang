//! Tier 2: ParserV3 + Control SSA pipeline integration tests
//!
//! 验证完整的 V3 管线：`ParserV3::compile → check_program_witnesses → runtime::vm::{execute_program, run_main_task}`

use std::sync::Arc;

use mora::mir::control::{ControlConst, ControlInst, ControlProgram};
use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::{execute_program, run_main_task};
use mora::typeck::check_mir::check_program_witnesses;

fn compile_v3(source: &str) -> ControlProgram {
    ParserV3::compile(source).expect("compile should succeed").0
}

fn entry_insts(prog: &ControlProgram) -> Vec<&ControlInst> {
    prog.func(prog.entry)
        .blocks
        .iter()
        .flat_map(|b| b.insts.iter())
        .collect()
}

fn run_v3_pipeline(source: &str) -> mora::error::MoraResult<()> {
    let (prog, witnesses) = ParserV3::compile(source)?;
    let _type_errors = check_program_witnesses(&witnesses);
    let mut core = CoreRuntime::default();
    let prog_arc = Arc::new(prog);
    let (_, globals) = execute_program(prog_arc.clone(), &mut core)?;
    let _ = run_main_task(prog_arc, globals, &mut core)?;
    Ok(())
}

// ===================================================================
// 1. 语法覆盖测试 — 验证 ParserV3 能解析基本结构
// ===================================================================

#[test]
fn v3_parse_literal_expression() {
    let prog = compile_v3("42");
    assert!(
        entry_insts(&prog)
            .into_iter()
            .any(|inst| matches!(inst, ControlInst::Const(_, _)))
    );
}

#[test]
fn v3_parse_string_expression() {
    let prog = compile_v3(r#""hello""#);
    assert!(
        entry_insts(&prog)
            .into_iter()
            .any(|inst| matches!(inst, ControlInst::Const(_, _)))
    );
}

#[test]
fn v3_parse_variable_reference() {
    let prog = compile_v3("let x = 1\nx");
    assert!(entry_insts(&prog).into_iter().any(|inst| matches!(
        inst,
        // P6-1 Perceus/FBIP：变量的**最后一次**读取会被升级为所有权转移指令
        // （`MoveGlobal` / `MoveLocal`）—— 它同样是「变量读取」指令，只是额外
        // 携带「该槽位在此处交出所有权」的语义。
        ControlInst::LoadGlobal(_, _)
            | ControlInst::LoadLocal(_, _)
            | ControlInst::MoveGlobal(_, _)
            | ControlInst::MoveLocal(_, _)
    )));
}

#[test]
fn v3_parse_binary_expression() {
    let prog = compile_v3("1 + 2");
    assert!(
        entry_insts(&prog)
            .into_iter()
            .any(|inst| matches!(inst, ControlInst::Const(_, _)))
    );
}

#[test]
fn v3_parse_list_literal() {
    let prog = compile_v3("[1, 2, 3]");
    assert!(
        entry_insts(&prog)
            .into_iter()
            .any(|inst| matches!(inst, ControlInst::ListLit(_, _)))
    );
}

#[test]
fn v3_parse_dict_literal() {
    let prog = compile_v3(r#"{key: "value"}"#);
    assert!(
        entry_insts(&prog)
            .into_iter()
            .any(|inst| matches!(inst, ControlInst::DictLit(_, _)))
    );
}

// ===================================================================
// 2. Lowering 单元测试 — 验证 MirWitness → ControlProgram 转换
// ===================================================================

#[test]
fn v3_lower_literal_produces_const() {
    let prog = compile_v3("42");
    let insts = entry_insts(&prog);
    assert!(!insts.is_empty());
    assert!(
        insts
            .into_iter()
            .any(|inst| matches!(inst, ControlInst::Const(_, ControlConst::Int(42))))
    );
}

#[test]
fn v3_lower_binary_produces_binary_op() {
    let prog = compile_v3("let a = 1\nlet b = 2\na + b");
    assert!(entry_insts(&prog).into_iter().any(|inst| matches!(
        inst,
        ControlInst::Binary(_, _, mora::common::BinaryOp::Add, _)
    )));
}

#[test]
fn v3_lower_let_binding_produces_define() {
    let prog = compile_v3("let x = 42");
    assert!(entry_insts(&prog).into_iter().any(|inst| matches!(
        inst,
        ControlInst::StoreGlobal(_, _) | ControlInst::StoreLocal(_, _)
    )));
}

#[test]
fn v3_lower_variable_reference_produces_var() {
    let prog = compile_v3("let x = 42\nx");
    assert!(entry_insts(&prog).into_iter().any(|inst| matches!(
        inst,
        // P6-1 Perceus/FBIP：变量的**最后一次**读取会被升级为所有权转移指令
        // （`MoveGlobal` / `MoveLocal`）—— 它同样是「变量读取」指令，只是额外
        // 携带「该槽位在此处交出所有权」的语义。
        ControlInst::LoadGlobal(_, _)
            | ControlInst::LoadLocal(_, _)
            | ControlInst::MoveGlobal(_, _)
            | ControlInst::MoveLocal(_, _)
    )));
}

#[test]
fn v3_lower_list_literal_produces_list_lit() {
    let prog = compile_v3("[1, 2]");
    assert!(
        entry_insts(&prog)
            .into_iter()
            .any(|inst| matches!(inst, ControlInst::ListLit(_, _)))
    );
}

#[test]
fn v3_lower_dict_literal_produces_dict_lit() {
    let prog = compile_v3(r#"{a: 1}"#);
    assert!(
        entry_insts(&prog)
            .into_iter()
            .any(|inst| matches!(inst, ControlInst::DictLit(_, _)))
    );
}

// ===================================================================
// 3. Type-check + lowering 集成测试
// ===================================================================

#[test]
fn v3_typecheck_then_lower_succeeds() {
    let (prog, witnesses) = ParserV3::compile("let x = 42").expect("compile should succeed");
    let _errors = check_program_witnesses(&witnesses);
    assert!(
        !entry_insts(&prog).is_empty(),
        "lowered function should have instructions"
    );
}

// ===================================================================
// 4. 端到端执行测试（简单程序）
// ===================================================================

#[test]
fn v3_pipeline_let_then_variable_runs() {
    run_v3_pipeline("task main()\n  let x = 42\nend")
        .expect("V3 pipeline should execute let + variable");
}

#[test]
fn v3_pipeline_binary_expression_runs() {
    run_v3_pipeline("task main()\n  let result = 1 + 2\nend")
        .expect("V3 pipeline should execute binary expression");
}

#[test]
fn v3_pipeline_nested_binary_runs() {
    run_v3_pipeline("task main()\n  let result = (1 + 2) * 3\nend")
        .expect("V3 pipeline should execute nested binary");
}

#[test]
fn v3_pipeline_list_literal_runs() {
    run_v3_pipeline("task main()\n  let xs = [1, 2, 3]\nend")
        .expect("V3 pipeline should execute list literal");
}

#[test]
fn v3_pipeline_dict_literal_runs() {
    run_v3_pipeline(
        r#"task main()
  let d = {"key": "value"}
end"#,
    )
    .expect("V3 pipeline should execute dict literal");
}

#[test]
fn v3_pipeline_multiple_statements_runs() {
    run_v3_pipeline("task main()\n  let a = 1\n  let b = 2\n  let c = a + b\nend")
        .expect("V3 pipeline should execute multiple statements");
}

// ===================================================================
// 5. 高级结构端到端测试
// ===================================================================

#[test]
fn v3_pipeline_if_else_runs() {
    run_v3_pipeline(
        "task main()\n  let x = 5\n  if x > 3 { print(\"big\") } else { print(\"small\") }\nend",
    )
    .expect("if-else should run");
}

#[test]
fn v3_pipeline_for_loop_runs() {
    run_v3_pipeline("task main()\n  let total = 0\n  for i in [1, 2, 3] { total = total + i }\n  print(total)\nend").expect("for loop should run");
}

#[test]
fn v3_pipeline_function_call_runs() {
    run_v3_pipeline("task main()\n  print(42)\nend").expect("function call should run");
}

#[test]
fn v3_pipeline_match_runs() {
    run_v3_pipeline("match 42 {\n  _ => print(\"matched\")\n}").expect("match should run");
}

#[test]
fn v3_pipeline_closure_runs() {
    run_v3_pipeline("let f = fn(x) x * 2 end\nprint(f(21))").expect("closure should run");
}

// ===================================================================
// 6. 类型检查集成测试
// ===================================================================

#[test]
fn v3_typecheck_unbound_variable() {
    let (_func, witnesses) = ParserV3::compile("let x = missing").expect("compile should succeed");
    let errs = check_program_witnesses(&witnesses);
    assert!(!errs.is_empty(), "unbound variable should produce error");
}

#[test]
fn v3_typecheck_clean_program() {
    let (_func, witnesses) =
        ParserV3::compile("let x = 1 + 2\nprint(x)").expect("compile should succeed");
    let errs = check_program_witnesses(&witnesses);
    assert!(errs.is_empty(), "clean program should have no errors");
}

// ===================================================================
// 7. v0.75 特性回归测试
// ===================================================================

#[test]
fn v3_pipeline_string_concat_runs() {
    run_v3_pipeline("let a = \"hello\"\nlet b = \" \"\nlet c = \"world\"\nprint(a + b + c)")
        .expect("string concat should run");
}

#[test]
fn v3_pipeline_dict_access_runs() {
    run_v3_pipeline("let d = {\"key\": 42}\nprint(d[\"key\"])").expect("dict access should run");
}

#[test]
fn v3_pipeline_nested_if_runs() {
    run_v3_pipeline("let x = 5\nif x > 3 {\n  if x > 4 {\n    print(\"big\")\n  }\n}")
        .expect("nested if should run");
}

#[test]
fn v3_pipeline_task_define_and_call_runs() {
    run_v3_pipeline("task add(a, b)\n  a + b\nend\nprint(add(1, 2))")
        .expect("task define and call should run");
}

#[test]
fn v3_pipeline_closure_capture_runs() {
    run_v3_pipeline("let base = 10\nlet offset = fn(x) x + base end\nprint(offset(5))")
        .expect("closure capture should run");
}

#[test]
fn v3_pipeline_match_with_literal_runs() {
    run_v3_pipeline(
        "let x = 42\nmatch x {\n  42 => print(\"found\"),\n  _ => print(\"not found\")\n}",
    )
    .expect("match with literal should run");
}

#[test]
fn v3_pipeline_eval_assertion_runs() {
    run_v3_pipeline("eval \"sanity\" 2 + 2, 4\nprint(\"ok\")").expect("eval assertion should run");
}
