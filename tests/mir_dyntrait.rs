//! DynTrait 路径测试 (v0.77 重命名自 tier0_dyntrait.rs)
//!
//! v0.77 重构：删除 4 个 source-grep 静态合约测试（`MirInst::DynTrait` 存在性、
//! lower_expr 处理 ExprKind::DynTrait、handlers 构造 TraitObject、lexer 支持
//! as/dyn 关键字 — 任何重命名都会假阳性断裂）。保留 2 个 runtime 测试。
//!
//! ⚠ **历史说明**：上文的 `MirInst::DynTrait` 属**已退役**的 9 层 IR（v0.110.0
//! 删除）。现存链路是 `WitnessKind::DynTrait`（`src/mir/witness.rs`）→
//! `ControlInst::MakeDynTrait`（`src/mir/control/ir.rs`）→ 构造
//! `Value::TraitObject`。
//!
//! α.12 验证 DynTrait cast 表达式从 parser → lowering → interp 完整链路，
//! 构造 Value::TraitObject 包内嵌 expr。

use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::{execute_program, run_main_task};
use mora::typeck::check_mir::check_program_witnesses;

fn run_via_execute(source: &str) -> Result<(), String> {
    let (prog, witnesses) = ParserV3::compile(source).map_err(|e| e.to_string())?;
    let type_errs = check_program_witnesses(&witnesses);
    if !type_errs.is_empty() {
        return Err(format!("typeck: {} error(s)", type_errs.len()));
    }
    let mut core = CoreRuntime::default();
    let arc = std::sync::Arc::new(prog);
    let (_, globals) =
        execute_program(std::sync::Arc::clone(&arc), &mut core).map_err(|e| e.to_string())?;
    run_main_task(arc, globals, &mut core)
        .map(|_| ())
        .map_err(|e| e.to_string())
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
    run_via_execute(src).expect("dyn Trait cast must parse and execute via MIR");
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
    run_via_execute(src).expect("chained dyn Trait cast must work");
}

// =================================================================
// v0.85: Compile path (single-pass emit) tests for the 3 new features.
// =================================================================
// The legacy tests above use `parse_code_v3` (parse→lower path).
// These tests use `ParserV3::compile` (single-pass emit path) which
// exercises the new emit_call_tail_w / emit_let_w / emit_with_w code.

use mora::mir::control::{ControlInst, ControlProgram};

/// Helper: compile via single-pass path and return the ControlProgram.
fn compile_prog(source: &str) -> ControlProgram {
    let (prog, _witnesses) =
        mora::parser_v3::ParserV3::compile(source).expect("compile should succeed");
    prog
}

/// Helper: find first matching `ControlInst` across entry function blocks.
fn find_dyn_trait(prog: &ControlProgram) -> Option<(String, u32, u32)> {
    let entry = prog.func(prog.entry);
    for block in &entry.blocks {
        for inst in &block.insts {
            if let ControlInst::MakeDynTrait {
                dst,
                src,
                trait_name,
                ..
            } = inst
            {
                return Some((prog.symbols.resolve(*trait_name).to_string(), src.0, dst.0));
            }
        }
    }
    None
}

#[test]
fn as_dyn_trait_in_emit_path_works() {
    let prog = compile_prog("42 as dyn Any");
    let found = find_dyn_trait(&prog);
    assert!(
        found.is_some(),
        "expected ControlInst::MakeDynTrait in compile path, got: {:?}",
        prog.func(prog.entry).blocks
    );
    let (trait_name, src_reg, dst) = found.unwrap();
    assert_eq!(trait_name, "Any");
    assert_ne!(src_reg, dst, "MakeDynTrait should write to a new register");
}

#[test]
fn let_dyn_trait_auto_coerces() {
    let prog = compile_prog("let p: dyn Any = 42");
    let found = find_dyn_trait(&prog);
    assert!(
        found.is_some(),
        "expected ControlInst::MakeDynTrait auto-coercion for `let p: dyn Any = 42`, got: {:?}",
        prog.func(prog.entry).blocks
    );
    let (trait_name, _, _) = found.unwrap();
    assert_eq!(trait_name, "Any");
}
