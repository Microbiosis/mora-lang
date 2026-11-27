//! P6-2: SciLean-Style Symbolic Adjoint `TensorComputeDag` & Joint Forward+Backward Fusion Tests.
//!
//! Verifies:
//! 1. Multi-output `TensorComputeDag` (`outputs: Vec<usize>`, `run_multi`, `output_count`, single-output `run` guard).
//! 2. Exact symbolic VJP parity across ALL `DagOp` variants against eager `value_and_grad` AND central
//!    finite differences (elementwise, broadcast, structural `Reshape`/`Transpose`/`Slice`/`Pad`/`Cat`/`Gather`,
//!    reductions `Sum`/`Mean`/`SumAxis`, `Softmax`/`LogSoftmax`/`RmsNorm`/`LayerNorm`, `MatMul`).
//! 3. Analytic symbolic backward through `FusedLinearAct` (all 6 activations + bias) and `FusedRmsNormLinear`,
//!    plus backward `StepPos`/`Sign` fusion into `FusedElementwise` hitting W^X AVX2/SSE2 `native_jit`.
//! 4. Tiled FlashAttention-2 symbolic backward (`FlashAttentionLse` + `FlashAttentionGrad` for `dQ`/`dK`/`dV`/`dMask`)
//!    with GQA, pre-transposed K, and broadcast causal mask — never materializing `O(Sq * Sk)` attention matrices.
//! 5. Dead-adjoint pruning for non-`wrt` inputs and Pass 0 CSE across forward+backward.
//! 6. Cost-guided activation checkpointing (`autodiff_checkpointed` & `checkpoint`) strictly reducing
//!    `planned_peak_bytes` with `rematerialized_nodes > 0` while preserving exact numerical `[loss, grads...]`.
//! 7. `trace_value_and_grad_dag` builtin, `MORABC02` bytecode codec roundtrip, and HM `tensor_mutate` rejection.
//! 8. Pure-Mora `std/nn/optim` `CompiledTrainStep` training a real `CausalLM` (RoPE + RMSNorm + GQA + SwiGLU)
//!    matching eager `value_and_grad` + `AdamW`, plus distributed `ProcessGroup.all_reduce_async` parity.
//! 9. Live hardware GPU VRAM execution (CUDA/Vulkan/Metal when present) with zero output aliasing across runs.

use std::sync::Arc;

use mora::error::MoraResult;
use mora::parser_v3::ParserV3;
use mora::runtime::{CoreRuntime, vm};
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;

fn eval_source(src: &str) -> MoraResult<Value> {
    let (prog, witnesses) = ParserV3::compile(src)?;
    let errors = check_program_witnesses(&witnesses);
    assert!(
        errors.is_empty(),
        "unexpected typeck errors: {errors:?}\nsource:\n{src}"
    );
    let mut core = CoreRuntime::default();
    let (val, _) = vm::execute_program(Arc::new(prog), &mut core)?;
    Ok(val)
}

#[test]
fn p6_2_multi_output_dag_api_and_single_output_guard() {
    let src = r#"
let x = tensor([[1.0, -2.0], [3.0, 0.5]])
let w = tensor([[0.4, -0.3], [0.7, 0.2]])
let fwd = trace_dag(fn(a, b) ((a @ b).relu() * 1.5).sum() end, x, w).fuse()
assert_eq(fwd.output_count(), 1)

-- Default autodiff() differentiates w.r.t. all inputs [0, 1] -> outputs [loss, d_x, d_w]
let joint_all = fwd.autodiff()
assert_eq(joint_all.output_count(), 3)
assert(joint_all.is_fused())
let res_all = joint_all.run_multi(x, w)
assert_eq(res_all.len(), 3)

-- Explicit reordered wrt_indices `[1, 0]` returns `[loss, d_w, d_x]`
let joint_rev = fwd.autodiff([1, 0])
assert_eq(joint_rev.output_count(), 3)
let res_rev = joint_rev.run_multi([x, w])
assert(float((res_all[0] - res_rev[0]).abs().max()) < 1e-12)
assert(float((res_all[1] - res_rev[2]).abs().max()) < 1e-12)
assert(float((res_all[2] - res_rev[1]).abs().max()) < 1e-12)

-- Single integer wrt_index `1` returns `[loss, d_w]`
let joint_w = fwd.autodiff(1)
assert_eq(joint_w.output_count(), 2)
let res_w = joint_w.run_multi(x, w)
assert(float((res_all[2] - res_w[1]).abs().max()) < 1e-12)

-- Metadata & cost_report / memory_plan expose P6-2 fields
let rep = joint_all.cost_report()
assert_eq(rep["output_count"], 3)
assert_eq(rep["rematerialized_nodes"], 0)
let mp = joint_all.memory_plan()
assert_eq(mp["rematerialized_nodes"], 0)
assert(joint_all.cache_key().len() == 16)
assert(joint_all.to_dot().contains("digraph TensorComputeDag"))
true
"#;
    assert_eq!(
        eval_source(src).expect("multi-output API"),
        Value::Bool(true)
    );

    // Single-output `.run()` on a multi-output DAG must reject cleanly with an informative error
    let bad_run_src = r#"
let x = tensor([[1.0, -2.0], [3.0, 0.5]])
let w = tensor([[0.4, -0.3], [0.7, 0.2]])
let joint = trace_dag(fn(a, b) ((a @ b).relu() * 1.5).sum() end, x, w).fuse().autodiff()
joint.run(x, w)
"#;
    let err = eval_source(bad_run_src).expect_err("run() on multi-output DAG must error");
    let msg = format!("{err}");
    assert!(
        msg.contains("run_multi") || msg.contains("outputs"),
        "expected informative multi-output error, got: {msg}"
    );
}

#[test]
fn p6_2_symbolic_autodiff_parity_elementwise_and_broadcast() {
    let src = r#"
let x = tensor([[1.2, -0.7, 0.5, 2.1], [-1.3, 0.9, -0.4, 1.5]])
let y = tensor([0.6, -0.3, 1.1, 0.8])
let z = tensor([[1.5], [2.0]])

let f = fn(a: Tensor<f64, [2, 4], cpu>, b: Tensor<f64, [4], cpu>, c: Tensor<f64, [2, 1], cpu>)
  let t1 = (a + b) * c
  let t2 = t1.relu() + t1.gelu() + t1.silu()
  let t3 = t2.tanh() + t2.sigmoid() + (a.abs() + 0.5).sqrt()
  let t4 = (a.sin() - b.cos()) / (c + 1.0) + 1.5 / (a.abs() + 0.5)
  let t5 = ((a.abs() + 1.1).ln() + (a * 0.25).exp() + (a * a) ** 1.25)
  return (t3 + t4 + t5 - (2.0 - a) * 0.3).mean()
end

let eager = value_and_grad(fn(ps: list<any>) f(ps[0], ps[1], ps[2]) end)([x, y, z])
let dag = trace_dag(f, x, y, z).fuse().autodiff()
let got = dag.run_multi(x, y, z)

assert(math.abs(float(got[0].item()) - float(eager[0])) < 1e-10)
let i = 0
while i < 3
  let diff = float((got[i + 1] - eager[1][i]).abs().max())
  assert(diff < 1e-10, "grad mismatch at input " + str(i) + ": " + str(diff))
  assign i = i + 1
end

-- Finite-difference check on input 0 element (0, 1)
let eps = 1e-6
let xp = tensor([[1.2, -0.7 + eps, 0.5, 2.1], [-1.3, 0.9, -0.4, 1.5]])
let xm = tensor([[1.2, -0.7 - eps, 0.5, 2.1], [-1.3, 0.9, -0.4, 1.5]])
let fd = (float(f(xp, y, z)) - float(f(xm, y, z))) / (2.0 * eps)
let sym = float(got[1].to_list()[0][1])
assert(math.abs(fd - sym) < 1e-6, "FD mismatch: " + str(fd) + " vs " + str(sym))
true
"#;
    assert_eq!(
        eval_source(src).expect("elementwise+broadcast parity"),
        Value::Bool(true)
    );
}

#[test]
fn p6_2_symbolic_autodiff_parity_structural_reductions_and_norms() {
    let src = r#"
let has_op = fn(ops: list<string>, target: string)
  for op in ops
    if op == target then
      return true
    end
  end
  return false
end

let x = tensor([[0.5, -1.2, 0.8, 1.4, -0.3, 0.9], [1.1, 0.4, -0.9, 0.2, 1.7, -0.6], [-0.5, 1.3, 0.6, -1.1, 0.4, 0.8]])
let y = tensor([[0.2, -0.4, 0.7, 0.1, -0.8, 0.5], [-0.9, 1.0, 0.3, -0.2, 0.6, -0.1]])
let c = tensor([[0.3, -0.2, 0.5, 0.4, -0.1, 0.6]])
let row_bias = tensor([0.1, -0.2, 0.3])

let f = fn(a: any, b: any, rb: any)
  -- Contiguous slice (TapeOp::Slice -> DagOp::Slice -> adjoint DagOp::Pad)
  let sl = a[0:2, 1:5]
  -- Strided gather (TapeOp::Gather -> DagOp::Gather -> adjoint DagOp::ScatterAdd)
  let gath = a[0:3:2, 0:6:2]
  -- Cat along axis 0 with mixed tracked & constant tensors (TapeOp::Cat -> DagOp::Cat -> adjoint Slice)
  let cat0_in: list<any> = [a, b, c]
  let cat0 = Tensor::cat(cat0_in, 0)
  -- Cat along axis 1
  let cat1_in: list<any> = [sl, b[0:2, 0:2]]
  let cat1 = Tensor::cat(cat1_in, 1)
  -- Reshape, Transpose, Broadcast (+ rb -> backward SumAxis), Softmax, LogSoftmax, RmsNorm, LayerNorm
  let tr = cat0.transpose(0, 1)
  let rsh = tr.reshape([12, 3]) + rb
  let n1 = rsh.rms_norm(1e-5).softmax()
  let n2 = cat1.layer_norm(1e-5).log_softmax()
  let s1 = (n1 * n1).mean()
  let s2 = (n2 * n2).sum()
  let s3 = (gath * gath).sum()
  return s1 + s2 * 0.25 + s3 * 0.1
end

let eager = value_and_grad(fn(ps: list<any>) f(ps[0], ps[1], ps[2]) end)([x, y, row_bias])
let fwd = trace_dag(f, x, y, row_bias)
let ops = fwd.op_names()
assert(has_op(ops, "Slice"), "contiguous slice must lower to DagOp::Slice")
assert(has_op(ops, "Cat"), "Tensor::cat must lower to DagOp::Cat")
assert(has_op(ops, "LogSoftmax"), "log_softmax must lower to DagOp::LogSoftmax")

let joint = fwd.fuse().autodiff()
let jops = joint.op_names()
assert(has_op(jops, "Pad"), "Slice adjoint must emit DagOp::Pad")
assert(has_op(jops, "ScatterAdd"), "Gather adjoint must emit DagOp::ScatterAdd")
assert(has_op(jops, "SumAxis"), "broadcast adjoint must emit DagOp::SumAxis")

let got = joint.run_multi(x, y, row_bias)
assert(math.abs(float(got[0].item()) - float(eager[0])) < 1e-10)
let diff_x = float((got[1] - eager[1][0]).abs().max())
let diff_y = float((got[2] - eager[1][1]).abs().max())
let diff_rb = float((got[3] - eager[1][2]).abs().max())
assert(diff_x < 1e-10, "diff_x = " + str(diff_x))
assert(diff_y < 1e-10, "diff_y = " + str(diff_y))
assert(diff_rb < 1e-10, "diff_rb = " + str(diff_rb))

-- Finite difference verification on y[1, 3]
let eps = 1e-6
let yp = tensor([[0.2, -0.4, 0.7, 0.1, -0.8, 0.5], [-0.9, 1.0, 0.3, -0.2 + eps, 0.6, -0.1]])
let ym = tensor([[0.2, -0.4, 0.7, 0.1, -0.8, 0.5], [-0.9, 1.0, 0.3, -0.2 - eps, 0.6, -0.1]])
let fd = (float(f(x, yp, row_bias)) - float(f(x, ym, row_bias))) / (2.0 * eps)
let sym = float(got[2].to_list()[1][3])
assert(math.abs(fd - sym) < 1e-6, "FD mismatch on Cat+Slice+LogSoftmax: " + str(fd) + " vs " + str(sym))
true
"#;
    assert_eq!(
        eval_source(src).expect("structural/reductions/norms parity"),
        Value::Bool(true)
    );
}

#[test]
fn p6_2_fused_linear_act_rms_norm_linear_and_backward_jit_fusion() {
    let src = r#"
let has_op = fn(ops: list<string>, target: string)
  for op in ops
    if op == target then
      return true
    end
  end
  return false
end

let x = tensor([[0.5, -0.8, 1.1, 0.3, -0.4, 0.9, 0.2, -0.6], [-0.7, 0.4, -0.2, 1.3, 0.6, -0.5, 0.8, 0.1], [0.9, 0.2, -1.1, 0.5, -0.3, 0.7, -0.4, 1.2], [-0.3, 1.0, 0.6, -0.9, 0.8, 0.1, -0.7, 0.4]])
let gamma = tensor([1.1, 0.9, 1.05, 0.95, 1.2, 0.85, 1.0, 1.15])
let w1 = Tensor::randn([8, 8], "f64") * 0.25
let b1 = tensor([0.1, -0.2, 0.05, 0.15, -0.1, 0.2, -0.05, 0.08])
let w2 = Tensor::randn([8, 8], "f64") * 0.25
let b2 = tensor([-0.05, 0.1, 0.02, -0.08, 0.12, -0.03, 0.07, -0.04])
let w3 = Tensor::randn([8, 8], "f64") * 0.25

-- Exercises FusedRmsNormLinear (with gamma+bias+silu) AND FusedLinearAct (relu, gelu, tanh, sigmoid, none)
let f = fn(a, g, p1, p2, p3, p4, p5)
  let h1 = ((a.rms_norm(1e-5) * g) @ p1 + p2).silu()
  let h2 = (h1 @ p3 + p4).relu()
  let h3 = (h2 @ p5).gelu()
  let h4 = (h3 @ p1).tanh()
  let h5 = (h4 @ p3).sigmoid()
  return (h5 * h5 + h2.abs() * 0.1).mean()
end

let fwd = trace_dag(f, x, gamma, w1, b1, w2, b2, w3).fuse()
let fwd_ops = fwd.op_names()
assert(has_op(fwd_ops, "FusedRmsNormLinear"), "expected FusedRmsNormLinear in forward")
assert(has_op(fwd_ops, "FusedLinearAct"), "expected FusedLinearAct in forward")

let joint = fwd.autodiff()
let jops = joint.op_names()
assert(has_op(jops, "FusedElementwise"), "backward activation derivatives must fuse into FusedElementwise")

let eager = value_and_grad(fn(ps: list<any>) f(ps[0], ps[1], ps[2], ps[3], ps[4], ps[5], ps[6]) end)([x, gamma, w1, b1, w2, b2, w3])
let before_jit = joint.cost_report()["native_jit_dispatches"]
let got = joint.run_multi(x, gamma, w1, b1, w2, b2, w3)
let after_jit = joint.cost_report()["native_jit_dispatches"]

assert(math.abs(float(got[0].item()) - float(eager[0])) < 1e-10)
let i = 0
while i < 7
  let diff = float((got[i + 1] - eager[1][i]).abs().max())
  assert(diff < 1e-10, "FusedLinear/RmsNorm grad mismatch #" + str(i) + ": " + str(diff))
  assign i = i + 1
end

-- Verify native JIT actually executed on x86-64
let isa = joint.cost_report()["native_jit_isa"]
if isa != "fallback" and isa != "disabled" then
  assert(after_jit > before_jit, "expected W^X native JIT dispatches during joint fwd+bwd run")
end
true
"#;
    assert_eq!(
        eval_source(src).expect("fused linear + rms_norm_linear + JIT parity"),
        Value::Bool(true)
    );
}

#[test]
fn p6_2_tiled_flash_attention_2_symbolic_backward_gqa_and_causal_mask() {
    let src = r#"
let has_op = fn(ops: list<string>, target: string)
  for op in ops
    if op == target then
      return true
    end
  end
  return false
end

-- 4D Multi/Grouped-Query Attention (B=2, Hq=4, Hkv=1) + broadcast causal mask [1, 1, 8, 8]
let q = Tensor::randn([2, 4, 8, 6], "f64") * 0.4
let k = Tensor::randn([2, 1, 8, 6], "f64") * 0.4
let v = Tensor::randn([2, 1, 8, 6], "f64") * 0.4
let scale = 1.0 / math.sqrt(6.0)

-- Build causal mask [1, 1, 8, 8] with -1e9 on upper triangle
let m_rows: list<any> = []
let r = 0
while r < 8
  let row: list<float> = []
  let c = 0
  while c < 8
    assign row = row.push(if c > r then -1e9 else 0.0 end)
    assign c = c + 1
  end
  assign m_rows = m_rows.push(row)
  assign r = r + 1
end
let mask = Tensor::from_list(m_rows, "f64").reshape([1, 1, 8, 8])

let attn_fn = fn(qq, kk, vv, mm)
  let kt = kk.transpose(2, 3)
  let scores = (qq @ kt) * scale + mm
  let probs = scores.softmax()
  let out = probs @ vv
  return (out * out).sum() * 0.5
end

let fwd = trace_dag(attn_fn, q, k, v, mask).fuse()
assert(has_op(fwd.op_names(), "FusedFlashAttention"), "forward must fuse into FusedFlashAttention")
assert(not has_op(fwd.op_names(), "Softmax"), "forward must NOT materialize Softmax")

let joint = fwd.autodiff([0, 1, 2, 3])
let jops = joint.op_names()
assert(has_op(jops, "FusedFlashAttention"), "joint DAG must retain FusedFlashAttention")
assert(has_op(jops, "FlashAttentionLse"), "joint DAG must emit O(Sq) FlashAttentionLse")
assert(has_op(jops, "FlashAttentionGrad(dQ)"), "joint DAG must emit tiled FlashAttentionGrad(dQ)")
assert(has_op(jops, "FlashAttentionGrad(dK)"), "joint DAG must emit tiled FlashAttentionGrad(dK)")
assert(has_op(jops, "FlashAttentionGrad(dV)"), "joint DAG must emit tiled FlashAttentionGrad(dV)")
assert(has_op(jops, "FlashAttentionGrad(dMask)"), "joint DAG must emit tiled FlashAttentionGrad(dMask)")
assert(not has_op(jops, "Softmax"), "joint DAG must NEVER unfuse back to O(Sq*Sk) Softmax")

let eager = value_and_grad(fn(ps: list<any>) attn_fn(ps[0], ps[1], ps[2], ps[3]) end)([q, k, v, mask])
let got = joint.run_multi(q, k, v, mask)

assert(math.abs(float(got[0].item()) - float(eager[0])) < 1e-10)
let i = 0
while i < 4
  let diff = float((got[i + 1] - eager[1][i]).abs().max())
  assert(diff < 1e-10, "FlashAttention-2 backward mismatch for input #" + str(i) + ": " + str(diff))
  assign i = i + 1
end

-- Also test pre-transposed K (`k_is_transposed = true`, K already shape [2, 2, 4, 6])
let q2 = Tensor::randn([2, 2, 6, 4], "f64") * 0.5
let kt2 = Tensor::randn([2, 2, 4, 6], "f64") * 0.5
let v2 = Tensor::randn([2, 2, 6, 4], "f64") * 0.5
let attn_t = fn(qq, kkt, vv)
  let out = ((qq @ kkt) * 0.5).softmax() @ vv
  return out.mean()
end
let fwd2 = trace_dag(attn_t, q2, kt2, v2).fuse()
assert(has_op(fwd2.op_names(), "FusedFlashAttention"))
let joint2 = fwd2.autodiff()
let eager2 = value_and_grad(fn(ps: list<any>) attn_t(ps[0], ps[1], ps[2]) end)([q2, kt2, v2])
let got2 = joint2.run_multi(q2, kt2, v2)
assert(math.abs(float(got2[0].item()) - float(eager2[0])) < 1e-10)
assert(float((got2[1] - eager2[1][0]).abs().max()) < 1e-10)
assert(float((got2[2] - eager2[1][1]).abs().max()) < 1e-10)
assert(float((got2[3] - eager2[1][2]).abs().max()) < 1e-10)
true
"#;
    assert_eq!(
        eval_source(src).expect("FlashAttention-2 symbolic backward parity"),
        Value::Bool(true)
    );
}

#[test]
fn p6_2_dead_adjoint_pruning_and_pass0_cse() {
    let src = r#"
let x = Tensor::randn([4, 6], "f64")
let w = Tensor::randn([6, 6], "f64")
let target = Tensor::randn([4, 6], "f64")

let f = fn(inp, weight, tgt)
  -- Deliberate repeated pure subexpressions (duplicate transpose & norm) for Pass 0 CSE
  let wt1 = weight.transpose(0, 1)
  let wt2 = weight.transpose(0, 1)
  let h1 = (inp.rms_norm(1e-5) @ wt1).relu()
  let h2 = (inp.rms_norm(1e-5) @ wt2).relu()
  let diff = (h1 + h2) - tgt
  return (diff * diff).mean()
end

let fwd = trace_dag(f, x, w, target).fuse()
assert(fwd.cost_report()["algebraic_rewrites"] > 0, "Pass 0 CSE must deduplicate repeated pure ops")

let all_grads = fwd.autodiff([0, 1, 2])
let w_only = fwd.autodiff([1])

assert_eq(all_grads.output_count(), 4)
assert_eq(w_only.output_count(), 2)
assert(w_only.node_count() < all_grads.node_count(), "dead-adjoint pruning must drop unused input/target adjoint nodes")

let r_all = all_grads.run_multi(x, w, target)
let r_w = w_only.run_multi(x, w, target)
assert(math.abs(float(r_all[0].item()) - float(r_w[0].item())) < 1e-12)
assert(float((r_all[2] - r_w[1]).abs().max()) < 1e-12)
true
"#;
    assert_eq!(
        eval_source(src).expect("dead-adjoint pruning + CSE"),
        Value::Bool(true)
    );
}

#[test]
fn p6_2_cost_guided_activation_checkpointing_reduces_peak_memory() {
    let src = r#"
-- Deep 6-layer MLP: saves 6 large [16, 32] activations across fwd->bwd without checkpointing!
let x = Tensor::randn([16, 32], "f64") * 0.2
let w1 = Tensor::randn([32, 32], "f64") * 0.15
let w2 = Tensor::randn([32, 32], "f64") * 0.15
let w3 = Tensor::randn([32, 32], "f64") * 0.15
let w4 = Tensor::randn([32, 32], "f64") * 0.15
let w5 = Tensor::randn([32, 32], "f64") * 0.15
let w6 = Tensor::randn([32, 32], "f64") * 0.15

let deep_mlp = fn(a, p1, p2, p3, p4, p5, p6)
  let h1 = (a @ p1).rms_norm(1e-5).silu()
  let h2 = (h1 @ p2).rms_norm(1e-5).silu()
  let h3 = (h2 @ p3).rms_norm(1e-5).silu()
  let h4 = (h3 @ p4).rms_norm(1e-5).silu()
  let h5 = (h4 @ p5).rms_norm(1e-5).silu()
  let h6 = (h5 @ p6).relu()
  return (h6 * h6).mean()
end

let fwd = trace_dag(deep_mlp, x, w1, w2, w3, w4, w5, w6).fuse()
let full = fwd.autodiff([1, 2, 3, 4, 5, 6])
let full_mp = full.memory_plan()
let full_peak = full_mp["planned_peak_bytes"]

-- Request a tight memory budget (half of uncheckpointed planned peak)
let budget = full_peak / 2
let ckpt = fwd.autodiff_checkpointed([1, 2, 3, 4, 5, 6], budget)
let ckpt_mp = ckpt.memory_plan()
let ckpt_peak = ckpt_mp["planned_peak_bytes"]

assert(ckpt_mp["rematerialized_nodes"] > 0, "expected checkpointing to rematerialize forward nodes")
assert(ckpt_peak < full_peak, "checkpointing must strictly reduce planned_peak_bytes: " + str(ckpt_peak) + " vs " + str(full_peak))

-- Exact numerical identity of [loss, g_w1..g_w6]
let full_out = full.run_multi(x, w1, w2, w3, w4, w5, w6)
let ckpt_out = ckpt.run_multi(x, w1, w2, w3, w4, w5, w6)
assert_eq(full_out.len(), ckpt_out.len())
let i = 0
while i < full_out.len()
  let diff = float((full_out[i] - ckpt_out[i]).abs().max())
  assert(diff < 1e-11, "checkpointed output #" + str(i) + " diverged: " + str(diff))
  assign i = i + 1
end

-- Also test 1-arg overload `fwd.autodiff_checkpointed(budget)`
let ckpt_all = fwd.autodiff_checkpointed(budget)
assert_eq(ckpt_all.output_count(), 8)
assert(ckpt_all.memory_plan()["planned_peak_bytes"] < fwd.autodiff().memory_plan()["planned_peak_bytes"])
true
"#;
    assert_eq!(
        eval_source(src).expect("cost-guided activation checkpointing"),
        Value::Bool(true)
    );
}

#[test]
fn p6_2_trace_value_and_grad_dag_codec_and_hm_purity() {
    // 1. Functional parity for both single-tensor and list-of-params + extra_inputs forms
    let src = r#"
let x = tensor([1.0, -2.0, 3.0])
let d1 = trace_value_and_grad_dag(fn(a) (a * a + a.relu()).sum() end, x)
assert_eq(d1.output_count(), 2)
let r1 = d1.run_multi(x)
let e1 = value_and_grad(fn(a) (a * a + a.relu()).sum() end)(x)
assert(math.abs(float(r1[0].item()) - float(e1[0])) < 1e-12)
assert(float((r1[1] - e1[1]).abs().max()) < 1e-12)

-- List of params + extra inputs (x_in, y_tgt)
let w = tensor([[0.5, -0.3], [0.2, 0.8]])
let b = tensor([0.1, -0.1])
let xin = tensor([[1.0, 2.0], [-1.0, 0.5]])
let ytgt = tensor([[0.4, 0.2], [-0.3, 0.7]])
let loss_fn = fn(ps, inp, tgt)
  let pred = (inp @ ps[0] + ps[1]).tanh()
  let diff = pred - tgt
  return (diff * diff).mean()
end
let d2 = trace_value_and_grad_dag(loss_fn, [w, b], xin, ytgt)
assert_eq(d2.output_count(), 3)
let r2 = d2.run_multi([w, b, xin, ytgt])
let e2 = value_and_grad(fn(ps) loss_fn(ps, xin, ytgt) end)([w, b])
assert(math.abs(float(r2[0].item()) - float(e2[0])) < 1e-12)
assert(float((r2[1] - e2[1][0]).abs().max()) < 1e-12)
assert(float((r2[2] - e2[1][1]).abs().max()) < 1e-12)
true
"#;
    assert_eq!(
        eval_source(src).expect("trace_value_and_grad_dag"),
        Value::Bool(true)
    );

    // 2. Static HM purity check rejects in-place tensor_mutate inside trace_value_and_grad_dag
    let impure_src = "task bad(x: Tensor<f64, [2], cpu>): Tensor<f64, [2], cpu>\n  x.fill_(1.0)\n  return x\nend\ntrace_value_and_grad_dag(bad, Tensor::zeros([2]))\n";
    let (_, witnesses) = ParserV3::compile(impure_src).expect("parse impure");
    let diags = check_program_witnesses(&witnesses);
    assert!(
        diags
            .iter()
            .any(|d| format!("{d:?}").contains("tensor_mutate")),
        "HM must reject in-place tensor mutation inside trace_value_and_grad_dag; got {diags:?}"
    );
}

#[test]
fn p6_2_compiled_train_step_causal_lm_e2e_and_eager_parity() {
    let src = r#"
import "std/nn/layers"
import "std/nn/optim"

let has_op = fn(ops: list<string>, target: string)
  for op in ops
    if op == target then
      return true
    end
  end
  return false
end

let cfg = ModelConfig(vocab_size = 16, d_model = 12, n_layers = 1, n_heads = 3, n_kv_heads = 1, d_ff = 24, dtype = "f32", device = "cpu")
let model = causal_lm_init(cfg, 0.12)
let inp_ids = [1, 3, 5, 7, 2]
let tgt_ids = [3, 5, 7, 2, 4]

let params0 = model.parameters()
let loss_fn = fn(ps: list<any>)
  let m = model.with_parameters(ps)
  let logits = m.forward(inp_ids)
  return cross_entropy_loss(logits, tgt_ids)
end

-- Build CompiledTrainStep once (compiles full RoPE+RMSNorm+GQA FlashAttention+SwiGLU fwd+bwd DAG!)
let trainer = compiled_train_step_new(loss_fn, params0)
let ops = trainer.dag.op_names()
assert(has_op(ops, "FusedFlashAttention"), "CausalLM compiled DAG must fuse FlashAttention")
assert(has_op(ops, "FlashAttentionGrad(dQ)"), "CausalLM compiled DAG must use tiled FlashAttention-2 backward")
assert(has_op(ops, "FusedRmsNormLinear") or has_op(ops, "FusedLinearAct"), "CausalLM compiled DAG must fuse linear layers")

-- Step 0 exact parity with eager value_and_grad!
let eager0 = value_and_grad(loss_fn)(params0)
let comp0 = trainer.forward_backward(params0)
assert(math.abs(float(comp0.loss.item()) - float(eager0[0])) < 1e-4, "CausalLM loss mismatch: " + str(float(comp0.loss.item())) + " vs " + str(float(eager0[0])))
assert_eq(comp0.grads.len(), eager0[1].len())
let gi = 0
while gi < comp0.grads.len()
  let d = float((comp0.grads[gi] - eager0[1][gi]).abs().max())
  assert(d < 2e-4, "CausalLM grad mismatch #" + str(gi) + ": " + str(d))
  assign gi = gi + 1
end

-- Train 6 steps with CompiledTrainStep.step_adamw vs eager AdamW and verify trajectory parity + loss drop!
let c_params = params0
let e_params = params0
let c_opt = adamw_new(params0, 0.04, 0.9, 0.999, 0.00000001, 0.001)
let e_opt = adamw_new(params0, 0.04, 0.9, 0.999, 0.00000001, 0.001)
let first_loss = 0.0
let last_loss = 0.0
let step = 0
while step < 6
  let c_res = trainer.step_adamw(c_opt, c_params, [], 5.0)
  assign c_params = c_res.params
  assign c_opt = c_res.state
  if step == 0 then
    assign first_loss = float(c_res.loss.item())
  end
  assign last_loss = float(c_res.loss.item())

  let evg = value_and_grad(loss_fn)(e_params)
  let eg = clip_grad_norm(evg[1], 5.0)
  let e_res = e_opt.step(e_params, eg)
  assign e_params = e_res.params
  assign e_opt = e_res.state
  assert(math.abs(float(c_res.loss.item()) - float(evg[0])) < 5e-4, "step " + str(step) + " loss diverged")
  assign step = step + 1
end
assert(last_loss < first_loss * 0.85, "compiled CausalLM training must decrease loss: " + str(first_loss) + " -> " + str(last_loss))

-- Also verify checkpointed CompiledTrainStep matches step 0 exactly!
let ckpt_budget = trainer.dag.memory_plan()["planned_peak_bytes"] / 2
let ckpt_trainer = compiled_train_step_new(loss_fn, params0, [], ckpt_budget)
let ckpt0 = ckpt_trainer.forward_backward(params0)
assert(math.abs(float(ckpt0.loss.item()) - float(comp0.loss.item())) < 1e-5)
assert(float((ckpt0.grads[0] - comp0.grads[0]).abs().max()) < 1e-4)
true
"#;
    assert_eq!(
        eval_source(src).expect("CausalLM CompiledTrainStep E2E"),
        Value::Bool(true)
    );
}

#[test]
fn p6_2_compiled_train_step_distributed_all_reduce_async() {
    let src = r#"
import "std/nn/optim"

let w0 = tensor([[0.4, -0.2], [0.1, 0.5]])
let b0 = tensor([0.05, -0.05])
let x_r0 = tensor([[1.0, 0.5], [-0.5, 1.2]])
let y_r0 = tensor([[0.8, -0.3], [0.2, 0.6]])
let x_r1 = tensor([[-0.6, 1.4], [0.9, -0.4]])
let y_r1 = tensor([[-0.2, 0.9], [0.5, -0.1]])

let loss_fn = fn(ps: list<any>, x, y)
  let diff = (x @ ps[0] + ps[1]).tanh() - y
  return (diff * diff).mean()
end

let pg = process_group(2)
let ranks = pg.ranks()

let run_rank = fn(my_pg, x_b, y_b)
  let params = [w0, b0]
  let opt = adamw_new(params, 0.05)
  let trainer = compiled_train_step_new(loss_fn, params, [x_b, y_b])
  let res = trainer.step_adamw(opt, params, [x_b, y_b], 0.0, -1.0, my_pg)
  return res.params
end

let h0 = spawn fn() run_rank(ranks[0], x_r0, y_r0) end
let h1 = spawn fn() run_rank(ranks[1], x_r1, y_r1) end
let p0 = h0.join()
let p1 = h1.join()

-- Both ranks must converge to identical weights after averaged all_reduce_async!
assert(float((p0[0] - p1[0]).abs().max()) < 1e-12)
assert(float((p0[1] - p1[1]).abs().max()) < 1e-12)
-- And weights must have moved from w0
assert(float((p0[0] - w0).abs().max()) > 1e-4)
true
"#;
    assert_eq!(
        eval_source(src).expect("distributed CompiledTrainStep"),
        Value::Bool(true)
    );
}

fn backend_usable(dev: &str) -> bool {
    let src = format!(
        "let t = Tensor::from_list([1.0, -2.0], \"f32\").to_device(\"{dev}\")\nlet y = (t + 1.0f32).relu()\ny.to_device(\"cpu\").to_list()\n"
    );
    eval_source(&src).is_ok()
}

fn target_devices_for_vram_test() -> Vec<String> {
    let mut devs = Vec::new();
    if let Ok(Value::List(items)) = eval_source("Tensor::devices()") {
        for item in items.iter() {
            if let Value::String(name) = item
                && ["cuda", "vulkan", "metal"]
                    .iter()
                    .any(|p| name.starts_with(p))
                && backend_usable(name)
            {
                devs.push(name.clone());
            }
        }
    }
    if devs.is_empty() {
        devs.push("cpu".to_string());
    }
    devs
}

#[test]
fn p6_2_gpu_vram_symbolic_autodiff_and_no_output_aliasing() {
    for gpu_dev in target_devices_for_vram_test() {
        let src = format!(
            r#"
let gpu_dev = "{gpu_dev}"

-- Exercise f32 on every live hardware GPU (CUDA PTX / Vulkan SPIR-V / Metal MSL),
-- including BOTH StepPos (relu bwd, opcode 25) and Sign (abs bwd, opcode 26)
let x_cpu = Tensor::randn([8, 16], "f32", "cpu") * 0.3f32
let w1_cpu = Tensor::randn([16, 16], "f32", "cpu") * 0.2f32
let b1_cpu = Tensor::randn([16], "f32", "cpu") * 0.1f32
let w2_cpu = Tensor::randn([16, 8], "f32", "cpu") * 0.2f32

let f = fn(a, p1, bias, p2)
  let h = (a @ p1 + bias).relu()
  let y = (h @ p2).silu()
  return (y * y).mean() + (h.abs() * 0.1f32).mean()
end

let cpu_dag = trace_dag(f, x_cpu, w1_cpu, b1_cpu, w2_cpu).fuse().autodiff([1, 2, 3])
let cpu_ref = cpu_dag.run_multi(x_cpu, w1_cpu, b1_cpu, w2_cpu)

let x_dev = x_cpu.to(gpu_dev)
let w1_dev = w1_cpu.to(gpu_dev)
let b1_dev = b1_cpu.to(gpu_dev)
let w2_dev = w2_cpu.to(gpu_dev)

let dev_dag = trace_dag(f, x_dev, w1_dev, b1_dev, w2_dev).fuse().autodiff([1, 2, 3])
-- Run twice with DIFFERENT inputs to prove returned VRAM output tensors never alias pooled slots!
let run1 = dev_dag.run_multi(x_dev, w1_dev, b1_dev, w2_dev)
let x_other = (x_cpu * -2.0f32 + 0.5f32).to(gpu_dev)
let run2 = dev_dag.run_multi(x_other, w1_dev, b1_dev, w2_dev)

-- Verify run1 outputs were NOT corrupted by run2 AND match CPU reference!
let i = 0
while i < 4
  let g_dev = run1[i].to("cpu")
  let diff = float((g_dev - cpu_ref[i]).abs().max())
  assert(diff < 5e-4, "GPU VRAM autodiff mismatch #" + str(i) + " on " + gpu_dev + ": " + str(diff))
  assign i = i + 1
end
-- Ensure run2 genuinely produced different gradients
assert(float((run2[1].to("cpu") - run1[1].to("cpu")).abs().max()) > 1e-4)

-- Also verify checkpointed autodiff on gpu_dev
let full_peak = dev_dag.memory_plan()["planned_peak_bytes"]
let ckpt_dag = trace_dag(f, x_dev, w1_dev, b1_dev, w2_dev).fuse().autodiff_checkpointed([1, 2, 3], full_peak / 2)
let ckpt_run = ckpt_dag.run_multi(x_dev, w1_dev, b1_dev, w2_dev)
let j = 0
while j < 4
  let diff = float((ckpt_run[j].to("cpu") - cpu_ref[j]).abs().max())
  assert(diff < 5e-4, "GPU VRAM checkpointed mismatch #" + str(j) + " on " + gpu_dev + ": " + str(diff))
  assign j = j + 1
end
true
"#
        );
        assert_eq!(
            eval_source(&src)
                .unwrap_or_else(|e| panic!("GPU VRAM autodiff failed on {gpu_dev}: {e}")),
            Value::Bool(true)
        );
    }
}

#[test]
fn p6_2_pow_scalar_nonpositive_base_and_zero_exponent_guards() {
    let src = r#"
-- Integer exponents so (-2.0) ** x and 0.0 ** x produce finite forward values;
-- symbolic autodiff must guard c <= 0.0 (avoiding c.ln() => NaN / -inf)
-- and x ** 0.0 at x == 0.0 (avoiding 0.0 ** -1.0 * 0.0 => NaN).
let x = tensor([2.0, 3.0, 4.0])
let x_with_zero = tensor([0.0, 2.0, -3.0])

let f_neg_base = fn(a: Tensor<f64, [3], cpu>) ((-2.0) ** a).sum() end
let d_neg = trace_dag(f_neg_base, x).fuse().autodiff()
let r_neg = d_neg.run_multi(x)
let e_neg = value_and_grad(f_neg_base)(x)
assert(math.abs(float(r_neg[0].item()) - float(e_neg[0])) < 1e-12)
assert(float((r_neg[1] - e_neg[1]).abs().max()) < 1e-12)
assert_eq(r_neg[1].to_list(), [0.0, 0.0, 0.0])

let f_zero_base = fn(a: Tensor<f64, [3], cpu>) (0.0 ** a).sum() end
let d_zero = trace_dag(f_zero_base, x).fuse().autodiff()
let r_zero = d_zero.run_multi(x)
let e_zero = value_and_grad(f_zero_base)(x)
assert(math.abs(float(r_zero[0].item()) - float(e_zero[0])) < 1e-12)
assert(float((r_zero[1] - e_zero[1]).abs().max()) < 1e-12)
assert_eq(r_zero[1].to_list(), [0.0, 0.0, 0.0])

let f_one_base = fn(a: Tensor<f64, [3], cpu>) (1.0 ** a).sum() end
let d_one = trace_dag(f_one_base, x).fuse().autodiff()
let r_one = d_one.run_multi(x)
assert_eq(r_one[1].to_list(), [0.0, 0.0, 0.0])

let f_pos_base = fn(a: Tensor<f64, [3], cpu>) (2.5 ** a).sum() end
let d_pos = trace_dag(f_pos_base, x).fuse().autodiff()
let r_pos = d_pos.run_multi(x)
let e_pos = value_and_grad(f_pos_base)(x)
assert(math.abs(float(r_pos[0].item()) - float(e_pos[0])) < 1e-10)
assert(float((r_pos[1] - e_pos[1]).abs().max()) < 1e-10)

let f_zero_exp = fn(a: Tensor<f64, [3], cpu>) (a ** 0.0).sum() end
let d_zexp = trace_dag(f_zero_exp, x_with_zero).fuse().autodiff()
let r_zexp = d_zexp.run_multi(x_with_zero)
let e_zexp = value_and_grad(f_zero_exp)(x_with_zero)
assert(math.abs(float(r_zexp[0].item()) - float(e_zexp[0])) < 1e-12)
assert(float((r_zexp[1] - e_zexp[1]).abs().max()) < 1e-12)
assert_eq(r_zexp[1].to_list(), [0.0, 0.0, 0.0])
true
"#;
    assert_eq!(
        eval_source(src).expect("PowScalar edge-case guards"),
        Value::Bool(true)
    );
}
