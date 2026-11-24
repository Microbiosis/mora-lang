//! P4-3 Integration & Parity Verification Suite:
//! - P4-3A: `DagCostModel` roofline analysis (`dag.cost_report()`: FLOPs, bytes read/written,
//!   arithmetic intensity, `compute-bound` vs `memory-bound` classification, fusion traffic
//!   savings) and DType-safe algebraic simplification (`Transpose(Transpose(x)) -> x`,
//!   `Reshape(Reshape(x)) -> Reshape`, `x ** 2.0 -> x * x`, `Neg(Neg(x)) -> x`, and
//!   strictly `DType::is_int()`-guarded `x + 0` / `x * 1` preserving IEEE-754 float semantics).
//! - P4-3B: Zero-dependency strict W^X (`RW -> RX`) native x86-64 SSE2/AVX2 machine-code kernel
//!   emitter for `FusedElementwise` chains, bit-exact parity against `simd_kernels`, kernel
//!   caching, and `MORA_DISABLE_NATIVE_JIT=1` kill-switch verification.

use std::process::Command;
use std::sync::Arc;

use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::test_support::TempDir;
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

fn eval_source(src: &str) -> Result<Value, String> {
    let witnesses = ParserV3::parse_witnesses(src).map_err(|e| format!("parse error: {e}"))?;
    let errors = check_program_witnesses(&witnesses);
    if !errors.is_empty() {
        return Err(errors
            .iter()
            .map(|e| format!("line {}: {}", e.line, e.message))
            .collect::<Vec<_>>()
            .join("\n"));
    }
    let (prog, _) = ParserV3::compile(src).map_err(|e| format!("compile error: {e}"))?;
    let mut rt = CoreRuntime::default();
    let (val, _) =
        execute_program(Arc::new(prog), &mut rt).map_err(|e| format!("runtime error: {e}"))?;
    Ok(val)
}

#[test]
fn p4_3a_algebraic_simplification_and_dtype_ieee754_safety() {
    let src = r#"
task layout_and_pow(x: Tensor<f64, [2, 3], cpu>): Tensor<f64, [2, 3], cpu>
  let t = x.transpose(0, 1).transpose(0, 1)
  let r = t.reshape([3, 2]).reshape([2, 3])
  let n = -(-r)
  return (n ** 2.0) + 1.0
end

let x = Tensor::from_list([[1.0, -2.0, 3.0], [-4.0, 5.0, -6.0]], "f64")
let raw = trace_dag(layout_and_pow, x)
let opt = raw.fuse()
let rep = opt.cost_report()

let y_raw = raw.run(x)
let y_opt = opt.run(x)
assert_eq(y_raw.to_list(), y_opt.to_list())
assert(rep["algebraic_rewrites"] >= 4)

task int_identities(a: Tensor<i32, [4], cpu>): Tensor<i32, [4], cpu>
  return ((a + 0i32) * 1i32) - 0i32
end

let ai = Tensor::from_list([10.0, 20.0, 30.0, 40.0], "i32")
let raw_i = trace_dag(int_identities, ai)
let opt_i = raw_i.fuse()
let rep_i = opt_i.cost_report()
assert(rep_i["algebraic_rewrites"] >= 3)
assert_eq(opt_i.run(ai).to_list(), ai.to_list())

task float_no_unsafe_identity(a: Tensor<f64, [2], cpu>): Tensor<f64, [2], cpu>
  return (a + 0.0) * 1.0
end

let af = Tensor::from_list([-0.0, 2.5], "f64")
let raw_f = trace_dag(float_no_unsafe_identity, af)
let opt_f = raw_f.fuse()
let rep_f = opt_f.cost_report()
assert_eq(rep_f["algebraic_rewrites"], 0)
let out_f = opt_f.run(af)

[rep["algebraic_rewrites"], rep_i["algebraic_rewrites"], rep_f["algebraic_rewrites"], out_f.to_list()]
"#;
    let val = eval_source(src).expect("algebraic simplification + DType guard must pass");
    let Value::List(items) = val else {
        panic!("expected list, got {val}");
    };
    assert_eq!(items.len(), 4);
    if let Value::Int(rw) = items[0] {
        assert!(
            rw >= 4,
            "expected >= 4 rewrites on layout_and_pow, got {rw}"
        );
    } else {
        panic!("expected int");
    }
    if let Value::Int(rwi) = items[1] {
        assert!(
            rwi >= 3,
            "expected >= 3 integer identity rewrites, got {rwi}"
        );
    } else {
        panic!("expected int");
    }
    assert_eq!(
        items[2],
        Value::Int(0),
        "f64 +0.0 / *1.0 must NOT be folded"
    );
    // Verify `-0.0 + 0.0` preserved IEEE-754 `+0.0` (positive zero sign bit)!
    if let Value::List(fs) = &items[3]
        && let Value::Float(z) = fs[0]
    {
        assert!(
            z == 0.0 && z.is_sign_positive(),
            "-0.0 + 0.0 must preserve IEEE-754 +0.0 sign bit, got {z} ({:#x})",
            z.to_bits()
        );
    } else {
        panic!("expected float list output");
    }
}

#[test]
fn p4_3a_dag_cost_model_roofline_flops_bandwidth_and_fusion_savings() {
    let src = r#"
task heavy_linear(x: Tensor<f32, [64, 64], cpu>, w: Tensor<f32, [64, 64], cpu>, b: Tensor<f32, [64], cpu>): Tensor<f32, [64, 64], cpu>
  return ((x @ w) + b).relu()
end

task mem_bound_ew(x: Tensor<f64, [256], cpu>): Tensor<f64, [256], cpu>
  return (((x * 2.0) + 1.0).relu() + 0.5).sqrt()
end

task flash_attn(q: Tensor<f32, [1, 16, 8], cpu>, k: Tensor<f32, [1, 16, 8], cpu>, v: Tensor<f32, [1, 16, 8], cpu>): Tensor<f32, [1, 16, 8], cpu>
  let scores = (q @ k.transpose(1, 2)) * 0.35355339f32
  return scores.softmax() @ v
end

let x64 = Tensor::ones([64, 64], "f32")
let w64 = Tensor::ones([64, 64], "f32")
let b64 = Tensor::zeros([64], "f32")
let d_lin = trace_dag(heavy_linear, x64, w64, b64).fuse()
let r_lin = d_lin.cost_report()

let x256 = Tensor::ones([256], "f64")
let d_ew = trace_dag(mem_bound_ew, x256).fuse()
let r_ew = d_ew.cost_report()

let q = Tensor::ones([1, 16, 8], "f32")
let k = Tensor::ones([1, 16, 8], "f32")
let v = Tensor::ones([1, 16, 8], "f32")
let d_fa = trace_dag(flash_attn, q, k, v).fuse()
let r_fa = d_fa.cost_report()

assert_eq(r_lin["bottleneck"], "compute-bound")
assert_eq(r_ew["bottleneck"], "memory-bound")
assert(r_lin["total_flops"] >= 524288)
assert(r_lin["fusion_bytes_saved"] > 0)
assert(r_ew["fusion_bytes_saved"] > 0)
assert(r_fa["fusion_bytes_saved"] >= 4096)
assert(r_ew["native_jit_eligible_nodes"] >= 1)
assert(len(r_lin["nodes"]) == d_lin.node_count())

[r_lin["bottleneck"], r_ew["bottleneck"]]
"#;
    let val = eval_source(src).expect("DagCostModel roofline report must pass");
    let Value::List(items) = val else {
        panic!("expected list, got {val}");
    };
    assert_eq!(items[0], Value::String("compute-bound".to_string()));
    assert_eq!(items[1], Value::String("memory-bound".to_string()));
}

#[test]
fn p4_3b_native_wx_jit_end_to_end_bit_exact_and_kill_switch() {
    let script = r#"
task fused_math(x: Tensor<f64, [131], cpu>, y: Tensor<f64, [131], cpu>): Tensor<f64, [131], cpu>
  let sq = x ** 2.0
  let comb = ((sq + y) * 0.5) - 1.25
  let r = comb.relu().abs() + 0.25
  return r.sqrt() / 2.0
end

let xs: list<float> = []
let ys: list<float> = []
let i = 0
while i < 131
  let f = float(i)
  xs = xs.push((f * 0.17) - 9.5)
  ys = ys.push((f * -0.11) + 4.2)
  i = i + 1
end
let tx = Tensor::from_list(xs, "f64")
let ty = Tensor::from_list(ys, "f64")

let raw = trace_dag(fused_math, tx, ty)
let dag = raw.fuse()
let y1 = dag.run(tx, ty)
let y2 = dag.run(tx, ty)
let y_raw = raw.run(tx, ty)
assert_eq(y1.to_list(), y2.to_list())
let diff = (y1 - y_raw).abs().sum()
assert(diff < 1.0e-12)
let rep = dag.cost_report()
print(rep["native_jit_isa"])
print(rep["native_jit_dispatches"])
print(y1.to_list())
"#;

    let dir = TempDir::new("p4_3_native_jit");
    let script_path = dir.join("jit_test.mora");
    std::fs::write(&script_path, script).expect("write test script");

    // 1. Default execution (native W^X x86-64 SSE2/AVX2 JIT enabled on x86_64).
    let out_jit = Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg("run")
        .arg(&script_path)
        .env_remove("MORA_DISABLE_NATIVE_JIT")
        .output()
        .expect("run mora with native JIT");
    assert!(
        out_jit.status.success(),
        "JIT run failed: stderr={}",
        String::from_utf8_lossy(&out_jit.stderr)
    );
    let jit_lines: Vec<String> = String::from_utf8_lossy(&out_jit.stdout)
        .lines()
        .map(|s| s.trim().to_string())
        .collect();
    assert_eq!(jit_lines.len(), 3, "unexpected stdout: {jit_lines:?}");

    #[cfg(target_arch = "x86_64")]
    {
        assert!(
            matches!(
                jit_lines[0].as_str(),
                "x86_64-avx2-fma" | "x86_64-avx2" | "x86_64-sse2"
            ),
            "expected native x86-64 JIT ISA, got {}",
            jit_lines[0]
        );
        let dispatches: u64 = jit_lines[1].parse().expect("parse dispatch count");
        assert!(
            dispatches >= 2,
            "expected >= 2 native W^X JIT dispatches across y1/y2, got {dispatches}"
        );
    }

    // 2. Kill-switch execution (`MORA_DISABLE_NATIVE_JIT=1`): must fall back cleanly to
    // `simd_kernels` and produce bit-identical output across ALL 131 elements!
    let out_off = Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg("run")
        .arg(&script_path)
        .env("MORA_DISABLE_NATIVE_JIT", "1")
        .output()
        .expect("run mora with native JIT disabled");
    assert!(
        out_off.status.success(),
        "kill-switch run failed: stderr={}",
        String::from_utf8_lossy(&out_off.stderr)
    );
    let off_lines: Vec<String> = String::from_utf8_lossy(&out_off.stdout)
        .lines()
        .map(|s| s.trim().to_string())
        .collect();
    assert_eq!(off_lines.len(), 3, "unexpected stdout: {off_lines:?}");
    #[cfg(target_arch = "x86_64")]
    {
        assert_eq!(off_lines[0], "disabled");
        assert_eq!(off_lines[1], "0");
    }
    assert_eq!(
        jit_lines[2], off_lines[2],
        "native W^X JIT vs simd_kernels must match bit-exactly across all 131 elements"
    );
}
