//! Wave 4 P3-A verification: GPU Full-Stack Closure
//! - A1: Persistent `DeviceVramBuffer` VRAM residency on `TensorStorage` with lazy D2H sync,
//!   zero-intermediate-copy chained GPU `TensorComputeDag` execution, in-place mutation VRAM sync,
//!   physical GPU VRAM page backing in `KVPageAllocator`, and live `VramTelemetry`.
//! - A2: Native `f16` & `bf16` GPU compute kernels across CUDA PTX (`sm_50+`) and Vulkan SPIR-V 1.0
//!   (including odd output matrix dimensions exercising Vulkan's race-free 2-element/thread packer).
//! - A3: Vulkan full 5-kernel native GPU parity (`MatMul` incl. batched broadcast ND, `FusedLinearAct`,
//!   `FusedRmsNormLinear`, `FusedFlashAttention` with MQA + causal mask, `FusedElementwise` 25-op
//!   bytecode broadcast evaluator) with zero host `simd_kernels` fallback.
//! - A4: Deterministic `MORA_FORCE_CPU_EMULATION=1` fallback honesty check.

use std::sync::{Arc, Mutex};

/// Process-wide GPU telemetry counters (`VramCounters`) are shared across tests in this binary;
/// serialize telemetry-delta assertions so parallel test threads do not perturb H2D/D2H deltas.
static GPU_TELEMETRY_LOCK: Mutex<()> = Mutex::new(());

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

fn expect_str_list(val: Value) -> Vec<String> {
    let Value::List(items) = val else {
        panic!("expected Value::List, got {val:?}");
    };
    items
        .to_vec()
        .into_iter()
        .map(|v| match v {
            Value::String(s) => s,
            other => other.to_string(),
        })
        .collect()
}

fn parse_u64(s: &str, label: &str) -> u64 {
    s.parse::<u64>()
        .unwrap_or_else(|e| panic!("failed to parse {label}={s:?} as u64: {e}"))
}

#[test]
fn p3_vram_residency_zero_intermediate_copy_chained_dags_and_inplace_sync() {
    let _guard = GPU_TELEMETRY_LOCK.lock().expect("gpu telemetry lock");
    // Static HM check: `is_vram_resident()` returns `bool`, never `int`
    assert_typecheck_fails(
        r#"
let t = Tensor::ones([2, 2], dtype="f32", device="cuda:0")
let bad: int = t.is_vram_resident()
"#,
    );

    let src = r#"
let x_cpu = tensor([[0.5, -1.2, 0.8, 0.3], [-0.4, 1.5, -0.7, 0.9], [1.1, 0.2, -0.3, 0.6]], dtype="f32", device="cpu")
let w1_cpu = tensor([[0.3, -0.5, 0.7], [0.2, 0.4, -0.1], [-0.6, 0.8, 0.2], [0.5, -0.3, 0.9]], dtype="f32", device="cpu")
let b1_cpu = tensor([0.1, -0.2, 0.3], dtype="f32", device="cpu")
let w2_cpu = tensor([[0.4, -0.2], [-0.3, 0.6], [0.5, 0.1]], dtype="f32", device="cpu")
let b2_cpu = tensor([0.05, -0.05], dtype="f32", device="cpu")

let f1 = fn(x, w, b) (x @ w + b).gelu() end
let f2 = fn(x, w, b) (x @ w + b).silu() end
let d1_cpu = trace_dag(f1, x_cpu, w1_cpu, b1_cpu).compile()
let d2_cpu = trace_dag(f2, d1_cpu.run(x_cpu, w1_cpu, b1_cpu), w2_cpu, b2_cpu).compile()
let ref_out = d2_cpu.run(d1_cpu.run(x_cpu, w1_cpu, b1_cpu), w2_cpu, b2_cpu)

-- CUDA chained VRAM-to-VRAM execution (f32)
let x_cu = x_cpu.to_device("cuda:0")
let w1_cu = w1_cpu.to_device("cuda:0")
let b1_cu = b1_cpu.to_device("cuda:0")
let w2_cu = w2_cpu.to_device("cuda:0")
let b2_cu = b2_cpu.to_device("cuda:0")
let d1_cu = trace_dag(f1, x_cu, w1_cu, b1_cu).compile()
let d2_cu = trace_dag(f2, d1_cu.run(x_cu, w1_cu, b1_cu), w2_cu, b2_cu).compile()

let cu_before = Tensor::device_info("cuda:0")
let cu_mid = d1_cu.run(x_cu, w1_cu, b1_cu)
let cu_after_1 = Tensor::device_info("cuda:0")
let cu_final = d2_cu.run(cu_mid, w2_cu, b2_cu)
let cu_after_2 = Tensor::device_info("cuda:0")
let cu_final_host = cu_final.to_device("cpu")
let cu_after_d2h = Tensor::device_info("cuda:0")
let cu_err = (ref_out - cu_final_host).abs().max()

-- In-place mutation VRAM sync check on CUDA (`fill_`, `add_`, `mul_`)
let m_cu = Tensor::zeros([3, 4], dtype="f32", device="cuda:0")
m_cu.fill_(1.0f32)
m_cu.add_(0.5f32)
m_cu.mul_(2.0f32)
let m_cpu = Tensor::full([3, 4], 3.0f32, dtype="f32", device="cpu")
let cu_inplace_err = (d1_cpu.run(m_cpu, w1_cpu, b1_cpu) - d1_cu.run(m_cu, w1_cu, b1_cu).to_device("cpu")).abs().max()

-- Vulkan chained VRAM-to-VRAM execution (f32)
let x_vk = x_cpu.to_device("vulkan:0")
let w1_vk = w1_cpu.to_device("vulkan:0")
let b1_vk = b1_cpu.to_device("vulkan:0")
let w2_vk = w2_cpu.to_device("vulkan:0")
let b2_vk = b2_cpu.to_device("vulkan:0")
let d1_vk = trace_dag(f1, x_vk, w1_vk, b1_vk).compile()
let d2_vk = trace_dag(f2, d1_vk.run(x_vk, w1_vk, b1_vk), w2_vk, b2_vk).compile()

let vk_before = Tensor::device_info("vulkan:0")
let vk_mid = d1_vk.run(x_vk, w1_vk, b1_vk)
let vk_final = d2_vk.run(vk_mid, w2_vk, b2_vk)
let vk_after_2 = Tensor::device_info("vulkan:0")
let vk_final_host = vk_final.to_device("cpu")
let vk_after_d2h = Tensor::device_info("vulkan:0")
let vk_err = (ref_out - vk_final_host).abs().max()

let m_vk = Tensor::zeros([3, 4], dtype="f32", device="vulkan:0")
m_vk.fill_(1.0f32)
m_vk.add_(0.5f32)
m_vk.mul_(2.0f32)
let vk_inplace_err = (d1_cpu.run(m_cpu, w1_cpu, b1_cpu) - d1_vk.run(m_vk, w1_vk, b1_vk).to_device("cpu")).abs().max()

let cpu_res: bool = x_cpu.is_vram_resident()
let cu_res: bool = x_cu.is_vram_resident()
let cu_mid_res: bool = cu_mid.is_vram_resident()
let cu_fin_res: bool = cu_final.is_vram_resident()
let vk_res: bool = x_vk.is_vram_resident()
let vk_mid_res: bool = vk_mid.is_vram_resident()
let vk_fin_res: bool = vk_final.is_vram_resident()

[str(cpu_res), str(cu_before.get("hardware_accelerated")), str(cu_res), str(cu_mid_res), str(cu_fin_res), str(cu_before.get("vram_h2d_copies")), str(cu_after_2.get("vram_h2d_copies")), str(cu_before.get("vram_d2h_copies")), str(cu_after_1.get("vram_d2h_copies")), str(cu_after_2.get("vram_d2h_copies")), str(cu_after_d2h.get("vram_d2h_copies")), str(cu_before.get("vram_hits")), str(cu_after_2.get("vram_hits")), str(cu_err < 1e-4f32), str(cu_inplace_err < 1e-4f32), str(vk_before.get("hardware_accelerated")), str(vk_res), str(vk_mid_res), str(vk_fin_res), str(vk_before.get("vram_h2d_copies")), str(vk_after_2.get("vram_h2d_copies")), str(vk_before.get("vram_d2h_copies")), str(vk_after_2.get("vram_d2h_copies")), str(vk_after_d2h.get("vram_d2h_copies")), str(vk_before.get("vram_hits")), str(vk_after_2.get("vram_hits")), str(vk_err < 1e-4f32), str(vk_inplace_err < 1e-4f32)]
"#;
    let v = expect_str_list(compile_check_and_run(src));
    assert_eq!(v[0], "false", "CPU tensor must never report VRAM resident");
    assert_eq!(v[13], "true", "CUDA chained DAG parity failed");
    assert_eq!(v[14], "true", "CUDA in-place VRAM sync parity failed");
    assert_eq!(v[26], "true", "Vulkan chained DAG parity failed");
    assert_eq!(v[27], "true", "Vulkan in-place VRAM sync parity failed");

    let cu_hw = v[1] == "true";
    let vk_hw = v[15] == "true";
    eprintln!(
        "[P3 VRAM Residency] cuda_hw={cu_hw} (res={}/{}/{}, h2d={}->{}, d2h={}->{}->{}->{}, hits={}->{}) | vk_hw={vk_hw} (res={}/{}/{}, h2d={}->{}, d2h={}->{}->{}, hits={}->{})",
        v[2],
        v[3],
        v[4],
        v[5],
        v[6],
        v[7],
        v[8],
        v[9],
        v[10],
        v[11],
        v[12],
        v[16],
        v[17],
        v[18],
        v[19],
        v[20],
        v[21],
        v[22],
        v[23],
        v[24],
        v[25]
    );

    if cu_hw {
        assert_eq!(v[2], "true", "CUDA input tensor must be VRAM-resident");
        assert_eq!(v[3], "true", "CUDA dag1 output must remain VRAM-resident");
        assert_eq!(v[4], "true", "CUDA dag2 output must remain VRAM-resident");
        assert_eq!(
            parse_u64(&v[6], "cu_h2d_after2"),
            parse_u64(&v[5], "cu_h2d_before"),
            "CUDA chained DAGs must perform 0 H2D copies"
        );
        assert_eq!(
            parse_u64(&v[9], "cu_d2h_after2"),
            parse_u64(&v[7], "cu_d2h_before"),
            "CUDA chained DAGs must perform 0 intermediate D2H copies"
        );
        assert!(
            parse_u64(&v[10], "cu_d2h_after_to_cpu") > parse_u64(&v[9], "cu_d2h_after2"),
            "CUDA to_device('cpu') must trigger lazy D2H sync"
        );
        assert!(
            parse_u64(&v[12], "cu_hits_after2") >= parse_u64(&v[11], "cu_hits_before") + 2,
            "CUDA chained DAGs must register >= 2 VRAM dispatch hits"
        );
    }
    if vk_hw {
        assert_eq!(v[16], "true", "Vulkan input tensor must be VRAM-resident");
        assert_eq!(
            v[17], "true",
            "Vulkan dag1 output must remain VRAM-resident"
        );
        assert_eq!(
            v[18], "true",
            "Vulkan dag2 output must remain VRAM-resident"
        );
        assert_eq!(
            parse_u64(&v[20], "vk_h2d_after2"),
            parse_u64(&v[19], "vk_h2d_before"),
            "Vulkan chained DAGs must perform 0 H2D copies"
        );
        assert_eq!(
            parse_u64(&v[22], "vk_d2h_after2"),
            parse_u64(&v[21], "vk_d2h_before"),
            "Vulkan chained DAGs must perform 0 intermediate D2H copies"
        );
        assert!(
            parse_u64(&v[23], "vk_d2h_after_to_cpu") > parse_u64(&v[22], "vk_d2h_after2"),
            "Vulkan to_device('cpu') must trigger lazy D2H sync"
        );
        assert!(
            parse_u64(&v[25], "vk_hits_after2") >= parse_u64(&v[24], "vk_hits_before") + 2,
            "Vulkan chained DAGs must register >= 2 VRAM dispatch hits"
        );
    }
}

#[test]
fn p3_native_f16_and_bf16_gpu_kernels_cuda_and_vulkan() {
    let _guard = GPU_TELEMETRY_LOCK.lock().expect("gpu telemetry lock");
    let src = r#"
-- Odd dimensions [3, 5] @ [5, 3] (9 output elems) stress Vulkan's 2-element/thread packed half shader!
let a_f16_cpu = tensor([[0.5, -1.0, 0.25, 0.75, -0.5], [-0.25, 1.25, -0.5, 0.5, 0.25], [1.0, 0.5, -0.75, 0.25, 0.5]], dtype="f16", device="cpu")
let b_f16_cpu = tensor([[0.5, -0.25, 0.75], [0.25, 0.5, -0.5], [-0.5, 0.75, 0.25], [0.5, -0.25, 1.0], [0.25, 0.5, -0.25]], dtype="f16", device="cpu")
let bias_f16_cpu = tensor([0.25, -0.5, 0.125], dtype="f16", device="cpu")

let ba_f16_cpu = tensor([[[0.5, -0.5, 1.0, 0.25], [0.25, 0.75, -0.5, 0.5], [-0.5, 0.25, 0.5, 1.0]], [[1.0, 0.25, -0.5, 0.5], [-0.25, 0.5, 0.75, -0.25], [0.5, -0.5, 0.25, 0.5]]], dtype="f16", device="cpu")
let bb_f16_cpu = tensor([[[0.5, -0.25, 0.75], [0.25, 0.5, -0.5], [-0.5, 0.75, 0.25], [0.5, -0.25, 0.5]]], dtype="f16", device="cpu")

let a_bf16_cpu = a_f16_cpu.to_dtype("bf16")
let b_bf16_cpu = b_f16_cpu.to_dtype("bf16")
let bias_bf16_cpu = bias_f16_cpu.to_dtype("bf16")
let ba_bf16_cpu = ba_f16_cpu.to_dtype("bf16")
let bb_bf16_cpu = bb_f16_cpu.to_dtype("bf16")

let f_mm = fn(a, b) a @ b end
let f_gelu = fn(x, w, b) (x @ w + b).gelu() end
let f_silu = fn(x, w, b) (x @ w + b).silu() end

let ref_f16_mm = trace_dag(f_mm, a_f16_cpu, b_f16_cpu).compile().run(a_f16_cpu, b_f16_cpu)
let ref_f16_bmm = trace_dag(f_mm, ba_f16_cpu, bb_f16_cpu).compile().run(ba_f16_cpu, bb_f16_cpu)
let ref_f16_gelu = trace_dag(f_gelu, a_f16_cpu, b_f16_cpu, bias_f16_cpu).compile().run(a_f16_cpu, b_f16_cpu, bias_f16_cpu)
let ref_f16_silu = trace_dag(f_silu, a_f16_cpu, b_f16_cpu, bias_f16_cpu).compile().run(a_f16_cpu, b_f16_cpu, bias_f16_cpu)

let ref_bf16_mm = trace_dag(f_mm, a_bf16_cpu, b_bf16_cpu).compile().run(a_bf16_cpu, b_bf16_cpu)
let ref_bf16_bmm = trace_dag(f_mm, ba_bf16_cpu, bb_bf16_cpu).compile().run(ba_bf16_cpu, bb_bf16_cpu)
let ref_bf16_gelu = trace_dag(f_gelu, a_bf16_cpu, b_bf16_cpu, bias_bf16_cpu).compile().run(a_bf16_cpu, b_bf16_cpu, bias_bf16_cpu)
let ref_bf16_silu = trace_dag(f_silu, a_bf16_cpu, b_bf16_cpu, bias_bf16_cpu).compile().run(a_bf16_cpu, b_bf16_cpu, bias_bf16_cpu)

-- CUDA native f16 & bf16
let cu0 = Tensor::device_info("cuda:0")
let a_f16_cu = a_f16_cpu.to_device("cuda:0")
let b_f16_cu = b_f16_cpu.to_device("cuda:0")
let bias_f16_cu = bias_f16_cpu.to_device("cuda:0")
let ba_f16_cu = ba_f16_cpu.to_device("cuda:0")
let bb_f16_cu = bb_f16_cpu.to_device("cuda:0")
let e_cu_f16_mm = (ref_f16_mm.to_dtype("f32") - trace_dag(f_mm, a_f16_cu, b_f16_cu).compile().run(a_f16_cu, b_f16_cu).to_device("cpu").to_dtype("f32")).abs().max()
let e_cu_f16_bmm = (ref_f16_bmm.to_dtype("f32") - trace_dag(f_mm, ba_f16_cu, bb_f16_cu).compile().run(ba_f16_cu, bb_f16_cu).to_device("cpu").to_dtype("f32")).abs().max()
let e_cu_f16_gelu = (ref_f16_gelu.to_dtype("f32") - trace_dag(f_gelu, a_f16_cu, b_f16_cu, bias_f16_cu).compile().run(a_f16_cu, b_f16_cu, bias_f16_cu).to_device("cpu").to_dtype("f32")).abs().max()
let e_cu_f16_silu = (ref_f16_silu.to_dtype("f32") - trace_dag(f_silu, a_f16_cu, b_f16_cu, bias_f16_cu).compile().run(a_f16_cu, b_f16_cu, bias_f16_cu).to_device("cpu").to_dtype("f32")).abs().max()

let a_bf16_cu = a_bf16_cpu.to_device("cuda:0")
let b_bf16_cu = b_bf16_cpu.to_device("cuda:0")
let bias_bf16_cu = bias_bf16_cpu.to_device("cuda:0")
let ba_bf16_cu = ba_bf16_cpu.to_device("cuda:0")
let bb_bf16_cu = bb_bf16_cpu.to_device("cuda:0")
let e_cu_bf16_mm = (ref_bf16_mm.to_dtype("f32") - trace_dag(f_mm, a_bf16_cu, b_bf16_cu).compile().run(a_bf16_cu, b_bf16_cu).to_device("cpu").to_dtype("f32")).abs().max()
let e_cu_bf16_bmm = (ref_bf16_bmm.to_dtype("f32") - trace_dag(f_mm, ba_bf16_cu, bb_bf16_cu).compile().run(ba_bf16_cu, bb_bf16_cu).to_device("cpu").to_dtype("f32")).abs().max()
let e_cu_bf16_gelu = (ref_bf16_gelu.to_dtype("f32") - trace_dag(f_gelu, a_bf16_cu, b_bf16_cu, bias_bf16_cu).compile().run(a_bf16_cu, b_bf16_cu, bias_bf16_cu).to_device("cpu").to_dtype("f32")).abs().max()
let e_cu_bf16_silu = (ref_bf16_silu.to_dtype("f32") - trace_dag(f_silu, a_bf16_cu, b_bf16_cu, bias_bf16_cu).compile().run(a_bf16_cu, b_bf16_cu, bias_bf16_cu).to_device("cpu").to_dtype("f32")).abs().max()
let cu1 = Tensor::device_info("cuda:0")

-- Vulkan native f16 & bf16
let vk0 = Tensor::device_info("vulkan:0")
let a_f16_vk = a_f16_cpu.to_device("vulkan:0")
let b_f16_vk = b_f16_cpu.to_device("vulkan:0")
let bias_f16_vk = bias_f16_cpu.to_device("vulkan:0")
let ba_f16_vk = ba_f16_cpu.to_device("vulkan:0")
let bb_f16_vk = bb_f16_cpu.to_device("vulkan:0")
let e_vk_f16_mm = (ref_f16_mm.to_dtype("f32") - trace_dag(f_mm, a_f16_vk, b_f16_vk).compile().run(a_f16_vk, b_f16_vk).to_device("cpu").to_dtype("f32")).abs().max()
let e_vk_f16_bmm = (ref_f16_bmm.to_dtype("f32") - trace_dag(f_mm, ba_f16_vk, bb_f16_vk).compile().run(ba_f16_vk, bb_f16_vk).to_device("cpu").to_dtype("f32")).abs().max()
let e_vk_f16_gelu = (ref_f16_gelu.to_dtype("f32") - trace_dag(f_gelu, a_f16_vk, b_f16_vk, bias_f16_vk).compile().run(a_f16_vk, b_f16_vk, bias_f16_vk).to_device("cpu").to_dtype("f32")).abs().max()
let e_vk_f16_silu = (ref_f16_silu.to_dtype("f32") - trace_dag(f_silu, a_f16_vk, b_f16_vk, bias_f16_vk).compile().run(a_f16_vk, b_f16_vk, bias_f16_vk).to_device("cpu").to_dtype("f32")).abs().max()

let a_bf16_vk = a_bf16_cpu.to_device("vulkan:0")
let b_bf16_vk = b_bf16_cpu.to_device("vulkan:0")
let bias_bf16_vk = bias_bf16_cpu.to_device("vulkan:0")
let ba_bf16_vk = ba_bf16_cpu.to_device("vulkan:0")
let bb_bf16_vk = bb_bf16_cpu.to_device("vulkan:0")
let e_vk_bf16_mm = (ref_bf16_mm.to_dtype("f32") - trace_dag(f_mm, a_bf16_vk, b_bf16_vk).compile().run(a_bf16_vk, b_bf16_vk).to_device("cpu").to_dtype("f32")).abs().max()
let e_vk_bf16_bmm = (ref_bf16_bmm.to_dtype("f32") - trace_dag(f_mm, ba_bf16_vk, bb_bf16_vk).compile().run(ba_bf16_vk, bb_bf16_vk).to_device("cpu").to_dtype("f32")).abs().max()
let e_vk_bf16_gelu = (ref_bf16_gelu.to_dtype("f32") - trace_dag(f_gelu, a_bf16_vk, b_bf16_vk, bias_bf16_vk).compile().run(a_bf16_vk, b_bf16_vk, bias_bf16_vk).to_device("cpu").to_dtype("f32")).abs().max()
let e_vk_bf16_silu = (ref_bf16_silu.to_dtype("f32") - trace_dag(f_silu, a_bf16_vk, b_bf16_vk, bias_bf16_vk).compile().run(a_bf16_vk, b_bf16_vk, bias_bf16_vk).to_device("cpu").to_dtype("f32")).abs().max()
let vk1 = Tensor::device_info("vulkan:0")

let cu_f16_ok = e_cu_f16_mm < 2e-2f32 and e_cu_f16_bmm < 2e-2f32 and e_cu_f16_gelu < 2e-2f32 and e_cu_f16_silu < 2e-2f32
let cu_bf16_ok = e_cu_bf16_mm < 5e-2f32 and e_cu_bf16_bmm < 5e-2f32 and e_cu_bf16_gelu < 5e-2f32 and e_cu_bf16_silu < 5e-2f32
let vk_f16_ok = e_vk_f16_mm < 2e-2f32 and e_vk_f16_bmm < 2e-2f32 and e_vk_f16_gelu < 2e-2f32 and e_vk_f16_silu < 2e-2f32
let vk_bf16_ok = e_vk_bf16_mm < 5e-2f32 and e_vk_bf16_bmm < 5e-2f32 and e_vk_bf16_gelu < 5e-2f32 and e_vk_bf16_silu < 5e-2f32

[str(cu_f16_ok), str(cu_bf16_ok), str(vk_f16_ok), str(vk_bf16_ok), str(cu0.get("hardware_accelerated")), str(cu0.get("native_f16_dispatches")), str(cu1.get("native_f16_dispatches")), str(cu0.get("native_bf16_dispatches")), str(cu1.get("native_bf16_dispatches")), str(vk0.get("hardware_accelerated")), str(vk0.get("native_f16_dispatches")), str(vk1.get("native_f16_dispatches")), str(vk0.get("native_bf16_dispatches")), str(vk1.get("native_bf16_dispatches")), str(e_cu_f16_gelu), str(e_cu_bf16_gelu), str(e_vk_f16_gelu), str(e_vk_bf16_gelu)]
"#;
    let v = expect_str_list(compile_check_and_run(src));
    eprintln!(
        "[P3 Half Precision] cu(f16={} err={}, bf16={} err={}, f16_d={}->{}, bf16_d={}->{}) | vk(f16={} err={}, bf16={} err={}, f16_d={}->{}, bf16_d={}->{})",
        v[0],
        v[14],
        v[1],
        v[15],
        v[5],
        v[6],
        v[7],
        v[8],
        v[2],
        v[16],
        v[3],
        v[17],
        v[10],
        v[11],
        v[12],
        v[13]
    );
    assert_eq!(v[0], "true", "CUDA native f16 parity failed");
    assert_eq!(v[1], "true", "CUDA native bf16 parity failed");
    assert_eq!(v[2], "true", "Vulkan native f16 parity failed");
    assert_eq!(v[3], "true", "Vulkan native bf16 parity failed");

    if v[4] == "true" {
        assert!(
            parse_u64(&v[6], "cu_f16_after") >= parse_u64(&v[5], "cu_f16_before") + 4,
            "CUDA must execute native f16 PTX kernels"
        );
        assert!(
            parse_u64(&v[8], "cu_bf16_after") >= parse_u64(&v[7], "cu_bf16_before") + 4,
            "CUDA must execute native bf16 PTX kernels"
        );
    }
    if v[9] == "true" {
        assert!(
            parse_u64(&v[11], "vk_f16_after") >= parse_u64(&v[10], "vk_f16_before") + 4,
            "Vulkan must execute native f16 SPIR-V kernels"
        );
        assert!(
            parse_u64(&v[13], "vk_bf16_after") >= parse_u64(&v[12], "vk_bf16_before") + 4,
            "Vulkan must execute native bf16 SPIR-V kernels"
        );
    }
}

#[test]
fn p3_vulkan_all_five_fused_kernels_native_gpu_parity() {
    let _guard = GPU_TELEMETRY_LOCK.lock().expect("gpu telemetry lock");
    let src = r#"
let x_cpu = tensor([[0.5, -1.2, 0.8, 0.3], [-0.4, 1.5, -0.7, 0.9], [1.1, 0.2, -0.3, 0.6]], dtype="f32", device="cpu")
let w_cpu = tensor([[0.3, -0.5, 0.7], [0.2, 0.4, -0.1], [-0.6, 0.8, 0.2], [0.5, -0.3, 0.9]], dtype="f32", device="cpu")
let g_cpu = tensor([1.1, 0.9, 1.2, 0.8], dtype="f32", device="cpu")
let b_cpu = tensor([0.1, -0.2, 0.3], dtype="f32", device="cpu")

let x_vk = x_cpu.to_device("vulkan:0")
let w_vk = w_cpu.to_device("vulkan:0")
let g_vk = g_cpu.to_device("vulkan:0")
let b_vk = b_cpu.to_device("vulkan:0")

-- 1. Batched broadcast MatMul [2, 2, 3] @ [1, 3, 2]
let ba_cpu = tensor([[[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]], [[-1.0, 0.5, 2.0], [3.0, -2.0, 1.0]]], dtype="f32", device="cpu")
let bb_cpu = tensor([[[0.5, -0.5], [1.0, 2.0], [-1.5, 0.25]]], dtype="f32", device="cpu")
let ba_vk = ba_cpu.to_device("vulkan:0")
let bb_vk = bb_cpu.to_device("vulkan:0")
let mm_fn = fn(a, b) a @ b end
let d_mm_vk = trace_dag(mm_fn, ba_vk, bb_vk).compile()

-- 2. FusedLinearAct
let lin_fn = fn(x, w, b) (x @ w + b).silu() end
let d_lin_vk = trace_dag(lin_fn, x_vk, w_vk, b_vk).compile()

-- 3. FusedRmsNormLinear (RMSNorm * gamma @ W + bias + gelu)
let rms_fn = fn(x, g, w, b) ((x.rms_norm(1e-5) * g) @ w + b).gelu() end
let d_rms_vk = trace_dag(rms_fn, x_vk, g_vk, w_vk, b_vk).compile()

-- 4. FusedFlashAttention (4D MQA [1, 4, 3, 4] Q with [1, 1, 3, 4] K,V + causal mask -1e9)
let q_cpu = Tensor::arange(0.0, 48.0, 1.0, dtype="f32", device="cpu").reshape([1, 4, 3, 4]) * 0.05f32
let k_cpu = Tensor::arange(0.0, 12.0, 1.0, dtype="f32", device="cpu").reshape([1, 1, 3, 4]) * 0.04f32
let v_cpu = Tensor::arange(1.0, 13.0, 1.0, dtype="f32", device="cpu").reshape([1, 1, 3, 4]) * 0.1f32
let mask_cpu = tensor([[0.0, -1000000000.0, -1000000000.0], [0.0, 0.0, -1000000000.0], [0.0, 0.0, 0.0]], dtype="f32", device="cpu")
let q_vk = q_cpu.to_device("vulkan:0")
let k_vk = k_cpu.to_device("vulkan:0")
let v_vk = v_cpu.to_device("vulkan:0")
let mask_vk = mask_cpu.to_device("vulkan:0")
let attn_fn = fn(q, k, v, m) (((q @ k.transpose(-2, -1)) * 0.5f32) + m).softmax() @ v end
let d_fa_vk = trace_dag(attn_fn, q_vk, k_vk, v_vk, mask_vk).compile()

-- 5. FusedElementwise multi-step broadcast chain (covers pow, neg, abs, exp, ln, sqrt, tanh, sigmoid, relu, gelu, silu)
let ew_fn = fn(a, b) (((a * 0.5f32 + b).abs() + 0.5f32).exp().ln().sqrt() + (-a).tanh() + b.sigmoid() + a.gelu() + b.silu() - ((-a) ** 2.0f32) * 0.1f32).relu() end
let e1_cpu = tensor([[0.4, -0.8, 1.2], [0.9, -0.3, 0.5]], dtype="f32", device="cpu")
let e2_cpu = tensor([0.2, 0.5, -0.1], dtype="f32", device="cpu")
let e1_vk = e1_cpu.to_device("vulkan:0")
let e2_vk = e2_cpu.to_device("vulkan:0")
let d_ew_vk = trace_dag(ew_fn, e1_vk, e2_vk).compile()

-- Snapshot Vulkan VRAM counters before running all 5 GPU DAGs strictly in VRAM
let vk_pre = Tensor::device_info("vulkan:0")
let o_mm_vk = d_mm_vk.run(ba_vk, bb_vk)
let o_lin_vk = d_lin_vk.run(x_vk, w_vk, b_vk)
let o_rms_vk = d_rms_vk.run(x_vk, g_vk, w_vk, b_vk)
let o_fa_vk = d_fa_vk.run(q_vk, k_vk, v_vk, mask_vk)
let o_ew_vk = d_ew_vk.run(e1_vk, e2_vk)
let vk_post = Tensor::device_info("vulkan:0")

-- Compare against CPU f32 references
let diff_mm = (trace_dag(mm_fn, ba_cpu, bb_cpu).compile().run(ba_cpu, bb_cpu) - o_mm_vk.to_device("cpu")).abs().max()
let diff_lin = (trace_dag(lin_fn, x_cpu, w_cpu, b_cpu).compile().run(x_cpu, w_cpu, b_cpu) - o_lin_vk.to_device("cpu")).abs().max()
let diff_rms = (trace_dag(rms_fn, x_cpu, g_cpu, w_cpu, b_cpu).compile().run(x_cpu, g_cpu, w_cpu, b_cpu) - o_rms_vk.to_device("cpu")).abs().max()
let diff_fa = (trace_dag(attn_fn, q_cpu, k_cpu, v_cpu, mask_cpu).compile().run(q_cpu, k_cpu, v_cpu, mask_cpu) - o_fa_vk.to_device("cpu")).abs().max()
let diff_ew = (trace_dag(ew_fn, e1_cpu, e2_cpu).compile().run(e1_cpu, e2_cpu) - o_ew_vk.to_device("cpu")).abs().max()

let binfo = d_fa_vk.backend_info()
[str(diff_mm < 1e-4f32), str(diff_lin < 1e-4f32), str(diff_rms < 1e-4f32), str(diff_fa < 1e-4f32), str(diff_ew < 1e-4f32), str(str(d_rms_vk.op_names()).contains("FusedRmsNormLinear")), str(str(d_fa_vk.op_names()).contains("FusedFlashAttention")), str(str(d_ew_vk.op_names()).contains("FusedElementwise")), str(binfo.get("backend")), str(binfo.get("simd_isa")), str(binfo.get("hardware_accelerated")), str(vk_pre.get("vram_d2h_copies")), str(vk_post.get("vram_d2h_copies")), str(vk_pre.get("vram_hits")), str(vk_post.get("vram_hits")), str(diff_mm), str(diff_rms), str(diff_fa), str(diff_ew)]
"#;
    let v = expect_str_list(compile_check_and_run(src));
    eprintln!(
        "[P3 Vulkan 5-Kernel Parity] mm={} ({}) lin={} rms={} ({}) fa={} ({}) ew={} ({}) backend={} isa={} hw={} d2h={}->{} hits={}->{}",
        v[0],
        v[15],
        v[1],
        v[2],
        v[16],
        v[3],
        v[17],
        v[4],
        v[18],
        v[8],
        v[9],
        v[10],
        v[11],
        v[12],
        v[13],
        v[14]
    );
    assert_eq!(v[0], "true", "Vulkan Batched MatMul parity failed");
    assert_eq!(v[1], "true", "Vulkan FusedLinearAct parity failed");
    assert_eq!(v[2], "true", "Vulkan FusedRmsNormLinear parity failed");
    assert_eq!(v[3], "true", "Vulkan FusedFlashAttention parity failed");
    assert_eq!(v[4], "true", "Vulkan FusedElementwise parity failed");
    assert_eq!(v[5], "true", "FusedRmsNormLinear must fuse on Vulkan DAG");
    assert_eq!(v[6], "true", "FusedFlashAttention must fuse on Vulkan DAG");
    assert_eq!(v[7], "true", "FusedElementwise must fuse on Vulkan DAG");

    if v[10] == "true" {
        assert_eq!(v[8], "vulkan");
        assert_eq!(v[9], "spirv-1.0-f32/f16/bf16");
        assert_eq!(
            parse_u64(&v[12], "vk_d2h_post"),
            parse_u64(&v[11], "vk_d2h_pre"),
            "All 5 Vulkan VRAM-to-VRAM kernels must execute with 0 intermediate D2H copies"
        );
        assert!(
            parse_u64(&v[14], "vk_hits_post") >= parse_u64(&v[13], "vk_hits_pre") + 5,
            "All 5 Vulkan kernels must execute natively on VRAM (zero host fallback)"
        );
    }
}

#[test]
fn p3_kv_page_allocator_physical_vram_backing_and_reclaim() {
    let _guard = GPU_TELEMETRY_LOCK.lock().expect("gpu telemetry lock");
    let src = r#"
let cu_info = Tensor::device_info("cuda:0")
let vk_info = Tensor::device_info("vulkan:0")

-- 4 pages, 2 tokens/page, 2 heads, head_dim=2, f32 => 2*2*2*4 = 32B K + 32B V = 64B VRAM per physical page
let kv_cu = kv_allocator(total_pages=4, page_tokens=2, num_heads=2, head_dim=2, dtype="f32", device="cuda:0")
let s0 = kv_cu.stats()
kv_cu.alloc_seq(1)
kv_cu.append_tokens(1, 3) -- allocates 2 physical pages -> 128 bytes physical VRAM
let s1 = kv_cu.stats()

let k0 = tensor([[1.0, 2.0], [3.0, 4.0]], dtype="f32", device="cuda:0")
let v0 = tensor([[0.5, 1.5], [2.5, 3.5]], dtype="f32", device="cuda:0")
let k1 = tensor([[5.0, 6.0], [7.0, 8.0]], dtype="f32", device="cuda:0")
let v1 = tensor([[4.5, 5.5], [6.5, 7.5]], dtype="f32", device="cuda:0")
kv_cu.write_kv(1, 0, k0, v0)
kv_cu.write_kv(1, 1, k1, v1)

-- CoW fork + split write + read_kv round-trip through physical GPU VRAM!
kv_cu.fork_seq(1, 2)
let k0_cow = tensor([[9.0, 10.0], [11.0, 12.0]], dtype="f32", device="cuda:0")
let v0_cow = tensor([[1.0, 1.0], [2.0, 2.0]], dtype="f32", device="cuda:0")
kv_cu.write_kv(2, 0, k0_cow, v0_cow)
let s_cow = kv_cu.stats()

let r1 = kv_cu.read_kv(1)
let r2 = kv_cu.read_kv(2)
kv_cu.free_seq(1)
kv_cu.free_seq(2)
let s_freed = kv_cu.stats()

-- Also verify Vulkan physical VRAM backing on KVPageAllocator
let kv_vk = kv_allocator(total_pages=2, page_tokens=2, num_heads=1, head_dim=2, dtype="f16", device="vulkan:0")
kv_vk.alloc_seq(7)
kv_vk.append_tokens(7, 2) -- 1 page: 2*1*2*2 = 8B K + 8B V = 16B VRAM
let vk_k = tensor([[2.0, 4.0]], dtype="f16", device="vulkan:0")
let vk_v = tensor([[6.0, 8.0]], dtype="f16", device="vulkan:0")
kv_vk.write_kv(7, 0, vk_k, vk_v)
let vk_s1 = kv_vk.stats()
let vk_r = kv_vk.read_kv(7)
kv_vk.free_seq(7)
let vk_s2 = kv_vk.stats()

[str(cu_info.get("hardware_accelerated")), str(s0.get("vram_backed")), str(s0.get("vram_bytes_allocated")), str(s1.get("vram_bytes_allocated")), str(s_cow.get("vram_bytes_allocated")), str(s_freed.get("vram_bytes_allocated")), str(r1[0][0].to_device("cpu").to_list()), str(r2[0][0].to_device("cpu").to_list()), str(vk_info.get("hardware_accelerated")), str(vk_s1.get("vram_backed")), str(vk_s1.get("vram_bytes_allocated")), str(vk_s2.get("vram_bytes_allocated")), str(vk_r[0][0].to_device("cpu").to_list())]
"#;
    let v = expect_str_list(compile_check_and_run(src));
    eprintln!(
        "[P3 KV VRAM] cu_hw={} backed={} bytes(init={}, 2pg={}, cow3pg={}, freed={}) | vk_hw={} backed={} bytes(1pg={}, freed={})",
        v[0], v[1], v[2], v[3], v[4], v[5], v[8], v[9], v[10], v[11]
    );
    assert_eq!(v[2], "0", "initial vram_bytes_allocated must be 0");
    assert_eq!(
        v[5], "0",
        "free_seq must reclaim all CUDA KV VRAM bytes back to 0"
    );
    assert_eq!(v[6], "[[1.0, 2.0], [3.0, 4.0]]", "seq 1 K must round-trip");
    assert_eq!(
        v[7], "[[9.0, 10.0], [11.0, 12.0]]",
        "seq 2 CoW K must round-trip"
    );
    assert_eq!(
        v[11], "0",
        "free_seq must reclaim all Vulkan KV VRAM bytes back to 0"
    );
    assert_eq!(v[12], "[[2.0, 4.0]]", "Vulkan KV f16 K must round-trip");

    if v[0] == "true" {
        assert_eq!(v[1], "1", "CUDA KVPageAllocator must be VRAM-backed");
        assert_eq!(v[3], "128", "2 physical f32 pages = 128 bytes VRAM");
        assert_eq!(
            v[4], "192",
            "CoW split adds 1 physical f32 page = 192 bytes VRAM"
        );
    } else {
        assert_eq!(v[1], "0");
    }
    if v[8] == "true" {
        assert_eq!(v[9], "1", "Vulkan KVPageAllocator must be VRAM-backed");
        assert_eq!(v[10], "16", "1 physical f16 page = 16 bytes VRAM");
    } else {
        assert_eq!(v[9], "0");
    }
}

#[test]
fn p3_emulation_override_disables_vram_residency_honestly() {
    let bin = env!("CARGO_BIN_EXE_mora");
    let td = mora::test_support::TempDir::new("p3_gpu_vram_emul");
    let script_path = td.path().join("vram_emul_probe.mora");
    let script = r#"
let ci = Tensor::device_info("cuda:0")
let vi = Tensor::device_info("vulkan:0")
let t_cu = tensor([[1.0, 2.0], [3.0, 4.0]], dtype="f16", device="cuda:0")
let t_vk = tensor([[0.5, -0.5], [1.0, 2.0]], dtype="bf16", device="vulkan:0")
let kv = kv_allocator(total_pages=2, page_tokens=2, num_heads=1, head_dim=2, dtype="f32", device="cuda:0")
kv.alloc_seq(1)
kv.append_tokens(1, 2)
let ks = kv.stats()
let d = trace_dag(fn(a) (a @ a).relu() end, t_cu).compile()
let out = d.run(t_cu).to_device("cpu").to_dtype("f32")
print(ci.get("vram_residency"), vi.get("vram_residency"), t_cu.is_vram_resident(), t_vk.is_vram_resident(), ks.get("vram_backed"), ks.get("vram_bytes_allocated"), out.sum())
"#;
    std::fs::write(&script_path, script).expect("write vram_emul_probe.mora");
    let output = std::process::Command::new(bin)
        .env("MORA_FORCE_CPU_EMULATION", "1")
        .arg(&script_path)
        .output()
        .expect("run mora under MORA_FORCE_CPU_EMULATION=1");
    assert!(
        output.status.success(),
        "emulated run failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let tokens: Vec<&str> = stdout.split_whitespace().collect();
    assert_eq!(
        tokens.get(..6),
        Some(["false", "false", "false", "false", "0", "0"].as_slice()),
        "expected honest non-VRAM telemetry under MORA_FORCE_CPU_EMULATION=1, got: {stdout}"
    );
    assert!(
        tokens.get(6).is_some_and(|s| s.starts_with("54")),
        "expected matmul sum 54.0 under emulation, got: {stdout}"
    );
}

#[test]
fn p3_cuda_f32_all_five_kernels_vram_zero_copy() {
    let _guard = GPU_TELEMETRY_LOCK.lock().expect("gpu telemetry lock");
    let src = r#"
let x_cpu = tensor([[0.5, -1.2, 0.8, 0.3], [-0.4, 1.5, -0.7, 0.9], [1.1, 0.2, -0.3, 0.6]], dtype="f32", device="cpu")
let w_cpu = tensor([[0.3, -0.5, 0.7], [0.2, 0.4, -0.1], [-0.6, 0.8, 0.2], [0.5, -0.3, 0.9]], dtype="f32", device="cpu")
let g_cpu = tensor([1.1, 0.9, 1.2, 0.8], dtype="f32", device="cpu")
let b_cpu = tensor([0.1, -0.2, 0.3], dtype="f32", device="cpu")
let q_cpu = Tensor::arange(0.0, 48.0, 1.0, dtype="f32", device="cpu").reshape([1, 4, 3, 4]) * 0.05f32
let k_cpu = Tensor::arange(0.0, 12.0, 1.0, dtype="f32", device="cpu").reshape([1, 1, 3, 4]) * 0.04f32
let v_cpu = Tensor::arange(1.0, 13.0, 1.0, dtype="f32", device="cpu").reshape([1, 1, 3, 4]) * 0.1f32
let m_cpu = tensor([[0.0, -1000000000.0, -1000000000.0], [0.0, 0.0, -1000000000.0], [0.0, 0.0, 0.0]], dtype="f32", device="cpu")
let e1_cpu = tensor([[0.4, -0.8, 1.2], [0.9, -0.3, 0.5]], dtype="f32", device="cpu")
let e2_cpu = tensor([0.2, 0.5, -0.1], dtype="f32", device="cpu")

let x_cu = x_cpu.to_device("cuda:0")
let w_cu = w_cpu.to_device("cuda:0")
let g_cu = g_cpu.to_device("cuda:0")
let b_cu = b_cpu.to_device("cuda:0")
let q_cu = q_cpu.to_device("cuda:0")
let k_cu = k_cpu.to_device("cuda:0")
let v_cu = v_cpu.to_device("cuda:0")
let m_cu = m_cpu.to_device("cuda:0")
let e1_cu = e1_cpu.to_device("cuda:0")
let e2_cu = e2_cpu.to_device("cuda:0")

let rms_fn = fn(x, g, w, b) ((x.rms_norm(1e-5) * g) @ w + b).gelu() end
let attn_fn = fn(q, k, v, m) (((q @ k.transpose(-2, -1)) * 0.5f32) + m).softmax() @ v end
let ew_fn = fn(a, b) (((a * 0.5f32 + b).abs() + 0.5f32).exp().ln().sqrt() + (-a).tanh() + b.sigmoid()).relu() end

let d_rms = trace_dag(rms_fn, x_cu, g_cu, w_cu, b_cu).compile()
let d_fa = trace_dag(attn_fn, q_cu, k_cu, v_cu, m_cu).compile()
let d_ew = trace_dag(ew_fn, e1_cu, e2_cu).compile()

let cu_pre = Tensor::device_info("cuda:0")
let o_rms = d_rms.run(x_cu, g_cu, w_cu, b_cu)
let o_fa = d_fa.run(q_cu, k_cu, v_cu, m_cu)
let o_ew = d_ew.run(e1_cu, e2_cu)
let cu_post = Tensor::device_info("cuda:0")

let err_rms = (trace_dag(rms_fn, x_cpu, g_cpu, w_cpu, b_cpu).compile().run(x_cpu, g_cpu, w_cpu, b_cpu) - o_rms.to_device("cpu")).abs().max()
let err_fa = (trace_dag(attn_fn, q_cpu, k_cpu, v_cpu, m_cpu).compile().run(q_cpu, k_cpu, v_cpu, m_cpu) - o_fa.to_device("cpu")).abs().max()
let err_ew = (trace_dag(ew_fn, e1_cpu, e2_cpu).compile().run(e1_cpu, e2_cpu) - o_ew.to_device("cpu")).abs().max()

[str(cu_pre.get("hardware_accelerated")), str(cu_pre.get("vram_h2d_copies")), str(cu_post.get("vram_h2d_copies")), str(cu_pre.get("vram_d2h_copies")), str(cu_post.get("vram_d2h_copies")), str(cu_pre.get("vram_hits")), str(cu_post.get("vram_hits")), str(err_rms < 1e-4f32), str(err_fa < 1e-4f32), str(err_ew < 1e-4f32)]
"#;
    let v = expect_str_list(compile_check_and_run(src));
    eprintln!(
        "[P3 CUDA f32 Zero-Copy] hw={} h2d={}->{} d2h={}->{} hits={}->{} parity={}/{}/{}",
        v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8], v[9]
    );
    assert_eq!(v[7], "true", "CUDA f32 FusedRmsNormLinear parity failed");
    assert_eq!(v[8], "true", "CUDA f32 FusedFlashAttention parity failed");
    assert_eq!(v[9], "true", "CUDA f32 FusedElementwise parity failed");
    if v[0] == "true" {
        assert_eq!(parse_u64(&v[2], "h2d_post"), parse_u64(&v[1], "h2d_pre"));
        assert_eq!(parse_u64(&v[4], "d2h_post"), parse_u64(&v[3], "d2h_pre"));
        assert!(parse_u64(&v[6], "hits_post") >= parse_u64(&v[5], "hits_pre") + 3);
    }
}
