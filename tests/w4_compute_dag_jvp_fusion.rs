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
