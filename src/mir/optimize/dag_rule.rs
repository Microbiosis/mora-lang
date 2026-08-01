//! v0.59: DAG-level rewrite rules — Cascades on MirDag.
//!
//! Unlike linear MirInst rewrite rules that scan backward through
//! the instruction stream, DAG rules navigate the explicit `MirDagEdge`
//! graph. A rule matches a subgraph of `MirDagNode`s and produces
//! a `DagRewrite` that describes which nodes to add, remove, and
//! which edges to redirect.

use crate::mir::dag::{EdgeKind, MirDag, MirDagEdge, MirDagNode, NodeId};
use crate::mir::{MirInst, Reg};
use crate::value::Value;

/// Describes a single rewrite operation on a MirDag.
#[derive(Debug, Clone)]
pub struct DagRewrite {
    /// New nodes to insert.
    pub added: Vec<MirDagNode>,
    /// Node ids to mark as removed.
    pub removed: Vec<NodeId>,
    /// New edges to add. `from` and `to` references are stable: they
    /// reference either existing node ids or a new node's position
    /// within `added` (shifted by `dag.nodes.len()`).
    pub added_edges: Vec<(NodeId, NodeId, EdgeKind)>,
}

impl DagRewrite {
    pub fn empty() -> Self {
        DagRewrite {
            added: vec![],
            removed: vec![],
            added_edges: vec![],
        }
    }
}

/// A DAG-level rewrite rule.
pub trait DagRewriteRule {
    fn name(&self) -> &'static str;

    /// Does this rule apply to the given node?
    fn matches(&self, node_id: NodeId, node: &MirDagNode, dag: &MirDag) -> bool;

    /// Produce a rewrite for the given node, or None if not applicable.
    fn rewrite(&self, node_id: NodeId, dag: &MirDag) -> Option<DagRewrite>;

    fn cost_gain(&self) -> i32 {
        1
    }
}

// ─── Helpers ────────────────────────────────────────────────────────

/// Find the value of a Const node, if it is one.
fn const_value(node: &MirDagNode) -> Option<&Value> {
    match node {
        MirDagNode::Compute {
            inst: MirInst::Const(_, v),
            ..
        } => Some(v),
        _ => None,
    }
}

/// Find incoming Data edges to `node_id` for a specific register.
fn find_data_source(dag: &MirDag, node_id: NodeId, reg: Reg) -> Option<NodeId> {
    dag.edges.iter().find_map(|e| {
        if e.to == node_id
            && let EdgeKind::Data { reg: r } = e.kind
            && r == reg
        {
            Some(e.from)
        } else {
            None
        }
    })
}

/// Find all outgoing Data edges from `node_id`.
fn outgoing_data_edges(dag: &MirDag, node_id: NodeId) -> Vec<&MirDagEdge> {
    dag.edges
        .iter()
        .filter(|e| e.from == node_id && matches!(e.kind, EdgeKind::Data { .. }))
        .collect()
}

// ─── Rule 1: Constant Folding on DAG ────────────────────────────────

/// Folds `BinaryOp(dst, lhs, op, rhs)` where both `lhs` and `rhs`
/// have Data edges pointing to `Const` nodes.
pub struct ConstFoldingDagRule;

impl DagRewriteRule for ConstFoldingDagRule {
    fn name(&self) -> &'static str {
        "dag_const_folding"
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

    fn rewrite(&self, node_id: NodeId, dag: &MirDag) -> Option<DagRewrite> {
        let node = dag.nodes.get(node_id)?;
        let (dst, lhs_reg, op, rhs_reg) = match node {
            MirDagNode::Compute {
                inst: MirInst::BinaryOp(d, l, o, r),
                ..
            } => (d, l, o, r),
            _ => return None,
        };

        // Find Const source nodes via Data edges
        let lhs_src = find_data_source(dag, node_id, *lhs_reg)?;
        let rhs_src = find_data_source(dag, node_id, *rhs_reg)?;

        let lhs_v = const_value(&dag.nodes[lhs_src])?.clone();
        let rhs_v = const_value(&dag.nodes[rhs_src])?.clone();

        let result = crate::flow::eval_binary(lhs_v, op, rhs_v).ok()?;

        let new_node = MirDagNode::Compute {
            inst: MirInst::Const(*dst, result),
            dst: *dst,
            input_regs: vec![],
        };

        let out_edges: Vec<(NodeId, NodeId, EdgeKind)> = dag
            .edges
            .iter()
            .filter(|e| e.from == node_id)
            // v0.75.6: placeholder 用 usize::MAX（此前用 0，与「节点 0 是合法 id」
            // 冲突 — 含变量操作数的真实代码会触发 index out of bounds）。
            .map(|e| (usize::MAX, e.to, e.kind.clone()))
            .collect();

        let mut removed = vec![node_id];
        if outgoing_data_edges(dag, lhs_src).len() <= 1 {
            removed.push(lhs_src);
        }
        if outgoing_data_edges(dag, rhs_src).len() <= 1 {
            removed.push(rhs_src);
        }

        Some(DagRewrite {
            added: vec![new_node],
            removed,
            added_edges: out_edges,
        })
    }

    fn cost_gain(&self) -> i32 {
        2
    }
}

// ─── Rule 2: Dead Node Removal on DAG ───────────────────────────────

/// Removes Compute nodes that have no outgoing edges (dead code).
/// After constant folding, the original Const sources may become
/// unreferenced and this rule cleans them up.
pub struct DeadNodeDagRule;

impl DagRewriteRule for DeadNodeDagRule {
    fn name(&self) -> &'static str {
        "dag_dead_node"
    }

    fn matches(&self, _node_id: NodeId, node: &MirDagNode, _dag: &MirDag) -> bool {
        matches!(node, MirDagNode::Compute { .. })
    }

    fn rewrite(&self, node_id: NodeId, dag: &MirDag) -> Option<DagRewrite> {
        // Don't remove exit nodes (they carry the function's result)
        if dag.exit.contains(&node_id) {
            return None;
        }
        let has_outgoing = dag.edges.iter().any(|e| e.from == node_id);
        if has_outgoing {
            return None;
        }

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

// ─── Rule 3: Common Subexpression Elimination on DAG ────────────────

/// Eliminates duplicate Compute nodes that have the same operation
/// and the same data sources.
pub struct CseDagRule;

impl DagRewriteRule for CseDagRule {
    fn name(&self) -> &'static str {
        "dag_cse"
    }

    fn matches(&self, _node_id: NodeId, node: &MirDagNode, _dag: &MirDag) -> bool {
        // Any pure Compute node is a candidate
        matches!(node, MirDagNode::Compute { .. })
    }

    fn rewrite(&self, node_id: NodeId, dag: &MirDag) -> Option<DagRewrite> {
        let node = dag.nodes.get(node_id)?;
        let _node_inst = match node {
            MirDagNode::Compute { inst, .. } => inst,
            _ => return None,
        };

        // Scan prior nodes for an equivalent one
        for prev_id in 0..node_id {
            if prev_id == node_id {
                break;
            }
            if dag.nodes[prev_id].is_removed() {
                continue;
            }

            if nodes_equivalent(&dag.nodes[prev_id], node, prev_id, node_id, dag) {
                // Found equivalent — redirect outgoing edges from node_id to prev_id
                let out_edges: Vec<(NodeId, NodeId, EdgeKind)> = dag
                    .edges
                    .iter()
                    .filter(|e| e.from == node_id)
                    .map(|e| (prev_id, e.to, e.kind.clone()))
                    .collect();

                return Some(DagRewrite {
                    added: vec![],
                    removed: vec![node_id],
                    added_edges: out_edges,
                });
            }
        }
        None
    }

    fn cost_gain(&self) -> i32 {
        2
    }
}

/// Check if two Compute nodes are structurally equivalent:
/// same instruction type + same data sources.
fn nodes_equivalent(
    a: &MirDagNode,
    b: &MirDagNode,
    a_id: NodeId,
    b_id: NodeId,
    dag: &MirDag,
) -> bool {
    let (inst_a, _dst_a, inputs_a) = match a {
        MirDagNode::Compute {
            inst,
            dst,
            input_regs,
        } => (inst, dst, input_regs),
        _ => return false,
    };
    let (inst_b, _dst_b, inputs_b) = match b {
        MirDagNode::Compute {
            inst,
            dst,
            input_regs,
        } => (inst, dst, input_regs),
        _ => return false,
    };

    if inputs_a.len() != inputs_b.len() {
        return false;
    }

    // Same instruction category?
    if !same_inst_category(inst_a, inst_b) {
        return false;
    }

    // Same data sources for each input register?
    for (&reg_a, &reg_b) in inputs_a.iter().zip(inputs_b.iter()) {
        let src_a = find_data_source(dag, a_id, reg_a);
        let src_b = find_data_source(dag, b_id, reg_b);
        if src_a != src_b {
            return false;
        }
    }

    true
}

/// Check if two instructions are in the same "category" for CSE purposes.
fn same_inst_category(a: &MirInst, b: &MirInst) -> bool {
    use crate::mir::MirInst;
    match (a, b) {
        (MirInst::Const(_, v1), MirInst::Const(_, v2)) => v1 == v2,
        (MirInst::BinaryOp(_, _, op1, _), MirInst::BinaryOp(_, _, op2, _)) => op1 == op2,
        (MirInst::Call(_, n1, _), MirInst::Call(_, n2, _)) => n1 == n2,
        (MirInst::MethodCall(_, _, m1, _), MirInst::MethodCall(_, _, m2, _)) => m1 == m2,
        (MirInst::ListLit(_, _), MirInst::ListLit(_, _)) => true,
        (MirInst::DictLit(_, _), MirInst::DictLit(_, _)) => true,
        (MirInst::Prompt(_, _), MirInst::Prompt(_, _)) => true,
        _ => false,
    }
}

// ─── Rule 4: Algebraic Simplification on DAG ────────────────────────

enum ReplaceWith {
    ReplaceWithConst(Reg, Value),
    ReplaceWithSource(Reg, Option<NodeId>),
}

/// Simplifies `x+0→x`, `x*1→x`, `x*0→0`, `x/1→x`, etc.
pub struct AlgebraicSimplifyDagRule;

impl DagRewriteRule for AlgebraicSimplifyDagRule {
    fn name(&self) -> &'static str {
        "dag_algebraic"
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

    fn rewrite(&self, node_id: NodeId, dag: &MirDag) -> Option<DagRewrite> {
        let node = dag.nodes.get(node_id)?;
        let (dst, lhs_reg, op, rhs_reg) = match node {
            MirDagNode::Compute {
                inst: MirInst::BinaryOp(d, l, o, r),
                ..
            } => (d, l, o, r),
            _ => return None,
        };

        let lhs_src = find_data_source(dag, node_id, *lhs_reg);
        let rhs_src = find_data_source(dag, node_id, *rhs_reg);

        let lhs_val = lhs_src.and_then(|id| const_value(&dag.nodes[id]));
        let rhs_val = rhs_src.and_then(|id| const_value(&dag.nodes[id]));

        use crate::common::BinaryOp::*;
        let (replacement, removed) = match (op, lhs_val, rhs_val) {
            // x + 0 → x
            (Add, Some(Value::Int(0)), _) => (
                ReplaceWith::ReplaceWithSource(*rhs_reg, rhs_src),
                vec![node_id],
            ),
            (Add, _, Some(Value::Int(0))) => (
                ReplaceWith::ReplaceWithSource(*lhs_reg, lhs_src),
                vec![node_id],
            ),
            // x * 1 → x
            (Mul, Some(Value::Int(1)), _) => (
                ReplaceWith::ReplaceWithSource(*rhs_reg, rhs_src),
                vec![node_id],
            ),
            (Mul, _, Some(Value::Int(1))) => (
                ReplaceWith::ReplaceWithSource(*lhs_reg, lhs_src),
                vec![node_id],
            ),
            // x * 0 → 0
            (Mul, Some(Value::Int(0)), _) => (
                ReplaceWith::ReplaceWithConst(*dst, Value::Int(0)),
                vec![node_id],
            ),
            (Mul, _, Some(Value::Int(0))) => (
                ReplaceWith::ReplaceWithConst(*dst, Value::Int(0)),
                vec![node_id],
            ),
            // x - 0 → x
            (Sub, _, Some(Value::Int(0))) => (
                ReplaceWith::ReplaceWithSource(*lhs_reg, lhs_src),
                vec![node_id],
            ),
            _ => return None,
        };

        match replacement {
            ReplaceWith::ReplaceWithConst(d, v) => Some(DagRewrite {
                added: vec![MirDagNode::Compute {
                    inst: MirInst::Const(d, v),
                    dst: d,
                    input_regs: vec![],
                }],
                removed,
                added_edges: vec![],
            }),
            ReplaceWith::ReplaceWithSource(reg, Some(src_id)) => {
                let out_edges: Vec<(NodeId, NodeId, EdgeKind)> = dag
                    .edges
                    .iter()
                    .filter(|e| e.from == node_id)
                    .map(|e| (src_id, e.to, EdgeKind::Data { reg }))
                    .collect();
                Some(DagRewrite {
                    added: vec![],
                    removed,
                    added_edges: out_edges,
                })
            }
            _ => None,
        }
    }

    fn cost_gain(&self) -> i32 {
        2
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::BinaryOp;
    use crate::mir::dag;
    use crate::mir::{MirFunction, MirInst};

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
    fn const_folding_folds_two_consts() {
        // r0=10, r1=32, r2=r0+r1  →  should fold to r2=42
        let dag = make_dag(vec![
            MirInst::Const(0, Value::Int(10)),
            MirInst::Const(1, Value::Int(32)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
        ]);
        // Find the BinaryOp node
        let binop_id = dag
            .nodes
            .iter()
            .position(|n| {
                matches!(
                    n,
                    MirDagNode::Compute {
                        inst: MirInst::BinaryOp(..),
                        ..
                    }
                )
            })
            .unwrap();

        let rule = ConstFoldingDagRule;
        let rw = rule.rewrite(binop_id, &dag).expect("should fold constants");
        assert_eq!(rw.added.len(), 1, "should add one Const node");
        assert!(rw.removed.contains(&binop_id), "should remove BinaryOp");
        // The new node should be a Const with value 42
        match &rw.added[0] {
            MirDagNode::Compute {
                inst: MirInst::Const(_, v),
                ..
            } => {
                assert_eq!(*v, Value::Int(42));
            }
            _ => panic!("expected Const node"),
        }
    }

    #[test]
    fn const_folding_skips_non_const_operands() {
        // r0=Var("x"), r1=10, r2=r0+r1  →  should NOT fold
        let dag = make_dag(vec![
            MirInst::Var(0, "x".to_string()),
            MirInst::Const(1, Value::Int(10)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
        ]);
        let binop_id = dag
            .nodes
            .iter()
            .position(|n| {
                matches!(
                    n,
                    MirDagNode::Compute {
                        inst: MirInst::BinaryOp(..),
                        ..
                    }
                )
            })
            .unwrap();
        let rule = ConstFoldingDagRule;
        assert!(
            rule.rewrite(binop_id, &dag).is_none(),
            "should not fold non-const lhs"
        );
    }

    #[test]
    fn cse_eliminates_duplicate_binaryop() {
        // r0=10, r1=20, r2=r0+r1, r3=r0+r1  — r3 is a duplicate of r2
        let dag = make_dag(vec![
            MirInst::Const(0, Value::Int(10)),
            MirInst::Const(1, Value::Int(20)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
            MirInst::BinaryOp(3, 0, BinaryOp::Add, 1),
        ]);
        // BinaryOp at r3 (node with dst=3) should be eliminated
        let dup_id = dag
            .nodes
            .iter()
            .position(|n| matches!(n, MirDagNode::Compute { dst: 3, .. }))
            .unwrap();
        let rule = CseDagRule;
        let rw = rule
            .rewrite(dup_id, &dag)
            .expect("should eliminate duplicate");
        assert!(
            rw.removed.contains(&dup_id),
            "should remove the duplicate BinaryOp"
        );
    }

    #[test]
    fn cse_preserves_different_ops() {
        let dag = make_dag(vec![
            MirInst::Const(0, Value::Int(10)),
            MirInst::Const(1, Value::Int(20)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
            MirInst::BinaryOp(3, 0, BinaryOp::Mul, 1), // different op
        ]);
        let dup_id = dag
            .nodes
            .iter()
            .position(|n| matches!(n, MirDagNode::Compute { dst: 3, .. }))
            .unwrap();
        let rule = CseDagRule;
        assert!(
            rule.rewrite(dup_id, &dag).is_none(),
            "different ops should not be eliminated"
        );
    }

    #[test]
    fn algebraic_x_plus_zero() {
        // r0=Var("x"), r1=0, r2=r0+r1  →  should simplify to just r0
        let dag = make_dag(vec![
            MirInst::Var(0, "x".to_string()),
            MirInst::Const(1, Value::Int(0)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
        ]);
        let binop_id = dag
            .nodes
            .iter()
            .position(|n| matches!(n, MirDagNode::Compute { dst: 2, .. }))
            .unwrap();
        let rule = AlgebraicSimplifyDagRule;
        let rw = rule.rewrite(binop_id, &dag).expect("x+0 should simplify");
        assert!(rw.removed.contains(&binop_id), "should remove the add");
    }

    #[test]
    fn algebraic_x_times_one() {
        let dag = make_dag(vec![
            MirInst::Var(0, "x".to_string()),
            MirInst::Const(1, Value::Int(1)),
            MirInst::BinaryOp(2, 0, BinaryOp::Mul, 1),
        ]);
        let binop_id = dag
            .nodes
            .iter()
            .position(|n| matches!(n, MirDagNode::Compute { dst: 2, .. }))
            .unwrap();
        let rule = AlgebraicSimplifyDagRule;
        let rw = rule.rewrite(binop_id, &dag).expect("x*1 should simplify");
        assert!(rw.removed.contains(&binop_id));
    }

    #[test]
    fn algebraic_x_times_zero() {
        let dag = make_dag(vec![
            MirInst::Var(0, "x".to_string()),
            MirInst::Const(1, Value::Int(0)),
            MirInst::BinaryOp(2, 0, BinaryOp::Mul, 1),
        ]);
        let binop_id = dag
            .nodes
            .iter()
            .position(|n| matches!(n, MirDagNode::Compute { dst: 2, .. }))
            .unwrap();
        let rule = AlgebraicSimplifyDagRule;
        let rw = rule
            .rewrite(binop_id, &dag)
            .expect("x*0 should simplify to 0");
        assert_eq!(rw.added.len(), 1);
        match &rw.added[0] {
            MirDagNode::Compute {
                inst: MirInst::Const(_, v),
                ..
            } => assert_eq!(*v, Value::Int(0)),
            _ => panic!("expected Const(0)"),
        }
    }
}
