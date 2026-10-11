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

    // v0.104.6 D310/D311/D417：这里曾有 `has_register_carrying_passthrough`
    // 守卫 —— 含 `Closure` / `MatchExpr` / `WithConfig` / `Perform` / `Handle` …
    // 的函数**整体跳过 SSA**。D417 已把守卫删除，理由不是「放宽判据」，
    // 而是它守的缺陷**不存在了**：
    //
    //     旧：construct 把这些指令丢出 IR（原样保留旧寄存器号），
    //         同函数其余指令重编号 ⇒ 两套寄存器空间
    //         （实测 opt=1：`Closure{dst:8}` 而 `map` 读 r2；
    //           `print([1,2,3].map(fn(v) v + 10))` **静默无输出、退出码 0**）。
    //     新：它们作为 `SsaInst::Opaque` **留在原位**参与同一条重命名路径
    //         （读经 `map_regs`、写分配 fresh 版本号）。
    //
    // 守卫当时只能「跳过」，因为跳过的前提是「不改也不会更坏」—— 实际相反：
    // 跳过意味着 57 个 fixture 里 14 个永远拿不到 SSA 优化，而真正的修复
    // （把不透明指令纳入同一条版本化路径）当时被判断为「架构级改造」而搁置。
    //
    // 回归护栏：`tests/ssa_passthrough_remap.rs`
    //   ① SSA 函数内任一寄存器不得被两条指令同时定义；
    //   ② 每个被读的寄存器都有生产者（否则执行期读到未初始化槽 = 静默错值）；
    //   ③ 57 个 fixture × `--opt=0/1/2` 输出逐字一致。

    // v0.104.6 D419：**越界跳转守卫已删除** —— 它守的缺陷不存在了。
    //
    // 守卫守的是：`IfSimplifyRule` 删掉恒不跳的 `JumpIfNot` 后留下 `Jump(end)`，
    // 而 `if` 是最后一句时 `end == body.len()`（**跳转目标越界**）。旧 SSA 把
    // 「越界跳转」映成 `Terminator::Return(None)`，而 `Return(None)` 在降维时
    // 是**被丢弃**的（「落下去，线性执行自然进入下一块」）⇒ 那条跳转彻底消失
    // ⇒ 块与块之间再无控制转移 ⇒ **两个分支都执行**。
    // 副作用会**重复发生**（重复写文件、重复扣款、重复发送），程序看起来还在
    // 正常工作、退出码 0 —— 比静默截断更危险。
    //
    // D419 的修法：新增 [`crate::mir::ssa::Terminator::JumpExit`]（「跳到函数
    // 末尾」），与「返回」/「落进下一块」彻底分开；降维时发一条真跳转，由第四遍
    // 补成重建后 body 的长度。`run_mir` 见 `pc >= body.len()` 即结束并走隐式
    // 返回；`dag_analyze` 解析不到目标节点 ⇒ 该 Jump 节点无出边。两个执行器都
    // 与 opt=off 一致。
    //
    // 守卫当时只能「跳过」，是因为跳过至少不改变行为；真正的修法要求 SSA 能
    // **表达**「跳到函数末尾」这件事。
    //
    // 回归护栏：`tests/opt_out_of_range_jump_guard.rs`（已翻转成正确行为断言）、
    // `tests/ssa_phi_roundtrip.rs`、57×3 的 `--opt` 等价差分
    // （`scripts/opt_levels_snapshot.py`）。

    // SSA 构造
    let mut ssa = crate::mir::ssa::construct(func);

    // v0.104.6 D418：**phi 守卫已删除** —— `rename_variables` 现在是
    // 教科书式支配树 DFS（进块压栈 / 出块弹栈 / 逐边记录 `phi.incoming`），
    // phi 机制首次真正生效（此前 `insert_phi_nodes` 以
    // `incoming: Vec::new()` 建、而 `rename_variables` 收不可变引用且内部
    // `clone()` ⇒ 全函数**无处可写** ⇒ `deconstruct` 的 `pred_copies`
    // 唯一来源恒空 ⇒ 零条回边 copy）。
    //
    // D312 记的两条根因现状：
    //   ① 遍历只沿 `succs`、从不 pop —— 已由 DFS + push/pop 取代；
    //   ② `phi.incoming` 恒空 —— 已由 `Phi::var` + 步骤 3 的逐边记录取代。
    //
    // 守卫当时只能「跳过」是因为 phi 一旦出现就**必然产出错误结果**
    // （D312 实测：`for` 循环 `--opt=1` 静默中止，只输出循环前的行）。
    // 现在 phi 目标寄存器有了生产者，前驱 copy 也真的发射了。
    //
    // 回归护栏：`tests/ssa_phi_incoming.rs`（incoming 覆盖每个前驱 /
    // 定义不撞号）、`tests/ssa_passthrough_remap.rs`（每个读都有生产者）、
    // `tests/opt_repair_acceptance.rs` 与 57×3 的 `--opt` 等价差分。

    // ── SSA 主路径至此**无门**（v0.104.6 D420）────────────────────────
    //
    // 三道门按时间顺序全部删除：
    // | 门 | 删除于 | 它守的缺陷的真正修法 |
    // |---|---|---|
    // | `has_register_carrying_passthrough` | D417 | 透传指令改为 `SsaInst::Opaque`，原位参与重命名 |
    // | `has_out_of_range_jump` | D419 | 新增 `Terminator::JumpExit`（「跳到函数末尾」） |
    // | 「出现 phi 就跳过」 | D420 | 支配树 DFS + `phi.incoming` 填充（D418）+ `SsaInst::Copy` 直接降为 `MirInst::Copy`（本条） |
    //
    // 本条（最后一道门的最后一块绊脚石）：`SsaInst::Copy` 此前降维成
    // `Assign(".tmp_N", src)` + `Var(dst, ".tmp_N")`，走 env 中转；而
    // `Environment::assign` 对**尚未绑定**的名字返回 false 且**什么都不写**
    // （`value.rs` 的 `assign`）。`.tmp_N` 从未被 define ⇒ 写入被静默丢弃
    // ⇒ `Var` 读回 Nil。
    //
    // ⇒ 凡是经过 SSA 的 `Copy`（`if` 汇合值、phi copy）都**静默丢值**：
    // ```text
    //   let x = if 1 > 0 / 7 / else / 8 / end
    //   print(x)        opt=0 → 7.0      opt=1 → nil
    // ```
    // 未经 SSA 的程序里 `.tmp_N` 同样读不到，但那里的值本来就是 Nil
    // （`let` 的 `Assign("__let_result", Nil)`），于是缺陷被 env 往返本身掩盖。
    //
    // 修法：`SsaInst::Copy(dst, src)` 直接降为 `MirInst::Copy(dst, src)`
    // —— 精确的寄存器语义，无 env 往返、无临时名污染、少两条指令。
    // phi 前驱 copy 早在 D418 就已是这个形式，两条路径现在一致。
    //
    // 回归护栏：`tests/ssa_passthrough_remap.rs`（57×3 差分 + 依赖同构）、
    // `tests/ssa_phi_roundtrip.rs`、`tests/opt_repair_acceptance.rs`、
    // `tests/opt_out_of_range_jump_guard.rs`。

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
