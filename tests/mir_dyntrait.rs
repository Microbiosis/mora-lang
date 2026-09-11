//! DynTrait MIR 路径测试 (v0.77 重命名自 tier0_dyntrait.rs)
//!
//! v0.77 重构：删除 4 个 source-grep 静态合约测试（MirInst::DynTrait 存在性、
//! lower_expr 处理 ExprKind::DynTrait、handlers 构造 TraitObject、lexer 支持
//! as/dyn 关键字 — 任何重命名都会假阳性断裂）。保留 2 个 runtime 测试。
//!
//! α.12 验证 DynTrait cast 表达式从 parser → lowering → interp 完整链路，
//! 构造 Value::TraitObject 包内嵌 expr。

use mora::interpreter::Interpreter;
use mora::mir::vm::{run_main_task, run_mir};
use mora::parser_v3::ParserV3;
use mora::typeck::check_mir::check_program_witnesses;

fn run_via_mir(source: &str) -> Result<(), String> {
    let (func, witnesses) = ParserV3::compile(source)?;
    let type_errs = check_program_witnesses(&witnesses);
    if !type_errs.is_empty() {
        return Err(format!("typeck: {} error(s)", type_errs.len()));
    }
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let func_arc = std::sync::Arc::new(func);
    run_mir(&func_arc, &mut interp, &mut env, &mut mora::mir::effect::Effects::new())?;
    run_main_task(&func_arc, &mut interp, &mut env, &mut mora::mir::effect::Effects::new())
}

#[test]
fn dyntrait_cast_parses_and_lowers() {
    let src = r#"
task main()
  let x = 42
  let obj = x as dyn Any
  print(obj)
end
"#;
    run_via_mir(src).expect("dyn Trait cast must parse and execute via MIR");
}

#[test]
fn dyntrait_chained_cast() {
    let src = r#"
task main()
  let n = 1
  let obj1 = n as dyn Any
  let obj2 = obj1 as dyn Any
  print(obj2)
end
"#;
    run_via_mir(src).expect("chained dyn Trait cast must work");
}

// =================================================================
// v0.85: Compile path (single-pass emit) tests for the 3 new features.
// =================================================================
// The legacy tests above use `parse_code_v3` (parse→lower path).
// These tests use `ParserV3::compile` (single-pass emit path) which
// exercises the new emit_call_tail_w / emit_let_w / emit_with_w code.

use mora::mir::MirInst;

/// Helper: compile via single-pass path and return the MirFunction body.
fn compile_body(source: &str) -> Vec<MirInst> {
    let (func, _witnesses) = mora::parser_v3::ParserV3::compile(source)
        .expect("compile should succeed");
    func.body
}

/// Helper: find first instruction of a given kind.
fn find_inst(body: &[MirInst], pred: impl Fn(&MirInst) -> bool) -> Option<&MirInst> {
    body.iter().find(|inst| pred(inst))
}

#[test]
fn as_dyn_trait_in_emit_path_works() {
    let src = "42 as dyn Any";
    let (func, _witnesses) = mora::parser_v3::ParserV3::compile(src)
        .expect("compile should succeed");
    let dyn_inst = find_inst(&func.body, |inst| matches!(inst, MirInst::DynTrait { .. }));
    assert!(
        dyn_inst.is_some(),
        "expected MirInst::DynTrait in compile path body, got: {:?}",
        func.body
    );
    if let MirInst::DynTrait {
        trait_name, src: src_reg, dst, ..
    } = dyn_inst.unwrap()
    {
        assert_eq!(trait_name, "Any");
        assert_ne!(*src_reg, *dst, "DynTrait should write to a new register");
    }
}

#[test]
fn let_dyn_trait_auto_coerces() {
    let body = compile_body("let p: dyn Any = 42");
    let dyn_inst = find_inst(&body, |inst| matches!(inst, MirInst::DynTrait { .. }));
    assert!(
        dyn_inst.is_some(),
        "expected MirInst::DynTrait auto-coercion for `let p: dyn Any = 42`, got: {:?}",
        body
    );
    if let MirInst::DynTrait { trait_name, .. } = dyn_inst.unwrap() {
        assert_eq!(trait_name, "Any");
    }
}

#[test]
fn with_mock_llm_block_parses() {
    // `with` at top level (not nested inside task body) — WithConfig lands directly in func.body
    let body = compile_body(
        r#"
with mock_llm = ["hello", "world"]
  print("test")
end
"#,
    );
    let with_inst = find_inst(&body, |inst| matches!(inst, MirInst::WithConfig { .. }));
    assert!(
        with_inst.is_some(),
        "expected MirInst::WithConfig in compile path body, got: {:?}",
        body
    );
    if let MirInst::WithConfig {
        bindings, body: nested, ..
    } = with_inst.unwrap()
    {
        assert!(
            bindings.iter().any(|(name, _)| name == "mock_llm"),
            "expected mock_llm binding in WithConfig, got: {:?}",
            bindings
        );
        assert!(
            !nested.body.is_empty(),
            "WithConfig body should contain at least one instruction"
        );
    }
}

#[test]
fn with_mock_llm_inside_task_parses() {
    // `with` inside task body — verifies emit_statement_expr_w dispatch
    let (func, _witnesses) = mora::parser_v3::ParserV3::compile(
        r#"
task main()
  with mock_llm = ["hello"]
    print("ok")
  end
end
"#,
    ).expect("compile should succeed");

    fn find_with_config(body: &[MirInst]) -> bool {
        body.iter().any(|inst| match inst {
            MirInst::WithConfig { .. } => true,
            MirInst::TaskDef { body: nested, .. } => find_with_config(&nested.body),
            _ => false,
        })
    }
    assert!(
        find_with_config(&func.body),
        "expected MirInst::WithConfig in task body, got: {:?}",
        func.body
    );
}