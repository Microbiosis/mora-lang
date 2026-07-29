//! Tier 0 → Tier 1 集成测试: `expr as dyn Trait` 语法走 MIR
//!
//! α.12 验证 DynTrait cast 表达式从 parser → lowering → interp 完整链路，
//! 构造 Value::TraitObject 包内嵌 expr。

use mora::interpreter::Interpreter;
use mora::mir::interp::{run_main_task, run_mir};
use mora::mir::lower::{lower_mir_exprs, typecheck_mir_exprs};

fn run_via_mir(source: &str) -> Result<(), String> {
    let mut exprs = mora::interpreter::parse_code_v3(source)?;
    let type_errs = typecheck_mir_exprs(&mut exprs);
    if !type_errs.is_empty() {
        return Err(format!("typeck: {} error(s)", type_errs.len()));
    }
    let func = lower_mir_exprs(&exprs)?;
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    run_mir(&func, &mut interp, &mut env)?;
    run_main_task(&func, &mut interp, &mut env)
}

// ─── 1. `expr as dyn Trait` 解析 + 构造 Value::TraitObject ──────────
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

// ─── 2. 嵌套 dyn trait cast (chained) ─────────────────────────────
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

// ─── 3. 静态合约 ────────────────────────────────────────────────
#[test]
fn dyntrait_mir_instruction_exists() {
    let src = std::fs::read_to_string("src/mir/mod.rs").expect("mir/mod.rs");
    assert!(
        src.contains("DynTrait {"),
        "MirInst::DynTrait variant must exist"
    );
}

#[test]
fn dyntrait_lowering_emits_instruction() {
    let src = std::fs::read_to_string("src/mir/lower.rs").expect("mir/lower.rs");
    assert!(
        src.contains("ExprKind::DynTrait"),
        "lower_expr must handle ExprKind::DynTrait"
    );
    assert!(
        src.contains("MirInst::DynTrait {"),
        "lower_expr must emit MirInst::DynTrait"
    );
}

#[test]
fn dyntrait_interp_constructs_trait_object() {
    let src = std::fs::read_to_string("src/mir/interp.rs").expect("mir/interp.rs");
    assert!(
        src.contains("MirInst::DynTrait {"),
        "run_mir must handle MirInst::DynTrait"
    );
    assert!(
        src.contains("Value::TraitObject {"),
        "DynTrait handler must construct Value::TraitObject"
    );
}

#[test]
fn dyntrait_parser_supports_as_dyn() {
    let src =
        std::fs::read_to_string("src/parser_v2/expressions.rs").expect("parser expressions.rs");
    assert!(
        src.contains("TokenType::As"),
        "parser must consume 'as' keyword"
    );
    assert!(
        src.contains("TokenType::Dyn"),
        "parser must consume 'dyn' keyword"
    );
    assert!(
        src.contains("ExprKind::DynTrait"),
        "parser must construct ExprKind::DynTrait"
    );
}

#[test]
fn dyntrait_typeck_returns_trait_type() {
    let src = std::fs::read_to_string("src/typeck/check.rs").expect("typeck/check.rs");
    assert!(
        src.contains("ExprKind::DynTrait"),
        "typeck must handle ExprKind::DynTrait"
    );
}
