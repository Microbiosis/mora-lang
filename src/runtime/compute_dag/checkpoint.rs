//! P6-2: Cost-Guided Activation Checkpointing (Rematerialization) on Joint Forward+Backward
//! [`TensorComputeDag`]s (`TensorComputeDag::autodiff_checkpointed`).
//!
//! Algorithm:
//! 1. Builds the fused joint forward+backward graph `[loss, grads...]` via [`TensorComputeDag::autodiff`].
//! 2. Evaluates its compile-time [`DagMemoryPlan`]; if `planned_peak_bytes <= vram_budget_bytes`,
//!    returns immediately with zero rematerialization overhead.
//! 3. Otherwise, identifies forward intermediates crossing the forward -> backward boundary
//!    (ancestors of `outputs[0]` consumed by backward nodes) whose liveness intervals overlap the
//!    graph's peak memory cut.
//! 4. Ranks candidates by `recompute_flops / (bytes * live_span)` using [`DagCostModel`] so cheap
//!    memory-bound activations (`FusedElementwise`, `RmsNorm`, `LayerNorm`, `Softmax`, `LogSoftmax`,
//!    `Transpose`, `Reshape`) rematerialize before expensive compute-bound ops (`MatMul`,
//!    `FusedLinearAct`, `FusedRmsNormLinear`).
//! 5. Clones the candidate's pure rematerialization closure right before its first backward consumer,
//!    marks the cloned nodes `no_cse = true` so subsequent joint `fuse()` fuses them into backward
//!    without hoisting them back across the forward boundary, and accepts the transform whenever
//!    `DagMemoryPlan::build(&candidate).planned_peak_bytes` strictly decreases.

use std::collections::{HashMap, HashSet};

use crate::error::MoraResult;
use crate::value::tensor::checked_numel;

use super::{DagCostModel, DagMemoryPlan, DagNode, DagOp, TensorComputeDag};

/// Compute `is_fwd[i] == true` for every node transitively reachable from `dag.outputs[0]`.
fn compute_forward_mask(dag: &TensorComputeDag) -> Vec<bool> {
    let n = dag.nodes.len();
    let mut is_fwd = vec![false; n];
    if let Some(&fwd_out) = dag.outputs.first()
        && fwd_out < n
    {
        let mut stack = vec![fwd_out];
        while let Some(id) = stack.pop() {
            if is_fwd[id] {
                continue;
            }
            is_fwd[id] = true;
            for pred in dag.nodes[id].op.inputs() {
                stack.push(pred);
            }
        }
    }
    is_fwd
}

/// Compute per-node `(last_use, last_fwd_use, first_bwd_use)` for boundary analysis.
fn compute_use_bounds(
    dag: &TensorComputeDag,
    is_fwd: &[bool],
) -> (Vec<usize>, Vec<usize>, Vec<Option<usize>>) {
    let n = dag.nodes.len();
    let mut last_use = vec![0usize; n];
    let mut last_fwd_use = vec![0usize; n];
    let mut first_bwd_use: Vec<Option<usize>> = vec![None; n];
    for i in 0..n {
        last_use[i] = i;
        last_fwd_use[i] = i;
    }
    for (idx, node) in dag.nodes.iter().enumerate() {
        for pred in node.op.inputs() {
            if idx > last_use[pred] {
                last_use[pred] = idx;
            }
            if is_fwd[idx] {
                if idx > last_fwd_use[pred] {
                    last_fwd_use[pred] = idx;
                }
            } else {
                first_bwd_use[pred] = Some(match first_bwd_use[pred] {
                    None => idx,
                    Some(prev) => prev.min(idx),
                });
            }
        }
    }
    (last_use, last_fwd_use, first_bwd_use)
}

/// Check whether `op` is safe and eligible to rematerialize during checkpointing.
fn is_rematerializable(op: &DagOp) -> bool {
    !matches!(
        op,
        DagOp::Input(_)
            | DagOp::ConstTensor(_)
            | DagOp::FlashAttentionLse { .. }
            | DagOp::FlashAttentionGrad { .. }
            | DagOp::FusedFlashAttention { .. }
    )
}

/// Build a small pure rematerialization closure for forward node `target` when recomputed at
/// backward insertion point `insert_pos`. Stops at `Input`, `ConstTensor`, nodes already alive
/// at `insert_pos` (so we don't clone live checkpoints), or when the closure exceeds `max_closure`.
fn build_remat_closure(
    dag: &TensorComputeDag,
    target: usize,
    insert_pos: usize,
    last_use: &[usize],
    is_output: &[bool],
    max_closure: usize,
) -> Option<Vec<usize>> {
    let mut closure: Vec<usize> = Vec::new();
    let mut seen: HashSet<usize> = HashSet::new();
    let mut stack = vec![target];
    while let Some(cur) = stack.pop() {
        if !seen.insert(cur) {
            continue;
        }
        if !is_rematerializable(&dag.nodes[cur].op) {
            return None;
        }
        closure.push(cur);
        if closure.len() > max_closure {
            return None;
        }
        for pred in dag.nodes[cur].op.inputs() {
            if matches!(dag.nodes[pred].op, DagOp::Input(_) | DagOp::ConstTensor(_)) {
                continue;
            }
            // If `pred` is already kept alive past `insert_pos` (by another consumer or as an output),
            // reading `pred` directly costs zero extra memory — no need to clone `pred`!
            if is_output[pred] || last_use[pred] >= insert_pos {
                continue;
            }
            stack.push(pred);
        }
    }
    closure.sort_unstable();
    Some(closure)
}

/// Clone `closure` (sorted topological node IDs ending at `target`) right before `insert_pos`,
/// marking cloned nodes `no_cse = true` and redirecting all backward consumers (`idx >= insert_pos`
/// and `!is_fwd[idx]`) of `target` to read the rematerialized clone.
fn apply_rematerialization(
    dag: &TensorComputeDag,
    target: usize,
    closure: &[usize],
    insert_pos: usize,
    is_fwd: &[bool],
) -> TensorComputeDag {
    let old_n = dag.nodes.len();
    let k = closure.len();
    let mut new_nodes: Vec<DagNode> = Vec::with_capacity(old_n + k);
    let mut old_to_new: Vec<usize> = vec![0usize; old_n];

    // 1. Copy prefix `0..insert_pos` unchanged.
    for (old_idx, node) in dag.nodes[..insert_pos].iter().enumerate() {
        old_to_new[old_idx] = new_nodes.len();
        new_nodes.push(node.clone());
    }

    // 2. Insert rematerialized clones of `closure` right before `insert_pos`.
    let mut clone_map: HashMap<usize, usize> = HashMap::with_capacity(k);
    for &orig_id in closure {
        let mut cloned = dag.nodes[orig_id].clone();
        cloned.no_cse = true;
        // Remap clone's inputs: prefer earlier clones inside `closure`, else original prefix IDs.
        let mut remap_vec = old_to_new.clone();
        for (&orig_k, &clone_v) in &clone_map {
            remap_vec[orig_k] = clone_v;
        }
        cloned.op.remap_inputs(&remap_vec);
        let new_clone_id = new_nodes.len();
        clone_map.insert(orig_id, new_clone_id);
        new_nodes.push(cloned);
    }
    let cloned_target_id = clone_map[&target];

    // 3. Copy suffix `insert_pos..old_n`, shifting node indices by `+k` and redirecting backward
    //    consumers of `target` to `cloned_target_id`.
    for (old_idx, slot) in old_to_new.iter_mut().enumerate().skip(insert_pos) {
        *slot = old_idx + k;
    }
    let mut bwd_remap = old_to_new.clone();
    bwd_remap[target] = cloned_target_id;

    for (rel_idx, node) in dag.nodes[insert_pos..].iter().enumerate() {
        let old_idx = insert_pos + rel_idx;
        let mut shifted = node.clone();
        if !is_fwd[old_idx] {
            shifted.op.remap_inputs(&bwd_remap);
        } else {
            shifted.op.remap_inputs(&old_to_new);
        }
        new_nodes.push(shifted);
    }

    let new_outputs: Vec<usize> = dag.outputs.iter().map(|&o| old_to_new[o]).collect();

    TensorComputeDag {
        input_specs: dag.input_specs.clone(),
        nodes: new_nodes,
        outputs: new_outputs,
        fused: false,
        algebraic_rewrites: dag.algebraic_rewrites,
        rematerialized_nodes: dag.rematerialized_nodes + k,
        compiled_plan: None,
    }
}

impl TensorComputeDag {
    /// P6-2: Cost-guided activation checkpointing for joint forward+backward [`TensorComputeDag`].
    ///
    /// Builds `self.autodiff(wrt_indices)` and iteratively rematerializes cheap memory-bound
    /// forward intermediates crossing the forward -> backward boundary until
    /// `memory_plan().planned_peak_bytes <= vram_budget_bytes` or no further peak-memory
    /// reduction is achievable.
    pub fn autodiff_checkpointed(
        &self,
        wrt_indices: Option<&[usize]>,
        vram_budget_bytes: usize,
    ) -> MoraResult<Self> {
        let joint = self.autodiff(wrt_indices)?;
        Ok(joint.checkpoint_joint(vram_budget_bytes))
    }

    /// Apply cost-guided activation checkpointing directly to an already-constructed joint
    /// multi-output `[loss, grads...]` DAG.
    pub(crate) fn checkpoint_joint(&self, vram_budget_bytes: usize) -> Self {
        let mut current = if self.fused {
            self.clone()
        } else {
            self.fuse()
        };
        let mut current_plan = DagMemoryPlan::build(&current);
        if current_plan.planned_peak_bytes <= vram_budget_bytes {
            return current;
        }
        let mut best_overall = current.clone();
        let mut best_peak = current_plan.planned_peak_bytes;

        // Iteratively select the lowest recompute-FLOPs-per-(byte*span) boundary activation
        // that strictly reduces `DagMemoryPlan::planned_peak_bytes`.
        let max_iters = current.nodes.len().clamp(8, 64);
        for _ in 0..max_iters {
            if current_plan.planned_peak_bytes <= vram_budget_bytes {
                break;
            }
            let n = current.nodes.len();
            let is_fwd = compute_forward_mask(&current);
            let mut is_output = vec![false; n];
            for &o in &current.outputs {
                if o < n {
                    is_output[o] = true;
                }
            }
            let (last_use, last_fwd_use, first_bwd_use) = compute_use_bounds(&current, &is_fwd);
            let cost_report = DagCostModel::analyze(&current);

            // Identify candidate saved forward activations crossing the forward -> backward boundary.
            // Score = recompute_flops / (bytes * saved_span), lowest first!
            let mut candidates: Vec<(f64, usize, usize, Vec<usize>)> = Vec::new();
            for u in 0..n {
                if !is_fwd[u]
                    || is_output[u]
                    || current.nodes[u].no_cse
                    || !is_rematerializable(&current.nodes[u].op)
                {
                    continue;
                }
                let Some(insert_pos) = first_bwd_use[u] else {
                    continue;
                };
                if insert_pos <= last_fwd_use[u] {
                    continue;
                }
                let Some(closure) =
                    build_remat_closure(&current, u, insert_pos, &last_use, &is_output, 16)
                else {
                    continue;
                };
                let recompute_flops: u64 = closure
                    .iter()
                    .map(|&cid| cost_report.node_costs[cid].flops.max(1))
                    .sum();
                let u_elems = checked_numel(&current.nodes[u].shape).unwrap_or(1);
                let u_bytes = current.nodes[u].dtype.storage_bytes(u_elems).max(1);
                let saved_span = insert_pos.saturating_sub(last_fwd_use[u]).max(1);
                let score = (recompute_flops as f64) / ((u_bytes as f64) * (saved_span as f64));
                candidates.push((score, u, insert_pos, closure));
            }

            if candidates.is_empty() {
                break;
            }
            candidates.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

            // Try cheapest candidates first; accept the first that strictly lowers planned peak bytes
            // (or if none alone lowers peak because of simultaneous peaks, pick the top cheap candidate
            // that lowers live-set pressure and check whether a 2-step move lowers peak).
            let mut improved = false;
            let mut best_fallback: Option<(TensorComputeDag, DagMemoryPlan, usize)> = None;
            for &(_, u, insert_pos, ref closure) in candidates.iter().take(12) {
                let mut cand = apply_rematerialization(&current, u, closure, insert_pos, &is_fwd);
                let remat_count = cand.rematerialized_nodes;
                cand = cand.fuse();
                cand.rematerialized_nodes = remat_count;
                let cand_plan = DagMemoryPlan::build(&cand);
                if cand_plan.planned_peak_bytes < current_plan.planned_peak_bytes {
                    current = cand;
                    current_plan = cand_plan;
                    if current_plan.planned_peak_bytes < best_peak {
                        best_peak = current_plan.planned_peak_bytes;
                        best_overall = current.clone();
                    }
                    improved = true;
                    break;
                }
                if cand_plan.planned_peak_bytes == current_plan.planned_peak_bytes
                    && best_fallback.is_none()
                {
                    let new_is_fwd = compute_forward_mask(&cand);
                    let (_, _, new_first_bwd) = compute_use_bounds(&cand, &new_is_fwd);
                    let crossing_bytes: usize = (0..cand.nodes.len())
                        .filter(|&i| new_is_fwd[i] && new_first_bwd[i].is_some())
                        .map(|i| {
                            let e = checked_numel(&cand.nodes[i].shape).unwrap_or(1);
                            cand.nodes[i].dtype.storage_bytes(e)
                        })
                        .sum();
                    let cur_crossing_bytes: usize = (0..n)
                        .filter(|&i| is_fwd[i] && first_bwd_use[i].is_some())
                        .map(|i| {
                            let e = checked_numel(&current.nodes[i].shape).unwrap_or(1);
                            current.nodes[i].dtype.storage_bytes(e)
                        })
                        .sum();
                    if crossing_bytes < cur_crossing_bytes {
                        best_fallback = Some((cand, cand_plan, crossing_bytes));
                    }
                }
            }

            if !improved {
                if let Some((cand, cand_plan, _)) = best_fallback {
                    current = cand;
                    current_plan = cand_plan;
                } else {
                    break;
                }
            }
        }

        best_overall
    }
}
