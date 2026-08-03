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

    // SSA 构造
    let mut ssa = crate::mir::ssa::construct(func);

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
