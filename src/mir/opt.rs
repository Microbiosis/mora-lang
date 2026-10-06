//! SSA 优化 pass（α.3 + α.4 + α.5 + α.6）
//!
//! 基础优化（α.3）：常量传播 (CP)、死代码消除 (DCE)、全局值编号 (GVN)。
//! 中高级（α.4）：拷贝传播 (Copy Propagation)。
//! 激进优化（α.5）：循环不变量外提 (LICM)。
//! 激进优化（α.6）：循环强度缩减 (Loop Strength Reduction)。
//! 激进优化（α.7）：尾递归优化 (Tail Call Optimization)。
//!
//! v0.58 Phase H.8: SSA passes 可组合 — 每个 pass 实现 `SsaPass` trait，
//! 通过 `default_pipeline()` 返回内置 pass 序列。
//!
//! 约束：C2 手写 / I5 可回退（MORA_OPT=0 跳过）

use std::collections::HashSet;

use crate::mir::ssa::{BlockId, MirSsaFunction, SsaInst, SsaReg, Terminator};

// v0.75.60: pass 实现按组拆分子模块（simple/loops/copy/tailcall/pregel_opt）
mod copy;
mod loops;
mod pregel_opt;
mod simple;
mod tailcall;
use copy::copy_propagate;
use loops::{loop_invariant_motion, loop_strength_reduction};
pub use pregel_opt::{optimize_pregel, superstep_fusion};
use simple::{const_propagate, dead_code_elim, global_value_numbering};
use tailcall::tail_call_optimize;

type LicmpOps = Vec<(BlockId, Vec<(SsaReg, SsaInst)>, HashSet<BlockId>)>;

/// v0.58: SSA 优化 pass trait — 每个 pass 是一个独立的变换单元。
///
/// 设计哲学：每个 SsaPass 类似 Cascades 的一条 RewriteRule，
/// 但在 SSA 层操作的是整个 MirSsaFunction（而非单条 MirInst）。
/// 这样可以保留 SSA 层的优化自由度（块内扫描、跨块分析等），
/// 同时享受 Cascades 的"可组合 pass 管线"架构。
pub trait SsaPass {
    /// Pass 名称（用于日志/调试）
    fn name(&self) -> &'static str;

    /// 应用该 pass 到 SSA 函数。返回 true 表示函数被修改。
    fn run(&self, ssa: &mut MirSsaFunction) -> bool;
}

/// 常量传播 pass
pub struct ConstPropPass;

/// 拷贝传播 pass
pub struct CopyPropPass;

/// 死代码消除 pass
pub struct DeadCodeElimPass;

/// 全局值编号 pass（局部 CSE）
pub struct GvnPass;

/// 循环不变量外提 pass（仅 Aggressive）
pub struct LicmPass;

/// 循环强度缩减 pass（仅 Aggressive）
pub struct LoopStrengthReductionPass;

/// 尾调用优化 pass（仅 Aggressive）
pub struct TailCallOptPass;

impl SsaPass for ConstPropPass {
    fn name(&self) -> &'static str {
        "const_prop"
    }
    fn run(&self, ssa: &mut MirSsaFunction) -> bool {
        let before = count_instructions(ssa);
        const_propagate(ssa);
        count_instructions(ssa) != before
    }
}

impl SsaPass for CopyPropPass {
    fn name(&self) -> &'static str {
        "copy_prop"
    }
    fn run(&self, ssa: &mut MirSsaFunction) -> bool {
        let before = count_instructions(ssa);
        copy_propagate(ssa);
        count_instructions(ssa) != before
    }
}

impl SsaPass for DeadCodeElimPass {
    fn name(&self) -> &'static str {
        "dce"
    }
    fn run(&self, ssa: &mut MirSsaFunction) -> bool {
        let before = count_instructions(ssa);
        dead_code_elim(ssa);
        count_instructions(ssa) != before
    }
}

impl SsaPass for GvnPass {
    fn name(&self) -> &'static str {
        "gvn"
    }
    fn run(&self, ssa: &mut MirSsaFunction) -> bool {
        let before = count_instructions(ssa);
        global_value_numbering(ssa);
        count_instructions(ssa) != before
    }
}

impl SsaPass for LicmPass {
    fn name(&self) -> &'static str {
        "licm"
    }
    fn run(&self, ssa: &mut MirSsaFunction) -> bool {
        let before = count_instructions(ssa);
        loop_invariant_motion(ssa);
        count_instructions(ssa) != before
    }
}

impl SsaPass for LoopStrengthReductionPass {
    fn name(&self) -> &'static str {
        "loop_strength_reduction"
    }
    fn run(&self, ssa: &mut MirSsaFunction) -> bool {
        let before = count_instructions(ssa);
        loop_strength_reduction(ssa);
        count_instructions(ssa) != before
    }
}

impl SsaPass for TailCallOptPass {
    fn name(&self) -> &'static str {
        "tail_call_opt"
    }
    fn run(&self, ssa: &mut MirSsaFunction) -> bool {
        let before = count_instructions(ssa);
        tail_call_optimize(ssa);
        count_instructions(ssa) != before
    }
}

/// 计数 SSA 函数中所有指令（用于变更检测）
fn count_instructions(ssa: &MirSsaFunction) -> usize {
    ssa.blocks.iter().map(|b| b.insts.len()).sum()
}

/// 返回默认的基础 pass 管线
pub fn default_basic_pipeline() -> Vec<Box<dyn SsaPass>> {
    vec![
        Box::new(ConstPropPass),
        Box::new(CopyPropPass),
        Box::new(DeadCodeElimPass),
        Box::new(GvnPass),
    ]
}

/// 返回默认的激进 pass 管线（在基础之上）
pub fn default_aggressive_pipeline() -> Vec<Box<dyn SsaPass>> {
    vec![
        Box::new(LicmPass),
        Box::new(LoopStrengthReductionPass),
        Box::new(TailCallOptPass),
    ]
}

/// 对 MIR-plain 函数执行优化 pass
///
/// level == None → 跳过（直接跑 MIR-plain）
/// level == Basic → SSA 构造 + CP + DCE + GVN + CopyProp
/// level == Aggressive → +LICM + LoopStrengthReduction + TailCallOpt
pub fn optimize(func: &mut crate::mir::MirFunction, level: crate::mir::ssa::OptLevel) {
    if !level.enabled() {
        return;
    }

    // v0.104.6 D310：含**透传指令**的函数**整体跳过 SSA**。
    //
    // 缺陷背景（实测，MIR 逐条对照）：
    //     opt=off:  Closure { dst: 8 }   MethodCall(9, 4, "map", [8])   实参 8 = 闭包 ✅
    //     opt=1:    Closure { dst: 8 }   MethodCall(5, 1, "map", [2])   实参 2 ≠ 8   ❌
    // `Closure` / `MatchExpr` / `WithConfig` / `Perform` / `Handle` … 在
    // `ssa::is_ssa_passthrough` 里 —— `construct` **原样保留**它们的寄存器，
    // 而同一个函数里其余指令被 SSA **重编号**（8→2）。于是**一个函数内出现
    // 两套寄存器空间**：闭包写 r8、`map` 却去读 r2。
    //
    // 后果：`print([1,2,3].map(fn(v) v + 10))` 在 `--opt=1` 下
    // **静默无输出、退出码 0、零诊断**（`match` 同理，`Call` 实参 7→0）。
    // opt=off 正常，因为根本没有重编号这回事。
    //
    // 为什么不「把透传指令也一起重编号」：那要求 `construct` 递归进
    // `Closure.body` / `MatchExpr` 各臂体 / `WithConfig.body` 这些
    // **独立 MirFunction** 的寄存器平面，是一次架构级改造。
    // 本轮取保守方向：**宁可不优化，不可静默产出错误结果** ——
    // 与 D302 停用 `ReplaceWithSource`、D170「宁可记档也不要引入新的静默
    // 错误源」是同一条原则。代价仅是这类函数失去 SSA 优化。
    //
    // v0.104.6 D311 收窄：只有**带寄存器**的透传指令才跨寄存器平面。
    // 宽判据（任何 passthrough）实测命中 48/56（85.7%），等于把 `--opt` 废掉。
    if crate::mir::ssa::has_register_carrying_passthrough(func) {
        return;
    }

    // v0.104.6 D313：body 里有**跳到 body 之外**的控制转移 ⇒ 整体跳过 SSA。
    //
    // 现象（实测，`if 1 > 0 / print(7) / else / print(8) / end`，`if` 是最后一句）：
    //     opt=off  → ["7.0"]            exit 0
    //     opt=1/2  → ["7.0", "8.0"]     exit 0   ← **两个分支都跑**
    //
    // `apply_rules`（两档都跑，`cli/mod.rs:56`）里 `IfSimplifyRule` 删掉
    // `JumpIfNot`，只留下 `Jump(end)`，而 `end` 已越过 body 末尾：
    //     6  Jump(10)              ← body 只有 0..9
    //     7  Const(7, 8.0)         ← else 分支紧跟其后
    // 于是 `construct` 不在那里起块（`lbl < body_len` 为假）⇒ CFG 断成两块；
    // `deconstruct` 又把该 terminal 跳转映成**被丢弃**的 `Return(None)`
    // ⇒ 第四遍线性拼接时两个分支之间再无控制转移 ⇒ 两段都被执行。
    //
    // **副作用会重复发生**（重复写文件、重复扣款、重复发送），程序看起来
    // 还在正常工作 —— 比 D312 那种静默截断更危险。
    //
    // 细节与「为什么不用 `BasicBlock.preds` 当判据」见
    // `ssa::has_out_of_range_jump` 的文档注释。实测 56 个真实 `.mora`
    // （探针置于**所有 early return 之前**，避免提前返回那批不打印的
    // 选择偏差）：narrow 15/56、phi 7/56、**oor 0/56** ⇒ 本守卫对现有程序
    // **零代价**，纯兜这个合成形状。三个守卫并集 22/56，34/56 照常走 SSA。
    if crate::mir::ssa::has_out_of_range_jump(func) {
        return;
    }

    // SSA 构造
    let mut ssa = crate::mir::ssa::construct(func);

    // v0.104.6 D312：需要 phi 的函数**整体跳过 SSA**（`construct` 之后、
    // `deconstruct` 之前直接 return，`func` 保持原样逐字节不变）。
    //
    // 缺陷背景（实测，逐条转储 construct 前后 + SSA 中间态）：
    //
    //     源 MIR（MIR-plain，物理寄存器）
    //       11  Const(20, Int(0))            ← for 循环的 __idx 初始化
    //       22  BinaryOp(20, 20, Add, 22)   ← 循环回边上的 __idx 自增
    //     **同一个物理寄存器 20**
    //
    //     construct 之后
    //       block 0 succs=[]  ← 缺陷核心，见下
    //         Const(9,  Int(0))            ← 被重命名 20→9
    //       block 1/2 succs=[…]  未被访问，**寄存器号原封不动**
    //         BinaryOp(23, 20, GreaterEqual, 21)
    //         BinaryOp(20, 20, Add, 22)     ← 仍然用 20
    //
    //     deconstruct 之后
    //         Const(9,  Int(0))            ← 初始化值躺在 r9
    //         BinaryOp(18, 19, GreaterEqual, 1)   ← 循环读 r19
    //         BinaryOp(19, 19, Add, 12)           ← 回边写 r19
    //     **r9 与 r19 永久失联**：循环体读的那个寄存器首次迭代时**无任何
    //     生产者**。DAG 执行器按「输入寄存器就绪」激活节点，于是该节点
    //     永不激活 → 整条链饿死 → 程序静默截断、退出码 0、零诊断。
    //
    //     实测（`for` 最小用例）：
    //       opt=off            → ["A", "B", "6.0"]   exit 0
    //       opt=1 / opt=2      → ["A"]                exit 0   ← 静默中止
    //
    // **根因有两层，都比「少一个优化」严重：**
    //
    // 1. `construct` 的 CFG 不记**顺序落下的后继**。block 0 的 terminator 是
    //    `Return(None)`，而 `deconstruct`（`deconstruct.rs:296-300`）明写
    //    「`Return(None)` 不发射，丢弃后线性执行自然落到最后一条指令」——
    //    即 `Return(None)` 的真实语义是**落下去**、不是返回。但 CFG 侧
    //    把它当成终点，`succs` 为空。`rename_variables` 从 `vec![0]` 起
    //    只沿 `succs` 走且 `visited` 只入一次 ⇒ **只有 block 0 被重命名**，
    //    块 1/2/3 原封不动。**部分重命名**于是把跨块的值定义与使用拆成
    //    两个物理寄存器。
    //
    // 2. `phi.incoming` **结构上恒为空**。`insert_phi_nodes` 以
    //    `incoming: Vec::new()` 建 phi；`rename_variables` 的签名收的是
    //    `phi_map: &HashMap<…>`（**不可变引用**），且 `block_phi_map` 是
    //    `phi.incoming.clone()` 的副本 —— 无处可写。而 `deconstruct` 的
    //    `pred_copies` **唯一来源**就是 `phi.incoming`（`deconstruct.rs:145`）
    //    ⇒ phi 的前驱 copy **一条都不会发出**。实测 `TOTAL_PHI=7
    //    TOTAL_INCOMING=0`，且 deconstruct 产物里零条 `Copy` ——
    //    **整个 phi 机制从未生效过**。
    //
    // 为什么不在这里「把 SSA 修对」：第 2 层要求实现教科书式的
    // **支配树 DFS + 进出栈 push/pop + 逐前驱边记录 incoming**，
    // 而 `rename_variables` 现在是「平铺工作表 + 全局 visited 集合」，
    // 两者不是同一个算法。在一个会**静默产出错误结果**的路径上做这种
    // 改写，风险远大于收益 —— 与 D302 停用 `ReplaceWithSource`、
    // D310 跳过透传函数同一原则：**宁可不优化，不可静默产出错误结果**。
    //
    // 守卫取「SSA 里出现 phi」而不是「CFG 有回边」：前者**更窄**。
    // 无循环携带值的循环（`while true do … end` 一类）不产生 phi，
    // 仍能享受 SSA 优化。
    //
    // 实测代价（56 个真实 `.mora`，`--opt=1`，临时探针在**两个守卫之前**
    // 统计，否则提前 return 的那批不打印、true 恒为 0 —— 选择偏差）：
    //   宽判据（任何 passthrough）      48/56 = 85.7%
    //   窄判据（带寄存器的 passthrough）  15/56 = 26.8%
    //   本守卫（出现 phi）                7/56 = 12.5%
    // 两个守卫的**并集** 21/56 ⇒ 仍有 **35/56** 照常走 SSA。
    //
    // 代价：含真正循环携带值的函数失去 SSA 优化。这是**已知且有界的**
    // 代价（`--opt` 本就默认关闭，见 `ssa.rs:117-122`），换来的是
    // 「opt 档位要么等价、要么不优化」这条可验证的性质。
    if ssa.blocks.iter().any(|b| !b.phis.is_empty()) {
        return;
    }

    // 基础管线（迭代至收敛）
    if level.enabled() {
        run_pipeline(&mut ssa, &default_basic_pipeline());
    }

    // 激进管线
    if level.aggressive() {
        run_pipeline(&mut ssa, &default_aggressive_pipeline());
    }

    // Deconstruct: SSA → MIR-plain
    *func = crate::mir::ssa::deconstruct(&ssa);
}

/// 在 SSA 函数上运行一组 pass，迭代直到收敛（fixed point）
pub fn run_pipeline(ssa: &mut MirSsaFunction, passes: &[Box<dyn SsaPass>]) {
    loop {
        let mut any_change = false;
        for pass in passes {
            if pass.run(ssa) {
                any_change = true;
            }
        }
        if !any_change {
            break;
        }
    }
}
