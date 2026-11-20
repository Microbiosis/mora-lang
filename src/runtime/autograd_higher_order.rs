//! Wave 4 P1: Exact Arbitrary-Order Automatic Differentiation (`backward_tracked` & `forward_jvp_tracked`).
//!
//! Architecture (`AGENTS.md §0`):
//! - Whenever an inner `grad` / `value_and_grad` / `jvp` tape finishes while one or more outer
//!   tapes remain active on [`AutogradContext`], VJP and JVP rules are executed using tracked
//!   [`Tensor`] primitives against the enclosing [`AutogradContext`] (`create_graph = true`).
//! - Because every backward/forward rule emits tracked operations onto all enclosing active tapes,
//!   arbitrary-order derivatives (`grad(grad(f))`, `grad(grad(grad(f)))`), mixed forward-over-reverse
//!   Hessian-vector products (`jvp(grad(f), x, v)`), reverse-over-forward HVPs
//!   (`grad(fn(x) jvp(f, x, v)[1] end)`), and tracing backward graphs into a fusable
//!   `TensorComputeDag` (`trace_dag(grad(f), x).fuse()`) all compose automatically from one primitive.

use std::sync::Arc;

use crate::common::{BinaryOp, DType, ScalarValue};
use crate::error::{MoraError, MoraResult};
use crate::runtime::autograd::{AutogradContext, AutogradTape, TapeNode, TapeOp};
use crate::value::Value;
use crate::value::tensor::{
    Tensor, checked_numel, eval_tensor_binary_tracked, eval_tensor_math_unary_tracked,
    eval_tensor_matmul_tracked, eval_tensor_neg_tracked,
};

// ════════════════════════════════════════════════════════════════════════════
// Tracked Tensor Micro-Helpers
// ════════════════════════════════════════════════════════════════════════════

#[inline]
fn scalar_value_for_dt(val: f64, dt: DType) -> Value {
    if dt == DType::F64 {
        Value::Float(val)
    } else {
        ScalarValue::from_f64_quantized(val, dt)
            .map(Value::Scalar)
            .unwrap_or(Value::Float(val))
    }
}

#[inline]
fn expect_tensor(v: Value, ctx: &str) -> MoraResult<Arc<Tensor>> {
    match v {
        Value::Tensor(t) => Ok(t),
        other => Err(MoraError::Runtime(format!(
            "higher-order autograd ({ctx}): expected Tensor, got {}",
            crate::flow::dynamic_type_name(&other)
        ))),
    }
}

fn t_bin(
    op: BinaryOp,
    a: &Arc<Tensor>,
    b: &Arc<Tensor>,
    ag: &mut AutogradContext,
) -> MoraResult<Arc<Tensor>> {
    let v = eval_tensor_binary_tracked(
        &op,
        &Value::Tensor(Arc::clone(a)),
        &Value::Tensor(Arc::clone(b)),
        Some(ag),
    )?;
    expect_tensor(v, "binary")
}

fn t_add(a: &Arc<Tensor>, b: &Arc<Tensor>, ag: &mut AutogradContext) -> MoraResult<Arc<Tensor>> {
    t_bin(BinaryOp::Add, a, b, ag)
}

fn t_sub(a: &Arc<Tensor>, b: &Arc<Tensor>, ag: &mut AutogradContext) -> MoraResult<Arc<Tensor>> {
    t_bin(BinaryOp::Sub, a, b, ag)
}

fn t_mul(a: &Arc<Tensor>, b: &Arc<Tensor>, ag: &mut AutogradContext) -> MoraResult<Arc<Tensor>> {
    t_bin(BinaryOp::Mul, a, b, ag)
}

fn t_div(a: &Arc<Tensor>, b: &Arc<Tensor>, ag: &mut AutogradContext) -> MoraResult<Arc<Tensor>> {
    t_bin(BinaryOp::Div, a, b, ag)
}

fn t_pow(a: &Arc<Tensor>, b: &Arc<Tensor>, ag: &mut AutogradContext) -> MoraResult<Arc<Tensor>> {
    t_bin(BinaryOp::Pow, a, b, ag)
}

fn t_add_s(a: &Arc<Tensor>, s: f64, ag: &mut AutogradContext) -> MoraResult<Arc<Tensor>> {
    let sv = scalar_value_for_dt(s, a.dtype);
    expect_tensor(
        eval_tensor_binary_tracked(&BinaryOp::Add, &Value::Tensor(Arc::clone(a)), &sv, Some(ag))?,
        "add_s",
    )
}

fn t_rsub_s(s: f64, a: &Arc<Tensor>, ag: &mut AutogradContext) -> MoraResult<Arc<Tensor>> {
    let sv = scalar_value_for_dt(s, a.dtype);
    expect_tensor(
        eval_tensor_binary_tracked(&BinaryOp::Sub, &sv, &Value::Tensor(Arc::clone(a)), Some(ag))?,
        "rsub_s",
    )
}

fn t_mul_s(a: &Arc<Tensor>, s: f64, ag: &mut AutogradContext) -> MoraResult<Arc<Tensor>> {
    let sv = scalar_value_for_dt(s, a.dtype);
    expect_tensor(
        eval_tensor_binary_tracked(&BinaryOp::Mul, &Value::Tensor(Arc::clone(a)), &sv, Some(ag))?,
        "mul_s",
    )
}

fn t_div_s(a: &Arc<Tensor>, s: f64, ag: &mut AutogradContext) -> MoraResult<Arc<Tensor>> {
    let sv = scalar_value_for_dt(s, a.dtype);
    expect_tensor(
        eval_tensor_binary_tracked(&BinaryOp::Div, &Value::Tensor(Arc::clone(a)), &sv, Some(ag))?,
        "div_s",
    )
}

fn t_pow_s(a: &Arc<Tensor>, s: f64, ag: &mut AutogradContext) -> MoraResult<Arc<Tensor>> {
    let sv = scalar_value_for_dt(s, a.dtype);
    expect_tensor(
        eval_tensor_binary_tracked(&BinaryOp::Pow, &Value::Tensor(Arc::clone(a)), &sv, Some(ag))?,
        "pow_s",
    )
}

fn t_neg(a: &Arc<Tensor>, ag: &mut AutogradContext) -> MoraResult<Arc<Tensor>> {
    expect_tensor(eval_tensor_neg_tracked(a, Some(ag))?, "neg")
}

fn t_unary(a: &Arc<Tensor>, op: &str, ag: &mut AutogradContext) -> MoraResult<Arc<Tensor>> {
    expect_tensor(eval_tensor_math_unary_tracked(a, op, Some(ag))?, op)
}

fn t_matmul(a: &Arc<Tensor>, b: &Arc<Tensor>, ag: &mut AutogradContext) -> MoraResult<Arc<Tensor>> {
    let v = eval_tensor_matmul_tracked(
        &Value::Tensor(Arc::clone(a)),
        &Value::Tensor(Arc::clone(b)),
        Some(ag),
    )?;
    match v {
        Value::Tensor(t) => Ok(t),
        Value::Float(f) => Tensor::from_raw(vec![f], Vec::new(), a.dtype, a.device.clone(), false),
        Value::Scalar(s) => Tensor::from_raw(
            vec![s.as_f64()],
            Vec::new(),
            s.dtype(),
            a.device.clone(),
            false,
        ),
        other => Err(MoraError::Runtime(format!(
            "higher-order autograd (matmul): unexpected value {}",
            crate::flow::dynamic_type_name(&other)
        ))),
    }
}

fn t_reshape(
    a: &Arc<Tensor>,
    new_shape: Vec<usize>,
    ag: &mut AutogradContext,
) -> MoraResult<Arc<Tensor>> {
    let orig = a.shape.clone();
    let out = a.reshape(new_shape)?;
    Ok(ag.record_unary(a, out, |src| TapeOp::Reshape {
        a: src,
        orig_shape: orig.clone(),
    }))
}

fn t_transpose(
    a: &Arc<Tensor>,
    dim0: usize,
    dim1: usize,
    ag: &mut AutogradContext,
) -> MoraResult<Arc<Tensor>> {
    let orig = a.shape.clone();
    let out = a.transpose(Some(dim0), Some(dim1))?;
    Ok(ag.record_unary(a, out, |src| TapeOp::Transpose {
        a: src,
        dim0,
        dim1,
        orig_shape: orig.clone(),
    }))
}

fn t_sum(a: &Arc<Tensor>, ag: &mut AutogradContext) -> MoraResult<Arc<Tensor>> {
    let s: f64 = a.to_flat_vec().into_iter().sum();
    let orig = a.shape.clone();
    let out = Tensor::from_raw(vec![s], Vec::new(), a.dtype, a.device.clone(), false)?;
    Ok(ag.record_unary(a, out, |src| TapeOp::Sum {
        a: src,
        a_shape: orig.clone(),
    }))
}

fn t_mean(a: &Arc<Tensor>, ag: &mut AutogradContext) -> MoraResult<Arc<Tensor>> {
    let flat = a.to_flat_vec();
    let n = flat.len().max(1) as f64;
    let m = flat.iter().sum::<f64>() / n;
    let orig = a.shape.clone();
    let out = Tensor::from_raw(vec![m], Vec::new(), a.dtype, a.device.clone(), false)?;
    Ok(ag.record_unary(a, out, |src| TapeOp::Mean {
        a: src,
        a_shape: orig.clone(),
    }))
}

fn t_sum_axis(
    a: &Arc<Tensor>,
    axis: usize,
    keepdim: bool,
    ag: &mut AutogradContext,
) -> MoraResult<Arc<Tensor>> {
    a.sum_axis_tracked(axis, keepdim, Some(ag))
}

fn t_gather(
    a: &Arc<Tensor>,
    out_shape: Vec<usize>,
    src_flat_indices: Vec<usize>,
    ag: &mut AutogradContext,
) -> MoraResult<Arc<Tensor>> {
    a.gather_tracked(out_shape, src_flat_indices, Some(ag))
}

fn t_scatter_add(
    a: &Arc<Tensor>,
    out_shape: Vec<usize>,
    dst_flat_indices: Vec<usize>,
    ag: &mut AutogradContext,
) -> MoraResult<Arc<Tensor>> {
    a.scatter_add_tracked(out_shape, dst_flat_indices, Some(ag))
}

/// Broadcast-aware tracked reduction matching `g_out` down to `target_shape`.
fn t_unbroadcast(
    g_out: Arc<Tensor>,
    target_shape: &[usize],
    ag: &mut AutogradContext,
) -> MoraResult<Arc<Tensor>> {
    if g_out.shape == target_shape {
        return Ok(g_out);
    }
    if target_shape.is_empty() {
        return t_sum(&g_out, ag);
    }
    let mut cur = g_out;
    while cur.shape.len() > target_shape.len() {
        cur = t_sum_axis(&cur, 0, false, ag)?;
    }
    for (ax, &td) in target_shape.iter().enumerate() {
        if td == 1 && cur.shape[ax] > 1 {
            cur = t_sum_axis(&cur, ax, true, ag)?;
        }
    }
    Ok(cur)
}

/// Broadcast `src` to `out_shape` via tracked addition of zeros (no-op if shapes already match).
fn t_broadcast_to(
    src: Arc<Tensor>,
    out_shape: &[usize],
    ag: &mut AutogradContext,
) -> MoraResult<Arc<Tensor>> {
    if src.shape == out_shape {
        return Ok(src);
    }
    let zeros = Tensor::zeros(out_shape.to_vec(), src.dtype, src.device.clone(), false)?;
    t_add(&src, &zeros, ag)
}

/// Tracked row-wise sum along the last dimension preserving broadcastable rank (`[..., 1]` or `[]`).
fn t_row_sum_keepdim(t: &Arc<Tensor>, ag: &mut AutogradContext) -> MoraResult<Arc<Tensor>> {
    if t.shape.len() <= 1 {
        t_sum(t, ag)
    } else {
        let last_ax = t.shape.len() - 1;
        t_sum_axis(t, last_ax, true, ag)
    }
}

fn resolve_saved_a(
    node: &TapeNode,
    fallback_vals: &[f64],
    fallback_shape: &[usize],
) -> MoraResult<Arc<Tensor>> {
    if let Some(t) = &node.saved_a {
        Ok(Arc::clone(t))
    } else {
        Tensor::from_raw(
            fallback_vals.to_vec(),
            fallback_shape.to_vec(),
            node.dtype,
            node.device.clone(),
            false,
        )
    }
}

fn resolve_saved_b(
    node: &TapeNode,
    fallback_vals: &[f64],
    fallback_shape: &[usize],
) -> MoraResult<Arc<Tensor>> {
    if let Some(t) = &node.saved_b {
        Ok(Arc::clone(t))
    } else {
        Tensor::from_raw(
            fallback_vals.to_vec(),
            fallback_shape.to_vec(),
            node.dtype,
            node.device.clone(),
            false,
        )
    }
}

fn resolve_saved_out(node: &TapeNode, fallback_vals: &[f64]) -> MoraResult<Arc<Tensor>> {
    if let Some(t) = &node.saved_out {
        Ok(Arc::clone(t))
    } else {
        Tensor::from_raw(
            fallback_vals.to_vec(),
            node.shape.clone(),
            node.dtype,
            node.device.clone(),
            false,
        )
    }
}

fn accum_tracked(
    grads: &mut [Option<Arc<Tensor>>],
    idx: usize,
    delta: Arc<Tensor>,
    ag: &mut AutogradContext,
) -> MoraResult<()> {
    match grads[idx].take() {
        Some(existing) => {
            grads[idx] = Some(t_add(&existing, &delta, ag)?);
        }
        None => {
            grads[idx] = Some(delta);
        }
    }
    Ok(())
}

type TrackedVjpPair = (Option<Arc<Tensor>>, Option<Arc<Tensor>>);

/// Tracked matmul VJP supporting 1D, 2D, and batched ND contractions to arbitrary differentiation order.
fn matmul_vjp_tracked(
    g_out: &Arc<Tensor>,
    a_t: &Arc<Tensor>,
    b_t: &Arc<Tensor>,
    need_a: bool,
    need_b: bool,
    ag: &mut AutogradContext,
) -> MoraResult<TrackedVjpPair> {
    match (a_t.shape.as_slice(), b_t.shape.as_slice()) {
        ([_k1], [_k2]) => {
            let ga = if need_a {
                Some(t_mul(g_out, b_t, ag)?)
            } else {
                None
            };
            let gb = if need_b {
                Some(t_mul(g_out, a_t, ag)?)
            } else {
                None
            };
            Ok((ga, gb))
        }
        ([m, k], [_k2]) => {
            let g_col = t_reshape(g_out, vec![*m, 1], ag)?;
            let ga = if need_a {
                let b_row = t_reshape(b_t, vec![1, *k], ag)?;
                Some(t_matmul(&g_col, &b_row, ag)?)
            } else {
                None
            };
            let gb = if need_b {
                let a_tr = t_transpose(a_t, 0, 1, ag)?;
                let prod = t_matmul(&a_tr, &g_col, ag)?;
                Some(t_reshape(&prod, vec![*k], ag)?)
            } else {
                None
            };
            Ok((ga, gb))
        }
        ([k], [_k2, n]) => {
            let ga = if need_a {
                let g_col = t_reshape(g_out, vec![*n, 1], ag)?;
                let prod = t_matmul(b_t, &g_col, ag)?;
                Some(t_reshape(&prod, vec![*k], ag)?)
            } else {
                None
            };
            let gb = if need_b {
                let a_col = t_reshape(a_t, vec![*k, 1], ag)?;
                let g_row = t_reshape(g_out, vec![1, *n], ag)?;
                Some(t_matmul(&a_col, &g_row, ag)?)
            } else {
                None
            };
            Ok((ga, gb))
        }
        (sa, sb) if sa.len() >= 2 && sb.len() >= 2 => {
            let ga = if need_a {
                let rb = sb.len();
                let b_tr = t_transpose(b_t, rb - 2, rb - 1, ag)?;
                let full = t_matmul(g_out, &b_tr, ag)?;
                Some(t_unbroadcast(full, sa, ag)?)
            } else {
                None
            };
            let gb = if need_b {
                let ra = sa.len();
                let a_tr = t_transpose(a_t, ra - 2, ra - 1, ag)?;
                let full = t_matmul(&a_tr, g_out, ag)?;
                Some(t_unbroadcast(full, sb, ag)?)
            } else {
                None
            };
            Ok((ga, gb))
        }
        (sa, sb) => Err(MoraError::Runtime(format!(
            "higher-order autograd: unsupported matmul shapes {sa:?} @ {sb:?}"
        ))),
    }
}

/// Shared tracked derivative formula for GELU (`0.5*x*(1 + tanh(c*(x + 0.044715*x^3)))`).
fn gelu_deriv_tracked(x: &Arc<Tensor>, ag: &mut AutogradContext) -> MoraResult<Arc<Tensor>> {
    let c = (2.0_f64 / std::f64::consts::PI).sqrt();
    let x2 = t_mul(x, x, ag)?;
    let x3 = t_mul(&x2, x, ag)?;
    let cubic = t_mul_s(&x3, 0.044715, ag)?;
    let inner = t_add(x, &cubic, ag)?;
    let u = t_mul_s(&inner, c, ag)?;
    let th = t_unary(&u, "tanh", ag)?;
    let th2 = t_mul(&th, &th, ag)?;
    let sech2 = t_rsub_s(1.0, &th2, ag)?;
    let du_dx = t_mul_s(
        &t_add_s(&t_mul_s(&x2, 3.0 * 0.044715, ag)?, 1.0, ag)?,
        c,
        ag,
    )?;
    let term1 = t_mul_s(&t_add_s(&th, 1.0, ag)?, 0.5, ag)?;
    let term2 = t_mul(&t_mul(&t_mul_s(x, 0.5, ag)?, &sech2, ag)?, &du_dx, ag)?;
    t_add(&term1, &term2, ag)
}

/// Shared tracked derivative formula for SiLU (`x * sigmoid(x)`).
fn silu_deriv_tracked(x: &Arc<Tensor>, ag: &mut AutogradContext) -> MoraResult<Arc<Tensor>> {
    let s = t_unary(x, "sigmoid", ag)?;
    let one_minus_s = t_rsub_s(1.0, &s, ag)?;
    let factor = t_add_s(&t_mul(x, &one_minus_s, ag)?, 1.0, ag)?;
    t_mul(&s, &factor, ag)
}

/// Shared tracked symmetric Jacobian action for Softmax (`y * (v - sum(v * y))`).
fn softmax_jvp_or_vjp_tracked(
    y: &Arc<Tensor>,
    v: &Arc<Tensor>,
    ag: &mut AutogradContext,
) -> MoraResult<Arc<Tensor>> {
    let vy = t_mul(v, y, ag)?;
    let dot = t_row_sum_keepdim(&vy, ag)?;
    let diff = t_sub(v, &dot, ag)?;
    t_mul(y, &diff, ag)
}

/// Shared tracked symmetric Jacobian action for RMSNorm.
fn rms_norm_jvp_or_vjp_tracked(
    x: &Arc<Tensor>,
    v: &Arc<Tensor>,
    last_dim: usize,
    eps: f64,
    ag: &mut AutogradContext,
) -> MoraResult<Arc<Tensor>> {
    let d_f = last_dim.max(1) as f64;
    let x2 = t_mul(x, x, ag)?;
    let ms = t_div_s(&t_row_sum_keepdim(&x2, ag)?, d_f, ag)?;
    let inv_rms = t_pow_s(&t_add_s(&ms, eps, ag)?, -0.5, ag)?;
    let dot_vx = t_row_sum_keepdim(&t_mul(v, x, ag)?, ag)?;
    let inv_rms2 = t_mul(&inv_rms, &inv_rms, ag)?;
    let coeff = t_mul(&t_div_s(&inv_rms2, d_f, ag)?, &dot_vx, ag)?;
    let scaled_x = t_mul(&coeff, x, ag)?;
    t_mul(&inv_rms, &t_sub(v, &scaled_x, ag)?, ag)
}

/// Shared tracked symmetric Jacobian action for LayerNorm.
fn layer_norm_jvp_or_vjp_tracked(
    x: &Arc<Tensor>,
    v: &Arc<Tensor>,
    last_dim: usize,
    eps: f64,
    ag: &mut AutogradContext,
) -> MoraResult<Arc<Tensor>> {
    let d_f = last_dim.max(1) as f64;
    let mean = t_div_s(&t_row_sum_keepdim(x, ag)?, d_f, ag)?;
    let centered = t_sub(x, &mean, ag)?;
    let var = t_div_s(
        &t_row_sum_keepdim(&t_mul(&centered, &centered, ag)?, ag)?,
        d_f,
        ag,
    )?;
    let inv_std = t_pow_s(&t_add_s(&var, eps, ag)?, -0.5, ag)?;
    let xhat = t_mul(&centered, &inv_std, ag)?;
    let sum_v = t_row_sum_keepdim(v, ag)?;
    let sum_vx = t_row_sum_keepdim(&t_mul(v, &xhat, ag)?, ag)?;
    let inner = t_sub(
        &t_sub(&t_mul_s(v, d_f, ag)?, &sum_v, ag)?,
        &t_mul(&xhat, &sum_vx, ag)?,
        ag,
    )?;
    t_mul(&t_div_s(&inv_std, d_f, ag)?, &inner, ag)
}

/// Safe tracked `Pow` partial w.r.t. base `a`: `b * a^(b - 1)` (0 where `b == 0`, NaN-free).
fn pow_da_tracked(
    a_t: &Arc<Tensor>,
    b_t: &Arc<Tensor>,
    ag: &mut AutogradContext,
) -> MoraResult<Arc<Tensor>> {
    let b_flat = b_t.to_flat_vec();
    let has_zero = b_flat.contains(&0.0);
    if !has_zero {
        let b_minus_1 = t_add_s(b_t, -1.0, ag)?;
        let p = t_pow(a_t, &b_minus_1, ag)?;
        return t_mul(b_t, &p, ag);
    }
    let nz_mask_vals: Vec<f64> = b_flat
        .iter()
        .map(|&v| if v == 0.0 { 0.0 } else { 1.0 })
        .collect();
    let nz_mask = Tensor::from_raw(
        nz_mask_vals,
        b_t.shape.clone(),
        b_t.dtype,
        b_t.device.clone(),
        false,
    )?;
    let exp_safe = t_sub(b_t, &nz_mask, ag)?;
    let p = t_pow(a_t, &exp_safe, ag)?;
    t_mul(b_t, &p, ag)
}

/// Safe tracked `Pow` partial w.r.t. exponent `b`: `out * ln(a)` (0 where `a <= 0`, NaN-free).
fn pow_db_tracked(
    a_t: &Arc<Tensor>,
    out_t: &Arc<Tensor>,
    ag: &mut AutogradContext,
) -> MoraResult<Arc<Tensor>> {
    let a_flat = a_t.to_flat_vec();
    let all_pos = a_flat.iter().all(|&v| v > 0.0);
    if all_pos {
        let lna = t_unary(a_t, "ln", ag)?;
        return t_mul(out_t, &lna, ag);
    }
    let pos_mask_vals: Vec<f64> = a_flat
        .iter()
        .map(|&v| if v > 0.0 { 1.0 } else { 0.0 })
        .collect();
    let one_minus_vals: Vec<f64> = pos_mask_vals.iter().map(|&m| 1.0 - m).collect();
    let pos_mask = Tensor::from_raw(
        pos_mask_vals,
        a_t.shape.clone(),
        a_t.dtype,
        a_t.device.clone(),
        false,
    )?;
    let safe_pad = Tensor::from_raw(
        one_minus_vals,
        a_t.shape.clone(),
        a_t.dtype,
        a_t.device.clone(),
        false,
    )?;
    let safe_a = t_add(&t_mul(a_t, &pos_mask, ag)?, &safe_pad, ag)?;
    let lna = t_unary(&safe_a, "ln", ag)?;
    t_mul(&t_mul(out_t, &lna, ag)?, &pos_mask, ag)
}

// ════════════════════════════════════════════════════════════════════════════
// 1. Tracked Reverse-Mode Backward (`create_graph = true`)
// ════════════════════════════════════════════════════════════════════════════

pub(crate) fn backward_tracked(
    tape: &AutogradTape,
    out_idx: usize,
    leaf_indices: &[usize],
    ag: &mut AutogradContext,
) -> MoraResult<Vec<Arc<Tensor>>> {
    let nodes = tape.nodes();
    let out_node = nodes.get(out_idx).ok_or_else(|| {
        MoraError::Runtime(format!("autograd: output node {out_idx} out of bounds"))
    })?;
    let out_numel = checked_numel(&out_node.shape)?;
    if out_numel != 1 {
        return Err(MoraError::Runtime(format!(
            "grad: differentiated function must return a scalar (0-D or 1-element tensor), got shape {:?}",
            out_node.shape
        )));
    }

    let mut grads: Vec<Option<Arc<Tensor>>> = vec![None; nodes.len()];
    grads[out_idx] = Some(Tensor::from_raw(
        vec![1.0],
        out_node.shape.clone(),
        out_node.dtype,
        out_node.device.clone(),
        false,
    )?);

    for idx in (0..=out_idx).rev() {
        let Some(g_out) = grads[idx].clone() else {
            continue;
        };
        let node = &nodes[idx];
        match &node.op {
            TapeOp::Leaf => {}
            TapeOp::Add {
                a,
                b,
                a_shape,
                b_shape,
                ..
            } => {
                if let Some(ia) = *a {
                    let ga = t_unbroadcast(Arc::clone(&g_out), a_shape, ag)?;
                    accum_tracked(&mut grads, ia, ga, ag)?;
                }
                if let Some(ib) = *b {
                    let gb = t_unbroadcast(g_out, b_shape, ag)?;
                    accum_tracked(&mut grads, ib, gb, ag)?;
                }
            }
            TapeOp::Sub {
                a,
                b,
                a_shape,
                b_shape,
                ..
            } => {
                if let Some(ia) = *a {
                    let ga = t_unbroadcast(Arc::clone(&g_out), a_shape, ag)?;
                    accum_tracked(&mut grads, ia, ga, ag)?;
                }
                if let Some(ib) = *b {
                    let neg_g = t_neg(&g_out, ag)?;
                    let gb = t_unbroadcast(neg_g, b_shape, ag)?;
                    accum_tracked(&mut grads, ib, gb, ag)?;
                }
            }
            TapeOp::Mul {
                a,
                b,
                a_vals,
                b_vals,
                a_shape,
                b_shape,
            } => {
                if let Some(ia) = *a {
                    let b_t = resolve_saved_b(node, b_vals, b_shape)?;
                    let full = t_mul(&g_out, &b_t, ag)?;
                    let ga = t_unbroadcast(full, a_shape, ag)?;
                    accum_tracked(&mut grads, ia, ga, ag)?;
                }
                if let Some(ib) = *b {
                    let a_t = resolve_saved_a(node, a_vals, a_shape)?;
                    let full = t_mul(&g_out, &a_t, ag)?;
                    let gb = t_unbroadcast(full, b_shape, ag)?;
                    accum_tracked(&mut grads, ib, gb, ag)?;
                }
            }
            TapeOp::Div {
                a,
                b,
                a_vals,
                b_vals,
                a_shape,
                b_shape,
            } => {
                let b_t = resolve_saved_b(node, b_vals, b_shape)?;
                if let Some(ia) = *a {
                    let full = t_div(&g_out, &b_t, ag)?;
                    let ga = t_unbroadcast(full, a_shape, ag)?;
                    accum_tracked(&mut grads, ia, ga, ag)?;
                }
                if let Some(ib) = *b {
                    let a_t = resolve_saved_a(node, a_vals, a_shape)?;
                    let b2 = t_mul(&b_t, &b_t, ag)?;
                    let full = t_neg(&t_div(&t_mul(&g_out, &a_t, ag)?, &b2, ag)?, ag)?;
                    let gb = t_unbroadcast(full, b_shape, ag)?;
                    accum_tracked(&mut grads, ib, gb, ag)?;
                }
            }
            TapeOp::Pow {
                a,
                b,
                a_vals,
                b_vals,
                out_vals,
                a_shape,
                b_shape,
            } => {
                let a_t = resolve_saved_a(node, a_vals, a_shape)?;
                let b_t = resolve_saved_b(node, b_vals, b_shape)?;
                if let Some(ia) = *a {
                    let da = pow_da_tracked(&a_t, &b_t, ag)?;
                    let full = t_mul(&g_out, &da, ag)?;
                    let ga = t_unbroadcast(full, a_shape, ag)?;
                    accum_tracked(&mut grads, ia, ga, ag)?;
                }
                if let Some(ib) = *b {
                    let out_t = resolve_saved_out(node, out_vals)?;
                    let db = pow_db_tracked(&a_t, &out_t, ag)?;
                    let full = t_mul(&g_out, &db, ag)?;
                    let gb = t_unbroadcast(full, b_shape, ag)?;
                    accum_tracked(&mut grads, ib, gb, ag)?;
                }
            }
            TapeOp::AddScalar { a, .. } => {
                accum_tracked(&mut grads, *a, g_out, ag)?;
            }
            TapeOp::SubScalar { a, reversed, .. } => {
                let ga = if *reversed { t_neg(&g_out, ag)? } else { g_out };
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::MulScalar { a, scalar } => {
                let ga = t_mul_s(&g_out, *scalar, ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::DivScalar {
                a,
                scalar,
                reversed,
                a_vals,
            } => {
                let ga = if *reversed {
                    let a_t = resolve_saved_a(node, a_vals, &node.shape)?;
                    let a2 = t_mul(&a_t, &a_t, ag)?;
                    t_neg(&t_div(&t_mul_s(&g_out, *scalar, ag)?, &a2, ag)?, ag)?
                } else {
                    t_div_s(&g_out, *scalar, ag)?
                };
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::PowScalar {
                a,
                scalar,
                reversed,
                a_vals,
                out_vals,
            } => {
                let ga = if *reversed {
                    if *scalar > 0.0 {
                        let out_t = resolve_saved_out(node, out_vals)?;
                        t_mul_s(&t_mul(&g_out, &out_t, ag)?, scalar.ln(), ag)?
                    } else {
                        Tensor::zeros(node.shape.clone(), node.dtype, node.device.clone(), false)?
                    }
                } else if *scalar == 0.0 {
                    Tensor::zeros(node.shape.clone(), node.dtype, node.device.clone(), false)?
                } else {
                    let a_t = resolve_saved_a(node, a_vals, &node.shape)?;
                    let pow_part = t_pow_s(&a_t, *scalar - 1.0, ag)?;
                    t_mul(&t_mul_s(&g_out, *scalar, ag)?, &pow_part, ag)?
                };
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::Neg { a } => {
                let ga = t_neg(&g_out, ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::MatMul {
                a,
                b,
                a_vals,
                b_vals,
                a_shape,
                b_shape,
            } => {
                let a_t = resolve_saved_a(node, a_vals, a_shape)?;
                let b_t = resolve_saved_b(node, b_vals, b_shape)?;
                let (ga_opt, gb_opt) =
                    matmul_vjp_tracked(&g_out, &a_t, &b_t, a.is_some(), b.is_some(), ag)?;
                if let (Some(ia), Some(ga)) = (*a, ga_opt) {
                    accum_tracked(&mut grads, ia, ga, ag)?;
                }
                if let (Some(ib), Some(gb)) = (*b, gb_opt) {
                    accum_tracked(&mut grads, ib, gb, ag)?;
                }
            }
            TapeOp::Sum { a, a_shape } => {
                let ga = t_broadcast_to(g_out, a_shape, ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::Mean { a, a_shape } => {
                let n = checked_numel(a_shape)?.max(1) as f64;
                let scaled = t_div_s(&g_out, n, ag)?;
                let ga = t_broadcast_to(scaled, a_shape, ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::Reshape { a, orig_shape } => {
                let ga = t_reshape(&g_out, orig_shape.clone(), ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::Identity { a } => {
                accum_tracked(&mut grads, *a, g_out, ag)?;
            }
            TapeOp::Transpose { a, dim0, dim1, .. } => {
                let ga = t_transpose(&g_out, *dim0, *dim1, ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::Gather {
                a,
                src_flat_indices,
                ..
            } => {
                let src_shape = nodes[*a].shape.clone();
                let ga = t_scatter_add(&g_out, src_shape, src_flat_indices.clone(), ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::ScatterAdd {
                a,
                dst_flat_indices,
                ..
            } => {
                let src_shape = nodes[*a].shape.clone();
                let ga = t_gather(&g_out, src_shape, dst_flat_indices.clone(), ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::SumAxis {
                a,
                axis,
                keepdim,
                orig_shape,
            } => {
                let prepared = if *keepdim {
                    g_out
                } else {
                    let mut unsqueezed = g_out.shape.clone();
                    unsqueezed.insert(*axis, 1);
                    t_reshape(&g_out, unsqueezed, ag)?
                };
                let ga = t_broadcast_to(prepared, orig_shape, ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::Relu { a, a_vals } => {
                let mask_vals: Vec<f64> = a_vals
                    .iter()
                    .map(|&x| if x > 0.0 { 1.0 } else { 0.0 })
                    .collect();
                let mask = Tensor::from_raw(
                    mask_vals,
                    node.shape.clone(),
                    node.dtype,
                    node.device.clone(),
                    false,
                )?;
                let ga = t_mul(&g_out, &mask, ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::Gelu { a, a_vals } => {
                let a_t = resolve_saved_a(node, a_vals, &node.shape)?;
                let deriv = gelu_deriv_tracked(&a_t, ag)?;
                let ga = t_mul(&g_out, &deriv, ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::Silu { a, a_vals } => {
                let a_t = resolve_saved_a(node, a_vals, &node.shape)?;
                let deriv = silu_deriv_tracked(&a_t, ag)?;
                let ga = t_mul(&g_out, &deriv, ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::Tanh { a, out_vals } => {
                let y = resolve_saved_out(node, out_vals)?;
                let deriv = t_rsub_s(1.0, &t_mul(&y, &y, ag)?, ag)?;
                let ga = t_mul(&g_out, &deriv, ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::Sigmoid { a, out_vals } => {
                let s = resolve_saved_out(node, out_vals)?;
                let deriv = t_mul(&s, &t_rsub_s(1.0, &s, ag)?, ag)?;
                let ga = t_mul(&g_out, &deriv, ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::Exp { a, out_vals } => {
                let y = resolve_saved_out(node, out_vals)?;
                let ga = t_mul(&g_out, &y, ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::Ln { a, a_vals } => {
                let a_t = resolve_saved_a(node, a_vals, &node.shape)?;
                let ga = t_div(&g_out, &a_t, ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::Sqrt { a, out_vals } => {
                let y = resolve_saved_out(node, out_vals)?;
                let ga = t_div(&g_out, &t_mul_s(&y, 2.0, ag)?, ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::Sin { a, a_vals } => {
                let a_t = resolve_saved_a(node, a_vals, &node.shape)?;
                let ga = t_mul(&g_out, &t_unary(&a_t, "cos", ag)?, ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::Cos { a, a_vals } => {
                let a_t = resolve_saved_a(node, a_vals, &node.shape)?;
                let ga = t_neg(&t_mul(&g_out, &t_unary(&a_t, "sin", ag)?, ag)?, ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::Abs { a, a_vals } => {
                let sign_vals: Vec<f64> = a_vals
                    .iter()
                    .map(|&x| {
                        if x > 0.0 {
                            1.0
                        } else if x < 0.0 {
                            -1.0
                        } else {
                            0.0
                        }
                    })
                    .collect();
                let sign = Tensor::from_raw(
                    sign_vals,
                    node.shape.clone(),
                    node.dtype,
                    node.device.clone(),
                    false,
                )?;
                let ga = t_mul(&g_out, &sign, ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::Softmax { a, out_vals, .. } => {
                let y = resolve_saved_out(node, out_vals)?;
                let ga = softmax_jvp_or_vjp_tracked(&y, &g_out, ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::RmsNorm {
                a,
                a_vals,
                last_dim,
                eps,
            } => {
                let x = resolve_saved_a(node, a_vals, &node.shape)?;
                let ga = rms_norm_jvp_or_vjp_tracked(&x, &g_out, *last_dim, *eps, ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
            TapeOp::LayerNorm {
                a,
                a_vals,
                last_dim,
                eps,
            } => {
                let x = resolve_saved_a(node, a_vals, &node.shape)?;
                let ga = layer_norm_jvp_or_vjp_tracked(&x, &g_out, *last_dim, *eps, ag)?;
                accum_tracked(&mut grads, *a, ga, ag)?;
            }
        }
    }

    let mut out = Vec::with_capacity(leaf_indices.len());
    for &leaf_idx in leaf_indices {
        let leaf = &nodes[leaf_idx];
        let gt = match grads[leaf_idx].clone() {
            Some(t) => t,
            None => Tensor::zeros(leaf.shape.clone(), leaf.dtype, leaf.device.clone(), false)?,
        };
        out.push(gt);
    }
    Ok(out)
}

// ════════════════════════════════════════════════════════════════════════════
// 2. Tracked Forward-Mode JVP (`create_graph = true`, e.g. `grad(jvp(f))`)
// ════════════════════════════════════════════════════════════════════════════

pub(crate) fn forward_jvp_tracked(
    tape: &AutogradTape,
    out_idx: usize,
    leaf_indices: &[usize],
    leaf_tangents: &[Arc<Tensor>],
    ag: &mut AutogradContext,
) -> MoraResult<Arc<Tensor>> {
    let nodes = tape.nodes();
    let out_node = nodes
        .get(out_idx)
        .ok_or_else(|| MoraError::Runtime(format!("jvp: output node {out_idx} out of bounds")))?;
    if leaf_indices.len() != leaf_tangents.len() {
        return Err(MoraError::Runtime(format!(
            "jvp: expected {} tangent(s) matching leaf count, got {}",
            leaf_indices.len(),
            leaf_tangents.len()
        )));
    }

    let mut tangents: Vec<Option<Arc<Tensor>>> = vec![None; nodes.len()];
    for (&li, tan) in leaf_indices.iter().zip(leaf_tangents.iter()) {
        let leaf = nodes
            .get(li)
            .ok_or_else(|| MoraError::Runtime(format!("jvp: leaf node {li} out of bounds")))?;
        if leaf.shape != tan.shape {
            return Err(MoraError::Runtime(format!(
                "jvp: tangent shape {:?} must match primal shape {:?}",
                tan.shape, leaf.shape
            )));
        }
        tangents[li] = Some(Arc::clone(tan));
    }

    for idx in 0..=out_idx {
        let node = &nodes[idx];
        let out_tan: Option<Arc<Tensor>> = match &node.op {
            TapeOp::Leaf => tangents[idx].clone(),
            TapeOp::Add { a, b, .. } => {
                let ta = a.and_then(|i| tangents[i].as_ref());
                let tb = b.and_then(|i| tangents[i].as_ref());
                match (ta, tb) {
                    (None, None) => None,
                    (Some(da), None) => Some(t_broadcast_to(Arc::clone(da), &node.shape, ag)?),
                    (None, Some(db)) => Some(t_broadcast_to(Arc::clone(db), &node.shape, ag)?),
                    (Some(da), Some(db)) => Some(t_add(da, db, ag)?),
                }
            }
            TapeOp::Sub { a, b, .. } => {
                let ta = a.and_then(|i| tangents[i].as_ref());
                let tb = b.and_then(|i| tangents[i].as_ref());
                match (ta, tb) {
                    (None, None) => None,
                    (Some(da), None) => Some(t_broadcast_to(Arc::clone(da), &node.shape, ag)?),
                    (None, Some(db)) => {
                        let neg = t_neg(db, ag)?;
                        Some(t_broadcast_to(neg, &node.shape, ag)?)
                    }
                    (Some(da), Some(db)) => Some(t_sub(da, db, ag)?),
                }
            }
            TapeOp::Mul {
                a,
                b,
                a_vals,
                b_vals,
                a_shape,
                b_shape,
            } => {
                let ta = a.and_then(|i| tangents[i].as_ref());
                let tb = b.and_then(|i| tangents[i].as_ref());
                match (ta, tb) {
                    (None, None) => None,
                    (Some(da), None) => {
                        let b_t = resolve_saved_b(node, b_vals, b_shape)?;
                        Some(t_mul(da, &b_t, ag)?)
                    }
                    (None, Some(db)) => {
                        let a_t = resolve_saved_a(node, a_vals, a_shape)?;
                        Some(t_mul(&a_t, db, ag)?)
                    }
                    (Some(da), Some(db)) => {
                        let a_t = resolve_saved_a(node, a_vals, a_shape)?;
                        let b_t = resolve_saved_b(node, b_vals, b_shape)?;
                        let left = t_mul(da, &b_t, ag)?;
                        let right = t_mul(&a_t, db, ag)?;
                        Some(t_add(&left, &right, ag)?)
                    }
                }
            }
            TapeOp::Div {
                a,
                b,
                a_vals,
                b_vals,
                a_shape,
                b_shape,
            } => {
                let ta = a.and_then(|i| tangents[i].as_ref());
                let tb = b.and_then(|i| tangents[i].as_ref());
                match (ta, tb) {
                    (None, None) => None,
                    (Some(da), None) => {
                        let b_t = resolve_saved_b(node, b_vals, b_shape)?;
                        Some(t_div(da, &b_t, ag)?)
                    }
                    (None, Some(db)) => {
                        let a_t = resolve_saved_a(node, a_vals, a_shape)?;
                        let b_t = resolve_saved_b(node, b_vals, b_shape)?;
                        let b2 = t_mul(&b_t, &b_t, ag)?;
                        Some(t_neg(&t_div(&t_mul(&a_t, db, ag)?, &b2, ag)?, ag)?)
                    }
                    (Some(da), Some(db)) => {
                        let a_t = resolve_saved_a(node, a_vals, a_shape)?;
                        let b_t = resolve_saved_b(node, b_vals, b_shape)?;
                        let num = t_sub(&t_mul(da, &b_t, ag)?, &t_mul(&a_t, db, ag)?, ag)?;
                        let b2 = t_mul(&b_t, &b_t, ag)?;
                        Some(t_div(&num, &b2, ag)?)
                    }
                }
            }
            TapeOp::Pow {
                a,
                b,
                a_vals,
                b_vals,
                out_vals,
                a_shape,
                b_shape,
            } => {
                let ta = a.and_then(|i| tangents[i].as_ref());
                let tb = b.and_then(|i| tangents[i].as_ref());
                if ta.is_none() && tb.is_none() {
                    None
                } else {
                    let a_t = resolve_saved_a(node, a_vals, a_shape)?;
                    let b_t = resolve_saved_b(node, b_vals, b_shape)?;
                    let part_a = if let Some(da) = ta {
                        Some(t_mul(da, &pow_da_tracked(&a_t, &b_t, ag)?, ag)?)
                    } else {
                        None
                    };
                    let part_b = if let Some(db) = tb {
                        let out_t = resolve_saved_out(node, out_vals)?;
                        Some(t_mul(db, &pow_db_tracked(&a_t, &out_t, ag)?, ag)?)
                    } else {
                        None
                    };
                    match (part_a, part_b) {
                        (Some(x), Some(y)) => Some(t_add(&x, &y, ag)?),
                        (Some(x), None) => Some(x),
                        (None, Some(y)) => Some(y),
                        (None, None) => None,
                    }
                }
            }
            TapeOp::AddScalar { a, .. } => tangents[*a].clone(),
            TapeOp::SubScalar { a, reversed, .. } => match &tangents[*a] {
                None => None,
                Some(da) => Some(if *reversed {
                    t_neg(da, ag)?
                } else {
                    Arc::clone(da)
                }),
            },
            TapeOp::MulScalar { a, scalar } => match &tangents[*a] {
                None => None,
                Some(da) => Some(t_mul_s(da, *scalar, ag)?),
            },
            TapeOp::DivScalar {
                a,
                scalar,
                reversed,
                a_vals,
            } => match &tangents[*a] {
                None => None,
                Some(da) => Some(if *reversed {
                    let a_t = resolve_saved_a(node, a_vals, &node.shape)?;
                    let a2 = t_mul(&a_t, &a_t, ag)?;
                    t_neg(&t_div(&t_mul_s(da, *scalar, ag)?, &a2, ag)?, ag)?
                } else {
                    t_div_s(da, *scalar, ag)?
                }),
            },
            TapeOp::PowScalar {
                a,
                scalar,
                reversed,
                a_vals,
                out_vals,
            } => match &tangents[*a] {
                None => None,
                Some(da) => Some(if *reversed {
                    if *scalar > 0.0 {
                        let out_t = resolve_saved_out(node, out_vals)?;
                        t_mul_s(&t_mul(da, &out_t, ag)?, scalar.ln(), ag)?
                    } else {
                        Tensor::zeros(node.shape.clone(), node.dtype, node.device.clone(), false)?
                    }
                } else if *scalar == 0.0 {
                    Tensor::zeros(node.shape.clone(), node.dtype, node.device.clone(), false)?
                } else {
                    let a_t = resolve_saved_a(node, a_vals, &node.shape)?;
                    let pow_part = t_pow_s(&a_t, *scalar - 1.0, ag)?;
                    t_mul(&t_mul_s(da, *scalar, ag)?, &pow_part, ag)?
                }),
            },
            TapeOp::Neg { a } => match &tangents[*a] {
                None => None,
                Some(da) => Some(t_neg(da, ag)?),
            },
            TapeOp::MatMul {
                a,
                b,
                a_vals,
                b_vals,
                a_shape,
                b_shape,
            } => {
                let ta = a.and_then(|i| tangents[i].as_ref());
                let tb = b.and_then(|i| tangents[i].as_ref());
                match (ta, tb) {
                    (None, None) => None,
                    (Some(da), None) => {
                        let b_t = resolve_saved_b(node, b_vals, b_shape)?;
                        Some(t_matmul(da, &b_t, ag)?)
                    }
                    (None, Some(db)) => {
                        let a_t = resolve_saved_a(node, a_vals, a_shape)?;
                        Some(t_matmul(&a_t, db, ag)?)
                    }
                    (Some(da), Some(db)) => {
                        let a_t = resolve_saved_a(node, a_vals, a_shape)?;
                        let b_t = resolve_saved_b(node, b_vals, b_shape)?;
                        let left = t_matmul(da, &b_t, ag)?;
                        let right = t_matmul(&a_t, db, ag)?;
                        Some(t_add(&left, &right, ag)?)
                    }
                }
            }
            TapeOp::Sum { a, .. } => match &tangents[*a] {
                None => None,
                Some(da) => Some(t_sum(da, ag)?),
            },
            TapeOp::Mean { a, .. } => match &tangents[*a] {
                None => None,
                Some(da) => Some(t_mean(da, ag)?),
            },
            TapeOp::Reshape { a, .. } => match &tangents[*a] {
                None => None,
                Some(da) => Some(t_reshape(da, node.shape.clone(), ag)?),
            },
            TapeOp::Identity { a } => tangents[*a].clone(),
            TapeOp::Transpose { a, dim0, dim1, .. } => match &tangents[*a] {
                None => None,
                Some(da) => Some(t_transpose(da, *dim0, *dim1, ag)?),
            },
            TapeOp::Gather {
                a,
                src_flat_indices,
                ..
            } => match &tangents[*a] {
                None => None,
                Some(da) => Some(t_gather(
                    da,
                    node.shape.clone(),
                    src_flat_indices.clone(),
                    ag,
                )?),
            },
            TapeOp::ScatterAdd {
                a,
                out_shape,
                dst_flat_indices,
            } => match &tangents[*a] {
                None => None,
                Some(da) => Some(t_scatter_add(
                    da,
                    out_shape.clone(),
                    dst_flat_indices.clone(),
                    ag,
                )?),
            },
            TapeOp::SumAxis {
                a, axis, keepdim, ..
            } => match &tangents[*a] {
                None => None,
                Some(da) => Some(t_sum_axis(da, *axis, *keepdim, ag)?),
            },
            TapeOp::Relu { a, a_vals } => match &tangents[*a] {
                None => None,
                Some(da) => {
                    let mask_vals: Vec<f64> = a_vals
                        .iter()
                        .map(|&x| if x > 0.0 { 1.0 } else { 0.0 })
                        .collect();
                    let mask = Tensor::from_raw(
                        mask_vals,
                        node.shape.clone(),
                        node.dtype,
                        node.device.clone(),
                        false,
                    )?;
                    Some(t_mul(da, &mask, ag)?)
                }
            },
            TapeOp::Gelu { a, a_vals } => match &tangents[*a] {
                None => None,
                Some(da) => {
                    let a_t = resolve_saved_a(node, a_vals, &node.shape)?;
                    let deriv = gelu_deriv_tracked(&a_t, ag)?;
                    Some(t_mul(da, &deriv, ag)?)
                }
            },
            TapeOp::Silu { a, a_vals } => match &tangents[*a] {
                None => None,
                Some(da) => {
                    let a_t = resolve_saved_a(node, a_vals, &node.shape)?;
                    let deriv = silu_deriv_tracked(&a_t, ag)?;
                    Some(t_mul(da, &deriv, ag)?)
                }
            },
            TapeOp::Tanh { a, out_vals } => match &tangents[*a] {
                None => None,
                Some(da) => {
                    let y = resolve_saved_out(node, out_vals)?;
                    let deriv = t_rsub_s(1.0, &t_mul(&y, &y, ag)?, ag)?;
                    Some(t_mul(da, &deriv, ag)?)
                }
            },
            TapeOp::Sigmoid { a, out_vals } => match &tangents[*a] {
                None => None,
                Some(da) => {
                    let s = resolve_saved_out(node, out_vals)?;
                    let deriv = t_mul(&s, &t_rsub_s(1.0, &s, ag)?, ag)?;
                    Some(t_mul(da, &deriv, ag)?)
                }
            },
            TapeOp::Exp { a, out_vals } => match &tangents[*a] {
                None => None,
                Some(da) => {
                    let y = resolve_saved_out(node, out_vals)?;
                    Some(t_mul(da, &y, ag)?)
                }
            },
            TapeOp::Ln { a, a_vals } => match &tangents[*a] {
                None => None,
                Some(da) => {
                    let a_t = resolve_saved_a(node, a_vals, &node.shape)?;
                    Some(t_div(da, &a_t, ag)?)
                }
            },
            TapeOp::Sqrt { a, out_vals } => match &tangents[*a] {
                None => None,
                Some(da) => {
                    let y = resolve_saved_out(node, out_vals)?;
                    Some(t_div(da, &t_mul_s(&y, 2.0, ag)?, ag)?)
                }
            },
            TapeOp::Sin { a, a_vals } => match &tangents[*a] {
                None => None,
                Some(da) => {
                    let a_t = resolve_saved_a(node, a_vals, &node.shape)?;
                    Some(t_mul(da, &t_unary(&a_t, "cos", ag)?, ag)?)
                }
            },
            TapeOp::Cos { a, a_vals } => match &tangents[*a] {
                None => None,
                Some(da) => {
                    let a_t = resolve_saved_a(node, a_vals, &node.shape)?;
                    Some(t_neg(&t_mul(da, &t_unary(&a_t, "sin", ag)?, ag)?, ag)?)
                }
            },
            TapeOp::Abs { a, a_vals } => match &tangents[*a] {
                None => None,
                Some(da) => {
                    let sign_vals: Vec<f64> = a_vals
                        .iter()
                        .map(|&x| {
                            if x > 0.0 {
                                1.0
                            } else if x < 0.0 {
                                -1.0
                            } else {
                                0.0
                            }
                        })
                        .collect();
                    let sign = Tensor::from_raw(
                        sign_vals,
                        node.shape.clone(),
                        node.dtype,
                        node.device.clone(),
                        false,
                    )?;
                    Some(t_mul(da, &sign, ag)?)
                }
            },
            TapeOp::Softmax { a, out_vals, .. } => match &tangents[*a] {
                None => None,
                Some(da) => {
                    let y = resolve_saved_out(node, out_vals)?;
                    Some(softmax_jvp_or_vjp_tracked(&y, da, ag)?)
                }
            },
            TapeOp::RmsNorm {
                a,
                a_vals,
                last_dim,
                eps,
            } => match &tangents[*a] {
                None => None,
                Some(da) => {
                    let x = resolve_saved_a(node, a_vals, &node.shape)?;
                    Some(rms_norm_jvp_or_vjp_tracked(&x, da, *last_dim, *eps, ag)?)
                }
            },
            TapeOp::LayerNorm {
                a,
                a_vals,
                last_dim,
                eps,
            } => match &tangents[*a] {
                None => None,
                Some(da) => {
                    let x = resolve_saved_a(node, a_vals, &node.shape)?;
                    Some(layer_norm_jvp_or_vjp_tracked(&x, da, *last_dim, *eps, ag)?)
                }
            },
        };
        tangents[idx] = out_tan;
    }

    match tangents[out_idx].clone() {
        Some(t) => Ok(t),
        None => Tensor::zeros(
            out_node.shape.clone(),
            out_node.dtype,
            out_node.device.clone(),
            false,
        ),
    }
}
