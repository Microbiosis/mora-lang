//! Wave 4 P1: Hardware-SIMD (`AVX2+FMA` / `NEON`) & Multi-Threaded Cache-Blocked CPU Kernels.
//!
//! Architecture (`AGENTS.md §0`):
//! - Zero external crates — pure Rust stable `std::arch` intrinsics with runtime ISA dispatch
//!   plus `std::thread::scope` structured parallelism (zero thread-spawn overhead on small tensors).
//! - Cache-blocked (`MC=64, KC=64, NC=64`) tiled GEMM micro-kernels driving:
//!   1. General 2D & batched ND `MatMul`
//!   2. `FusedLinearAct` (`MatMul + Bias + Act`)
//!   3. `FusedRmsNormLinear` (`RMSNorm [+ gamma] + MatMul [+ Bias] [+ Act]`) with zero `[M, K]` heap intermediate
//!   4. `FusedFlashAttention` (FlashAttention-2 tiled `Br=32, Bc=32` online softmax with `O(Br*Bc)` tile scratch,
//!      zero `O(Sq*Sk)` score matrix materialization, GQA/MQA batch broadcasting, and additive/causal masks)
//!   5. Multi-threaded fused elementwise execution.

use crate::common::DType;
use crate::error::{MoraError, MoraResult};
use crate::runtime::autograd::read_broadcast_flat;
use crate::runtime::compute_dag::{FusedAct, FusedEwStep};
use crate::value::tensor::{broadcast_runtime_shapes, quantize_for_dtype};

/// L1/L2 cache blocking dimensions for tiled GEMM micro-kernels.
const TILE_M: usize = 64;
const TILE_K: usize = 64;
const TILE_N: usize = 64;

/// FlashAttention-2 query/key-value tile sizes (`Br = 32`, `Bc = 32`).
const FA_BR: usize = 32;
const FA_BC: usize = 32;

/// Minimum operation count before fanning out across `std::thread::scope` workers.
const PARALLEL_FLOP_THRESHOLD: usize = 32_768;
const PARALLEL_EW_THRESHOLD: usize = 16_384;
const MAX_CPU_WORKERS: usize = 8;

/// Report the active hardware SIMD instruction set selected at runtime.
pub(crate) fn simd_isa_name() -> &'static str {
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
            return "avx2+fma";
        }
        if std::is_x86_feature_detected!("sse2") {
            return "sse2";
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        return "neon";
    }
    #[allow(unreachable_code)]
    "scalar-unrolled"
}

/// Number of CPU worker threads available for parallel compute-graph kernels (capped to avoid oversubscription).
pub(crate) fn effective_worker_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, MAX_CPU_WORKERS)
}

// ════════════════════════════════════════════════════════════════════════════
// 1. Hardware SIMD Micro-Kernels (`axpy`, `dot`, `sum_sq`, `scale`)
// ════════════════════════════════════════════════════════════════════════════

/// Compute `dst[j] += a * src[j]` for `j in 0..len` using runtime-dispatched SIMD FMA.
#[inline]
pub(crate) fn axpy_f64(dst: &mut [f64], a: f64, src: &[f64]) {
    debug_assert_eq!(dst.len(), src.len());
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
            // SAFETY: CPU feature detection guarantees AVX2 + FMA support and slices have equal length.
            unsafe { axpy_f64_avx2_fma(dst, a, src) };
            return;
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: NEON is mandatory on aarch64 and slices have equal length.
        unsafe { axpy_f64_neon(dst, a, src) };
        return;
    }
    #[allow(unreachable_code)]
    axpy_f64_scalar(dst, a, src);
}

/// Compute inner product `sum_j a[j] * b[j]` using runtime-dispatched SIMD FMA.
#[inline]
pub(crate) fn dot_f64(a: &[f64], b: &[f64]) -> f64 {
    debug_assert_eq!(a.len(), b.len());
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
            // SAFETY: CPU feature detection guarantees AVX2 + FMA support and slices have equal length.
            return unsafe { dot_f64_avx2_fma(a, b) };
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: NEON is mandatory on aarch64 and slices have equal length.
        return unsafe { dot_f64_neon(a, b) };
    }
    #[allow(unreachable_code)]
    dot_f64_scalar(a, b)
}

/// Compute `sum_j a[j] * a[j]` using runtime-dispatched SIMD FMA.
#[inline]
pub(crate) fn sum_sq_f64(a: &[f64]) -> f64 {
    dot_f64(a, a)
}

/// Compute `dst[j] *= scale` in place using runtime-dispatched SIMD.
#[inline]
pub(crate) fn scale_f64(dst: &mut [f64], scale: f64) {
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            // SAFETY: CPU feature detection guarantees AVX2 support.
            unsafe { scale_f64_avx2(dst, scale) };
            return;
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: NEON is mandatory on aarch64.
        unsafe { scale_f64_neon(dst, scale) };
        return;
    }
    #[allow(unreachable_code)]
    for x in dst.iter_mut() {
        *x *= scale;
    }
}

#[inline]
fn axpy_f64_scalar(dst: &mut [f64], a: f64, src: &[f64]) {
    let n = dst.len();
    let mut j = 0;
    while j + 4 <= n {
        dst[j] += a * src[j];
        dst[j + 1] += a * src[j + 1];
        dst[j + 2] += a * src[j + 2];
        dst[j + 3] += a * src[j + 3];
        j += 4;
    }
    while j < n {
        dst[j] += a * src[j];
        j += 1;
    }
}

#[inline]
fn dot_f64_scalar(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len();
    let mut acc0 = 0.0;
    let mut acc1 = 0.0;
    let mut acc2 = 0.0;
    let mut acc3 = 0.0;
    let mut j = 0;
    while j + 4 <= n {
        acc0 += a[j] * b[j];
        acc1 += a[j + 1] * b[j + 1];
        acc2 += a[j + 2] * b[j + 2];
        acc3 += a[j + 3] * b[j + 3];
        j += 4;
    }
    let mut total = (acc0 + acc1) + (acc2 + acc3);
    while j < n {
        total += a[j] * b[j];
        j += 1;
    }
    total
}

// SAFETY: Caller must verify AVX2 + FMA support at runtime and `dst.len() == src.len()`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn axpy_f64_avx2_fma(dst: &mut [f64], a: f64, src: &[f64]) {
    use std::arch::x86_64::{_mm256_fmadd_pd, _mm256_loadu_pd, _mm256_set1_pd, _mm256_storeu_pd};
    let n = dst.len().min(src.len());
    let d_ptr = dst.as_mut_ptr();
    let s_ptr = src.as_ptr();
    let mut j = 0usize;
    // SAFETY: Loop bounds ensure every unaligned 4-lane (`f64x4`) load/store stays within `0..n`,
    // and AVX2+FMA target features are enabled by this function's contract.
    unsafe {
        let va = _mm256_set1_pd(a);
        while j + 8 <= n {
            let vd0 = _mm256_loadu_pd(d_ptr.add(j));
            let vs0 = _mm256_loadu_pd(s_ptr.add(j));
            let vr0 = _mm256_fmadd_pd(va, vs0, vd0);
            _mm256_storeu_pd(d_ptr.add(j), vr0);

            let vd1 = _mm256_loadu_pd(d_ptr.add(j + 4));
            let vs1 = _mm256_loadu_pd(s_ptr.add(j + 4));
            let vr1 = _mm256_fmadd_pd(va, vs1, vd1);
            _mm256_storeu_pd(d_ptr.add(j + 4), vr1);
            j += 8;
        }
        while j + 4 <= n {
            let vd = _mm256_loadu_pd(d_ptr.add(j));
            let vs = _mm256_loadu_pd(s_ptr.add(j));
            let vr = _mm256_fmadd_pd(va, vs, vd);
            _mm256_storeu_pd(d_ptr.add(j), vr);
            j += 4;
        }
    }
    while j < n {
        dst[j] += a * src[j];
        j += 1;
    }
}

// SAFETY: Caller must verify AVX2 + FMA support at runtime and `a.len() == b.len()`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn dot_f64_avx2_fma(a: &[f64], b: &[f64]) -> f64 {
    use std::arch::x86_64::{
        _mm256_add_pd, _mm256_fmadd_pd, _mm256_loadu_pd, _mm256_setzero_pd, _mm256_storeu_pd,
    };
    let n = a.len().min(b.len());
    let a_ptr = a.as_ptr();
    let b_ptr = b.as_ptr();
    let mut j = 0usize;
    let mut lanes = [0.0_f64; 4];
    // SAFETY: Loop bounds guarantee every 4-lane load stays inside `0..n` and `lanes` holds 4 `f64`s.
    unsafe {
        let mut acc0 = _mm256_setzero_pd();
        let mut acc1 = _mm256_setzero_pd();
        while j + 8 <= n {
            let va0 = _mm256_loadu_pd(a_ptr.add(j));
            let vb0 = _mm256_loadu_pd(b_ptr.add(j));
            acc0 = _mm256_fmadd_pd(va0, vb0, acc0);

            let va1 = _mm256_loadu_pd(a_ptr.add(j + 4));
            let vb1 = _mm256_loadu_pd(b_ptr.add(j + 4));
            acc1 = _mm256_fmadd_pd(va1, vb1, acc1);
            j += 8;
        }
        while j + 4 <= n {
            let va = _mm256_loadu_pd(a_ptr.add(j));
            let vb = _mm256_loadu_pd(b_ptr.add(j));
            acc0 = _mm256_fmadd_pd(va, vb, acc0);
            j += 4;
        }
        let sum = _mm256_add_pd(acc0, acc1);
        _mm256_storeu_pd(lanes.as_mut_ptr(), sum);
    }
    let mut total = (lanes[0] + lanes[1]) + (lanes[2] + lanes[3]);
    while j < n {
        total += a[j] * b[j];
        j += 1;
    }
    total
}

// SAFETY: Caller must verify AVX2 support at runtime.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn scale_f64_avx2(dst: &mut [f64], scale: f64) {
    use std::arch::x86_64::{_mm256_loadu_pd, _mm256_mul_pd, _mm256_set1_pd, _mm256_storeu_pd};
    let n = dst.len();
    let ptr = dst.as_mut_ptr();
    let mut j = 0usize;
    // SAFETY: Loop condition `j + 4 <= n` keeps every 4-lane unaligned load/store within `dst`.
    unsafe {
        let vs = _mm256_set1_pd(scale);
        while j + 4 <= n {
            let v = _mm256_loadu_pd(ptr.add(j));
            _mm256_storeu_pd(ptr.add(j), _mm256_mul_pd(v, vs));
            j += 4;
        }
    }
    while j < n {
        dst[j] *= scale;
        j += 1;
    }
}

// SAFETY: NEON is architectural on aarch64; caller ensures `dst.len() == src.len()`.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn axpy_f64_neon(dst: &mut [f64], a: f64, src: &[f64]) {
    use std::arch::aarch64::{vdupq_n_f64, vfmaq_f64, vld1q_f64, vst1q_f64};
    let n = dst.len().min(src.len());
    let d_ptr = dst.as_mut_ptr();
    let s_ptr = src.as_ptr();
    let mut j = 0usize;
    // SAFETY: Loop condition `j + 2 <= n` keeps every 2-lane NEON load/store within `0..n`.
    unsafe {
        let va = vdupq_n_f64(a);
        while j + 2 <= n {
            let vd = vld1q_f64(d_ptr.add(j));
            let vs = vld1q_f64(s_ptr.add(j));
            vst1q_f64(d_ptr.add(j), vfmaq_f64(vd, va, vs));
            j += 2;
        }
    }
    while j < n {
        dst[j] += a * src[j];
        j += 1;
    }
}

// SAFETY: NEON is architectural on aarch64; caller ensures `a.len() == b.len()`.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn dot_f64_neon(a: &[f64], b: &[f64]) -> f64 {
    use std::arch::aarch64::{vaddvq_f64, vdupq_n_f64, vfmaq_f64, vld1q_f64};
    let n = a.len().min(b.len());
    let a_ptr = a.as_ptr();
    let b_ptr = b.as_ptr();
    let mut j = 0usize;
    // SAFETY: Loop condition `j + 2 <= n` keeps every 2-lane NEON load within `0..n`.
    let mut total = unsafe {
        let mut acc = vdupq_n_f64(0.0);
        while j + 2 <= n {
            let va = vld1q_f64(a_ptr.add(j));
            let vb = vld1q_f64(b_ptr.add(j));
            acc = vfmaq_f64(acc, va, vb);
            j += 2;
        }
        vaddvq_f64(acc)
    };
    while j < n {
        total += a[j] * b[j];
        j += 1;
    }
    total
}

// SAFETY: NEON is architectural on aarch64.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn scale_f64_neon(dst: &mut [f64], scale: f64) {
    use std::arch::aarch64::{vdupq_n_f64, vld1q_f64, vmulq_f64, vst1q_f64};
    let n = dst.len();
    let ptr = dst.as_mut_ptr();
    let mut j = 0usize;
    // SAFETY: Loop condition `j + 2 <= n` keeps every 2-lane NEON load/store within `dst`.
    unsafe {
        let vs = vdupq_n_f64(scale);
        while j + 2 <= n {
            let v = vld1q_f64(ptr.add(j));
            vst1q_f64(ptr.add(j), vmulq_f64(v, vs));
            j += 2;
        }
    }
    while j < n {
        dst[j] *= scale;
        j += 1;
    }
}

// ════════════════════════════════════════════════════════════════════════════
// 2. Cache-Blocked Multi-Threaded GEMM & Fused Linear / RMSNorm+Linear
// ════════════════════════════════════════════════════════════════════════════

/// Cache-blocked (`TILE_M x TILE_K x TILE_N`) GEMM accumulating `C += A @ B` on a contiguous row slice.
fn gemm_block_rows(
    a_rows: &[f64],
    b: &[f64],
    c_rows: &mut [f64],
    m_rows: usize,
    k: usize,
    n: usize,
) {
    c_rows.fill(0.0);
    for i0 in (0..m_rows).step_by(TILE_M) {
        let i_end = (i0 + TILE_M).min(m_rows);
        for p0 in (0..k).step_by(TILE_K) {
            let p_end = (p0 + TILE_K).min(k);
            for j0 in (0..n).step_by(TILE_N) {
                let j_end = (j0 + TILE_N).min(n);
                for i in i0..i_end {
                    let c_slice = &mut c_rows[i * n + j0..i * n + j_end];
                    let a_base = i * k;
                    for p in p0..p_end {
                        let a_val = a_rows[a_base + p];
                        let b_slice = &b[p * n + j0..p * n + j_end];
                        axpy_f64(c_slice, a_val, b_slice);
                    }
                }
            }
        }
    }
}

/// Multi-threaded cache-blocked SIMD GEMM writing `A[m, k] @ B[k, n]` into `out[0..m*n]`.
pub(crate) fn gemm_tiled_f64(a: &[f64], b: &[f64], out: &mut [f64], m: usize, k: usize, n: usize) {
    debug_assert_eq!(a.len(), m * k);
    debug_assert_eq!(b.len(), k * n);
    debug_assert_eq!(out.len(), m * n);
    let workers = effective_worker_threads();
    let flops = m.saturating_mul(k).saturating_mul(n);
    if workers <= 1 || m < 2 || flops < PARALLEL_FLOP_THRESHOLD {
        gemm_block_rows(a, b, out, m, k, n);
        return;
    }
    let chunk_rows = m.div_ceil(workers).max(1);
    std::thread::scope(|s| {
        for (chunk_idx, out_chunk) in out.chunks_mut(chunk_rows * n).enumerate() {
            let row_start = chunk_idx * chunk_rows;
            let rows_here = out_chunk.len() / n;
            let a_chunk = &a[row_start * k..(row_start + rows_here) * k];
            s.spawn(move || {
                gemm_block_rows(a_chunk, b, out_chunk, rows_here, k, n);
            });
        }
    });
}

/// Multi-threaded cache-blocked SIMD general MatMul supporting 1D, 2D, and batched ND contractions.
pub(crate) fn matmul_simd_into(
    a_vals: &[f64],
    b_vals: &[f64],
    a_shape: &[usize],
    b_shape: &[usize],
    out: &mut Vec<f64>,
) -> MoraResult<()> {
    match (a_shape, b_shape) {
        ([k1], [k2]) if k1 == k2 => {
            out.clear();
            out.push(dot_f64(a_vals, b_vals));
            Ok(())
        }
        ([m, k1], [k2]) if k1 == k2 => {
            out.resize(*m, 0.0);
            for (i, slot) in out.iter_mut().enumerate() {
                *slot = dot_f64(&a_vals[i * k1..(i + 1) * k1], b_vals);
            }
            Ok(())
        }
        ([k1], [k2, n]) if k1 == k2 => {
            out.resize(*n, 0.0);
            out.fill(0.0);
            for p in 0..*k1 {
                axpy_f64(out, a_vals[p], &b_vals[p * n..(p + 1) * n]);
            }
            Ok(())
        }
        ([m, k1], [k2, n]) if k1 == k2 => {
            out.resize(m * n, 0.0);
            gemm_tiled_f64(a_vals, b_vals, out, *m, *k1, *n);
            Ok(())
        }
        (sa, sb) if sa.len() >= 2 && sb.len() >= 2 => {
            let m = sa[sa.len() - 2];
            let k = sa[sa.len() - 1];
            let k2 = sb[sb.len() - 2];
            let n = sb[sb.len() - 1];
            if k != k2 {
                return Err(MoraError::Runtime(format!(
                    "matmul shape mismatch: {:?} @ {:?}",
                    sa, sb
                )));
            }
            let batch_a = &sa[..sa.len() - 2];
            let batch_b = &sb[..sb.len() - 2];
            let batch_out = broadcast_runtime_shapes(batch_a, batch_b)?;
            let batch_count: usize = batch_out.iter().copied().product::<usize>().max(1);
            let mat_a = m * k;
            let mat_b = k * n;
            let mat_o = m * n;
            out.resize(batch_count * mat_o, 0.0);

            let workers = effective_worker_threads();
            let total_flops = batch_count
                .saturating_mul(m)
                .saturating_mul(k)
                .saturating_mul(n);
            if workers > 1 && batch_count >= 2 && total_flops >= PARALLEL_FLOP_THRESHOLD {
                let b_chunk = batch_count.div_ceil(workers).max(1);
                let batch_out_ref = batch_out.as_slice();
                std::thread::scope(|s| {
                    for (c_idx, o_chunk) in out.chunks_mut(b_chunk * mat_o).enumerate() {
                        let b_start = c_idx * b_chunk;
                        let count_here = o_chunk.len() / mat_o;
                        s.spawn(move || {
                            for local_b in 0..count_here {
                                let b_idx = b_start + local_b;
                                let a_base =
                                    map_broadcast_batch(b_idx, batch_out_ref, batch_a) * mat_a;
                                let b_base =
                                    map_broadcast_batch(b_idx, batch_out_ref, batch_b) * mat_b;
                                let o_slice = &mut o_chunk[local_b * mat_o..(local_b + 1) * mat_o];
                                gemm_block_rows(
                                    &a_vals[a_base..a_base + mat_a],
                                    &b_vals[b_base..b_base + mat_b],
                                    o_slice,
                                    m,
                                    k,
                                    n,
                                );
                            }
                        });
                    }
                });
            } else {
                for b_idx in 0..batch_count {
                    let a_base = map_broadcast_batch(b_idx, &batch_out, batch_a) * mat_a;
                    let b_base = map_broadcast_batch(b_idx, &batch_out, batch_b) * mat_b;
                    let o_slice = &mut out[b_idx * mat_o..(b_idx + 1) * mat_o];
                    gemm_tiled_f64(
                        &a_vals[a_base..a_base + mat_a],
                        &b_vals[b_base..b_base + mat_b],
                        o_slice,
                        m,
                        k,
                        n,
                    );
                }
            }
            Ok(())
        }
        _ => Err(MoraError::Runtime(format!(
            "unsupported matmul shapes {:?} @ {:?}",
            a_shape, b_shape
        ))),
    }
}

#[inline]
fn map_broadcast_batch(mut b_lin: usize, batch_out: &[usize], target_batch: &[usize]) -> usize {
    if target_batch.is_empty() {
        return 0;
    }
    let rdiff = batch_out.len() - target_batch.len();
    let mut idx = 0usize;
    let mut stride = 1usize;
    for i in (0..batch_out.len()).rev() {
        let dim = batch_out[i];
        let coord = b_lin % dim;
        b_lin /= dim;
        if i >= rdiff {
            let ta = i - rdiff;
            let td = target_batch[ta];
            let tc = if td == 1 { 0 } else { coord };
            idx += tc * stride;
            stride *= td;
        }
    }
    idx
}

/// Apply optional broadcast bias + activation + dtype quantization to rows `[row_start..row_start+rows_here]`.
fn apply_linear_epilogue(
    out_chunk: &mut [f64],
    row_start: usize,
    n: usize,
    bias: Option<(&[f64], &[usize])>,
    act: FusedAct,
    out_shape: &[usize],
    dtype: DType,
) {
    let simple_1d_bias = bias.and_then(|(bv, bs)| {
        if (bs.len() == 1 && bs[0] == n) || (bs.len() == 2 && bs[0] == 1 && bs[1] == n) {
            Some(bv)
        } else {
            None
        }
    });
    let rows_here = out_chunk.len() / n;
    for local_i in 0..rows_here {
        let global_i = row_start + local_i;
        let row = &mut out_chunk[local_i * n..(local_i + 1) * n];
        if let Some(bv) = simple_1d_bias {
            for (v, &b) in row.iter_mut().zip(bv.iter()) {
                *v = quantize_for_dtype(act.eval(*v + b), dtype);
            }
        } else if let Some((bv, bs)) = bias {
            let base = global_i * n;
            for (j, v) in row.iter_mut().enumerate() {
                let b = read_broadcast_flat(bv, bs, out_shape, base + j);
                *v = quantize_for_dtype(act.eval(*v + b), dtype);
            }
        } else {
            for v in row.iter_mut() {
                *v = quantize_for_dtype(act.eval(*v), dtype);
            }
        }
    }
}

/// Execute `FusedLinearAct`: `Act(X @ W + bias)` supporting 2D `[M, K] @ [K, N]` and ND `[..., M, K] @ [K, N]`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fused_linear_act_into(
    x_vals: &[f64],
    w_vals: &[f64],
    bias: Option<(&[f64], &[usize])>,
    act: FusedAct,
    m_total: usize,
    k: usize,
    n: usize,
    out_shape: &[usize],
    dtype: DType,
    out: &mut Vec<f64>,
) {
    out.resize(m_total * n, 0.0);
    let workers = effective_worker_threads();
    let flops = m_total.saturating_mul(k).saturating_mul(n);
    if workers <= 1 || m_total < 2 || flops < PARALLEL_FLOP_THRESHOLD {
        gemm_block_rows(x_vals, w_vals, out, m_total, k, n);
        apply_linear_epilogue(out, 0, n, bias, act, out_shape, dtype);
        return;
    }

    let chunk_rows = m_total.div_ceil(workers).max(1);
    std::thread::scope(|s| {
        for (chunk_idx, out_chunk) in out.chunks_mut(chunk_rows * n).enumerate() {
            let row_start = chunk_idx * chunk_rows;
            let rows_here = out_chunk.len() / n;
            let x_chunk = &x_vals[row_start * k..(row_start + rows_here) * k];
            s.spawn(move || {
                gemm_block_rows(x_chunk, w_vals, out_chunk, rows_here, k, n);
                apply_linear_epilogue(out_chunk, row_start, n, bias, act, out_shape, dtype);
            });
        }
    });
}

/// Execute `FusedRmsNormLinear`: `Act((RMSNorm(X, eps) [* gamma]) @ W [+ bias])`
/// with cache-local tile normalization (zero `[M_total, K]` intermediate tensor allocation!).
#[allow(clippy::too_many_arguments)]
pub(crate) fn fused_rms_norm_linear_into(
    x_vals: &[f64],
    eps: f64,
    gamma: Option<(&[f64], &[usize])>,
    w_vals: &[f64],
    bias: Option<(&[f64], &[usize])>,
    act: FusedAct,
    m_total: usize,
    k: usize,
    n: usize,
    x_shape: &[usize],
    out_shape: &[usize],
    dtype: DType,
    out: &mut Vec<f64>,
) {
    out.resize(m_total * n, 0.0);
    let run_rows = |x_chunk: &[f64], out_chunk: &mut [f64], row_start: usize, rows_here: usize| {
        let k_f = k.max(1) as f64;
        // Stack/tile scratch holding at most TILE_M normalized rows (fits in L1/L2, zero full-tensor alloc).
        let tile_rows = TILE_M.min(rows_here).max(1);
        let mut norm_tile = vec![0.0_f64; tile_rows * k];
        let simple_gamma = gamma.and_then(|(gv, gs)| {
            if (gs.len() == 1 && gs[0] == k) || (gs.len() == 2 && gs[0] == 1 && gs[1] == k) {
                Some(gv)
            } else {
                None
            }
        });

        for local_i0 in (0..rows_here).step_by(tile_rows) {
            let cur_rows = (local_i0 + tile_rows).min(rows_here) - local_i0;
            for r in 0..cur_rows {
                let row_in = &x_chunk[(local_i0 + r) * k..(local_i0 + r + 1) * k];
                let ms = sum_sq_f64(row_in) / k_f;
                let inv_rms = 1.0 / (ms + eps).sqrt();
                let dst = &mut norm_tile[r * k..(r + 1) * k];
                if let Some(gv) = simple_gamma {
                    for ((d, &xv), &g) in dst.iter_mut().zip(row_in.iter()).zip(gv.iter()) {
                        *d = xv * inv_rms * g;
                    }
                } else if let Some((gv, gs)) = gamma {
                    let global_base = (row_start + local_i0 + r) * k;
                    for (c, (d, &xv)) in dst.iter_mut().zip(row_in.iter()).enumerate() {
                        let g = read_broadcast_flat(gv, gs, x_shape, global_base + c);
                        *d = xv * inv_rms * g;
                    }
                } else {
                    for (d, &xv) in dst.iter_mut().zip(row_in.iter()) {
                        *d = xv * inv_rms;
                    }
                }
            }
            let out_tile = &mut out_chunk[local_i0 * n..(local_i0 + cur_rows) * n];
            gemm_block_rows(&norm_tile[..cur_rows * k], w_vals, out_tile, cur_rows, k, n);
            apply_linear_epilogue(
                out_tile,
                row_start + local_i0,
                n,
                bias,
                act,
                out_shape,
                dtype,
            );
        }
    };

    let workers = effective_worker_threads();
    let flops = m_total.saturating_mul(k).saturating_mul(n);
    if workers <= 1 || m_total < 2 || flops < PARALLEL_FLOP_THRESHOLD {
        run_rows(x_vals, out, 0, m_total);
        return;
    }

    let chunk_rows = m_total.div_ceil(workers).max(1);
    std::thread::scope(|s| {
        for (chunk_idx, out_chunk) in out.chunks_mut(chunk_rows * n).enumerate() {
            let row_start = chunk_idx * chunk_rows;
            let rows_here = out_chunk.len() / n;
            let x_chunk = &x_vals[row_start * k..(row_start + rows_here) * k];
            s.spawn(move || {
                run_rows(x_chunk, out_chunk, row_start, rows_here);
            });
        }
    });
}

// ════════════════════════════════════════════════════════════════════════════
// 3. Tiled Online-Softmax FlashAttention-2 Kernel (`Br=32, Bc=32`)
// ════════════════════════════════════════════════════════════════════════════

/// Execute `FusedFlashAttention`:
/// `Softmax((Q @ K^T) * scale [+ mask]) @ V` via tiled FlashAttention-2 online softmax.
///
/// Memory complexity is `O(Br * (Bc + Dv))` scratch per thread — never materializes the
/// `[..., Sq, Sk]` attention matrix. Supports 2D/3D/4D/ND tensors, GQA/MQA batch broadcasting,
/// `k_is_transposed = false` (K is `[..., Sk, D]`) or `true` (K is `[..., D, Sk]`), and optional
/// broadcast additive mask.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fused_flash_attention_into(
    q_vals: &[f64],
    k_vals: &[f64],
    v_vals: &[f64],
    mask: Option<(&[f64], &[usize])>,
    q_shape: &[usize],
    k_shape: &[usize],
    v_shape: &[usize],
    out_shape: &[usize],
    scale: f64,
    k_is_transposed: bool,
    dtype: DType,
    out: &mut Vec<f64>,
) -> MoraResult<()> {
    fused_flash_attention_with_lse_into(
        q_vals,
        k_vals,
        v_vals,
        mask,
        q_shape,
        k_shape,
        v_shape,
        out_shape,
        scale,
        k_is_transposed,
        dtype,
        out,
        None,
    )
}

/// Execute `FusedFlashAttention` and optionally emit per-row log-sum-exp
/// `L[b, i] = m_i + ln(l_i)` (`shape = [...batch_out, Sq]`, `O(Sq)` memory)
/// for tiled FlashAttention-2 backward without ever materializing `O(Sq * Sk)`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fused_flash_attention_with_lse_into(
    q_vals: &[f64],
    k_vals: &[f64],
    v_vals: &[f64],
    mask: Option<(&[f64], &[usize])>,
    q_shape: &[usize],
    k_shape: &[usize],
    v_shape: &[usize],
    out_shape: &[usize],
    scale: f64,
    k_is_transposed: bool,
    dtype: DType,
    out: &mut Vec<f64>,
    mut lse_out: Option<&mut Vec<f64>>,
) -> MoraResult<()> {
    let q_rank = q_shape.len();
    let k_rank = k_shape.len();
    let v_rank = v_shape.len();
    if q_rank < 2 || k_rank < 2 || v_rank < 2 {
        return Err(MoraError::Runtime(
            "FusedFlashAttention requires rank >= 2 tensors for Q, K, V".to_string(),
        ));
    }

    let s_q = q_shape[q_rank - 2];
    let d_k = q_shape[q_rank - 1];
    let (s_k, k_d) = if k_is_transposed {
        (k_shape[k_rank - 1], k_shape[k_rank - 2])
    } else {
        (k_shape[k_rank - 2], k_shape[k_rank - 1])
    };
    let v_sk = v_shape[v_rank - 2];
    let d_v = v_shape[v_rank - 1];
    if d_k != k_d || s_k != v_sk {
        return Err(MoraError::Runtime(format!(
            "FusedFlashAttention shape mismatch: Q={q_shape:?}, K={k_shape:?}, V={v_shape:?}"
        )));
    }

    let batch_q = &q_shape[..q_rank - 2];
    let batch_k = &k_shape[..k_rank - 2];
    let batch_v = &v_shape[..v_rank - 2];
    let batch_out = &out_shape[..out_shape.len() - 2];
    let batch_count: usize = batch_out.iter().copied().product::<usize>().max(1);
    let mut score_shape = batch_out.to_vec();
    score_shape.push(s_q);
    score_shape.push(s_k);

    let mat_q = s_q * d_k;
    let mat_k = s_k * d_k;
    let mat_v = s_k * d_v;
    let mat_o = s_q * d_v;
    out.resize(batch_count * mat_o, 0.0);
    if let Some(ref mut lse) = lse_out {
        lse.resize(batch_count * s_q, f64::NEG_INFINITY);
    }

    let run_one_batch = |b_idx: usize, o_slice: &mut [f64], mut lse_slice: Option<&mut [f64]>| {
        let q_base = map_broadcast_batch(b_idx, batch_out, batch_q) * mat_q;
        let k_base = map_broadcast_batch(b_idx, batch_out, batch_k) * mat_k;
        let v_base = map_broadcast_batch(b_idx, batch_out, batch_v) * mat_v;
        let q_mat = &q_vals[q_base..q_base + mat_q];
        let k_mat = &k_vals[k_base..k_base + mat_k];
        let v_mat = &v_vals[v_base..v_base + mat_v];

        // Reusable per-batch tile scratch (never O(s_q * s_k)!).
        let mut s_tile = [0.0_f64; FA_BR * FA_BC];
        let mut p_row = [0.0_f64; FA_BC];
        let mut o_tile = vec![0.0_f64; FA_BR * d_v];
        let mut m_running = [f64::NEG_INFINITY; FA_BR];
        let mut l_running = [0.0_f64; FA_BR];
        // When K is pre-transposed [D, Sk], materialize one [Bc, D] tile for contiguous SIMD dot!
        let mut k_col_tile = if k_is_transposed {
            vec![0.0_f64; FA_BC * d_k]
        } else {
            Vec::new()
        };

        for q0 in (0..s_q).step_by(FA_BR) {
            let br = (q0 + FA_BR).min(s_q) - q0;
            for r in 0..br {
                m_running[r] = f64::NEG_INFINITY;
                l_running[r] = 0.0;
            }
            o_tile[..br * d_v].fill(0.0);

            for k0 in (0..s_k).step_by(FA_BC) {
                let bc = (k0 + FA_BC).min(s_k) - k0;

                if k_is_transposed {
                    for c in 0..bc {
                        let dst = &mut k_col_tile[c * d_k..(c + 1) * d_k];
                        for d in 0..d_k {
                            dst[d] = k_mat[d * s_k + (k0 + c)];
                        }
                    }
                }

                // 1. Compute score tile S[r, c] = (Q[q0+r] . K[k0+c]) * scale + mask
                for r in 0..br {
                    let q_row = &q_mat[(q0 + r) * d_k..(q0 + r + 1) * d_k];
                    let s_row = &mut s_tile[r * bc..(r + 1) * bc];
                    for (c, s_slot) in s_row.iter_mut().enumerate() {
                        let k_row = if k_is_transposed {
                            &k_col_tile[c * d_k..(c + 1) * d_k]
                        } else {
                            &k_mat[(k0 + c) * d_k..(k0 + c + 1) * d_k]
                        };
                        let mut score = dot_f64(q_row, k_row) * scale;
                        if let Some((mv, ms)) = mask {
                            let flat_score_idx = (b_idx * s_q + (q0 + r)) * s_k + (k0 + c);
                            score += read_broadcast_flat(mv, ms, &score_shape, flat_score_idx);
                        }
                        *s_slot = score;
                    }
                }

                // 2. Online softmax rescaling & V accumulation (FlashAttention-2 inner loop)
                for r in 0..br {
                    let s_row = &s_tile[r * bc..(r + 1) * bc];
                    let mut m_tilde = f64::NEG_INFINITY;
                    for &sv in s_row {
                        if sv > m_tilde {
                            m_tilde = sv;
                        }
                    }
                    let m_prev = m_running[r];
                    let m_new = m_prev.max(m_tilde);
                    if m_new == f64::NEG_INFINITY {
                        continue;
                    }
                    let alpha = if m_prev == f64::NEG_INFINITY {
                        0.0
                    } else {
                        (m_prev - m_new).exp()
                    };

                    let mut l_tilde = 0.0;
                    for c in 0..bc {
                        let p = (s_row[c] - m_new).exp();
                        p_row[c] = p;
                        l_tilde += p;
                    }

                    let o_acc = &mut o_tile[r * d_v..(r + 1) * d_v];
                    if alpha != 1.0 {
                        scale_f64(o_acc, alpha);
                    }
                    for (c, &p_val) in p_row.iter().enumerate().take(bc) {
                        if p_val != 0.0 {
                            let v_row = &v_mat[(k0 + c) * d_v..(k0 + c + 1) * d_v];
                            axpy_f64(o_acc, p_val, v_row);
                        }
                    }

                    l_running[r] = l_running[r] * alpha + l_tilde;
                    m_running[r] = m_new;
                }
            }

            // 3. Final normalization, optional LSE save, & dtype quantization into output tile
            for r in 0..br {
                if let Some(ref mut lse) = lse_slice {
                    lse[q0 + r] = if l_running[r] > 0.0 && m_running[r].is_finite() {
                        m_running[r] + l_running[r].ln()
                    } else {
                        f64::NEG_INFINITY
                    };
                }
                let inv_l = if l_running[r] > 0.0 {
                    1.0 / l_running[r]
                } else {
                    0.0
                };
                let src = &o_tile[r * d_v..(r + 1) * d_v];
                let dst = &mut o_slice[(q0 + r) * d_v..(q0 + r + 1) * d_v];
                for (d_slot, &ov) in dst.iter_mut().zip(src.iter()) {
                    *d_slot = quantize_for_dtype(ov * inv_l, dtype);
                }
            }
        }
    };

    let workers = effective_worker_threads();
    let total_work = batch_count
        .saturating_mul(s_q)
        .saturating_mul(s_k)
        .saturating_mul(d_k.max(d_v));
    if workers > 1 && batch_count >= 2 && total_work >= PARALLEL_FLOP_THRESHOLD {
        let b_chunk = batch_count.div_ceil(workers).max(1);
        std::thread::scope(|s| {
            if let Some(lse_vec) = lse_out.as_deref_mut() {
                for (c_idx, (o_chunk, l_chunk)) in out
                    .chunks_mut(b_chunk * mat_o)
                    .zip(lse_vec.chunks_mut(b_chunk * s_q))
                    .enumerate()
                {
                    let b_start = c_idx * b_chunk;
                    let count_here = o_chunk.len() / mat_o;
                    s.spawn(move || {
                        for local_b in 0..count_here {
                            let o_slice = &mut o_chunk[local_b * mat_o..(local_b + 1) * mat_o];
                            let l_slice = &mut l_chunk[local_b * s_q..(local_b + 1) * s_q];
                            run_one_batch(b_start + local_b, o_slice, Some(l_slice));
                        }
                    });
                }
            } else {
                for (c_idx, o_chunk) in out.chunks_mut(b_chunk * mat_o).enumerate() {
                    let b_start = c_idx * b_chunk;
                    let count_here = o_chunk.len() / mat_o;
                    s.spawn(move || {
                        for local_b in 0..count_here {
                            let o_slice = &mut o_chunk[local_b * mat_o..(local_b + 1) * mat_o];
                            run_one_batch(b_start + local_b, o_slice, None);
                        }
                    });
                }
            }
        });
    } else {
        for b_idx in 0..batch_count {
            let o_slice = &mut out[b_idx * mat_o..(b_idx + 1) * mat_o];
            let l_slice = lse_out
                .as_deref_mut()
                .map(|v| &mut v[b_idx * s_q..(b_idx + 1) * s_q]);
            run_one_batch(b_idx, o_slice, l_slice);
        }
    }

    Ok(())
}

// ════════════════════════════════════════════════════════════════════════════
// 4. Multi-Threaded Fused Elementwise Execution
// ════════════════════════════════════════════════════════════════════════════

fn eval_ew_chunk(
    input_flats: &[&[f64]],
    input_shapes: &[&[usize]],
    steps: &[FusedEwStep],
    out_shape: &[usize],
    dtype: DType,
    start_idx: usize,
    out_chunk: &mut [f64],
) -> MoraResult<()> {
    let n_in = input_flats.len();
    let mut reg_buf = vec![0.0_f64; n_in + steps.len()];
    for (offset, slot) in out_chunk.iter_mut().enumerate() {
        let elem_idx = start_idx + offset;
        for i in 0..n_in {
            reg_buf[i] = read_broadcast_flat(input_flats[i], input_shapes[i], out_shape, elem_idx);
        }
        for (s_idx, step) in steps.iter().enumerate() {
            let val = match *step {
                FusedEwStep::Add(a, b) => reg_buf[a] + reg_buf[b],
                FusedEwStep::Sub(a, b) => reg_buf[a] - reg_buf[b],
                FusedEwStep::Mul(a, b) => reg_buf[a] * reg_buf[b],
                FusedEwStep::Div(a, b) => {
                    if reg_buf[b] == 0.0 {
                        return Err(MoraError::Runtime(
                            "ComputeDag fused division by zero".to_string(),
                        ));
                    }
                    reg_buf[a] / reg_buf[b]
                }
                FusedEwStep::Pow(a, b) => reg_buf[a].powf(reg_buf[b]),
                FusedEwStep::AddScalar(a, c) => reg_buf[a] + c,
                FusedEwStep::SubScalar(a, c, rev) => {
                    if rev {
                        c - reg_buf[a]
                    } else {
                        reg_buf[a] - c
                    }
                }
                FusedEwStep::MulScalar(a, c) => reg_buf[a] * c,
                FusedEwStep::DivScalar(a, c, rev) => {
                    if rev {
                        if reg_buf[a] == 0.0 {
                            return Err(MoraError::Runtime(
                                "ComputeDag fused division by zero".to_string(),
                            ));
                        }
                        c / reg_buf[a]
                    } else {
                        if c == 0.0 {
                            return Err(MoraError::Runtime(
                                "ComputeDag fused division by zero".to_string(),
                            ));
                        }
                        reg_buf[a] / c
                    }
                }
                FusedEwStep::PowScalar(a, c, rev) => {
                    if rev {
                        c.powf(reg_buf[a])
                    } else {
                        reg_buf[a].powf(c)
                    }
                }
                FusedEwStep::Neg(a) => -reg_buf[a],
                FusedEwStep::Relu(a) => FusedAct::Relu.eval(reg_buf[a]),
                FusedEwStep::Gelu(a) => FusedAct::Gelu.eval(reg_buf[a]),
                FusedEwStep::Silu(a) => FusedAct::Silu.eval(reg_buf[a]),
                FusedEwStep::Tanh(a) => FusedAct::Tanh.eval(reg_buf[a]),
                FusedEwStep::Sigmoid(a) => FusedAct::Sigmoid.eval(reg_buf[a]),
                FusedEwStep::Exp(a) => reg_buf[a].exp(),
                FusedEwStep::Ln(a) => reg_buf[a].ln(),
                FusedEwStep::Sqrt(a) => reg_buf[a].sqrt(),
                FusedEwStep::Sin(a) => reg_buf[a].sin(),
                FusedEwStep::Cos(a) => reg_buf[a].cos(),
                FusedEwStep::Abs(a) => reg_buf[a].abs(),
                FusedEwStep::StepPos(a) => {
                    if reg_buf[a] > 0.0 {
                        1.0
                    } else {
                        0.0
                    }
                }
                FusedEwStep::Sign(a) => {
                    let x = reg_buf[a];
                    if x > 0.0 {
                        1.0
                    } else if x < 0.0 {
                        -1.0
                    } else {
                        0.0
                    }
                }
            };
            reg_buf[n_in + s_idx] = val;
        }
        *slot = quantize_for_dtype(reg_buf[n_in + steps.len() - 1], dtype);
    }
    Ok(())
}

/// Multi-threaded cache-local fused elementwise evaluation writing directly into `out`.
pub(crate) fn fused_elementwise_into(
    input_flats: &[&[f64]],
    input_shapes: &[&[usize]],
    steps: &[FusedEwStep],
    out_shape: &[usize],
    out_numel: usize,
    dtype: DType,
    out: &mut Vec<f64>,
) -> MoraResult<()> {
    out.resize(out_numel, 0.0);
    let workers = effective_worker_threads();
    if workers <= 1 || out_numel < PARALLEL_EW_THRESHOLD {
        return eval_ew_chunk(input_flats, input_shapes, steps, out_shape, dtype, 0, out);
    }
    let chunk_size = out_numel.div_ceil(workers).max(1);
    let mut results: Vec<MoraResult<()>> = Vec::with_capacity(workers);
    std::thread::scope(|s| {
        let mut handles = Vec::new();
        for (c_idx, out_chunk) in out.chunks_mut(chunk_size).enumerate() {
            let start_idx = c_idx * chunk_size;
            handles.push(s.spawn(move || {
                eval_ew_chunk(
                    input_flats,
                    input_shapes,
                    steps,
                    out_shape,
                    dtype,
                    start_idx,
                    out_chunk,
                )
            }));
        }
        for h in handles {
            match h.join() {
                Ok(res) => results.push(res),
                Err(_) => results.push(Err(MoraError::Runtime(
                    "ComputeDag parallel elementwise worker panicked".to_string(),
                ))),
            }
        }
    });
    for res in results {
        res?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simd_microkernels_match_scalar_across_tail_lengths() {
        for len in [0usize, 1, 2, 3, 4, 5, 7, 8, 9, 15, 16, 33, 129] {
            let a: Vec<f64> = (0..len).map(|i| (i as f64) * 0.13 - 1.7).collect();
            let b: Vec<f64> = (0..len).map(|i| (i as f64) * -0.07 + 0.9).collect();

            let expected_dot: f64 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
            let got_dot = dot_f64(&a, &b);
            assert!(
                (got_dot - expected_dot).abs() < 1e-10,
                "dot mismatch at len={len}: got={got_dot}, exp={expected_dot}"
            );

            let expected_sq: f64 = a.iter().map(|x| x * x).sum();
            let got_sq = sum_sq_f64(&a);
            assert!(
                (got_sq - expected_sq).abs() < 1e-10,
                "sum_sq mismatch at len={len}: got={got_sq}, exp={expected_sq}"
            );

            let mut dst_simd = a.clone();
            let mut dst_ref = a.clone();
            axpy_f64(&mut dst_simd, 2.35, &b);
            for (d, &bv) in dst_ref.iter_mut().zip(b.iter()) {
                *d += 2.35 * bv;
            }
            for (i, (&s, &r)) in dst_simd.iter().zip(dst_ref.iter()).enumerate() {
                assert!(
                    (s - r).abs() < 1e-11,
                    "axpy mismatch at len={len} idx={i}: got={s}, exp={r}"
                );
            }

            let mut scaled = b.clone();
            scale_f64(&mut scaled, -0.42);
            for (i, (&s, &orig)) in scaled.iter().zip(b.iter()).enumerate() {
                assert!(
                    (s - orig * -0.42).abs() < 1e-12,
                    "scale mismatch at len={len} idx={i}"
                );
            }
        }
    }

    #[test]
    fn tiled_multithreaded_gemm_matches_naive_on_non_aligned_shapes() {
        let (m, k, n) = (67usize, 71usize, 65usize);
        let a: Vec<f64> = (0..m * k)
            .map(|i| ((i % 29) as f64 - 14.0) * 0.05)
            .collect();
        let b: Vec<f64> = (0..k * n)
            .map(|i| ((i % 31) as f64 - 15.0) * 0.04)
            .collect();
        let mut got = vec![0.0; m * n];
        gemm_tiled_f64(&a, &b, &mut got, m, k, n);

        for i in 0..m {
            for j in 0..n {
                let mut acc = 0.0;
                for p in 0..k {
                    acc += a[i * k + p] * b[p * n + j];
                }
                let diff = (got[i * n + j] - acc).abs();
                assert!(
                    diff < 1e-10,
                    "tiled GEMM mismatch at ({i},{j}): got={}, exp={acc}, diff={diff}",
                    got[i * n + j]
                );
            }
        }
    }

    #[test]
    fn flash_attention_handles_fully_masked_neg_inf_rows_without_nan() {
        let q = vec![1.0, 2.0, 3.0, 4.0]; // [2, 2]
        let k = vec![0.5, -0.5, 1.0, 0.25]; // [2, 2]
        let v = vec![10.0, 20.0, 30.0, 40.0]; // [2, 2]
        // Row 0 completely masked with -inf; Row 1 unmasked.
        let mask = vec![f64::NEG_INFINITY, f64::NEG_INFINITY, 0.0, 0.0];
        let mut out = Vec::new();
        fused_flash_attention_into(
            &q,
            &k,
            &v,
            Some((&mask, &[2, 2])),
            &[2, 2],
            &[2, 2],
            &[2, 2],
            &[2, 2],
            0.5,
            false,
            DType::F64,
            &mut out,
        )
        .expect("flash attention");
        assert!(
            out.iter().all(|x| !x.is_nan()),
            "must never produce NaN: {out:?}"
        );
        assert_eq!(out[0], 0.0);
        assert_eq!(out[1], 0.0);
        assert!(out[2] > 10.0 && out[2] < 30.0);
    }
}
