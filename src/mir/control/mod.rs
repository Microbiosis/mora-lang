//! v0.110.0: Canonical explicit-CFG Control SSA IR and optimization subsystem.
//!
//! Pipeline:
//! `MirWitness[] -> control::lower::lower_program -> ControlProgram -> control::opt::optimize_program`

pub mod codec;
pub mod ir;
pub mod lower;
pub mod opt;

pub use ir::{
    BasicBlock, BlockId, BuiltinFnId, CalleeTarget, CaptureSource, CompiledPattern, ControlConst,
    ControlFunction, ControlImplMethod, ControlInst, ControlProgram, ControlQqSegment,
    ControlSelectArm, ControlSliceAxis, ControlSpawnTarget, ControlStructField, ControlTerminator,
    ControlTraitMethod, FnId, FunctionKind, GlobalKind, GlobalMeta, GlobalSlot, LocalSlot,
    ParallelSegment, PatternTarget, Purity, SymId, SymbolTable, UnaryOp, UpvalueDesc, UpvalueSlot,
    VReg, verify_program,
};
pub use lower::{lower_program, lower_program_with_dyn_globals};
pub use opt::{OptLevel, optimize_program};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser_v3::ParserV3;

    fn compile_ctrl(src: &str, opt: OptLevel) -> ControlProgram {
        let (_legacy, witnesses) = ParserV3::compile(src).expect("parse program");
        let mut prog = lower_program(&witnesses).expect("lower to ControlProgram");
        optimize_program(&mut prog, opt);
        verify_program(&prog).expect("optimized ControlProgram must pass verification");
        prog
    }

    #[test]
    fn lowers_and_verifies_control_flow_and_loops() {
        let src = r#"
let total = 0i
for x in [1i, 2i, 3i, 4i]
  if x == 3i
    continue
  end
  let total = total + x
end
total
"#;
        for opt in [OptLevel::None, OptLevel::Basic, OptLevel::Aggressive] {
            let prog = compile_ctrl(src, opt);
            assert!(!prog.globals.is_empty());
        }
    }

    #[test]
    fn constant_folding_and_dead_branch_elimination() {
        let src = r#"
if true
  10i + 32i
else
  print("dead branch")
  999i
end
"#;
        let prog = compile_ctrl(src, OptLevel::Aggressive);
        let main = prog.func(prog.entry);
        // Constant branch folded -> unreachable else block stripped by DCE.
        let has_print = main.blocks.iter().any(|b| {
            b.insts.iter().any(|i| {
                matches!(
                    i,
                    ControlInst::Call {
                        target: CalleeTarget::Builtin(BuiltinFnId::Print),
                        ..
                    }
                )
            })
        });
        assert!(
            !has_print,
            "dead constant-false else branch should be eliminated"
        );
        let has_42 = main.blocks.iter().any(|b| {
            b.insts
                .iter()
                .any(|i| matches!(i, ControlInst::Const(_, ControlConst::Int(42))))
        });
        assert!(has_42, "10i + 32i should fold to 42i");
    }

    #[test]
    fn preserves_fallible_ops_when_dce_runs() {
        let src = r#"
task test(x)
  let _unused = x / 0i
  return 1i
end
"#;
        let prog = compile_ctrl(src, OptLevel::Aggressive);
        let test_fn = prog
            .functions
            .iter()
            .find(|f| f.kind == FunctionKind::Task)
            .expect("task compiled");
        let has_div = test_fn.blocks.iter().any(|b| {
            b.insts.iter().any(|i| {
                matches!(
                    i,
                    ControlInst::Binary(_, _, crate::common::BinaryOp::Div, _)
                )
            })
        });
        assert!(
            has_div,
            "fallible divide-by-zero must not be eliminated by DCE"
        );
    }

    #[test]
    fn preserves_fallible_ops_in_zero_trip_loop_licm() {
        let src = r#"
task f(n)
  let i = 0i
  while i < 0i
    let bad = n / 0i
    i = i + 1i
  end
  return 7i
end
"#;
        let prog = compile_ctrl(src, OptLevel::Aggressive);
        let f_fn = prog
            .functions
            .iter()
            .find(|f| f.kind == FunctionKind::Task)
            .expect("task f");
        // Verify divide remains inside loop block, NOT hoisted to header/entry predecessor.
        assert!(
            !f_fn.blocks[0].insts.iter().any(|i| matches!(
                i,
                ControlInst::Binary(_, _, crate::common::BinaryOp::Div, _)
            )),
            "fallible op must never be hoisted into preheader outside 0-trip loop"
        );
    }

    #[test]
    fn short_circuit_and_match_guards_lower_cleanly() {
        let src = r#"
task bump() : bool
  print("BUMP")
  return true
end
let a = true or bump()
let b = false and bump()
let m = match -5i with
  x when x > 0i -> 1i
  x when x < 0i -> 2i
  _ -> 0i
end
m
"#;
        for opt in [OptLevel::None, OptLevel::Basic, OptLevel::Aggressive] {
            compile_ctrl(src, opt);
        }
    }

    #[test]
    fn delimited_effect_handler_auto_resume_and_explicit_resume() {
        let src = r#"
let r1 = handle Ai {
  perform Ai("hello")
} {
  "mock:" + __arg0
}
let r2 = handle Choice {
  let x = perform Choice()
  x * 10i
} {
  resume(1i) + resume(2i)
}
"#;
        let prog = compile_ctrl(src, OptLevel::Basic);
        let main = prog.func(prog.entry);
        let handles: Vec<bool> = main
            .blocks
            .iter()
            .flat_map(|b| &b.insts)
            .filter_map(|i| match i {
                ControlInst::Handle { auto_resume, .. } => Some(*auto_resume),
                _ => None,
            })
            .collect();
        assert_eq!(handles, vec![true, false]);
    }

    #[test]
    fn closure_capture_promotes_mutated_outer_local_to_shared() {
        let src = r#"
task make_counter()
  let n = 0i
  let inc = fn()
    n = n + 1i
    return n
  end
  return inc
end
"#;
        let prog = compile_ctrl(src, OptLevel::Basic);
        let maker = prog
            .functions
            .iter()
            .find(|f| f.kind == FunctionKind::Task)
            .expect("make_counter");
        assert!(
            maker.shared_locals.iter().any(|&s| s),
            "captured and mutated local `n` must promote to shared UpvalueCell"
        );
        let closure = prog
            .functions
            .iter()
            .find(|f| f.kind == FunctionKind::Closure)
            .expect("closure");
        assert!(
            closure.upvalues.iter().any(|u| u.shared),
            "closure upvalue descriptor for `n` must have shared=true"
        );
    }

    #[test]
    fn compiles_every_example_program_without_panic() {
        let examples_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
        if !examples_dir.exists() {
            return;
        }
        let mut count = 0usize;
        for entry in std::fs::read_dir(&examples_dir).expect("read examples") {
            let path = entry.expect("entry").path();
            if path.extension().and_then(|s| s.to_str()) == Some("mora") {
                let src = std::fs::read_to_string(&path).expect("read .mora");
                if let Ok((_, witnesses)) = ParserV3::compile(&src) {
                    let mut prog = lower_program(&witnesses)
                        .unwrap_or_else(|e| panic!("{}: lower failed: {e}", path.display()));
                    optimize_program(&mut prog, OptLevel::Aggressive);
                    verify_program(&prog)
                        .unwrap_or_else(|e| panic!("{}: verify failed: {e}", path.display()));
                    count += 1;
                }
            }
        }
        assert!(count > 0, "expected at least one .mora example compiled");
    }
}
