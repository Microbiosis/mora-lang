//! Wave 2 — Stage W2.3 regression suite:
//! - Multi-shot delimited continuations (`amb`, abort without `resume`, stateful replay,
//!   closure capture + `list.map`)
//! - O(1) CallFrame tail-call optimization (100,000+ calls on default OS thread stack + CLI)
//! - Iterative Pratt / unary parsing (`emit_pratt_w` / `emit_unary_w`) on default OS stack
//! - Static HM typechecking of explicit `resume` handlers

use std::process::Command;
use std::sync::Arc;

use mora::mir::control::{OptLevel, optimize_program};
use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::test_support::TempDir;
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

fn check_and_run(src: &str) -> Result<Value, String> {
    let (mut prog, witnesses) = ParserV3::compile(src).map_err(|e| e.to_string())?;
    let errs = check_program_witnesses(&witnesses);
    if !errs.is_empty() {
        return Err(format!(
            "TYPECK: {}",
            errs.iter()
                .map(|e| e.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    optimize_program(&mut prog, OptLevel::Basic);
    let mut core = CoreRuntime::default();
    let (val, _) = execute_program(Arc::new(prog), &mut core).map_err(|e| e.to_string())?;
    Ok(val)
}

fn run_cli(tag: &str, src: &str) -> (i32, String, String) {
    let dir = TempDir::new(tag);
    let path = dir.join("script.mora");
    std::fs::write(&path, src).expect("write temp mora script");
    let out = Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg("run")
        .arg(&path)
        .output()
        .expect("spawn mora cli");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn multishot_amb_backtracking_across_nested_performs() {
    let src = r#"
let total = handle Flip {
  let a = perform Flip()
  let b = perform Flip()
  if a + b == 5i then
    1i
  else
    0i
  end
} {
  resume(1i) + resume(4i)
}
total
"#;
    let val = check_and_run(src).expect("nested multi-shot amb backtracking must succeed");
    assert_eq!(val, Value::Int(2));
}

#[test]
fn handler_abort_without_resume_skips_body_continuation() {
    // Handler binding `resume` without invoking it opts out of legacy auto-resume and aborts.
    let src = r#"
let touched = 0i
let res = handle Abort {
  perform Abort(0i)
  touched = 999i
  1i
} {
  if __arg0 == 0i then
    42i
  else
    resume(nil)
  end
}
[res, touched]
"#;
    let val = check_and_run(src).expect("aborting handler without invoking resume must succeed");
    assert_eq!(val, Value::List(vec![Value::Int(42), Value::Int(0)].into()));
}

#[test]
fn stateful_local_snapshot_vs_shared_global_across_multishot_resume() {
    let src = r#"
let shots = 0i
task run_case() : int
  return handle Step {
    let local_snap = 10i
    let delta = perform Step()
    shots = shots + 1i
    local_snap + delta
  } {
    resume(1i) + resume(2i)
  }
end
let sum = run_case()
[sum, shots]
"#;
    let val = check_and_run(src).expect("stateful local replay + global counter must work");
    assert_eq!(val, Value::List(vec![Value::Int(23), Value::Int(2)].into()));
}

#[test]
fn resume_captured_in_closure_and_invoked_via_list_map() {
    let src = r#"
let out = handle Pick {
  let x = perform Pick()
  x * 10i
} {
  let k = resume
  let ys = [1i, 2i, 3i].map(fn(v) k(v) end)
  ys[0] + ys[1] + ys[2]
}
out
"#;
    let val = check_and_run(src).expect("resume captured in closure inside list.map must run");
    assert_eq!(val, Value::Int(60));
}

#[test]
fn tail_call_100k_iterations_and_mutual_tco_on_default_stack() {
    let direct_src = r#"
task count_down(n: int, acc: int) : int
  if n == 0i then
    return acc
  end
  return count_down(n - 1i, acc + 1i)
end
count_down(100000i, 0i)
"#;
    let val =
        check_and_run(direct_src).expect("100k direct tail recursion must not overflow stack");
    assert_eq!(val, Value::Int(100_000));

    let mutual_src = r#"
task is_even(n: int) : bool
  if n == 0i then
    return true
  end
  return is_odd(n - 1i)
end
task is_odd(n: int) : bool
  if n == 0i then
    return false
  end
  return is_even(n - 1i)
end
is_even(100000i)
"#;
    let mval =
        check_and_run(mutual_src).expect("100k mutual tail recursion must not overflow stack");
    assert_eq!(mval, Value::Bool(true));

    // Also run via compiled CLI binary on default OS main thread stack (no 64 MB wrapper).
    let cli_src = format!("{direct_src}\nprint(count_down(100000i, 0i))\n");
    let (code, stdout, stderr) = run_cli("w2_3_tco_cli", &cli_src);
    assert_eq!(code, 0, "CLI 100k TCO failed: stderr={stderr}");
    assert_eq!(stdout.trim(), "100000");
}

#[test]
fn deep_multiline_expressions_parse_and_diagnose_without_stack_overflow() {
    // Iterative `emit_pratt_w` handles 450 chained additions cleanly within MAX_NESTING_DEPTH.
    let pratt_src = format!("1i{}", " + 1i".repeat(450));
    let cli_pratt = format!("let s = {pratt_src}\nprint(s)\n");
    let (code, stdout, stderr) = run_cli("w2_3_pratt_450", &cli_pratt);
    assert_eq!(code, 0, "CLI 450-deep Pratt expr failed: stderr={stderr}");
    assert_eq!(stdout.trim(), "451");

    // Iterative `emit_unary_w` handles 450 chained unary operators cleanly on default stack.
    let unary_src = format!("let u = {}1i\nprint(u)\n", "- ".repeat(450));
    let (ucode, uout, uerr) = run_cli("w2_3_unary_450", &unary_src);
    assert_eq!(ucode, 0, "CLI 450-deep unary expr failed: stderr={uerr}");
    assert_eq!(uout.trim(), "1");

    // Multi-line `|>` pipeline (450 stages) via iterative `emit_pratt_w` on default OS stack.
    let mut pipe_450 = String::from("task inc(x: int) : int\n  return x + 1i\nend\nlet p = 0i");
    for _ in 0..450 {
        pipe_450.push_str("\n  |> inc");
    }
    pipe_450.push_str("\nprint(p)\n");
    let (pcode, pout, perr) = run_cli("w2_3_pipe_450", &pipe_450);
    assert_eq!(
        pcode, 0,
        "CLI 450-stage multiline pipe failed: stderr={perr}"
    );
    assert_eq!(pout.trim(), "450");

    // 2,000-stage multi-line `|>` chain: iterative parsing must never stack-overflow OS thread;
    // it cleanly reaches MAX_NESTING_DEPTH diagnostic with exit code 2.
    let mut pipe_2k = String::from("task inc(x: int) : int\n  return x + 1i\nend\nlet p = 0i");
    for _ in 0..2000 {
        pipe_2k.push_str("\n  |> inc");
    }
    pipe_2k.push_str("\nprint(p)\n");
    let (dcode, _dout, derr) = run_cli("w2_3_pipe_2000", &pipe_2k);
    assert_ne!(
        dcode, -1,
        "CLI crashed / aborted with stack overflow on 2000-level expr: {derr}"
    );
    assert_eq!(dcode, 2, "expected clean compile diagnostic exit=2: {derr}");
    assert!(
        derr.contains("nesting too deep"),
        "expected readable MAX_NESTING_DEPTH diagnostic, got: {derr}"
    );
}

#[test]
fn explicit_resume_typing_enforces_handler_and_argument_compatibility() {
    let valid = r#"
let ok = handle Eff {
  let x = perform Eff()
  x + 1i
} {
  resume(10i) + resume(20i)
}
ok
"#;
    let (_, w_ok) = ParserV3::compile(valid).expect("compile valid explicit resume");
    let errs_ok = check_program_witnesses(&w_ok);
    assert!(
        errs_ok.is_empty(),
        "valid explicit resume failed typeck: {errs_ok:?}"
    );

    // Mismatched handler return type: body produces Int, handler returns String.
    let bad_ret = r#"
handle Eff {
  let x = perform Eff()
  x + 1i
} {
  let _ = resume(10i)
  "not-an-int"
}
"#;
    let (_, w_bad_ret) = ParserV3::compile(bad_ret).expect("compile bad handler return");
    let errs_bad_ret = check_program_witnesses(&w_bad_ret);
    assert!(
        !errs_bad_ret.is_empty(),
        "handler returning String while body returns Int must fail typeck"
    );

    // Mismatched resume argument type: body uses `perform Eff()` as Int, handler passes String.
    let bad_arg = r#"
handle Eff {
  let x = perform Eff()
  x + 1i
} {
  resume("wrong-type")
}
"#;
    let (_, w_bad_arg) = ParserV3::compile(bad_arg).expect("compile bad resume arg");
    let errs_bad_arg = check_program_witnesses(&w_bad_arg);
    assert!(
        !errs_bad_arg.is_empty(),
        "passing String to `resume` when perform expects Int must fail typeck"
    );
}
