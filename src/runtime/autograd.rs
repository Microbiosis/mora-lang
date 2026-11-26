//! v0.110.0: Reverse-mode Automatic Differentiation (`AutogradContext` / `AutogradTape`).
//!
//! Architecture:
//! - Per-`CoreRuntime` explicit tape stack (zero global or thread-local state, fully isolated
//!   across `spawn`ed concurrent tasks).
//! - Supports arbitrary Mora control flow, `struct`/`impl` layers, multi-dim zero-copy slices,
//!   activations (`relu`/`gelu`/`silu`/`tanh`/`sigmoid`), normalizations (`softmax`/`rms_norm`/
//!   `layer_norm`), contractions (`@` 1D/2D/ND batched), reductions (`sum`/`mean`), and `no_grad`
//!   scoping with unwinding safety.
//! - A tape records every differentiable primitive it sees; reverse-mode VJP accumulation
//!   (`AutogradTape::backward`) and forward-mode JVP propagation (`AutogradTape::forward_jvp`)
//!   replay that tape, so `grad` / `value_and_grad` / `jvp` all read one primitive set.

use std::sync::Arc;

use crate::common::{DType, Device};
use crate::error::{MoraError, MoraResult};
use crate::value::tensor::{RowNorm, Tensor, broadcast_runtime_shapes, checked_numel};

/// Unique handle `(tape_id, node_index)` attached to tracked [`Tensor`]s during differentiation.
pub type TapeNodeId = (u64, usize);

/// Differentiable primitive operations recorded onto an active [`AutogradTape`].
#[derive(Debug, Clone)]
pub enum TapeOp {
    Leaf,
    Add {
        a: Option<usize>,
        b: Option<usize>,
        a_const: Option<Vec<f64>>,
        b_const: Option<Vec<f64>>,
        a_shape: Vec<usize>,
        b_shape: Vec<usize>,
    },
    Sub {
        a: Option<usize>,
        b: Option<usize>,
        a_const: Option<Vec<f64>>,
        b_const: Option<Vec<f64>>,
        a_shape: Vec<usize>,
        b_shape: Vec<usize>,
    },
    Mul {
        a: Option<usize>,
        b: Option<usize>,
        a_vals: Vec<f64>,
        b_vals: Vec<f64>,
        a_shape: Vec<usize>,
        b_shape: Vec<usize>,
    },
    Div {
        a: Option<usize>,
        b: Option<usize>,
        a_vals: Vec<f64>,
        b_vals: Vec<f64>,
        a_shape: Vec<usize>,
        b_shape: Vec<usize>,
    },
    Pow {
        a: Option<usize>,
        b: Option<usize>,
        a_vals: Vec<f64>,
        b_vals: Vec<f64>,
        out_vals: Vec<f64>,
        a_shape: Vec<usize>,
        b_shape: Vec<usize>,
    },
    AddScalar {
        a: usize,
        scalar: f64,
    },
    SubScalar {
        a: usize,
        scalar: f64,
        reversed: bool,
    },
    MulScalar {
        a: usize,
        scalar: f64,
    },
    DivScalar {
        a: usize,
        scalar: f64,
        reversed: bool,
        a_vals: Vec<f64>,
    },
    PowScalar {
        a: usize,
        scalar: f64,
        reversed: bool,
        a_vals: Vec<f64>,
        out_vals: Vec<f64>,
    },
    Neg {
        a: usize,
    },
    MatMul {
        a: Option<usize>,
        b: Option<usize>,
        a_vals: Vec<f64>,
        b_vals: Vec<f64>,
        a_shape: Vec<usize>,
        b_shape: Vec<usize>,
    },
    Sum {
        a: usize,
        a_shape: Vec<usize>,
    },
    Mean {
        a: usize,
        a_shape: Vec<usize>,
    },
    Reshape {
        a: usize,
        orig_shape: Vec<usize>,
    },
    Transpose {
        a: usize,
        dim0: usize,
        dim1: usize,
        orig_shape: Vec<usize>,
    },
    Identity {
        a: usize,
    },
    /// Zero-copy slice or index mapping each output linear slot back to its source flat index.
    Gather {
        a: usize,
        src_numel: usize,
        src_flat_indices: Vec<usize>,
    },
    /// Adjoint of `Gather`: scatters `a` into a zero-initialized tensor of `out_shape` via `dst_flat_indices`.
    ScatterAdd {
        a: usize,
        out_shape: Vec<usize>,
        dst_flat_indices: Vec<usize>,
    },
    /// Sum reduction along a single `axis` (used by tracked unbroadcasting in higher-order AD).
    SumAxis {
        a: usize,
        axis: usize,
        keepdim: bool,
        orig_shape: Vec<usize>,
    },
    Relu {
        a: usize,
        a_vals: Vec<f64>,
    },
    Gelu {
        a: usize,
        a_vals: Vec<f64>,
    },
    Silu {
        a: usize,
        a_vals: Vec<f64>,
    },
    Tanh {
        a: usize,
        out_vals: Vec<f64>,
    },
    Sigmoid {
        a: usize,
        out_vals: Vec<f64>,
    },
    Exp {
        a: usize,
        out_vals: Vec<f64>,
    },
    Ln {
        a: usize,
        a_vals: Vec<f64>,
    },
    Sqrt {
        a: usize,
        out_vals: Vec<f64>,
    },
    Sin {
        a: usize,
        a_vals: Vec<f64>,
    },
    Cos {
        a: usize,
        a_vals: Vec<f64>,
    },
    Abs {
        a: usize,
        a_vals: Vec<f64>,
    },
    Softmax {
        a: usize,
        out_vals: Vec<f64>,
        last_dim: usize,
    },
    RmsNorm {
        a: usize,
        a_vals: Vec<f64>,
        last_dim: usize,
        eps: f64,
    },
    LayerNorm {
        a: usize,
        a_vals: Vec<f64>,
        last_dim: usize,
        eps: f64,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct TapeNode {
    pub(crate) op: TapeOp,
    pub(crate) shape: Vec<usize>,
    pub(crate) dtype: DType,
    pub(crate) device: Device,
    /// Saved outer-tape-tracked operands/output populated only when nested tapes (`tapes.len() > 1`)
    /// are active, enabling exact arbitrary-order AD (`grad(grad(f))`, `jvp(grad(f), ...)`, `trace_dag(grad(f))`).
    pub(crate) saved_a: Option<Arc<Tensor>>,
    pub(crate) saved_b: Option<Arc<Tensor>>,
    pub(crate) saved_out: Option<Arc<Tensor>>,
}

/// One reverse-mode Wengert tape corresponding to a single `grad` / `value_and_grad` / `jvp` / `trace_dag` call.
#[derive(Debug, Clone)]
pub struct AutogradTape {
    pub id: u64,
    pub(crate) nodes: Vec<TapeNode>,
}

impl AutogradTape {
    fn new(id: u64) -> Self {
        Self {
            id,
            nodes: Vec::new(),
        }
    }

    pub(crate) fn nodes(&self) -> &[TapeNode] {
        &self.nodes
    }

    fn push_node(&mut self, op: TapeOp, shape: Vec<usize>, dtype: DType, device: Device) -> usize {
        let idx = self.nodes.len();
        self.nodes.push(TapeNode {
            op,
            shape,
            dtype,
            device,
            saved_a: None,
            saved_b: None,
            saved_out: None,
        });
        idx
    }

    /// Execute higher-order tracked reverse-mode VJP accumulation onto enclosing active tapes.
    pub(crate) fn backward_tracked(
        &self,
        out_idx: usize,
        leaf_indices: &[usize],
        autograd: &mut AutogradContext,
    ) -> MoraResult<Vec<Arc<Tensor>>> {
        crate::runtime::autograd_higher_order::backward_tracked(
            self,
            out_idx,
            leaf_indices,
            autograd,
        )
    }

    /// Execute higher-order tracked forward-mode JVP propagation onto enclosing active tapes.
    pub(crate) fn forward_jvp_tracked(
        &self,
        out_idx: usize,
        leaf_indices: &[usize],
        leaf_tangents: &[Arc<Tensor>],
        autograd: &mut AutogradContext,
    ) -> MoraResult<Arc<Tensor>> {
        crate::runtime::autograd_higher_order::forward_jvp_tracked(
            self,
            out_idx,
            leaf_indices,
            leaf_tangents,
            autograd,
        )
    }

    /// Build zero-gradient tensors matching each leaf node's shape, dtype, and device
    /// (used when a differentiated function returns a constant or `no_grad`-detached scalar).
    pub fn zero_grads_for_leaves(&self, leaf_indices: &[usize]) -> MoraResult<Vec<Arc<Tensor>>> {
        let mut out = Vec::with_capacity(leaf_indices.len());
        for &li in leaf_indices {
            let leaf = self.nodes.get(li).ok_or_else(|| {
                MoraError::Runtime(format!("autograd: leaf node {li} out of bounds"))
            })?;
            out.push(Tensor::zeros(
                leaf.shape.clone(),
                leaf.dtype,
                leaf.device.clone(),
                false,
            )?);
        }
        Ok(out)
    }

    /// Execute reverse-mode VJP accumulation from scalar output `out_idx` back to all `leaf_indices`.
    pub fn backward(&self, out_idx: usize, leaf_indices: &[usize]) -> MoraResult<Vec<Arc<Tensor>>> {
        let out_node = self.nodes.get(out_idx).ok_or_else(|| {
            MoraError::Runtime(format!("autograd: output node {out_idx} out of bounds"))
        })?;
        let out_numel = checked_numel(&out_node.shape)?;
        if out_numel != 1 {
            return Err(MoraError::Runtime(format!(
                "grad: differentiated function must return a scalar (0-D or 1-element tensor), got shape {:?}",
                out_node.shape
            )));
        }

        let mut grads: Vec<Option<Vec<f64>>> = vec![None; self.nodes.len()];
        grads[out_idx] = Some(vec![1.0]);

        for idx in (0..=out_idx).rev() {
            let Some(g_out) = grads[idx].clone() else {
                continue;
            };
            let node = &self.nodes[idx];
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
                        let ga = unbroadcast(&g_out, &node.shape, a_shape);
                        accum_grad(&mut grads, ia, &ga);
                    }
                    if let Some(ib) = *b {
                        let gb = unbroadcast(&g_out, &node.shape, b_shape);
                        accum_grad(&mut grads, ib, &gb);
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
                        let ga = unbroadcast(&g_out, &node.shape, a_shape);
                        accum_grad(&mut grads, ia, &ga);
                    }
                    if let Some(ib) = *b {
                        let neg_g: Vec<f64> = g_out.iter().map(|v| -v).collect();
                        let gb = unbroadcast(&neg_g, &node.shape, b_shape);
                        accum_grad(&mut grads, ib, &gb);
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
                        let full: Vec<f64> = g_out
                            .iter()
                            .enumerate()
                            .map(|(k, &g)| g * read_broadcast_flat(b_vals, b_shape, &node.shape, k))
                            .collect();
                        let ga = unbroadcast(&full, &node.shape, a_shape);
                        accum_grad(&mut grads, ia, &ga);
                    }
                    if let Some(ib) = *b {
                        let full: Vec<f64> = g_out
                            .iter()
                            .enumerate()
                            .map(|(k, &g)| g * read_broadcast_flat(a_vals, a_shape, &node.shape, k))
                            .collect();
                        let gb = unbroadcast(&full, &node.shape, b_shape);
                        accum_grad(&mut grads, ib, &gb);
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
                    if let Some(ia) = *a {
                        let full: Vec<f64> = g_out
                            .iter()
                            .enumerate()
                            .map(|(k, &g)| g / read_broadcast_flat(b_vals, b_shape, &node.shape, k))
                            .collect();
                        let ga = unbroadcast(&full, &node.shape, a_shape);
                        accum_grad(&mut grads, ia, &ga);
                    }
                    if let Some(ib) = *b {
                        let full: Vec<f64> = g_out
                            .iter()
                            .enumerate()
                            .map(|(k, &g)| {
                                let av = read_broadcast_flat(a_vals, a_shape, &node.shape, k);
                                let bv = read_broadcast_flat(b_vals, b_shape, &node.shape, k);
                                -g * av / (bv * bv)
                            })
                            .collect();
                        let gb = unbroadcast(&full, &node.shape, b_shape);
                        accum_grad(&mut grads, ib, &gb);
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
                    if let Some(ia) = *a {
                        let full: Vec<f64> = g_out
                            .iter()
                            .enumerate()
                            .map(|(k, &g)| {
                                let av = read_broadcast_flat(a_vals, a_shape, &node.shape, k);
                                let bv = read_broadcast_flat(b_vals, b_shape, &node.shape, k);
                                if bv == 0.0 {
                                    0.0
                                } else {
                                    g * bv * av.powf(bv - 1.0)
                                }
                            })
                            .collect();
                        let ga = unbroadcast(&full, &node.shape, a_shape);
                        accum_grad(&mut grads, ia, &ga);
                    }
                    if let Some(ib) = *b {
                        let full: Vec<f64> = g_out
                            .iter()
                            .enumerate()
                            .map(|(k, &g)| {
                                let av = read_broadcast_flat(a_vals, a_shape, &node.shape, k);
                                if av > 0.0 {
                                    g * out_vals[k] * av.ln()
                                } else {
                                    0.0
                                }
                            })
                            .collect();
                        let gb = unbroadcast(&full, &node.shape, b_shape);
                        accum_grad(&mut grads, ib, &gb);
                    }
                }
                TapeOp::AddScalar { a, .. } => {
                    accum_grad(&mut grads, *a, &g_out);
                }
                TapeOp::SubScalar { a, reversed, .. } => {
                    if *reversed {
                        let neg: Vec<f64> = g_out.iter().map(|v| -v).collect();
                        accum_grad(&mut grads, *a, &neg);
                    } else {
                        accum_grad(&mut grads, *a, &g_out);
                    }
                }
                TapeOp::MulScalar { a, scalar } => {
                    let ga: Vec<f64> = g_out.iter().map(|&g| g * *scalar).collect();
                    accum_grad(&mut grads, *a, &ga);
                }
                TapeOp::DivScalar {
                    a,
                    scalar,
                    reversed,
                    a_vals,
                } => {
                    let ga: Vec<f64> = if *reversed {
                        g_out
                            .iter()
                            .zip(a_vals.iter())
                            .map(|(&g, &av)| -g * *scalar / (av * av))
                            .collect()
                    } else {
                        g_out.iter().map(|&g| g / *scalar).collect()
                    };
                    accum_grad(&mut grads, *a, &ga);
                }
                TapeOp::PowScalar {
                    a,
                    scalar,
                    reversed,
                    a_vals,
                    out_vals,
                } => {
                    let ga: Vec<f64> = if *reversed {
                        let lns = if *scalar > 0.0 { scalar.ln() } else { 0.0 };
                        g_out
                            .iter()
                            .zip(out_vals.iter())
                            .map(|(&g, &ov)| g * ov * lns)
                            .collect()
                    } else {
                        g_out
                            .iter()
                            .zip(a_vals.iter())
                            .map(|(&g, &av)| {
                                if *scalar == 0.0 {
                                    0.0
                                } else {
                                    g * *scalar * av.powf(*scalar - 1.0)
                                }
                            })
                            .collect()
                    };
                    accum_grad(&mut grads, *a, &ga);
                }
                TapeOp::Neg { a } => {
                    let ga: Vec<f64> = g_out.iter().map(|&g| -g).collect();
                    accum_grad(&mut grads, *a, &ga);
                }
                TapeOp::MatMul {
                    a,
                    b,
                    a_vals,
                    b_vals,
                    a_shape,
                    b_shape,
                } => {
                    let (ga_opt, gb_opt) = matmul_vjp(
                        &g_out,
                        a_vals,
                        b_vals,
                        a_shape,
                        b_shape,
                        a.is_some(),
                        b.is_some(),
                    )?;
                    if let (Some(ia), Some(ga)) = (*a, ga_opt) {
                        accum_grad(&mut grads, ia, &ga);
                    }
                    if let (Some(ib), Some(gb)) = (*b, gb_opt) {
                        accum_grad(&mut grads, ib, &gb);
                    }
                }
                TapeOp::Sum { a, a_shape } => {
                    let n = checked_numel(a_shape)?;
                    let g0 = g_out.first().copied().unwrap_or(0.0);
                    accum_grad(&mut grads, *a, &vec![g0; n]);
                }
                TapeOp::Mean { a, a_shape } => {
                    let n = checked_numel(a_shape)?;
                    let g0 = g_out.first().copied().unwrap_or(0.0) / (n as f64);
                    accum_grad(&mut grads, *a, &vec![g0; n]);
                }
                TapeOp::Reshape { a, .. } | TapeOp::Identity { a } => {
                    accum_grad(&mut grads, *a, &g_out);
                }
                TapeOp::Transpose {
                    a,
                    dim0,
                    dim1,
                    orig_shape,
                } => {
                    let ga = transpose_flat(&g_out, &node.shape, *dim0, *dim1, orig_shape);
                    accum_grad(&mut grads, *a, &ga);
                }
                TapeOp::Gather {
                    a,
                    src_numel,
                    src_flat_indices,
                } => {
                    let ga = scatter_add_flat(&g_out, *src_numel, src_flat_indices);
                    accum_grad(&mut grads, *a, &ga);
                }
                TapeOp::ScatterAdd {
                    a,
                    dst_flat_indices,
                    ..
                } => {
                    let ga: Vec<f64> = dst_flat_indices
                        .iter()
                        .map(|&di| g_out.get(di).copied().unwrap_or(0.0))
                        .collect();
                    accum_grad(&mut grads, *a, &ga);
                }
                TapeOp::SumAxis {
                    a,
                    axis,
                    keepdim,
                    orig_shape,
                } => {
                    let ga = expand_axis_flat(&g_out, &node.shape, *axis, *keepdim, orig_shape);
                    accum_grad(&mut grads, *a, &ga);
                }
                TapeOp::Relu { a, a_vals } => {
                    let ga: Vec<f64> = g_out
                        .iter()
                        .zip(a_vals.iter())
                        .map(|(&g, &x)| if x > 0.0 { g } else { 0.0 })
                        .collect();
                    accum_grad(&mut grads, *a, &ga);
                }
                TapeOp::Gelu { a, a_vals } => {
                    let c = (2.0 / std::f64::consts::PI).sqrt();
                    let ga: Vec<f64> = g_out
                        .iter()
                        .zip(a_vals.iter())
                        .map(|(&g, &x)| {
                            let u = c * (x + 0.044715 * x.powi(3));
                            let th = u.tanh();
                            let sech2 = 1.0 - th * th;
                            let du_dx = c * (1.0 + 3.0 * 0.044715 * x * x);
                            g * (0.5 * (1.0 + th) + 0.5 * x * sech2 * du_dx)
                        })
                        .collect();
                    accum_grad(&mut grads, *a, &ga);
                }
                TapeOp::Silu { a, a_vals } => {
                    let ga: Vec<f64> = g_out
                        .iter()
                        .zip(a_vals.iter())
                        .map(|(&g, &x)| {
                            let s = 1.0 / (1.0 + (-x).exp());
                            g * (s * (1.0 + x * (1.0 - s)))
                        })
                        .collect();
                    accum_grad(&mut grads, *a, &ga);
                }
                TapeOp::Tanh { a, out_vals } => {
                    let ga: Vec<f64> = g_out
                        .iter()
                        .zip(out_vals.iter())
                        .map(|(&g, &y)| g * (1.0 - y * y))
                        .collect();
                    accum_grad(&mut grads, *a, &ga);
                }
                TapeOp::Sigmoid { a, out_vals } => {
                    let ga: Vec<f64> = g_out
                        .iter()
                        .zip(out_vals.iter())
                        .map(|(&g, &s)| g * s * (1.0 - s))
                        .collect();
                    accum_grad(&mut grads, *a, &ga);
                }
                TapeOp::Exp { a, out_vals } => {
                    let ga: Vec<f64> = g_out
                        .iter()
                        .zip(out_vals.iter())
                        .map(|(&g, &y)| g * y)
                        .collect();
                    accum_grad(&mut grads, *a, &ga);
                }
                TapeOp::Ln { a, a_vals } => {
                    let ga: Vec<f64> = g_out
                        .iter()
                        .zip(a_vals.iter())
                        .map(|(&g, &x)| g / x)
                        .collect();
                    accum_grad(&mut grads, *a, &ga);
                }
                TapeOp::Sqrt { a, out_vals } => {
                    let ga: Vec<f64> = g_out
                        .iter()
                        .zip(out_vals.iter())
                        .map(|(&g, &y)| g / (2.0 * y))
                        .collect();
                    accum_grad(&mut grads, *a, &ga);
                }
                TapeOp::Sin { a, a_vals } => {
                    let ga: Vec<f64> = g_out
                        .iter()
                        .zip(a_vals.iter())
                        .map(|(&g, &x)| g * x.cos())
                        .collect();
                    accum_grad(&mut grads, *a, &ga);
                }
                TapeOp::Cos { a, a_vals } => {
                    let ga: Vec<f64> = g_out
                        .iter()
                        .zip(a_vals.iter())
                        .map(|(&g, &x)| -g * x.sin())
                        .collect();
                    accum_grad(&mut grads, *a, &ga);
                }
                TapeOp::Abs { a, a_vals } => {
                    let ga: Vec<f64> = g_out
                        .iter()
                        .zip(a_vals.iter())
                        .map(|(&g, &x)| {
                            if x > 0.0 {
                                g
                            } else if x < 0.0 {
                                -g
                            } else {
                                0.0
                            }
                        })
                        .collect();
                    accum_grad(&mut grads, *a, &ga);
                }
                TapeOp::Softmax {
                    a,
                    out_vals,
                    last_dim,
                } => {
                    let d = (*last_dim).max(1);
                    let mut ga = vec![0.0; g_out.len()];
                    for row in 0..(g_out.len() / d) {
                        let base = row * d;
                        let dot: f64 = (0..d).map(|j| g_out[base + j] * out_vals[base + j]).sum();
                        for j in 0..d {
                            ga[base + j] = out_vals[base + j] * (g_out[base + j] - dot);
                        }
                    }
                    accum_grad(&mut grads, *a, &ga);
                }
                TapeOp::RmsNorm {
                    a,
                    a_vals,
                    last_dim,
                    eps,
                } => {
                    let ga = RowNorm::Rms.differential(a_vals, &g_out, *last_dim, *eps);
                    accum_grad(&mut grads, *a, &ga);
                }
                TapeOp::LayerNorm {
                    a,
                    a_vals,
                    last_dim,
                    eps,
                } => {
                    let ga = RowNorm::Layer.differential(a_vals, &g_out, *last_dim, *eps);
                    accum_grad(&mut grads, *a, &ga);
                }
            }
        }

        let mut out = Vec::with_capacity(leaf_indices.len());
        for &leaf_idx in leaf_indices {
            let leaf = &self.nodes[leaf_idx];
            let numel = checked_numel(&leaf.shape)?;
            let data = grads[leaf_idx].clone().unwrap_or_else(|| vec![0.0; numel]);
            let t = Tensor::from_raw(
                data,
                leaf.shape.clone(),
                leaf.dtype,
                leaf.device.clone(),
                false,
            )?;
            out.push(t);
        }
        Ok(out)
    }

    /// Execute forward-mode Jacobian-vector product (JVP) from `leaf_indices` seeded with
    /// `leaf_tangents` forward to `out_idx`, returning the directional derivative `Tensor`.
    pub fn forward_jvp(
        &self,
        out_idx: usize,
        leaf_indices: &[usize],
        leaf_tangents: &[Arc<Tensor>],
    ) -> MoraResult<Arc<Tensor>> {
        let out_node = self.nodes.get(out_idx).ok_or_else(|| {
            MoraError::Runtime(format!("jvp: output node {out_idx} out of bounds"))
        })?;
        if leaf_indices.len() != leaf_tangents.len() {
            return Err(MoraError::Runtime(format!(
                "jvp: expected {} tangent(s) matching leaf count, got {}",
                leaf_indices.len(),
                leaf_tangents.len()
            )));
        }

        let mut tangents: Vec<Option<Vec<f64>>> = vec![None; self.nodes.len()];
        for (&li, tan) in leaf_indices.iter().zip(leaf_tangents.iter()) {
            let leaf = self
                .nodes
                .get(li)
                .ok_or_else(|| MoraError::Runtime(format!("jvp: leaf node {li} out of bounds")))?;
            if leaf.shape != tan.shape {
                return Err(MoraError::Runtime(format!(
                    "jvp: tangent shape {:?} must match primal shape {:?}",
                    tan.shape, leaf.shape
                )));
            }
            tangents[li] = Some(tan.to_flat_vec());
        }

        for idx in 0..=out_idx {
            let node = &self.nodes[idx];
            let out_numel = checked_numel(&node.shape)?;
            let out_tan: Option<Vec<f64>> = match &node.op {
                TapeOp::Leaf => tangents[idx].clone(),
                TapeOp::Add {
                    a,
                    b,
                    a_shape,
                    b_shape,
                    ..
                } => {
                    let ta = a.and_then(|i| tangents[i].as_ref());
                    let tb = b.and_then(|i| tangents[i].as_ref());
                    match (ta, tb) {
                        (None, None) => None,
                        (Some(da), None) => Some(
                            (0..out_numel)
                                .map(|k| read_broadcast_flat(da, a_shape, &node.shape, k))
                                .collect(),
                        ),
                        (None, Some(db)) => Some(
                            (0..out_numel)
                                .map(|k| read_broadcast_flat(db, b_shape, &node.shape, k))
                                .collect(),
                        ),
                        (Some(da), Some(db)) => Some(
                            (0..out_numel)
                                .map(|k| {
                                    read_broadcast_flat(da, a_shape, &node.shape, k)
                                        + read_broadcast_flat(db, b_shape, &node.shape, k)
                                })
                                .collect(),
                        ),
                    }
                }
                TapeOp::Sub {
                    a,
                    b,
                    a_shape,
                    b_shape,
                    ..
                } => {
                    let ta = a.and_then(|i| tangents[i].as_ref());
                    let tb = b.and_then(|i| tangents[i].as_ref());
                    match (ta, tb) {
                        (None, None) => None,
                        (Some(da), None) => Some(
                            (0..out_numel)
                                .map(|k| read_broadcast_flat(da, a_shape, &node.shape, k))
                                .collect(),
                        ),
                        (None, Some(db)) => Some(
                            (0..out_numel)
                                .map(|k| -read_broadcast_flat(db, b_shape, &node.shape, k))
                                .collect(),
                        ),
                        (Some(da), Some(db)) => Some(
                            (0..out_numel)
                                .map(|k| {
                                    read_broadcast_flat(da, a_shape, &node.shape, k)
                                        - read_broadcast_flat(db, b_shape, &node.shape, k)
                                })
                                .collect(),
                        ),
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
                    if ta.is_none() && tb.is_none() {
                        None
                    } else {
                        Some(
                            (0..out_numel)
                                .map(|k| {
                                    let da = ta
                                        .map(|d| read_broadcast_flat(d, a_shape, &node.shape, k))
                                        .unwrap_or(0.0);
                                    let db = tb
                                        .map(|d| read_broadcast_flat(d, b_shape, &node.shape, k))
                                        .unwrap_or(0.0);
                                    let av = read_broadcast_flat(a_vals, a_shape, &node.shape, k);
                                    let bv = read_broadcast_flat(b_vals, b_shape, &node.shape, k);
                                    da * bv + av * db
                                })
                                .collect(),
                        )
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
                    if ta.is_none() && tb.is_none() {
                        None
                    } else {
                        Some(
                            (0..out_numel)
                                .map(|k| {
                                    let da = ta
                                        .map(|d| read_broadcast_flat(d, a_shape, &node.shape, k))
                                        .unwrap_or(0.0);
                                    let db = tb
                                        .map(|d| read_broadcast_flat(d, b_shape, &node.shape, k))
                                        .unwrap_or(0.0);
                                    let av = read_broadcast_flat(a_vals, a_shape, &node.shape, k);
                                    let bv = read_broadcast_flat(b_vals, b_shape, &node.shape, k);
                                    (da * bv - av * db) / (bv * bv)
                                })
                                .collect(),
                        )
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
                        Some(
                            (0..out_numel)
                                .map(|k| {
                                    let da = ta
                                        .map(|d| read_broadcast_flat(d, a_shape, &node.shape, k))
                                        .unwrap_or(0.0);
                                    let db = tb
                                        .map(|d| read_broadcast_flat(d, b_shape, &node.shape, k))
                                        .unwrap_or(0.0);
                                    let av = read_broadcast_flat(a_vals, a_shape, &node.shape, k);
                                    let bv = read_broadcast_flat(b_vals, b_shape, &node.shape, k);
                                    let term_a = if da == 0.0 || bv == 0.0 {
                                        0.0
                                    } else {
                                        da * bv * av.powf(bv - 1.0)
                                    };
                                    let term_b = if db == 0.0 || av <= 0.0 {
                                        0.0
                                    } else {
                                        db * out_vals[k] * av.ln()
                                    };
                                    term_a + term_b
                                })
                                .collect(),
                        )
                    }
                }
                TapeOp::AddScalar { a, .. } => tangents[*a].clone(),
                TapeOp::SubScalar { a, reversed, .. } => tangents[*a].as_ref().map(|da| {
                    if *reversed {
                        da.iter().map(|v| -v).collect()
                    } else {
                        da.clone()
                    }
                }),
                TapeOp::MulScalar { a, scalar } => tangents[*a]
                    .as_ref()
                    .map(|da| da.iter().map(|&v| v * *scalar).collect()),
                TapeOp::DivScalar {
                    a,
                    scalar,
                    reversed,
                    a_vals,
                } => tangents[*a].as_ref().map(|da| {
                    if *reversed {
                        da.iter()
                            .zip(a_vals.iter())
                            .map(|(&d, &x)| -d * *scalar / (x * x))
                            .collect()
                    } else {
                        da.iter().map(|&d| d / *scalar).collect()
                    }
                }),
                TapeOp::PowScalar {
                    a,
                    scalar,
                    reversed,
                    a_vals,
                    out_vals,
                } => tangents[*a].as_ref().map(|da| {
                    if *reversed {
                        let ln_s = if *scalar > 0.0 { scalar.ln() } else { 0.0 };
                        da.iter()
                            .zip(out_vals.iter())
                            .map(|(&d, &y)| d * y * ln_s)
                            .collect()
                    } else {
                        da.iter()
                            .zip(a_vals.iter())
                            .map(|(&d, &x)| {
                                if *scalar == 0.0 {
                                    0.0
                                } else {
                                    d * *scalar * x.powf(*scalar - 1.0)
                                }
                            })
                            .collect()
                    }
                }),
                TapeOp::Neg { a } => tangents[*a]
                    .as_ref()
                    .map(|da| da.iter().map(|v| -v).collect()),
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
                        (Some(da), None) => Some(matmul_f64(da, b_vals, a_shape, b_shape)?),
                        (None, Some(db)) => Some(matmul_f64(a_vals, db, a_shape, b_shape)?),
                        (Some(da), Some(db)) => {
                            let left = matmul_f64(da, b_vals, a_shape, b_shape)?;
                            let right = matmul_f64(a_vals, db, a_shape, b_shape)?;
                            Some(left.into_iter().zip(right).map(|(x, y)| x + y).collect())
                        }
                    }
                }
                TapeOp::Sum { a, .. } => tangents[*a].as_ref().map(|da| vec![da.iter().sum()]),
                TapeOp::Mean { a, a_shape } => {
                    let n = checked_numel(a_shape)?.max(1) as f64;
                    tangents[*a]
                        .as_ref()
                        .map(|da| vec![da.iter().sum::<f64>() / n])
                }
                TapeOp::Reshape { a, .. } | TapeOp::Identity { a } => tangents[*a].clone(),
                TapeOp::Transpose {
                    a,
                    dim0,
                    dim1,
                    orig_shape,
                } => tangents[*a]
                    .as_ref()
                    .map(|da| transpose_flat(da, orig_shape, *dim0, *dim1, &node.shape)),
                TapeOp::Gather {
                    a,
                    src_flat_indices,
                    ..
                } => tangents[*a].as_ref().map(|da| {
                    src_flat_indices
                        .iter()
                        .map(|&si| da.get(si).copied().unwrap_or(0.0))
                        .collect()
                }),
                TapeOp::ScatterAdd {
                    a,
                    out_shape,
                    dst_flat_indices,
                } => {
                    let out_n = checked_numel(out_shape)?;
                    tangents[*a]
                        .as_ref()
                        .map(|da| scatter_add_flat(da, out_n, dst_flat_indices))
                }
                TapeOp::SumAxis {
                    a,
                    axis,
                    keepdim,
                    orig_shape,
                } => tangents[*a]
                    .as_ref()
                    .map(|da| sum_axis_flat(da, orig_shape, *axis, *keepdim)),
                TapeOp::Relu { a, a_vals } => tangents[*a].as_ref().map(|da| {
                    da.iter()
                        .zip(a_vals.iter())
                        .map(|(&d, &x)| if x > 0.0 { d } else { 0.0 })
                        .collect()
                }),
                TapeOp::Gelu { a, a_vals } => {
                    let c = (2.0_f64 / std::f64::consts::PI).sqrt();
                    tangents[*a].as_ref().map(|da| {
                        da.iter()
                            .zip(a_vals.iter())
                            .map(|(&d, &x)| {
                                let u = c * (x + 0.044715 * x.powi(3));
                                let th = u.tanh();
                                let sech2 = 1.0 - th * th;
                                let du_dx = c * (1.0 + 3.0 * 0.044715 * x * x);
                                d * (0.5 * (1.0 + th) + 0.5 * x * sech2 * du_dx)
                            })
                            .collect()
                    })
                }
                TapeOp::Silu { a, a_vals } => tangents[*a].as_ref().map(|da| {
                    da.iter()
                        .zip(a_vals.iter())
                        .map(|(&d, &x)| {
                            let s = 1.0 / (1.0 + (-x).exp());
                            d * (s * (1.0 + x * (1.0 - s)))
                        })
                        .collect()
                }),
                TapeOp::Tanh { a, out_vals } => tangents[*a].as_ref().map(|da| {
                    da.iter()
                        .zip(out_vals.iter())
                        .map(|(&d, &y)| d * (1.0 - y * y))
                        .collect()
                }),
                TapeOp::Sigmoid { a, out_vals } => tangents[*a].as_ref().map(|da| {
                    da.iter()
                        .zip(out_vals.iter())
                        .map(|(&d, &s)| d * s * (1.0 - s))
                        .collect()
                }),
                TapeOp::Exp { a, out_vals } => tangents[*a].as_ref().map(|da| {
                    da.iter()
                        .zip(out_vals.iter())
                        .map(|(&d, &y)| d * y)
                        .collect()
                }),
                TapeOp::Ln { a, a_vals } => tangents[*a]
                    .as_ref()
                    .map(|da| da.iter().zip(a_vals.iter()).map(|(&d, &x)| d / x).collect()),
                TapeOp::Sqrt { a, out_vals } => tangents[*a].as_ref().map(|da| {
                    da.iter()
                        .zip(out_vals.iter())
                        .map(|(&d, &y)| d / (2.0 * y))
                        .collect()
                }),
                TapeOp::Sin { a, a_vals } => tangents[*a].as_ref().map(|da| {
                    da.iter()
                        .zip(a_vals.iter())
                        .map(|(&d, &x)| d * x.cos())
                        .collect()
                }),
                TapeOp::Cos { a, a_vals } => tangents[*a].as_ref().map(|da| {
                    da.iter()
                        .zip(a_vals.iter())
                        .map(|(&d, &x)| -d * x.sin())
                        .collect()
                }),
                TapeOp::Abs { a, a_vals } => tangents[*a].as_ref().map(|da| {
                    da.iter()
                        .zip(a_vals.iter())
                        .map(|(&d, &x)| {
                            if x > 0.0 {
                                d
                            } else if x < 0.0 {
                                -d
                            } else {
                                0.0
                            }
                        })
                        .collect()
                }),
                TapeOp::Softmax {
                    a,
                    out_vals,
                    last_dim,
                } => tangents[*a].as_ref().map(|da| {
                    let d = (*last_dim).max(1);
                    let mut out = vec![0.0; da.len()];
                    for row in 0..(da.len() / d) {
                        let base = row * d;
                        let dot: f64 = (0..d).map(|j| da[base + j] * out_vals[base + j]).sum();
                        for j in 0..d {
                            out[base + j] = out_vals[base + j] * (da[base + j] - dot);
                        }
                    }
                    out
                }),
                TapeOp::RmsNorm {
                    a,
                    a_vals,
                    last_dim,
                    eps,
                } => tangents[*a]
                    .as_ref()
                    .map(|da| RowNorm::Rms.differential(a_vals, da, *last_dim, *eps)),
                TapeOp::LayerNorm {
                    a,
                    a_vals,
                    last_dim,
                    eps,
                } => tangents[*a]
                    .as_ref()
                    .map(|da| RowNorm::Layer.differential(a_vals, da, *last_dim, *eps)),
            };
            tangents[idx] = out_tan;
        }

        let out_numel = checked_numel(&out_node.shape)?;
        let data = tangents[out_idx]
            .clone()
            .unwrap_or_else(|| vec![0.0; out_numel]);
        Tensor::from_raw(
            data,
            out_node.shape.clone(),
            out_node.dtype,
            out_node.device.clone(),
            false,
        )
    }
}

/// Per-`CoreRuntime` autograd tape stack and `no_grad` nesting depth.
#[derive(Debug, Clone, Default)]
pub struct AutogradContext {
    next_tape_id: u64,
    no_grad_depth: usize,
    tapes: Vec<AutogradTape>,
}

impl AutogradContext {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn is_recording(&self) -> bool {
        self.no_grad_depth == 0 && !self.tapes.is_empty()
    }

    #[inline]
    pub fn active_tape_id(&self) -> Option<u64> {
        if self.no_grad_depth > 0 {
            return None;
        }
        self.tapes.last().map(|t| t.id)
    }

    pub fn enter_no_grad(&mut self) {
        self.no_grad_depth += 1;
    }

    pub fn exit_no_grad(&mut self) {
        self.no_grad_depth = self.no_grad_depth.saturating_sub(1);
    }

    pub fn push_tape(&mut self) -> u64 {
        self.next_tape_id += 1;
        let id = self.next_tape_id;
        self.tapes.push(AutogradTape::new(id));
        id
    }

    pub fn pop_tape(&mut self, tape_id: u64) -> Option<AutogradTape> {
        if let Some(pos) = self.tapes.iter().rposition(|t| t.id == tape_id) {
            Some(self.tapes.remove(pos))
        } else {
            None
        }
    }

    pub fn register_leaf(
        &mut self,
        tape_id: u64,
        shape: Vec<usize>,
        dtype: DType,
        device: Device,
    ) -> Option<TapeNodeId> {
        let tape = self.tapes.iter_mut().rev().find(|t| t.id == tape_id)?;
        let idx = tape.push_node(TapeOp::Leaf, shape, dtype, device);
        Some((tape_id, idx))
    }

    /// Check whether `tensor` is tracked on ANY currently active tape (and `no_grad` is inactive).
    #[inline]
    pub(crate) fn is_tracked(&self, tensor: &Tensor) -> bool {
        if !self.is_recording() || tensor.grad_nodes.is_empty() {
            return false;
        }
        self.tapes
            .iter()
            .any(|t| tensor.node_on_tape(t.id).is_some())
    }

    /// True when `tensor` is attached to ANY live tape on `self.tapes` — regardless of
    /// `no_grad_depth` — so a nested `no_grad` block inside an outer `grad` / `jvp` / `trace_dag`
    /// never mutates an outer-tape tensor's storage in place, while standalone `no_grad` inference
    /// and optimizer steps (`tapes.is_empty()`) safely reuse uniquely-owned `requires_grad = true`
    /// tensors in place.
    #[inline]
    pub(crate) fn has_live_tape_node(&self, tensor: &Tensor) -> bool {
        !self.tapes.is_empty()
            && !tensor.grad_nodes.is_empty()
            && self
                .tapes
                .iter()
                .any(|t| tensor.node_on_tape(t.id).is_some())
    }

    /// Record a unary operation on EVERY active tape tracking `input`, returning `out` attached to
    /// all newly recorded `(tape_id, node_idx)` entries. When nested tapes (`tapes.len() > 1`) are
    /// active, saves `input` and the tracked `out` on inner tapes to enable exact arbitrary-order AD.
    pub(crate) fn record_unary(
        &mut self,
        input: &Arc<Tensor>,
        out: Arc<Tensor>,
        mut make_op: impl FnMut(usize) -> TapeOp,
    ) -> Arc<Tensor> {
        if !self.is_recording() || input.grad_nodes.is_empty() {
            return out;
        }
        let multi = self.tapes.len() > 1;
        let mut new_nodes = Vec::with_capacity(self.tapes.len());
        let mut inner_locs = Vec::new();
        for (t_pos, tape) in self.tapes.iter_mut().enumerate() {
            if let Some(src_idx) = input.node_on_tape(tape.id) {
                let op = make_op(src_idx);
                let node_idx = tape.push_node(op, out.shape.clone(), out.dtype, out.device.clone());
                new_nodes.push((tape.id, node_idx));
                if multi && t_pos > 0 {
                    inner_locs.push((t_pos, node_idx));
                }
            }
        }
        if new_nodes.is_empty() {
            return out;
        }
        let tracked_out = out.with_grad_nodes(new_nodes);
        for (t_pos, node_idx) in inner_locs {
            let node = &mut self.tapes[t_pos].nodes[node_idx];
            node.saved_a = Some(Arc::clone(input));
            node.saved_out = Some(Arc::clone(&tracked_out));
        }
        tracked_out
    }

    /// Record a binary operation on EVERY active tape tracking `a` or `b`, returning `out` attached
    /// to all newly recorded `(tape_id, node_idx)` entries.
    pub(crate) fn record_binary(
        &mut self,
        a: &Arc<Tensor>,
        b: &Arc<Tensor>,
        out: Arc<Tensor>,
        mut make_op: impl FnMut(Option<usize>, Option<usize>) -> TapeOp,
    ) -> Arc<Tensor> {
        if !self.is_recording() || (a.grad_nodes.is_empty() && b.grad_nodes.is_empty()) {
            return out;
        }
        let multi = self.tapes.len() > 1;
        let mut new_nodes = Vec::with_capacity(self.tapes.len());
        let mut inner_locs = Vec::new();
        for (t_pos, tape) in self.tapes.iter_mut().enumerate() {
            let na = a.node_on_tape(tape.id);
            let nb = b.node_on_tape(tape.id);
            if na.is_some() || nb.is_some() {
                let op = make_op(na, nb);
                let node_idx = tape.push_node(op, out.shape.clone(), out.dtype, out.device.clone());
                new_nodes.push((tape.id, node_idx));
                if multi && t_pos > 0 {
                    inner_locs.push((t_pos, node_idx));
                }
            }
        }
        if new_nodes.is_empty() {
            return out;
        }
        let tracked_out = out.with_grad_nodes(new_nodes);
        for (t_pos, node_idx) in inner_locs {
            let node = &mut self.tapes[t_pos].nodes[node_idx];
            node.saved_a = Some(Arc::clone(a));
            node.saved_b = Some(Arc::clone(b));
            node.saved_out = Some(Arc::clone(&tracked_out));
        }
        tracked_out
    }
}

pub(crate) fn scatter_add_flat(
    src_vals: &[f64],
    out_numel: usize,
    dst_flat_indices: &[usize],
) -> Vec<f64> {
    let mut out = vec![0.0; out_numel];
    for (&dst_idx, &v) in dst_flat_indices.iter().zip(src_vals.iter()) {
        if dst_idx < out.len() {
            out[dst_idx] += v;
        }
    }
    out
}

pub(crate) fn sum_axis_output_shape(shape: &[usize], axis: usize, keepdim: bool) -> Vec<usize> {
    let mut out = Vec::with_capacity(shape.len());
    for (i, &d) in shape.iter().enumerate() {
        if i == axis {
            if keepdim {
                out.push(1);
            }
        } else {
            out.push(d);
        }
    }
    out
}

pub(crate) fn sum_axis_flat(
    vals: &[f64],
    orig_shape: &[usize],
    axis: usize,
    keepdim: bool,
) -> Vec<f64> {
    let out_shape = sum_axis_output_shape(orig_shape, axis, keepdim);
    let out_numel: usize = out_shape.iter().copied().product::<usize>().max(1);
    let mut out = vec![0.0; out_numel];
    let outer: usize = orig_shape[..axis].iter().copied().product::<usize>().max(1);
    let axis_dim = orig_shape[axis];
    let inner: usize = orig_shape[axis + 1..]
        .iter()
        .copied()
        .product::<usize>()
        .max(1);
    for o in 0..outer {
        for k in 0..axis_dim {
            let src_base = (o * axis_dim + k) * inner;
            let dst_base = o * inner;
            for i in 0..inner {
                out[dst_base + i] += vals[src_base + i];
            }
        }
    }
    out
}

pub(crate) fn expand_axis_flat(
    vals: &[f64],
    _cur_shape: &[usize],
    axis: usize,
    _keepdim: bool,
    orig_shape: &[usize],
) -> Vec<f64> {
    let orig_numel: usize = orig_shape.iter().copied().product::<usize>().max(1);
    let mut out = vec![0.0; orig_numel];
    let outer: usize = orig_shape[..axis].iter().copied().product::<usize>().max(1);
    let axis_dim = orig_shape[axis];
    let inner: usize = orig_shape[axis + 1..]
        .iter()
        .copied()
        .product::<usize>()
        .max(1);
    for o in 0..outer {
        let src_base = o * inner;
        for k in 0..axis_dim {
            let dst_base = (o * axis_dim + k) * inner;
            out[dst_base..dst_base + inner].copy_from_slice(&vals[src_base..src_base + inner]);
        }
    }
    out
}

fn accum_grad(grads: &mut [Option<Vec<f64>>], idx: usize, delta: &[f64]) {
    match &mut grads[idx] {
        Some(existing) => {
            for (e, &d) in existing.iter_mut().zip(delta.iter()) {
                *e += d;
            }
        }
        None => {
            grads[idx] = Some(delta.to_vec());
        }
    }
}

pub(crate) fn read_broadcast_flat(
    vals: &[f64],
    src_shape: &[usize],
    out_shape: &[usize],
    mut out_linear: usize,
) -> f64 {
    if src_shape == out_shape {
        return vals[out_linear];
    }
    if src_shape.is_empty() {
        return vals[0];
    }
    let rank_diff = out_shape.len() - src_shape.len();
    let mut src_idx = 0usize;
    let mut stride = 1usize;
    for i in (0..out_shape.len()).rev() {
        let dim = out_shape[i];
        let coord = out_linear % dim;
        out_linear /= dim;
        if i >= rank_diff {
            let s_axis = i - rank_diff;
            let s_dim = src_shape[s_axis];
            let s_coord = if s_dim == 1 { 0 } else { coord };
            src_idx += s_coord * stride;
            stride *= s_dim;
        }
    }
    vals[src_idx]
}

fn unbroadcast(grad_out: &[f64], out_shape: &[usize], target_shape: &[usize]) -> Vec<f64> {
    if out_shape == target_shape {
        return grad_out.to_vec();
    }
    if target_shape.is_empty() {
        return vec![grad_out.iter().sum()];
    }
    let target_numel: usize = target_shape.iter().copied().product::<usize>().max(1);
    let mut out = vec![0.0; target_numel];
    let rank_diff = out_shape.len() - target_shape.len();
    for (mut out_lin, &g) in grad_out.iter().enumerate() {
        let mut t_idx = 0usize;
        let mut stride = 1usize;
        for i in (0..out_shape.len()).rev() {
            let dim = out_shape[i];
            let coord = out_lin % dim;
            out_lin /= dim;
            if i >= rank_diff {
                let t_axis = i - rank_diff;
                let t_dim = target_shape[t_axis];
                let t_coord = if t_dim == 1 { 0 } else { coord };
                t_idx += t_coord * stride;
                stride *= t_dim;
            }
        }
        out[t_idx] += g;
    }
    out
}

pub(crate) fn transpose_flat(
    vals: &[f64],
    cur_shape: &[usize],
    dim0: usize,
    dim1: usize,
    orig_shape: &[usize],
) -> Vec<f64> {
    let total = vals.len();
    let mut out = vec![0.0; total];
    let rank = cur_shape.len();
    let mut orig_strides = vec![1usize; rank];
    for i in (0..rank.saturating_sub(1)).rev() {
        orig_strides[i] = orig_strides[i + 1] * orig_shape[i + 1];
    }
    let mut coords = vec![0usize; rank];
    for (mut cur_lin, &v) in vals.iter().enumerate() {
        for i in (0..rank).rev() {
            coords[i] = cur_lin % cur_shape[i];
            cur_lin /= cur_shape[i];
        }
        coords.swap(dim0, dim1);
        let orig_lin: usize = coords
            .iter()
            .zip(orig_strides.iter())
            .map(|(c, s)| c * s)
            .sum();
        out[orig_lin] = v;
    }
    out
}

pub(crate) fn matmul_f64(
    a_vals: &[f64],
    b_vals: &[f64],
    a_shape: &[usize],
    b_shape: &[usize],
) -> MoraResult<Vec<f64>> {
    match (a_shape, b_shape) {
        ([k1], [k2]) if k1 == k2 => {
            let dot: f64 = a_vals.iter().zip(b_vals.iter()).map(|(x, y)| x * y).sum();
            Ok(vec![dot])
        }
        ([m, k1], [k2]) if k1 == k2 => {
            let mut out = vec![0.0; *m];
            for i in 0..*m {
                let mut acc = 0.0;
                for p in 0..*k1 {
                    acc += a_vals[i * k1 + p] * b_vals[p];
                }
                out[i] = acc;
            }
            Ok(out)
        }
        ([k1], [k2, n]) if k1 == k2 => {
            let mut out = vec![0.0; *n];
            for j in 0..*n {
                let mut acc = 0.0;
                for p in 0..*k1 {
                    acc += a_vals[p] * b_vals[p * n + j];
                }
                out[j] = acc;
            }
            Ok(out)
        }
        ([m, k1], [k2, n]) if k1 == k2 => {
            let mut out = vec![0.0; m * n];
            for i in 0..*m {
                for p in 0..*k1 {
                    let av = a_vals[i * k1 + p];
                    for j in 0..*n {
                        out[i * n + j] += av * b_vals[p * n + j];
                    }
                }
            }
            Ok(out)
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
            let mut out = vec![0.0; batch_count * mat_o];
            let map_batch = |mut b_lin: usize, target_batch: &[usize]| -> usize {
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
            };
            for b_idx in 0..batch_count {
                let a_base = map_batch(b_idx, batch_a) * mat_a;
                let b_base = map_batch(b_idx, batch_b) * mat_b;
                let o_base = b_idx * mat_o;
                for i in 0..m {
                    for p in 0..k {
                        let av = a_vals[a_base + i * k + p];
                        for j in 0..n {
                            out[o_base + i * n + j] += av * b_vals[b_base + p * n + j];
                        }
                    }
                }
            }
            Ok(out)
        }
        _ => Err(MoraError::Runtime(format!(
            "jvp: unsupported matmul shapes {:?} @ {:?}",
            a_shape, b_shape
        ))),
    }
}

type VjpPair = (Option<Vec<f64>>, Option<Vec<f64>>);

fn matmul_vjp(
    g_out: &[f64],
    a_vals: &[f64],
    b_vals: &[f64],
    a_shape: &[usize],
    b_shape: &[usize],
    need_a: bool,
    need_b: bool,
) -> MoraResult<VjpPair> {
    match (a_shape, b_shape) {
        ([k1], [k2]) if k1 == k2 => {
            let g0 = g_out[0];
            let ga = if need_a {
                Some(b_vals.iter().map(|&bv| g0 * bv).collect())
            } else {
                None
            };
            let gb = if need_b {
                Some(a_vals.iter().map(|&av| g0 * av).collect())
            } else {
                None
            };
            Ok((ga, gb))
        }
        ([m, k], [k2]) if k == k2 => {
            let ga = if need_a {
                let mut v = vec![0.0; m * k];
                for i in 0..*m {
                    for p in 0..*k {
                        v[i * k + p] = g_out[i] * b_vals[p];
                    }
                }
                Some(v)
            } else {
                None
            };
            let gb = if need_b {
                let mut v = vec![0.0; *k];
                for i in 0..*m {
                    for p in 0..*k {
                        v[p] += g_out[i] * a_vals[i * k + p];
                    }
                }
                Some(v)
            } else {
                None
            };
            Ok((ga, gb))
        }
        ([k], [k2, n]) if k == k2 => {
            let ga = if need_a {
                let mut v = vec![0.0; *k];
                for p in 0..*k {
                    let mut acc = 0.0;
                    for j in 0..*n {
                        acc += g_out[j] * b_vals[p * n + j];
                    }
                    v[p] = acc;
                }
                Some(v)
            } else {
                None
            };
            let gb = if need_b {
                let mut v = vec![0.0; k * n];
                for p in 0..*k {
                    for j in 0..*n {
                        v[p * n + j] = a_vals[p] * g_out[j];
                    }
                }
                Some(v)
            } else {
                None
            };
            Ok((ga, gb))
        }
        ([m, k], [k2, n]) if k == k2 => {
            let ga = if need_a {
                let mut v = vec![0.0; m * k];
                for i in 0..*m {
                    for p in 0..*k {
                        let mut acc = 0.0;
                        for j in 0..*n {
                            acc += g_out[i * n + j] * b_vals[p * n + j];
                        }
                        v[i * k + p] = acc;
                    }
                }
                Some(v)
            } else {
                None
            };
            let gb = if need_b {
                let mut v = vec![0.0; k * n];
                for p in 0..*k {
                    for j in 0..*n {
                        let mut acc = 0.0;
                        for i in 0..*m {
                            acc += a_vals[i * k + p] * g_out[i * n + j];
                        }
                        v[p * n + j] = acc;
                    }
                }
                Some(v)
            } else {
                None
            };
            Ok((ga, gb))
        }
        (sa, sb) if sa.len() >= 2 && sb.len() >= 2 => {
            let m = sa[sa.len() - 2];
            let k = sa[sa.len() - 1];
            let n = sb[sb.len() - 1];
            let batch_a = &sa[..sa.len() - 2];
            let batch_b = &sb[..sb.len() - 2];
            let batch_out = broadcast_runtime_shapes(batch_a, batch_b)?;
            let batch_count: usize = batch_out.iter().copied().product::<usize>().max(1);
            let mat_a = m * k;
            let mat_b = k * n;
            let mat_o = m * n;

            let mut ga = if need_a {
                Some(vec![0.0; a_vals.len()])
            } else {
                None
            };
            let mut gb = if need_b {
                Some(vec![0.0; b_vals.len()])
            } else {
                None
            };

            let map_batch = |mut b_lin: usize, target_batch: &[usize]| -> usize {
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
            };

            for b_idx in 0..batch_count {
                let a_base = map_batch(b_idx, batch_a) * mat_a;
                let b_base = map_batch(b_idx, batch_b) * mat_b;
                let o_base = b_idx * mat_o;
                if let Some(ref mut ga_vec) = ga {
                    for i in 0..m {
                        for p in 0..k {
                            let mut acc = 0.0;
                            for j in 0..n {
                                acc += g_out[o_base + i * n + j] * b_vals[b_base + p * n + j];
                            }
                            ga_vec[a_base + i * k + p] += acc;
                        }
                    }
                }
                if let Some(ref mut gb_vec) = gb {
                    for p in 0..k {
                        for j in 0..n {
                            let mut acc = 0.0;
                            for i in 0..m {
                                acc += a_vals[a_base + i * k + p] * g_out[o_base + i * n + j];
                            }
                            gb_vec[b_base + p * n + j] += acc;
                        }
                    }
                }
            }
            Ok((ga, gb))
        }
        _ => Err(MoraError::Runtime(format!(
            "autograd: unsupported matmul shapes {:?} @ {:?}",
            a_shape, b_shape
        ))),
    }
}
