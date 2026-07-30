//! v0.58: Cascades Pattern-Rule 优化器框架（Phase H）
//!
//! 完全 MIR-native：所有 Pattern 匹配 `MirInst` 变体，零 AST 依赖。
//!
//! # 模块结构
//!
//! - [`pattern`] — `MirPattern` 匹配 `MirInst`（扁平指令列表）
//! - [`ssa_pattern`] — `SsaPattern` 匹配 `SsaInst`（SSA 化指令）
//! - [`rule`] — `RewriteRule` trait + 示例规则（基于 `MirPattern`）
//! - [`cost`] — `CostModel` trait + `InstructionCount` / `TokenEstimate` 实现
//! - [`search`] — 贪心搜索算法（`greedy_search`）
//!
//! # 设计原则：为什么 MirPattern 与 SsaPattern 双层？
//!
//! 两者都基于 `MirExprKind`（v0.55 MIR）但操作不同层级：
//!
//! 1. **`MirPattern`（pattern.rs）** — 匹配 `MirInst`（`Vec<MirInst>` body）
//!    - 简单局部规则（`RedundantJumpRule`、`ConstFoldingRule`）
//!    - 不需要 dataflow
//!
//! 2. **`SsaPattern`（ssa_pattern.rs）** — 匹配 `SsaInst`
//!    - 需要 dataflow 的规则（`SsaConstFoldingRule`）
//!    - 常量值通过外部 `&HashMap<SsaReg, Value>` 注入
//!
//! **为什么不合并？**
//! - `MirInst::Jump/Return` 是指令变体；`Terminator::Jump/Return` 是 SSA 独立 enum
//! - `MirSsaFunction` 包含 `blocks`/`phis`/`terminator` 复合结构，不是 `Vec<MirInst>`
//! - 完全统一需要 `<R: RegLike, I: InstLike>` 泛型 + 大量 trait bound
//! - 双层独立 + 共享 trait 概念（`Match`/`RewriteRule`/`CostModel`）是务实选择
//!
//! # 未来统一方向（Phase H.6+）
//!
//! ```ignore
//! // 1. 提取 RegLike trait 抽象寄存器类型
//! pub trait RegLike: Copy + Eq + std::fmt::Debug {
//!     fn index(self) -> usize;
//! }
//! impl RegLike for Reg { fn index(self) -> usize { self } }
//! impl RegLike for SsaReg { fn index(self) -> usize { self } }
//!
//! // 2. 提取 InstLike trait
//! pub trait InstLike<'a> {
//!     type Reg: RegLike;
//!     fn is_const(&'a self) -> Option<(Self::Reg, &'a Value)>;
//!     fn is_binaryop(&'a self) -> Option<...>;
//!     // ...
//! }
//!
//! // 3. Pattern 泛型化
//! pub struct GenericPattern<I: InstLike>(...);
//! ```
//!
//! 见 `pattern.rs::RegMatcher` / `ssa_pattern.rs::SsaRegMatcher` 注释中的 TODO 标记。

pub mod cost;
pub mod dag_rule;
pub mod dag_search;
pub mod pattern;
pub mod rule;
pub mod search;
pub mod ssa_pattern;

use crate::mir::MirFunction;
use crate::mir::dag::MirDag;
use crate::mir::optimize::cost::TokenEstimate;
use crate::mir::optimize::dag_rule::{AlgebraicSimplifyDagRule, ConstFoldingDagRule, CseDagRule, DeadNodeDagRule, DagRewriteRule};
use crate::mir::optimize::dag_search::dag_search_staged;
use crate::mir::optimize::rule::builtin_rules;
use crate::mir::optimize::search::greedy_search;

/// 对 MirFunction.body 应用 Cascades 优化 pass
///
/// 使用内置规则库 + 默认 cost model，贪心搜索最多 50 轮。
pub fn apply_rules(func: &mut MirFunction) {
    let rules = builtin_rules();
    let cost = TokenEstimate;
    let result = greedy_search(&func.body, &rules, &cost, 50);
    func.body = result.body;
    func.n_regs = func.n_regs.max(
        func.body.iter().map(|inst| {
            match inst {
                crate::mir::MirInst::Const(r, _)
                | crate::mir::MirInst::Var(r, _)
                | crate::mir::MirInst::BinaryOp(r, ..)
                | crate::mir::MirInst::Call(r, ..)
                | crate::mir::MirInst::Expr(r)
                | crate::mir::MirInst::Index(r, ..)
                | crate::mir::MirInst::IndexAssign(r, ..)
                | crate::mir::MirInst::MethodCall(r, ..)
                | crate::mir::MirInst::Pipe(r, ..)
                | crate::mir::MirInst::Prompt(r, ..)
                | crate::mir::MirInst::ListLit(r, ..)
                | crate::mir::MirInst::DictLit(r, ..) => r + 1,
                _ => 0,
            }
        }).max().unwrap_or(0),
    );
}

/// v0.60: Staged DAG-level optimization pass.
///
/// Rules are grouped into four stages, applied in order:
/// 1. Algebraic simplification — cheap, enables further folding
/// 2. Constant folding — eliminates known computation
/// 3. Common subexpression elimination — removes duplicates
/// 4. Dead node removal — cleanup
///
/// Dirty-tracking ensures only nodes whose inputs changed are re-checked.
/// Convergence: outer loop repeats until no stage produces a change.
pub fn dag_optimize(dag: &mut MirDag) {
    let stages: Vec<Vec<Box<dyn DagRewriteRule>>> = vec![
        vec![Box::new(AlgebraicSimplifyDagRule)],
        vec![Box::new(ConstFoldingDagRule)],
        vec![Box::new(CseDagRule)],
        vec![Box::new(DeadNodeDagRule)],
    ];
    let cost = TokenEstimate;
    dag_search_staged(dag, &stages, &cost);
}
