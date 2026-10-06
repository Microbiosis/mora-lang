//! v0.59: DAG IR — dataflow graph analysis from linear MIR.
//!
//! Phase D1: Static analysis pass that constructs a `MirDag` from a
//! `MirFunction.body` (flat `Vec<MirInst>`). The DAG makes implicit
//! register-level data dependencies explicit as graph edges, enabling:
//!
//! - Topological ordering (expose instruction-level parallelism)
//! - Dataflow-aware optimization (CSE, LICM on DAG nodes)
//! - DAG-based execution (Phase D2: `run_mir_dag`)
//!
//! Design: additive, not replacement. `MirFunction.body` remains the
//! canonical linear form; `MirDag` is an analysis artifact.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::mir::{Label, MirFunction, MirInst, Reg};

/// Unique identifier for a DAG node.
pub type NodeId = usize;

/// DAG representation of a `MirFunction`.
#[derive(Debug, Clone)]
pub struct MirDag {
    /// All nodes in the DAG.
    pub nodes: Vec<MirDagNode>,
    /// All edges in the DAG.
    pub edges: Vec<MirDagEdge>,
    /// Nodes with no incoming edges (execution starts here).
    pub entry: Vec<NodeId>,
    /// Nodes with no outgoing edges (final result).
    pub exit: Vec<NodeId>,
    /// 节点是否**从程序入口可达**（`dag_analyze` 计算，优化阶段保持有效）。
    ///
    /// v0.104.6 D9：不可达 = 死块，不参与执行（entry 过滤、执行器的
    /// `seq_preds` 就绪门槛都据此排除它）。优化改写入口后由
    /// [`MirDag::recompute_reachable_from_entry`] 按新入口重算。
    pub reachable: Vec<bool>,
    /// Number of virtual registers (from MirFunction.n_regs).
    pub n_regs: usize,
}

/// One vertex in the dataflow graph.
#[derive(Debug, Clone)]
pub enum MirDagNode {
    /// Pure computation producing a register value.
    /// e.g. Const, BinaryOp, Call, ListLit, etc.
    Compute {
        /// The original MIR instruction.
        inst: MirInst,
        /// Destination register written by this instruction.
        dst: Reg,
        /// Input registers read by this instruction.
        input_regs: Vec<Reg>,
    },
    /// Side-effecting operation (Define, Assign, I/O, etc.).
    /// Must be executed in order — creates Sequence edges.
    Effect { inst: MirInst },
    /// Control-flow branch point (JumpIf / JumpIfNot).
    Branch {
        cond: Reg,
        true_target: Option<NodeId>,
        false_target: Option<NodeId>,
    },
    /// Unconditional jump to another node.
    Jump { target: Option<NodeId> },
    /// Phi node at a basic block boundary (SSA concept, placeholder).
    Phi {
        reg: Reg,
        sources: Vec<(NodeId, Reg)>,
    },
    /// Placeholder for labels (mapping Label→NodeId).
    Label { label: Label },
    /// Tombstone for nodes removed during DAG optimization.
    Removed,
}

impl MirDagNode {
    pub fn is_removed(&self) -> bool {
        matches!(self, MirDagNode::Removed)
    }
}

/// A directed edge between two DAG nodes.
#[derive(Debug, Clone)]
pub struct MirDagEdge {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
}

/// What kind of dependency an edge represents.
#[derive(Debug, Clone, PartialEq)]
pub enum EdgeKind {
    /// register `reg` produced by `from`, consumed by `to`.
    Data { reg: Reg },
    /// Control flow: `from` unconditionally jumps to `to`.
    Control,
    /// Control flow: `from` conditionally jumps to `to` if truthy.
    ControlIfTrue,
    /// Control flow: `from` conditionally jumps to `to` if falsy.
    ControlIfFalse,
    /// Sequential dependency (side effects must follow order).
    Sequence,
    /// Back edge (loop).
    BackEdge,
}

// ─── Basic Block ────────────────────────────────────────────────────

/// A basic block in the linear MIR: a contiguous range in `body`
/// with a single entry (the first instruction) and a single exit
/// (the terminator).
#[derive(Debug, Clone)]
struct BasicBlock {
    /// Index of the first instruction in this block.
    start: usize,
    /// Index after the last instruction (exclusive).
    end: usize,
}

/// Partition `body` into basic blocks.
///
/// Block boundaries are drawn at:
/// - Label instructions (block entry)
/// - Jump/JumpIf/JumpIfNot/Return/Break/Continue (block terminator)
/// - Start of body (implicit entry)
fn partition_blocks(body: &[MirInst]) -> Vec<BasicBlock> {
    if body.is_empty() {
        return vec![];
    }

    // Step 1: find all block-start positions
    let mut starts: HashSet<usize> = HashSet::new();
    starts.insert(0); // implicit entry

    // Find all jump targets (Label positions)
    let mut label_to_pc: HashMap<Label, usize> = HashMap::new();
    for (pc, inst) in body.iter().enumerate() {
        if let MirInst::Label(l) = inst {
            starts.insert(pc);
            label_to_pc.insert(*l, pc);
        }
    }

    // Find all block starts after terminators
    for (pc, inst) in body.iter().enumerate() {
        match inst {
            MirInst::Jump(_)
            | MirInst::JumpIf(_, _)
            | MirInst::JumpIfNot(_, _)
            | MirInst::Return(_)
            | MirInst::Break(_)
            | MirInst::Continue(_)
                if pc + 1 < body.len() =>
            {
                starts.insert(pc + 1);
            }
            _ => {}
        }
    }

    // v0.104.6 E1 修复：**裸 pc 跳转目标也是块首**。
    //
    // `lower` / `fcfg_lower` 的控制转移用**裸 pc 数字**做目标（不插 Label），
    // 循环回边（`Jump(6)`）与 if/else 汇合点（`Jump(21)`）都是如此。Label
    // 扫描一个都发现不了它们，此前只有「终结符之后」这一条规则在切块。
    //
    // 后果不是「少切一块」这么轻：**跳转目标落在某个块的中间，于是该目标与
    // 它后面的整段尾部被并进前驱那一块**。随后 `dag_analyze` 的两处「块内
    // 顺序」机制都会越界：
    //
    //   * Step 1 的块内 Sequence 链 `prev → idx` 从前一块末尾**直连**跳转目标；
    //   * Step 3 的 `last_effect` 扇出（Effect 之后的每个节点都连一条
    //     Sequence）越过跳转目标**继续**连向尾部。
    //
    // 而执行器的就绪门槛是 `seq_preds[n].iter().all(|&p| executed[p])`
    // —— 它把「同一块内必然先执行」当成了「必经」。于是在 if/else 上，跳转目标
    // （汇合点）被**未被选中的那一臂**挡住。
    //
    // 实测（E1，本注释的起因）：
    // ```text
    // let c = 1
    // let x = if c == 1 then 5 else 7 end
    // print(x + 1)
    // ```
    // 汇合点 `Var("x")`（node 21）的 `seq_preds = [20, 19]`，而 19/20 全在
    // **else 臂**内（18,19,20 = `Const 7 / Assign x / Copy`）。`c == 1` 成立
    // 时 else 臂不执行 → `executed[19]`、`executed[20]` 恒 false → 汇合点及其
    // **整个尾部**（`print(x + 1)`，含 Step 3 从 19 扇出的 19→22/23/24）
    // 永不就绪 → 语句静默消失：**无报错、退出码 0**。
    //
    // 修正后 `[18,21)` 与 `[21,25)` 是两块，`last_effect` 在块边界重置，
    // 汇合点的 `seq_preds` 为空 —— 它只由控制边激活（then 路径来自 node 17 的
    // `Jump`，else 路径来自新补的 fall-through `Control`）。
    //
    // **为什么循环回边不会因此丢门控**：`Jump(6)` 让 header 6 成为块首，
    // `5 → 6` 从 Sequence 改为 Control（见 `dag_analyze` 的块边界 fall-through
    // 补边），但**循环体本身** `[8,14)` 仍是一整块 —— 其
    // `seq_preds[12] = [11, 9]`、`seq_preds[13] = [12, 9]` 一字未动。
    // 那两条正是让 `Index(list, i)` 不早于 `i = i + 1` 的承重约束。
    for inst in body.iter() {
        let target = match inst {
            MirInst::Jump(t) => Some(*t),
            MirInst::JumpIf(_, t) | MirInst::JumpIfNot(_, t) => Some(*t),
            _ => None,
        };
        if let Some(t) = target {
            // 与 `dag_analyze` Step 2 的解析顺序保持一致：Label 优先，裸 pc 兜底
            // （`Label = usize`，两者共用键空间）。
            let target_pc = label_to_pc.get(&t).copied().unwrap_or(t);
            if target_pc < body.len() {
                starts.insert(target_pc);
            }
        }
    }

    // Step 2: build blocks from sorted starts
    let mut sorted_starts: Vec<usize> = starts.into_iter().collect();
    sorted_starts.sort();

    let mut blocks: Vec<BasicBlock> = Vec::new();
    for i in 0..sorted_starts.len() {
        let start = sorted_starts[i];
        let end = if i + 1 < sorted_starts.len() {
            sorted_starts[i + 1]
        } else {
            body.len()
        };

        blocks.push(BasicBlock { start, end });
    }

    blocks
}

// ─── DAG Construction ────────────────────────────────────────────────

/// 该指令是否终结基本块 —— 即它之后的指令只能经由显式跳转到达。
///
/// 用于判断「前块末尾 → 下一块首」是否需要补 fall-through 控制边：
/// 以终结符收尾的块，其后继由 handler（`Jump`/`Branch`）或根本不发生
/// （`Return`/`Break`/`Continue`）决定，**不能**再补一条无条件边。
fn is_block_terminator(inst: &MirInst) -> bool {
    matches!(
        inst,
        MirInst::Jump(_)
            | MirInst::JumpIf(_, _)
            | MirInst::JumpIfNot(_, _)
            | MirInst::Return(_)
            | MirInst::Break(_)
            | MirInst::Continue(_)
    )
}

/// Entry point: analyze a `MirFunction` and construct its `MirDag`.
pub fn dag_analyze(func: &MirFunction) -> MirDag {
    let body = &func.body;
    let blocks = partition_blocks(body);

    let mut nodes: Vec<MirDagNode> = Vec::new();
    let mut edges: Vec<MirDagEdge> = Vec::new();
    // Maps (pc) -> NodeId for quick lookup
    let mut pc_to_node: HashMap<usize, NodeId> = HashMap::new();
    // Maps Label -> NodeId for control flow edges
    let mut label_to_node: HashMap<Label, NodeId> = HashMap::new();
    // Maps block index -> entry NodeId
    let mut block_entry: HashMap<usize, NodeId> = HashMap::new();

    // v0.104.6 E1 修复：块边界 fall-through 补边所需的块端信息
    // （最后一个非 Label 节点、该块是否以终结符收尾）。
    //
    // `partition_blocks` 现在把**裸 pc 跳转目标**也当块首，于是「前一块的
    // 末尾 → 跳转目标」不再由 Step 1 的块内 Sequence 链覆盖（`prev_node`
    // 按块重置）。必须显式补一条控制边，否则 else 路径下汇合点**永不被激活**。
    //
    // 补边条件：前块**不以终结符收尾**。以 `Jump` 收尾时其 handler 已经推
    // target，以 `JumpIf/JumpIfNot` 收尾时 Step 2 已补 `ControlIfFalse`
    // fall-through，`Return/Break/Continue` 则根本不 fall through —— 这几种
    // 再补一条会把两个后继一起激活（正是 v0.103 修掉的 exit/body 竞态）。
    let mut blk_tails: Vec<(Option<NodeId>, bool)> = Vec::with_capacity(blocks.len());
    let mut blk_heads: Vec<Option<NodeId>> = Vec::with_capacity(blocks.len());

    // Step 1: Create nodes for every instruction
    for blk in &blocks {
        let mut block_first_node: Option<NodeId> = None;
        let mut blk_head: Option<NodeId> = None;
        let mut blk_tail: (Option<NodeId>, bool) = (None, false);
        let mut prev_node: Option<NodeId> = None;

        for (pc, inst) in body.iter().enumerate().take(blk.end).skip(blk.start) {
            // Create the node (Branch cond is set to 0 temporarily, patched below)
            let node = if inst.is_effect() {
                MirDagNode::Effect { inst: inst.clone() }
            } else if matches!(inst, MirInst::Jump(_)) {
                MirDagNode::Jump { target: None }
            } else if matches!(inst, MirInst::JumpIf(_, _) | MirInst::JumpIfNot(_, _)) {
                MirDagNode::Branch {
                    cond: 0,
                    true_target: None,
                    false_target: None,
                }
            } else if let MirInst::Label(lbl) = inst {
                MirDagNode::Label { label: *lbl }
            } else if let Some(dst) = inst.dst() {
                MirDagNode::Compute {
                    inst: inst.clone(),
                    dst,
                    input_regs: inst.input_regs(),
                }
            } else {
                MirDagNode::Effect { inst: inst.clone() }
            };

            // Push the node first, then patch Branch cond in-place
            let idx = nodes.len();
            nodes.push(node);
            pc_to_node.insert(pc, idx);

            // Register labels for later control-flow edge resolution
            if let MirInst::Label(lbl) = inst {
                label_to_node.insert(*lbl, idx);
            }

            // Patch Branch nodes with the actual cond register (now mutable via nodes[idx])
            if matches!(inst, MirInst::JumpIf(_, _) | MirInst::JumpIfNot(_, _))
                && let MirDagNode::Branch { ref mut cond, .. } = nodes[idx]
            {
                match inst {
                    MirInst::JumpIf(c, _) | MirInst::JumpIfNot(c, _) => *cond = *c,
                    _ => {}
                }
            }

            if block_first_node.is_none() {
                block_first_node = Some(idx);
            }

            // v0.75.33: 基本块内全序 — 每个节点（含 Compute）与前一个节点
            // 连 Sequence，不只 Effect 对。Compute（Var/Const/Index）读 env
            // 或依赖前序语句的值，若只给 Effect 保序，Compute 会提前执行
            // 读脏值（`let total = 0` 后 `Var(total)` 抢跑读 Nil）。
            // 块间（控制转移处）不连 — 由 Branch/Jump handler 决定激活。
            if let Some(prev) = prev_node {
                edges.push(MirDagEdge {
                    from: prev,
                    to: idx,
                    kind: EdgeKind::Sequence,
                });
            }
            if !matches!(inst, MirInst::Label(_)) {
                prev_node = Some(idx);
                if blk_head.is_none() {
                    blk_head = Some(idx);
                }
                // v0.104.6 E1：记录块端信息（供块边界 fall-through 补边）。
                // 逐节点覆盖，块结束时自然停在「最后一个非 Label 节点」上；
                // Label 只是块入口标记，不参与执行。
                blk_tail = (Some(idx), is_block_terminator(inst));
            }
        }
        blk_heads.push(blk_head);
        blk_tails.push(blk_tail);

        if let Some(first) = block_first_node {
            block_entry.insert(blk.start, first);
        }
    }

    // v0.104.6 E1 修复：块边界 fall-through 补边（详见上方声明处）。
    //
    // 以前这条边由 Step 1 的 `prev_node` 链顺带产出，跨块时它是一条
    // **Sequence** 边 —— 于是「同一块内必然先执行」这个不变量在块边界上被
    // 悄悄违反（E1 的根因）。现在跨块统一发 `Control`：激活语义不变
    // （`should_push = is_control_edge || Sequence`），但不再进入
    // `seq_preds` 的就绪门槛。
    for (tail, head) in blk_tails.iter().zip(blk_heads.iter().skip(1)) {
        let last = match *tail {
            (Some(last), false) => last,
            _ => continue,
        };
        let next_head = match *head {
            Some(next_head) => next_head,
            None => continue,
        };
        // 同块的「伪边界」（理论上不会出现在 blocks 里）不加边。
        if next_head == last {
            continue;
        }
        edges.push(MirDagEdge {
            from: last,
            to: next_head,
            kind: EdgeKind::Control,
        });
    }

    // Step 2: Resolve control flow edges
    // Patch Jump targets
    // v0.75.33: 跳转目标解析 = Label 优先，裸 pc 兜底 — lower 的 for/while
    // 循环用 pc 数字做 Jump 目标（不插 Label）；此前只查 label_to_node 导致
    // 循环跳转全部 patch 失败（Jump{target:None} → 循环控制流断裂）。
    // 目标可能是 Label（SSA/手写）或 pc（循环 lowering）。
    for (pc, node_id) in &pc_to_node {
        let inst = &body[*pc];
        match inst {
            MirInst::Jump(target) => {
                let target_id = label_to_node
                    .get(target)
                    .or_else(|| pc_to_node.get(target))
                    .copied();
                if let Some(target_id) = target_id {
                    edges.push(MirDagEdge {
                        from: *node_id,
                        to: target_id,
                        kind: EdgeKind::Control,
                    });
                    // Patch the Jump node
                    if let MirDagNode::Jump { ref mut target } = nodes[*node_id] {
                        *target = Some(target_id);
                    }
                }
            }
            MirInst::JumpIf(_cond, target) => {
                let target_id = label_to_node
                    .get(target)
                    .or_else(|| pc_to_node.get(target))
                    .copied();
                if let Some(target_id) = target_id {
                    edges.push(MirDagEdge {
                        from: *node_id,
                        to: target_id,
                        kind: EdgeKind::ControlIfTrue,
                    });
                    if let MirDagNode::Branch {
                        ref mut true_target,
                        ..
                    } = nodes[*node_id]
                    {
                        *true_target = Some(target_id);
                    }
                }
                // Fall-through: either next instruction or next block
                let fall_through = pc + 1;
                if let Some(&fall_id) = pc_to_node.get(&fall_through) {
                    edges.push(MirDagEdge {
                        from: *node_id,
                        to: fall_id,
                        kind: EdgeKind::ControlIfFalse,
                    });
                    if let MirDagNode::Branch {
                        ref mut false_target,
                        ..
                    } = nodes[*node_id]
                    {
                        *false_target = Some(fall_id);
                    }
                }
            }
            MirInst::JumpIfNot(_cond, target) => {
                let target_id = label_to_node
                    .get(target)
                    .or_else(|| pc_to_node.get(target))
                    .copied();
                if let Some(target_id) = target_id {
                    edges.push(MirDagEdge {
                        from: *node_id,
                        to: target_id,
                        kind: EdgeKind::ControlIfFalse,
                    });
                    if let MirDagNode::Branch {
                        ref mut false_target,
                        ..
                    } = nodes[*node_id]
                    {
                        *false_target = Some(target_id);
                    }
                }
                let fall_through = pc + 1;
                if let Some(&fall_id) = pc_to_node.get(&fall_through) {
                    edges.push(MirDagEdge {
                        from: *node_id,
                        to: fall_id,
                        kind: EdgeKind::ControlIfTrue,
                    });
                    if let MirDagNode::Branch {
                        ref mut true_target,
                        ..
                    } = nodes[*node_id]
                    {
                        *true_target = Some(fall_id);
                    }
                }
            }
            // v0.103: Break/Continue 目标解析 = Label 优先，裸 pc 兜底 ——
            // 与上方 Jump/JumpIf 同一契约（v0.75.33 给 Jump 补了 pc 兜底，
            // 但 Break/Continue 漏了同一处）。
            //
            // **缺陷背景**：`fcfg_lower` 与 `emit_loop_w` 都不发 `MirInst::Label`，
            // 它们把 break/continue 的 label 后修补成**指令下标**（`end` /
            // `loop_start` 的 `ctx.insts.len()`）。因此 `label_to_node` 里没有
            // 这些键 → 只查 label_to_node 时 Break 节点拿不到任何出边 →
            // DAG 执行器无从激活退出目标。而 DAG 执行器对 Effect 节点返回的
            // `Flow::Jump(_)` 是 no-op（控制转移完全由边决定，见 run_dag 内的
            // 分支注释）→ `break` 被静默忽略 → 循环永不退出（挂死）。
            //
            // 实例：`while t < 2i / if t == 1i / break / end / ... end`
            // 顶层（非 task）挂死；这正是 DAG 边解析缺失而非运行时跳转问题。
            MirInst::Break(target) | MirInst::Continue(target) => {
                let target_id = label_to_node
                    .get(target)
                    .or_else(|| pc_to_node.get(target))
                    .copied();
                if let Some(target_id) = target_id {
                    edges.push(MirDagEdge {
                        from: *node_id,
                        to: target_id,
                        kind: EdgeKind::Control,
                    });
                }
            }
            _ => {}
        }
    }

    // Step 3: Create Data edges + Sequence edges (reaching definitions within each block)
    for blk in &blocks {
        // reg_deps[reg] = (node_id, pc) of the most recent definition within this block
        let mut reg_deps: HashMap<Reg, (NodeId, usize)> = HashMap::new();
        // Track the last effect node for sequential ordering
        let mut last_effect: Option<NodeId> = None;

        for (pc, inst) in body.iter().enumerate().take(blk.end).skip(blk.start) {
            let Some(&node_id) = pc_to_node.get(&pc) else {
                continue;
            };

            let node = &nodes[node_id];

            // For Compute AND Effect nodes that read registers (Define, Assign, etc.):
            // connect their input regs to definitions
            let node_input_regs = inst.input_regs();
            if !node_input_regs.is_empty() {
                for &input_reg in &node_input_regs {
                    if let Some(&(def_node, _def_pc)) = reg_deps.get(&input_reg) {
                        edges.push(MirDagEdge {
                            from: def_node,
                            to: node_id,
                            kind: EdgeKind::Data { reg: input_reg },
                        });
                    }
                }
            }

            // For Branch nodes: connect cond register
            if let MirDagNode::Branch { cond, .. } = node
                && let Some(&(def_node, _def_pc)) = reg_deps.get(cond)
            {
                edges.push(MirDagEdge {
                    from: def_node,
                    to: node_id,
                    kind: EdgeKind::Data { reg: *cond },
                });
            }

            // Register the definition from this node (if any)
            if let MirDagNode::Compute { dst, .. } = node {
                reg_deps.insert(*dst, (node_id, pc));
            }

            // Sequence edges: Effect nodes (Define, Assign, I/O) must
            // execute before subsequent Var/Call nodes that read from env.
            //
            // v0.104.6 D305（**已回滚的尝试，保留记录**）：曾删掉 `else` 那一支
            // 的 Effect 扇出，理由是「Step 1 的块内全序链已保证顺序、扇出冗余」。
            //
            // 实测**只对了一半**：
            //   ✅ 直线尾部确实好了 —— `let a = 5i` + N 条 print 的**阶梯式重复**
            //      （D303）消失；
            //   ❌ 但 **56 个真实程序的分叉从 6 涨到 8** —— 新增
            //      `loop_beyond_dag_limit` 与 `loop_break` 两个**循环**程序回归。
            //
            // ⇒ 这些边**不是冗余的**：循环体靠它们承重（与 v0.75.33 那条
            // 「放松它会得到 `run_mir: index 2 out of bounds (len 2)`」同源）。
            // 正确的修法必须**区分**「循环体（需要额外排序）」与「直线尾部（不需要）」，
            // 而这正是 `partition_blocks` / E1 一族已经在处理、且**前五次尝试
            // 全部翻车**的那个区分。故本轮只留此记录，不改代码。
            if matches!(nodes[node_id], MirDagNode::Effect { .. }) {
                // Chain from last effect to this one
                if let Some(prev) = last_effect {
                    edges.push(MirDagEdge {
                        from: prev,
                        to: node_id,
                        kind: EdgeKind::Sequence,
                    });
                }
                last_effect = Some(node_id);
            } else if let Some(prev) = last_effect {
                // Non-effect nodes after an Effect depend on it
                // (e.g., Var(name) must execute after Define(name, r))
                edges.push(MirDagEdge {
                    from: prev,
                    to: node_id,
                    kind: EdgeKind::Sequence,
                });
            }
        }
    }

    // Step 4: Compute entry/exit sets
    let mut has_incoming: HashSet<NodeId> = HashSet::new();
    let mut has_outgoing: HashSet<NodeId> = HashSet::new();
    for edge in &edges {
        has_incoming.insert(edge.to);
        has_outgoing.insert(edge.from);
    }

    // v0.104.2: entry = **从 pc 0 可达**的无入边节点，而不是「所有无入边节点」。
    //
    // **缺陷背景（`if true { print("p") }` 之后 7i 丢失、run_mir 返回 Nil）**：
    // 常量条件的 `JumpIfNot` 被 `IfSimplifyRule` 折叠掉后，else 臂（`Const(Nil)`
    // + `Copy(dst, …)`）成为**不可达死块**，它自然没有入边。此前 entry 取
    // 「所有无入边节点」→ 死块与函数入口**并列成为入口** → DAG 无条件下执行
    // 它 → 其 `Copy(dst, Nil)` 覆盖了真分支写好的结果，且 `n6 -> n7` 的
    // Sequence 让死块抢先于尾部语句 → 尾值被 Nil 取代（**静默返回 Nil**）。
    //
    // 判据：函数入口只有 pc 0（`partition_blocks` 恒把 0 放进 starts）。
    // 从它出发沿边做一次可达性遍历，只有可达且无入边的节点才是合法入口；
    // 其余无入边节点是优化留下的死块，不参与执行（它们仍留在 nodes 里，
    // 与 `MirDagNode::Removed` 的既有约定一致）。
    let exit: Vec<NodeId> = (0..nodes.len())
        .filter(|n| !has_outgoing.contains(n))
        .collect();

    // v0.104.6：Label 透明化 —— 给每个 Label 节点补一条到「其后第一个非 Label
    // 节点」的控制边。
    //
    // 建节点时 Sequence 链刻意跳过 Label（`prev_node` 不更新），于是 Label 自身
    // **没有任何出边**。而 Label 正是块入口标记 —— 跳转命中它之后控制流就断在
    // 那里，既到不了块内第一句，也让「从 pc 0 出发的可达集」断在此处。
    //
    // 实例（`--opt=1` 下的 `let x = 1 + 2 / return x`）：SSA 在 pc 0 插入
    // `Label(0)`，边表里没有 `0 -> 1` → 可达集 = `{0}`，入口只剩这个 no-op
    // → 程序什么都不执行 → **顶层结果静默变 Nil**
    // （`mir_ssa_roundtrip` 的 `top_level_*_equiv` 三项）。
    for (i, node) in nodes.iter().enumerate() {
        if !matches!(node, MirDagNode::Label { .. }) {
            continue;
        }
        if let Some(next) =
            (i + 1..nodes.len()).find(|&j| !matches!(nodes[j], MirDagNode::Label { .. }))
        {
            edges.push(MirDagEdge {
                from: i,
                to: next,
                kind: EdgeKind::Control,
            });
        }
    }

    // v0.104.6 D308：可达性必须在**边全部建完之后**才算。
    //
    // 缺陷背景：这段遍历原先位于本函数更靠前的位置，早于
    // ①「Label 透明化」边（给 Label 补到其后第一个非 Label 节点的 Control 边）
    // ② E1 修复的「块边界 fall-through Control 边」。
    // 而 SSA（`--opt=1` 及以上）会在函数体 pc 0 插入一个 `Label(0)` ——
    // 那个 Label 节点的出边**只**由 ① 提供 ⇒ 遍历跑到它时**一条出边都没有**
    // ⇒ `reachable = {0}`。
    //
    // 后果（实测，D307）：执行器 `seq_preds` 的构造带
    // `dag.reachable.get(e.from)` 过滤 ⇒ 节点 1 之后的所有节点被判「不可达」
    // ⇒ **所有链式 Sequence 边被丢弃** ⇒ 就绪门槛完全失效，只剩
    // `dag_analyze` Step 3 的 Effect 扇出在排序 ⇒ 直线代码里第 k 条语句的
    // 节点跑 k 遍（打印出 `A,B,C,B,C,C`），`match` / 闭包 / `for` 更是
    // 静默无输出。
    //
    // 讽刺之处：① 那段修复**自己就是为了修这个问题**（其注释逐字描述了
    // 「可达集 = {0} ⇒ 顶层结果静默变 Nil」），但它 push 的边对**更早跑的**
    // 那次遍历不可见 —— 与 D302 的 `ReplaceWithSource` 同形
    // （产生值的东西落在需要它之后才到位）。
    let true_entry: Option<NodeId> = pc_to_node.get(&0).copied();
    let mut reachable: HashSet<NodeId> = HashSet::new();
    if let Some(root) = true_entry {
        let mut stack = vec![root];
        reachable.insert(root);
        while let Some(n) = stack.pop() {
            for e in edges.iter().filter(|e| e.from == n) {
                if reachable.insert(e.to) {
                    stack.push(e.to);
                }
            }
        }
    }

    let n_nodes = nodes.len();
    let mut dag = MirDag {
        nodes,
        edges,
        entry: Vec::new(), // 由 recompute_entry() 统一填充
        exit,
        reachable: (0..n_nodes).map(|i| reachable.contains(&i)).collect(),
        n_regs: func.n_regs,
    };
    dag.recompute_entry();
    dag
}

// ─── Topological Sort ────────────────────────────────────────────────

/// Topological sort using Kahn's algorithm.
///
/// Returns `Vec<Vec<NodeId>>` where each inner vec is a level of
/// nodes that can execute in parallel (no dependencies between them).
/// Returns `None` if there is a cycle (should not happen in valid MIR).
pub fn topological_sort(dag: &MirDag) -> Option<Vec<Vec<NodeId>>> {
    let n = dag.nodes.len();
    let mut in_degree: Vec<usize> = vec![0; n];
    let mut adjacency: Vec<Vec<NodeId>> = vec![Vec::new(); n];

    // Build adjacency and in-degree
    for edge in &dag.edges {
        // Skip back edges for topological sort
        if edge.kind == EdgeKind::BackEdge {
            continue;
        }
        adjacency[edge.from].push(edge.to);
        in_degree[edge.to] += 1;
    }

    let mut queue: VecDeque<NodeId> = dag
        .entry
        .iter()
        .filter(|&&n| in_degree[n] == 0)
        .copied()
        .collect();

    let mut levels: Vec<Vec<NodeId>> = Vec::new();
    let mut visited: HashSet<NodeId> = HashSet::new();

    while !queue.is_empty() {
        let mut current_level: Vec<NodeId> = Vec::new();
        let level_size = queue.len();

        for _ in 0..level_size {
            let node = queue
                .pop_front()
                .expect("dag: queue drained within level_size iteration");
            current_level.push(node);
            visited.insert(node);

            for &succ in &adjacency[node] {
                if in_degree[succ] > 0 {
                    in_degree[succ] -= 1;
                }
                if in_degree[succ] == 0 && !visited.contains(&succ) {
                    queue.push_back(succ);
                }
            }
        }

        levels.push(current_level);
    }

    // Check for cycles: if we didn't reach all nodes, there's a cycle
    if visited.len() < n {
        return None;
    }

    Some(levels)
}

/// Return the nodes in a single topological order (flattened).
pub fn topological_order(dag: &MirDag) -> Option<Vec<NodeId>> {
    topological_sort(dag).map(|levels| levels.into_iter().flatten().collect())
}

// ─── Debug ──────────────────────────────────────────────────────────

impl std::fmt::Display for MirDag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "MirDag {{ n_nodes={}, n_edges={}, n_regs={} }}",
            self.nodes.len(),
            self.edges.len(),
            self.n_regs
        )?;
        writeln!(f, "  entry: {:?}", self.entry)?;
        writeln!(f, "  exit: {:?}", self.exit)?;
        writeln!(f, "  nodes:")?;
        for (i, node) in self.nodes.iter().enumerate() {
            match node {
                MirDagNode::Compute {
                    inst: _,
                    dst,
                    input_regs,
                } => {
                    writeln!(
                        f,
                        "    [{}] Compute dst=r{} inputs={:?}",
                        i, dst, input_regs
                    )?;
                }
                MirDagNode::Effect { inst: _ } => {
                    writeln!(f, "    [{}] Effect", i)?;
                }
                MirDagNode::Branch {
                    cond,
                    true_target,
                    false_target,
                } => {
                    writeln!(
                        f,
                        "    [{}] Branch cond=r{} true={:?} false={:?}",
                        i, cond, true_target, false_target
                    )?;
                }
                MirDagNode::Jump { target } => {
                    writeln!(f, "    [{}] Jump target={:?}", i, target)?;
                }
                MirDagNode::Phi { reg, sources } => {
                    writeln!(f, "    [{}] Phi r{} sources={:?}", i, reg, sources)?;
                }
                MirDagNode::Label { label } => {
                    writeln!(f, "    [{}] Label {}", i, label)?;
                }
                MirDagNode::Removed => {
                    writeln!(f, "    [{}] (removed)", i)?;
                }
            }
        }
        writeln!(f, "  edges:")?;
        for edge in &self.edges {
            writeln!(f, "    {} -> {} {:?}", edge.from, edge.to, edge.kind)?;
        }
        Ok(())
    }
}

impl MirDag {
    /// 保留基本块内全序的 Sequence 边（no-op，v0.75.33 起 dag_analyze
    /// 已建完整块内链）。
    ///
    /// v0.104.6 D9：改走 `recompute_entry()`，恢复 `dag_analyze` 自 v0.104.2
    /// 起用、却被此处的旧公式（「仅无入边」）悄悄撤销的「可达 ∧ 无入边」过滤 ——
    /// 否则优化留下的不可达死块会被拉回入口集与真入口并列执行，其写入覆盖真实
    /// 结果（静默返回错值）。
    pub fn prune_sequence_edges(&mut self) {
        self.recompute_entry();
    }

    /// v0.104.6 D9：入口重算的**单一实现** —— 「可达 ∧ 无入边 ∧ 未移除」。
    ///
    /// `dag_analyze`、`prune_sequence_edges`、`add_sequential_edges` 与
    /// `dag_search::apply_rewrite` 第 5 步统一走本实现，避免 v0.104.2 的可达性
    /// 过滤被任一处旧公式撤销。
    pub fn recompute_entry(&mut self) {
        let n = self.nodes.len();
        // v0.104.6 D31：位向量取代 `HashSet<NodeId>`。
        //
        // 本函数被调用的次数是**改写次数的数倍** —— `apply_rewrite` 第 5 步的
        // 不动点循环每轮各调一次 `recompute_reachable_from_entry` +
        // `recompute_entry`，实测 375 条语句的程序里改写 374 次 → 本函数
        // 被调约 1 870 次。而旧实现**每次都从零重建两个 `HashSet<NodeId>`**
        // （逐边插入 ~4 876 次 + 逐节点过滤 ~1 877 次），合计约 **1 260 万次
        // 哈希插入**外加反复扩容 —— 这是 `dag_optimize` 全部耗时（约 17.5 s）
        // 的真正来源，与规则回调里的边扫描无关。
        //
        // 判据逐字等价：`has_incoming.contains(&i)` ≡ `has_incoming[i]`，
        // `reachable.contains(&i)` ≡ `self.reachable[i]`。`reachable` 本来
        // 就是 `Vec<bool>`，旧代码却又把它拷进一个 `HashSet` —— 白拷一遍。
        let mut has_incoming = vec![false; n];
        for e in &self.edges {
            if e.to < n {
                has_incoming[e.to] = true;
            }
        }
        let mut entry: Vec<NodeId> = (0..n)
            .filter(|&i| {
                !self.nodes[i].is_removed()
                    && !has_incoming[i]
                    && self.reachable.get(i).copied().unwrap_or(false)
            })
            .collect();
        if entry.is_empty() {
            // 退化兜底：根不可达（或全部候选被判不可达）时退回「无入边 ∧ 未移除」
            // —— 与 v0.104.2 之前的既有行为一致。必须保留：入口集为空时
            // `run_dag_with_signal_memo` 的 while 一次都不进，程序返回 Nil。
            entry = (0..n)
                .filter(|&i| !self.nodes[i].is_removed() && !has_incoming[i])
                .collect();
        }
        self.entry = entry;
    }

    /// v0.104.6 D9：按**当前 `entry`** 重算可达集（沿全部边 BFS）。
    ///
    /// 必要性：`dag_analyze` 的 `reachable` 从 **pc 0** 出发，但优化（尤其
    /// CSE / 常量折叠追加新节点并改写 entry）之后真正被激活的是**新入口集合**。
    /// 此时原入口及其前驱链被标 `Removed`，仍被标为「可达」，于是作为 Sequence
    /// 前驱进入执行器的 `seq_preds` 却**永远不会被激活**（`executed[]` 恒 false），
    /// 把其后继永久阻塞在就绪门槛外。
    ///
    /// 实测：手写直线链 `1+2 → +3` 在已优化路径上得 **3** 应 6。
    /// 回归测试：`tests/run_mir_equiv_run_dag.rs::optimize_preserves_handwritten_mir_chain`。
    ///
    /// # v0.104.6 D31：邻接表取代「每弹一个节点扫全边表」
    ///
    /// 旧实现的 BFS 内层是 `self.edges.iter().filter(|e| e.from == x)` ——
    /// **每弹出一个节点就把整张边表扫一遍**，即一次 BFS 是 O(V·E)。而本方法
    /// 被 `apply_rewrite` 第 5 步的**不动点循环**反复调用（每轮一次，最多 4 轮），
    /// 实测 375 条语句的程序里改写 374 次 → 本方法被调约 1 496 次 →
    /// 1 496 × 1 877 × 4 876 ≈ **1.4×10¹⁰ 次边比较**。这是 `dag_optimize`
    /// 全部耗时（约 17.5 s → 14.4 s）的**主因**。
    ///
    /// 改法：进入 BFS 前用**两趟线性扫描**建一份扁平 CSR 邻接表
    /// （`out_start` 为每节点出度前缀和，`out_targets` 紧凑存后继），之后
    /// BFS 只走邻接表 → 单次 O(V+E)。
    ///
    /// **语义逐字等价**：`reach` 是可达关系的**传递闭包**，与 BFS 的访问
    /// 顺序无关，故换遍历方式不改变结果。邻接表是**本次调用内现建现用**的
    /// 临时量，不缓存回 `MirDag` —— 故不存在「图变了而邻接表陈旧」的风险
    /// （`MirDag` 的边在 `dag_analyze` / `apply_rewrite` /
    /// `add_sequential_edges` 多处增长，任何挂字段的缓存都要逐处维护）。
    pub fn recompute_reachable_from_entry(&mut self) {
        let n = self.nodes.len();
        // CSR 第一趟：出度计数（用 `usize`，避免大图上 `u32` 前缀和溢出）
        let mut out_degree = vec![0usize; n + 1];
        for e in &self.edges {
            if e.from < n {
                out_degree[e.from + 1] += 1;
            }
        }
        // CSR 第二趟：前缀和 → out_start[i]..out_start[i+1] 是 i 的出边槽位
        for i in 0..n {
            out_degree[i + 1] += out_degree[i];
        }
        // 第三趟：填后继（按边序，故槽位内顺序与旧的全表扫描一致）
        let mut cursor = out_degree.clone();
        let mut out_targets: Vec<NodeId> = vec![0; self.edges.len()];
        for e in &self.edges {
            if e.from < n {
                let slot = &mut cursor[e.from];
                out_targets[*slot] = e.to;
                *slot += 1;
            }
        }

        let mut reach = vec![false; n];
        let mut stack: Vec<NodeId> = Vec::new();
        for &e in &self.entry {
            if !reach[e] {
                reach[e] = true;
                stack.push(e);
            }
        }
        while let Some(x) = stack.pop() {
            let xi = x;
            if xi >= n {
                continue;
            }
            for &to in &out_targets[out_degree[xi]..out_degree[xi + 1]] {
                if to < n && !reach[to] {
                    reach[to] = true;
                    stack.push(to);
                }
            }
        }
        self.reachable = reach;
    }

    /// v0.104.6：**Sequence 连通分量** —— 每个分量恰是一个基本块。
    ///
    /// `dag_analyze` 对每个基本块内的**相邻指令对**连 Sequence 边，跨块的
    /// 控制转移只连 Control 边，故「Sequence 连通分量 ≡ 基本块」。
    pub fn sequence_components(&self) -> Vec<usize> {
        let n = self.nodes.len();
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(parent: &mut [usize], mut x: usize) -> usize {
            while parent[x] != x {
                parent[x] = parent[parent[x]];
                x = parent[x];
            }
            x
        }
        for e in &self.edges {
            if !matches!(e.kind, EdgeKind::Sequence) {
                continue;
            }
            let (ra, rb) = (find(&mut parent, e.from), find(&mut parent, e.to));
            if ra != rb {
                parent[ra] = rb;
            }
        }
        (0..n).map(|i| find(&mut parent, i)).collect()
    }

    /// Add Sequence edges between consecutive nodes in each basic block.
    ///
    /// # ⚠️ 本方法**不保语义** —— 它是「强制线性化」实验装置，不是等价变换
    ///
    /// v0.104.6 复核：本注释原文是
    /// 「This forces linear execution order, making `run_dag` produce
    /// exactly the same result as `run_mir`」—— **该表述为假**。
    /// 它是 v0.59 那句「`run_mir ≡ run_dag`（add_sequential_edges 后退化线性）」
    /// 的**源头**：正因如此，后续 `src/mir/vm.rs` 的公开 API 文档、
    /// `tests/run_mir_equiv_run_dag.rs` 的对照实验都建立在这个前提上，
    /// 而当对照实验出现 3/9 分歧时，又被误判成「生产路径有缺陷（E1）」。
    ///
    /// **实测（`tests/run_mir_equiv_run_dag.rs`，9 例）**：6 例等价、
    /// **3 例发散**，且发散时**错的都是本方法这一侧**：
    ///
    /// | 用例          | `run_mir`（生产） | 加本方法后 | 正确值 |
    /// |---------------|------------------|-----------|--------|
    /// | if/else       | 8.0              | `Bool(false)` | 8.0 |
    /// | for + break   | 2.0              | 5.0      | 2.0 |
    /// | while + continue | 5.0           | 6.0      | 5.0 |
    ///
    /// 「哪边对」由独立判据确定：把 `continue` 改写成语义等价的 `if/else`
    /// 仍得 5.0、删掉 `continue` 才得 6.0 —— 见 `tests/continue_semantics.rs`。
    ///
    /// **机制**：本方法把整图连成一条线性链，而 `break` / `continue` 是
    /// **跳出**线性链的控制转移，在这个模型里失效 —— 循环体被跳过的部分照跑，
    /// 恰好多执行一轮。`if/else` 那例则是汇合点的分支臂被线性链强行串行化。
    ///
    /// **结论**：它**不能**用来判定生产路径的对错（那是把「线性化 + 忽略跳转」
    /// 的混合体当成基准）。生产路径 `run_mir` 在上述 3 例中全部正确。
    ///
    /// 注：本方法在 `src/` 里**无生产调用点**（`DagCache::build` 走的是
    /// `dag_analyze → dag_optimize → prune_sequence_edges`），仅测试使用。
    pub fn add_sequential_edges(&mut self) {
        // Nodes are created in program order (pc ascending), so
        // consecutive node IDs within each basic block already reflect
        // the correct linear order. We add Sequence edges between
        // adjacent pairs.
        let n = self.nodes.len();
        for i in 0..n.saturating_sub(1) {
            let j = i + 1;
            // Skip Label nodes (they're control-flow markers, not real ops)
            let from_is_label = matches!(self.nodes[i], MirDagNode::Label { .. });
            let to_is_label = matches!(self.nodes[j], MirDagNode::Label { .. });
            if from_is_label || to_is_label {
                continue;
            }
            self.edges.push(MirDagEdge {
                from: i,
                to: j,
                kind: EdgeKind::Sequence,
            });
        }
        // v0.104.6 D9：entry 走统一实现（与 `prune_sequence_edges` 同一语义），
        // 避免两处公式再次漂移。
        //
        // 注：本方法目前**无生产调用点**（`DagCache::build` 走的是
        // `dag_analyze → dag_optimize → prune_sequence_edges`），仅文档提及。
        self.recompute_entry();
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::BinaryOp;
    use crate::value::Value;

    fn make_func(body: Vec<MirInst>) -> MirFunction {
        let n_regs = body
            .iter()
            .filter_map(|i| i.dst())
            .max()
            .map(|r| r + 1)
            .unwrap_or(0);
        MirFunction {
            params: vec![],
            body,
            n_regs,
            ..Default::default()
        }
    }

    #[test]
    fn empty_body_produces_empty_dag() {
        let func = make_func(vec![]);
        let dag = dag_analyze(&func);
        assert!(dag.nodes.is_empty());
        assert!(dag.edges.is_empty());
    }

    #[test]
    fn single_const_has_one_node() {
        let func = make_func(vec![MirInst::Const(0, Value::Int(42))]);
        let dag = dag_analyze(&func);
        assert_eq!(dag.nodes.len(), 1);
        assert!(matches!(dag.nodes[0], MirDagNode::Compute { .. }));
    }

    #[test]
    fn binary_op_creates_data_edges() {
        // r0 = Const 10
        // r1 = Const 32
        // r2 = BinaryOp r0 + r1
        let func = make_func(vec![
            MirInst::Const(0, Value::Int(10)),
            MirInst::Const(1, Value::Int(32)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
        ]);
        let dag = dag_analyze(&func);
        assert_eq!(dag.nodes.len(), 3);

        // There should be Data edges from r0→r2 and r1→r2
        let data_edges: Vec<_> = dag
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Data { .. }))
            .collect();
        assert_eq!(data_edges.len(), 2, "should have 2 data edges");

        // Entry should be the first node(s) with no incoming edges
        assert!(!dag.entry.is_empty(), "should have entry nodes");
    }

    #[test]
    fn jump_creates_control_edge() {
        // Label 0: r0 = Const 1, Jump 0
        let func = make_func(vec![
            MirInst::Label(0),
            MirInst::Const(0, Value::Int(1)),
            MirInst::Jump(0),
        ]);
        let dag = dag_analyze(&func);
        assert!(dag.nodes.len() >= 3);

        let control_edges: Vec<_> = dag
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Control))
            .collect();
        assert!(
            !control_edges.is_empty(),
            "should have control edges from Jump"
        );
    }

    #[test]
    fn side_effects_create_sequence_edges() {
        // r0 = Const 42; Define x r0; r1 = Const 99; Assign x r1
        let func = make_func(vec![
            MirInst::Const(0, Value::Int(42)),
            MirInst::Define("x".to_string(), 0),
            MirInst::Const(1, Value::Int(99)),
            MirInst::Assign("x".to_string(), 1),
        ]);
        let dag = dag_analyze(&func);
        let seq_edges: Vec<_> = dag
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Sequence)
            .collect();
        assert!(
            !seq_edges.is_empty(),
            "Effects should create Sequence edges to subsequent nodes (found {})",
            seq_edges.len()
        );
    }

    #[test]
    fn topological_sort_linear_dag() {
        // v0.75.33: 块内全序 — 三个 const 无数据依赖，但 dag_analyze 建
        // 块内全序 Sequence 边（Compute 读 env/前序语句值，提前执行读脏值；
        // ILP 从未在 dag_interp 实现 — 顺序执行 ready 列表）。断言顺序保持。
        let func = make_func(vec![
            MirInst::Const(0, Value::Int(1)),
            MirInst::Const(1, Value::Int(2)),
            MirInst::Const(2, Value::Int(3)),
        ]);
        let dag = dag_analyze(&func);
        let levels = topological_sort(&dag).expect("should have valid topo sort");
        // 全序链：每节点一层，共 3 层
        assert_eq!(levels.len(), 3, "block-internal full order → 3 levels");
        assert_eq!(levels[0].len(), 1);
    }

    #[test]
    fn topological_sort_dependent_chain() {
        // r0=1, r1=r0+1, r2=r1+1 — chain, each depends on previous
        let func = make_func(vec![
            MirInst::Const(0, Value::Int(1)),
            MirInst::Const(1, Value::Int(1)), // helper const
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
            MirInst::BinaryOp(3, 2, BinaryOp::Add, 1),
        ]);
        let dag = dag_analyze(&func);
        let levels = topological_sort(&dag).expect("should have valid topo sort");
        // At minimum: r0 and r1 (no deps) in level 0,
        // r2 depends on r0,r1 → level 1,
        // r3 depends on r2 → level 2+
        assert!(
            levels.len() >= 2,
            "dependent chain should span multiple levels"
        );
    }

    #[test]
    fn prune_preserves_block_order() {
        // v0.75.33: prune 保留块内全序（旧实现裁剪 Compute 之间 Sequence，
        // 导致 Var/Compute 提前执行读脏值；ILP 从未在 dag_interp 实现 —
        // 顺序执行 ready 列表，裁剪零收益）。
        // r0=10, r1=32, r2=r0+r1 — 全序链 3 层，prune 后仍 3 层。
        let func = make_func(vec![
            MirInst::Const(0, Value::Int(10)),
            MirInst::Const(1, Value::Int(32)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
        ]);
        let dag = dag_analyze(&func);
        let levels_before = topological_sort(&dag).unwrap();
        assert_eq!(
            levels_before.len(),
            3,
            "block-internal full order → 3 levels"
        );

        // prune_sequence_edges 保留所有 Sequence 边（no-op 保留正确性）。
        let mut dag_pruned = dag_analyze(&func);
        dag_pruned.prune_sequence_edges();
        let levels_pruned = topological_sort(&dag_pruned).unwrap();
        assert_eq!(
            levels_pruned.len(),
            3,
            "prune 保留全序链 → 仍 3 levels, got {}",
            levels_pruned.len()
        );
        // 全序链被保持 — 无「并行化」裁剪。
        let seq_count = dag_pruned
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Sequence)
            .count();
        assert_eq!(seq_count, 2, "3 节点全序链应有 2 条 Sequence 边");
    }

    #[test]
    fn jump_if_creates_branch_node() {
        // r0 = Const true; JumpIf r0 label_1 ; Label 1: r1 = Const 42
        let func = make_func(vec![
            MirInst::Const(0, Value::Bool(true)),
            MirInst::JumpIf(0, 2),
            MirInst::Label(2),
            MirInst::Const(1, Value::Int(42)),
        ]);
        let dag = dag_analyze(&func);
        let branch_nodes: Vec<_> = dag
            .nodes
            .iter()
            .filter(|n| matches!(n, MirDagNode::Branch { .. }))
            .collect();
        assert_eq!(branch_nodes.len(), 1, "should have one branch node");
    }
}
