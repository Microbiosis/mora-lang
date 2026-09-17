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
        // v0.75.33: 控制流入口保护 — 被任意控制边（Control/ControlIfTrue/
        // ControlIfFalse）target 引用的节点即使无数据出边也**不能删**：
        // dag_search 删节点只清边、不修补引用者的 target 指针，删除后
        // target 悬垂 → 执行器跳进 Removed 死路。循环退出目标（for 循环后
        // 的 Const 占位）是典型：被 JumpIf true_target 引用、无出边。
        if is_control_target(node_id, dag) {
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

    fn matches(&self, _node_id: NodeId, node: &MirDagNode, _dag: &MirDag) -> bool {
        // Any pure Compute node is a candidate
        matches!(node, MirDagNode::Compute { .. })
    }

    fn rewrite(&self, node_id: NodeId, dag: &MirDag) -> Option<DagRewrite> {
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
        if is_control_target(node_id, dag) {
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

            if nodes_equivalent(&dag.nodes[prev_id], node, prev_id, node_id, dag) {
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
                    .filter(|e| {
                        e.from == node_id && !matches!(e.kind, EdgeKind::Sequence)
                    })
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

/// Check if two Compute nodes are structurally equivalent:
/// same instruction type + same data sources.
/// v0.75.33: 控制流入口判定 — 该节点是否被任意控制边（Control/
/// ControlIfTrue/ControlIfFalse）作为 target 引用。被引用的节点是控制流
/// 入口：删除会导致引用者的 target 指针悬垂（dag_search 删节点不清指针）。
fn is_control_target(node_id: NodeId, dag: &MirDag) -> bool {
    dag.edges.iter().any(|e| {
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
                reg_rename: None,
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
        
            ..Default::default()};
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
        let idx_init = dag
            .nodes
            .iter()
            .position(|n| matches!(n, MirDagNode::Compute { dst: 7, .. }))
            .expect("index init node");
        let rule = CseDagRule;
        assert!(
            rule.rewrite(idx_init, &dag).is_none(),
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
        let dup_id = dag
            .nodes
            .iter()
            .position(|n| matches!(n, MirDagNode::Compute { dst: 3, .. }))
            .unwrap();
        assert!(
            CseDagRule.rewrite(dup_id, &dag).is_some(),
            "单定义寄存器的等价节点仍必须被 CSE 消除"
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
