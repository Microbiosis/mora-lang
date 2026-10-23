//! SSA 构造与 deconstruct（α.3）
//!
//! MIR-plain（线性指令列表 + 直接索引跳转） → MIR-ssa（基本块 + phi + 单赋值）
//!
//! 算法（基本块划分 → 支配树 → 支配边界/phi → DFS 重命名 → deconstruct）：
//! 1. 基本块划分 — Label 入口，terminator 出口
//! 2. 支配树（迭代数据流，CFG 块数少 ~20）
//! 3. 支配边界 + phi 插入
//! 4. 变量重命名（DFS）
//! 5. Deconstruct：phi → copy 指令，SSA → MIR-plain
//!
//! 约束：C2 手写 / I5 可回退（MORA_OPT=0 跳过）

use std::collections::{HashMap, HashSet, VecDeque};

use super::{MirFunction, MirInst};

use crate::common::BinaryOp;

use crate::value::Value;

/// SSA 寄存器（每个定义点分配唯一版本号）
pub type SsaReg = usize;
/// 基本块索引
pub type BlockId = usize;

/// SSA 形式的 MIR 函数
#[derive(Debug, Clone)]
pub struct MirSsaFunction {
    /// (param_name, ssa_reg) 映射
    pub params: Vec<(String, SsaReg)>,
    pub blocks: Vec<BasicBlock>,
    pub entry: BlockId,
    /// v0.104.6 D418：版本号计数器的终值 = **SSA 寄存器空间的大小**。
    ///
    /// 形参占 `0..params.len()`，此后每次定义（phi 与指令各算一次）`+1`。
    /// 于是「任一被定义的寄存器 `< n_versions`」是一条精确的不变式：
    /// **任何漏掉重命名的块都会暴露** —— 它保留的是原始物理编号，可能超出
    /// 这个空间（未重命名的症状正是 D312 观测到的「部分重命名」）。
    pub n_versions: SsaReg,
}

// v0.75.82: RegType 已删除 — 死类型推断（零消费者）。infer_types 全仓
// 零调用者、types 字段零读取、从未接入 JIT（jit.rs 用自有局部类型跟踪）。
// 类型推断的活机制是 witness typeck（typeck/hm，compile 主路径
// check_program_witnesses）— 与 v0.75.81 RecordTokens/Route 删除同一先例。

/// 基本块：phi + 纯值指令 + terminator
#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub id: BlockId,
    pub phis: Vec<Phi>,
    pub insts: Vec<SsaInst>,
    pub terminator: Terminator,
    pub preds: Vec<BlockId>,
    pub succs: Vec<BlockId>,
}

/// Phi 节点
#[derive(Debug, Clone)]
pub struct Phi {
    /// phi 结果的 **SSA 版本号** —— 由 `rename_variables` 分配（fresh）。
    pub dst: SsaReg,
    /// phi 合并的**原始变量编号**。
    ///
    /// v0.104.6 D418：`dst` 会被重命名覆盖，而**回边上的 incoming 必须按
    /// 原始编号查 rename 栈**（回边指向的祖先/自身块早已重命名完毕，
    /// 那里 `dst` 已是新号）。没有这个字段就只能在「phi 尚未重命名」时
    /// 记录 incoming —— 而那恰好排除了全部回边。
    pub var: SsaReg,
    /// `(前驱块, 该前驱出口处可见的 SSA 版本)`。
    ///
    /// v0.104.6 D418：此前**恒为空** —— `insert_phi_nodes` 以
    /// `incoming: Vec::new()` 建，而 `rename_variables` 收的是不可变引用且
    /// 内部 `clone()`，无处可写 ⇒ `deconstruct` 一条前驱 copy 都不发 ⇒
    /// phi 目标寄存器**没有生产者**。
    pub incoming: Vec<(BlockId, SsaReg)>,
}

/// SSA 纯值指令
#[derive(Debug, Clone)]
pub enum SsaInst {
    Const(SsaReg, Value),
    Var(SsaReg, String),
    BinaryOp(SsaReg, SsaReg, BinaryOp, SsaReg),
    Call(SsaReg, String, Vec<SsaReg>),
    /// N 元值调用（被调者在寄存器里）—— 对应 [`MirInst::CallValue`]。
    CallValue(SsaReg, SsaReg, Vec<SsaReg>),
    ListLit(SsaReg, Vec<SsaReg>),
    DictLit(SsaReg, Vec<(String, SsaReg)>),
    Index(SsaReg, SsaReg, SsaReg),
    IndexAssign(SsaReg, SsaReg, SsaReg),
    MethodCall(SsaReg, SsaReg, String, Vec<SsaReg>),
    Pipe(SsaReg, SsaReg, SsaReg),
    Prompt(SsaReg, Vec<SsaReg>),
    Copy(SsaReg, SsaReg),
    Define(String, SsaReg),
    Assign(String, SsaReg),
    Expr(SsaReg),
    /// v0.104.6 D417：**不透明指令** —— SSA 不为其建模语义（声明型 /
    /// effectful：`Closure` / `MatchExpr` / `Handle` / `TaskDef` / `RelDef` …），
    /// 但它**与普通指令走同一条重命名路径**：输入经 `map_regs` 解析、
    /// 输出按 `written_reg()` 分配 fresh 版本号。
    ///
    /// ## 为什么必须是 IR 的一等变体，而不是「收集起来事后拼回去」
    ///
    /// 旧做法（v0.75.30 ~ v0.104.6 D316）是：把这类指令抽进
    /// `MirSsaFunction::passthrough`，`deconstruct` 时用 `chain` **整体前插**
    /// 到 body 头部。于是：
    ///
    /// - **两套寄存器空间**：它们的寄存器不参与重编号，同函数其余指令却会
    ///   （D310 实测：闭包写 r8 而 `map` 读 r2）。
    /// - **顺序被打乱**：`passthrough` 只按顺序收集、不记原始位置，
    ///   前插后声明型指令跑到了使用者之前/之后都取决于原位置。
    /// - **`n_regs` 只能靠 `max(next_plain_reg, orig_n_regs)` 掩盖**越界
    ///   （D304）—— 值是错的，只是没报错。
    ///
    /// 三条都源于同一个结构缺陷：**`construct` 把指令从 IR 里丢出去，
    /// 再在降维时猜着放回来**。放进 `insts` 之后，顺序由位置保证、
    /// 寄存器由重命名保证、`n_regs` 由 `map_ssa` 的实际分配保证，
    /// 三条缺陷同时消失，不需要任何回填映射。
    ///
    /// SSA pass 对它一律**保守**：不可删除（[`crate::mir::opt`] 的 DCE）、
    /// 不可外提（LICM）、只做寄存器替换。
    Opaque(MirInst),
}

/// 基本块 terminator
#[derive(Debug, Clone)]
pub enum Terminator {
    Jump(BlockId),
    JumpIf(SsaReg, BlockId, BlockId),
    JumpIfNot(SsaReg, BlockId, BlockId),
    Return(Option<SsaReg>),
    Break(BlockId),
    Continue(BlockId),
    /// v0.104.6 D419：**跳到函数末尾**（`Jump(end)` 且 `end == body.len()`）。
    ///
    /// 它与 [`Terminator::Return`] 是**两件事**，此前被混成同一个：
    /// `split_into_ssa` 把越界跳转目标映成 `Return(None)`，而
    /// `deconstruct` 又把 `Return(None)` **丢弃**（「落下去，线性执行自然
    /// 进入下一块」）—— 于是那条跳转彻底消失，块与块之间再无控制转移。
    ///
    /// 实测（D313）：`if 1 > 0 / print(7) / else / print(8) / end`（`if` 是最后
    /// 一句）在 `--opt=1` 下**两个分支都执行** ⇒ 副作用重复（重复写文件、
    /// 重复扣款、重复发送），而程序看起来一切正常、退出码 0。
    ///
    /// 降维时它必须产出**真正的终止指令**（见 `deconstruct.rs`），而不是被
    /// 丢弃的 `Return(None)`。
    JumpExit,
    Unreachable,
}

// ── OptLevel ──

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OptLevel {
    None,
    Basic,
    Aggressive,
}

impl OptLevel {
    /// v0.75.30: CLI 显式编译选项 `--opt=N` 解析（与 `from_env` 共享 0/1/2
    /// 语义）。`mora --opt=1 file.mora` 显式指定优化等级；返回 `None` 表示
    /// 未指定（调用方走 env 兜底）。
    pub fn from_arg(value: &str) -> Option<OptLevel> {
        value.parse::<u32>().ok().map(|n| match n {
            0 => OptLevel::None,
            1 => OptLevel::Basic,
            _ => OptLevel::Aggressive,
        })
    }

    /// `MORA_OPT` 环境变量 → 优化等级（v0.75.7 引入的渐进式启用开关）：
    /// - 未设置 / `MORA_OPT=0` → `None`（默认）— 热路径零开销；
    ///   优化 pass（SSA rename/支配树）未证明对所有程序安全前，默认关闭
    ///   作可回退逃生舱（I5 约束：C2 手写 / 优化失败可回退）。
    /// - `MORA_OPT=1` → `Basic`（CP/CopyProp/DCE/GVN 基础管线）。
    /// - `>= 2` → `Aggressive`（叠加 LICM/LSR/TCO）。
    ///
    /// v0.75.30: 显式编译选项提升 — CLI `--opt=N` 优先（见
    /// `lower_mir_exprs_with_opt`），env 仅作未走编译命令入口（REPL/import/pregel）
    /// 的兜底。v1.0 演进：优化等级应成为编译命令的一等参数（类似 rustc
    /// `-O`），env 最终退役。
    pub fn from_env() -> OptLevel {
        std::env::var("MORA_OPT")
            .ok()
            .and_then(|v| OptLevel::from_arg(&v))
            .unwrap_or(OptLevel::None)
    }

    pub fn enabled(&self) -> bool {
        self != &OptLevel::None
    }

    pub fn aggressive(&self) -> bool {
        self == &OptLevel::Aggressive
    }
}

impl Default for OptLevel {
    fn default() -> Self {
        Self::from_env()
    }
}

// ── construct: MIR-plain → MIR-ssa ──

/// 将 MIR-plain 转为 SSA 形式（含支配树、phi 插入、重命名）
pub fn construct(func: &MirFunction) -> MirSsaFunction {
    if func.body.is_empty() {
        return MirSsaFunction {
            params: func
                .params
                .iter()
                .enumerate()
                .map(|(i, name)| (name.clone(), i))
                .collect(),
            blocks: vec![BasicBlock {
                id: 0,
                phis: Vec::new(),
                insts: Vec::new(),
                terminator: Terminator::Return(None),
                preds: Vec::new(),
                succs: Vec::new(),
            }],
            entry: 0,
            n_versions: func.params.len(),
        };
    }

    let label_to_pos = find_label_targets(&func.body);
    let block_starts = find_block_starts(&func.body, &label_to_pos);
    let bid_to_start = block_starts.clone();
    let start_to_bid: HashMap<usize, BlockId> = bid_to_start
        .iter()
        .enumerate()
        .map(|(bid, &start)| (start, bid))
        .collect();
    let label_to_bid: HashMap<usize, BlockId> = label_to_pos
        .iter()
        .map(|(&label_val, &pos)| (label_val, *start_to_bid.get(&pos).unwrap_or(&0)))
        .collect();

    let body_len = func.body.len();

    let num_blocks = bid_to_start.len();

    let mut blocks: Vec<BasicBlock> = (0..num_blocks)
        .map(|bid| {
            let start = bid_to_start[bid];
            let end = if bid + 1 < num_blocks {
                bid_to_start[bid + 1]
            } else {
                func.body.len()
            };
            let (insts, terminator) = split_into_ssa(
                &func.body[start..end],
                &label_to_bid,
                &start_to_bid,
                bid,
                body_len,
                num_blocks,
            );
            BasicBlock {
                id: bid,
                phis: Vec::new(),
                insts,
                terminator,
                preds: Vec::new(),
                succs: Vec::new(),
            }
        })
        .collect();

    for (bid, block) in blocks.iter_mut().enumerate() {
        let mut succs = Vec::new();
        match &block.terminator {
            Terminator::Jump(t) | Terminator::Break(t) | Terminator::Continue(t) => {
                if *t < num_blocks {
                    succs.push(*t);
                }
            }
            Terminator::JumpIf(_, tt, ft) | Terminator::JumpIfNot(_, tt, ft) => {
                if *tt < num_blocks {
                    succs.push(*tt);
                }
                if *ft < num_blocks {
                    succs.push(*ft);
                }
            }
            _ => {}
        }
        // v0.104.6 D312 根因之一：`Return(None)` 在 `deconstruct` 里的真实
        // 语义是「**落下去**」（该 terminator 被降维成 `Label(usize::MAX)`
        // 后跳过，线性执行自然进入下一块），但 CFG 侧把它当终点 ⇒ 顺序后继
        // 边缺失 ⇒ 后继块 `preds` 为空、遍历从 `vec![0]` 出发走不到 ⇒
        // **该块整块未被重命名**（部分重命名，把跨块的值定义与使用拆成两个
        // 物理寄存器；实测 `for` 循环里 r9 与 r19 永久失联，程序静默截断）。
        if matches!(block.terminator, Terminator::Return(None)) && bid + 1 < num_blocks {
            succs.push(bid + 1);
        }
        block.succs = succs;
    }
    let succs_by_bid: Vec<Vec<BlockId>> = blocks.iter().map(|b| b.succs.clone()).collect();

    // v0.104.6 D418：**只有可达块参与 CFG**。
    //
    // 判据是「从入口沿 succs 能到」—— 注意 D417 已经给 `Return(None)` 补了
    // 顺序后继边，所以 CFG 可达性此时与线性可达性一致（除被 `Jump` /
    // `Break` 跳过的块，那些是真死代码）。
    //
    // 为什么必须先切可达集：`compute_dominators` 用 `⋂_{p ∈ preds} Dom(p)`
    // 不动点。**死块**的 dom 集是 `{dead}`，它一旦出现在某个可达块的 preds 里，
    // 那个可达块的 dom 集就被交成空的 ⇒ 它的 idom 变成「无」⇒ 支配树在这里
    // 断开 ⇒ 后续整棵子树在**空栈**下被重命名 ⇒ 读解析全部落回原始编号。
    //
    // 实测症状（`loop_for_break.mora`，`--opt=1` 静默无输出）：`if i == 4 /
    // break / end` 之后 lowering 仍发射了一段不可达的 join 代码，它成为循环体
    // 块的一个前驱 ⇒ 循环体块的 dom 集被交空 ⇒ 循环归纳变量的
    // `__idx += 1` 读到了**从未被写入**的寄存器 ⇒ DAG 节点永不激活 ⇒ 饿死。
    let mut reachable = vec![false; num_blocks];
    let mut reach_stack = vec![0usize];
    while let Some(bid) = reach_stack.pop() {
        if reachable[bid] {
            continue;
        }
        reachable[bid] = true;
        for &succ in &succs_by_bid[bid] {
            if succ < num_blocks && !reachable[succ] {
                reach_stack.push(succ);
            }
        }
    }

    for (bid, succs_copy) in succs_by_bid.iter().enumerate() {
        if !reachable[bid] {
            // 死块：不进 CFG —— 清空 pred **与 succ**，让它自成一棵树
            // （重命名时栈为空）。succ 必须一起清：否则它的边会给**可达后继**
            // 的 phi 记上一条来自死块的 incoming，而那条 incoming 的源版本
            // 是在空栈下解析出来的 ⇒ 前驱 copy 去读一个没有生产者的寄存器
            // ⇒ DAG 节点永不激活 ⇒ 静默饿死。
            blocks[bid].preds.clear();
            blocks[bid].succs.clear();
            continue;
        }
        for &succ in succs_copy {
            if succ < num_blocks {
                blocks[succ].preds.push(bid);
            }
        }
    }
    for block in blocks.iter_mut() {
        if !reachable[block.id] {
            block.preds.clear();
            block.succs.clear();
        }
    }

    let idom = compute_dominators(&blocks);
    let dom_frontier = compute_dominance_frontier(&blocks, &idom);
    let defs = collect_definitions(&blocks);

    let mut phi_map: HashMap<(BlockId, SsaReg), Phi> = HashMap::new();
    insert_phi_nodes(&blocks, &dom_frontier, &defs, &mut phi_map);

    // v0.104.6 D417：phi **先装进块、再重命名** —— `rename_variables` 要把
    // fresh 版本号写回 `phi.dst`（此前只压栈不写回，phi 仍占原编号 ⇒ 与被
    // 重编号的普通指令撞号，D256 实测 6 处）。
    //
    // 装入顺序按原始 dst 升序：`phi_map` 是 `HashMap`，直接 `.iter()` 的
    // 顺序不确定 ⇒ SSA 编号不确定 ⇒ 同一份源码两次编译产出不同 MIR。
    let mut by_block: HashMap<BlockId, Vec<Phi>> = HashMap::new();
    for (&(bid, _), phi) in phi_map.iter() {
        by_block.entry(bid).or_default().push(phi.clone());
    }
    for v in by_block.values_mut() {
        v.sort_by_key(|p| p.var);
    }
    for block in blocks.iter_mut() {
        block.phis = by_block.remove(&block.id).unwrap_or_default();
    }

    let n_versions = rename_variables(&mut blocks, &idom, func.params.len(), func.n_regs);

    MirSsaFunction {
        params: func
            .params
            .iter()
            .enumerate()
            .map(|(i, name)| (name.clone(), i))
            .collect(),
        blocks,
        entry: 0,
        n_versions,
    }
}

fn find_label_targets(body: &[MirInst]) -> HashMap<usize, usize> {
    let mut out = HashMap::new();
    for (i, inst) in body.iter().enumerate() {
        if let MirInst::Label(lbl) = inst {
            out.insert(*lbl, i);
        }
    }
    out
}

fn find_block_starts(body: &[MirInst], label_to_pos: &HashMap<usize, usize>) -> Vec<usize> {
    let mut starts = HashSet::new();
    starts.insert(0);

    for &pos in label_to_pos.values() {
        starts.insert(pos);
    }

    let body_len = body.len();

    for (i, inst) in body.iter().enumerate() {
        let is_term = matches!(
            inst,
            MirInst::Return(_)
                | MirInst::Jump(_)
                | MirInst::JumpIf(_, _)
                | MirInst::JumpIfNot(_, _)
                | MirInst::Break(_)
                | MirInst::Continue(_)
        );
        if is_term && i + 1 < body_len {
            starts.insert(i + 1);
        }

        let lbls: Vec<usize> = match inst {
            MirInst::Jump(l)
            | MirInst::JumpIf(_, l)
            | MirInst::JumpIfNot(_, l)
            | MirInst::Break(l)
            | MirInst::Continue(l) => vec![*l],
            _ => vec![],
        };
        for lbl in lbls {
            // α.0 lowering 不 emit Label 指令：Jump/Break/Continue 的 label 即 body 索引
            // 直接将跳转目标位置作为基本块起点插入（注释："α.0 不 emit Label 指令（label 即索引）"）
            if lbl < body_len && lbl > 0 {
                starts.insert(lbl);
            } else if let Some(pos) = label_to_pos.get(&lbl) {
                starts.insert(*pos);
            }
        }
    }

    let mut starts: Vec<usize> = starts.drain().collect();
    starts.sort();
    starts
}

fn split_into_ssa(
    insts: &[MirInst],
    label_to_bid: &HashMap<usize, BlockId>,
    start_to_bid: &HashMap<usize, BlockId>,
    bid: BlockId,
    total_body_len: usize,
    num_blocks: BlockId,
) -> (Vec<SsaInst>, Terminator) {
    let mut ssa_insts = Vec::new();

    fn resolve_jump_target(
        target: usize,
        label_to_bid: &HashMap<usize, BlockId>,
        start_to_bid: &HashMap<usize, BlockId>,
        bid: BlockId,
        _is_jump_if_not: bool,
        total_body_len: usize,
    ) -> BlockId {
        // First try label lookup
        if let Some(&t) = label_to_bid.get(&target) {
            return t;
        }
        // Then try exact position lookup (target is a block start)
        if let Some(&t) = start_to_bid.get(&target) {
            return t;
        }
        // Target past end of body → return (no successor)
        if target >= total_body_len {
            return BlockId::MAX;
        }
        // Find the block that CONTAINS the target position
        if let Some(t) = start_to_bid
            .iter()
            .filter(|&(&p, _)| p <= target)
            .max_by_key(|&(&p, _)| p)
            .map(|(_, &b)| b)
        {
            return t;
        }
        // Fall back to current block
        bid
    }

    for inst in insts {
        match inst {
            MirInst::Return(r) => {
                return (ssa_insts, Terminator::Return(r.map(|r| r as SsaReg)));
            }
            MirInst::Halt(r) => {
                // v0.75: vote_to_halt 在 SSA 线性上下文中等价于 Return
                // （BSP 引擎的 Halt 语义只在 MirPregelEngine 里消费）。
                return (ssa_insts, Terminator::Return(r.map(|r| r as SsaReg)));
            }
            MirInst::Jump(l) => {
                let target =
                    resolve_jump_target(*l, label_to_bid, start_to_bid, bid, false, total_body_len);
                // v0.104.6 D419：跳到 body 之外 = **跳到函数末尾**，不是
                // 「返回」也不是「落进下一块」。见 `Terminator::JumpExit`。
                return if target == BlockId::MAX {
                    (ssa_insts, Terminator::JumpExit)
                } else {
                    (ssa_insts, Terminator::Jump(target))
                };
            }
            MirInst::JumpIf(cond, l) => {
                let true_t =
                    resolve_jump_target(*l, label_to_bid, start_to_bid, bid, false, total_body_len);
                let fall_through = if bid + 1 < num_blocks {
                    bid + 1
                } else {
                    BlockId::MAX
                };
                return if true_t == BlockId::MAX {
                    (
                        ssa_insts,
                        Terminator::JumpIf(*cond as SsaReg, fall_through, BlockId::MAX),
                    )
                } else {
                    (
                        ssa_insts,
                        Terminator::JumpIf(*cond as SsaReg, true_t, fall_through),
                    )
                };
            }
            MirInst::JumpIfNot(cond, l) => {
                let false_t =
                    resolve_jump_target(*l, label_to_bid, start_to_bid, bid, true, total_body_len);
                let fall_through = if bid + 1 < num_blocks {
                    bid + 1
                } else {
                    BlockId::MAX
                };
                return if false_t == BlockId::MAX {
                    (
                        ssa_insts,
                        Terminator::JumpIfNot(*cond as SsaReg, fall_through, BlockId::MAX),
                    )
                } else {
                    (
                        ssa_insts,
                        Terminator::JumpIfNot(*cond as SsaReg, fall_through, false_t),
                    )
                };
            }
            MirInst::Break(l) => {
                let target =
                    resolve_jump_target(*l, label_to_bid, start_to_bid, bid, false, total_body_len);
                return if target == BlockId::MAX {
                    (ssa_insts, Terminator::JumpExit)
                } else {
                    (ssa_insts, Terminator::Break(target))
                };
            }
            MirInst::Continue(l) => {
                let target =
                    resolve_jump_target(*l, label_to_bid, start_to_bid, bid, false, total_body_len);
                return if target == BlockId::MAX {
                    (ssa_insts, Terminator::JumpExit)
                } else {
                    (ssa_insts, Terminator::Continue(target))
                };
            }
            MirInst::Label(_) => continue,

            MirInst::Const(dst, v) => {
                ssa_insts.push(SsaInst::Const(*dst as SsaReg, v.clone()));
            }
            MirInst::Var(dst, name) => {
                ssa_insts.push(SsaInst::Var(*dst as SsaReg, name.clone()));
            }
            MirInst::Copy(dst, src) => {
                ssa_insts.push(SsaInst::Copy(*dst as SsaReg, *src as SsaReg));
            }
            MirInst::BinaryOp(dst, l, op, r) => {
                ssa_insts.push(SsaInst::BinaryOp(
                    *dst as SsaReg,
                    *l as SsaReg,
                    op.clone(),
                    *r as SsaReg,
                ));
            }
            MirInst::CallValue(dst, callee, args) => {
                ssa_insts.push(SsaInst::CallValue(
                    *dst as SsaReg,
                    *callee as SsaReg,
                    args.iter().map(|a| *a as SsaReg).collect(),
                ));
            }
            MirInst::Call(dst, callee, args) => {
                ssa_insts.push(SsaInst::Call(
                    *dst as SsaReg,
                    callee.clone(),
                    args.iter().map(|r| *r as SsaReg).collect(),
                ));
            }
            MirInst::ListLit(dst, items) => {
                ssa_insts.push(SsaInst::ListLit(
                    *dst as SsaReg,
                    items.iter().map(|r| *r as SsaReg).collect(),
                ));
            }
            MirInst::DictLit(dst, pairs) => {
                ssa_insts.push(SsaInst::DictLit(
                    *dst as SsaReg,
                    pairs
                        .iter()
                        .map(|(k, v)| (k.clone(), *v as SsaReg))
                        .collect(),
                ));
            }
            MirInst::Index(dst, obj, idx) => {
                ssa_insts.push(SsaInst::Index(
                    *dst as SsaReg,
                    *obj as SsaReg,
                    *idx as SsaReg,
                ));
            }
            MirInst::IndexAssign(dst, obj, idx) => {
                ssa_insts.push(SsaInst::IndexAssign(
                    *dst as SsaReg,
                    *obj as SsaReg,
                    *idx as SsaReg,
                ));
            }
            MirInst::MethodCall(dst, recv, method, args) => {
                ssa_insts.push(SsaInst::MethodCall(
                    *dst as SsaReg,
                    *recv as SsaReg,
                    method.clone(),
                    args.iter().map(|r| *r as SsaReg).collect(),
                ));
            }
            MirInst::Pipe(dst, lhs, rhs) => {
                ssa_insts.push(SsaInst::Pipe(
                    *dst as SsaReg,
                    *lhs as SsaReg,
                    *rhs as SsaReg,
                ));
            }
            MirInst::Prompt(dst, parts) => {
                ssa_insts.push(SsaInst::Prompt(
                    *dst as SsaReg,
                    parts.iter().map(|r| *r as SsaReg).collect(),
                ));
            }
            MirInst::Define(name, reg) => {
                ssa_insts.push(SsaInst::Define(name.clone(), *reg as SsaReg));
            }
            MirInst::Assign(name, reg) => {
                ssa_insts.push(SsaInst::Assign(name.clone(), *reg as SsaReg));
            }
            MirInst::Expr(reg) => {
                ssa_insts.push(SsaInst::Expr(*reg as SsaReg));
            }
            // v0.104.6 D417：SSA 不为其余变体建模语义，但**留在原位**、
            // 参与重命名。此前它们被丢出 IR 再由 deconstruct 前插回 body 头部
            // ⇒ 两套寄存器空间 + 顺序被打乱 + `n_regs` 靠 `max()` 掩盖。
            // 覆盖集合由 `MirInst` 的其余全部变体构成，编译器强制新变体
            // 必须在此显式表态（不会静默落入「丢弃」）。
            _ => ssa_insts.push(SsaInst::Opaque(inst.clone())),
        }
    }

    (ssa_insts, Terminator::Return(None))
}

// ── v0.104.6 D419：已删除的越界跳转守卫 ─────────────────────────────
//
// `has_out_of_range_jump` 曾守：body 里有**跳到 body 之外**的控制转移
// （`Jump(end)` 且 `end == body.len()`，`if` 是最后一句 + `IfSimplifyRule`
// 删掉恒不跳的 `JumpIfNot` 就会留下它）。
//
// 旧 SSA 把「越界跳转」映成 `Terminator::Return(None)`，而 `Return(None)` 在
// 降维时是**被丢弃**的（「落下去，线性执行自然进入下一块」）⇒ 那条跳转彻底消失
// ⇒ 块与块之间再无控制转移 ⇒ **两个分支都执行**。
//
// 现在有 [`Terminator::JumpExit`] 专门表达「跳到函数末尾」，它与「返回」/「落进
// 下一块」彻底分开，不再需要事后猜测。详见 `opt.rs` 里 D419 的说明注释与
// `tests/opt_out_of_range_jump_guard.rs`。
//

// ── v0.104.6 D417：已删除的 SSA 门 ─────────────────────────────────
//
// `has_register_carrying_passthrough` 曾守的缺陷是「一个函数内两套寄存器
// 空间」：`Closure` / `MatchExpr` / `WithConfig` / `Perform` / `Handle` /
// `Solve` / `DynTrait` / `Quasiquote` 的**写寄存器**不参与重编号，而同函数
// 其余指令会（D310 实测：闭包写 r8 而 `map` 读 r2）。后果是静默的 ——
// `function_call.mora` 在 `--opt=1/2` 下无输出、退出码 0。
//
// 当时的两个「掩盖」：`deconstruct` 的 `n_regs.max(orig_n_regs)`（撑大帧，
// **值是错的、只是没越界报错**）与 `opt::optimize` 的整体跳过（安全，但 57 个
// fixture 里 14 个拿不到优化）。D311 曾试图按 `input_regs()` 收窄判据，
// 实测是错的 —— 冲突来自**写**不是**读**。
//
// D417 把这些指令改成 [`SsaInst::Opaque`] **留在原位**参与同一条重命名路径
// （读经 `map_regs`、写分配 fresh 版本号），门随之删除。详见
// `SsaInst::Opaque` 的文档注释与 `tests/ssa_passthrough_remap.rs`。

/// 计算每个块的**立即支配者**（`idom`）。
///
/// ## 支配集是**极大**不动点，不是极小不动点
///
/// `Dom(b) = {b} ∪ ⋂_{p ∈ preds(b)} Dom(p)` 这个方程有多个解，**支配集是其中
/// 最大的那个**。因此迭代必须从 `Universe`（全集）**向下收缩**。
///
/// 本函数此前从**空集**起步（`preds.is_empty()` 的块才是 `{bid}`，其余为空）
/// —— 那算的是**极小**不动点，在含回边的 CFG 上它根本不是支配集。实测症状
/// （嵌套 `for`，`tests/ssa_phi_roundtrip.rs::d418_loop_shapes_survive_every_opt_level` 抓到）：
///
/// ```text
///   dom[0]={0}  dom[1]={1}  dom[2]={2,1}  dom[3]={3}  dom[4]={4,3}  dom[5]={5,3}
/// ```
///
/// 于是每个块的 idom 都成了**它自己** ⇒ 支配树碎成一堆单点 ⇒ 每棵树都在
/// **空栈**下被重命名 ⇒ 外层循环的归纳变量 `__idx += 1` 读到从未被写入的
/// 寄存器 ⇒ `--opt=1` 下内层循环只跑一半（`2,3` 而非 `2,3,3,4`）⇒
/// **静默少输出、退出码 0**。
///
/// ## 无前驱的块
///
/// `preds` 为空 ⇒ 没有任何块能到达它 ⇒ `Dom(b) = {b}`。这同时覆盖入口块与
/// 不可达块（`construct` 已把不可达块的 `preds`/`succs` 清空，让它们不进 CFG）。
#[allow(clippy::needless_range_loop)]
fn compute_dominators(blocks: &[BasicBlock]) -> Vec<Option<BlockId>> {
    let n = blocks.len();
    if n == 0 {
        return vec![];
    }

    // 从全集起步 ⇒ 迭代单调收缩 ⇒ 收敛到极大（支配）不动点
    let universe: HashSet<BlockId> = (0..n).collect();
    let mut dom_sets: Vec<HashSet<BlockId>> = vec![universe; n];

    // 轮转迭代至多 n 轮收敛（每轮至少定住一个块）；留 2 轮余量。
    let mut changed = true;
    let mut iter = 0usize;
    while changed && iter <= n + 2 {
        changed = false;
        iter += 1;
        for bid in 0..n {
            let preds = &blocks[bid].preds;
            let new_dom: HashSet<BlockId> = if preds.is_empty() {
                [bid].into_iter().collect()
            } else {
                let mut it = preds.iter().map(|&p| dom_sets[p].clone());
                let mut inter = it.next().expect("preds 非空");
                for s in it {
                    inter.retain(|x| s.contains(x));
                }
                inter.insert(bid);
                inter
            };
            if new_dom != dom_sets[bid] {
                dom_sets[bid] = new_dom;
                changed = true;
            }
        }
    }
    debug_assert!(
        !changed,
        "支配集迭代在 {} 轮后仍未收敛（块数 {}）—— CFG 可能有环之外的异常结构",
        iter, n
    );

    // 立即支配者 = Dom(b)\{b} 中支配集**最大**者（最靠近 b 的那个）
    let mut idom: Vec<Option<BlockId>> = vec![None; n];
    for bid in 0..n {
        let dom = &dom_sets[bid];
        if dom.len() <= 1 {
            idom[bid] = if dom.contains(&bid) { Some(bid) } else { None };
            continue;
        }

        let mut best: Option<BlockId> = None;
        let mut best_size = 0usize;

        for &d in dom {
            if d == bid {
                continue;
            }
            let d_size = dom_sets[d].len();
            if d_size < dom.len() && d_size > best_size {
                best_size = d_size;
                best = Some(d);
            }
        }

        idom[bid] = best;
    }

    idom
}

#[allow(clippy::needless_range_loop)]
fn compute_dominance_frontier(
    blocks: &[BasicBlock],
    idom: &[Option<BlockId>],
) -> Vec<Vec<BlockId>> {
    let n = blocks.len();
    let mut dom_frontier: Vec<Vec<BlockId>> = vec![Vec::new(); n];

    for bid in 0..n {
        let preds = &blocks[bid].preds;
        if preds.is_empty() {
            continue;
        }

        for &pred in preds {
            let mut runner = pred;
            loop {
                dom_frontier[runner].push(bid);
                if let Some(next) = idom[runner] {
                    if next == bid || next == runner {
                        break;
                    }
                    runner = next;
                } else {
                    break;
                }
            }
        }
    }

    for df in &mut dom_frontier {
        df.sort();
        df.dedup();
    }

    dom_frontier
}

fn collect_definitions(blocks: &[BasicBlock]) -> HashMap<SsaReg, Vec<BlockId>> {
    let mut defs: HashMap<SsaReg, Vec<BlockId>> = HashMap::new();
    for block in blocks {
        for inst in &block.insts {
            // `Define(name, reg)` / `Assign(name, reg)` 写的是 **env**，第二
            // 字段是被绑定的**源**寄存器 —— 它们**不定义**任何 SSA 寄存器
            // （v0.75.7 起 `rename_variables` 对二者也只 resolve 不重编号）。
            // 把它们算作定义会在本块凭空多出一条 def ⇒ phi 插入点被放大 ⇒
            // `opt.rs` 的「出现 phi 就跳过 SSA」守卫误伤本可优化的函数。
            if let SsaInst::Define(_, _) | SsaInst::Assign(_, _) = inst {
                continue;
            }
            // 无写的指令（声明型 Opaque：`TaskDef` / `Import` / …）不是值定义。
            if let Some(dst) = ssa_dst(inst) {
                defs.entry(dst).or_default().push(block.id);
            }
        }
    }
    defs
}

/// 指令定义的 SSA 寄存器；**不写任何寄存器**时为 `None`。
///
/// `Option` 不是可有可无的签名选择：声明型不透明指令（`TaskDef` / `Import` /
/// `RelDef` / TEA 定义 …）确实什么都不写，而它们现在也走同一条 SSA 指令流。
/// 若用哨兵值（如 `usize::MAX`）代替，`const_propagate` 里
/// `vec![None; max_dst + 1]` 会直接 OOM —— **静默的资源耗尽**正是本项目
/// 要消灭的失败模式之一。
pub fn ssa_dst(inst: &SsaInst) -> Option<SsaReg> {
    Some(match inst {
        SsaInst::Const(d, _)
        | SsaInst::Var(d, _)
        | SsaInst::BinaryOp(d, _, _, _)
        | SsaInst::Call(d, _, _)
        | SsaInst::CallValue(d, _, _)
        | SsaInst::ListLit(d, _)
        | SsaInst::DictLit(d, _)
        | SsaInst::Index(d, _, _)
        | SsaInst::IndexAssign(d, _, _)
        | SsaInst::MethodCall(d, _, _, _)
        | SsaInst::Pipe(d, _, _)
        | SsaInst::Prompt(d, _)
        | SsaInst::Copy(d, _)
        | SsaInst::Define(_, d)
        | SsaInst::Assign(_, d)
        | SsaInst::Expr(d) => *d,
        SsaInst::Opaque(m) => return m.written_reg(),
    })
}

fn insert_phi_nodes(
    _blocks: &[BasicBlock],
    dom_frontier: &[Vec<BlockId>],
    defs: &HashMap<SsaReg, Vec<BlockId>>,
    phi_map: &mut HashMap<(BlockId, SsaReg), Phi>,
) {
    let mut worklist: VecDeque<BlockId> = VecDeque::new();
    let mut visited: HashSet<BlockId> = HashSet::new();

    for block_ids in defs.values() {
        for &bid in block_ids {
            if !visited.contains(&bid) {
                worklist.push_back(bid);
            }
        }
    }

    while let Some(bid) = worklist.pop_front() {
        if visited.contains(&bid) {
            continue;
        }
        visited.insert(bid);

        let block_defs: Vec<SsaReg> = defs
            .iter()
            .filter(|(_, block_ids)| block_ids.contains(&bid))
            .map(|(&reg, _)| reg)
            .collect();

        for reg in block_defs {
            for &target in &dom_frontier[bid] {
                if let std::collections::hash_map::Entry::Vacant(e) = phi_map.entry((target, reg)) {
                    e.insert(Phi {
                        dst: reg,
                        var: reg,
                        incoming: Vec::new(),
                    });
                    if !visited.contains(&target) && !worklist.iter().any(|&x| x == target) {
                        worklist.push_back(target);
                    }
                }
            }
        }
    }
}

/// 变量重命名 —— **教科书式支配树 DFS**：进块压栈、出块弹栈，并沿每条
/// CFG 边为后继块的 phi 记录 incoming。
///
/// ## 为什么必须是支配树 DFS，而不是「沿 succs 的平铺工作表」
///
/// rename 的正确性依赖一条不变式：**读点看到的版本 = 支配该读点的定义**。
/// 沿 `succs` 的 BFS/DFS 只能保证「块被访问过」，不能保证「支配它的定义先进栈」。
/// 更致命的是**从不 pop**：一个变量的所有历史版本会一直堆在栈里，任何后来的
/// 读都解析到**最后一个**版本 —— 包括那些并不支配它的版本。
///
/// v0.104.6 D312 记录的三处缺口，本函数一次填掉两处：
/// - 遍历从 `vec![0]` 出发、只沿 `succs` ⇒ 走不到的块**整块未重命名**；
/// - 全程不 pop ⇒ 跨块的版本解析错乱。
///
/// 第三处（`phi.incoming` 恒空）在本函数里一并解决。
///
/// ## 算法
///
/// ```text
/// rename(b):
///   1. b 的每条 phi：new = fresh；push(var → new)；phi.dst = new
///   2. b 的每条指令：resolve 读；new = fresh；push(原 dst → new)；写回
///   3. 对每个后继 s 的每条 phi φ：φ.incoming += (b, 当前版本(φ.var))
///   4. 递归 rename(idom 树的每个子块)
///   5. 逆序 pop 第 1、2 步压入的每一项
/// ```
///
/// 步骤 3 必须在第 2 步**之后**（要的是块出口处可见的版本），且必须按
/// `phi.var`（原始编号）查栈 —— 回边指向的祖先/自身块的 `phi.dst` 早已被
/// 改写成新号。
fn rename_variables(
    blocks: &mut [BasicBlock],
    idom: &[Option<BlockId>],
    num_params: usize,
    orig_n_regs: usize,
) -> SsaReg {
    let n = blocks.len();
    if n == 0 {
        return num_params as SsaReg;
    }

    // 支配树：`idom[b] == Some(b)`（入口）与 `None`（无支配者）都是根。
    // 不可达块会自成一棵树 —— 它们同样必须被重命名，否则又是「部分重命名」。
    let mut children: Vec<Vec<BlockId>> = vec![Vec::new(); n];
    let mut roots: Vec<BlockId> = Vec::new();
    for b in 0..n {
        match idom.get(b).copied().flatten() {
            Some(d) if d != b && d < n => children[d].push(b),
            _ => roots.push(b),
        }
    }

    let mut reg_counter = num_params;
    // `orig_n_regs` 是原始 body 里任何寄存器引用的可靠上界（形参占 0..n_params，
    // 原始 body 中每个被引用的寄存器都 < 原始 n_regs）。取「所有写的最大值」
    // 是不够的：只被读、从未在本函数写的寄存器会让栈偏短，push 被静默跳过。
    let mut rename_stack: Vec<Vec<SsaReg>> = vec![Vec::new(); orig_n_regs.max(num_params).max(1)];

    // 显式栈而非递归：块数可到数千，递归会爆栈。
    // `Enter` 进块并压栈，`Exit` 出块并按逆序弹回该块压入的编号。
    enum Step {
        Enter(BlockId),
        Exit(Vec<usize>),
    }
    let mut work: Vec<Step> = roots.iter().rev().map(|&b| Step::Enter(b)).collect();
    let mut entered: Vec<bool> = vec![false; n];

    while let Some(step) = work.pop() {
        let (bid, mut pushed) = match step {
            Step::Enter(b) => {
                if entered[b] {
                    continue;
                }
                entered[b] = true;
                (b, Vec::new())
            }
            Step::Exit(pushed) => {
                for var in pushed.into_iter().rev() {
                    rename_stack[var]
                        .pop()
                        .expect("每个被 push 的编号此刻都还有一个版本在栈上");
                }
                continue;
            }
        };

        // ── 1. phi ────────────────────────────────────────────────
        // phi 先于块内指令取得版本号（D256：此前只压栈、不写回 `phi.dst`，
        // phi 仍占原编号 ⇒ 与被重编号的普通指令撞号）。
        for phi in &mut blocks[bid].phis {
            let new_dst = reg_counter;
            reg_counter += 1;
            if phi.var < rename_stack.len() {
                rename_stack[phi.var].push(new_dst);
                pushed.push(phi.var);
            }
            phi.dst = new_dst;
        }

        // ── 2. 块内指令 ──────────────────────────────────────────
        let succs: Vec<BlockId> = {
            let block = &mut blocks[bid];
            for inst in &mut block.insts {
                // `Define` / `Assign` 的第二字段是「被赋值值的来源寄存器」（读），
                // 不是 dst —— 只 resolve src，不重编号、不推栈（v0.75.7）。
                if let SsaInst::Define(_, _) | SsaInst::Assign(_, _) = inst {
                    rename_reads(inst, &rename_stack);
                    continue;
                }
                // 不透明指令与普通指令走同一条路径（D417）：读经 `map_regs`
                // 解析、写分配 fresh 版本号。
                if let SsaInst::Opaque(m) = inst {
                    let mapped = m.map_regs(&mut |r| resolve_reg(r, &rename_stack));
                    // **无写就不占版本号** —— 声明型指令什么都不写；给它分配
                    // 版本号会让 `reg_counter` 凭空前移，而 `deconstruct` 只按
                    // 真正出现的寄存器分配 plain 号 ⇒ `n_regs` 偏小 ⇒ 执行期
                    // 报「references register N but the function only has M」。
                    match mapped.written_reg() {
                        Some(old_dst) => {
                            let new_dst = reg_counter;
                            reg_counter += 1;
                            *inst = SsaInst::Opaque(mapped.with_written_reg(new_dst));
                            if old_dst < rename_stack.len() {
                                rename_stack[old_dst].push(new_dst);
                                pushed.push(old_dst);
                            }
                        }
                        None => *inst = SsaInst::Opaque(mapped),
                    }
                    continue;
                }
                rename_reads(inst, &rename_stack);
                let old_dst = ssa_dst(inst).expect("非 Opaque 指令恒有定义寄存器");
                let new_dst = reg_counter;
                reg_counter += 1;
                set_dst(inst, new_dst);
                if old_dst < rename_stack.len() {
                    rename_stack[old_dst].push(new_dst);
                    pushed.push(old_dst);
                }
            }
            match &mut block.terminator {
                Terminator::JumpIf(cond, _, _) | Terminator::JumpIfNot(cond, _, _) => {
                    *cond = resolve_reg(*cond, &rename_stack);
                }
                Terminator::Return(Some(reg)) => *reg = resolve_reg(*reg, &rename_stack),
                _ => {}
            }
            block.succs.clone()
        };

        // ── 3. 为后继块的 phi 记录本块的 incoming ────────────────
        for &s in &succs {
            if s >= n {
                continue;
            }
            for phi in &mut blocks[s].phis {
                let src = resolve_reg(phi.var, &rename_stack);
                // 同一对 (前驱, phi) 只记一次：`JumpIf(c, H, H)` 这类同前驱
                // 双边会重复走到；不去重会让 `deconstruct` 发两条同样的 copy。
                if !phi.incoming.iter().any(|&(p, _)| p == bid) {
                    phi.incoming.push((bid, src));
                }
            }
        }

        // ── 4 / 5. 先安排退出（弹栈），再压子块（深度优先）──────
        work.push(Step::Exit(pushed));
        for &c in children[bid].iter().rev() {
            work.push(Step::Enter(c));
        }
    }
    reg_counter as SsaReg
}

fn set_dst(inst: &mut SsaInst, d: SsaReg) {
    match inst {
        SsaInst::Const(dst, _) => *dst = d,
        SsaInst::Var(dst, _) => *dst = d,
        SsaInst::BinaryOp(dst, _, _, _) => *dst = d,
        SsaInst::Call(dst, _, _) => *dst = d,
        SsaInst::ListLit(dst, _) => *dst = d,
        SsaInst::DictLit(dst, _) => *dst = d,
        SsaInst::Index(dst, _, _) => *dst = d,
        SsaInst::IndexAssign(dst, _, _) => *dst = d,
        SsaInst::MethodCall(dst, _, _, _) => *dst = d,
        SsaInst::Pipe(dst, _, _) => *dst = d,
        SsaInst::Prompt(dst, _) => *dst = d,
        SsaInst::Copy(dst, _) => *dst = d,
        SsaInst::CallValue(dst, _, _) => *dst = d,
        SsaInst::Define(_, dst) => *dst = d,
        SsaInst::Assign(_, dst) => *dst = d,
        SsaInst::Expr(dst) => *dst = d,
        SsaInst::Opaque(m) => {
            *m = m.with_written_reg(d);
        }
    }
}

/// 把原始寄存器号解析为**当前可见的 SSA 版本**。
///
/// 栈空（形参、从未在本函数被定义的寄存器）时返回原号 —— 形参与外部
/// 寄存器共用编号空间，这是 SSA 构造的既定约定（`deconstruct` 把
/// `ssa.params` 映回 `0..n_params`）。
fn resolve_reg(reg: SsaReg, stack: &[Vec<SsaReg>]) -> SsaReg {
    if reg < stack.len() && !stack[reg].is_empty() {
        *stack[reg].last().expect("is_empty checked above")
    } else {
        reg
    }
}

fn rename_reads(inst: &mut SsaInst, stack: &[Vec<SsaReg>]) {
    use resolve_reg as resolve;

    match inst {
        SsaInst::BinaryOp(_, l, _, r) => {
            *l = resolve(*l, stack);
            *r = resolve(*r, stack);
        }
        SsaInst::Call(_, _, args) => {
            for r in args.iter_mut() {
                *r = resolve(*r, stack);
            }
        }
        // 值调用：被调者寄存器与实参同样需重命名（它们都是读取点）。
        SsaInst::CallValue(_, callee, args) => {
            *callee = resolve(*callee, stack);
            for r in args.iter_mut() {
                *r = resolve(*r, stack);
            }
        }
        SsaInst::ListLit(_, items) => {
            for r in items.iter_mut() {
                *r = resolve(*r, stack);
            }
        }
        SsaInst::DictLit(_, pairs) => {
            for (_, v) in pairs.iter_mut() {
                *v = resolve(*v, stack);
            }
        }
        SsaInst::Index(_, obj, idx) => {
            *obj = resolve(*obj, stack);
            *idx = resolve(*idx, stack);
        }
        SsaInst::IndexAssign(_, obj, idx) => {
            *obj = resolve(*obj, stack);
            *idx = resolve(*idx, stack);
        }
        SsaInst::MethodCall(_, recv, _, args) => {
            *recv = resolve(*recv, stack);
            for r in args.iter_mut() {
                *r = resolve(*r, stack);
            }
        }
        SsaInst::Pipe(_, lhs, rhs) => {
            *lhs = resolve(*lhs, stack);
            *rhs = resolve(*rhs, stack);
        }
        SsaInst::Prompt(_, parts) => {
            for r in parts.iter_mut() {
                *r = resolve(*r, stack);
            }
        }
        SsaInst::Copy(_, src) => *src = resolve(*src, stack),
        SsaInst::Assign(_, src) => *src = resolve(*src, stack),
        SsaInst::Expr(src) => *src = resolve(*src, stack),
        // v0.75.7: Define 的 src 也是寄存器读，参与 rename（此前跳过 →
        // deconstruct 映射错乱）。与 Assign 一致。
        SsaInst::Define(_, src) => *src = resolve(*src, stack),
        // Opaque 的读由 `rename_variables` 走 `MirInst::map_regs` 处理 ——
        // 不透明指令的寄存器位置只有 `MirInst` 知道。
        SsaInst::Opaque(_) => {}
        SsaInst::Var(_, _) | SsaInst::Const(_, _) => {}
    }
}

// ── deconstruct: MIR-ssa → MIR-plain ──

/// 将 SSA 函数转回 MIR-plain（phi → copy 指令，BlockId → Label）。
///
/// 关键策略：
/// - 每个 phi 节点分配一个共享 MIR-plain 寄存器 dst_p
/// - 每个前驱块在 terminator 之前把 src 复制到 dst_p
/// - 当前块不再读 phi，直接从 dst_p 读（已经是最终值）
/// - 无 incoming 的 phi → 块开头 emit Const(Nil) 到 dst_p（默认值）
/// - 第二遍补丁：Jump 目标从 BlockId → body 索引
///
mod deconstruct; // v0.75.58: SSA deconstruct（phi → copy，SSA → MIR-plain）
pub use deconstruct::deconstruct; // 保持 ssa::deconstruct 路径
