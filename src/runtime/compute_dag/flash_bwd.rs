//! Tiled FlashAttention-2 Symbolic Backward Kernel (Dao 2023 Algorithm 2).
//!
//! Computes exact `(dQ, dK, dV, dMask)` using only per-row log-sum-exp `L[...batch, Sq]` (`O(Sq)` memory)
//! and fixed `Br × Bc` (`32 × 32`) stack tiles — NEVER materializing the `O(Sq * Sk)` attention score
//! or probability matrices. Supports 2D/3D/4D/ND tensors, GQA/MQA batch broadcasting, pre-transposed
//! `K` (`k_is_transposed = true`), optional broadcast additive masks, and all Mora floating-point dtypes.

use crate::common::DType;
use crate::error::{MoraError, MoraResult};
use crate::value::tensor::{checked_numel, quantize_for_dtype};

const FA_BWD_BR: usize = 32;
const FA_BWD_BC: usize = 32;

/// Selects which gradient tensor a `DagOp::FlashAttentionGrad` node outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlashAttnGradTarget {
    DQ,
    DK,
    DV,
    DMask,
}

impl FlashAttnGradTarget {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::DQ => "FlashAttentionGrad(dQ)",
            Self::DK => "FlashAttentionGrad(dK)",
            Self::DV => "FlashAttentionGrad(dV)",
            Self::DMask => "FlashAttentionGrad(dMask)",
        }
    }

    pub(crate) fn tag(self) -> u8 {
        match self {
            Self::DQ => 0,
            Self::DK => 1,
            Self::DV => 2,
            Self::DMask => 3,
        }
    }
}

/// Cached result of one tiled FlashAttention-2 backward pass shared across sibling
/// `FlashAttentionGrad` nodes (`dQ`, `dK`, `dV`, and optional `dMask`).
#[derive(Debug, Clone)]
pub(crate) struct FlashBwdResult {
    pub(crate) dq: Vec<f64>,
    pub(crate) dk: Vec<f64>,
    pub(crate) dv: Vec<f64>,
    pub(crate) dmask: Option<Vec<f64>>,
}

#[inline]
fn dot_f64_local(a: &[f64], b: &[f64]) -> f64 {
    debug_assert_eq!(a.len(), b.len());
    let mut sum = 0.0;
    for (&x, &y) in a.iter().zip(b.iter()) {
        sum += x * y;
    }
    sum
}

#[inline]
fn axpy_f64_local(dst: &mut [f64], alpha: f64, x: &[f64]) {
    debug_assert_eq!(dst.len(), x.len());
    for (d, &xv) in dst.iter_mut().zip(x.iter()) {
        *d += alpha * xv;
    }
}

fn map_broadcast_batch_local(flat_idx: usize, out_batch: &[usize], in_batch: &[usize]) -> usize {
    if in_batch.is_empty() {
        return 0;
    }
    let out_rank = out_batch.len();
    let in_rank = in_batch.len();
    let mut rem = flat_idx;
    let mut in_flat = 0usize;
    let mut in_stride = 1usize;
    for d in (0..out_rank).rev() {
        let dim = out_batch[d];
        let coord = rem % dim;
        rem /= dim;
        let from_right = out_rank - 1 - d;
        if from_right < in_rank {
            let in_dim = in_batch[in_rank - 1 - from_right];
            let in_coord = if in_dim == 1 { 0 } else { coord };
            in_flat += in_coord * in_stride;
            in_stride *= in_dim;
        }
    }
    in_flat
}

#[inline]
fn map_broadcast_flat_index(in_shape: &[usize], out_shape: &[usize], out_flat: usize) -> usize {
    if in_shape.is_empty() {
        return 0;
    }
    let out_rank = out_shape.len();
    let in_rank = in_shape.len();
    let mut rem = out_flat;
    let mut in_flat = 0usize;
    let mut in_stride = 1usize;
    for d in (0..out_rank).rev() {
        let out_dim = out_shape[d];
        let coord = rem % out_dim;
        rem /= out_dim;
        let from_right = out_rank - 1 - d;
        if from_right < in_rank {
            let in_dim = in_shape[in_rank - 1 - from_right];
            let in_coord = if in_dim == 1 { 0 } else { coord };
            in_flat += in_coord * in_stride;
            in_stride *= in_dim;
        }
    }
    in_flat
}

/// Compute exact tiled FlashAttention-2 backward `(dQ, dK, dV, dMask)` using only `O(Br * Bc)`
/// stack scratch and saved per-row log-sum-exp `lse`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fused_flash_attention_backward(
    q_vals: &[f64],
    k_vals: &[f64],
    v_vals: &[f64],
    mask: Option<(&[f64], &[usize])>,
    out_vals: &[f64],
    lse_vals: &[f64],
    dout_vals: &[f64],
    q_shape: &[usize],
    k_shape: &[usize],
    v_shape: &[usize],
    out_shape: &[usize],
    scale: f64,
    k_is_transposed: bool,
    dtype: DType,
) -> MoraResult<FlashBwdResult> {
    let q_rank = q_shape.len();
    let k_rank = k_shape.len();
    let v_rank = v_shape.len();
    if q_rank < 2 || k_rank < 2 || v_rank < 2 || out_shape.len() < 2 {
        return Err(MoraError::Runtime(
            "FlashAttentionGrad requires rank >= 2 tensors for Q, K, V, Out".to_string(),
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
            "FlashAttentionGrad shape mismatch: Q={q_shape:?}, K={k_shape:?}, V={v_shape:?}"
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

    let mut dq = vec![0.0_f64; checked_numel(q_shape)?];
    let mut dk = vec![0.0_f64; checked_numel(k_shape)?];
    let mut dv = vec![0.0_f64; checked_numel(v_shape)?];
    let mut dmask = match mask {
        Some((_, ms)) => Some(vec![0.0_f64; checked_numel(ms)?]),
        None => None,
    };

    // Reusable O(Sq) row dot buffer D_i = sum_d (dO[i, d] * O[i, d]) and O(Bc * D) K tile.
    let mut row_d = vec![0.0_f64; s_q];
    let mut k_tile = vec![0.0_f64; FA_BWD_BC * d_k];
    let mut dk_tile = vec![0.0_f64; FA_BWD_BC * d_k];
    let mut dv_tile = vec![0.0_f64; FA_BWD_BC * d_v];
    let mut s_tile = [0.0_f64; FA_BWD_BR * FA_BWD_BC];

    for b_idx in 0..batch_count {
        let q_base = map_broadcast_batch_local(b_idx, batch_out, batch_q) * mat_q;
        let k_base = map_broadcast_batch_local(b_idx, batch_out, batch_k) * mat_k;
        let v_base = map_broadcast_batch_local(b_idx, batch_out, batch_v) * mat_v;
        let o_base = b_idx * mat_o;
        let lse_base = b_idx * s_q;

        let q_mat = &q_vals[q_base..q_base + mat_q];
        let k_mat = &k_vals[k_base..k_base + mat_k];
        let v_mat = &v_vals[v_base..v_base + mat_v];
        let o_mat = &out_vals[o_base..o_base + mat_o];
        let do_mat = &dout_vals[o_base..o_base + mat_o];
        let lse_row = &lse_vals[lse_base..lse_base + s_q];

        // 1. Precompute D_i = <dO_i, O_i> in O(Sq * Dv) time, O(Sq) memory.
        for i in 0..s_q {
            let o_r = &o_mat[i * d_v..(i + 1) * d_v];
            let do_r = &do_mat[i * d_v..(i + 1) * d_v];
            row_d[i] = dot_f64_local(do_r, o_r);
        }

        // 2. Outer loop over KV blocks [k0..k0+bc], inner loop over Q blocks [q0..q0+br].
        for k0 in (0..s_k).step_by(FA_BWD_BC) {
            let bc = (k0 + FA_BWD_BC).min(s_k) - k0;
            // Pack K block [bc, d_k] contiguously for cache-local dots & axpys.
            if k_is_transposed {
                for c in 0..bc {
                    let dst = &mut k_tile[c * d_k..(c + 1) * d_k];
                    for d in 0..d_k {
                        dst[d] = k_mat[d * s_k + (k0 + c)];
                    }
                }
            } else {
                k_tile[..bc * d_k].copy_from_slice(&k_mat[k0 * d_k..(k0 + bc) * d_k]);
            }
            dk_tile[..bc * d_k].fill(0.0);
            dv_tile[..bc * d_v].fill(0.0);

            for q0 in (0..s_q).step_by(FA_BWD_BR) {
                let br = (q0 + FA_BWD_BR).min(s_q) - q0;

                // Recompute S_tile[r, c] = (Q[q0+r] . K[k0+c]) * scale + mask (O(Br * Bc) stack!)
                for r in 0..br {
                    let q_row = &q_mat[(q0 + r) * d_k..(q0 + r + 1) * d_k];
                    let s_row = &mut s_tile[r * bc..(r + 1) * bc];
                    for (c, s_slot) in s_row.iter_mut().enumerate() {
                        let k_row = &k_tile[c * d_k..(c + 1) * d_k];
                        let mut score = dot_f64_local(q_row, k_row) * scale;
                        if let Some((mv, ms)) = mask {
                            let flat_score_idx = (b_idx * s_q + (q0 + r)) * s_k + (k0 + c);
                            let m_idx = map_broadcast_flat_index(ms, &score_shape, flat_score_idx);
                            score += mv[m_idx];
                        }
                        *s_slot = score;
                    }
                }

                // Accumulate dV, dQ, dK, and optional dMask on the fly without storing P or dS!
                for r in 0..br {
                    let i = q0 + r;
                    let l_i = lse_row[i];
                    let d_i = row_d[i];
                    let q_row = &q_mat[i * d_k..(i + 1) * d_k];
                    let do_row = &do_mat[i * d_v..(i + 1) * d_v];
                    let dq_row = &mut dq[q_base + i * d_k..q_base + (i + 1) * d_k];
                    let s_row = &s_tile[r * bc..(r + 1) * bc];

                    for (c, &s_ij) in s_row.iter().enumerate() {
                        let p_ij = if s_ij.is_finite() && l_i.is_finite() {
                            (s_ij - l_i).exp()
                        } else {
                            0.0
                        };
                        if p_ij == 0.0 {
                            continue;
                        }
                        let v_row = &v_mat[(k0 + c) * d_v..(k0 + c + 1) * d_v];
                        let dp_ij = dot_f64_local(do_row, v_row);
                        let ds_ij = p_ij * (dp_ij - d_i);

                        // dV[k0+c, :] += P[i, k0+c] * dO[i, :]
                        let dv_row = &mut dv_tile[c * d_v..(c + 1) * d_v];
                        axpy_f64_local(dv_row, p_ij, do_row);

                        let scaled_ds = ds_ij * scale;
                        // dQ[i, :] += (scale * dS[i, k0+c]) * K[k0+c, :]
                        let k_row = &k_tile[c * d_k..(c + 1) * d_k];
                        axpy_f64_local(dq_row, scaled_ds, k_row);

                        // dK[k0+c, :] += (scale * dS[i, k0+c]) * Q[i, :]
                        let dk_row = &mut dk_tile[c * d_k..(c + 1) * d_k];
                        axpy_f64_local(dk_row, scaled_ds, q_row);

                        if let (Some((_, ms)), Some(dm_buf)) = (mask, dmask.as_deref_mut()) {
                            let flat_score_idx = (b_idx * s_q + i) * s_k + (k0 + c);
                            let m_idx = map_broadcast_flat_index(ms, &score_shape, flat_score_idx);
                            dm_buf[m_idx] += ds_ij;
                        }
                    }
                }
            }

            // Flush dV tile and dK tile into batch-mapped outputs (handling GQA/MQA accumulation!).
            for c in 0..bc {
                let dst_v = &mut dv[v_base + (k0 + c) * d_v..v_base + (k0 + c + 1) * d_v];
                let src_v = &dv_tile[c * d_v..(c + 1) * d_v];
                for (d_slot, &v) in dst_v.iter_mut().zip(src_v.iter()) {
                    *d_slot += v;
                }
                let src_k = &dk_tile[c * d_k..(c + 1) * d_k];
                if k_is_transposed {
                    for d in 0..d_k {
                        dk[k_base + d * s_k + (k0 + c)] += src_k[d];
                    }
                } else {
                    let dst_k = &mut dk[k_base + (k0 + c) * d_k..k_base + (k0 + c + 1) * d_k];
                    for (d_slot, &v) in dst_k.iter_mut().zip(src_k.iter()) {
                        *d_slot += v;
                    }
                }
            }
        }
    }

    // Final dtype quantization matching Mora's tensor dtype semantics.
    if dtype != DType::F64 {
        for v in &mut dq {
            *v = quantize_for_dtype(*v, dtype);
        }
        for v in &mut dk {
            *v = quantize_for_dtype(*v, dtype);
        }
        for v in &mut dv {
            *v = quantize_for_dtype(*v, dtype);
        }
        if let Some(ref mut dm) = dmask {
            for v in dm.iter_mut() {
                *v = quantize_for_dtype(*v, dtype);
            }
        }
    }

    Ok(FlashBwdResult { dq, dk, dv, dmask })
}
