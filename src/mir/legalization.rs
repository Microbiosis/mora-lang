//! v0.104.6：9 层管线的**显式合法性登记**（Legality Registry）。
//!
//! ## 为什么要这张表
//!
//! 生产编译入口 `cli::compile_and_opt` 用**整函数二值差分**决定走 9 层还是
//! 回落 `emit.rs`：`differential_ok == false` 就**整份丢弃**管线产出。这带来
//! 两个结构性缺陷：
//!
//! 1. **「已知未支持」与「管线回归」不可区分。** 前者是功能覆盖缺口（预期），
//!    后者是管线在**已支持**构造上出了 bug（缺陷）。二者生产侧看到的一模
//!    一样：都是「回落 + 一行 stderr」，于是**后者静默藏住了**。
//! 2. **判据是「与旧管线逐条一致」。** 旧管线（`emit.rs`）正是被 D1–D416 查出
//!    整轮缺陷的那一套，于是新管线结构上**无法比旧管线更好**。
//!
//! ## MLIR 的做法（参照）
//!
//! MLIR 的 dialect conversion 不是「全转或全废」的二值门，而是**逐 op 声明
//! 合法性**：`ConversionTarget` 把每个 op 标成 `Legal` / `Dynamic` / `Illegal`，
//! `applyPartialConversion` **只转能转的、转不动的原样留着**，并让「哪些还没
//! legalize」成为一个可见、可回归的集合。
//!
//! 本表就是 Mora 侧的 `ConversionTarget`：把「9 层降级链当前还接不进 / 会整条
//! 丢弃」的构造成**显式、按名、带原因**地登记下来。生产门据此把回落拆成三态
//! ——「合法且绿」走快路径、「含未 legalize 构造」按名回落（预期）、「全合法却
//! 差分红」= **回归**（大声报）。
//!
//! ## 登记的判据：结构签名，不是裸类别
//!
//! 关键取舍：**不能把整类 MIR 指令一律标未 legalize**。实测证据 ——
//! 合法程序 `handle Ai { g = perform Ai("hello") } { … }` 的 MIR 里**同样含
//! `Perform` 指令**，而它走 9 层管线**差分通过**（`tests/` 里
//! `should_fall_back == false` 的 `handle` 条即是）。故 `Perform` 只有
//! **不在任何 `Handle` 作用域内**（裸 perform）才算未 legalize。
//!
//! 登记表因此按**结构签名**（在哪种上下文里出现）而非裸类别判定，与
//! `tests/nine_layer_fallback_reasons.rs` 的分诊口径一一对应。

use crate::mir::{MirFunction, MirInst};

/// 一条「未 legalize」的构造登记项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unlegalized {
    /// 稳定的构造名（用于 stderr / CI 断言）。
    pub name: &'static str,
    /// 回落原因（人类可读；口径对齐 `nine_layer_fallback_reasons.rs`）。
    pub reason: &'static str,
}

/// 扫描一个 `MirFunction`（含所有嵌套 `MirFunction`），返回命中的未 legalize
/// 构造名（去重、保序）。
///
/// `in_handle` 标记当前是否处于某个 `Handle` 的 body/handler 作用域内 —— 裸
/// `perform` 与被 `handle` 包裹的 `perform` 因此可区分。
pub fn scan_unlegalized(func: &MirFunction) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    scan_body(&func.body, false, &mut out);
    out
}

fn push(out: &mut Vec<&'static str>, name: &'static str) {
    if !out.contains(&name) {
        out.push(name);
    }
}

/// 未 legalize 的 4 类构造：与 `nine_layer_fallback_reasons.rs` 的
/// `FALLBACK_REASONS` 一一对应（`model` / `tea_standalone` 已于 D412 走上管线）。
pub const UNLEGALIZED_WORKER: Unlegalized = Unlegalized {
    name: "worker",
    reason: "9 层降级链里没有 Worker 类别",
};
pub const UNLEGALIZED_TRANSACTION: Unlegalized = Unlegalized {
    name: "transaction",
    reason: "9 层降级链里没有 Transaction 类别",
};
pub const UNLEGALIZED_PERFORM_BARE: Unlegalized = Unlegalized {
    name: "perform_bare",
    reason: "裸 perform：emit 侧不发射该 Perform",
};
pub const UNLEGALIZED_EVAL_BARE: Unlegalized = Unlegalized {
    name: "eval_bare",
    reason: "eval 顶层裸形态：管线整条丢弃（产出 0 条指令）",
};

/// 全量登记表（供测试遍历；生产判定走 `scan_unlegalized`）。
pub const REGISTRY: &[Unlegalized] = &[
    UNLEGALIZED_WORKER,
    UNLEGALIZED_TRANSACTION,
    UNLEGALIZED_PERFORM_BARE,
    UNLEGALIZED_EVAL_BARE,
];

fn scan_body(body: &[MirInst], in_handle: bool, out: &mut Vec<&'static str>) {
    for inst in body {
        match inst {
            // Worker / Transaction：降级链无对应类别 → 未 legalize。
            MirInst::Worker { .. } => push(out, UNLEGALIZED_WORKER.name),
            MirInst::Transaction { .. } => push(out, UNLEGALIZED_TRANSACTION.name),

            // 裸 perform：不在任何 Handle 作用域内。
            MirInst::Perform { .. } => {
                if !in_handle {
                    push(out, UNLEGALIZED_PERFORM_BARE.name);
                }
            }

            // 顶层裸 eval（无 handler 包裹）：管线整条丢弃。
            // 注：当前实现里 `Eval` 只出现在裸顶层形态，故此处无条件登记；
            // 若将来 Eval 被 handle 包裹接入管线，需与 Perform 一样加 in_handle
            // 判别 —— 届时 REGISTRY / reasons 测试会强制同步。
            MirInst::Eval { .. } => push(out, UNLEGALIZED_EVAL_BARE.name),

            // 进入 Handle 的 body / handler：其内的 Perform 不再是裸的。
            MirInst::Handle { body, handler, .. } => {
                scan_body(&body.body, true, out);
                scan_body(&handler.body, true, out);
            }

            // 其余嵌套 MirFunction：保持当前 in_handle 上下文。
            MirInst::TaskDef { body, .. } => scan_body(&body.body, in_handle, out),
            // MatchArmInst = (pattern, Option<guard>, body, _)
            MirInst::MatchExpr { arms, .. } => {
                for (_pat, guard, body, _) in arms {
                    if let Some(g) = guard {
                        scan_body(&g.body, in_handle, out);
                    }
                    scan_body(&body.body, in_handle, out);
                }
            }
            MirInst::Parallel { body } => scan_body(&body.body, in_handle, out),
            _ => {}
        }
    }
}

/// 生产门的**三态判定**结果（MLIR `applyPartialConversion` 语义的直接对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// 全部构造已接入 9 层管线 —— 可走快路径（仍需差分绿）。
    AllLegal,
    /// 含未 legalize 构造 —— 功能覆盖缺口，**预期**回落（非缺陷）。
    HasUnlegalized,
    /// 全部构造已接入、差分却失败 —— 管线在**已支持**构造上出 bug（**回归**）。
    ///
    /// 生产门此时仍回落以保正确，但必须与 [`GateDecision::HasUnlegalized`]
    /// 用**不同措辞**报出，否则真实缺陷会被当成「已知缺口」静默藏住。
    Regression,
}

/// 依据「未 legalize 构造集合 + 差分是否通过」给出生产门的**唯一**判定。
///
/// 这是生产门与测试共用的**单一事实源**：三态的划分只在这一处发生，
/// 生产侧（[`crate::cli::compile_and_opt`]）与测试断言不会各写一份而漂移。
pub fn classify(unlegalized_constructs: &[&'static str], differential_ok: bool) -> GateDecision {
    if !unlegalized_constructs.is_empty() {
        GateDecision::HasUnlegalized
    } else if !differential_ok {
        GateDecision::Regression
    } else {
        GateDecision::AllLegal
    }
}
