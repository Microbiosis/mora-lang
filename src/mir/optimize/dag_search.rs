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


/// v0.103: Sequence 边连通分量 —— 每个分量恰是一个基本块。
///
/// `dag_analyze` 对每个基本块内的**相邻指令对**无条件连 Sequence 边
/// （`prev_node` 链），因此块内节点由 Sequence 边连成一条链；而跨块的
/// 控制转移（Jump/Branch 目标）只连 Control 边，不连 Sequence。
/// 于是「Sequence 连通分量 ≡ 基本块」这一对应关系成立，可在不引入额外
/// 块信息的前提下判定两个节点是否同块。
///
/// 用途见 [`apply_rewrite`] 的 Sequence 缝合 —— 跨块缝合会破坏该等价关系，
/// 故缝合前用它把配对限制在同块内。
fn sequence_components(dag: &MirDag) -> Vec<usize> {
    let n = dag.nodes.len();
    let mut parent: Vec<usize> = (0..n).collect();

    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }

    for e in &dag.edges {
        if !matches!(e.kind, crate::mir::dag::EdgeKind::Sequence) {
            continue;
        }
        let (ra, rb) = (find(&mut parent, e.from), find(&mut parent, e.to));
        if ra != rb {
            parent[ra] = rb;
        }
    }

    (0..n).map(|i| find(&mut parent, i)).collect()
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

    // 4a. v0.75.33: Sequence 缝合 — removed 节点若位于线性链中间
    // （如 let 占位 Const 被 CSE 合并进等价节点），直接删边会断开保序链
    // （Define → 占位 → 下一语句 之间的 Sequence 断裂 → 后续 Var 提前
    // 执行读脏值，如 `let c = 5` 后 `let d = c + 1` 的 Var(c) 抢跑）。
    // 收集 removed 节点的 Sequence 前驱/后继，补「前驱→后继」跳过 removed，
    // 保持线性执行顺序。
    let seq_preds: Vec<NodeId> = dag
        .edges
        .iter()
        .filter(|e| {
            !removed_set.contains(&e.from)
                && removed_set.contains(&e.to)
                && matches!(e.kind, crate::mir::dag::EdgeKind::Sequence)
        })
        .map(|e| e.from)
        .collect();
    let seq_succs: Vec<NodeId> = dag
        .edges
        .iter()
        .filter(|e| {
            removed_set.contains(&e.from)
                && !removed_set.contains(&e.to)
                && matches!(e.kind, crate::mir::dag::EdgeKind::Sequence)
        })
        .map(|e| e.to)
        .collect();
    // v0.103: 缝合必须**块内**进行 —— Sequence 边是「基本块内全序」，
    // DAG 执行器把它当作激活通道（scan 阶段沿 Sequence 边推入消费者）。
    // 跨块缝合会让「另一个控制区域的节点」被提前激活并对当前区域产生副作用。
    //
    // **缺陷背景（`while ... if ... break ... end` 死循环）**：CSE 把循环体
    // 内块的 `Const(Int(1))` 合并进内层 if 块的同值 `Const`，缝合就把
    // 「if 块 → 循环体内块」连成 Sequence 边。于是即便 `break` 分支被选中，
    // 递增语句仍被该边激活执行，循环回边再次点火 → break 被完全架空、挂死。
    //
    // 判据：Sequence 边只在基本块内创建，故「Sequence 连通分量 ≡ 基本块」。
    // 只连接同一分量的前驱/后继，即恢复构造器的不变量（无需额外块信息）。
    let comps = sequence_components(dag);
    for &a in &seq_preds {
        for &b in &seq_succs {
            if comps[a] != comps[b] {
                continue;
            }
            dag.edges.push(crate::mir::dag::MirDagEdge {
                from: a,
                to: b,
                kind: crate::mir::dag::EdgeKind::Sequence,
            });
        }
    }
    dag.edges
        .retain(|e| !removed_set.contains(&e.from) && !removed_set.contains(&e.to));

    // 4b. v0.75.33: 寄存器重命名 — CSE 合并不同 dst 的节点后，dag_interp
    // 按 input_regs 取数（不按 Data 边），必须把存活消费者的寄存器引用
    // 从旧 dst 改写到新 dst，否则旧 dst 失去 producer → 消费者永不 ready。
    // 示例：`Const(Nil)` 占位节点 dst=7 被合并进 dst=4 的等价节点后，
    // `Assign("__let_result", 7)` 的 input_regs 里的 7 必须改为 4。
    if let Some((old_reg, new_reg)) = rw.reg_rename {
        for node in dag.nodes.iter_mut() {
            if node.is_removed() {
                continue;
            }
            match node {
                MirDagNode::Compute {
                    inst, input_regs, ..
                } => {
                    *inst = inst.map_regs(&mut |r| {
                        if r == old_reg { new_reg } else { r }
                    });
                    for r in input_regs.iter_mut() {
                        if *r == old_reg {
                            *r = new_reg;
                        }
                    }
                }
                MirDagNode::Effect { inst } => {
                    *inst = inst.map_regs(&mut |r| {
                        if r == old_reg { new_reg } else { r }
                    });
                }
                MirDagNode::Branch { cond, .. } => {
                    if *cond == old_reg {
                        *cond = new_reg;
                    }
                }
                MirDagNode::Phi { reg, sources } => {
                    if *reg == old_reg {
                        *reg = new_reg;
                    }
                    for (_, src) in sources.iter_mut() {
                        if *src == old_reg {
                            *src = new_reg;
                        }
                    }
                }
                _ => {}
            }
        }
        // 同步改写 Data 边上的寄存器号，保持「边 / input_regs」一致。
        for e in dag.edges.iter_mut() {
            if let crate::mir::dag::EdgeKind::Data { reg } = &mut e.kind
                && *reg == old_reg
            {
                *reg = new_reg;
            }
        }
    }

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
    use crate::mir::optimize::dag_optimize;
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
        
            ..Default::default()};
        dag::dag_analyze(&func)
    }

    /// v0.103: `dag_optimize` 后**不得存在跨基本块的 Sequence 边**。
    ///
    /// Sequence 边是 DAG 执行器的控制激活通道（scan 沿它推入消费者），
    /// 语义是「同一基本块内相邻指令的保序」。一旦跨块，另一个控制区域的
    /// 节点会被提前激活执行。
    ///
    /// 缺陷背景（`while ... if ... break ... end` 死循环）：CSE 合并同值
    /// `Const` 后把该节点的**出边全部**重定向到合并目标 —— 目标可能在别的
    /// 基本块，于是「内层 if 块的 Const → 循环体内块的增量」出现，`break`
    /// 被选中时增量仍被激活，回边再次点火 → 挂死。
    #[test]
    fn dag_optimize_keeps_sequence_edges_intra_block() {
        // 形状：外层常量 + 循环（回边）+ 内层分支（break），
        // 三个基本块，且相邻块首指令与循环体内的常量同值（触发 CSE 合并）。
        let mut d = make_dag(vec![
            // 块 A
            MirInst::Const(0, Value::Int(0)),
            MirInst::Const(1, Value::Int(1)),
            MirInst::JumpIfNot(0, 10),
            // 块 B（循环体入口）
            MirInst::Const(2, Value::Int(1)),
            MirInst::Const(3, Value::Int(1)),
            MirInst::JumpIf(1, 10),
            // 块 C（循环体后段）
            MirInst::Const(4, Value::Int(1)),
            MirInst::BinaryOp(5, 2, BinaryOp::Add, 4),
            MirInst::Jump(3),
            // 块 D（退出）
            MirInst::Const(6, Value::Nil),
        ]);
        let before = sequence_components(&d);
        dag_optimize(&mut d);
        let cross: Vec<(usize, usize)> = d
            .edges
            .iter()
            .filter(|e| {
                matches!(e.kind, crate::mir::dag::EdgeKind::Sequence)
                    && !d.nodes[e.from].is_removed()
                    && !d.nodes[e.to].is_removed()
                    && before[e.from] != before[e.to]
            })
            .map(|e| (e.from, e.to))
            .collect();
        assert!(
            cross.is_empty(),
            "CSE 重定向产生了跨基本块的 Sequence 边 {:?} —— 会提前激活别的控制区域的节点",
            cross
        );
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
                reg_rename: None,
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

    // ─── v0.75.33: CSE 寄存器重命名回归 ─────────────────────────────

    /// 两个不同 dst 的等价 Const 被 CSE 合并后，消费者的 input_regs 必须
    /// 从旧 dst 改写为新 dst。dag_interp 按 input_regs（寄存器号）取数、
    /// 不按 Data 边；旧实现在 SSA 下合并不同 dst 节点只重定向边，导致
    /// 被合并的 dst 失去 producer → 消费者永不 ready（`let x = 1` 后
    /// 再 `let x = 2` 的占位 Const 是典型受害者，见 for/while 循环的
    /// 尾部 `__let_result` 占位）。
    #[test]
    fn cse_renames_consumer_regs_on_merge() {
        let mut dag = make_dag(vec![
            MirInst::Const(4, Value::Nil),             // n0: let 占位（dst=4）
            MirInst::Assign("__let_result".into(), 4), // n1: 消费 reg4
            MirInst::Const(7, Value::Nil),             // n2: 第二个 let 占位（dst=7）
            MirInst::Assign("__let_result".into(), 7), // n3: 消费 reg7
        ]);
        let stages: Vec<Vec<Box<dyn DagRewriteRule>>> = vec![vec![Box::new(CseDagRule)]];
        dag_search_staged(&mut dag, &stages, &InstructionCount);

        // 等价的两个 Nil Const 合并：n2（dst=7）被删，n0（dst=4）存活。
        assert!(dag.nodes[0].is_removed() || dag.nodes[2].is_removed());
        let removed_dst = if dag.nodes[0].is_removed() { 4 } else { 7 };

        // 没有任何存活消费者引用被删除节点的 dst — rename 必须已生效。
        for (id, node) in dag.nodes.iter().enumerate() {
            if node.is_removed() {
                continue;
            }
            match node {
                MirDagNode::Effect { inst } => {
                    for r in inst.input_regs() {
                        assert_ne!(
                            r, removed_dst,
                            "Effect n{id} 仍引用被删节点的 dst={removed_dst}（CSE rename 未生效）"
                        );
                    }
                }
                MirDagNode::Compute { input_regs, .. } => {
                    for r in input_regs {
                        assert_ne!(
                            *r, removed_dst,
                            "Compute n{id} 仍引用被删节点的 dst={removed_dst}"
                        );
                    }
                }
                _ => {}
            }
        }
        // 两个 Assign（n1/n3）必须都引用存活的 dst — 原 reg7 消费者已重命名。
        let assign_regs: Vec<usize> = dag
            .nodes
            .iter()
            .filter_map(|n| match n {
                MirDagNode::Effect {
                    inst: MirInst::Assign(_, r),
                } => Some(*r),
                _ => None,
            })
            .collect();
        assert_eq!(assign_regs.len(), 2, "两个 Assign 都应存活");
        assert!(
            assign_regs.iter().all(|&r| r != removed_dst),
            "Assign 寄存器应全部重命名离被删 dst: {assign_regs:?}"
        );
    }
}
