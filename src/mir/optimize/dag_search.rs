//! v0.60: Staged DAG optimization with dirty-tracking and convergence.
//!
//! ## v0.60: Staged + Dirty Tracking
//!
//! Replaces the v0.59 greedy "pick best, apply one, repeat" loop with:
//! 1. **DagOptimizer** — tracks dirty nodes and per-node execution count.
//! 2. **Staged application** — rules grouped into stages (algebraic → fold → CSE → dead).
//! 3. **Convergence detection** — outer loop runs until no stage produces a change.
//! 4. **Dirty propagation** — when a node is rewritten, all its consumers are marked dirty.

use std::collections::HashSet;

use crate::mir::dag::{MirDag, MirDagNode, NodeId};
use crate::mir::optimize::cost::CostModel;
use crate::mir::optimize::dag_rule::{DagRewrite, DagRewriteRule};

// ─── DagOptimizer ─────────────────────────────────────────────────────

/// Tracks optimization state across a DAG.
struct DagOptimizer {
    /// Which nodes still need to be checked.
    dirty: Vec<bool>,
    /// How many times each node has been rewritten (safety: prevents infinite loops).
    exec_count: Vec<usize>,
    /// Maximum rewrites per node before giving up.
    max_exec: usize,
}

impl DagOptimizer {
    fn new(node_count: usize) -> Self {
        // All nodes start dirty (unchecked).
        let mut dirty = Vec::with_capacity(node_count);
        dirty.resize(node_count, true);
        DagOptimizer {
            dirty,
            exec_count: vec![0; node_count],
            max_exec: 5,
        }
    }

    /// Mark `node_id` and all transitive consumers as dirty.
    /// Consumers are nodes that have an incoming edge FROM `node_id`.
    fn mark_dirty(&mut self, node_id: NodeId, dag: &MirDag) {
        if node_id >= self.dirty.len() {
            return; // new nodes added by rewrite: already dirty-at-creation
        }
        if self.dirty[node_id] {
            return; // already dirty, skip to avoid infinite recursion
        }
        self.dirty[node_id] = true;
        // Propagate to all consumers (nodes that have an edge FROM this node).
        for edge in &dag.edges {
            if edge.from == node_id {
                self.mark_dirty(edge.to, dag);
            }
        }
    }

    /// Check if a node is eligible for optimization.
    fn can_optimize(&self, node_id: NodeId, node: &MirDagNode) -> bool {
        !node.is_removed() && self.dirty[node_id] && self.exec_count[node_id] < self.max_exec
    }
}

// ─── Staged Search ────────────────────────────────────────────────────

/// Run DAG rewrite rules in stages, with dirty-tracking and convergence.
///
/// Each stage is a group of rules applied together. Stages are processed
/// in order. Within a stage, dirty nodes are scanned in topological order
/// (by `node_id`). When a rewrite fires, the affected node and all its
/// consumers are marked dirty for the next pass.
///
/// The outer loop converges when a full pass over all stages produces zero changes.
pub fn dag_search_staged(
    dag: &mut MirDag,
    stages: &[Vec<Box<dyn DagRewriteRule>>],
    cost: &dyn CostModel,
) {
    let mut opt = DagOptimizer::new(dag.nodes.len());

    loop {
        let mut any_change = false;

        for stage in stages {
            // Reset dirty for this stage: all unremoved nodes should be
            // checked by this stage's rules (different stages = different rules).
            for (i, node) in dag.nodes.iter().enumerate() {
                opt.dirty[i] = !node.is_removed();
            }

            for node_id in 0..dag.nodes.len() {
                if !opt.can_optimize(node_id, &dag.nodes[node_id]) {
                    continue;
                }
                opt.dirty[node_id] = false; // we're checking it now

                // v0.75.5: Cascades 择优 — 收集本节点所有可应用重写（rule.rewrite
                // 只读返回 owned DagRewrite，不改 dag），选 cost delta 最大的应用。
                // 此前同 stage 内是"第一个 delta>0 就 break"，可能选中次优重写。
                let mut best: Option<(i32, DagRewrite)> = None;
                for rule in stage {
                    let node = &dag.nodes[node_id]; // re-borrow after dirty=false
                    if !rule.matches(node_id, node, dag) {
                        continue;
                    }
                    if let Some(rw) = rule.rewrite(node_id, dag) {
                        // Compute cost delta
                        let old_cost = rw
                            .removed
                            .iter()
                            .map(|&id| node_cost(&dag.nodes[id], cost))
                            .sum::<u32>();
                        let new_cost = rw.added.iter().map(|n| dag_node_cost(n, cost)).sum::<u32>();
                        let delta = old_cost as i32 - new_cost as i32;
                        if delta > 0 && best.as_ref().is_none_or(|(bd, _)| delta > *bd) {
                            best = Some((delta, rw));
                        }
                    }
                }
                if let Some((_delta, rw)) = best {
                    // Extend dirty/exec_count for any new nodes
                    let new_count = dag.nodes.len() + rw.added.len();
                    opt.dirty.resize(new_count, true);
                    opt.exec_count.resize(new_count, 0);

                    apply_rewrite(dag, rw);
                    opt.exec_count[node_id] += 1;
                    // Re-mark the rewritten node and its consumers
                    opt.mark_dirty(node_id, dag);
                    any_change = true;
                }
            }
        }

        if !any_change {
            break; // converged
        }
    }
}

// ─── Legacy: Single-phase greedy search ───────────────────────────────

/// Run DAG rewrite rules until convergence, using cost deltas
/// from the provided cost model. (Legacy: single flat rule set.)
pub fn dag_search(
    dag: &mut MirDag,
    rules: &[Box<dyn DagRewriteRule>],
    cost: &dyn CostModel,
    max_iter: u32,
) {
    for _iter in 0..max_iter {
        let mut best: Option<(NodeId, DagRewrite, i32)> = None;

        for node_id in 0..dag.nodes.len() {
            if dag.nodes[node_id].is_removed() {
                continue;
            }
            let node = &dag.nodes[node_id];
            for rule in rules {
                if rule.matches(node_id, node, dag)
                    && let Some(rw) = rule.rewrite(node_id, dag)
                {
                    // Compute actual cost delta
                    let old_cost = rw
                        .removed
                        .iter()
                        .map(|&id| node_cost(&dag.nodes[id], cost))
                        .sum::<u32>();
                    let new_cost = rw.added.iter().map(|n| dag_node_cost(n, cost)).sum::<u32>();
                    let delta = old_cost as i32 - new_cost as i32;
                    if delta > best.as_ref().map(|b| b.2).unwrap_or(0) {
                        best = Some((node_id, rw, delta));
                    }
                }
            }
        }

        match best {
            Some((_node_id, rw, _gain)) => apply_rewrite(dag, rw),
            None => break,
        }
    }
}

/// Apply a `DagRewrite` to the DAG in-place.
fn apply_rewrite(dag: &mut MirDag, rw: DagRewrite) {
    let old_len = dag.nodes.len();
    let new_base = old_len; // new nodes start at this index

    // 1. Add new nodes
    for node in rw.added {
        dag.nodes.push(node);
    }

    // 2. Add new edges (remap `usize::MAX` placeholders to `new_base`).
    //    v0.75.6: placeholder 由 0 改为 usize::MAX — 节点 0 是合法 id，
    //    旧实现会在含变量操作数的图上触发 index out of bounds。
    for (from, to, kind) in rw.added_edges {
        let actual_from = if from == usize::MAX { new_base } else { from };
        dag.edges.push(crate::mir::dag::MirDagEdge {
            from: actual_from,
            to,
            kind,
        });
    }

    // 3. Mark removed nodes
    for &rm_id in &rw.removed {
        dag.nodes[rm_id] = MirDagNode::Removed;
    }

    // 4. Remove edges to/from removed nodes
    let removed_set: HashSet<NodeId> = rw.removed.iter().copied().collect();
    dag.edges
        .retain(|e| !removed_set.contains(&e.from) && !removed_set.contains(&e.to));

    // 5. Recompute entry/exit
    let mut has_incoming: HashSet<NodeId> = HashSet::new();
    let mut has_outgoing: HashSet<NodeId> = HashSet::new();
    for edge in &dag.edges {
        has_incoming.insert(edge.to);
        has_outgoing.insert(edge.from);
    }
    dag.entry = (0..dag.nodes.len())
        .filter(|n| !dag.nodes[*n].is_removed() && !has_incoming.contains(n))
        .collect();
    dag.exit = (0..dag.nodes.len())
        .filter(|n| !dag.nodes[*n].is_removed() && !has_outgoing.contains(n))
        .collect();
}

/// Cost of an existing DAG node, using the cost model.
fn node_cost(node: &MirDagNode, cost: &dyn CostModel) -> u32 {
    match node {
        MirDagNode::Compute { inst, .. } => cost.inst_cost(inst),
        MirDagNode::Effect { inst } => cost.inst_cost(inst),
        _ => 0,
    }
}

/// Cost of a new DAG node (not yet in the graph).
fn dag_node_cost(node: &MirDagNode, cost: &dyn CostModel) -> u32 {
    match node {
        MirDagNode::Compute { inst, .. } => cost.inst_cost(inst),
        _ => 0,
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::BinaryOp;
    use crate::mir::dag;
    use crate::mir::optimize::cost::{InstructionCount, TokenEstimate};
    use crate::mir::optimize::dag_rule::{ConstFoldingDagRule, DeadNodeDagRule};
    use crate::mir::{MirFunction, MirInst};
    use crate::value::Value;

    fn make_dag(body: Vec<MirInst>) -> MirDag {
        let n = body
            .iter()
            .filter_map(|i| i.dst())
            .max()
            .map(|r| r + 1)
            .unwrap_or(1);
        let func = MirFunction {
            params: vec![],
            body,
            n_regs: n,
        };
        dag::dag_analyze(&func)
    }

    #[test]
    fn dag_search_folds_constants() {
        let mut dag = make_dag(vec![
            MirInst::Const(0, Value::Int(10)),
            MirInst::Const(1, Value::Int(32)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
        ]);
        let rules: Vec<Box<dyn DagRewriteRule>> =
            vec![Box::new(ConstFoldingDagRule), Box::new(DeadNodeDagRule)];
        let before = dag.nodes.iter().filter(|n| !n.is_removed()).count();
        let cost = InstructionCount;
        dag_search(&mut dag, &rules, &cost, 10);
        let after = dag.nodes.iter().filter(|n| !n.is_removed()).count();
        assert!(
            after < before,
            "nodes should decrease after folding: {} -> {}",
            before,
            after
        );
    }

    #[test]
    fn dag_search_no_op_on_empty() {
        let mut dag = make_dag(vec![MirInst::Const(0, Value::Int(42))]);
        let rules: Vec<Box<dyn DagRewriteRule>> = vec![Box::new(ConstFoldingDagRule)];
        let before = dag.nodes.iter().filter(|n| !n.is_removed()).count();
        let cost = InstructionCount;
        dag_search(&mut dag, &rules, &cost, 10);
        let after = dag.nodes.iter().filter(|n| !n.is_removed()).count();
        assert_eq!(before, after, "single Const should not be modified");
    }

    // ─── Staged search tests ───────────────────────────────────────

    use crate::mir::optimize::dag_rule::{
        AlgebraicSimplifyDagRule, ConstFoldingDagRule as CfRule, CseDagRule,
        DeadNodeDagRule as DnRule,
    };

    #[test]
    fn staged_folds_constants() {
        let mut dag = make_dag(vec![
            MirInst::Const(0, Value::Int(10)),
            MirInst::Const(1, Value::Int(32)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
        ]);
        let stages: Vec<Vec<Box<dyn DagRewriteRule>>> = vec![
            vec![Box::new(AlgebraicSimplifyDagRule)],
            vec![Box::new(CfRule)],
            vec![Box::new(CseDagRule)],
            vec![Box::new(DnRule)],
        ];
        let before = dag.nodes.iter().filter(|n| !n.is_removed()).count();
        let cost = TokenEstimate;
        dag_search_staged(&mut dag, &stages, &cost);
        let after = dag.nodes.iter().filter(|n| !n.is_removed()).count();
        assert!(
            after <= before,
            "staged should not increase node count: {} -> {}",
            before,
            after
        );
    }

    #[test]
    fn staged_cascading_simplify_then_fold() {
        // r0=5, r1=0, r2=r0+r1  →  algebraic: r2→r0  →  no further fold needed
        // r3=2, r4=3, r5=r3+r4  →  const fold: r5=5
        let mut dag = make_dag(vec![
            MirInst::Const(0, Value::Int(5)),
            MirInst::Const(1, Value::Int(0)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1), // x+0 → x
            MirInst::Const(3, Value::Int(2)),
            MirInst::Const(4, Value::Int(3)),
            MirInst::BinaryOp(5, 3, BinaryOp::Add, 4), // 2+3 → 5
        ]);
        let stages: Vec<Vec<Box<dyn DagRewriteRule>>> = vec![
            vec![Box::new(AlgebraicSimplifyDagRule)],
            vec![Box::new(CfRule)],
            vec![Box::new(CseDagRule)],
            vec![Box::new(DnRule)],
        ];
        let cost = TokenEstimate;
        dag_search_staged(&mut dag, &stages, &cost);
        // After algebraic: r2 is removed (redirected to r0)
        // After const fold: r5 becomes Const(5)
        let active: Vec<_> = dag.nodes.iter().filter(|n| !n.is_removed()).collect();
        // We should have fewer nodes than the original 6
        assert!(
            active.len() < 6,
            "nodes should decrease with cascading: {}",
            active.len()
        );
    }

    #[test]
    fn staged_removes_cse_after_fold() {
        // r0=2, r1=3, r2=r0+r1, r3=r0+r1  — same inputs, r3 is CSE of r2
        // Const folding fires first (2+3→5 for both), then CSE eliminates duplicate Const(5)
        let mut dag = make_dag(vec![
            MirInst::Const(0, Value::Int(2)),
            MirInst::Const(1, Value::Int(3)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
            MirInst::BinaryOp(3, 0, BinaryOp::Add, 1), // same inputs as r2
        ]);
        let stages: Vec<Vec<Box<dyn DagRewriteRule>>> = vec![
            vec![Box::new(AlgebraicSimplifyDagRule)],
            vec![Box::new(CfRule)],
            vec![Box::new(CseDagRule)],
            vec![Box::new(DnRule)],
        ];
        let cost = TokenEstimate;
        dag_search_staged(&mut dag, &stages, &cost);
        let binops: Vec<_> = dag
            .nodes
            .iter()
            .filter(|n| {
                matches!(
                    n,
                    MirDagNode::Compute {
                        inst: MirInst::BinaryOp(..),
                        ..
                    }
                )
            })
            .filter(|n| !n.is_removed())
            .collect();
        assert_eq!(
            binops.len(),
            0,
            "all BinaryOps should be folded, got {}",
            binops.len()
        );
    }

    #[test]
    fn staged_converges_on_no_op() {
        let mut dag = make_dag(vec![MirInst::Const(0, Value::Int(42))]);
        let stages: Vec<Vec<Box<dyn DagRewriteRule>>> = vec![
            vec![Box::new(AlgebraicSimplifyDagRule)],
            vec![Box::new(CfRule)],
            vec![Box::new(CseDagRule)],
            vec![Box::new(DnRule)],
        ];
        let before = dag.nodes.iter().filter(|n| !n.is_removed()).count();
        let cost = TokenEstimate;
        dag_search_staged(&mut dag, &stages, &cost);
        let after = dag.nodes.iter().filter(|n| !n.is_removed()).count();
        assert_eq!(before, after, "single Const should converge with no change");
    }

    #[test]
    fn staged_dead_node_cleanup_after_cse() {
        // r0=10, r1=20, r2=r0+r1, r3=r0+r1  (r3 is CSE of r2)
        // After CSE removes r3, the edge count should decrease
        let mut dag = make_dag(vec![
            MirInst::Const(0, Value::Int(10)),
            MirInst::Const(1, Value::Int(20)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1), // entry
            MirInst::BinaryOp(3, 0, BinaryOp::Add, 1), // duplicate → CSE removes
        ]);
        let before = dag.nodes.iter().filter(|n| !n.is_removed()).count();
        let stages: Vec<Vec<Box<dyn DagRewriteRule>>> = vec![
            vec![Box::new(AlgebraicSimplifyDagRule)],
            vec![Box::new(CfRule)],
            vec![Box::new(CseDagRule)],
            vec![Box::new(DnRule)],
        ];
        let cost = TokenEstimate;
        dag_search_staged(&mut dag, &stages, &cost);
        let after = dag.nodes.iter().filter(|n| !n.is_removed()).count();
        // CSE should remove the duplicate BinaryOp
        assert!(
            after < before,
            "CSE should remove duplicate: {} -> {}",
            before,
            after
        );
    }

    // ─── v0.75.5: Cascades 同 stage 择优 ──────────────────────────

    /// 测试用低收益规则：匹配 BinaryOp，只移除自身（InstructionCount delta=1）。
    /// 用于证明同 stage 内选 max delta 而非"先匹配先应用"。
    struct TestSmallGainRule;

    impl DagRewriteRule for TestSmallGainRule {
        fn name(&self) -> &'static str {
            "test_small_gain"
        }

        fn matches(&self, _node_id: NodeId, node: &MirDagNode, _dag: &MirDag) -> bool {
            matches!(
                node,
                MirDagNode::Compute {
                    inst: MirInst::BinaryOp(..),
                    ..
                }
            )
        }

        fn rewrite(&self, node_id: NodeId, _dag: &MirDag) -> Option<DagRewrite> {
            Some(DagRewrite {
                added: vec![],
                removed: vec![node_id],
                added_edges: vec![],
            })
        }

        fn cost_gain(&self) -> i32 {
            1
        }
    }

    #[test]
    fn staged_picks_highest_gain_in_stage() {
        // r0=2, r1=3, r2=r0+r1（r2 是 exit，无 out edge，小 gain 规则移除无副作用）。
        // 同 stage 内两个可应用规则：
        //   TestSmallGainRule（数组在前）：移除 r2 → delta=1，剩 r0,r1（后由 dead 清理）
        //   ConstFoldingDagRule：折叠为 Const(5)，移除 r0,r1,r2 → delta=2
        // Cascades 择优应选中 ConstFolding（最大 delta）。
        let mut dag = make_dag(vec![
            MirInst::Const(0, Value::Int(2)),
            MirInst::Const(1, Value::Int(3)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
        ]);
        let stages: Vec<Vec<Box<dyn DagRewriteRule>>> = vec![vec![
            Box::new(TestSmallGainRule),
            Box::new(ConstFoldingDagRule),
            Box::new(DeadNodeDagRule),
        ]];
        let cost = InstructionCount;
        dag_search_staged(&mut dag, &stages, &cost);
        let active: Vec<_> = dag.nodes.iter().filter(|n| !n.is_removed()).collect();
        // ConstFold 折叠 r2 → Const(5)（1 节点）；小 gain 路径会先删 r2 剩 2 节点。
        assert_eq!(
            active.len(),
            1,
            "应择优选中 ConstFolding（delta=2）而非 TestSmallGain（delta=1），剩 {} 节点",
            active.len()
        );
        assert!(
            matches!(
                active[0],
                MirDagNode::Compute {
                    inst: MirInst::Const(2, Value::Int(5)),
                    ..
                }
            ),
            "剩余节点应为折叠后的 Const(5), got {:?}",
            active[0]
        );
    }
}
