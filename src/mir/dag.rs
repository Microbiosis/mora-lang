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

    // Step 1: Create nodes for every instruction
    for blk in &blocks {
        let mut block_first_node: Option<NodeId> = None;
        let mut prev_effect_node: Option<NodeId> = None;

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

            // Connect sequential side-effect chain
            if inst.is_effect() {
                if let Some(prev) = prev_effect_node {
                    edges.push(MirDagEdge {
                        from: prev,
                        to: idx,
                        kind: EdgeKind::Sequence,
                    });
                }
                prev_effect_node = Some(idx);
            }
        }

        if let Some(first) = block_first_node {
            block_entry.insert(blk.start, first);
        }
    }

    // Step 2: Resolve control flow edges
    // Patch Jump targets
    for (pc, node_id) in &pc_to_node {
        let inst = &body[*pc];
        match inst {
            MirInst::Jump(target) => {
                if let Some(&target_id) = label_to_node.get(target) {
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
                if let Some(&target_id) = label_to_node.get(target) {
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
                if let Some(&target_id) = label_to_node.get(target) {
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
            MirInst::Break(target) | MirInst::Continue(target) => {
                if let Some(&target_id) = label_to_node.get(target) {
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

    let entry: Vec<NodeId> = (0..nodes.len())
        .filter(|n| !has_incoming.contains(n))
        .collect();
    let exit: Vec<NodeId> = (0..nodes.len())
        .filter(|n| !has_outgoing.contains(n))
        .collect();

    MirDag {
        nodes,
        edges,
        entry,
        exit,
        n_regs: func.n_regs,
    }
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
            let node = queue.pop_front().unwrap();
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
    /// Remove redundant Sequence edges between pure Compute nodes.
    ///
    /// Data edges already encode register-level dependencies, so
    /// Sequence edges between two Compute nodes add no information
    /// and only force unnecessary serialization.
    ///
    /// After pruning, independent instructions naturally fall into
    /// the same topological level, exposing instruction-level
    /// parallelism (the "electronic spreadsheet" model).
    pub fn prune_sequence_edges(&mut self) {
        self.edges.retain(|e| {
            if e.kind != EdgeKind::Sequence {
                return true;
            }
            // Keep Sequence edges originating from Effect nodes
            // (side effects must be ordered).
            matches!(self.nodes[e.from], MirDagNode::Effect { .. })
        });
        // Recompute entry
        let n = self.nodes.len();
        let mut has_incoming: HashSet<NodeId> = HashSet::new();
        for edge in &self.edges {
            has_incoming.insert(edge.to);
        }
        self.entry = (0..n)
            .filter(|&i| !self.nodes[i].is_removed() && !has_incoming.contains(&i))
            .collect();
    }

    /// Add Sequence edges between consecutive nodes in each basic block.
    ///
    /// This forces linear execution order, making `run_dag` produce
    /// exactly the same result as `run_mir`. Without this, only true
    /// data+control dependencies constrain ordering, which exposes
    /// instruction-level parallelism.
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
        // Recompute entry set
        let mut has_incoming: HashSet<NodeId> = HashSet::new();
        for edge in &self.edges {
            has_incoming.insert(edge.to);
        }
        self.entry = (0..n).filter(|i| !has_incoming.contains(i)).collect();
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
        // No dependencies between consts — should all be in one level
        let func = make_func(vec![
            MirInst::Const(0, Value::Int(1)),
            MirInst::Const(1, Value::Int(2)),
            MirInst::Const(2, Value::Int(3)),
        ]);
        let dag = dag_analyze(&func);
        let levels = topological_sort(&dag).expect("should have valid topo sort");
        // All three nodes are independent — one level
        assert_eq!(levels.len(), 1, "independent consts should be in one level");
        assert_eq!(levels[0].len(), 3);
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
    fn prune_exposes_parallelism() {
        // r0=10, r1=32, r2=r0+r1 — Consts independent, should be in same level
        let func = make_func(vec![
            MirInst::Const(0, Value::Int(10)),
            MirInst::Const(1, Value::Int(32)),
            MirInst::BinaryOp(2, 0, BinaryOp::Add, 1),
        ]);
        let dag = dag_analyze(&func);
        let levels_before = topological_sort(&dag).unwrap();
        // With sequential edges: every node in its own level
        let mut dag_seq = dag_analyze(&func);
        dag_seq.add_sequential_edges();
        let levels_linear = topological_sort(&dag_seq).unwrap();
        assert!(
            levels_linear.len() > levels_before.len(),
            "sequential edges increase level count: before={} after={}",
            levels_before.len(),
            levels_linear.len()
        );

        // After pruning: two Consts should be in same level
        let mut dag_pruned = dag_analyze(&func);
        dag_pruned.prune_sequence_edges();
        let levels_pruned = topological_sort(&dag_pruned).unwrap();
        assert!(
            levels_pruned.len() <= 2,
            "pruned DAG should have <=2 levels (Consts parallel), got {}",
            levels_pruned.len()
        );
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
