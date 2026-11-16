//! Regression suite for optimizer soundness (Wave 2 Stage W2.1).
//!
//! Guards two historical unsound rewrites:
//! 1. LICM (`src/mir/opt/loops.rs::is_loop_invariant`) previously treated
//!    `SsaInst::Var` and call instructions (`Call`, `MethodCall`, `Pipe`,
//!    `CallValue`) as loop-invariant — freezing mutable loop variable reads
//!    to iteration 1 and hoisting side-effectful calls out of 0-trip or N-trip
//!    loops.
//! 2. `AlgebraicSimplifyDagRule` (`src/mir/optimize/dag_rule.rs`) previously
//!    rewrote untyped `x * 0i` and `0i * x` to `Value::Int(0)`, corrupting
//!    Float runtime types (`2.5 * 0i -> 0.0`) and IEEE-754 semantics
//!    (`Inf * 0i -> NaN`, `NaN * 0i -> NaN`).

use mora::cli::compile_and_opt;
use mora::mir::control::{CalleeTarget, ControlInst, ControlTerminator, OptLevel};
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::test_support::TempDir;
use mora::value::Value;
use std::process::Command;
use std::sync::Arc;

const OPT_LEVELS: [OptLevel; 3] = [OptLevel::None, OptLevel::Basic, OptLevel::Aggressive];
const CLI_OPTS: [Option<&str>; 3] = [None, Some("--opt=1"), Some("--opt=2")];

fn eval_with_opt(src: &str, opt: OptLevel) -> Value {
    let (prog, _witnesses) = compile_and_opt(src, Some(opt)).expect("compile source");
    let mut core = CoreRuntime::default();
    execute_program(Arc::new(prog), &mut core)
        .expect("execute top")
        .0
}

fn run_cli(tag: &str, src: &str, opt: Option<&str>) -> (String, String, i32) {
    let dir = TempDir::new(tag);
    let path = dir.join("p.mora");
    std::fs::write(&path, src).expect("write test fixture");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mora"));
    if let Some(flag) = opt {
        cmd.arg(flag);
    }
    let out = cmd.arg("run").arg(&path).output().expect("invoke mora");
    (
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn licm_preserves_call_side_effects_in_ntrip_and_zero_trip_loops() {
    let ntrip_src = r#"
task tick() : int
  print("TICK")
  return 1i
end

let i = 0i
let total = 0i
while i < 3i
  total = total + tick()
  i = i + 1i
end
print(total)
"#;

    let zerotrip_src = r#"
task tick() : int
  print("TICK")
  return 1i
end

let i = 0i
let total = 0i
while i < 0i
  total = total + tick()
  i = i + 1i
end
print(total)
"#;

    for flag in CLI_OPTS {
        let (out_n, err_n, code_n) = run_cli("licm_ntrip", ntrip_src, flag);
        assert_eq!(code_n, 0, "opt={flag:?} failed:\nstderr={err_n}");
        assert_eq!(
            out_n.trim_end_matches('\n'),
            "TICK\nTICK\nTICK\n3",
            "3-trip loop call must run 3 times under opt={flag:?}, got stdout={out_n:?}"
        );

        let (out_z, err_z, code_z) = run_cli("licm_0trip", zerotrip_src, flag);
        assert_eq!(code_z, 0, "opt={flag:?} failed:\nstderr={err_z}");
        assert_eq!(
            out_z.trim_end_matches('\n'),
            "0",
            "0-trip loop call must run 0 times under opt={flag:?}, got stdout={out_z:?}"
        );
    }

    // Structural IR check: `Call(Fn(tick))` must remain inside the loop body
    // (reachable after the conditional loop header branch, not hoisted into entry block).
    for level in [OptLevel::Basic, OptLevel::Aggressive] {
        let (optimized, _) = compile_and_opt(ntrip_src, Some(level)).expect("compile");
        let main_fn = optimized.func(optimized.entry);
        let entry_has_call = main_fn.blocks[main_fn.entry.0 as usize]
            .insts
            .iter()
            .any(|i| {
                matches!(
                    i,
                    ControlInst::Call {
                        target: CalleeTarget::Fn(_),
                        ..
                    }
                )
            });
        let branch_bb = main_fn
            .blocks
            .iter()
            .position(|b| matches!(b.terminator, ControlTerminator::Branch { .. }))
            .expect("loop header branch");
        let call_bb = main_fn
            .blocks
            .iter()
            .position(|b| {
                b.insts.iter().any(|i| {
                    matches!(
                        i,
                        ControlInst::Call {
                            target: CalleeTarget::Fn(_),
                            ..
                        }
                    )
                })
            })
            .expect("call to tick");
        assert!(
            !entry_has_call && call_bb >= branch_bb,
            "opt={level:?}: Call(tick) in block {call_bb} was hoisted ahead of loop guard block {branch_bb}"
        );
    }
}

#[test]
fn licm_does_not_hoist_loop_mutated_var_reads() {
    let src = r#"
let i = 0i
let acc = 0i
let x = 10i
while i < 3i
  acc = acc + x
  x = x + 5i
  i = i + 1i
end
acc
"#;

    for level in OPT_LEVELS {
        let val = eval_with_opt(src, level);
        assert_eq!(
            val,
            Value::Int(45),
            "opt={level:?}: x mutates (10 -> 15 -> 20), so acc must equal 45, got {val:?}"
        );
    }
}

#[test]
fn mul_zero_preserves_float_type_and_ieee754_nan_semantics() {
    // Pass dynamic values via `var x` so ConstantFolding cannot fold statically,
    // exercising the DAG algebraic simplifier path under all opt levels.
    let float_rhs_src = "let x = 2.5\nif false { x = 1.0 }\nx * 0i";
    let float_lhs_src = "let x = 2.5\nif false { x = 1.0 }\n0i * x";
    let inf_rhs_src = "let x = 1.0 / 0.0\nif false { x = 1.0 }\nx * 0i";
    let inf_lhs_src = "let x = 1.0 / 0.0\nif false { x = 1.0 }\n0i * x";
    let nan_rhs_src = "let x = 0.0 / 0.0\nif false { x = 1.0 }\nx * 0i";
    let int_folded_src = "5i * 0i";

    for level in OPT_LEVELS {
        match eval_with_opt(float_rhs_src, level) {
            Value::Float(v) => assert_eq!(v, 0.0, "opt={level:?}: 2.5 * 0i must remain Float(0.0)"),
            other => panic!("opt={level:?}: 2.5 * 0i coerced to non-Float {other:?}"),
        }
        match eval_with_opt(float_lhs_src, level) {
            Value::Float(v) => assert_eq!(v, 0.0, "opt={level:?}: 0i * 2.5 must remain Float(0.0)"),
            other => panic!("opt={level:?}: 0i * 2.5 coerced to non-Float {other:?}"),
        }
        match eval_with_opt(inf_rhs_src, level) {
            Value::Float(v) => {
                assert!(v.is_nan(), "opt={level:?}: Inf * 0i must be NaN, got {v}")
            }
            other => panic!("opt={level:?}: Inf * 0i coerced to non-Float {other:?}"),
        }
        match eval_with_opt(inf_lhs_src, level) {
            Value::Float(v) => {
                assert!(v.is_nan(), "opt={level:?}: 0i * Inf must be NaN, got {v}")
            }
            other => panic!("opt={level:?}: 0i * Inf coerced to non-Float {other:?}"),
        }
        match eval_with_opt(nan_rhs_src, level) {
            Value::Float(v) => {
                assert!(v.is_nan(), "opt={level:?}: NaN * 0i must be NaN, got {v}")
            }
            other => panic!("opt={level:?}: NaN * 0i coerced to non-Float {other:?}"),
        }
        assert_eq!(
            eval_with_opt(int_folded_src, level),
            Value::Int(0),
            "opt={level:?}: pure constant 5i * 0i should yield Int(0)"
        );
    }
}
