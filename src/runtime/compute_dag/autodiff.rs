//! P6-2: SciLean-Style Symbolic Graph-to-Graph Reverse-Mode Automatic Differentiation
//! (`TensorComputeDag::autodiff`) with Joint Forward+Backward Fusion.
//!
//! Architecture:
//! - Given any single-output [`TensorComputeDag`] (unfused or already fused) and an optional
//!   subset of differentiable input indices `wrt_indices` (defaulting to all inputs `0..N`),
//!   constructs a single multi-output [`TensorComputeDag`] whose outputs are
//!   `[forward_output, d_input_i0, d_input_i1, ...]`.
//! - Unlike naive tape replay or `unfuse_to_primitives()`, this pass:
//!   1. **Preserves `FusedFlashAttention`** directly, attaching an `O(Sq)` `FlashAttentionLse`
//!      node and emitting tiled `FlashAttentionGrad` (`DQ` / `DK` / `DV` / `DMask`) nodes that
//!      NEVER materialize the `O(Sq * Sk)` attention score/probability matrix.
//!   2. **Differentiates `FusedLinearAct` and `FusedRmsNormLinear` analytically** (avoiding
//!      pre-activation GEMM recomputation for `None` / `Relu` / `Tanh` / `Sigmoid` and sharing
//!      recomputed `RmsNorm` between `d_gamma` and `d_x`).
//!   3. **Prunes dead adjoints** via forward `depends_on_wrt` reachability so gradients for
//!      frozen parameters, targets, or masks outside `wrt_indices` are never emitted.
//!   4. **Folds structural AD identities** (`0 + g -> g`, `1 * g -> g`, scalar broadcast of
//!      structural ones) during IR construction without altering user IEEE-754 semantics.
//!   5. **Runs joint forward+backward optimization & 4-pass fusion** (`fuse()`), including Pass 0
//!      CSE of shared pure nodes (`Transpose`, `Reshape`, `RmsNorm`, etc.) and Pass 3 fusion of
//!      backward `StepPos`/`Sign`/activation chains into `FusedElementwise` (hitting W^X AVX2/SSE2
//!      `native_jit` and GPU VRAM residency).

use std::collections::HashSet;

use crate::error::{MoraError, MoraResult};
use crate::value::tensor::checked_numel;

use super::flash_bwd::FlashAttnGradTarget;
use super::{
    DagIrBuilder, DagNode, DagOp, FusedAct, FusedEwStep, TensorComputeDag, broadcast_shapes_dag,
};

/// Prepare a DAG for symbolic joint AD:
/// - Keeps `FusedFlashAttention`, `FusedLinearAct`, and `FusedRmsNormLinear` intact.
/// - Expands `FusedElementwise` into primitive elementwise ops so backward can differentiate each
///   step symbolically and let joint `fuse()` re-fuse forward+backward elementwise chains cleanly.
fn prepare_forward_for_autodiff(dag: &TensorComputeDag) -> MoraResult<(Vec<DagNode>, usize)> {
    if dag.outputs.len() != 1 {
        return Err(MoraError::Runtime(format!(
            "ComputeDag.autodiff requires a single-output DAG, got {} outputs",
            dag.outputs.len()
        )));
    }
    let has_fused_ew = dag
        .nodes
        .iter()
        .any(|n| matches!(n.op, DagOp::FusedElementwise { .. }));
    if !has_fused_ew {
        return Ok((dag.nodes.clone(), dag.outputs[0]));
    }

    let mut b = DagIrBuilder::new();
    let mut map = vec![0usize; dag.nodes.len()];
    for (idx, node) in dag.nodes.iter().enumerate() {
        let new_id = match &node.op {
            DagOp::FusedElementwise { inputs, steps } => {
                let mut slots: Vec<usize> = inputs.iter().map(|&i| map[i]).collect();
                for step in steps {
                    let step_id = match *step {
                        FusedEwStep::Add(s0, s1) => b.add(slots[s0], slots[s1])?,
                        FusedEwStep::Sub(s0, s1) => b.sub(slots[s0], slots[s1])?,
                        FusedEwStep::Mul(s0, s1) => b.mul(slots[s0], slots[s1])?,
                        FusedEwStep::Div(s0, s1) => b.div(slots[s0], slots[s1])?,
                        FusedEwStep::Pow(s0, s1) => b.pow(slots[s0], slots[s1])?,
                        FusedEwStep::AddScalar(s0, c) => {
                            b.unary(DagOp::AddScalar(slots[s0], c), slots[s0])
                        }
                        FusedEwStep::SubScalar(s0, c, r) => {
                            b.unary(DagOp::SubScalar(slots[s0], c, r), slots[s0])
                        }
                        FusedEwStep::MulScalar(s0, c) => {
                            b.unary(DagOp::MulScalar(slots[s0], c), slots[s0])
                        }
                        FusedEwStep::DivScalar(s0, c, r) => {
                            b.unary(DagOp::DivScalar(slots[s0], c, r), slots[s0])
                        }
                        FusedEwStep::PowScalar(s0, c, r) => {
                            b.unary(DagOp::PowScalar(slots[s0], c, r), slots[s0])
                        }
                        FusedEwStep::Neg(s0) => b.unary(DagOp::Neg(slots[s0]), slots[s0]),
                        FusedEwStep::Relu(s0) => b.unary(DagOp::Relu(slots[s0]), slots[s0]),
                        FusedEwStep::Gelu(s0) => b.unary(DagOp::Gelu(slots[s0]), slots[s0]),
                        FusedEwStep::Silu(s0) => b.unary(DagOp::Silu(slots[s0]), slots[s0]),
                        FusedEwStep::Tanh(s0) => b.unary(DagOp::Tanh(slots[s0]), slots[s0]),
                        FusedEwStep::Sigmoid(s0) => b.unary(DagOp::Sigmoid(slots[s0]), slots[s0]),
                        FusedEwStep::Exp(s0) => b.unary(DagOp::Exp(slots[s0]), slots[s0]),
                        FusedEwStep::Ln(s0) => b.unary(DagOp::Ln(slots[s0]), slots[s0]),
                        FusedEwStep::Sqrt(s0) => b.unary(DagOp::Sqrt(slots[s0]), slots[s0]),
                        FusedEwStep::Sin(s0) => b.unary(DagOp::Sin(slots[s0]), slots[s0]),
                        FusedEwStep::Cos(s0) => b.unary(DagOp::Cos(slots[s0]), slots[s0]),
                        FusedEwStep::Abs(s0) => b.unary(DagOp::Abs(slots[s0]), slots[s0]),
                        FusedEwStep::StepPos(s0) => b.unary(DagOp::StepPos(slots[s0]), slots[s0]),
                        FusedEwStep::Sign(s0) => b.unary(DagOp::Sign(slots[s0]), slots[s0]),
                    };
                    slots.push(step_id);
                }
                *slots.last().ok_or_else(|| {
                    MoraError::Runtime("ComputeDag.autodiff: empty FusedElementwise".to_string())
                })?
            }
            _ => {
                let mut cloned = node.clone();
                cloned.op.remap_inputs(&map);
                b.push(cloned.op, cloned.shape, cloned.dtype, cloned.device)
            }
        };
        map[idx] = new_id;
    }
    Ok((b.nodes, map[dag.outputs[0]]))
}

/// Emit `g_pre = g * act'(pre)` for `FusedLinearAct` / `FusedRmsNormLinear`.
/// Avoids recomputing `pre` for `None`, `Relu`, `Tanh`, and `Sigmoid`.
fn emit_fused_act_vjp(
    b: &mut DagIrBuilder,
    g: usize,
    y_out: usize,
    act: FusedAct,
    recompute_pre: impl FnOnce(&mut DagIrBuilder) -> MoraResult<usize>,
    is_one: impl Fn(usize) -> bool,
) -> MoraResult<usize> {
    let mul_opt = |b: &mut DagIrBuilder, lhs: usize, rhs: usize| -> MoraResult<usize> {
        if is_one(lhs) && b.nodes[lhs].shape == b.nodes[rhs].shape {
            Ok(rhs)
        } else if is_one(rhs) && b.nodes[lhs].shape == b.nodes[rhs].shape {
            Ok(lhs)
        } else {
            b.mul(lhs, rhs)
        }
    };
    match act {
        FusedAct::None => Ok(g),
        FusedAct::Relu => {
            // `relu(z) > 0 <=> z > 0`, so `StepPos(y_out)` is exact with zero pre-act recomputation!
            let mask = b.unary(DagOp::StepPos(y_out), y_out);
            mul_opt(b, g, mask)
        }
        FusedAct::Tanh => {
            let y2 = b.mul(y_out, y_out)?;
            let one_m_y2 = b.unary(DagOp::SubScalar(y2, 1.0, true), y2);
            mul_opt(b, g, one_m_y2)
        }
        FusedAct::Sigmoid => {
            let one_m_y = b.unary(DagOp::SubScalar(y_out, 1.0, true), y_out);
            let y_omy = b.mul(y_out, one_m_y)?;
            mul_opt(b, g, y_omy)
        }
        FusedAct::Gelu => {
            let pre = recompute_pre(b)?;
            let dgelu = b.gelu_derivative(pre)?;
            mul_opt(b, g, dgelu)
        }
        FusedAct::Silu => {
            let pre = recompute_pre(b)?;
            let dsilu = b.silu_derivative(pre, y_out)?;
            mul_opt(b, g, dsilu)
        }
    }
}

/// Emit `RmsNorm(a, eps)` VJP: `da = inv_rms * (g - y * mean(g * y))`.
fn emit_rms_norm_vjp(
    b: &mut DagIrBuilder,
    a: usize,
    y: usize,
    eps: f64,
    g: usize,
) -> MoraResult<usize> {
    let sh = b.shape(a);
    let rank = sh.len();
    let last_ax = rank.saturating_sub(1);
    let d = sh.last().copied().unwrap_or(1) as f64;
    let a2 = b.mul(a, a)?;
    let sum_a2 = if rank == 0 {
        a2
    } else {
        b.sum_axis(a2, last_ax, true)
    };
    let ms = b.unary(DagOp::MulScalar(sum_a2, 1.0 / d), sum_a2);
    let ms_eps = b.unary(DagOp::AddScalar(ms, eps), ms);
    let rms = b.unary(DagOp::Sqrt(ms_eps), ms_eps);
    let inv_rms = b.unary(DagOp::DivScalar(rms, 1.0, true), rms);
    let gy = b.mul(g, y)?;
    let sum_gy = if rank == 0 {
        gy
    } else {
        b.sum_axis(gy, last_ax, true)
    };
    let mean_gy = b.unary(DagOp::MulScalar(sum_gy, 1.0 / d), sum_gy);
    let proj = b.mul(y, mean_gy)?;
    let diff = b.sub(g, proj)?;
    b.mul(inv_rms, diff)
}

/// Emit `MatMul(a, b_id)` VJP into `da` and/or `db` guarded by `need_a`/`need_b`.
fn emit_matmul_vjp(
    b: &mut DagIrBuilder,
    a: usize,
    b_id: usize,
    g: usize,
    need_a: bool,
    need_b: bool,
) -> MoraResult<(Option<usize>, Option<usize>)> {
    let sh_a = b.shape(a);
    let sh_b = b.shape(b_id);
    let (ra, rb) = (sh_a.len(), sh_b.len());
    let mut out_da = None;
    let mut out_db = None;
    match (ra, rb) {
        (1, 1) => {
            if need_a {
                out_da = Some(b.mul(g, b_id)?);
            }
            if need_b {
                out_db = Some(b.mul(g, a)?);
            }
        }
        (2, 1) => {
            let g_col = b.reshape(g, vec![sh_a[0], 1]);
            if need_a {
                let b_row = b.reshape(b_id, vec![1, sh_b[0]]);
                out_da = Some(b.matmul(g_col, b_row)?);
            }
            if need_b {
                let at = b.transpose(a, 0, 1);
                out_db = Some(b.matmul(at, g)?);
            }
        }
        (1, 2) => {
            if need_a {
                let bt = b.transpose(b_id, 0, 1);
                out_da = Some(b.matmul(g, bt)?);
            }
            if need_b {
                let a_col = b.reshape(a, vec![sh_a[0], 1]);
                let g_row = b.reshape(g, vec![1, sh_b[1]]);
                out_db = Some(b.matmul(a_col, g_row)?);
            }
        }
        (ra, rb) if ra >= 2 && rb >= 2 => {
            if need_a {
                let bt = b.transpose(b_id, rb - 2, rb - 1);
                let da_raw = b.matmul(g, bt)?;
                out_da = Some(b.unbroadcast_to(da_raw, &sh_a));
            }
            if need_b {
                let at = b.transpose(a, ra - 2, ra - 1);
                let db_raw = b.matmul(at, g)?;
                out_db = Some(b.unbroadcast_to(db_raw, &sh_b));
            }
        }
        _ => {
            return Err(MoraError::Runtime(format!(
                "ComputeDag.autodiff: unsupported MatMul shapes {sh_a:?} @ {sh_b:?}"
            )));
        }
    }
    Ok((out_da, out_db))
}

impl TensorComputeDag {
    /// P6-2: Build a fused joint forward+backward [`TensorComputeDag`] outputting
    /// `[forward_output, d_input_i0, d_input_i1, ...]` for the specified `wrt_indices`
    /// (defaulting to all inputs `0..input_specs.len()` when `wrt_indices` is `None`).
    ///
    /// Preserves `FusedFlashAttention` via tiled `O(Sq)` `FlashAttentionLse` + `FlashAttentionGrad`,
    /// prunes dead adjoints outside `wrt_indices`, folds structural AD zero/one identities, and
    /// runs joint CSE + 4-pass operator fusion across both forward and backward subgraphs.
    pub fn autodiff(&self, wrt_indices: Option<&[usize]>) -> MoraResult<Self> {
        let joint = self.build_joint_autodiff_unfused(wrt_indices)?;
        Ok(joint.fuse())
    }

    pub(super) fn build_joint_autodiff_unfused(
        &self,
        wrt_indices: Option<&[usize]>,
    ) -> MoraResult<Self> {
        let n_inputs = self.input_specs.len();
        let wrt: Vec<usize> = match wrt_indices {
            Some(slice) => {
                for &idx in slice {
                    if idx >= n_inputs {
                        return Err(MoraError::Runtime(format!(
                            "ComputeDag.autodiff: wrt index {idx} out of bounds (graph has {n_inputs} input(s))"
                        )));
                    }
                }
                slice.to_vec()
            }
            None => (0..n_inputs).collect(),
        };
        let wrt_set: HashSet<usize> = wrt.iter().copied().collect();

        let (fwd_nodes, fwd_out) = prepare_forward_for_autodiff(self)?;
        let n_fwd = fwd_nodes.len();

        // Forward reachability: mark every node that transitively depends on at least one `wrt` input.
        let mut depends_on_wrt = vec![false; n_fwd];
        for (idx, node) in fwd_nodes.iter().enumerate() {
            depends_on_wrt[idx] = match &node.op {
                DagOp::Input(slot) => wrt_set.contains(slot),
                DagOp::ConstTensor(_) | DagOp::StepPos(_) | DagOp::Sign(_) => false,
                _ => node.op.inputs().into_iter().any(|p| depends_on_wrt[p]),
            };
        }

        let mut b = DagIrBuilder::from_nodes(fwd_nodes.clone());
        let mut adj: Vec<Option<usize>> = vec![None; n_fwd];
        let mut structural_ones: HashSet<usize> = HashSet::new();

        // Seed output cotangent `dy = ones_like(fwd_out)` only if `fwd_out` depends on any `wrt`.
        if depends_on_wrt[fwd_out] {
            let out_node = &fwd_nodes[fwd_out];
            let seed = b.const_filled(1.0, &out_node.shape, out_node.dtype, &out_node.device)?;
            structural_ones.insert(seed);
            adj[fwd_out] = Some(seed);
        }

        let acc = |adj: &mut [Option<usize>],
                   target: usize,
                   grad: usize,
                   b: &mut DagIrBuilder|
         -> MoraResult<()> {
            if !depends_on_wrt[target] {
                return Ok(());
            }
            adj[target] = match adj[target] {
                None => Some(grad),
                Some(prev) => Some(b.add(prev, grad)?),
            };
            Ok(())
        };

        let mul_opt = |b: &mut DagIrBuilder,
                       lhs: usize,
                       rhs: usize,
                       ones: &HashSet<usize>|
         -> MoraResult<usize> {
            if ones.contains(&lhs) && b.nodes[lhs].shape == b.nodes[rhs].shape {
                Ok(rhs)
            } else if ones.contains(&rhs) && b.nodes[lhs].shape == b.nodes[rhs].shape {
                Ok(lhs)
            } else {
                b.mul(lhs, rhs)
            }
        };

        for idx in (0..n_fwd).rev() {
            let Some(g) = adj[idx] else {
                continue;
            };
            if !depends_on_wrt[idx] {
                continue;
            }
            let node_op = fwd_nodes[idx].op.clone();
            match node_op {
                DagOp::Input(_)
                | DagOp::ConstTensor(_)
                | DagOp::StepPos(_)
                | DagOp::Sign(_)
                | DagOp::FlashAttentionLse { .. } => {}
                DagOp::Identity(a) => acc(&mut adj, a, g, &mut b)?,
                DagOp::Reshape(a, _) => {
                    if depends_on_wrt[a] {
                        let sh_a = b.shape(a);
                        let da = b.reshape(g, sh_a);
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::Transpose(a, d0, d1) => {
                    if depends_on_wrt[a] {
                        let da = b.transpose(g, d0, d1);
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::Gather(a, indices) => {
                    if depends_on_wrt[a] {
                        let sh_a = b.shape(a);
                        let out_n = checked_numel(&sh_a)?;
                        let dt = b.dtype(a);
                        let dev = b.device(a);
                        let sc = b.push(DagOp::ScatterAdd(g, out_n, indices), sh_a, dt, dev);
                        acc(&mut adj, a, sc, &mut b)?;
                    }
                }
                DagOp::ScatterAdd(a, _, indices) => {
                    if depends_on_wrt[a] {
                        let sh_a = b.shape(a);
                        let dt = b.dtype(a);
                        let dev = b.device(a);
                        let ga = b.push(DagOp::Gather(g, indices), sh_a, dt, dev);
                        acc(&mut adj, a, ga, &mut b)?;
                    }
                }
                DagOp::Slice {
                    src,
                    starts,
                    ends: _,
                } => {
                    if depends_on_wrt[src] {
                        let src_sh = b.shape(src);
                        let out_sh = b.shape(idx);
                        let pads: Vec<(usize, usize)> = starts
                            .iter()
                            .enumerate()
                            .map(|(d, &s)| (s, src_sh[d] - (s + out_sh[d])))
                            .collect();
                        let dt = b.dtype(src);
                        let dev = b.device(src);
                        let da = b.push(DagOp::Pad { src: g, pads }, src_sh, dt, dev);
                        acc(&mut adj, src, da, &mut b)?;
                    }
                }
                DagOp::Pad { src, pads } => {
                    if depends_on_wrt[src] {
                        let src_sh = b.shape(src);
                        let starts: Vec<usize> = pads.iter().map(|&(lo, _)| lo).collect();
                        let ends: Vec<usize> = pads
                            .iter()
                            .enumerate()
                            .map(|(d, &(lo, _))| lo + src_sh[d])
                            .collect();
                        let dt = b.dtype(src);
                        let dev = b.device(src);
                        let da = b.push(
                            DagOp::Slice {
                                src: g,
                                starts,
                                ends,
                            },
                            src_sh,
                            dt,
                            dev,
                        );
                        acc(&mut adj, src, da, &mut b)?;
                    }
                }
                DagOp::Cat { inputs, axis } => {
                    let out_sh = b.shape(idx);
                    let rank = out_sh.len();
                    let mut offset = 0usize;
                    for inp in inputs {
                        let in_sh = b.shape(inp);
                        let dim = in_sh[axis];
                        if depends_on_wrt[inp] {
                            let mut starts = vec![0usize; rank];
                            let mut ends = out_sh.clone();
                            starts[axis] = offset;
                            ends[axis] = offset + dim;
                            let dt = b.dtype(inp);
                            let dev = b.device(inp);
                            let da = b.push(
                                DagOp::Slice {
                                    src: g,
                                    starts,
                                    ends,
                                },
                                in_sh,
                                dt,
                                dev,
                            );
                            acc(&mut adj, inp, da, &mut b)?;
                        }
                        offset += dim;
                    }
                }
                DagOp::Add(a, b_id) => {
                    if depends_on_wrt[a] {
                        let sh_a = b.shape(a);
                        let da = b.unbroadcast_to(g, &sh_a);
                        acc(&mut adj, a, da, &mut b)?;
                    }
                    if depends_on_wrt[b_id] {
                        let sh_b = b.shape(b_id);
                        let db = b.unbroadcast_to(g, &sh_b);
                        acc(&mut adj, b_id, db, &mut b)?;
                    }
                }
                DagOp::Sub(a, b_id) => {
                    if depends_on_wrt[a] {
                        let sh_a = b.shape(a);
                        let da = b.unbroadcast_to(g, &sh_a);
                        acc(&mut adj, a, da, &mut b)?;
                    }
                    if depends_on_wrt[b_id] {
                        let sh_b = b.shape(b_id);
                        let ng = b.unary(DagOp::Neg(g), g);
                        let db = b.unbroadcast_to(ng, &sh_b);
                        acc(&mut adj, b_id, db, &mut b)?;
                    }
                }
                DagOp::Mul(a, b_id) => {
                    if depends_on_wrt[a] {
                        let sh_a = b.shape(a);
                        let da_raw = mul_opt(&mut b, g, b_id, &structural_ones)?;
                        let da = b.unbroadcast_to(da_raw, &sh_a);
                        acc(&mut adj, a, da, &mut b)?;
                    }
                    if depends_on_wrt[b_id] {
                        let sh_b = b.shape(b_id);
                        let db_raw = mul_opt(&mut b, g, a, &structural_ones)?;
                        let db = b.unbroadcast_to(db_raw, &sh_b);
                        acc(&mut adj, b_id, db, &mut b)?;
                    }
                }
                DagOp::Div(a, b_id) => {
                    if depends_on_wrt[a] {
                        let sh_a = b.shape(a);
                        let da_raw = b.div(g, b_id)?;
                        let da = b.unbroadcast_to(da_raw, &sh_a);
                        acc(&mut adj, a, da, &mut b)?;
                    }
                    if depends_on_wrt[b_id] {
                        let sh_b = b.shape(b_id);
                        let ga = mul_opt(&mut b, g, a, &structural_ones)?;
                        let neg_ga = b.unary(DagOp::Neg(ga), ga);
                        let b2 = b.mul(b_id, b_id)?;
                        let db_raw = b.div(neg_ga, b2)?;
                        let db = b.unbroadcast_to(db_raw, &sh_b);
                        acc(&mut adj, b_id, db, &mut b)?;
                    }
                }
                DagOp::Pow(a, b_id) => {
                    if depends_on_wrt[a] {
                        let sh_a = b.shape(a);
                        let bm1 = b.unary(DagOp::SubScalar(b_id, 1.0, false), b_id);
                        let a_bm1 = b.pow(a, bm1)?;
                        let coeff = b.mul(b_id, a_bm1)?;
                        let da_raw = mul_opt(&mut b, g, coeff, &structural_ones)?;
                        let da = b.unbroadcast_to(da_raw, &sh_a);
                        acc(&mut adj, a, da, &mut b)?;
                    }
                    if depends_on_wrt[b_id] {
                        let sh_b = b.shape(b_id);
                        let ln_a = b.unary(DagOp::Ln(a), a);
                        let y_lna = b.mul(idx, ln_a)?;
                        let db_raw = mul_opt(&mut b, g, y_lna, &structural_ones)?;
                        let db = b.unbroadcast_to(db_raw, &sh_b);
                        acc(&mut adj, b_id, db, &mut b)?;
                    }
                }
                DagOp::AddScalar(a, _) => acc(&mut adj, a, g, &mut b)?,
                DagOp::SubScalar(a, _, rev) => {
                    if depends_on_wrt[a] {
                        let da = if rev { b.unary(DagOp::Neg(g), g) } else { g };
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::MulScalar(a, c) => {
                    if depends_on_wrt[a] {
                        let da = if c == 1.0 && structural_ones.contains(&g) {
                            g
                        } else {
                            b.unary(DagOp::MulScalar(g, c), g)
                        };
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::DivScalar(a, c, rev) => {
                    if depends_on_wrt[a] {
                        let da = if rev {
                            let a2 = b.mul(a, a)?;
                            let neg_gc = b.unary(DagOp::MulScalar(g, -c), g);
                            b.div(neg_gc, a2)?
                        } else {
                            b.unary(DagOp::DivScalar(g, c, false), g)
                        };
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::PowScalar(a, c, rev) => {
                    if depends_on_wrt[a] {
                        let da = if rev {
                            let scale = c.ln();
                            let gy = mul_opt(&mut b, g, idx, &structural_ones)?;
                            b.unary(DagOp::MulScalar(gy, scale), gy)
                        } else if c == 2.0 {
                            let two_a = b.unary(DagOp::MulScalar(a, 2.0), a);
                            mul_opt(&mut b, g, two_a, &structural_ones)?
                        } else if c == 1.0 {
                            g
                        } else {
                            let a_cm1 = b.unary(DagOp::PowScalar(a, c - 1.0, false), a);
                            let coeff = b.unary(DagOp::MulScalar(a_cm1, c), a_cm1);
                            mul_opt(&mut b, g, coeff, &structural_ones)?
                        };
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::Neg(a) => {
                    if depends_on_wrt[a] {
                        let da = b.unary(DagOp::Neg(g), g);
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::Relu(a) => {
                    if depends_on_wrt[a] {
                        let mask = b.unary(DagOp::StepPos(a), a);
                        let da = mul_opt(&mut b, g, mask, &structural_ones)?;
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::Abs(a) => {
                    if depends_on_wrt[a] {
                        let sgn = b.unary(DagOp::Sign(a), a);
                        let da = mul_opt(&mut b, g, sgn, &structural_ones)?;
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::Gelu(a) => {
                    if depends_on_wrt[a] {
                        let dgelu = b.gelu_derivative(a)?;
                        let da = mul_opt(&mut b, g, dgelu, &structural_ones)?;
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::Silu(a) => {
                    if depends_on_wrt[a] {
                        let dsilu = b.silu_derivative(a, idx)?;
                        let da = mul_opt(&mut b, g, dsilu, &structural_ones)?;
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::Tanh(a) => {
                    if depends_on_wrt[a] {
                        let y2 = b.mul(idx, idx)?;
                        let one_m_y2 = b.unary(DagOp::SubScalar(y2, 1.0, true), y2);
                        let da = mul_opt(&mut b, g, one_m_y2, &structural_ones)?;
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::Sigmoid(a) => {
                    if depends_on_wrt[a] {
                        let one_m_y = b.unary(DagOp::SubScalar(idx, 1.0, true), idx);
                        let y_omy = b.mul(idx, one_m_y)?;
                        let da = mul_opt(&mut b, g, y_omy, &structural_ones)?;
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::Exp(a) => {
                    if depends_on_wrt[a] {
                        let da = mul_opt(&mut b, g, idx, &structural_ones)?;
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::Ln(a) => {
                    if depends_on_wrt[a] {
                        let da = b.div(g, a)?;
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::Sqrt(a) => {
                    if depends_on_wrt[a] {
                        let two_y = b.unary(DagOp::MulScalar(idx, 2.0), idx);
                        let da = b.div(g, two_y)?;
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::Sin(a) => {
                    if depends_on_wrt[a] {
                        let ca = b.unary(DagOp::Cos(a), a);
                        let da = mul_opt(&mut b, g, ca, &structural_ones)?;
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::Cos(a) => {
                    if depends_on_wrt[a] {
                        let sa = b.unary(DagOp::Sin(a), a);
                        let nsa = b.unary(DagOp::Neg(sa), sa);
                        let da = mul_opt(&mut b, g, nsa, &structural_ones)?;
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::Sum(a) => {
                    if depends_on_wrt[a] {
                        let sh_a = b.shape(a);
                        let da = if structural_ones.contains(&g) {
                            let ones = b.const_filled(1.0, &sh_a, b.dtype(a), &b.device(a))?;
                            structural_ones.insert(ones);
                            ones
                        } else {
                            b.broadcast_to_shape(g, &sh_a)?
                        };
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::Mean(a) => {
                    if depends_on_wrt[a] {
                        let sh_a = b.shape(a);
                        let n = checked_numel(&sh_a)?.max(1) as f64;
                        let da = if structural_ones.contains(&g) {
                            b.const_filled(1.0 / n, &sh_a, b.dtype(a), &b.device(a))?
                        } else {
                            let scaled = b.unary(DagOp::DivScalar(g, n, false), g);
                            b.broadcast_to_shape(scaled, &sh_a)?
                        };
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::SumAxis(a, axis, keepdim) => {
                    if depends_on_wrt[a] {
                        let sh_a = b.shape(a);
                        let g_kd = if keepdim {
                            g
                        } else {
                            let mut kd_sh = b.shape(g);
                            kd_sh.insert(axis, 1);
                            b.reshape(g, kd_sh)
                        };
                        let da = b.broadcast_to_shape(g_kd, &sh_a)?;
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::MatMul(a, b_id) => {
                    let (da_opt, db_opt) = emit_matmul_vjp(
                        &mut b,
                        a,
                        b_id,
                        g,
                        depends_on_wrt[a],
                        depends_on_wrt[b_id],
                    )?;
                    if let Some(da) = da_opt {
                        acc(&mut adj, a, da, &mut b)?;
                    }
                    if let Some(db) = db_opt {
                        acc(&mut adj, b_id, db, &mut b)?;
                    }
                }
                DagOp::Softmax(a) => {
                    if depends_on_wrt[a] {
                        let sh = b.shape(a);
                        let rank = sh.len();
                        let gy = mul_opt(&mut b, g, idx, &structural_ones)?;
                        let dot = if rank == 0 {
                            gy
                        } else {
                            b.sum_axis(gy, rank - 1, true)
                        };
                        let diff = b.sub(g, dot)?;
                        let da = b.mul(idx, diff)?;
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::LogSoftmax(a) => {
                    if depends_on_wrt[a] {
                        let sh = b.shape(a);
                        let rank = sh.len();
                        let p = b.unary(DagOp::Exp(idx), idx);
                        let sum_g = if rank == 0 {
                            g
                        } else {
                            b.sum_axis(g, rank - 1, true)
                        };
                        let corr = b.mul(p, sum_g)?;
                        let da = b.sub(g, corr)?;
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::RmsNorm(a, eps) => {
                    if depends_on_wrt[a] {
                        let da = emit_rms_norm_vjp(&mut b, a, idx, eps, g)?;
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::LayerNorm(a, eps) => {
                    if depends_on_wrt[a] {
                        let sh = b.shape(a);
                        let rank = sh.len();
                        let last_ax = rank.saturating_sub(1);
                        let d = sh.last().copied().unwrap_or(1) as f64;
                        let sum_a = if rank == 0 {
                            a
                        } else {
                            b.sum_axis(a, last_ax, true)
                        };
                        let mean = b.unary(DagOp::MulScalar(sum_a, 1.0 / d), sum_a);
                        let centered = b.sub(a, mean)?;
                        let c2 = b.mul(centered, centered)?;
                        let sum_c2 = if rank == 0 {
                            c2
                        } else {
                            b.sum_axis(c2, last_ax, true)
                        };
                        let var = b.unary(DagOp::MulScalar(sum_c2, 1.0 / d), sum_c2);
                        let var_eps = b.unary(DagOp::AddScalar(var, eps), var);
                        let std = b.unary(DagOp::Sqrt(var_eps), var_eps);
                        let inv_std = b.unary(DagOp::DivScalar(std, 1.0, true), std);
                        let sum_g = if rank == 0 {
                            g
                        } else {
                            b.sum_axis(g, last_ax, true)
                        };
                        let mean_g = b.unary(DagOp::MulScalar(sum_g, 1.0 / d), sum_g);
                        let gy = mul_opt(&mut b, g, idx, &structural_ones)?;
                        let sum_gy = if rank == 0 {
                            gy
                        } else {
                            b.sum_axis(gy, last_ax, true)
                        };
                        let mean_gy = b.unary(DagOp::MulScalar(sum_gy, 1.0 / d), sum_gy);
                        let g_centered = b.sub(g, mean_g)?;
                        let y_term = b.mul(idx, mean_gy)?;
                        let diff = b.sub(g_centered, y_term)?;
                        let da = b.mul(inv_std, diff)?;
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::FusedLinearAct { x, w, bias, act } => {
                    let g_pre = emit_fused_act_vjp(
                        &mut b,
                        g,
                        idx,
                        act,
                        |b_ref| {
                            let mm = b_ref.matmul(x, w)?;
                            match bias {
                                Some(bi) => b_ref.add(mm, bi),
                                None => Ok(mm),
                            }
                        },
                        |id| structural_ones.contains(&id),
                    )?;
                    if let Some(bi) = bias
                        && depends_on_wrt[bi]
                    {
                        let sh_b = b.shape(bi);
                        let db = b.unbroadcast_to(g_pre, &sh_b);
                        acc(&mut adj, bi, db, &mut b)?;
                    }
                    let (dx_opt, dw_opt) =
                        emit_matmul_vjp(&mut b, x, w, g_pre, depends_on_wrt[x], depends_on_wrt[w])?;
                    if let Some(dx) = dx_opt {
                        acc(&mut adj, x, dx, &mut b)?;
                    }
                    if let Some(dw) = dw_opt {
                        acc(&mut adj, w, dw, &mut b)?;
                    }
                }
                DagOp::FusedRmsNormLinear {
                    x,
                    eps,
                    gamma,
                    w,
                    bias,
                    act,
                } => {
                    let need_x = depends_on_wrt[x];
                    let need_gamma = gamma.is_some_and(|gi| depends_on_wrt[gi]);
                    let need_w = depends_on_wrt[w];
                    let need_bias = bias.is_some_and(|bi| depends_on_wrt[bi]);
                    let need_norm = need_x
                        || need_gamma
                        || need_w
                        || matches!(act, FusedAct::Gelu | FusedAct::Silu);
                    let (norm_x, norm_scaled) = if need_norm {
                        let rn = b.unary(DagOp::RmsNorm(x, eps), x);
                        let sc = match gamma {
                            Some(gi) => b.mul(rn, gi)?,
                            None => rn,
                        };
                        (Some(rn), Some(sc))
                    } else {
                        (None, None)
                    };
                    let g_pre = emit_fused_act_vjp(
                        &mut b,
                        g,
                        idx,
                        act,
                        |b_ref| {
                            let ns = norm_scaled.expect("norm_scaled materialized for Gelu/Silu");
                            let mm = b_ref.matmul(ns, w)?;
                            match bias {
                                Some(bi) => b_ref.add(mm, bi),
                                None => Ok(mm),
                            }
                        },
                        |id| structural_ones.contains(&id),
                    )?;
                    if let Some(bi) = bias
                        && need_bias
                    {
                        let sh_b = b.shape(bi);
                        let db = b.unbroadcast_to(g_pre, &sh_b);
                        acc(&mut adj, bi, db, &mut b)?;
                    }
                    if let Some(ns) = norm_scaled {
                        let (dns_opt, dw_opt) =
                            emit_matmul_vjp(&mut b, ns, w, g_pre, need_x || need_gamma, need_w)?;
                        if let Some(dw) = dw_opt {
                            acc(&mut adj, w, dw, &mut b)?;
                        }
                        if let Some(d_ns) = dns_opt {
                            let rn = norm_x.expect("norm_x materialized");
                            if let Some(gi) = gamma
                                && need_gamma
                            {
                                let sh_g = b.shape(gi);
                                let dgam_raw = b.mul(d_ns, rn)?;
                                let dgam = b.unbroadcast_to(dgam_raw, &sh_g);
                                acc(&mut adj, gi, dgam, &mut b)?;
                            }
                            if need_x {
                                let d_rn = match gamma {
                                    Some(gi) => b.mul(d_ns, gi)?,
                                    None => d_ns,
                                };
                                let dx = emit_rms_norm_vjp(&mut b, x, rn, eps, d_rn)?;
                                acc(&mut adj, x, dx, &mut b)?;
                            }
                        }
                    }
                }
                DagOp::FusedFlashAttention {
                    q,
                    k,
                    v,
                    mask,
                    scale,
                    k_is_transposed,
                } => {
                    let need_q = depends_on_wrt[q];
                    let need_k = depends_on_wrt[k];
                    let need_v = depends_on_wrt[v];
                    let need_mask = mask.is_some_and(|mi| depends_on_wrt[mi]);
                    if need_q || need_k || need_v || need_mask {
                        // O(Sq) log-sum-exp node (paired with `FusedFlashAttention` forward pass!).
                        let out_sh = b.shape(idx);
                        let mut lse_sh = out_sh[..out_sh.len() - 1].to_vec();
                        if lse_sh.is_empty() {
                            lse_sh.push(1);
                        }
                        let dt = b.dtype(idx);
                        let dev = b.device(idx);
                        let lse_id = b.push(
                            DagOp::FlashAttentionLse {
                                q,
                                k,
                                v,
                                mask,
                                scale,
                                k_is_transposed,
                            },
                            lse_sh,
                            dt,
                            dev.clone(),
                        );

                        if need_q {
                            let dq = b.push(
                                DagOp::FlashAttentionGrad {
                                    q,
                                    k,
                                    v,
                                    mask,
                                    out: idx,
                                    lse: lse_id,
                                    dout: g,
                                    scale,
                                    k_is_transposed,
                                    target: FlashAttnGradTarget::DQ,
                                },
                                b.shape(q),
                                b.dtype(q),
                                b.device(q),
                            );
                            acc(&mut adj, q, dq, &mut b)?;
                        }
                        if need_k {
                            let dk = b.push(
                                DagOp::FlashAttentionGrad {
                                    q,
                                    k,
                                    v,
                                    mask,
                                    out: idx,
                                    lse: lse_id,
                                    dout: g,
                                    scale,
                                    k_is_transposed,
                                    target: FlashAttnGradTarget::DK,
                                },
                                b.shape(k),
                                b.dtype(k),
                                b.device(k),
                            );
                            acc(&mut adj, k, dk, &mut b)?;
                        }
                        if need_v {
                            let dv = b.push(
                                DagOp::FlashAttentionGrad {
                                    q,
                                    k,
                                    v,
                                    mask,
                                    out: idx,
                                    lse: lse_id,
                                    dout: g,
                                    scale,
                                    k_is_transposed,
                                    target: FlashAttnGradTarget::DV,
                                },
                                b.shape(v),
                                b.dtype(v),
                                b.device(v),
                            );
                            acc(&mut adj, v, dv, &mut b)?;
                        }
                        if let Some(mi) = mask
                            && need_mask
                        {
                            let dm = b.push(
                                DagOp::FlashAttentionGrad {
                                    q,
                                    k,
                                    v,
                                    mask,
                                    out: idx,
                                    lse: lse_id,
                                    dout: g,
                                    scale,
                                    k_is_transposed,
                                    target: FlashAttnGradTarget::DMask,
                                },
                                b.shape(mi),
                                b.dtype(mi),
                                b.device(mi),
                            );
                            acc(&mut adj, mi, dm, &mut b)?;
                        }
                    }
                }
                DagOp::FlashAttentionGrad { .. } => {
                    return Err(MoraError::Runtime(
                        "ComputeDag.autodiff: higher-order autodiff through FlashAttentionGrad requires unfusing first".to_string(),
                    ));
                }
                DagOp::FusedElementwise { .. } => {
                    unreachable!("prepare_forward_for_autodiff expands FusedElementwise")
                }
            }
        }

        // Collect outputs: `[fwd_out, d_input_wrt[0], d_input_wrt[1], ...]`
        let mut raw_outputs = Vec::with_capacity(1 + wrt.len());
        raw_outputs.push(fwd_out);
        for &in_slot in &wrt {
            let mut grad_out: Option<usize> = None;
            for (idx, node) in fwd_nodes.iter().enumerate() {
                if matches!(node.op, DagOp::Input(s) if s == in_slot)
                    && let Some(g) = adj[idx]
                {
                    grad_out = match grad_out {
                        None => Some(g),
                        Some(prev) => Some(b.add(prev, g)?),
                    };
                }
            }
            let out_id = match grad_out {
                Some(id) => id,
                None => {
                    let (ref sh, dt, ref dev) = self.input_specs[in_slot];
                    b.const_filled(0.0, sh, dt, dev)?
                }
            };
            raw_outputs.push(out_id);
        }

        let (nodes, outputs) = super::compact_live_nodes(b.nodes, &raw_outputs);
        let _ = broadcast_shapes_dag;
        Ok(Self {
            input_specs: self.input_specs.clone(),
            nodes,
            outputs,
            fused: false,
            algebraic_rewrites: 0,
            rematerialized_nodes: 0,
            compiled_plan: None,
        })
    }
}
