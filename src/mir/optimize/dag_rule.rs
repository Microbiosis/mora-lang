//! v0.59: DAG-level rewrite rules — Cascades on MirDag.
//!
//! Unlike linear MirInst rewrite rules that scan backward through
//! the instruction stream, DAG rules navigate the explicit `MirDagEdge`
//! graph. A rule matches a subgraph of `MirDagNode`s and produces
//! a `DagRewrite` that describes which nodes to add, remove, and
//! which edges to redirect.

use crate::mir::dag::{EdgeKind, MirDag, MirDagEdge, MirDagNode, NodeId};
// v0.104.6 D31：规则的邻接查询一律经它，避免退回 O(E) 全图扫描。
use crate::mir::optimize::dag_search::DagIndex;
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
    /// v0.75.33: 寄存器重命名 — CSE 合并不同 dst 的节点时，把旧 dst 的
    /// 消费者引用改写到新 dst。dag_interp 按 input_regs（寄存器号）
    /// 取数、不按 Data 边，仅重定向边会让旧 dst 失去 producer → 消费者
    /// 永不 ready。`Some((old, new))` 表示把 old 重命名为 new。
    pub reg_rename: Option<(Reg, Reg)>,
}

impl DagRewrite {
    pub fn empty() -> Self {
        DagRewrite {
            added: vec![],
            removed: vec![],
            added_edges: vec![],
            reg_rename: None,
        }
    }
}

/// A DAG-level rewrite rule.
pub trait DagRewriteRule {
    fn name(&self) -> &'static str;

    /// Does this rule apply to the given node?
    ///
    /// v0.104.6 D31：新增 `idx` 邻接索引参数。**必须用它做邻接查询**
    /// （`idx.out(n)` / `idx.inc(n)`），不要退回 `dag.edges.iter()` 全图扫
    /// —— 那是本缺陷的根因，实测让 375 条语句的编译从 22.9 s 变成二次方。
    fn matches(&self, node_id: NodeId, node: &MirDagNode, dag: &MirDag, idx: &DagIndex) -> bool;

    /// Produce a rewrite for the given node, or None if not applicable.
    fn rewrite(&self, node_id: NodeId, dag: &MirDag, idx: &DagIndex) -> Option<DagRewrite>;

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
///
/// v0.104.6 D31：走 `idx.inc(node_id)` 的入边桶，O(入度) 而非 O(E)。
/// 索引与图恒等（见 `DagIndex` 文档），故结果与旧的全表 `find_map`
/// **逐字相同** —— 包括「同一 (node, reg) 有多条 Data 边时取第一条」
/// 这一顺序语义：桶内按边在 `dag.edges` 中的下标升序，与全表扫描同序。
fn find_data_source(dag: &MirDag, idx: &DagIndex, node_id: NodeId, reg: Reg) -> Option<NodeId> {
    idx.inc(node_id).iter().find_map(|&ei| {
        let e = &dag.edges[ei as usize];
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
///
/// v0.104.6 D31：走 `idx.out(node_id)` 出边桶，O(出度) 而非 O(E)。
fn outgoing_data_edges<'a>(
    dag: &'a MirDag,
    idx: &DagIndex,
    node_id: NodeId,
) -> Vec<&'a MirDagEdge> {
    idx.out(node_id)
        .iter()
        .map(|&ei| &dag.edges[ei as usize])
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

    fn matches(&self, _node_id: NodeId, node: &MirDagNode, _dag: &MirDag, _idx: &DagIndex) -> bool {
        matches!(
            node,
            MirDagNode::Compute {
                inst: MirInst::BinaryOp(..),
                ..
            }
        )
    }

    fn rewrite(&self, node_id: NodeId, dag: &MirDag, idx: &DagIndex) -> Option<DagRewrite> {
        let node = dag.nodes.get(node_id)?;
        let (dst, lhs_reg, op, rhs_reg) = match node {
            MirDagNode::Compute {
                inst: MirInst::BinaryOp(d, l, o, r),
                ..
            } => (d, l, o, r),
            _ => return None,
        };

        // Find Const source nodes via Data edges
        let lhs_src = find_data_source(dag, idx, node_id, *lhs_reg)?;
        let rhs_src = find_data_source(dag, idx, node_id, *rhs_reg)?;

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
        if outgoing_data_edges(dag, idx, lhs_src).len() <= 1 {
            removed.push(lhs_src);
        }
        if outgoing_data_edges(dag, idx, rhs_src).len() <= 1 {
            removed.push(rhs_src);
        }

        Some(DagRewrite {
            added: vec![new_node],
            removed,
            added_edges: out_edges,
            reg_rename: None,
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

    fn matches(&self, _node_id: NodeId, node: &MirDagNode, _dag: &MirDag, _idx: &DagIndex) -> bool {
        matches!(node, MirDagNode::Compute { .. })
    }

    fn rewrite(&self, node_id: NodeId, dag: &MirDag, idx: &DagIndex) -> Option<DagRewrite> {
        // Don't remove exit nodes (they carry the function's result)
        if dag.exit.contains(&node_id) {
            return None;
        }
        // v0.104.6（**已知限制，未修**）：透明穿通保留了 removed 节点的边
        // （`dag.reachable` 的有效性依赖它们），故「有出边」这一判据会放过
        // 「消费方**全是 Removed**」的节点 —— 它们其实已死，却删不掉，死节点
        // 会逐轮堆积（每轮参与 `node_ready` 判定、在 `seq_preds` 里占位）。
        //
        // 试过把判据改成「有**存活**出边」，实测破坏 `jit_equiv_folded_constants`
        // 与 `if_as_value_not_starved_by_dead_branch` 两项 —— 沿穿通链继续
        // 承担激活传递的节点也被判死并摘掉，链条断裂。两者相较，此处**正确性
        // 优先**：保留旧判据，接受死节点堆积这一有界开销。
        // v0.104.6 D31：走 `idx.has_outgoing(node_id)`（出边桶是否为空），
        // O(1) 而非扫全部 E 条边。
        let has_outgoing = idx.has_outgoing(node_id);
        if has_outgoing {
            return None;
        }
        // v0.75.33: 控制流入口保护 — 被任意控制边（Control/ControlIfTrue/
        // ControlIfFalse）target 引用的节点即使无数据出边也**不能删**：
        // dag_search 删节点只清边、不修补引用者的 target 指针，删除后
        // target 悬垂 → 执行器跳进 Removed 死路。循环退出目标（for 循环后
        // 的 Const 占位）是典型：被 JumpIf true_target 引用、无出边。
        if is_control_target(node_id, dag, idx) {
            return None;
        }
        // v0.75.33: 活跃 use 保护 — 节点的 dst reg 若被其他存活节点作为
        // input 消费，删除会让消费者 reg 永无生产者（node_ready 恒 false，
        // 执行卡死）。典型：`Const(7, Nil)` 是 `Assign("__let_result", 7)`
        // 的输入 def，无出边但被 use → DCE 误删导致循环后执行卡死。
        let dst = match &dag.nodes[node_id] {
            MirDagNode::Compute { dst, .. } => *dst,
            _ => return None,
        };
        let has_live_use = dag.nodes.iter().enumerate().any(|(i, n)| {
            i != node_id
                && !n.is_removed()
                && match n {
                    MirDagNode::Compute { input_regs, .. } => input_regs.contains(&dst),
                    MirDagNode::Effect { inst } => inst.input_regs().contains(&dst),
                    _ => false,
                }
        });
        if has_live_use {
            return None;
        }

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

// ─── Rule 3: Common Subexpression Elimination on DAG ────────────────

/// Eliminates duplicate Compute nodes that have the same operation
/// and the same data sources.
pub struct CseDagRule;

impl DagRewriteRule for CseDagRule {
    fn name(&self) -> &'static str {
        "dag_cse"
    }

    fn matches(&self, _node_id: NodeId, node: &MirDagNode, _dag: &MirDag, _idx: &DagIndex) -> bool {
        // Any pure Compute node is a candidate
        matches!(node, MirDagNode::Compute { .. })
    }

    fn rewrite(&self, node_id: NodeId, dag: &MirDag, idx: &DagIndex) -> Option<DagRewrite> {
        let (dst_b, _) = match &dag.nodes[node_id] {
            MirDagNode::Compute { dst, .. } => (*dst, ()),
            _ => return None,
        };
        let node = dag.nodes.get(node_id)?;
        let _node_inst = match node {
            MirDagNode::Compute { inst, .. } => inst,
            _ => return None,
        };

        // v0.75.33: 控制流入口保护 — 被 Branch/Jump target 引用的节点不参与
        // CSE 合并（合并=删除 + 重定向出边，但不修补入边指针 → target 悬垂）。
        // 循环退出目标（for 后的 Const 占位）是典型受害者。
        if is_control_target(node_id, dag, idx) {
            return None;
        }

        // Scan prior nodes for an equivalent one
        for prev_id in 0..node_id {
            if prev_id == node_id {
                break;
            }
            if dag.nodes[prev_id].is_removed() {
                continue;
            }

            if nodes_equivalent(&dag.nodes[prev_id], node, prev_id, node_id, dag, idx) {
                // Found equivalent — redirect outgoing edges from node_id to prev_id
                // v0.75.33: 合并不同 dst 的节点必须重命名 — dag_interp 按
                // input_regs（寄存器号）取数，不按 Data 边；只重定向边会让
                // dst_b 失去 producer。reg_rename 由 apply_rewrite 全局改写
                // 消费者的 input_regs（Compute/Effect/Branch/Phi + Data 边）。
                let dst_a = match &dag.nodes[prev_id] {
                    MirDagNode::Compute { dst, .. } => *dst,
                    _ => unreachable!("nodes_equivalent only matches Compute"),
                };

                // v0.103: 重命名的**健全性前置条件** —— 见 [`is_multi_defined`]。
                // 两个寄存器都必须是单定义（SSA），否则全局改名会把「读某个
                // 程序点的值」错误地改成「读另一个程序点的值」。环携带寄存器
                // （for/while 的索引：init 与增量写同一寄存器）与跨分支同值
                // 常量（只有一边执行）都会命中。
                if is_multi_defined(dag, dst_b) || is_multi_defined(dag, dst_a) {
                    return None;
                }

                // v0.104.6 D9：胜者必须在败者执行时**一定已执行**（支配关系）。
                // 否则 `reg_rename` 会把败者的消费者改写成一个「只在该胜者所在
                // 控制区域被选中时才写该寄存器」的 producer → 消费者永久
                // not-ready → 前沿饿死 → 程序静默结束。
                //
                // **触发实例（D9：`while` + `continue` 返回 0，应 4）**：
                // ```mora
                // let n = 0i
                // let k = 0i
                // while k < 5i
                //   if k == 2i then
                //     assign k = k + 1i
                //     continue
                //   end
                //   assign n = n + 1i     -- 需要 if 块里的 Const(1)
                //   assign k = k + 1i
                // end
                // ```
                // if 块内的 `Const(1)`（`k = k + 1`）与 if 之后的
                // `n = n + 1` 所用的 `Const(1)` 同值 → 被 CSE 合并。两者
                // 寄存器都是单定义，`is_control_target` / `is_multi_defined`
                // 三道守卫全部放行。但两块**互斥**：k != 2 时 if 块不执行，
                // 其 `Const(1)` 永不写寄存器 → `n = n + 1` 永久 not-ready
                // → 循环体尾部永不执行 → 循环无法推进 → 静默结束。
                //
                // 判据取「同一基本块」：Sequence 边只在基本块内创建，故沿
                // Sequence 边可达 ≡ 同块；同块节点同生共死，合并必然安全。
                // 这是**充分**条件（支配但不同块的节点仍会被保守地放弃合并），
                // 代价只是少做一些本可做的优化。
                if !seq_reachable(dag, idx, prev_id, node_id) {
                    return None;
                }

                // v0.104.6：**胜者必须与败者一样可达**。
                //
                // 上面那道「同基本块」判据挡不住「死块里的等价节点」这一类：
                // Sequence 边会从死块跨进活块（实例：`let x = if 1 == 1 then 5 end`
                // 里，常量折叠删掉 `JumpIfNot` 后 else 臂成为死块，其
                // `Const(10, Nil)` → `Copy(4,10)` → `Define(x,4)` 仍与
                // 后续活代码同处一条 Sequence 链），于是「同块」成立、合并放行。
                //
                // 但**死块里的节点从不写它的寄存器**：把败者消费者改写成读胜者
                // 的寄存器后，那个寄存器永远 not-ready → 消费者永不执行 →
                // 其后整条尾部（`print`）静默消失。
                // 实测：`Assign("__let_result", 5)` 被改名成读 reg 10（死 else
                // 臂的 `Const(10, Nil)`）→ 程序无任何输出、退出码 0。
                //
                // 判据：败者可达时，胜者也必须可达（死块里的节点只配与死块
                // 合并，而那没有收益）。
                if dag.reachable.get(node_id).copied().unwrap_or(false)
                    && !dag.reachable.get(prev_id).copied().unwrap_or(false)
                {
                    return None;
                }

                // v0.103: **不重定向 Sequence 出边** —— Sequence 是「基本块内
                // 相邻指令的保序边」，位置语义而非数据语义。目标节点
                // （`prev_id`）可能与被删节点不同块：把它当普通出边重定向会把
                // 「另一个控制区域的节点」接到当前块的保序链上（DAG 执行器
                // 沿 Sequence 边激活消费者）→ 该节点被提前激活执行。
                //
                // **缺陷背景（`while ... if ... break ... end` 死循环）**：内层
                // if 块的同值 `Const` 被 CSE 合并后，`Const(pc19) → BinaryOp(pc20)`
                // 的块内 Sequence 边被重定向成 `Const(pc10) → BinaryOp(pc20)`，
                // 从 if 块跨越到循环体内块。于是 `break` 分支被选中时递增语句
                // 仍被激活执行，循环回边再次点火 → break 架空、挂死。
                //
                // 线性链由 `apply_rewrite` 的 Sequence 缝合（块内 pred→succ）
                // 恢复，无需在此重定向。
                let out_edges: Vec<(NodeId, NodeId, EdgeKind)> = dag
                    .edges
                    .iter()
                    .filter(|e| e.from == node_id && !matches!(e.kind, EdgeKind::Sequence))
                    .map(|e| (prev_id, e.to, e.kind.clone()))
                    .collect();

                return Some(DagRewrite {
                    added: vec![],
                    removed: vec![node_id],
                    added_edges: out_edges,
                    reg_rename: Some((dst_b, dst_a)),
                });
            }
        }
        None
    }

    fn cost_gain(&self) -> i32 {
        2
    }
}

/// v0.103: 寄存器 `reg` 在 DAG 中是否被**多于一个**节点定义（非 SSA）。
///
/// **缺陷背景（CSE 重命名的健全性条件）**：CSE 合并两个等价节点后，用
/// [`DagRewrite::reg_rename`] 把「读 `old_reg`」的**全部**位点全局改写成
/// 「读 `new_reg`」。这一改写等价于断言：
///
/// > 在每一个读 `old_reg` 的程序点，`old_reg` 的值都等于被删节点的输出。
///
/// 只有当 `old_reg` **只有一处定义**（SSA）时该断言成立 —— 那时任何读
/// `old_reg` 都读的是被删节点写的那个值。若 `old_reg` 还有别的定义，则
/// 不同程序点读到的是不同值，全局改名把语义改错了。`new_reg` 同理：
/// 多定义会让改名后的读点读到另一个定义的值。
///
/// 触发实例（v0.103 `for` 循环值传递缺陷，根因之一）：
/// ```mora
/// let t = 0i              -- Const(r0, Int(0))
/// for x in [1i, 2i, 3i]   -- 索引 init: Const(r7, Int(0))，增量: BinaryOp(r7, r7, Add, r_one)
///   let total = total + x
/// end
/// ```
/// 索引 `r7` 有两处定义（init + 增量），而 init 与 `t` 的初始化是两个
/// **同值常量** → CSE 把 `Const(r7, Int(0))` 并进 `Const(r0, Int(0))`，
/// 再把读 `r7` 的循环条件改写成读 `r0`，但增量仍写 `r7` → 条件恒为
/// `0 >= len`（false）→ **死循环**（`for_loop.mora` 由「返回 0」退化为
/// 无限输出）。`let t = 1i` 时两个常量不同值 → 不合并 → 侥幸正确，正是
/// 「同值才碰撞」的特征。
fn is_multi_defined(dag: &MirDag, reg: Reg) -> bool {
    let mut defs = 0usize;
    for node in &dag.nodes {
        if let MirDagNode::Compute { dst, .. } = node
            && *dst == reg
        {
            defs += 1;
            if defs > 1 {
                return true;
            }
        }
    }
    false
}

/// v0.104.6 D9：节点 `to` 是否可从 `from` 沿**仅 Sequence 边**到达。
///
/// 用途：CSE 合并的支配性判据。Sequence 边是「基本块内相邻指令的保序边」，
/// 只在块内创建（见 `dag_analyze` 的 Sequence 构造与 `prune_sequence_edges`
/// 的「全保留」约定），故「沿 Sequence 边可达」≡「同一基本块」≡ 同生共死。
/// 跨块（互斥控制区域）的两个节点不可合并 —— 见 `CseDagRule::rewrite` 中
/// D9 的触发实例。
/// v0.104.6 D31：走 `idx.out(n)` 出边桶，O(Σ度) 而非 O(V·E)。
/// 桶内按边下标升序，与旧的全表扫描同序，故可达性判定逐字等价。
fn seq_reachable(dag: &MirDag, idx: &DagIndex, from: NodeId, to: NodeId) -> bool {
    if from == to {
        return true;
    }
    let mut seen: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
    let mut stack = vec![from];
    seen.insert(from);
    while let Some(n) = stack.pop() {
        for &ei in idx.out(n) {
            let e = &dag.edges[ei as usize];
            if !matches!(e.kind, EdgeKind::Sequence) {
                continue;
            }
            if e.to == to {
                return true;
            }
            if seen.insert(e.to) {
                stack.push(e.to);
            }
        }
    }
    false
}

/// Check if two Compute nodes are structurally equivalent:
/// same instruction type + same data sources.
/// v0.75.33: 控制流入口判定 — 该节点是否被任意控制边（Control/
/// ControlIfTrue/ControlIfFalse）作为 target 引用。被引用的节点是控制流
/// 入口：删除会导致引用者的 target 指针悬垂（dag_search 删节点不清指针）。
fn is_control_target(node_id: NodeId, dag: &MirDag, idx: &DagIndex) -> bool {
    idx.inc(node_id).iter().any(|&ei| {
        let e = &dag.edges[ei as usize];
        e.to == node_id
            && matches!(
                e.kind,
                crate::mir::dag::EdgeKind::Control
                    | crate::mir::dag::EdgeKind::ControlIfTrue
                    | crate::mir::dag::EdgeKind::ControlIfFalse
            )
    })
}

fn nodes_equivalent(
    a: &MirDagNode,
    b: &MirDagNode,
    a_id: NodeId,
    b_id: NodeId,
    dag: &MirDag,
    idx: &DagIndex,
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
        let src_a = find_data_source(dag, idx, a_id, reg_a);
        let src_b = find_data_source(dag, idx, b_id, reg_b);
        if src_a != src_b {
            return false;
        }
    }

    true
}

/// Check if two instructions are in the same "category" for CSE purposes.
/// v0.87: Call 和 MethodCall 不在此列表中 — 它们不是可证明纯的（gensym/print/eval 等
/// 都有副作用），对同名零参数调用做 CSE 会把多次调用合并为一次（v0.87 gensym 全部
/// 返回 g0 的根因）。is_memoizable_pure 在 vm/dag.rs 有相同的保守白名单策略。
fn same_inst_category(a: &MirInst, b: &MirInst) -> bool {
    use crate::mir::MirInst;
    match (a, b) {
        (MirInst::Const(_, v1), MirInst::Const(_, v2)) => v1 == v2,
        (MirInst::BinaryOp(_, _, op1, _), MirInst::BinaryOp(_, _, op2, _)) => op1 == op2,
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

    fn matches(&self, _node_id: NodeId, node: &MirDagNode, _dag: &MirDag, _idx: &DagIndex) -> bool {
        matches!(
            node,
            MirDagNode::Compute {
                inst: MirInst::BinaryOp(..),
                ..
            }
        )
    }

    fn rewrite(&self, node_id: NodeId, dag: &MirDag, idx: &DagIndex) -> Option<DagRewrite> {
        let node = dag.nodes.get(node_id)?;
        let (dst, lhs_reg, op, rhs_reg) = match node {
            MirDagNode::Compute {
                inst: MirInst::BinaryOp(d, l, o, r),
                ..
            } => (d, l, o, r),
            _ => return None,
        };

        let lhs_src = find_data_source(dag, idx, node_id, *lhs_reg);
        let rhs_src = find_data_source(dag, idx, node_id, *rhs_reg);

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
                reg_rename: None,
            }),
            ReplaceWith::ReplaceWithSource(reg, Some(src_id)) => {
                // v0.104.6 D302：**本分支已停用**（原本就在产出错误代码）。
                //
                // 它是 `MirInst::Copy` 时代的遗留物 —— 而 `Copy` 在 v0.55 已删除
                // （见 `optimize/rule.rs` 的 `DeadAssignRule` 注释）。该分支
                // `added: vec![]`：**不添加任何节点**，只把原节点标 `Removed`
                // 并把出边改指到源节点。于是没有任何指令写 `dst`，而消费者
                // （`reg_rename: None` ⇒ 读寄存器没变）仍在读 `dst`。
                //
                // 后果（实测，默认档 `OptLevel::None`）：
                // ```mora
                // print("A")
                // let a = 100i + 0i
                // print(a)
                // print("B")
                // ```
                // 只输出 `A`；`a` 与 `B` 两条语句**静默不执行**，
                // 退出码 **0**、零诊断 —— 即 D35「静默饿死」那一族。
                //
                // 修法不是把边的 `reg` 改成 `dst`（**已试，无效**：执行器要的是
                // 一个 `dst` 匹配的**节点**，不是一条声称携带 dst 的边），
                // 而是让它**别产出错误代码**。要恢复这条优化需要重新引入
                // 「把源的值搬进 dst」的机制（恢复 `Copy` 或加等价节点）——
                // 那是**架构决定**，未擅自实施，已上报。
                //
                // 停用的代价：恒等运算不再被化简（`x+0` 保留 `BinaryOp`）。
                // 语义上完全等价（执行器照常算出 `x`），只少了这一处优化；
                // 两个字面量的情形仍由 MIR 层 `ConstFoldingRule` 折叠。
                let _ = (reg, src_id);
                return None;
                #[allow(unreachable_code)]
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
                    reg_rename: None,
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

            ..Default::default()
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
        // v0.104.6 D31：规则回调的邻接查询走索引（见 `DagIndex` 文档）
        let idx = DagIndex::build(&dag);
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
        let rw = rule
            .rewrite(binop_id, &dag, &idx)
            .expect("should fold constants");
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
        // v0.104.6 D31：规则回调的邻接查询走索引（见 `DagIndex` 文档）
        let idx = DagIndex::build(&dag);
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
            rule.rewrite(binop_id, &dag, &idx).is_none(),
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
        // v0.104.6 D31：规则回调的邻接查询走索引（见 `DagIndex` 文档）
        let idx = DagIndex::build(&dag);
        // BinaryOp at r3 (node with dst=3) should be eliminated
        let dup_id = dag
            .nodes
            .iter()
            .position(|n| matches!(n, MirDagNode::Compute { dst: 3, .. }))
            .unwrap();
        let rule = CseDagRule;
        let rw = rule
            .rewrite(dup_id, &dag, &idx)
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
        // v0.104.6 D31：规则回调的邻接查询走索引（见 `DagIndex` 文档）
        let idx = DagIndex::build(&dag);
        let dup_id = dag
            .nodes
            .iter()
            .position(|n| matches!(n, MirDagNode::Compute { dst: 3, .. }))
            .unwrap();
        let rule = CseDagRule;
        assert!(
            rule.rewrite(dup_id, &dag, &idx).is_none(),
            "different ops should not be eliminated"
        );
    }

    /// v0.103 回归：CSE 不得重命名**多定义**（非 SSA）寄存器。
    ///
    /// 缺陷形状（`for` 循环值传递，`for_loop.mora` 返回 0 / 死循环的根因）：
    /// 循环索引寄存器有两处定义 —— init `Const(7, Int(0))` 与增量
    /// `BinaryOp(7, 7, Add, one)`。当 init 与另一个语句的初始化恰好是
    /// **同值常量**（`let t = 0i` 的 `Const(0, Int(0))`）时，CSE 判定两者
    /// 等价 → 删除 init → 把「读 r7」全局改名成「读 r0」。但增量仍写 r7，
    /// 于是循环条件永远读 r0（不变的 0）→ `0 >= len` 恒假 → 死循环。
    ///
    /// 修复：`rewrite` 在产生 `reg_rename` 之前要求 dst_a/dst_b 均为单定义。
    /// `let t = 1i` 时两常量不同值 → 不合并 → 侥幸正确，正是「同值才碰撞」
    /// 的特征（测试用 Int(0)/Int(0) 复现碰撞）。
    #[test]
    fn cse_rejects_rename_of_multi_defined_loop_index() {
        let dag = make_dag(vec![
            MirInst::Const(0, Value::Int(0)), // let t = 0i
            MirInst::Define("t".to_string(), 0),
            MirInst::Const(7, Value::Int(0)), // loop idx init（与 t 同值 → CSE 候选）
            MirInst::Const(9, Value::Int(1)), // increment step
            MirInst::BinaryOp(10, 7, BinaryOp::GreaterEqual, 9), // cond
            // 增量：**第二次**定义 r7 → r7 非 SSA
            MirInst::BinaryOp(7, 7, BinaryOp::Add, 9),
        ]);
        // v0.104.6 D31：规则回调的邻接查询走索引（见 `DagIndex` 文档）
        let idx = DagIndex::build(&dag);
        let idx_init = dag
            .nodes
            .iter()
            .position(|n| matches!(n, MirDagNode::Compute { dst: 7, .. }))
            .expect("index init node");
        let rule = CseDagRule;
        assert!(
            rule.rewrite(idx_init, &dag, &idx).is_none(),
            "多定义寄存器（循环索引）不得被 CSE 重命名 — 否则条件读到 init 值、\
             增量写到另一个寄存器，循环永不终止"
        );
    }

    /// 反向断言：单定义（SSA）寄存器的重复计算**仍应**被 CSE 消除 ——
    /// 上一条的健全性守卫不能把合法优化一并禁掉。
    #[test]
    fn cse_still_eliminates_single_defined_duplicates() {
        let dag = make_dag(vec![
            MirInst::Const(0, Value::Int(10)),
            MirInst::Const(1, Value::Int(20)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
            MirInst::BinaryOp(3, 0, BinaryOp::Add, 1), // r3 单定义 → 可消除
        ]);
        // v0.104.6 D31：规则回调的邻接查询走索引（见 `DagIndex` 文档）
        let idx = DagIndex::build(&dag);
        let dup_id = dag
            .nodes
            .iter()
            .position(|n| matches!(n, MirDagNode::Compute { dst: 3, .. }))
            .unwrap();
        assert!(
            CseDagRule.rewrite(dup_id, &dag, &idx).is_some(),
            "单定义寄存器的等价节点仍必须被 CSE 消除"
        );
    }

    #[test]
    fn algebraic_x_plus_zero_is_refused_not_applied() {
        // v0.104.6 D302：本条**原先断言规则会触发**（`x+0 → x`），现已反转为
        // 「必须**拒绝**改写」。
        //
        // 原因：`ReplaceWithSource` 分支是 `MirInst::Copy` 的遗留物（`Copy`
        // 在 v0.55 已删）。它 `added: vec![]` —— 不添加任何节点，只把原节点
        // 标 `Removed` 并把出边改指到源节点。于是没有任何指令写 `dst`，
        // 消费者（`reg_rename: None` ⇒ 读寄存器没变）永远等不到 `dst`。
        //
        // 端到端后果（默认档 `OptLevel::None`）：
        //     print("A") / let a = 100i + 0i / print(a) / print("B")
        // 只输出 `A` —— 后两条语句静默不执行，退出码 0、零诊断。
        // 端到端判据：`tests/identity_op_silently_drops_statements.rs`。
        let dag = make_dag(vec![
            MirInst::Var(0, "x".to_string()),
            MirInst::Const(1, Value::Int(0)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
        ]);
        let idx = DagIndex::build(&dag);
        let binop_id = dag
            .nodes
            .iter()
            .position(|n| matches!(n, MirDagNode::Compute { dst: 2, .. }))
            .unwrap();
        let rule = AlgebraicSimplifyDagRule;
        assert!(
            rule.rewrite(binop_id, &dag, &idx).is_none(),
            "`x + 0` 的改写会打断 dst 的数据依赖（D302）—— 该分支必须保持停用，\
             直到重新引入「把源的值搬进 dst」的机制"
        );
    }

    #[test]
    fn algebraic_x_times_one_is_refused_not_applied() {
        // 同上：`x * 1` 与 `x + 0` 走的是同一条 `ReplaceWithSource` 分支。
        // 对照 `algebraic_x_times_zero`：`x * 0` 走 `ReplaceWithConst`
        // （会真的加一个写 dst 的 `Const` 节点）—— 那一支是安全的，仍在生效。
        let dag = make_dag(vec![
            MirInst::Var(0, "x".to_string()),
            MirInst::Const(1, Value::Int(1)),
            MirInst::BinaryOp(2, 0, BinaryOp::Mul, 1),
        ]);
        let idx = DagIndex::build(&dag);
        let binop_id = dag
            .nodes
            .iter()
            .position(|n| matches!(n, MirDagNode::Compute { dst: 2, .. }))
            .unwrap();
        let rule = AlgebraicSimplifyDagRule;
        assert!(
            rule.rewrite(binop_id, &dag, &idx).is_none(),
            "`x * 1` 的改写会打断 dst 的数据依赖（D302）—— 该分支必须保持停用"
        );
    }

    #[test]
    fn algebraic_x_times_zero() {
        let dag = make_dag(vec![
            MirInst::Var(0, "x".to_string()),
            MirInst::Const(1, Value::Int(0)),
            MirInst::BinaryOp(2, 0, BinaryOp::Mul, 1),
        ]);
        // v0.104.6 D31：规则回调的邻接查询走索引（见 `DagIndex` 文档）
        let idx = DagIndex::build(&dag);
        let binop_id = dag
            .nodes
            .iter()
            .position(|n| matches!(n, MirDagNode::Compute { dst: 2, .. }))
            .unwrap();
        let rule = AlgebraicSimplifyDagRule;
        let rw = rule
            .rewrite(binop_id, &dag, &idx)
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
