//! Wave 4 P2-2 verification: Native Hardware GPU `DeviceBackend`s
//! (`CUDA Driver API` + PTX ISA 7.0 `f64`, `Vulkan 1.0` + pure-Rust SPIR-V, `Metal` + MSL).
//!
//! - Works cleanly on BOTH real GPU hosts (verified live on NVIDIA CUDA + Vulkan GPUs)
//!   and headless CPU-only CI runners (graceful fallback to `EmulatedDeviceBackend`).
//! - Verifies numerical parity across all 5 fused kernels (`MatMul`, `FusedLinearAct`,
//!   `FusedRmsNormLinear`, `FusedFlashAttention`, `FusedElementwise`) between GPU and CPU.
//! - Verifies `Device::Vulkan` static HM cross-device checks, `Tensor::devices()` /
//!   `Tensor::device_info()`, and LSP completion parity.

use std::collections::HashMap;
use std::sync::Arc;

use mora::flow::json::JsonValue as JsonVal;
use mora::lsp::providers::completion_v3;
use mora::lsp::server::DocumentState;
use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
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

fn assert_typecheck_fails(src: &str) {
    let (_prog, witnesses) =
        ParserV3::compile(src).unwrap_or_else(|_e| panic!("compile failed:\n{src}"));
    let errs = check_program_witnesses(&witnesses);
    assert!(
        !errs.is_empty(),
        "expected static type error for:\n{src}\nbut typeck succeeded"
    );
}

#[test]
fn p2_gpu_device_discovery_telemetry_and_static_vulkan_types() {
    let src = r#"
let devs: list<string> = Tensor::devices()
let cpu_info: dict<string, string> = Tensor::device_info("cpu")
let cuda_info: dict<string, string> = Tensor::device_info("cuda:0")
let vk_info: dict<string, string> = Tensor::device_info("vulkan:0")
let mtl_info: dict<string, string> = Tensor::device_info("metal:0")
let vt: Tensor<f32, [2, 3], vulkan:0> = Tensor::ones([2, 3], dtype="f32", device="vulkan:0")
[str(devs.len() >= 1), str(devs.get(0)), str(cpu_info.get("driver")), str(cpu_info.get("hardware_accelerated")), str(cuda_info.get("driver")), str(cuda_info.get("hardware_accelerated")), str(cuda_info.get("gpu_name")), str(vk_info.get("driver")), str(vk_info.get("hardware_accelerated")), str(vk_info.get("gpu_name")), str(mtl_info.get("driver")), vt.device()]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    assert_eq!(v[0], Value::String("true".to_string()));
    assert_eq!(v[1], Value::String("cpu".to_string()));
    assert_eq!(v[2], Value::String("cpu".to_string()));
    assert_eq!(v[3], Value::String("true".to_string()));
    assert_eq!(v[11], Value::String("vulkan:0".to_string()));

    // Log detected backends cleanly for inspection
    eprintln!(
        "[P2 GPU Probe] cuda: driver={:?} hw={:?} name={:?} | vulkan: driver={:?} hw={:?} name={:?} | metal: driver={:?}",
        v[4], v[5], v[6], v[7], v[8], v[9], v[10]
    );

    // If hardware CUDA is present, verify real hardware telemetry invariants
    if v[5] == Value::String("true".to_string()) {
        assert_eq!(v[4], Value::String("cuda".to_string()));
        assert!(!v[6].to_string().is_empty());
    }
    if v[8] == Value::String("true".to_string()) {
        assert_eq!(v[7], Value::String("vulkan".to_string()));
        assert!(!v[9].to_string().is_empty());
    }

    // Static HM cross-device isolation for `vulkan:0`
    assert_typecheck_fails(
        r#"
let a = Tensor::ones([2, 2], device="vulkan:0")
let b = Tensor::ones([2, 2], device="cpu")
let c = a + b
"#,
    );
    assert_typecheck_fails(
        r#"
let a = Tensor::ones([2, 2], device="vulkan:0")
let b = Tensor::ones([2, 2], device="cuda:0")
let c = a @ b
"#,
    );
    assert_typecheck_fails("let d: int = Tensor::devices()\n");
    assert_typecheck_fails("let info: string = Tensor::device_info(\"cpu\")\n");

    // LSP completion includes Tensor::devices & Tensor::device_info
    let mut docs = HashMap::new();
    let uri = "file:///p2_gpu.mora";
    docs.insert(
        uri.to_string(),
        DocumentState {
            uri: uri.to_string(),
            text: "let d = Tensor::devices()\n".to_string(),
            version: 1,
            diagnostics: Vec::new(),
        },
    );
    let comp = completion_v3(
        &docs,
        &JsonVal::Object(
            [(
                "textDocument".to_string(),
                JsonVal::Object(
                    [("uri".to_string(), JsonVal::String_(uri.to_string()))]
                        .into_iter()
                        .collect(),
                ),
            )]
            .into_iter()
            .collect(),
        ),
    );
    let labels: Vec<&str> = comp
        .as_array()
        .expect("completion array")
        .iter()
        .filter_map(|item| item.get("label").and_then(|v| v.as_str()))
        .collect();
    assert!(labels.contains(&"Tensor::devices"));
    assert!(labels.contains(&"Tensor::device_info"));
}

#[test]
fn p2_cuda_ptx_all_five_fused_kernels_match_cpu_reference() {
    let src = r#"
-- Build identical fused ComputeDags on `cpu` and `cuda:0` and compare elementwise parity!
let x_cpu = tensor([[0.5, -1.2, 0.8, 0.3], [-0.4, 1.5, -0.7, 0.9], [1.1, 0.2, -0.3, 0.6]], dtype="f64", device="cpu")
let w_cpu = tensor([[0.3, -0.5, 0.7], [0.2, 0.4, -0.1], [-0.6, 0.8, 0.2], [0.5, -0.3, 0.9]], dtype="f64", device="cpu")
let g_cpu = tensor([1.1, 0.9, 1.2, 0.8], dtype="f64", device="cpu")
let b_cpu = tensor([0.1, -0.2, 0.3], dtype="f64", device="cpu")

let x_gpu = x_cpu.to_device("cuda:0")
let w_gpu = w_cpu.to_device("cuda:0")
let g_gpu = g_cpu.to_device("cuda:0")
let b_gpu = b_cpu.to_device("cuda:0")

-- 1. FusedRmsNormLinear (RMSNorm * gamma @ W + bias + gelu)
let rms_fn = fn(x, g, w, b) ((x.rms_norm(1e-5) * g) @ w + b).gelu() end
let d_rms_cpu = trace_dag(rms_fn, x_cpu, g_cpu, w_cpu, b_cpu).compile()
let d_rms_gpu = trace_dag(rms_fn, x_gpu, g_gpu, w_gpu, b_gpu).compile()
let out_rms_cpu = d_rms_cpu.run(x_cpu, g_cpu, w_cpu, b_cpu)
let out_rms_gpu = d_rms_gpu.run(x_gpu, g_gpu, w_gpu, b_gpu)
let diff_rms = (out_rms_cpu - out_rms_gpu.to_device("cpu")).abs().max()

-- 2. FusedLinearAct across silu / tanh / sigmoid / relu
let lin_fn = fn(x, w, b) (x @ w + b).silu().tanh().sigmoid().relu() end
let d_lin_cpu = trace_dag(lin_fn, x_cpu, w_cpu, b_cpu).compile()
let d_lin_gpu = trace_dag(lin_fn, x_gpu, w_gpu, b_gpu).compile()
let diff_lin = (d_lin_cpu.run(x_cpu, w_cpu, b_cpu) - d_lin_gpu.run(x_gpu, w_gpu, b_gpu).to_device("cpu")).abs().max()

-- 3. Batched broadcast MatMul [2, 2, 3] @ [1, 3, 2]
let ba_cpu = tensor([[[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]], [[-1.0, 0.5, 2.0], [3.0, -2.0, 1.0]]], dtype="f64", device="cpu")
let bb_cpu = tensor([[[0.5, -0.5], [1.0, 2.0], [-1.5, 0.25]]], dtype="f64", device="cpu")
let ba_gpu = ba_cpu.to_device("cuda:0")
let bb_gpu = bb_cpu.to_device("cuda:0")
let mm_fn = fn(a, b) a @ b end
let diff_mm = (trace_dag(mm_fn, ba_cpu, bb_cpu).compile().run(ba_cpu, bb_cpu) - trace_dag(mm_fn, ba_gpu, bb_gpu).compile().run(ba_gpu, bb_gpu).to_device("cpu")).abs().max()

-- 4. FusedFlashAttention (multi-head GQA [1, 4, 3, 4] Q with [1, 2, 3, 4] K,V + causal mask!)
let q_cpu = Tensor::arange(0.0, 48.0, 1.0, dtype="f64", device="cpu").reshape([1, 4, 3, 4]) * 0.05
let k_cpu = Tensor::arange(0.0, 24.0, 1.0, dtype="f64", device="cpu").reshape([1, 2, 3, 4]) * 0.04
let v_cpu = Tensor::arange(1.0, 25.0, 1.0, dtype="f64", device="cpu").reshape([1, 2, 3, 4]) * 0.1
let mask_cpu = tensor([[0.0, -1000000000.0, -1000000000.0], [0.0, 0.0, -1000000000.0], [0.0, 0.0, 0.0]], dtype="f64", device="cpu")

-- Broadcast K, V along dim 1 cleanly via repeat-compatible shape [1, 1, 3, 4] (MQA)
let km_cpu = Tensor::arange(0.0, 12.0, 1.0, dtype="f64", device="cpu").reshape([1, 1, 3, 4]) * 0.04
let vm_cpu = Tensor::arange(1.0, 13.0, 1.0, dtype="f64", device="cpu").reshape([1, 1, 3, 4]) * 0.1
let q_gpu = q_cpu.to_device("cuda:0")
let km_gpu = km_cpu.to_device("cuda:0")
let vm_gpu = vm_cpu.to_device("cuda:0")
let mask_gpu = mask_cpu.to_device("cuda:0")

let attn_fn = fn(q, k, v, m) (((q @ k.transpose(-2, -1)) * 0.5) + m).softmax() @ v end
let d_fa_cpu = trace_dag(attn_fn, q_cpu, km_cpu, vm_cpu, mask_cpu).compile()
let d_fa_gpu = trace_dag(attn_fn, q_gpu, km_gpu, vm_gpu, mask_gpu).compile()
let diff_fa = (d_fa_cpu.run(q_cpu, km_cpu, vm_cpu, mask_cpu) - d_fa_gpu.run(q_gpu, km_gpu, vm_gpu, mask_gpu).to_device("cpu")).abs().max()

-- 5. FusedElementwise multi-step broadcast chain (exp, ln, sqrt, tanh, sigmoid, pow, abs, neg)
let ew_fn = fn(a, b) (((a * 0.5 + b).abs() + 0.5).exp().ln().sqrt() + (-a).tanh() + b.sigmoid() - (a ** 2.0) * 0.1) end
let e1_cpu = tensor([[0.4, -0.8, 1.2], [0.9, -0.3, 0.5]], dtype="f64", device="cpu")
let e2_cpu = tensor([0.2, 0.5, -0.1], dtype="f64", device="cpu")
let e1_gpu = e1_cpu.to_device("cuda:0")
let e2_gpu = e2_cpu.to_device("cuda:0")
let d_ew_cpu = trace_dag(ew_fn, e1_cpu, e2_cpu).compile()
let d_ew_gpu = trace_dag(ew_fn, e1_gpu, e2_gpu).compile()
let diff_ew = (d_ew_cpu.run(e1_cpu, e2_cpu) - d_ew_gpu.run(e1_gpu, e2_gpu).to_device("cpu")).abs().max()

let binfo = d_fa_gpu.backend_info()
[str(diff_rms < 1e-10), str(diff_lin < 1e-10), str(diff_mm < 1e-10), str(diff_fa < 1e-10), str(diff_ew < 1e-10), str(binfo.get("backend")), str(binfo.get("hardware_accelerated")), str(str(d_fa_gpu.op_names()).contains("FusedFlashAttention")), str(str(d_rms_gpu.op_names()).contains("FusedRmsNormLinear")), str(str(d_ew_gpu.op_names()).contains("FusedElementwise")), str(diff_rms), str(diff_fa), str(diff_ew)]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    eprintln!(
        "[P2 CUDA Parity] rms={} (err={}) lin={} mm={} fa={} (err={}) ew={} (err={}) backend={:?} hw={:?} fused_ew={}",
        v[0], v[10], v[1], v[2], v[3], v[11], v[4], v[12], v[5], v[6], v[9]
    );
    assert_eq!(
        v[0],
        Value::String("true".to_string()),
        "RMSNormLinear parity failed"
    );
    assert_eq!(
        v[1],
        Value::String("true".to_string()),
        "FusedLinearAct parity failed"
    );
    assert_eq!(
        v[2],
        Value::String("true".to_string()),
        "Batched MatMul parity failed"
    );
    assert_eq!(
        v[3],
        Value::String("true".to_string()),
        "FusedFlashAttention parity failed"
    );
    assert_eq!(
        v[4],
        Value::String("true".to_string()),
        "FusedElementwise parity failed"
    );
    assert_eq!(
        v[7],
        Value::String("true".to_string()),
        "FlashAttention must fuse on GPU DAG"
    );
    assert_eq!(
        v[8],
        Value::String("true".to_string()),
        "RmsNormLinear must fuse on GPU DAG"
    );
    assert_eq!(
        v[9],
        Value::String("true".to_string()),
        "Elementwise chain must fuse on GPU DAG"
    );
}

#[test]
fn p2_vulkan_spirv_compute_and_concurrent_gpu_spawn_parity() {
    let src = r#"
let x_cpu = tensor([[0.5, -1.2, 0.8, 0.3], [-0.4, 1.5, -0.7, 0.9], [1.1, 0.2, -0.3, 0.6]], dtype="f32", device="cpu")
let w_cpu = tensor([[0.3, -0.5, 0.7], [0.2, 0.4, -0.1], [-0.6, 0.8, 0.2], [0.5, -0.3, 0.9]], dtype="f32", device="cpu")
let b_cpu = tensor([0.1, -0.2, 0.3], dtype="f32", device="cpu")

let x_vk = x_cpu.to_device("vulkan:0")
let w_vk = w_cpu.to_device("vulkan:0")
let b_vk = b_cpu.to_device("vulkan:0")

let f_mm = fn(x, w) x @ w end
let f_relu = fn(x, w, b) (x @ w + b).relu() end
let f_gelu = fn(x, w, b) (x @ w + b).gelu() end
let f_silu = fn(x, w, b) (x @ w + b).silu() end
let f_tanh = fn(x, w, b) (x @ w + b).tanh() end
let f_sig = fn(x, w, b) (x @ w + b).sigmoid() end

let d_mm_vk = trace_dag(f_mm, x_vk, w_vk).compile()
let d_relu_vk = trace_dag(f_relu, x_vk, w_vk, b_vk).compile()
let d_gelu_vk = trace_dag(f_gelu, x_vk, w_vk, b_vk).compile()
let d_silu_vk = trace_dag(f_silu, x_vk, w_vk, b_vk).compile()
let d_tanh_vk = trace_dag(f_tanh, x_vk, w_vk, b_vk).compile()
let d_sig_vk = trace_dag(f_sig, x_vk, w_vk, b_vk).compile()

-- Concurrent spawn executing Vulkan & CUDA DAGs simultaneously
let h_vk = spawn fn()
  let e_mm = (trace_dag(f_mm, x_cpu, w_cpu).compile().run(x_cpu, w_cpu) - d_mm_vk.run(x_vk, w_vk).to_device("cpu")).abs().max()
  let e_relu = (trace_dag(f_relu, x_cpu, w_cpu, b_cpu).compile().run(x_cpu, w_cpu, b_cpu) - d_relu_vk.run(x_vk, w_vk, b_vk).to_device("cpu")).abs().max()
  let e_gelu = (trace_dag(f_gelu, x_cpu, w_cpu, b_cpu).compile().run(x_cpu, w_cpu, b_cpu) - d_gelu_vk.run(x_vk, w_vk, b_vk).to_device("cpu")).abs().max()
  let e_silu = (trace_dag(f_silu, x_cpu, w_cpu, b_cpu).compile().run(x_cpu, w_cpu, b_cpu) - d_silu_vk.run(x_vk, w_vk, b_vk).to_device("cpu")).abs().max()
  let e_tanh = (trace_dag(f_tanh, x_cpu, w_cpu, b_cpu).compile().run(x_cpu, w_cpu, b_cpu) - d_tanh_vk.run(x_vk, w_vk, b_vk).to_device("cpu")).abs().max()
  let e_sig = (trace_dag(f_sig, x_cpu, w_cpu, b_cpu).compile().run(x_cpu, w_cpu, b_cpu) - d_sig_vk.run(x_vk, w_vk, b_vk).to_device("cpu")).abs().max()
  return e_mm < 1e-4f32 and e_relu < 1e-4f32 and e_gelu < 1e-4f32 and e_silu < 1e-4f32 and e_tanh < 1e-4f32 and e_sig < 1e-4f32
end

let x_cu = x_cpu.to_device("cuda:0")
let w_cu = w_cpu.to_device("cuda:0")
let b_cu = b_cpu.to_device("cuda:0")
let d_cu = trace_dag(f_gelu, x_cu, w_cu, b_cu).compile()
let h_cu = spawn fn()
  let e = (trace_dag(f_gelu, x_cpu, w_cpu, b_cpu).compile().run(x_cpu, w_cpu, b_cpu) - d_cu.run(x_cu, w_cu, b_cu).to_device("cpu")).abs().max()
  return e < 1e-4f32
end

let ok_vk = h_vk.join()
let ok_cu = h_cu.join()
let vk_info = d_gelu_vk.backend_info()
[str(ok_vk), str(ok_cu), str(vk_info.get("backend")), str(vk_info.get("simd_isa")), str(vk_info.get("hardware_accelerated"))]
"#;
    let out = compile_check_and_run(src);
    let Value::List(items) = out else {
        panic!("expected list, got {out:?}");
    };
    let v = items.to_vec();
    eprintln!(
        "[P2 Vulkan+Concurrent Parity] vk={} cu={} backend={:?} isa={:?} hw={:?}",
        v[0], v[1], v[2], v[3], v[4]
    );
    assert_eq!(
        v[0],
        Value::String("true".to_string()),
        "Vulkan SPIR-V parity failed"
    );
    assert_eq!(
        v[1],
        Value::String("true".to_string()),
        "Concurrent CUDA parity failed"
    );
}

#[test]
fn p2_force_cpu_emulation_env_override_switches_cleanly() {
    let bin = env!("CARGO_BIN_EXE_mora");
    let td = mora::test_support::TempDir::new("p2_gpu_emul");
    let script_path = td.path().join("emul_probe.mora");
    let script = r#"
let ci = Tensor::device_info("cuda:0")
let vi = Tensor::device_info("vulkan:0")
let x = tensor([[1.0, 2.0], [3.0, 4.0]], dtype="f64", device="cuda:0")
let y = tensor([[0.5, -0.5], [1.0, 2.0]], dtype="f64", device="cuda:0")
let d = trace_dag(fn(a, b) (a @ b).relu() end, x, y).compile()
let out = d.run(x, y).to_device("cpu")
print(ci.get("driver"), ci.get("hardware_accelerated"), vi.get("driver"), vi.get("hardware_accelerated"), out.sum())
"#;
    std::fs::write(&script_path, script).expect("write emul probe script");
    let output = std::process::Command::new(bin)
        .env("MORA_FORCE_CPU_EMULATION", "1")
        .arg(&script_path)
        .output()
        .expect("run mora with MORA_FORCE_CPU_EMULATION=1");
    assert!(
        output.status.success(),
        "emulated run failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("cuda(host-emulated)") && stdout.contains("vulkan(host-emulated)"),
        "expected host-emulated drivers under MORA_FORCE_CPU_EMULATION=1, got: {stdout}"
    );
    assert!(
        stdout.contains("false"),
        "expected hardware_accelerated=false, got: {stdout}"
    );
}
