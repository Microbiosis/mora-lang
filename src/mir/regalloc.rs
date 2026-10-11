//! v0.104.6：寄存器分配器 —— 槽位复用（linear-scan / 活跃区间着色）。
//!
//! ## 为什么需要
//!
//! 此前 `EmitContext::alloc_reg`（`mir/lower.rs`）是**单调自增计数器**：
//! 每个值一个独占槽位，**用完不回收**。于是 `n_regs == 指令数` 量级，而
//! 实际同时活跃的值远少于此 —— 实测（e2e fixture，活跃变量峰值分析）：
//!
//! | fixture | 指令数 | 原`n_regs` | 同时活跃峰值 |
//! |---|---|---|---|
//! | `for_loop`      | 30 | 21 | 6 |
//! | `handle_effect` |  7 |  5 | 1 |
//! | `match_guard`   | 45 | 32 | 4 |
//!
//! 解释器每条指令都在 21/32 槽的 `Vec<Value>` 上跑，峰值只需 6/4。
//!
//! ## 算法：活跃区间着色（linear-scan 的无序等价形式）
//!
//! 对每个寄存器算活跃区间 `[first_def, last_use]`，再���区间图按「起点递增、
//! 重叠者异色」贪心着色。相比图着色快一个量级（SSA 短生命期 IR 的业界标准做法，
//! Mössenböck 2017），而这里的收益本就来自**复用槽位**而非最优着色。
//!
//! ## 保守边界（首版刻意为之）
//!
//! - 只做**函数体单块**的区间复用；**不做**跨基本块活跃性合并。故控制流密集的
//!   程序收益较小，但**绝不会**因跨块活跃性算错而出错。
//! - 区间重叠判定用**闭区间**（`a.first <= b.last && b.first <= a.last`），
//!   宁可多分槽、不冒险 —— 正确性优先于槽位数。
//! - 同一寄存器被多次写（重新定义）时，区间取**首次 def 到末次 use**；
//!   这让「旧值最后一次读」到「新值第一次写」之间仍判为重叠 ⇒ 安全但保守。
//!
//! ## 嵌套 MirFunction
//!
//! 闭包体 / task 体 / handler 是**独立寄存器空间**（见 `mir/lower.rs`、
//! `mir/fcfg.rs` 的既有约定），故递归处理，每个空间独立分配。

use crate::mir::{MirFunction, MirInst, Reg};

/// 函数体是否**可安全分配**（见 `allocate` 的两类限制说明）。
///
/// 保守判据：必须同时满足
/// 1. **无回跳** —— 无 `Jump`/`JumpIf`/`JumpIfNot`/`Break`/`Continue` 指向
///    `pc <= 当前 pc`（即无循环）；
/// 2. **无副作用/控制语句** —— 只允许纯计算与声明类指令。效果语句
///    （`Handle`/`Perform`/`Parallel`/`Worker`/…）写寄存器的方式不由
///    `written_reg()` 完整覆盖，逐指令 def 计数会漏。
///
/// 两条都是**充分不必要**的保守判据：命中即跳过分配，行为与分配前逐位一致。
fn is_allocatable(body: &[MirInst]) -> bool {
    for (pc, inst) in body.iter().enumerate() {
        // (1) 回跳检测
        let target = match inst {
            MirInst::Jump(t)
            | MirInst::JumpIf(_, t)
            | MirInst::JumpIfNot(_, t)
            | MirInst::Break(t)
            | MirInst::Continue(t) => Some(*t),
            _ => None,
        };
        if let Some(t) = target
            && t <= pc
        {
            return false;
        }
        // (2) 效果/控制语句一律排除（含向前 Jump：即便无环，控制转移也让
        //     「pc 序 = 执行序」这一前提失效）。
        if matches!(
            inst,
            MirInst::Jump(..)
                | MirInst::JumpIf(..)
                | MirInst::JumpIfNot(..)
                | MirInst::Break(..)
                | MirInst::Continue(..)
                | MirInst::Return(..)
                | MirInst::Halt(..)
                | MirInst::Handle { .. }
                | MirInst::Perform { .. }
                | MirInst::TaskDef { .. }
                | MirInst::Closure { .. }
                | MirInst::Parallel { .. }
                | MirInst::Worker { .. }
                | MirInst::Transaction { .. }
                | MirInst::Observe { .. }
                | MirInst::Span { .. }
                | MirInst::Import { .. }
                | MirInst::ExportMark { .. }
                | MirInst::Commit
                | MirInst::Rollback
                | MirInst::Label { .. }
        ) {
            return false;
        }
    }
    true
}

/// 只递归处理嵌套函数体（本体含环、跳过），保证嵌套体仍能拿到无环部分的收益。
fn allocate_nested_only(func: &mut MirFunction) {
    for inst in func.body.iter_mut() {
        visit_nested(inst, &mut |nested: &mut MirFunction| {
            allocate(nested);
        });
    }
}

/// 收集嵌套 `MirFunction` 并对其递归分配（每个空间独立）。
///
/// 遍历顺序任意 —— 各空间互不共享寄存器号，顺序不影响结果。
fn visit_nested(inst: &mut MirInst, f: &mut impl Fn(&mut MirFunction)) {
    match inst {
        MirInst::Closure { body, .. } => f(body),
        MirInst::TaskDef { body, .. } => f(body),
        MirInst::Handle { body, handler, .. } => {
            f(body);
            f(handler);
        }
        _ => {}
    }
}

/// 一个虚拟寄存器**某一次定义**的活跃区间（segment）。
///
/// ⚠ **为什么按「段」而不是「每寄存器一区间」**：本仓主路径**不是 SSA**
/// （`ssa.rs` 存在但未在 `compile_and_opt` 强制），寄存器会被**反复重定义**。
/// 典型如 for 循环索引：
///
/// ```text
/// pc9   Const(9, 0)              <- 第 1 次定义
/// pc12  BinaryOp(12, 9, GE, 10)  <- 读第 1 次的值
/// pc22  BinaryOp(9, 9, Add, 11)  <- **读旧值 + 写第 2 次的值**（read-modify-write）
/// ```
///
/// 第 1 次的值在 pc13 之后即死，与第 2 次的值**不重叠**。若按「一寄存器一区间」
/// 建模，会把两段并成 `[9,22]`，既虚增活跃度、又把不重叠的值判成冲突。
/// 故此处为**每次 def 建一段**，`read-modify-write` 会产生两段（各自的起止不同），
/// 由着色器按段分配槽。
#[derive(Debug, Clone, Copy)]
struct Segment {
    reg: usize,
    /// 第几次定义（用于查映射：同一 reg 的不同次定义可能落到不同槽）。
    def_id: usize,
    first: usize,
    last: usize,
}

/// 收集指令的**全部输入寄存器**（含 `input_regs()` 漏掉的 Quasiquote unquote）。
fn all_inputs(inst: &MirInst) -> Vec<Reg> {
    let mut inputs = inst.input_regs();
    if let MirInst::Quasiquote { segments, .. } = inst {
        for seg in segments {
            match seg {
                crate::mir::QuasiquoteSegment::Unquote(r)
                | crate::mir::QuasiquoteSegment::UnquoteSplice(r) => inputs.push(*r),
                crate::mir::QuasiquoteSegment::Quote(_) => {}
            }
        }
    }
    inputs
}

/// 计算函数体的活跃**段**列表（不做分配）。
///
/// 每条指令按顺序：
/// 1. 把本指令的**所有输入**所依赖的「当前段」的 `last` 推到本 pc
///    （它们在本 pc 仍存活 —— read-modify-write 的旧值也靠这一步活到本 pc）；
/// 2. 若有 `dst`，**结束旧段**（其 `last` 就是它的最后一次读）并开新段。
///
/// 段在「最后一次读」即结束；**从不读**的值 `last == first`（只占一个瞬时点）。
/// 故未被后续指令引用的值不会一直活跃到函数末尾 —— 这是槽位能真正被复用的关键。
///
/// 从未被写过的寄存器（读未初始化）不产生段 ⇒ 映射到 0（退化情形，保持可运行）。
fn segments(body: &[MirInst], n_regs: usize) -> Vec<Segment> {
    // 当前段：reg -> (def_id, first_pc, last_pc)
    let mut cur: Vec<Option<(usize, usize, usize)>> = vec![None; n_regs];
    let mut def_counter: Vec<usize> = vec![0; n_regs];
    let mut out: Vec<Segment> = Vec::new();

    for (pc, inst) in body.iter().enumerate() {
        let inputs = all_inputs(inst);
        // 1) 输入延活到本 pc。
        for r in &inputs {
            if *r < n_regs
                && let Some((_, _, last)) = cur[*r]
            {
                cur[*r].as_mut().expect("刚判过 is_some").2 = last.max(pc);
            }
        }
        // 2) 结束旧段、开新段。
        if let Some(d) = inst.dst()
            && d < n_regs
        {
            if let Some((def_id, first, last)) = cur[d] {
                out.push(Segment {
                    reg: d,
                    def_id,
                    first,
                    last,
                });
            }
            let def_id = def_counter[d];
            def_counter[d] += 1;
            cur[d] = Some((def_id, pc, pc));
        }
    }
    // 收尾：把仍开着的段以「其当前 last」结束（未被再引用过 ⇒ last 就是 def 点）。
    for (r, slot) in cur.iter().enumerate() {
        if let Some((def_id, first, last)) = *slot {
            out.push(Segment {
                reg: r,
                def_id,
                first,
                last,
            });
        }
    }
    out
}

/// 对**单个寄存器空间**（一个 `MirFunction` 的 body）做槽位复用重写。
///
/// 返回是否发生了改写；`false` 表示无寄存器可回收（调用方可跳过）。
fn allocate_body(body: &mut [MirInst], n_regs: usize) -> bool {
    let segs = segments(body, n_regs);
    if segs.is_empty() {
        return false;
    }

    // 区间图贪心着色：按 `first` 递增；与所有「已开未闭」区间重叠者不复用其色。
    let mut open: Vec<(usize, usize)> = Vec::new(); // (slot, end_pc)
    // 映射键是 (reg, def_id) —— 同一寄存器**不同次定义**可能落在不同槽。
    let mut map: std::collections::BTreeMap<(usize, usize), usize> = Default::default();
    let mut next_slot = 0usize;
    let mut peak_slot = 0usize;

    let mut sorted = segs.clone();
    sorted.sort_by_key(|s| (s.first, s.last));

    for sg in &sorted {
        open.retain(|&(_, end)| end >= sg.first);
        let used: Vec<usize> = open.iter().map(|&(s, _)| s).collect();
        let slot = match (0..next_slot).find(|s| !used.contains(s)) {
            Some(s) => s,
            None => {
                let s = next_slot;
                next_slot += 1;
                s
            }
        };
        map.insert((sg.reg, sg.def_id), slot);
        open.push((slot, sg.last));
        if slot + 1 > peak_slot {
            peak_slot = slot + 1;
        }
    }

    // 无收益则不改写（避免无谓扰动）。
    if peak_slot >= n_regs {
        return false;
    }

    // 逐指令重写：先按本指令的 dst 算出「这是该 reg 的第几次定义」，得到槽号；
    // 输入寄存器则查「当前活跃段」的槽。
    let mut def_seen: Vec<usize> = vec![0; n_regs];
    let mut cur_slot: Vec<Option<usize>> = vec![None; n_regs];
    for (pc, inst) in body.iter_mut().enumerate() {
        // 输入：映射到其当前活跃段的槽。
        let _ins: Vec<Reg> = all_inputs(inst);
        // 无活跃段（读未初始化）⇒ 归 0，保持可运行。
        let mut m = |r: Reg| -> Reg { cur_slot.get(r).copied().flatten().unwrap_or(0) };
        let old = std::mem::replace(inst, MirInst::Expr(0));
        let mut new_inst = old.map_regs(&mut m);
        // Quasiquote 的 unquote 寄存器同款映射（`map_regs` 不覆盖）。
        if let MirInst::Quasiquote { segments: qs, .. } = &mut new_inst {
            for seg in qs.iter_mut() {
                match seg {
                    crate::mir::QuasiquoteSegment::Unquote(r)
                    | crate::mir::QuasiquoteSegment::UnquoteSplice(r) => *r = m(*r),
                    crate::mir::QuasiquoteSegment::Quote(_) => {}
                }
            }
        }
        // 写寄存器：本指令的 def_id（当前计数）。
        if let Some(d) = old.written_reg()
            && d < n_regs
        {
            let def_id = def_seen[d];
            def_seen[d] += 1;
            if let Some(&sl) = map.get(&(d, def_id)) {
                remap_write_slot(&mut new_inst, sl);
                cur_slot[d] = Some(sl);
            }
        }
        let _ = pc;
        *inst = new_inst;
    }
    true
}

/// 把指令的「写寄存器」位设为 `r`（与 [`MirInst::written_reg`] 对称，
/// 但 `MatchExpr` 额外覆盖**全部** arm 的 `output_reg` —— `h_match_expr`
/// 对命中的 arm 写 `regs[*out]`，而 `map_regs` 对 arms 是原样透传）。
fn remap_write_slot(inst: &mut MirInst, r: Reg) {
    match inst {
        MirInst::Const(d, _)
        | MirInst::Var(d, _)
        | MirInst::Copy(d, _)
        | MirInst::BinaryOp(d, ..)
        | MirInst::Call(d, ..)
        | MirInst::MethodCall(d, ..)
        | MirInst::ListLit(d, _)
        | MirInst::DictLit(d, _)
        | MirInst::Index(d, ..)
        | MirInst::IndexAssign(d, ..)
        | MirInst::Pipe(d, ..)
        | MirInst::CallValue(d, ..)
        | MirInst::Prompt(d, _)
        | MirInst::Perform { dst: d, .. }
        | MirInst::Solve { dst: d, .. }
        | MirInst::Closure { dst: d, .. }
        | MirInst::DynTrait { dst: d, .. }
        | MirInst::Quasiquote { dst: d, .. } => *d = r,
        MirInst::MatchExpr { arms, .. } => {
            for arm in arms.iter_mut() {
                arm.3 = r;
            }
        }
        MirInst::Handle { k_dst, .. } => *k_dst = r,
        _ => {}
    }
}

/// 对函数（及其所有嵌套函数体）执行槽位复用分配。
///
/// - 纯重编号：只改寄存器编号与 `n_regs`，**不改变任何指令语义或顺序**。
/// - 幂等：分配后槽号已稠密，再次调用会因 `peak_slot >= n_regs` 而跳过。
pub fn allocate(func: &mut MirFunction) {
    // v0.104.6：安全阀 `MORA_NO_REGALLOC=1` 跳过分配（保留 emit 原编号）。
    // 用于 A/B 对照与紧急回退；正常编译路径不设此变量。
    if std::env::var("MORA_NO_REGALLOC").as_deref() == Ok("1") {
        return;
    }
    // v0.104.6：**只对「证明安全」的函数体分配**。
    //
    // 本分配器按 pc 线性扫描建活跃区间，它在下列情况**无法**给出正确的活跃性：
    //
    // 1. **含回跳（循环）**：回边让「pc 序」不等于执行序。for 索引在 pc22 被
    //    重定义、却在下一轮 pc12 被读；单遍扫描看不到跨迭代活跃性，会把两个
    //    **同时存活**的值判成不重叠而复用同槽 → 死循环（实测 4 个 fixture）。
    // 2. **含效果/handler 语句**（`Handle` / `Perform` / 语句型 `Define`…）：
    //    这些指令写寄存器的方式由 dispatch 自行管理（`written_reg()` 对它们
    //    返回 `None`，而 handler 仍会写 `k_dst`），逐指令的 def 计数无法
    //    覆盖 ⇒ 可能把两次不同的 def 映到同槽（实测 `handle_effect` 越界）。
    //
    // 跨迭代/跨语句活跃性需要 CFG 级数据流分析（支配边界 + 活跃性不动点），
    // 属另一量级的工程。本刀采取**安全优先**：不满足条件的函数体**原样保留**
    // emit 的编号，行为与分配前逐位一致；只对「纯计算 + 无环」的函数体启用。
    if !is_allocatable(&func.body) {
        allocate_nested_only(func);
        return;
    }
    let before = func.n_regs;
    let changed = allocate_body(&mut func.body, before);
    if changed {
        // 重算 n_regs：映射后用到的最大槽 + 1。
        let mut max_slot = 0usize;
        let mut any = false;
        for inst in &func.body {
            let mut visit = |r: usize| {
                any = true;
                if r + 1 > max_slot {
                    max_slot = r + 1;
                }
            };
            if let Some(d) = inst.dst() {
                visit(d);
            }
            for r in inst.input_regs() {
                visit(r);
            }
        }
        func.n_regs = if any { max_slot } else { 0 };
    }

    // 嵌套空间递归（无论本层是否改写，嵌套都要处理）。
    for inst in func.body.iter_mut() {
        visit_nested(inst, &mut |nested: &mut MirFunction| {
            allocate(nested);
        });
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;

    fn f(body: Vec<MirInst>, n: usize) -> MirFunction {
        MirFunction {
            params: vec![],
            body,
            n_regs: n,
            ..Default::default()
        }
    }

    #[test]
    fn empty_body_is_noop() {
        let mut func = f(vec![], 0);
        allocate(&mut func);
        assert_eq!(func.n_regs, 0);
    }

    #[test]
    fn disjoint_intervals_reuse_one_slot() {
        // 两个**不重叠**的值应共享槽：
        //   pc0 Const(0)  ← r0 的第 1 次定义
        //   pc1 Var(1, "a")  写 r1（**不读 r0**）⇒ 两段不重叠
        //   pc2 Const(0, …) ← r0 的第 2 次定义（r0 第 1 次已在 pc0 后死亡）
        // 用 `Var`（写 dst 不读输入）而非 `Expr`：`Expr(r)` 的 `written_reg`
        // 也是 `r`，会把它误当成对该值的**重定义**。
        let body = vec![
            MirInst::Const(0, Value::Int(1)),
            MirInst::Var(1, "a".to_string()),
            MirInst::Const(0, Value::Int(2)),
            MirInst::Var(2, "b".to_string()),
        ];
        let mut func = f(body, 3);
        allocate(&mut func);
        assert!(
            func.n_regs < 3,
            "互不重叠的值应复用槽位（期望 <3，实得 {}）",
            func.n_regs
        );
    }

    #[test]
    fn overlapping_intervals_get_distinct_slots() {
        // r0 定义后，r1 读 r0 ⇒ 区间 [0,1] 与 [1,1] 在 pc=1 重叠（闭区间）。
        let body = vec![
            MirInst::Const(0, Value::Int(1)),
            MirInst::BinaryOp(1, 0, crate::common::BinaryOp::Add, 0),
        ];
        let mut func = f(body, 2);
        allocate(&mut func);
        assert_eq!(func.n_regs, 2, "重叠的值不可复用");
    }
}
