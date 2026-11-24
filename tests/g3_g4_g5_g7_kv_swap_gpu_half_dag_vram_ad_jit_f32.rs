//! Batch 2 Integration Verification Suite (G3 + G4 + G5 + G7-JIT):
//! - G3: `KVPageAllocator` physical VRAM <-> Host `swap_out` / `swap_in` / `is_swapped`, quota & watermarks.
//! - G4: Native `f16` / `bf16` zero-fallback compute across ALL 5 fused kernels on Vulkan & CUDA (+ Metal MSL).
//! - G5: `DagMemoryPlan` physical GPU VRAM scratch slot pooling (`vram_buffer_reuses > 0`) &
//!   first-class IR-level symbolic `ComputeDag.{vjp, grad, jvp}` (including fusion & higher-order HVP).
//! - G7-JIT: `8×f32` native AVX2 + `1×f32` SSE strict W^X JIT dispatch on `DType::F32` fused DAGs.

use std::sync::Arc;

use mora::common::Device;
use mora::mir::control::{OptLevel, optimize_program};
use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::compute_dag::backend_for_device;
use mora::runtime::vm::execute_program;
use mora::typeck::check_mir::check_program_witnesses;
use mora::value::Value;
use mora::value::tensor::Tensor;

fn compile_check_and_run(src: &str) -> Result<Value, String> {
    let (mut prog, witnesses) = ParserV3::compile(src).map_err(|e| format!("compile: {e}"))?;
    let errs = check_program_witnesses(&witnesses);
    if !errs.is_empty() {
        let formatted = errs
            .iter()
            .map(mora::typeck::format_error)
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!("typeck:\n{formatted}"));
    }
    optimize_program(&mut prog, OptLevel::Basic);
    let mut core = CoreRuntime::default();
    let (val, _) =
        execute_program(Arc::new(prog), &mut core).map_err(|e| format!("runtime: {e}"))?;
    Ok(val)
}

fn as_str_list(v: Value) -> Vec<String> {
    match v {
        Value::List(items) => items
            .iter()
            .map(|x| match x {
                Value::String(s) => s.clone(),
                other => format!("{other}"),
            })
            .collect(),
        other => panic!("expected List, got {other:?}"),
    }
}

fn as_tensor_list(v: Value) -> Vec<Arc<Tensor>> {
    match v {
        Value::List(items) => items
            .iter()
            .map(|x| match x {
                Value::Tensor(t) => Arc::clone(t),
                other => panic!("expected Tensor in list, got {other:?}"),
            })
            .collect(),
        other => panic!("expected List<Tensor>, got {other:?}"),
    }
}

fn assert_tensors_close(actual: &[f64], expected: &[f64], tol: f64, label: &str) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "{label}: length mismatch {} vs {}",
        actual.len(),
        expected.len()
    );
    for (i, (&a, &e)) in actual.iter().zip(expected.iter()).enumerate() {
        let diff = (a - e).abs();
        assert!(
            diff <= tol,
            "{label}[{i}]: actual={a}, expected={e}, diff={diff} > tol={tol}"
        );
    }
}

#[test]
fn g3_kv_cache_swap_out_and_swap_in_roundtrip_quota_watermarks_cpu() {
    let src = r#"
-- 4 physical pages, 2 tokens/page, 2 heads, head_dim=2, f32 (4 bytes/elem => 64 bytes/page)
let kv: kv_allocator = kv_allocator(total_pages=4, page_tokens=2, num_heads=2, head_dim=2, dtype="f32", device="cpu")
let wm_hits = atom(0)
kv.on_watermark(0.5, fn(ev)
  swap(wm_hits, fn(c) c + 1 end)
end)

kv.alloc_seq(1, 5)
let pages_before = kv.append_tokens(1, 3)

let k0 = tensor([[1.5, -2.25], [3.0, 0.5]], dtype="f32", device="cpu")
let v0 = tensor([[0.25, 0.5], [0.75, 1.0]], dtype="f32", device="cpu")
let k1 = tensor([[4.0, 5.5], [-6.0, 7.25]], dtype="f32", device="cpu")
let v1 = tensor([[-1.0, -2.0], [-3.0, -4.0]], dtype="f32", device="cpu")
kv.write_kv(1, 0, k0, v0)
kv.write_kv(1, 1, k1, v1)
let kv_before = kv.read_kv(1)

let released = kv.swap_out(1)
let is_swapped_after_out = kv.is_swapped(1)
let st_out = kv.stats()

-- Restore via swap_in and verify rising-edge 50% watermark fires again + bit-exact K/V roundtrip!
let restored_pages = kv.swap_in(1)
let is_swapped_after_in = kv.is_swapped(1)
let kv_after = kv.read_kv(1)
let st_in = kv.stats()

let k_err = (kv_after[0] - kv_before[0]).abs().sum()
let v_err = (kv_after[1] - kv_before[1]).abs().sum()

[str(pages_before.len()), str(released), str(is_swapped_after_out), str(st_out.get("free_pages")), str(st_out.get("swapped_seqs")), str(st_out.get("swap_outs")), str(st_out.get("swap_out_bytes")), str(restored_pages.len()), str(is_swapped_after_in), str(st_in.get("swap_ins")), str(st_in.get("swap_in_bytes")), str(st_in.get("swapped_seqs")), str(deref(wm_hits)), str(k_err < 1e-6f32), str(v_err < 1e-6f32)]
"#;
    let out = as_str_list(compile_check_and_run(src).expect("G3 CPU swap test must succeed"));
    assert_eq!(out[0], "2");
    assert_eq!(out[1], "2");
    assert_eq!(out[2], "true");
    assert_eq!(out[3], "4");
    assert_eq!(out[4], "1");
    assert_eq!(out[5], "1");
    let swap_out_bytes: i64 = out[6].parse().expect("swap_out_bytes int");
    assert!(swap_out_bytes > 0);
    assert_eq!(out[7], "2");
    assert_eq!(out[8], "false");
    assert_eq!(out[9], "1");
    assert_eq!(out[10], out[6]);
    assert_eq!(out[11], "0");
    let wm_hits: i64 = out[12].parse().expect("wm_hits int");
    assert!(
        wm_hits >= 2,
        "expected rising watermark on append AND swap_in, got {wm_hits}"
    );
    assert_eq!(out[13], "true");
    assert_eq!(out[14], "true");

    // Verify quota enforcement on swap_in rejects cleanly!
    let quota_err = compile_check_and_run(
        r#"
let kv: kv_allocator = kv_allocator(total_pages=4, page_tokens=2, num_heads=2, head_dim=2, dtype="f32", device="cpu")
kv.alloc_seq(1)
kv.append_tokens(1, 3)
kv.swap_out(1)
kv.set_quota(64)
kv.swap_in(1)
"#,
    )
    .expect_err("swap_in exceeding quota must fail");
    assert!(
        quota_err.contains("quota exceeded"),
        "expected quota exceeded error, got: {quota_err}"
    );

    // Verify writing to a swapped-out sequence fails explicitly.
    let err = compile_check_and_run(
        r#"
let kv: kv_allocator = kv_allocator(total_pages=4, page_tokens=2, num_heads=2, head_dim=2, dtype="f32", device="cpu")
kv.alloc_seq(1)
kv.append_tokens(1, 2)
kv.swap_out(1)
kv.append_tokens(1, 1)
"#,
    )
    .expect_err("append_tokens on swapped sequence must fail");
    assert!(
        err.contains("swapped out"),
        "expected swapped out error, got: {err}"
    );
}

#[test]
fn g3_kv_cache_swap_out_releases_physical_gpu_vram_and_restores_on_swap_in() {
    for dev_str in ["cuda:0", "vulkan:0"] {
        let dev = Device::parse(dev_str).expect("valid device");
        if !backend_for_device(&dev).supports_vram_residency() {
            continue;
        }
        let src = format!(
            r#"
let kv: kv_allocator = kv_allocator(total_pages=4, page_tokens=2, num_heads=2, head_dim=2, dtype="f32", device="{dev_str}")
kv.alloc_seq(7, 10)
kv.append_tokens(7, 3)
let k0 = tensor([[1.25, -0.5], [2.0, 3.5]], dtype="f32", device="{dev_str}")
let v0 = tensor([[9.0, 8.0], [7.0, 6.0]], dtype="f32", device="{dev_str}")
let k1 = tensor([[0.5, 1.5], [-2.5, 4.0]], dtype="f32", device="{dev_str}")
let v1 = tensor([[-1.5, -2.5], [-3.5, -4.5]], dtype="f32", device="{dev_str}")
kv.write_kv(7, 0, k0, v0)
kv.write_kv(7, 1, k1, v1)
let before_kv = kv.read_kv(7)
let st_before = kv.stats()
let freed = kv.swap_out(7)
let st_swapped = kv.stats()
kv.swap_in(7)
let st_restored = kv.stats()
let after_kv = kv.read_kv(7)
let k_err = (after_kv[0] - before_kv[0]).abs().sum()
let v_err = (after_kv[1] - before_kv[1]).abs().sum()
[str(st_before.get("vram_backed")), str(st_before.get("vram_bytes_allocated")), str(st_swapped.get("vram_bytes_allocated")), str(st_restored.get("vram_bytes_allocated")), str(freed), str(k_err < 1e-5f32), str(v_err < 1e-5f32)]
"#
        );
        let out = as_str_list(
            compile_check_and_run(&src)
                .unwrap_or_else(|e| panic!("G3 GPU swap on {dev_str} failed: {e}")),
        );
        assert_eq!(out[0], "1", "{dev_str}: expected VRAM backed");
        let vb: i64 = out[1].parse().expect("vram_before int");
        assert!(vb > 0, "{dev_str}: expected positive VRAM before swap_out");
        assert_eq!(out[2], "0", "{dev_str}: expected 0 VRAM after swap_out");
        assert_eq!(
            out[3], out[1],
            "{dev_str}: expected exact VRAM restored after swap_in"
        );
        assert_eq!(out[4], "2");
        assert_eq!(out[5], "true", "{dev_str}: K data mismatch after swap_in");
        assert_eq!(out[6], "true", "{dev_str}: V data mismatch after swap_in");
    }
}

#[test]
fn g4_gpu_native_f16_and_bf16_all_five_fused_kernels_parity() {
    for dev_str in ["vulkan:0", "cuda:0"] {
        let dev = Device::parse(dev_str).expect("valid device");
        let backend = backend_for_device(&dev);
        if !backend.supports_vram_residency() {
            continue;
        }
        for (dt_str, tol) in [("f16", 2.5e-2), ("bf16", 6.0e-2)] {
            let before = backend.vram_telemetry();
            let make_script = |target_dev: &str| {
                format!(
                    r#"
let x = tensor([[0.5, -0.25, 1.0, 0.75, -0.5], [-0.75, 0.5, 0.25, -1.0, 1.25], [0.25, 0.75, -0.5, 0.5, 0.25]], dtype="{dt_str}", device="{target_dev}")
let w = tensor([[0.2, -0.1, 0.3, 0.1, -0.2, 0.15, 0.05], [-0.1, 0.25, -0.15, 0.2, 0.1, -0.05, 0.2], [0.15, 0.05, 0.2, -0.25, 0.3, 0.1, -0.15], [-0.2, 0.15, 0.1, 0.3, -0.1, 0.2, 0.05], [0.1, -0.2, 0.25, -0.05, 0.15, -0.1, 0.3]], dtype="{dt_str}", device="{target_dev}")
let gamma = tensor([1.1, 0.9, 1.05, 0.95, 1.0], dtype="{dt_str}", device="{target_dev}")
let bias = tensor([0.1, -0.1, 0.05, 0.2, -0.05, 0.15, -0.1], dtype="{dt_str}", device="{target_dev}")

-- 1. MatMul (odd dims 3x5 @ 5x7)
let d_mm = trace_dag(fn(a, b) a @ b end, x, w).compile()
let o_mm = d_mm.run(x, w)

-- 2. FusedLinearAct
let d_la = trace_dag(fn(a, b, bi) (a @ b + bi).gelu() end, x, w, bias).compile()
let o_la = d_la.run(x, w, bias)

-- 3. FusedRmsNormLinear (with gamma + bias + silu)
let d_rn = trace_dag(fn(a, g, b, bi) ((a.rms_norm(1e-5) * g) @ b + bi).silu() end, x, gamma, w, bias).compile()
let o_rn = d_rn.run(x, gamma, w, bias)

-- 4. FusedFlashAttention (batched 3D with odd Sq=3, Dk=5, Sk=3, Dv=5 + additive mask)
let q = tensor([[[0.4, -0.2, 0.5, 0.1, -0.3], [-0.1, 0.3, 0.2, -0.4, 0.5], [0.2, 0.1, -0.3, 0.4, 0.2]], [[0.1, 0.5, -0.2, 0.3, 0.1], [0.3, -0.1, 0.4, 0.2, -0.2], [-0.2, 0.4, 0.1, -0.1, 0.3]]], dtype="{dt_str}", device="{target_dev}")
let k = tensor([[[0.2, 0.3, -0.1, 0.4, 0.2], [-0.3, 0.1, 0.5, -0.2, 0.1], [0.4, -0.2, 0.1, 0.3, -0.1]], [[0.3, -0.2, 0.2, 0.1, 0.4], [0.1, 0.4, -0.3, 0.2, 0.1], [-0.1, 0.2, 0.3, -0.2, 0.5]]], dtype="{dt_str}", device="{target_dev}")
let v = tensor([[[1.0, 0.5, -0.5, 0.25, 0.75], [0.25, -0.75, 0.5, 1.0, -0.25], [-0.5, 0.25, 1.0, -0.5, 0.5]], [[0.5, -0.25, 0.75, 0.5, -0.5], [0.75, 0.5, -0.25, -0.5, 1.0], [-0.25, 1.0, 0.25, 0.75, 0.25]]], dtype="{dt_str}", device="{target_dev}")
let mask = tensor([[0.0, -0.5, -1.0], [0.0, 0.0, -0.5], [-0.25, 0.0, 0.0]], dtype="{dt_str}", device="{target_dev}")
let d_fa = trace_dag(fn(qq, kk, vv, mm) (((qq @ kk.transpose(1, 2)) * 0.5{dt_str}) + mm).softmax() @ vv end, q, k, v, mask).compile()
let o_fa = d_fa.run(q, k, v, mask)

-- 5. FusedElementwise (odd 3x5=15 elements, broadcast bias, transcendentals)
let b_row = tensor([0.2, -0.1, 0.3, -0.2, 0.1], dtype="{dt_str}", device="{target_dev}")
let d_ew = trace_dag(fn(a, b) ((a + b) * 0.5{dt_str}).tanh().abs() + 0.25{dt_str} end, x, b_row).compile()
let o_ew = d_ew.run(x, b_row)

[o_mm, o_la, o_rn, o_fa, o_ew]
"#
                )
            };

            let gpu_list = as_tensor_list(
                compile_check_and_run(&make_script(dev_str))
                    .unwrap_or_else(|e| panic!("G4 {dev_str} {dt_str} failed: {e}")),
            );
            let cpu_list = as_tensor_list(
                compile_check_and_run(&make_script("cpu"))
                    .unwrap_or_else(|e| panic!("G4 cpu ref {dt_str} failed: {e}")),
            );
            let after = backend.vram_telemetry();

            let names = ["mm", "la", "rn", "fa", "ew"];
            for (idx, name) in names.iter().enumerate() {
                assert_tensors_close(
                    &gpu_list[idx].to_flat_vec(),
                    &cpu_list[idx].to_flat_vec(),
                    tol,
                    &format!("{dev_str}/{dt_str}/{name}"),
                );
            }

            assert!(
                after.vram_hits >= before.vram_hits + 5,
                "{dev_str}/{dt_str}: expected >=5 VRAM hits, got delta {}",
                after.vram_hits - before.vram_hits
            );
            if dev_str.starts_with("vulkan") {
                if dt_str == "f16" {
                    assert!(
                        after.native_f16_dispatches >= before.native_f16_dispatches + 5,
                        "vulkan/f16: expected >=5 native f16 dispatches, got delta {}",
                        after.native_f16_dispatches - before.native_f16_dispatches
                    );
                } else {
                    assert!(
                        after.native_bf16_dispatches >= before.native_bf16_dispatches + 5,
                        "vulkan/bf16: expected >=5 native bf16 dispatches, got delta {}",
                        after.native_bf16_dispatches - before.native_bf16_dispatches
                    );
                }
            }
        }
    }
}

#[test]
fn g5_dag_memory_plan_reuses_physical_gpu_vram_scratch_slots() {
    for dev_str in ["cuda:0", "vulkan:0"] {
        let dev = Device::parse(dev_str).expect("valid device");
        if !backend_for_device(&dev).supports_vram_residency() {
            continue;
        }
        let src = format!(
            r#"
let x = tensor([[1.0, 2.0, 3.0, 4.0], [-1.0, 0.5, 1.5, -0.5]], dtype="f32", device="{dev_str}")
let w1 = tensor([[0.2, 0.1, -0.1, 0.3], [0.1, 0.2, 0.1, -0.1], [-0.1, 0.1, 0.2, 0.1], [0.2, -0.1, 0.1, 0.2]], dtype="f32", device="{dev_str}")
let w2 = tensor([[0.15, -0.1, 0.2, 0.1], [0.2, 0.15, -0.1, 0.1], [0.1, 0.2, 0.15, -0.1], [-0.1, 0.1, 0.2, 0.15]], dtype="f32", device="{dev_str}")
let w3 = tensor([[0.1, 0.2, -0.1, 0.15], [-0.1, 0.1, 0.2, 0.1], [0.15, -0.1, 0.1, 0.2], [0.2, 0.15, -0.1, 0.1]], dtype="f32", device="{dev_str}")
let w4 = tensor([[0.25, -0.15], [0.1, 0.2], [-0.1, 0.25], [0.2, 0.1]], dtype="f32", device="{dev_str}")

task deep_mlp(a: Tensor<f32, [2, 4], {dev_str}>, m1: Tensor<f32, [4, 4], {dev_str}>, m2: Tensor<f32, [4, 4], {dev_str}>, m3: Tensor<f32, [4, 4], {dev_str}>, m4: Tensor<f32, [4, 2], {dev_str}>): Tensor<f32, [2, 2], {dev_str}>
  let h1 = (a @ m1).relu()
  let h2 = (h1 @ m2).tanh()
  let h3 = (h2 @ m3).silu()
  return h3 @ m4
end

let dag = trace_dag(deep_mlp, x, w1, w2, w3, w4).compile()
let r1 = dag.run(x, w1, w2, w3, w4)
let r2 = dag.run(x, w1, w2, w3, w4)
let r3 = dag.run(x, w1, w2, w3, w4)
let mp = dag.memory_plan()
let diff_12 = (r1 - r2).abs().sum()
let diff_23 = (r2 - r3).abs().sum()

[str(mp.get("total_intermediates")), str(mp.get("vram_peak_slots")), str(mp.get("vram_peak_bytes")), str(mp.get("vram_scratch_allocs")), str(mp.get("vram_buffer_reuses")), str(diff_12 < 1e-6f32), str(diff_23 < 1e-6f32)]
"#
        );
        let out = as_str_list(
            compile_check_and_run(&src)
                .unwrap_or_else(|e| panic!("G5 GPU VRAM scratch reuse on {dev_str} failed: {e}")),
        );
        let total_inter: i64 = out[0].parse().expect("total_intermediates");
        let peak_slots: i64 = out[1].parse().expect("vram_peak_slots");
        let allocs: i64 = out[3].parse().expect("vram_scratch_allocs");
        let reuses: i64 = out[4].parse().expect("vram_buffer_reuses");
        assert!(
            total_inter >= 3,
            "{dev_str}: expected >=3 intermediates, got {total_inter}"
        );
        assert!(
            peak_slots > 0 && peak_slots < total_inter,
            "{dev_str}: expected vram_peak_slots ({peak_slots}) < total_intermediates ({total_inter})"
        );
        assert!(
            allocs <= peak_slots,
            "{dev_str}: VRAM scratch slots must allocate at most once per slot (allocs={allocs}, peak_slots={peak_slots})"
        );
        assert!(
            reuses >= 6,
            "{dev_str}: expected >=6 in-place GPU VRAM buffer reuses across 3 runs, got {reuses}"
        );
        assert_eq!(out[5], "true");
        assert_eq!(out[6], "true");
    }
}

#[test]
fn g5_ir_level_symbolic_dag_vjp_grad_jvp_and_second_order_hvp_parity() {
    let src = r#"
let x = tensor([[0.8, -0.4, 0.6], [-0.5, 0.9, 0.3]], dtype="f64", device="cpu")
let w = tensor([[0.5, -0.3, 0.2], [0.1, 0.6, -0.4], [-0.2, 0.4, 0.7]], dtype="f64", device="cpu")
let b = tensor([0.1, -0.2, 0.15], dtype="f64", device="cpu")

task scalar_loss(a: Tensor<f64, [2, 3], cpu>, m: Tensor<f64, [3, 3], cpu>, bias: Tensor<f64, [3], cpu>): float
  let h = (a.rms_norm(1e-5) @ m + bias).gelu()
  let p = h.softmax()
  return (p * h).sum()
end

let fwd = trace_dag(scalar_loss, x, w, b)

-- Symbolic IR-level grad w.r.t. x (input 0) AND w (input 1) — unfused, fused, and from fused!
let gx_dag = fwd.grad(0)
let gx_fused = gx_dag.compile()
let gx_from_fused = fwd.compile().grad(0).compile()
let gw_dag = fwd.grad(1).compile()

let gx = gx_dag.run(x, w, b)
let gx_f = gx_fused.run(x, w, b)
let gx_ff = gx_from_fused.run(x, w, b)
let gw = gw_dag.run(x, w, b)

-- Compare against central finite differences for x and w
let eps = 1e-6
let e00 = tensor([[eps, 0.0, 0.0], [0.0, 0.0, 0.0]], dtype="f64", device="cpu")
let fd_00 = (fwd.run(x + e00, w, b) - fwd.run(x - e00, w, b)).sum() / (2.0 * eps)

let e12 = tensor([[0.0, 0.0, 0.0], [0.0, 0.0, eps]], dtype="f64", device="cpu")
let fd_12 = (fwd.run(x + e12, w, b) - fwd.run(x - e12, w, b)).sum() / (2.0 * eps)

let ew01 = tensor([[0.0, eps, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]], dtype="f64", device="cpu")
let fd_w01 = (fwd.run(x, w + ew01, b) - fwd.run(x, w - ew01, b)).sum() / (2.0 * eps)

let gx_list = gx.to_list()
let gw_list = gw.to_list()
let gx_00 = gx_list[0][0]
let gx_12 = gx_list[1][2]
let gw_01 = gw_list[0][1]

-- Symbolic VJP vs JVP adjointness identity on vector-valued function:
--   dot(jvp(x, dx), dy) == dot(dx, vjp(x, dy))
task vec_block(a: Tensor<f64, [2, 3], cpu>, m: Tensor<f64, [3, 3], cpu>, bias: Tensor<f64, [3], cpu>): Tensor<f64, [2, 3], cpu>
  return (a @ m + bias).silu() + a.layer_norm(1e-5)
end
let vec_dag = trace_dag(vec_block, x, w, b)
let dx = tensor([[0.3, -0.7, 0.2], [0.5, 0.1, -0.4]], dtype="f64", device="cpu")
let dy = tensor([[-0.2, 0.6, 0.8], [0.4, -0.3, 0.5]], dtype="f64", device="cpu")

let jvp_dag = vec_dag.jvp(0).compile()
let vjp_dag = vec_dag.vjp(0).compile()
let jvp_out = jvp_dag.run(x, w, b, dx)
let vjp_out = vjp_dag.run(x, w, b, dy)
let lhs_adj = (jvp_out * dy).sum()
let rhs_adj = (dx * vjp_out).sum()

-- Higher-order symbolic IR-level HVP: `grad(0).jvp(0)` on cubic scalar function `sum(x^3 + 2*x^2)`
-- Exact Hessian-vector product is `(6*x + 4) * v`!
let xv = tensor([1.5, -2.0, 0.5, 3.0], dtype="f64", device="cpu")
let vv = tensor([0.5, 1.0, -2.0, 0.25], dtype="f64", device="cpu")
let cubic = trace_dag(fn(t) (t * t * t + t * t * 2.0).sum() end, xv)
let hvp_dag = cubic.grad(0).jvp(0).compile()
let hvp_out = hvp_dag.run(xv, vv)
let hvp_exact = (xv * 6.0 + 4.0) * vv
let hvp_err = (hvp_out - hvp_exact).abs().sum()

-- Relu & Abs exact zero-safe derivatives (`StepPos` / `Sign`)
let rz = tensor([-1.5, 0.0, 2.0], dtype="f64", device="cpu")
let relu_g = trace_dag(fn(t) t.relu().sum() end, rz).grad(0).run(rz)
let abs_g = trace_dag(fn(t) t.abs().sum() end, rz).grad(0).run(rz)

[str((gx - gx_f).abs().sum() < 1e-10), str((gx - gx_ff).abs().sum() < 1e-10), str((gx_00 - fd_00) * (gx_00 - fd_00) < 1e-10), str((gx_12 - fd_12) * (gx_12 - fd_12) < 1e-10), str((gw_01 - fd_w01) * (gw_01 - fd_w01) < 1e-10), str((lhs_adj - rhs_adj) * (lhs_adj - rhs_adj) < 1e-18), str(hvp_err < 1e-10), str(vjp_dag.is_fused()), str(vjp_dag.node_count()), str(relu_g.to_list()), str(abs_g.to_list())]
"#;
    let out = as_str_list(
        compile_check_and_run(src)
            .expect("G5 IR-level symbolic VJP/Grad/JVP/HVP test must succeed"),
    );
    assert_eq!(out[0], "true", "gx vs fused gx mismatch");
    assert_eq!(out[1], "true", "gx vs unfused-from-fused gx mismatch");
    assert_eq!(out[2], "true", "fd_00 mismatch");
    assert_eq!(out[3], "true", "fd_12 mismatch");
    assert_eq!(out[4], "true", "fd_w01 mismatch");
    assert_eq!(out[5], "true", "VJP/JVP adjointness mismatch");
    assert_eq!(out[6], "true", "second-order symbolic HVP mismatch");
    assert_eq!(out[7], "true", "vjp_dag should be fused");
    let vjp_nodes: i64 = out[8].parse().expect("vjp_nodes int");
    assert!(vjp_nodes > 0);
    assert_eq!(out[9], "[0.0, 0.0, 1.0]");
    assert_eq!(out[10], "[-1.0, 0.0, 1.0]");
}

#[test]
fn g7_native_jit_8xf32_avx2_dag_dispatch_and_parity() {
    let src = r#"
let x = tensor([[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0], [-2.0, 0.5, 1.5, -0.25, 3.25, 4.5, -1.0, 2.5, 0.75, 1.25, 6.0, 0.0, 3.0], [0.25, 1.75, 2.25, 3.5, -0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 0.5, 2.5, 1.5]], dtype="f32", device="cpu")
let y = tensor([[0.5, 1.5, 2.0, 0.25, 1.0, 0.5, 2.0, 1.5, 0.25, 0.5, 1.0, 2.0, 0.5], [1.0, 2.0, 0.5, 4.0, 0.25, 1.0, 3.0, 0.5, 2.0, 1.5, 0.25, 1.0, 2.0], [2.0, 0.5, 1.0, 0.5, 2.0, 3.0, 1.5, 0.5, 0.25, 0.125, 4.0, 0.5, 1.0]], dtype="f32", device="cpu")

let dag = trace_dag(fn(a, b) ((a * b + a - 0.25f32).relu().sqrt() + (a / 2.0f32).abs()) * 1.5f32 end, x, y).compile()

let out = dag.run(x, y)
let eager = ((x * y + x - 0.25f32).relu().sqrt() + (x / 2.0f32).abs()) * 1.5f32
let err = (out - eager).abs().sum()
let rep = dag.cost_report()

[str(err < 1e-4f32), str(rep.get("native_jit_isa")), str(rep.get("native_jit_f32_dispatches")), str(rep.get("native_jit_eligible_nodes"))]
"#;
    let out =
        as_str_list(compile_check_and_run(src).expect("G7 8xf32 native JIT DAG test must succeed"));
    assert_eq!(out[0], "true", "f32 native JIT output mismatch");
    let eligible: i64 = out[3].parse().expect("eligible int");
    assert!(eligible >= 1);
    if out[1] == "x86_64-avx2-jit" {
        let f32_dispatches: i64 = out[2].parse().expect("f32_dispatches int");
        assert!(
            f32_dispatches > 0,
            "expected native_jit_f32_dispatches > 0 on x86_64 AVX2 host"
        );
    }
}
