//! Batch 1 verification suite (G2 + G7-typeck):
//! - G2: Postfix `expr?` error/optional propagation (`Result<T, E>`, `Option<T>`, `T | nil`)
//! - G2: Explicit LIFO `defer` resource cleanup (normal return, early return, `?`, tail call, error unwind, loops)
//! - G2: Native `Layer` / parameter-state tracking on `StructInstance` (`parameters`, `named_parameters`,
//!   `state_dict`, `load_state_dict`, `to_device`, `@tag(param/buffer/transient)`)
//! - G7: Explicit function & trait effect row constraints (`! pure`, `! io`, `! io, async`, `! {io, ..r}`)
//! - G7: Linear arithmetic `DimExpr` (`+`, `-`, `*`, `/`) in tensor shape annotations & const generics

use std::sync::Arc;

use mora::mir::control::{OptLevel, optimize_program};
use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

fn typecheck_errors(src: &str) -> Vec<String> {
    let (_, witnesses) = ParserV3::compile(src).expect("source should parse");
    check_program_witnesses(&witnesses)
        .into_iter()
        .map(|e| format!("line {}: {}", e.line, e.message))
        .collect()
}

fn eval_checked(src: &str) -> Value {
    let (mut prog, witnesses) = ParserV3::compile(src).expect("source should compile");
    let errs = check_program_witnesses(&witnesses);
    assert!(
        errs.is_empty(),
        "unexpected type errors:\n{}",
        errs.iter()
            .map(|e| format!("  line {}: {}", e.line, e.message))
            .collect::<Vec<_>>()
            .join("\n")
    );
    optimize_program(&mut prog, OptLevel::Basic);
    let mut core = CoreRuntime::default();
    let (val, _) = execute_program(Arc::new(prog), &mut core).expect("program should run");
    val
}

fn eval_checked_expect_runtime_err(src: &str) -> String {
    let (mut prog, witnesses) = ParserV3::compile(src).expect("source should compile");
    let errs = check_program_witnesses(&witnesses);
    assert!(
        errs.is_empty(),
        "unexpected type errors:\n{}",
        errs.iter()
            .map(|e| format!("  line {}: {}", e.line, e.message))
            .collect::<Vec<_>>()
            .join("\n")
    );
    optimize_program(&mut prog, OptLevel::Basic);
    let mut core = CoreRuntime::default();
    let err = execute_program(Arc::new(prog), &mut core).expect_err("expected runtime error");
    err.to_string()
}

// ═════════════════════════════════════════════════════════════════════════════
// 1. G2: Postfix `expr?` Error & Optional Propagation
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn g2_try_result_option_and_nullable_runtime() {
    let src = r#"
enum Res<T, E>
  Ok(T)
  Err(E)
end

enum Opt<T>
  Some(T)
  None
end

task safe_div(a: int, b: int): Res<int, string>
  if b == 0
    return Res::Err("div_by_zero")
  end
  return Res::Ok(a / b)
end

task compute_res(a: int, b: int, c: int): Res<int, string>
  let x = safe_div(a, b)?
  let y = safe_div(x, c)?
  return Res::Ok(y + 1)
end

task find_pos(x: int): Opt<int>
  if x > 0
    return Opt::Some(x * 2)
  end
  return Opt::None
end

task chain_opt(a: int, b: int): Opt<int>
  let p = find_pos(a)?
  let q = find_pos(b)?
  return Opt::Some(p + q)
end

task maybe_inc(x: int): int | nil
  if x < 0
    return nil
  end
  return x + 1
end

task chain_nullable(a: int, b: int): int | nil
  let u = maybe_inc(a)?
  let v = maybe_inc(b)?
  return u * v
end

let r_ok = match compute_res(100, 5, 4) with
  Res::Ok(v) -> v
  Res::Err(_) -> -1
end
let r_err = match compute_res(100, 0, 4) with
  Res::Ok(_) -> ""
  Res::Err(e) -> e
end
let o_some = match chain_opt(3, 4) with
  Opt::Some(v) -> v
  Opt::None -> -1
end
let o_none = match chain_opt(3, -1) with
  Opt::Some(v) -> v
  Opt::None -> -99
end
let n_ok = chain_nullable(2, 3)
let n_nil = chain_nullable(-5, 3)

[str(r_ok), r_err, str(o_some), str(o_none), str(n_ok), str(n_nil)]
"#;
    let val = eval_checked(src);
    assert_eq!(
        val,
        Value::List(
            vec![
                Value::String("6".to_string()),
                Value::String("div_by_zero".to_string()),
                Value::String("14".to_string()),
                Value::String("-99".to_string()),
                Value::String("12".to_string()),
                Value::String("nil".to_string()),
            ]
            .into()
        )
    );
}

#[test]
fn g2_try_static_type_rejections() {
    // 1. `?` applied to plain `int`
    let errs = typecheck_errors(
        r#"
task bad(): int | nil
  let x = 42?
  return x
end
"#,
    );
    assert!(
        !errs.is_empty(),
        "expected static error applying `?` to plain int"
    );

    // 2. `?` on Result inside function returning non-Result `int`
    let errs = typecheck_errors(
        r#"
enum Res<T, E>
  Ok(T)
  Err(E)
end
task f(): Res<int, string>
  return Res::Ok(1)
end
task bad(): int
  let v = f()?
  return v
end
"#,
    );
    assert!(
        !errs.is_empty(),
        "expected static error using `?` on Result in task returning int"
    );

    // 3. `?` with mismatched Err payload type (`int` vs `string`)
    let errs = typecheck_errors(
        r#"
enum Res<T, E>
  Ok(T)
  Err(E)
end
task f(): Res<int, int>
  return Res::Err(404)
end
task bad(): Res<int, string>
  let v = f()?
  return Res::Ok(v)
end
"#,
    );
    assert!(
        !errs.is_empty(),
        "expected static error on mismatched Err payload type"
    );

    // 4. `?` on `int | nil` inside function returning non-nullable `int`
    let errs = typecheck_errors(
        r#"
task f(): int | nil
  return nil
end
task bad(): int
  let v = f()?
  return v
end
"#,
    );
    assert!(
        !errs.is_empty(),
        "expected static error using `?` on `int | nil` in task returning non-nullable int"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// 2. G2: Explicit LIFO `defer` Resource Cleanup
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn g2_defer_lifo_normal_early_try_tailcall_and_loop() {
    let src = r#"
enum Res<T, E>
  Ok(T)
  Err(E)
end

let trace = atom([])

task log(tag: string)
  swap(trace, fn(xs) => xs.push(tag))
end

task normal_fn(): int
  defer log("n1")
  defer do
    log("n2a")
    log("n2b")
  end
  defer { log("n3") }
  log("body")
  return 10
end

task early_return(flag: bool): int
  defer log("e1")
  if flag
    defer log("e_branch")
    return 7
  end
  defer log("e_never")
  return 0
end

task tail_helper(): int
  log("tail_helper")
  return 99
end

task with_tail(): int
  defer log("after_tail")
  tail_helper()
end

task loop_defer(): nil
  for i in [0, 1, 2]
    defer log("L" + str(i))
  end
end

task fail_opt(): Res<int, string>
  return Res::Err("boom")
end

task try_with_defer(): Res<int, string>
  defer log("try_cleanup")
  let x = fail_opt()?
  defer log("try_unreachable")
  return Res::Ok(x)
end

let a = normal_fn()
let b = early_return(true)
let c = with_tail()
loop_defer()
let _ = try_with_defer()
[a, b, c, deref(trace)]
"#;
    let val = eval_checked(src);
    let expected_trace: Vec<Value> = [
        "body",
        "n3",
        "n2a",
        "n2b",
        "n1",
        "e_branch",
        "e1",
        "tail_helper",
        "after_tail",
        "L2",
        "L1",
        "L0",
        "try_cleanup",
    ]
    .into_iter()
    .map(|s| Value::String(s.to_string()))
    .collect();
    assert_eq!(
        val,
        Value::List(
            vec![
                Value::Int(10),
                Value::Int(7),
                Value::Int(99),
                Value::List(expected_trace.into()),
            ]
            .into()
        )
    );
}

#[test]
fn g2_defer_runs_lifo_on_runtime_error_unwind() {
    // Spawn a fiber that fails after registering LIFO defers across two frames; the parent
    // receives all cleanup messages on the channel in exact LIFO order across unwind!
    let runnable = r#"
let ch = channel(8)

task inner(): int
  defer ch.send("inner_first_reg")
  defer ch.send("inner_second_reg")
  return 1 / 0
end

task outer(): int
  defer ch.send("outer_cleanup")
  return inner()
end

let h = spawn outer()
let m1 = ch.recv()
let m2 = ch.recv()
let m3 = ch.recv()
[m1, m2, m3]
"#;
    let val = eval_checked(runnable);
    assert_eq!(
        val,
        Value::List(
            vec![
                Value::String("inner_second_reg".to_string()),
                Value::String("inner_first_reg".to_string()),
                Value::String("outer_cleanup".to_string()),
            ]
            .into()
        )
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// 3. G7: Explicit Function & Trait Effect Row Constraints (`! eff`)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn g7_explicit_effect_row_constraints_pass_and_reject() {
    // Valid pure, io, io+async, sub-effecting, open row, and trait contract
    let ok_src = r#"
task pure_add(a: int, b: int): int ! pure
  return a + b
end

task sub_effect_io(x: int): int ! io
  return pure_add(x, 1)
end

task do_io_and_async(): int ! io, async
  sleep(1)
  return 7
end

task open_effects(): int ! {io, ..r}
  sleep(1)
  return 9
end

trait PureMath
  task eval(self, x: int): int ! pure
end

struct Scaler
  k: int
end

impl PureMath for Scaler
  task eval(self, x: int): int ! pure
    return self.k * x
  end
end

let s = Scaler(k=3)
[pure_add(2, 3), sub_effect_io(4), do_io_and_async(), open_effects(), s.eval(5)]
"#;
    let val = eval_checked(ok_src);
    assert_eq!(
        val,
        Value::List(
            vec![
                Value::Int(5),
                Value::Int(5),
                Value::Int(7),
                Value::Int(9),
                Value::Int(15),
            ]
            .into()
        )
    );

    // Reject `! pure` performing `io` (`file.exists`)
    let errs = typecheck_errors(
        r#"
task illegal_io(): bool ! pure
  return file.exists("Cargo.toml")
end
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains("Effect row mismatch")),
        "expected EffectRowMismatch for `! pure` doing io, got: {errs:?}"
    );

    // Reject `! pure` performing `async` (`sleep`)
    let errs = typecheck_errors(
        r#"
task illegal_async(): nil ! pure
  sleep(1)
end
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains("Effect row mismatch")),
        "expected EffectRowMismatch for `! pure` doing async, got: {errs:?}"
    );

    // Reject `! pure` performing `io` inside `defer`
    let errs = typecheck_errors(
        r#"
task illegal_defer_io(): int ! pure
  defer file.exists("Cargo.toml")
  return 1
end
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains("Effect row mismatch")),
        "expected EffectRowMismatch for `! pure` with io inside defer, got: {errs:?}"
    );

    // Reject `! io` performing `async`
    let errs = typecheck_errors(
        r#"
task io_only_but_async(): nil ! io
  sleep(1)
end
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains("Effect row mismatch")),
        "expected EffectRowMismatch for `! io` doing async, got: {errs:?}"
    );

    // Reject `impl PureMath` violating trait's `! pure` contract
    let errs = typecheck_errors(
        r#"
trait PureMath
  task eval(self, x: int): int ! pure
end
struct BadScaler
  k: int
end
impl PureMath for BadScaler
  task eval(self, x: int): int
    let _ = file.exists("Cargo.toml")
    return self.k * x
  end
end
"#,
    );
    assert!(
        errs.iter().any(|e| e.contains("Effect row mismatch")),
        "expected EffectRowMismatch when impl violates trait `! pure` contract, got: {errs:?}"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// 4. G7: Linear Arithmetic Dimension Expressions (`DimExpr`)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn g7_dim_expr_linear_arithmetic_typeck_and_const_generics() {
    // Valid arithmetic dims: const generics + matmul + linear unification
    let ok_src = r#"
struct FFN<const D: int>
  up: Tensor<f32, [D, D * 4], cpu>
  down: Tensor<f32, [D * 4, D], cpu>
end

task run_ffn(x: Tensor<f32, [2, 4], cpu>, m: FFN<4>): Tensor<f32, [2, 4], cpu>
  let h = x @ m.up
  return h @ m.down
end

task symbolic_proj<B, S, D>(x: Tensor<f32, [B, S + 1, D], cpu>, w: Tensor<f32, [D, D * 2], cpu>): Tensor<f32, [B, S + 1, D * 2], cpu>
  return x @ w
end

let up = Tensor::ones([4, 16], "f32", "cpu")
let down = Tensor::ones([16, 4], "f32", "cpu")
let m = FFN<4>(up=up, down=down)
let x = Tensor::ones([2, 4], "f32", "cpu")
let y = run_ffn(x, m)

let sx = Tensor::ones([2, 5, 4], "f32", "cpu")
let sw = Tensor::ones([4, 8], "f32", "cpu")
let sy = symbolic_proj(sx, sw)
[y.shape(), sy.shape()]
"#;
    let val = eval_checked(ok_src);
    assert_eq!(
        val,
        Value::List(
            vec![
                Value::List(vec![Value::Int(2), Value::Int(4)].into()),
                Value::List(vec![Value::Int(2), Value::Int(5), Value::Int(8)].into()),
            ]
            .into()
        )
    );

    // Reject incompatible arithmetic dimension: D = 4 => D * 4 = 16, passing [4, 12] must fail!
    let errs = typecheck_errors(
        r#"
struct FFN<const D: int>
  up: Tensor<f32, [D, D * 4], cpu>
end
let bad = FFN<4>(up=Tensor::ones([4, 12], "f32", "cpu"))
"#,
    );
    assert!(
        !errs.is_empty(),
        "expected static shape mismatch for D*4 (16) vs 12"
    );

    // Reject linear solver contradiction: [B + 1, 4] unified with [5, 4] => B=4, so [B * 2, 4] is [8, 4] != [10, 4]
    let errs = typecheck_errors(
        r#"
task check_pair<B>(a: Tensor<f32, [B + 1, 4], cpu>, b: Tensor<f32, [B * 2, 4], cpu>): int
  return 1
end
let _ = check_pair(Tensor::ones([5, 4], "f32", "cpu"), Tensor::ones([10, 4], "f32", "cpu"))
"#,
    );
    assert!(
        !errs.is_empty(),
        "expected static linear dim solver contradiction (B+1=5 => B=4 => B*2=8 != 10)"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// 5. G2: Native `Layer` / Parameter-State Tracking on `StructInstance`
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn g2_native_layer_parameter_state_tracking() {
    let src = r#"
struct Linear
  weight: Tensor<f32, [2, 2], cpu>
  bias: Tensor<f32, [2], cpu>
  running_scale: Tensor<f32, [2], cpu> @tag(buffer="true")
  scratch: Tensor<f32, [2], cpu> @tag(transient="true")
end

struct MLP
  fc1: Linear
  blocks: list<Linear>
  hidden_dim: int
end

let l1 = Linear(weight=Tensor::ones([2, 2], "f32", "cpu"), bias=Tensor::zeros([2], "f32", "cpu"), running_scale=Tensor::ones([2], "f32", "cpu"), scratch=Tensor::zeros([2], "f32", "cpu"))
let l2 = Linear(weight=Tensor::full([2, 2], 2.0, "f32", "cpu"), bias=Tensor::full([2], 0.5, "f32", "cpu"), running_scale=Tensor::full([2], 3.0, "f32", "cpu"), scratch=Tensor::zeros([2], "f32", "cpu"))
let model = MLP(fc1=l1, blocks=[l2], hidden_dim=2)

let params = model.parameters()
let nparams = model.named_parameters()
let prefixed = model.named_parameters("enc")
let sd = model.state_dict()

-- Mutate a new state_dict and functionally load it back
let new_sd = {"fc1.weight": Tensor::full([2, 2], 9.0, "f32", "cpu"), "fc1.bias": Tensor::full([2], 1.5, "f32", "cpu"), "fc1.running_scale": Tensor::full([2], 4.0, "f32", "cpu"), "blocks.0.weight": Tensor::full([2, 2], 8.0, "f32", "cpu"), "blocks.0.bias": Tensor::full([2], 2.5, "f32", "cpu"), "blocks.0.running_scale": Tensor::full([2], 5.0, "f32", "cpu")}
let loaded = model.load_state_dict(new_sd)

-- Partial non-strict update
let partial = loaded.load_state_dict({"fc1.weight": Tensor::full([2, 2], 7.0, "f32", "cpu")}, strict=false)

-- Recursive device transfer
let on_cpu = partial.to_device("cpu")

[str(len(params)), str(len(nparams)), str(len(sd)), str(prefixed.get("enc.fc1.weight") != nil), str(nparams.get("fc1.running_scale") != nil), str(sd.get("fc1.running_scale") != nil), str(sd.get("fc1.scratch") != nil), str(int(model.fc1.weight.sum())), str(int(loaded.fc1.weight.sum())), str(int(loaded.fc1.running_scale.sum())), str(int(partial.fc1.weight.sum())), str(int(on_cpu.blocks[0].weight.sum()))]
"#;
    let val = eval_checked(src);
    let expected: Vec<Value> = [
        "4", "4", "6", "true", "false", "true", "false", "4", "36", "8", "28", "32",
    ]
    .into_iter()
    .map(|s| Value::String(s.to_string()))
    .collect();
    assert_eq!(val, Value::List(expected.into()));
}

#[test]
fn g2_layer_load_state_dict_errors_and_tag_validation() {
    // Strict mismatch (missing key)
    let err = eval_checked_expect_runtime_err(
        r#"
struct Lin
  w: Tensor<f32, [2, 2], cpu>
  b: Tensor<f32, [2], cpu>
end
let m = Lin(w=Tensor::ones([2, 2], "f32", "cpu"), b=Tensor::zeros([2], "f32", "cpu"))
m.load_state_dict({"w": Tensor::ones([2, 2], "f32", "cpu")})
"#,
    );
    assert!(
        err.contains("missing keys"),
        "expected missing keys diagnostic, got: {err}"
    );

    // Shape mismatch
    let err = eval_checked_expect_runtime_err(
        r#"
struct Lin
  w: Tensor<f32, [2, 2], cpu>
end
let m = Lin(w=Tensor::ones([2, 2], "f32", "cpu"))
m.load_state_dict({"w": Tensor::ones([3, 3], "f32", "cpu")})
"#,
    );
    assert!(
        err.contains("shape mismatch"),
        "expected shape mismatch diagnostic, got: {err}"
    );

    // Static rejection of invalid `@tag(param="yes")`
    let errs = typecheck_errors(
        r#"
struct BadTag
  w: Tensor<f32, [2], cpu> @tag(param="yes")
end
"#,
    );
    assert!(
        !errs.is_empty(),
        "expected static error on non-boolean @tag(param=\"yes\")"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// 6. MORABC02 AOT + O2 Optimization + Formatter Idempotence
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn batch1_morabc02_o2_roundtrip_and_formatter_idempotence() {
    let src = r#"enum Res<T, E>
  Ok(T)
  Err(E)
end

struct Layer
  w: Tensor<f32, [2, 2], cpu>
  buf: Tensor<f32, [2], cpu> @tag(buffer="true")
end

task check_layer(m: Layer): Res<int, string> ! pure
  let log = atom([])
  defer do
    swap(log, fn(xs) => xs.push(1))
  end
  let ok_v = Res::Ok(len(m.parameters()))?
  return Res::Ok(ok_v + len(m.state_dict()))
end

let m = Layer(w=Tensor::ones([2, 2], "f32", "cpu"), buf=Tensor::zeros([2], "f32", "cpu"))
let res = check_layer(m)
let ans = match res with
  Res::Ok(n) -> n
  Res::Err(_) -> -1
end
ans
"#;
    let (mut prog, witnesses) = ParserV3::compile(src).expect("compile");
    let errs = check_program_witnesses(&witnesses);
    assert!(errs.is_empty(), "type errors: {errs:?}");
    optimize_program(&mut prog, OptLevel::Aggressive);
    let mut core = CoreRuntime::default();
    let (val, _) = execute_program(Arc::new(prog), &mut core).expect("execute O2");
    assert_eq!(val, Value::Int(3));

    // Verify end-to-end MORABC02 `.morac` AOT build & zero-source execution!
    let tmp = mora::test_support::TempDir::new("batch1_morac");
    let src_path = tmp.path().join("batch1.mora");
    let bc_path = tmp.path().join("batch1.morac");
    let cli_src = format!("{}\nprint(ans)\n", src.trim_end());
    std::fs::write(&src_path, &cli_src).expect("write src");
    let build_out = std::process::Command::new(env!("CARGO_BIN_EXE_mora"))
        .args(["build", "--emit-bc", "-o"])
        .arg(&bc_path)
        .arg(&src_path)
        .output()
        .expect("run mora build --emit-bc");
    assert!(
        build_out.status.success(),
        "mora build --emit-bc failed: {}",
        String::from_utf8_lossy(&build_out.stderr)
    );
    let run_out = std::process::Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg(&bc_path)
        .output()
        .expect("run .morac");
    assert!(
        run_out.status.success(),
        "running .morac failed: {}",
        String::from_utf8_lossy(&run_out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run_out.stdout).contains('3'),
        "expected '3' in .morac stdout, got: {}",
        String::from_utf8_lossy(&run_out.stdout)
    );

    // Verify `mora fmt -` idempotence on `?`, `defer do ... end`, `! pure`, and `@tag(...)`.
    let format_via_cli = |input: &str| -> String {
        use std::io::Write;
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_mora"))
            .args(["fmt", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn mora fmt -");
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(input.as_bytes())
            .expect("write stdin");
        let out = child.wait_with_output().expect("wait mora fmt -");
        assert!(
            out.status.success(),
            "mora fmt - failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("utf8 stdout")
    };
    let fmt1 = format_via_cli(src);
    let fmt2 = format_via_cli(&fmt1);
    assert_eq!(fmt1, fmt2, "mora fmt must be strictly idempotent");

    // Verify generic return types followed by `! eff` keep exact spacing (`task_handle<string> ! io, async`).
    let generic_eff = "export task read_async(p: string): task_handle<string> ! io, async\n  return spawn fn() file.read_text(p) end\nend\n";
    assert_eq!(
        format_via_cli(generic_eff),
        generic_eff,
        "generic return type + ! eff must preserve `<T>` and `! io` spacing"
    );
}
