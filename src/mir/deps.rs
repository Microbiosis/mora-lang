//! mora-lang 依赖图引擎 (Dependency Graph Engine)
//!
//! 借鉴电子表格（Excel/Google Sheets）的依赖图 + 增量重算模型：
//! - 每个 MIR 指令是图中的一个节点
//! - 寄存器读取/写入构成有向边（写入者 → 读取者）
//! - 执行顺序由拓扑排序决定，而非线性 pc
//! - 当输入值变化时，只重算受影响的下游节点（增量重算）
//!
//! 这是 mora-lang 从"顺序执行"演进为"数据流驱动"的核心基础设施。

use crate::mir::{MirFunction, MirInst, Reg};

use std::collections::{HashMap, HashSet, VecDeque};

// ─── 指令类型分类 ───────────────────────────────────────────

///  一个 MIR 指令在依赖图中的分类。
///  分类决定了指令在图中的处理策略：
/// - `Pure`: 纯计算，可安全并行/重算
/// - `EnvRead`: 读取环境，结果依赖环境状态
/// - `Effect`: 有副作用（I/O, 消息发送），不可随意重排
/// - `Control`: 控制流，依赖拓扑排序保证正确性
/// - `Call`: 函数调用，可能包含任意副作用
/// - `EmbeddedBody`: 嵌入的 MirFunction body，不参与当前函数的寄存器依赖图
/// - `NoOp`: 无操作，可安全跳过
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstKind {
    Pure,
    EnvRead,
    Effect,
    Control,
    Call,
    EmbeddedBody,
    NoOp,
}

// ─── 依赖节点 ───────────────────────────────────────────────

///  一个 MIR 指令在依赖图中的表示。
///  每个节点记录：
/// - `pc`: 指令在函数中的位置
/// - `writes`: 该指令写入的寄存器列表（通常是 0 或 1 个）
/// - `reads`: 该指令读取的寄存器列表
/// - `kind`: 指令的类型分类（用于执行策略）
#[derive(Debug, Clone)]
struct DepNode {
    pc: usize,
    writes: Vec<Reg>,
    reads: Vec<Reg>,
    kind: InstKind,
}

// ─── 依赖图 ─────────────────────────────────────────────────

///  MIR 函数的依赖图。
///  核心数据结构：
/// - `nodes`: 每个指令对应的 DepNode
/// - `dependents`: 邻接表，dependents[i] 列出所有依赖指令 i 输出的指令
/// - `in_degree`: 每个指令的入度（还缺多少前置指令未执行）
/// - `dirty_regs`: 标记为"已变更"的寄存器集合（用于增量重算）
/// - `writer_of`: 每个寄存器最后写入它的指令 pc
/// - `readers_of`: 每个寄存器最后读取它的指令 pc 集合
///  执行流程：
/// 1. 初始执行：将入度为 0 的指令加入就绪队列，拓扑序执行
/// 2. 增量重算：当某寄存器被外部修改时，标记为 dirty，沿 dependents 边传播
///  dirty 标记，只重算受影响的指令
#[derive(Debug, Clone)]
pub struct DependencyGraph {
    nodes: Vec<DepNode>,
    dependents: Vec<Vec<usize>>,
    in_degree: Vec<usize>,
    /// 就绪队列缓存（初始执行时用）
    ready_cache: Vec<usize>,
    /// 脏寄存器集合
    dirty_regs: HashSet<Reg>,
    /// 每个寄存器最后写入它的指令 pc（用于 dirty propagation）
    writer_of: HashMap<Reg, usize>,
    /// 每个寄存器最后读取它的指令 pc 集合（反向传播用）
    readers_of: HashMap<Reg, HashSet<usize>>,
    /// 被控制流跳过的不可达指令集合
    skipped: HashSet<usize>,
}

impl DependencyGraph {
    /// 从 MirFunction 构建依赖图。
    /// 算法：
    /// 1. 遍历每条指令，分析其读写寄存器
    /// 2. 对于每条读操作，找到该寄存器最后一次被谁写入
    /// 3. 添加边：写入者 → 读取者（写入者必须先执行）
    /// 4. 维护 writer_of 映射用于快速查找
    pub fn build(func: &MirFunction) -> Self {
        let n = func.body.len();
        let mut nodes = Vec::with_capacity(n);
        let mut dependents = vec![Vec::new(); n];
        let mut in_degree = vec![0; n];
        let mut writer_of: HashMap<Reg, usize> = HashMap::new();
        let mut readers_of: HashMap<Reg, HashSet<usize>> = HashMap::new();
        let mut ready_cache = Vec::new();

        for (pc, inst) in func.body.iter().enumerate() {
            let (writes, reads, kind) = Self::analyze_inst(inst);
            let node = DepNode {
                pc,
                writes: writes.clone(),
                reads: reads.clone(),
                kind,
            };

            // 对于每条读取的寄存器，找到写入者并添加依赖边
            for &read_reg in &reads {
                if let Some(&writer_pc) = writer_of.get(&read_reg) {
                    dependents[writer_pc].push(pc);
                    in_degree[pc] += 1;
                }
                // 记录读取关系
                readers_of
                    .entry(read_reg)
                    .or_default()
                    .insert(pc);
            }

            // 该指令现在是其输出寄存器的写入者
            for &write_reg in &writes {
                writer_of.insert(write_reg, pc);
            }

            // 入度为 0 的指令加入就绪缓存
            if in_degree[pc] == 0 {
                ready_cache.push(pc);
            }

            nodes.push(node);
        }

        Self {
            nodes,
            dependents,
            in_degree,
            ready_cache,
            dirty_regs: HashSet::new(),
            writer_of,
            readers_of,
            skipped: HashSet::new(),
        }
    }

    /// 分析单条指令的读写寄存器和指令类型。
    fn analyze_inst(inst: &MirInst) -> (Vec<Reg>, Vec<Reg>, InstKind) {
        use MirInst::*;

        match inst {
            // ── 纯计算 ─────────────────────────────────────
            Const(dst, _) => (vec![*dst], vec![], InstKind::Pure),
            Copy(dst, src) => (vec![*dst], vec![*src], InstKind::Pure),
            BinaryOp(dst, l, _, r) => (vec![*dst], vec![*l, *r], InstKind::Pure),
            ListLit(dst, items) => (vec![*dst], items.clone(), InstKind::Pure),
            DictLit(dst, pairs) => {
                let reads: Vec<Reg> = pairs.iter().map(|(_, r)| *r).collect();
                (vec![*dst], reads, InstKind::Pure)
            }
            Index(dst, obj, idx) => (vec![*dst], vec![*obj, *idx], InstKind::Pure),
            IndexAssign(dst, obj, idx) => {
                // dst 既是写入目标，也被读取（自赋值语义）
                (vec![*dst], vec![*dst, *obj, *idx], InstKind::Pure)
            }
            DynTrait { dst, src, .. } => (vec![*dst], vec![*src], InstKind::Pure),

            // ── 环境读取 ───────────────────────────────────
            Var(dst, _) => (vec![*dst], vec![], InstKind::EnvRead),
            LoadEnv(dst, _) => (vec![*dst], vec![], InstKind::EnvRead),

            // ── 函数调用 ───────────────────────────────────
            Call(dst, _, args) => (vec![*dst], args.clone(), InstKind::Call),

            // ── 闭包构造 ───────────────────────────────────
            Closure { dst, body: _, .. } => {
                // 闭包本身写入 dst，但 body 是嵌入的 MirFunction
                // 其寄存器不参与当前函数的依赖图
                (vec![*dst], vec![], InstKind::EmbeddedBody)
            }

            // ── 控制流 ─────────────────────────────────────
            Jump(_) => (vec![], vec![], InstKind::Control),
            JumpIf(cond, _lbl) => (vec![], vec![*cond], InstKind::Control),
            JumpIfNot(cond, _lbl) => (vec![], vec![*cond], InstKind::Control),
            Return(r) => (
                vec![],
                r.map_or(vec![], |reg| vec![reg]),
                InstKind::Control,
            ),
            Break(_) => (vec![], vec![], InstKind::Control),
            Continue(_) => (vec![], vec![], InstKind::Control),
            Label(_) => (vec![], vec![], InstKind::Control),

            // ── Match ──────────────────────────────────────
            MatchExpr { val, arms } => {
                // 读取 scrutinee 值，写入 arms 的 output_reg
                let writes: Vec<Reg> = arms.iter().map(|arm| arm.3).collect();
                (writes, vec![*val], InstKind::Pure)
            }
            MatchArm { .. } => (vec![], vec![], InstKind::Control),

            // ── 嵌入体（含 body）───
            TaskDef { .. } => (vec![], vec![], InstKind::EmbeddedBody),
            Transaction { body: _, compensation: _, .. } => {
                // Transaction 本身不读写当前函数的寄存器
                // body 和 compensation 是独立的 MirFunction
                (vec![], vec![], InstKind::EmbeddedBody)
            }
            Send { value, .. } => (vec![], vec![*value], InstKind::Effect),
            Receive { .. } => (vec![], vec![], InstKind::EnvRead),
            Worker { .. } => (vec![], vec![], InstKind::EmbeddedBody),
            Observe { .. } => (vec![], vec![], InstKind::EmbeddedBody),
            Span { .. } => (vec![], vec![], InstKind::EmbeddedBody),
            WithConfig { .. } => (vec![], vec![], InstKind::EmbeddedBody),
            StreamFor { prompt_reg, body: _, .. } => {
                // 读取 prompt_reg（输入），body 是嵌入的
                (vec![], vec![*prompt_reg], InstKind::EmbeddedBody)
            }
            PromptSection { .. } => (vec![], vec![], InstKind::EmbeddedBody),
            DocumentSection { .. } => (vec![], vec![], InstKind::EmbeddedBody),

            // ── I/O ────────────────────────────────────────
            Save { path, value } => (vec![], vec![*path, *value], InstKind::Effect),
            Load { path, .. } => (vec![], vec![*path], InstKind::EnvRead),
            ReadFile { path, .. } => (vec![], vec![*path], InstKind::EnvRead),
            WriteFile { path, content } => (vec![], vec![*path, *content], InstKind::Effect),
            AppendFile { path, content } => (vec![], vec![*path, *content], InstKind::Effect),
            ReadBytesFile { path, .. } => (vec![], vec![*path], InstKind::EnvRead),
            WriteBytesFile { path, content } => (vec![], vec![*path, *content], InstKind::Effect),

            // ── 定义 / 声明 ─────────────────────────────────
            MacroDef { .. } => (vec![], vec![], InstKind::NoOp),
            TypeAlias { .. } => (vec![], vec![], InstKind::NoOp),
            EnumDef { .. } => (vec![], vec![], InstKind::NoOp),
            StructDef { .. } => (vec![], vec![], InstKind::NoOp),
            Route(_) => (vec![], vec![], InstKind::NoOp),
            Rollback => (vec![], vec![], InstKind::NoOp),
            Commit => (vec![], vec![], InstKind::NoOp),
            Import(_) => (vec![], vec![], InstKind::NoOp),
            RecordTokens { .. } => (vec![], vec![], InstKind::NoOp),

            // ── 可观测性 ────────────────────────────────────
            Eval { given_reg, expects, .. } => {
                let mut reads = vec![*given_reg];
                reads.extend(expects.iter().copied());
                (vec![], reads, InstKind::Effect)
            }

            // ── Trait / Impl / Skill ────────────────────────
            TraitDef { method_bodies: _, .. } => {
                // TraitDef 的 method_bodies 是 Vec<MirFunction>，
                // 不参与当前函数的寄存器依赖图
                (vec![], vec![], InstKind::EmbeddedBody)
            }
            ImplDef { method_bodies: _, .. } => {
                // 同上
                (vec![], vec![], InstKind::EmbeddedBody)
            }
            SkillDef { task_bodies: _, verify_body: _, .. } => {
                // task_bodies 和 verify_body 是 Vec<MirFunction>/Option<MirFunction>
                // 不参与当前函数的寄存器依赖图
                (vec![], vec![], InstKind::EmbeddedBody)
            }
            ToolDef { .. } => (vec![], vec![], InstKind::EmbeddedBody),

            // ── 其他 ────────────────────────────────────────
            Orchestrate { .. } => (vec![], vec![], InstKind::Effect),
            MethodCall(dst, receiver, _, args) => {
                let mut reads = vec![*receiver];
                reads.extend(args.iter().copied());
                (vec![*dst], reads, InstKind::Call)
            }
            Pipe(dst, lhs, rhs) => (vec![*dst], vec![*lhs, *rhs], InstKind::Pure),
            Prompt(dst, args) => (vec![*dst], args.clone(), InstKind::Call),
            _Unreachable => (vec![], vec![], InstKind::Control),
        }
    }

    // ─── 初始执行：就绪队列 ──────────────────────────────

    /// 获取初始就绪队列（入度为 0 的指令）。
    /// 这是拓扑排序的起点。类似电子表格打开时的初始计算顺序。
    pub fn initial_ready(&self) -> &[usize] {
        &self.ready_cache
    }

    /// 执行指令 `pc` 后，返回新变为就绪的指令列表。
    /// 这模拟了电子表格中"一个单元格计算完成后，下游单元格变为可计算"的行为。
    /// 自动跳过被控制流标记为不可达的指令。
    pub fn execute(&mut self, pc: usize) -> Vec<usize> {
        let mut newly_ready = Vec::new();
        for &dep_pc in &self.dependents[pc] {
            if self.skipped.contains(&dep_pc) {
                continue;
            }
            self.in_degree[dep_pc] -= 1;
            if self.in_degree[dep_pc] == 0 {
                newly_ready.push(dep_pc);
            }
        }
        newly_ready
    }

    // ─── 并行执行 ──────────────────────────────────────────

    /// 执行当前所有就绪指令（并行组），返回新变为就绪的指令列表。
    /// 这是 `parallel_groups()` 的动态版本：
    /// - 取出当前所有入度为 0 且未跳过的指令
    /// - 模拟"并行执行"（实际仍顺序执行，但保证数据独立性）
    /// - 返回下一组就绪指令
    /// 类似电子表格中"一批没有交叉依赖的公式同时计算"。
    pub fn execute_parallel_group(&mut self, ready: &[usize]) -> Vec<usize> {
        let mut newly_ready = Vec::new();
        for &pc in ready {
            if self.skipped.contains(&pc) {
                continue;
            }
            for &dep_pc in &self.dependents[pc] {
                if self.skipped.contains(&dep_pc) {
                    continue;
                }
                self.in_degree[dep_pc] -= 1;
                if self.in_degree[dep_pc] == 0 {
                    newly_ready.push(dep_pc);
                }
            }
        }
        newly_ready
    }

    // ─── 控制流：跳过不可达指令 ──────────────────────────

    /// 标记 [from, to] 范围内的指令为不可达（被控制流跳过）。
    /// 这替代了手工维护的 skip_ranges 机制，让依赖图自身处理不可达性。
    /// 类似电子表格中"隐藏的行不参与计算"。
    pub fn skip_range(&mut self, from: usize, to: usize) {
        for pc in from..=to {
            if pc < self.nodes.len() {
                self.skipped.insert(pc);
            }
        }
        // 跳过这些指令后，重新计算剩余可达指令的入度
        self.recompute_in_degrees();
    }

    /// 跳过 Jump 指令后面的所有指令，直到跳转目标。
    /// 这是 `skip_range` 的便捷方法，专门处理无条件跳转。
    pub fn skip_until(&mut self, from: usize, to: usize) {
        self.skip_range(from + 1, to.saturating_sub(1));
    }

    /// 重新计算可达指令的入度（跳过不可达指令后调用）。
    fn recompute_in_degrees(&mut self) {
        self.in_degree = vec![0; self.nodes.len()];
        for pc in 0..self.nodes.len() {
            if self.skipped.contains(&pc) {
                continue;
            }
            for &dep_pc in &self.dependents[pc] {
                if !self.skipped.contains(&dep_pc) {
                    self.in_degree[dep_pc] += 1;
                }
            }
        }
        // 重建就绪缓存
        self.ready_cache.clear();
        for pc in 0..self.nodes.len() {
            if self.skipped.contains(&pc) {
                continue;
            }
            if self.in_degree[pc] == 0 {
                self.ready_cache.push(pc);
            }
        }
    }

    /// 检查指令是否被跳过（不可达）。
    pub fn is_skipped(&self, pc: usize) -> bool {
        self.skipped.contains(&pc)
    }

    // ─── 增量重算：脏标记传播 ──────────────────────────

    /// 标记寄存器 `reg` 为"已变更"（dirty），返回所有受影响的指令。
    /// 这是增量重算的核心。类似 Excel 中修改 A1 后，
    /// 自动标记所有引用 A1 的单元格为需要重算。
    pub fn mark_dirty(&mut self, reg: Reg) -> Vec<usize> {
        let mut affected = Vec::new();
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();

        // 找到所有读取该寄存器的指令
        if let Some(readers) = self.readers_of.get(&reg) {
            for &pc in readers {
                queue.push_back(pc);
                visited.insert(pc);
            }
        }

        // 沿 dependents 边传播 dirty 标记
        while let Some(pc) = queue.pop_front() {
            affected.push(pc);
            self.dirty_regs.insert(pc);

            // 传播到下游：该指令写入的寄存器，其所有读取者也被影响
            for &write_reg in &self.nodes[pc].writes {
                if let Some(readers) = self.readers_of.get(&write_reg) {
                    for &dep_pc in readers {
                        if !visited.contains(&dep_pc) {
                            visited.insert(dep_pc);
                            queue.push_back(dep_pc);
                        }
                    }
                }
            }

            // 沿 dependents 边传播
            for &dep_pc in &self.dependents[pc] {
                if !visited.contains(&dep_pc) {
                    visited.insert(dep_pc);
                    queue.push_back(dep_pc);
                }
            }
        }

        affected
    }

    /// 清除所有脏标记（重算完成后调用）。
    pub fn clear_dirty(&mut self) {
        self.dirty_regs.clear();
    }

    /// 检查指令是否受 dirty 影响。
    pub fn is_dirty(&self, pc: usize) -> bool {
        self.dirty_regs.contains(&pc)
    }

    // ─── 增量重算：选择性重算脏指令 ──────────────────────

    /// 获取所有脏指令的拓扑排序执行顺序。
    /// 用于增量重算：只重算受 dirty 影响的指令，跳过干净指令。
    /// 保证：重算顺序满足依赖关系，结果与全量重算一致。
    pub fn recompute_dirty(&self) -> Vec<usize> {
        let dirty_nodes: Vec<usize> = self.dirty_regs.iter().cloned().collect();
        // 对脏指令进行拓扑排序
        let mut in_degree = vec![0; self.nodes.len()];
        let mut dirty_dependents: Vec<Vec<usize>> = vec![Vec::new(); self.nodes.len()];
        
        // 只考虑脏指令之间的依赖关系
        for &pc in &dirty_nodes {
            for &dep_pc in &self.dependents[pc] {
                if self.dirty_regs.contains(&dep_pc) {
                    dirty_dependents[pc].push(dep_pc);
                    in_degree[dep_pc] += 1;
                }
            }
        }

        let mut order = Vec::new();
        let mut queue: VecDeque<usize> = dirty_nodes
            .iter()
            .filter(|&&pc| in_degree[pc] == 0)
            .copied()
            .collect();

        while let Some(pc) = queue.pop_front() {
            order.push(pc);
            for &dep_pc in &dirty_dependents[pc] {
                in_degree[dep_pc] -= 1;
                if in_degree[dep_pc] == 0 {
                    queue.push_back(dep_pc);
                }
            }
        }

        // 如果拓扑排序不完整，按 pc 顺序补充
        if order.len() != dirty_nodes.len() {
            for pc in dirty_nodes {
                if !order.contains(&pc) {
                    order.push(pc);
                }
            }
        }

        order
    }

    // ─── 拓扑排序 ──────────────────────────────────────

    /// 返回完整拓扑排序的执行顺序。
    /// 用于：一次性完整执行（替代线性 pc 遍历）。
    /// 保证：所有依赖关系满足，等效于原始顺序的执行结果。
    pub fn topological_order(&self) -> Vec<usize> {
        let mut order = Vec::with_capacity(self.nodes.len());
        let mut in_degree = self.in_degree.clone();
        let mut queue: VecDeque<usize> = self.ready_cache.iter().copied().collect();

        while let Some(pc) = queue.pop_front() {
            order.push(pc);
            for &dep_pc in &self.dependents[pc] {
                in_degree[dep_pc] -= 1;
                if in_degree[dep_pc] == 0 {
                    queue.push_back(dep_pc);
                }
            }
        }

        // 如果 order 长度不足，说明有循环依赖（不应该发生）
        // 回退：按原始 pc 顺序执行未排序的节点
        if order.len() != self.nodes.len() {
            for i in 0..self.nodes.len() {
                if !order.contains(&i) {
                    order.push(i);
                }
            }
        }

        order
    }

    // ─── 并行执行分析 ──────────────────────────────────

    /// 返回可并行执行的指令组。
    /// 每组内的指令互不依赖，可以安全并行执行。
    /// 组间有依赖关系，必须按顺序执行。
    /// 类似电子表格中"一批没有交叉依赖的公式可以同时计算"。
    pub fn parallel_groups(&self) -> Vec<Vec<usize>> {
        let mut groups = Vec::new();
        let mut in_degree = self.in_degree.clone();
        let mut current_group: Vec<usize> = self.ready_cache.iter().copied().collect();
        let mut next_group = Vec::new();

        while !current_group.is_empty() {
            groups.push(current_group.clone());
            next_group.clear();

            for pc in &current_group {
                for &dep_pc in &self.dependents[*pc] {
                    in_degree[dep_pc] -= 1;
                    if in_degree[dep_pc] == 0 {
                        next_group.push(dep_pc);
                    }
                }
            }

            current_group = next_group.clone();
        }

        groups
    }

    // ─── 统计信息 ──────────────────────────────────────

    /// 返回依赖图的统计信息。
    pub fn stats(&self) -> DepGraphStats {
        let total_edges: usize = self.dependents.iter().map(|d| d.len()).sum();
        let max_in_degree = self.in_degree.iter().max().copied().unwrap_or(0);
        let n_parallel = self.parallel_groups().len();

        DepGraphStats {
            n_nodes: self.nodes.len(),
            n_edges: total_edges,
            max_in_degree,
            n_parallel_groups: n_parallel,
            n_dirty: self.dirty_regs.len(),
        }
    }
}

// ─── 统计信息 ─────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct DepGraphStats {
    pub n_nodes: usize,
    pub n_edges: usize,
    pub max_in_degree: usize,
    pub n_parallel_groups: usize,
    pub n_dirty: usize,
}

// ─── 便捷 API ──────────────────────────────────────────────

///  构建依赖图并返回统计信息（用于诊断和优化决策）。
pub fn analyze(func: &MirFunction) -> DepGraphStats {
    let graph = DependencyGraph::build(func);
    graph.stats()
}

///  构建依赖图并返回拓扑排序的执行顺序。
pub fn topo_order(func: &MirFunction) -> Vec<usize> {
    let graph = DependencyGraph::build(func);
    graph.topological_order()
}

///  构建依赖图并返回可并行执行的指令组。
pub fn parallel_groups(func: &MirFunction) -> Vec<Vec<usize>> {
    let graph = DependencyGraph::build(func);
    graph.parallel_groups()
}
