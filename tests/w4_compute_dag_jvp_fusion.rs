//! Wave 4 Step 1 verification:
//! - Native `TensorComputeDag` tracing (`trace_dag`), Graphviz DOT export (`to_dot`),
//!   operator fusion (`FusedLinearAct` + `FusedElementwise`), and `DeviceBackend` execution (`run`).
//! - Forward-mode JVP differentiation (`jvp`) verified against numerical finite differences
//!   across scalar floats, multi-dim tensors, `@` matmul, activations, normalizations, and slices.
//! - Static HM checks rejecting `tensor_mutate` and primal/tangent mismatches.

use std::sync::Arc;

use mora::common::Device;
use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::compute_dag::backend_for_device;
use mora::runtime::vm::execute_program;
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

fn compile_check_and_run(src: &str) -> Value {
    let (prog, witnesses) =
        ParserV3::compile(src).unwrap_or_else(|e| panic!("compile failed:\n{src}\nerr: {e}"));
    let errs = check_program_witnesses(&witnesses);
    assert!(
        errs.is_empty(),
        "unexpected type errors for:\n{src}\ngot: {errs:?}"
    );
    let mut core = CoreRuntime::default();
    let (val, _) = execute_program(Arc::new(prog), &mut core)
        .unwrap_or_else(|e| panic!("runtime error for:\n{src}\nerr: {e}"));
    val
}

fn assert_typecheck_fails(src: &str, needle: &str) {
    let (_prog, witnesses) =
        ParserV3::compile(src).unwrap_or_else(|_e| panic!("compile failed:\n{src}"));
    let errs = check_program_witnesses(&witnesses);
    assert!(
        !errs.is_empty(),
        "expected static type error containing {needle:?} for:\n{src}\nbut typeck succeeded"
    );
    let msg = errs
        .iter()
        .map(mora::typeck::format_error)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        msg.to_lowercase().contains(&needle.to_lowercase()),
        "expected {needle:?}, got:\n{msg}"
    );
}

#[test]
fn w4_compute_dag_trace_fuse_linear_act_and_elementwise_chains() {
    let src = r#"
let w = tensor([[1.0, -0.5], [0.25, 0.75]])
let b = tensor([[0.1, -0.2]])

task mlp_block(x: Tensor<f64, [1, 2], cpu>): Tensor<f64, [1, 2], cpu>
  let h = (x @ w + b).silu()
  return (h * 2.0 + 0.5).tanh()
end

let x0 = tensor([[1.0, 1.0]])
let dag: compute_dag = trace_dag(mlp_block, x0)
let fused: compute_dag = dag.fuse()

let x_test = tensor([[1.5, -2.0]])
let eager_out = mlp_block(x_test)
let unfused_out = dag.run(x_test)
let fused_out = fused.run(x_test)

let diff_fused = (eager_out - fused_out).abs().sum()
let diff_unfused = (eager_out - unfused_out).abs().sum()

[str(dag.is_fused()), str(fused.is_fused()), str(dag.node_count()), str(fused.node_count()), str(fused.fused_count()), str(fused.op_names()), fused.to_dot(), str(diff_fused < 1e-12), str(diff_unfused < 1e-12)]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("false".to_string()));
    assert_eq!(v[1], Value::String("true".to_string()));
    let orig_nodes: usize = v[2].to_string().parse().expect("int");
    let fused_nodes: usize = v[3].to_string().parse().expect("int");
    let fused_ops: usize = v[4].to_string().parse().expect("int");
    assert!(
        fused_nodes < orig_nodes,
        "fusion must shrink node count: {orig_nodes} -> {fused_nodes}"
    );
    assert!(
        fused_ops >= 2,
        "expected both FusedLinearAct and FusedElementwise (got {fused_ops}: {})",
        v[5]
    );
    assert!(v[5].to_string().contains("FusedLinearAct"));
    assert!(v[5].to_string().contains("FusedElementwise"));
    assert!(v[6].to_string().contains("digraph TensorComputeDag"));
    assert_eq!(v[7], Value::String("true".to_string()));
    assert_eq!(v[8], Value::String("true".to_string()));
}

#[test]
fn w4_device_backend_registry_and_multi_device_dag() {
    let cpu = backend_for_device(&Device::Cpu);
    assert_eq!(cpu.name(), "cpu");
    assert!(cpu.is_available());
    let cuda = backend_for_device(&Device::Cuda(0));
    assert!(cuda.name().contains("cuda"));
    assert!(cuda.is_available());

    let src = r#"
let w = tensor([[1.0, 2.0], [3.0, 4.0]], device="cuda:0")
let dag = trace_dag(fn(x: Tensor<f64, [1, 2], cuda:0>) (x @ w).relu() end, tensor([[1.0, -1.0]], device="cuda:0")).fuse()
let y = dag.run(tensor([[2.0, 1.0]], device="cuda:0"))
[y.device(), str(y.to_list()), str(dag.fused_count())]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("cuda:0".to_string()));
    assert_eq!(v[1], Value::String("[[5.0, 8.0]]".to_string()));
    assert_eq!(v[2], Value::String("1".to_string()));
}

#[test]
fn w4_forward_mode_jvp_and_static_checks() {
    let src = r#"
-- 1. Scalar JVP: f(x) = x^3 + 2*x at x=2, dx=1 => primal=12, tangent=14
let s_res = jvp(fn(x: float) (x ** 3.0) + 2.0 * x end, 2.0, 1.0)

-- 2. Non-scalar Tensor JVP: f(x, w) = (x @ w).gelu() + layer_norm
let w = tensor([[0.5, -0.25], [1.0, 0.75]])
let x = tensor([[1.2, -0.8]])
let dx = tensor([[0.3, -0.4]])
let dw = tensor([[0.1, 0.2], [-0.15, 0.05]])

task layer_fn(xi: Tensor<f64, [1, 2], cpu>, wi: Tensor<f64, [2, 2], cpu>): Tensor<f64, [1, 2], cpu>
  return (xi @ wi).gelu().layer_norm()
end

let t_res = jvp(layer_fn, [x, w], [dx, dw])
let primal = t_res[0]
let jvp_tan = t_res[1]

let eps = 1e-6
let fd = (layer_fn(x + dx * eps, w + dw * eps) - layer_fn(x - dx * eps, w - dw * eps)) / (2.0 * eps)
let max_err = (jvp_tan - fd).abs().max()

[str(s_res[0]), str(s_res[1]), str(primal.shape()), str(max_err < 1e-5)]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("12.0".to_string()));
    assert_eq!(v[1], Value::String("14.0".to_string()));
    assert_eq!(v[2], Value::String("[1, 2]".to_string()));
    assert_eq!(v[3], Value::String("true".to_string()));

    // Static rejection: in-place mutation inside jvp/trace_dag and mismatched primal/tangent types
    assert_typecheck_fails(
        "task bad(x: Tensor<f64, [2], cpu>): Tensor<f64, [2], cpu>\n  x.fill_(1.0)\n  return x\nend\njvp(bad, Tensor::zeros([2]), Tensor::ones([2]))\n",
        "tensor_mutate",
    );
    assert_typecheck_fails(
        "task bad(x: Tensor<f64, [2], cpu>): Tensor<f64, [2], cpu>\n  x.fill_(1.0)\n  return x\nend\ntrace_dag(bad, Tensor::zeros([2]))\n",
        "tensor_mutate",
    );
    assert_typecheck_fails(
        "jvp(fn(x: Tensor<f64, [2], cpu>) x end, Tensor::zeros([2]), Tensor::ones([3]))\n",
        "mismatch",
    );
}

#[test]
fn w4_p1_fused_rms_norm_linear_2d_and_3d_with_gamma_bias_act() {
    let src = r#"
let w = tensor([[0.5, -0.3, 0.2, 0.7], [-0.4, 0.6, 0.1, -0.2], [0.3, 0.2, -0.5, 0.4], [0.8, -0.1, 0.4, 0.3]])
let gamma = tensor([1.2, 0.8, 1.5, 0.9])
let bias = tensor([0.1, -0.2, 0.05, 0.3])

-- 2D Llama-style pre-norm linear with affine gamma, bias, and SiLU
task prenorm_2d(x: Tensor<f64, [2, 4], cpu>): Tensor<f64, [2, 4], cpu>
  return ((x.rms_norm(1e-5) * gamma) @ w + bias).silu()
end

-- 3D batched transformer projection [B=2, S=2, K=4] -> [2, 2, 4] (bare RMSNorm + Linear)
task prenorm_3d(x: Tensor<f64, [2, 2, 4], cpu>): Tensor<f64, [2, 2, 4], cpu>
  return (x.rms_norm(1e-5) @ w + bias).gelu()
end

let x2_init = tensor([[1.0, 2.0, -1.0, 0.5], [-0.5, 0.25, 1.5, -1.2]])
let dag2 = trace_dag(prenorm_2d, x2_init)
let fused2 = dag2.compile()
let x2_eval = tensor([[0.7, -1.3, 2.1, 0.4], [1.9, 0.2, -0.8, 1.1]])
let err2 = (prenorm_2d(x2_eval) - fused2.run(x2_eval)).abs().max()

let x3_init = tensor([[[1.0, -1.0, 0.5, 2.0], [0.3, 0.7, -0.2, 1.1]], [[-0.6, 1.4, 0.9, -0.3], [0.2, -0.5, 1.2, 0.8]]])
let dag3 = trace_dag(prenorm_3d, x3_init)
let fused3 = dag3.compile()
let x3_eval = x3_init * 1.35 + 0.2
let err3 = (prenorm_3d(x3_eval) - fused3.run(x3_eval)).abs().max()

[str(fused2.op_names()), str(fused3.op_names()), str(fused2.node_count() < dag2.node_count()), str(fused3.node_count() < dag3.node_count()), str(err2 < 1e-11), str(err3 < 1e-11), fused2.to_dot()]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert!(
        v[0].to_string().contains("FusedRmsNormLinear"),
        "expected FusedRmsNormLinear in 2D fused ops, got {}",
        v[0]
    );
    assert!(
        v[1].to_string().contains("FusedRmsNormLinear"),
        "expected FusedRmsNormLinear in 3D fused ops, got {}",
        v[1]
    );
    assert_eq!(v[2], Value::String("true".to_string()));
    assert_eq!(v[3], Value::String("true".to_string()));
    assert_eq!(v[4], Value::String("true".to_string()));
    assert_eq!(v[5], Value::String("true".to_string()));
    assert!(v[6].to_string().contains("FusedRmsNormLinear"));
}

#[test]
fn w4_p1_fused_flash_attention_tiled_multihead_gqa_and_causal_mask() {
    let src = r#"
-- 1. Multi-tile 4D Multi-Head Attention [B=1, H=2, Sq=40, D=8] (40 > Br=32 spans 2 Q & KV tiles!)
--    with negative-axis k.transpose(-2, -1) and * scale
task mha(q: Tensor<f64, [1, 2, 40, 8], cpu>, k: Tensor<f64, [1, 2, 40, 8], cpu>, v: Tensor<f64, [1, 2, 40, 8], cpu>): Tensor<f64, [1, 2, 40, 8], cpu>
  let scale = 1.0 / math.sqrt(8.0)
  return ((q @ k.transpose(-2, -1)) * scale).softmax() @ v
end

let q0 = Tensor::arange(0.0, 640.0, 1.0).reshape([1, 2, 40, 8]) * 0.01 - 0.5
let k0 = Tensor::arange(0.0, 640.0, 1.0).reshape([1, 2, 40, 8]) * 0.008 - 0.3
let v0 = Tensor::arange(0.0, 640.0, 1.0).reshape([1, 2, 40, 8]) * 0.005 + 0.1

let mha_dag = trace_dag(mha, [q0, k0, v0])
let mha_fused = mha_dag.compile()

let q1 = math.sin(q0 * 1.1 + 0.2)
let k1 = math.cos(k0 * 0.9 - 0.1)
let v1 = v0 * 0.7 - 0.2
let mha_err = (mha(q1, k1, v1) - mha_fused.run([q1, k1, v1])).abs().max()

-- 2. MQA/GQA batch broadcasting (Q has 4 heads, K/V have 1 shared head) + Dv != Dk + causal mask + / denom
let causal = tensor([[0.0, -1e9, -1e9, -1e9], [0.0, 0.0, -1e9, -1e9], [0.0, 0.0, 0.0, -1e9], [0.0, 0.0, 0.0, 0.0]])

task masked_mqa(q: Tensor<f64, [1, 4, 4, 6], cpu>, k: Tensor<f64, [1, 1, 4, 6], cpu>, v: Tensor<f64, [1, 1, 4, 3], cpu>): Tensor<f64, [1, 4, 4, 3], cpu>
  let logits = ((q @ k.transpose(-2, -1)) / math.sqrt(6.0)) + causal
  return logits.softmax() @ v
end

let mq = Tensor::arange(0.0, 96.0, 1.0).reshape([1, 4, 4, 6]) * 0.05 - 1.0
let mk = Tensor::arange(0.0, 24.0, 1.0).reshape([1, 1, 4, 6]) * 0.07 - 0.4
let mv = Tensor::arange(0.0, 12.0, 1.0).reshape([1, 1, 4, 3]) * 0.1 + 0.2

let mqa_dag = trace_dag(masked_mqa, mq, mk, mv)
let mqa_fused = mqa_dag.compile()
let mqa_err = (masked_mqa(mq * 0.8, mk + 0.3, mv * 1.2) - mqa_fused.run(mq * 0.8, mk + 0.3, mv * 1.2)).abs().max()

[str(mha_fused.op_names()), str(mqa_fused.op_names()), str(mha_fused.node_count() < mha_dag.node_count()), str(mha_err < 1e-11), str(mqa_err < 1e-11)]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    let mha_ops = v[0].to_string();
    let mqa_ops = v[1].to_string();
    assert!(
        mha_ops.contains("FusedFlashAttention"),
        "expected FusedFlashAttention in MHA ops, got {mha_ops}"
    );
    assert!(
        !mha_ops.contains("Transpose") && !mha_ops.contains("Softmax"),
        "Transpose and Softmax should be absorbed into FusedFlashAttention, got {mha_ops}"
    );
    assert!(
        mqa_ops.contains("FusedFlashAttention"),
        "expected FusedFlashAttention in masked MQA ops, got {mqa_ops}"
    );
    assert!(
        !mqa_ops.contains("Transpose") && !mqa_ops.contains("Softmax"),
        "Transpose and Softmax should be absorbed in masked MQA, got {mqa_ops}"
    );
    assert_eq!(v[2], Value::String("true".to_string()));
    assert_eq!(v[3], Value::String("true".to_string()));
    assert_eq!(v[4], Value::String("true".to_string()));
}

#[test]
fn w4_p1_multithreaded_simd_cpu_backend_and_concurrent_spawn_dag() {
    let src = r#"
-- Large matrix (96x64 @ 64x80 = 491,520 FLOPs >> PARALLEL_FLOP_THRESHOLD=32,768)
-- exercises multi-threaded tiled cache-blocked AVX2/NEON kernels!
let w = Tensor::arange(0.0, 5120.0, 1.0).reshape([64, 80]) * 0.0002 - 0.3
let b = Tensor::arange(0.0, 80.0, 1.0) * 0.01 - 0.2

task big_block(x: Tensor<f64, [96, 64], cpu>): Tensor<f64, [96, 80], cpu>
  return (x.rms_norm(1e-5) @ w + b).silu()
end

let x_base = Tensor::arange(0.0, 6144.0, 1.0).reshape([96, 64]) * 0.0005 - 0.8
let compiled = trace_dag(big_block, x_base).compile()

let eager = big_block(x_base)
let fast = compiled.run(x_base)
let max_err = (eager - fast).abs().max()

-- Concurrent execution of the exact same compiled DAG across 4 spawned tasks
let h1 = spawn fn() compiled.run(x_base * 1.1).sum() end
let h2 = spawn fn() compiled.run(x_base * 0.9).sum() end
let h3 = spawn fn() compiled.run(x_base * 1.25).sum() end
let h4 = spawn fn() compiled.run(x_base * 0.75).sum() end

let e1 = big_block(x_base * 1.1).sum()
let e2 = big_block(x_base * 0.9).sum()
let e3 = big_block(x_base * 1.25).sum()
let e4 = big_block(x_base * 0.75).sum()

let ok1 = math.abs(h1.join() - e1) < 1e-8
let ok2 = math.abs(h2.join() - e2) < 1e-8
let ok3 = math.abs(h3.join() - e3) < 1e-8
let ok4 = math.abs(h4.join() - e4) < 1e-8
let conc_ok = ok1 and ok2 and ok3 and ok4

let info = compiled.backend_info()
[str(max_err < 1e-10), str(conc_ok), str(info.get("backend")), str(info.get("simd_isa")), str(info.get("worker_threads"))]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("true".to_string()));
    assert_eq!(v[1], Value::String("true".to_string()));
    assert_eq!(v[2], Value::String("cpu".to_string()));
    assert!(!v[3].to_string().is_empty());
    let threads: usize = v[4].to_string().parse().expect("worker_threads int");
    assert!(threads >= 1);
}

#[test]
fn w4_p1_dag_plan_cache_and_static_buffer_lifetime_reuse() {
    let src = r#"
let w1 = Tensor::eye(8, 8) * 0.6 + 0.05
let w2 = Tensor::eye(8, 8) * 0.7 - 0.04
let w3 = Tensor::eye(8, 8) * 0.8 + 0.03
let w4 = Tensor::eye(8, 8) * 0.5 - 0.02
let w5 = Tensor::eye(8, 8) * 0.9 + 0.01
let w6 = Tensor::eye(8, 8) * 0.65 - 0.03

task deep_chain(x: Tensor<f64, [4, 8], cpu>): Tensor<f64, [4, 8], cpu>
  let h1 = (x.rms_norm(1e-5) @ w1).silu()
  let h2 = (h1.rms_norm(1e-5) @ w2).gelu()
  let h3 = (h2.rms_norm(1e-5) @ w3).tanh()
  let h4 = (h3.rms_norm(1e-5) @ w4).silu()
  let h5 = (h4.rms_norm(1e-5) @ w5).relu()
  return (h5.rms_norm(1e-5) @ w6).tanh()
end

let x0 = Tensor::arange(0.0, 32.0, 1.0).reshape([4, 8]) * 0.1 - 1.0
let raw1 = trace_dag(deep_chain, x0)
let raw2 = trace_dag(deep_chain, x0)

let c1 = raw1.compile()
let stats_after_first = c1.cache_stats()
let c2 = raw2.compile()
let c3 = c1.compile()
let stats_after_hits = c2.cache_stats()

let mem = c1.memory_plan()

-- Run compiled plan repeatedly with different inputs to prove scratch buffer reuse never corrupts state
let xa = x0 * 1.2 + 0.3
let xb = x0 * -0.8 + 0.5
let xc = x0 * 0.4 - 0.7
let err_a = (deep_chain(xa) - c1.run(xa)).abs().max()
let err_b = (deep_chain(xb) - c1.run(xb)).abs().max()
let err_c = (deep_chain(xc) - c2.run(xc)).abs().max()

[str(raw1.cache_key() == raw2.cache_key()), str(stats_after_first.get("misses")), str(stats_after_hits.get("hits")), str(mem.get("total_intermediates")), str(mem.get("buffer_slots")), str(mem.get("naive_peak_bytes")), str(mem.get("planned_peak_bytes")), str(mem.get("reuse_count")), str((err_a < 1e-11) and (err_b < 1e-11) and (err_c < 1e-11))]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("true".to_string()));
    let misses: usize = v[1].to_string().parse().expect("misses");
    let hits: usize = v[2].to_string().parse().expect("hits");
    let total_inter: usize = v[3].to_string().parse().expect("total_intermediates");
    let buf_slots: usize = v[4].to_string().parse().expect("buffer_slots");
    let naive_bytes: usize = v[5].to_string().parse().expect("naive_peak_bytes");
    let planned_bytes: usize = v[6].to_string().parse().expect("planned_peak_bytes");
    let reuse_count: usize = v[7].to_string().parse().expect("reuse_count");

    assert_eq!(misses, 1, "first compile should incur exactly 1 cache miss");
    assert!(
        hits >= 2,
        "structurally identical DAG compiles must hit DagPlanCache (hits={hits})"
    );
    assert!(
        total_inter >= 5,
        "expected >= 5 intermediate nodes in 6-layer chain, got {total_inter}"
    );
    assert!(
        buf_slots < total_inter,
        "linear-scan memory plan must reuse scratch buffers: slots={buf_slots}, intermediates={total_inter}"
    );
    assert!(
        planned_bytes < naive_bytes,
        "planned peak memory ({planned_bytes}B) must be strictly less than naive ({naive_bytes}B)"
    );
    assert!(
        reuse_count >= 3,
        "expected >= 3 scratch buffer reuses in 6-layer chain, got {reuse_count}"
    );
    assert_eq!(v[8], Value::String("true".to_string()));
}

#[test]
fn w4_p1_higher_order_ad_2nd_3rd_derivatives_and_exact_hvps() {
    let src = r#"
-- 1. Scalar 2nd derivative: f(x) = x^4 - 2*x^2 + 3*x => f''(x) = 12*x^2 - 4 => f''(2) = 44
task poly(x: float): float
  return (x ** 4.0) - 2.0 * (x ** 2.0) + 3.0 * x
end
let d2_poly = grad(grad(poly))(2.0)

-- 2. Scalar 3rd derivative: g(x) = x^4 + sin(x) => g'''(x) = 24*x - cos(x)
task trig_poly(x: float): float
  return (x ** 4.0) + math.sin(x)
end
let x_t = 1.5
let d3_trig = grad(grad(grad(trig_poly)))(x_t)
let d3_expected = 24.0 * x_t - math.cos(x_t)

-- 3. Multi-dim Tensor Higher-Order AD + ALL THREE exact Hessian-Vector Products (HVP):
--    (a) Forward-over-Reverse:  jvp(grad(loss), x, v)[1]
--    (b) Reverse-over-Forward:  grad(fn(xi) jvp(loss, xi, v)[1] end)(x)
--    (c) Reverse-over-Reverse:  grad(fn(xi) (grad(loss)(xi) * v).sum() end)(x)
--    verified against central finite differences of grad(loss)!
let w = tensor([[0.6, -0.4], [0.3, 0.8]])
let target = tensor([[1.0, -0.5], [0.25, 0.75]])

task neural_loss(x: Tensor<f64, [2, 2], cpu>): float
  let h = (x @ w).silu().rms_norm(1e-5)
  let p = h.softmax()
  let ln_part = (x @ w).gelu().layer_norm(1e-5)
  return (p * target).sum() + (ln_part * ln_part).sum() * 0.5
end

let x = tensor([[0.7, -0.5], [1.1, 0.3]])
let v = tensor([[0.4, -0.2], [0.15, 0.35]])

let g_fn = grad(neural_loss)
let f_over_r = jvp(g_fn, x, v)
let grad_val = f_over_r[0]
let hvp_fr = f_over_r[1]

let hvp_rf = grad(fn(xi: Tensor<f64, [2, 2], cpu>) jvp(neural_loss, xi, v)[1] end)(x)
let hvp_rr = grad(fn(xi: Tensor<f64, [2, 2], cpu>) (g_fn(xi) * v).sum() end)(x)

let eps = 1e-6
let hvp_fd = (g_fn(x + v * eps) - g_fn(x - v * eps)) / (2.0 * eps)

let err_fr_rf = (hvp_fr - hvp_rf).abs().max()
let err_fr_rr = (hvp_fr - hvp_rr).abs().max()
let err_fr_fd = (hvp_fr - hvp_fd).abs().max()
let grad_match = (grad_val - g_fn(x)).abs().max()

-- 4. Higher-order differentiation through zero-copy slicing (Gather + ScatterAdd adjoints)
task slice_loss(z: Tensor<f64, [4], cpu>): float
  let sub = z[1..3]
  return (sub ** 3.0).sum()
end
let z0 = tensor([1.0, 2.0, 3.0, 4.0])
let zv = tensor([1.0, 1.0, 1.0, 1.0])
-- d2/dz^2 (z[1]^3 + z[2]^3) * [1,1,1,1] = [0, 6*2, 6*3, 0] = [0, 12, 18, 0]
let slice_hvp = jvp(grad(slice_loss), z0, zv)[1]
let slice_expected = tensor([0.0, 12.0, 18.0, 0.0])
let slice_err = (slice_hvp - slice_expected).abs().max()

[str(math.abs(d2_poly - 44.0) < 1e-10), str(math.abs(d3_trig - d3_expected) < 1e-10), str(err_fr_rf < 1e-10), str(err_fr_rr < 1e-10), str(err_fr_fd < 1e-5), str(grad_match < 1e-11), str(slice_err < 1e-10)]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(
        v[0],
        Value::String("true".to_string()),
        "2nd deriv mismatch"
    );
    assert_eq!(
        v[1],
        Value::String("true".to_string()),
        "3rd deriv mismatch"
    );
    assert_eq!(
        v[2],
        Value::String("true".to_string()),
        "fwd-over-rev vs rev-over-fwd HVP mismatch"
    );
    assert_eq!(
        v[3],
        Value::String("true".to_string()),
        "fwd-over-rev vs rev-over-rev HVP mismatch"
    );
    assert_eq!(
        v[4],
        Value::String("true".to_string()),
        "exact HVP vs finite-diff HVP mismatch"
    );
    assert_eq!(
        v[5],
        Value::String("true".to_string()),
        "jvp(grad) primal vs grad mismatch"
    );
    assert_eq!(
        v[6],
        Value::String("true".to_string()),
        "slice Gather/ScatterAdd 2nd-order mismatch"
    );

    // Static HM rejection of invalid higher-order AD
    assert_typecheck_fails(
        "task bad(x: Tensor<f64, [2], cpu>): float\n  x.fill_(1.0)\n  return x.sum()\nend\ngrad(grad(bad))\n",
        "tensor_mutate",
    );
    assert_typecheck_fails(
        "task f(x: Tensor<f64, [2], cpu>): float\n  return (x * x).sum()\nend\njvp(grad(f), Tensor::zeros([2]), Tensor::ones([3]))\n",
        "mismatch",
    );
}

#[test]
fn w4_p1_trace_dag_of_grad_fuses_backward_graph() {
    let src = r#"
let w = tensor([[0.5, -0.3], [0.8, 0.4]])

task loss(x: Tensor<f64, [2, 2], cpu>): float
  let h = (x.rms_norm(1e-5) @ w).silu()
  return (h * h).sum()
end

let x0 = tensor([[1.0, -0.5], [0.3, 0.9]])
let bwd_dag = trace_dag(grad(loss), x0)
let bwd_compiled = bwd_dag.compile()

let x_new = tensor([[-0.7, 1.4], [0.6, -1.1]])
let eager_grad = grad(loss)(x_new)
let dag_grad = bwd_compiled.run(x_new)
let err = (eager_grad - dag_grad).abs().max()

[str(bwd_compiled.is_fused()), str(bwd_compiled.fused_count() >= 1), str(bwd_compiled.node_count() < bwd_dag.node_count()), str(err < 1e-11)]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("true".to_string()));
    assert_eq!(v[1], Value::String("true".to_string()));
    assert_eq!(v[2], Value::String("true".to_string()));
    assert_eq!(v[3], Value::String("true".to_string()));
}
