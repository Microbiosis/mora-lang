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
    ///
    /// v0.104.6 D31：走 [`DagIndex`] 的出边桶，不再为「找第一条出边」扫
    /// 整张边表。旧实现 `for edge in &dag.edges { if edge.from == node_id }`
    /// 在每次改写后都要扫全部 E 条边才找到第一条 —— 375 个 `let` 的实测里
    /// 改写 374 次、E=4876，即 374×4876 ≈ 1.8M 次无效比较。
    fn mark_dirty(&mut self, node_id: NodeId, dag: &MirDag, idx: &DagIndex) {
        if node_id >= self.dirty.len() {
            return; // new nodes added by rewrite: already dirty-at-creation
        }
        if self.dirty[node_id] {
            return; // already dirty, skip to avoid infinite recursion
        }
        self.dirty[node_id] = true;
        // Propagate to all consumers (nodes that have an edge FROM this node).
        for &ei in idx.out(node_id) {
            let to = dag.edges[ei as usize].to;
            self.mark_dirty(to, dag, idx);
        }
    }

    /// Check if a node is eligible for optimization.
    fn can_optimize(&self, node_id: NodeId, node: &MirDagNode) -> bool {
        !node.is_removed() && self.dirty[node_id] && self.exec_count[node_id] < self.max_exec
    }
}

// ─── DagIndex ──────────────────────────────────────────────────────────

/// 邻接索引：节点 ↔ 边**下标**的双向映射。
///
/// # v0.104.6 D31：为什么规则需要它
///
/// 修前每条规则回调都拿到**整个 `&dag`**，为查「某寄存器的定义节点」
/// 「谁还在用我」只能线性扫边表。六处这样的扫描（`find_data_source` /
/// `outgoing_data_edges` / DCE `has_outgoing` / `seq_reachable` /
/// `is_control_target` / `mark_dirty`）乘以「规则数 × 节点数」次调用 =
/// 实测 375 个 `let` 那一档 13 146 次规则调用 × 4 876 条边 ≈ 6.4×10⁷ 次边
/// 比较，耗时 ~n^2.5（n=375 → 22.9 s）。
///
/// # 为什么可以纯追加维护、不会陈旧
///
/// 这是本索引成立的全部依据，依赖 `apply_rewrite` 的三条不变量：
///
/// 1. **边只 push、从不删除** —— step 4 明确「removed 节点的边不剥离」，
///    节点被删只是变**透明穿通**（`Removed` 标记），边仍留在 `dag.edges` 里。
/// 2. **节点只被标记 `Removed`、从不物理移除** —— 新节点一律追加到末尾，
///    `NodeId` 单调递增，不存在「下标被回收」。
/// 3. **唯一的原地内容改写是 step 4b 的 `reg_rename`**，它改的是边上的
///    `reg` 字段。本索引**只存边下标**、查询时读 `dag.edges[ei]` 的实时
///    值，故寄存器改名对索引透明。
///
/// 三者合起来 → 索引与图**恒等**，`sync` 只是把新 push 的边补进桶里。
/// 因此查询结果与线性扫描**逐字等价**，只是把 O(E) 换成 O(度)。
pub struct DagIndex {
    /// `outgoing[n]` = 以 `n` 为 `from` 的边在 `dag.edges` 中的下标。
    outgoing: Vec<Vec<u32>>,
    /// `incoming[n]` = 以 `n` 为 `to` 的边在 `dag.edges` 中的下标。
    incoming: Vec<Vec<u32>>,
    /// 已并入索引的边数。`dag.edges` 只增，故单调不减。
    indexed: usize,
}

impl DagIndex {
    /// 全量构建（首次进入优化循环时调用一次，O(V+E)）。
    pub fn build(dag: &MirDag) -> Self {
        let mut idx = DagIndex {
            outgoing: vec![Vec::new(); dag.nodes.len()],
            incoming: vec![Vec::new(); dag.nodes.len()],
            indexed: 0,
        };
        idx.sync(dag, 0);
        idx
    }

    /// 把 `dag.edges[from..]`（即自 `from` 起新 push 的边）补进索引。
    ///
    /// 必须在**每次** `apply_rewrite` 之后调用，使索引与图保持恒等。
    /// 摊销代价 O(新增边数)。
    pub fn sync(&mut self, dag: &MirDag, from: usize) {
        // 新增节点带来的下标空间
        if self.outgoing.len() < dag.nodes.len() {
            self.outgoing.resize_with(dag.nodes.len(), Vec::new);
            self.incoming.resize_with(dag.nodes.len(), Vec::new);
        }
        let start = from.max(self.indexed);
        for (ei, e) in dag.edges.iter().enumerate().skip(start) {
            let ei32 = ei as u32;
            if let Some(b) = self.outgoing.get_mut(e.from) {
                b.push(ei32);
            }
            if let Some(b) = self.incoming.get_mut(e.to) {
                b.push(ei32);
            }
        }
        self.indexed = self.indexed.max(dag.edges.len());
    }

    /// `n` 的出边下标（空切片表示无出边）。
    #[inline]
    pub fn out(&self, n: NodeId) -> &[u32] {
        self.outgoing.get(n).map_or(&[], |v| v.as_slice())
    }

    /// `n` 的入边下标。
    #[inline]
    pub fn inc(&self, n: NodeId) -> &[u32] {
        self.incoming.get(n).map_or(&[], |v| v.as_slice())
    }

    /// `n` 是否有出边（等价于旧代码的 `dag.edges.iter().any(|e| e.from == n)`）。
    #[inline]
    pub fn has_outgoing(&self, n: NodeId) -> bool {
        !self.out(n).is_empty()
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
    // v0.104.6 D31：邻接索引，建一次 + 每次改写后补新边（见 `DagIndex` 的
    // 「为什么可以纯追加维护」）。规则回调经它把 O(E) 的全图扫描换成 O(度)。
    let mut idx = DagIndex::build(dag);

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
                    if !rule.matches(node_id, node, dag, &idx) {
                        continue;
                    }
                    if let Some(rw) = rule.rewrite(node_id, dag, &idx) {
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

                    // v0.104.6 D31：改写前的边数 —— `apply_rewrite` 只 push
                    // 不删边（见 `DagIndex` 文档的不变量 1），故事后把这段
                    // 新边补进索引即与图重新恒等。
                    let edges_before = dag.edges.len();
                    apply_rewrite(dag, rw);
                    idx.sync(dag, edges_before);
                    opt.exec_count[node_id] += 1;
                    // Re-mark the rewritten node and its consumers
                    opt.mark_dirty(node_id, dag, &idx);
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
/// v0.104.6: 本模块曾持有 `sequence_components`（Sequence 连通分量 ≡ 基本块），
/// 现已上移到 `MirDag::sequence_components` —— 它是**图本身的属性**（执行器的
/// 就绪门槛也要用），不属于优化器私有逻辑。
///
/// Apply a `DagRewrite` to the DAG in-place.
fn apply_rewrite(dag: &mut MirDag, rw: DagRewrite) {
    let old_len = dag.nodes.len();
    let new_base = old_len; // new nodes start at this index

    // 1. Add new nodes
    //
    // v0.104.6 D9：追加节点必须**继承被替换节点的可达性**。
    //
    // `ConstFoldingDagRule` 等规则是「新增节点 + 标旧节点 Removed」，而
    // `dag.reachable` 是 `dag_analyze` 按当时的 `nodes.len()` 分配的 —— 新追加
    // 的下标落在数组之外，语义上等同 `false`（不可达）。后果：追加节点不进入
    // 入口集、其激活路径被 `seq_preds` 可达性过滤排除 → 永不执行 → 它写出的
    // 寄存器永不 ready → 消费者卡住 → **顶层结果变 Nil**
    // （实测：`mir_ssa_roundtrip` 的 `top_level_const_fold_equiv` /
    // `top_level_variable_equiv` / `top_level_reassignment_equiv`，
    // 报错「Basic 改变顶层结果: baseline=Ok(Float(3.0)) basic=Ok(Nil)」）。
    //
    // 追加节点是旧节点的等价替代物，可达性必然相同，故从 `rw.removed` 里
    // 取任一旧节点继承（removed 与 added 一一对应，见各规则的构造）。
    // 追加节点是旧节点的等价替代物，可达性必然与旧节点相同。
    //
    // 判据用 `removed` 里**任一可达**的旧节点（而非「全可达」）：`removed`
    // 与 `added` 由同一条规则成对产出（`ConstFoldingDagRule` 折一个、删一个），
    // 正常情况下两者可达性一致；取「任一可达」只是对不齐时的宽松取值 ——
    // 漏继承会让追加节点不被激活、其写出的寄存器永不 ready，比误继承更糟。
    let inherit = rw
        .removed
        .iter()
        .any(|&r| dag.reachable.get(r).copied().unwrap_or(false));
    for (i, node) in rw.added.into_iter().enumerate() {
        let new_id = old_len + i;
        dag.reachable.resize(new_id + 1, inherit);
        dag.reachable[new_id] = inherit;
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

    // 4. v0.104.6 D9：**不剥离** removed 节点的边 —— removed 节点改为
    // **透明穿节点**（pass-through）。
    //
    // DAG 执行器（`vm/dag.rs`）对 `MirDagNode::Removed` 的 match 分支是空操作
    // `=> {}`，但它仍进入 `ready`（`node_ready` 对 Removed 恒真）并参与边传播
    // —— 于是 removed 节点**沿其原出边继续激活后继**，`dag.reachable` 在
    // 优化后依然成立。
    //
    // **为何必须这样**：可达性是图的不变量。原实现用
    // `edges.retain(!removed)` 剥离全部出入边，把「删掉节点」变成了「同时删掉
    // 它的控制流」，再用 4a 的 Sequence 缝合去**部分**补回 —— 但缝合只覆盖
    // 「同 Sequence 连通分量内」的前后继。
    //
    // 穿通语义把两件事解耦：节点的工作被消除（不再执行任何指令），控制流
    // 原样保留。4a 的缝合随之**不再需要**（它当年只是为补偿剥边而加，见其
    // 注释里记录的 v0.75.33 / v0.103 两个历史缺陷），保留反而会重复激活。
    // 死活性判定由 `DeadNodeDagRule` 按「消费方是否全是 Removed」另行处理。
    let removed_set: HashSet<NodeId> = rw.removed.iter().copied().collect();
    let _ = &removed_set;

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
    //
    // v0.104.6 D9：改走 `recompute_entry()`，恢复 `dag_analyze` 自 v0.104.2
    // 起用、却被此处的旧公式（「仅无入边」）悄悄撤销的「可达 ∧ 无入边」过滤 ——
    // 优化留下的**不可达死块**（常量条件折叠后的 else 臂、`Continue` 之后
    // 残留的 `Copy(dst,…)` 等）会被拉回入口集与真入口并列执行，其写入覆盖
    // 真实结果 → 静默返回错值。
    //
    // 可行性依赖第 4 步的**透明穿通**：`dag.reachable` 由 `dag_analyze` 在图
    // 良构时算出，删节点不剥离其边即可保持有效。
    dag.recompute_entry();

    // v0.104.6 D9：`dag_analyze` 的 `reachable` 是**从 pc 0** 出发的，但优化
    // 会改变入口 —— 直线链 `1+2 → +3` 经 CSE 折叠后，CSE 追加的折叠节点成为
    // 新的 entry，而原来的节点 0（也是原入口）被标 Removed。此时：
    //
    //   * `reachable` 仍说「从节点 0 可达」，故节点 0/1/2 标记为可达；
    //   * 但**没人激活它们** —— 新入口是折叠节点，它只沿边到达后面的节点。
    //
    // 于是节点 0/1/2 会作为 Sequence 前驱进入 `seq_preds`，却永不执行
    // （`executed[]` 恒 false），把其后继永久阻塞在就绪门槛外 → 结果停在
    // 折叠节点写的值（实测：手写直线链优化路径得 3，应 6）。
    //
    // **正确判据：不能被激活的节点不得参与就绪门槛。** 故在每次 rewrite 后按
    // **当前 entry** 重算可达集（沿全部边 BFS），再据此重算 entry —— 两者互为
    // 依赖，迭代到不动点（至多几轮即可收敛：删节点只会减少可达集）。
    for _ in 0..4 {
        let before: Vec<bool> = dag.reachable.clone();
        dag.recompute_reachable_from_entry();
        dag.recompute_entry();
        if dag.reachable == before {
            break;
        }
    }

    // v0.104.6 D31：位向量取代 `HashSet`（同 `recompute_entry` 的理由 ——
    // 本段每次改写都跑，是「改写次数 × (V+E)」的那一项）。
    let n = dag.nodes.len();
    let mut has_outgoing = vec![false; n];
    for edge in &dag.edges {
        if edge.from < n {
            has_outgoing[edge.from] = true;
        }
    }
    dag.exit = (0..n)
        .filter(|&x| !dag.nodes[x].is_removed() && !has_outgoing[x])
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
    use crate::mir::optimize::dag_optimize;
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

            ..Default::default()
        };
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
        let before = d.sequence_components();
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
        let mut dag = make_dag(vec![
            MirInst::Const(0, Value::Int(10)),
            MirInst::Const(1, Value::Int(20)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1), // entry
            MirInst::BinaryOp(3, 0, BinaryOp::Add, 1), // duplicate → CSE removes
        ]);
        let dup_node = 3usize;
        let stages: Vec<Vec<Box<dyn DagRewriteRule>>> = vec![
            vec![Box::new(AlgebraicSimplifyDagRule)],
            vec![Box::new(CfRule)],
            vec![Box::new(CseDagRule)],
            vec![Box::new(DnRule)],
        ];
        let cost = TokenEstimate;
        dag_search_staged(&mut dag, &stages, &cost);

        // v0.104.6：断言「重复节点被消除」这一**语义**，而不是节点计数。
        //
        // 旧断言 `after < before` 编码的是实现细节「删 1 个 → 计数下降」，但
        // `ConstFoldingDagRule` 是「**新增**一个折叠后的 Const 节点 + 把旧
        // 节点标 Removed」，删 1 增 1 恰好抵消（实测 4 -> 4），与 CSE 是否生效
        // 无关。改断言被消除的那个具体节点，才能真正锁住 CSE 的行为。
        assert!(
            dag.nodes[dup_node].is_removed(),
            "CSE 应消除重复的 BinaryOp（node[{dup_node}]）"
        );
        // 且折叠后的等价常量节点应存在，携带 r3 的值（30）
        let folded_r3 = dag.nodes.iter().any(|n| {
            matches!(n, MirDagNode::Compute { dst, inst: MirInst::Const(_, Value::Int(v)), .. } if *dst == 3 && *v == 30)
        });
        assert!(folded_r3, "r3 的值应被常量折叠保留（Const(3, 30)）");
    }

    // ─── v0.75.5: Cascades 同 stage 择优 ──────────────────────────

    /// 测试用低收益规则：匹配 BinaryOp，只移除自身（InstructionCount delta=1）。
    /// 用于证明同 stage 内选 max delta 而非"先匹配先应用"。
    struct TestSmallGainRule;

    impl DagRewriteRule for TestSmallGainRule {
        fn name(&self) -> &'static str {
            "test_small_gain"
        }

        fn matches(
            &self,
            _node_id: NodeId,
            node: &MirDagNode,
            _dag: &MirDag,
            _idx: &DagIndex,
        ) -> bool {
            matches!(
                node,
                MirDagNode::Compute {
                    inst: MirInst::BinaryOp(..),
                    ..
                }
            )
        }

        fn rewrite(&self, node_id: NodeId, _dag: &MirDag, _idx: &DagIndex) -> Option<DagRewrite> {
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
