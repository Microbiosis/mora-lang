//! v0.90: pipeline — 9 层 IR 管线驱动。
//!
//! ## 三个入口（v0.104.6 D474）
//!
//! | 函数 | 用途 | 额外开销 |
//! |------|------|---------|
//! | [`compile`] | **生产路径** —— witness → FCFG → MIR | 无 |
//! | [`compile_optimized`] | **生产编译** —— 上一项 + 优化 + 寄存器分配 | 优化器 |
//! | [`probe_all_layers`] | **CI 探针** —— 额外跑 EHIR / Core 两层（仅统计） | EHIR + Core |
//!
//! ## 差分机制为何退役（D474）
//!
//! v0.90.3 起，生产入口每次编译都跑**两条完整 lowering**（9 层 + `emit.rs`
//! 参考实现）**加**一次逐条差分，差分失败就回落旧路径。实测代价（debug
//! build，300 个 task 的程序）：9 层开 317 ms / 关 213 ms ⇒ 影子管线 + 差分
//! 约占总耗时 **31%**（且 EHIR / Core 两层只用于统计，不进产出路径）。
//!
//! D473 把**最后一批**未接入的构造补齐（`worker` / `transaction` / `eval` /
//! `aggregate` / `commit` / `rollback` / `pipe` / `orchestrate`），全量普查
//! 零回落（`tests/nine_layer_full_census.rs`：117 个程序）。
//!
//! **D474 把差分机制整个退役** —— 因为它的**对照物已经不存在**：parser 不再
//! 发射 MirInst（只产 witness），MIR 全部由 9 层产出 ⇒「参考实现」这一侧
//! **没有第二条路径可对比**。差分若继续存在，只会变成**自己跟自己比**（恒等）
//! —— 一条恒真的判据比没有判据更坏：它让人以为覆盖还在。
//!
//! ⇒ **等价性改由「期望输出」保证**：CI 断言每个构造 / 程序的**具体输出**，
//! 而不是两条路径互相印证。这比原判据**更强** —— 原判据只能发现
//! 「两条路径不一致」（两条一起错到同一个错值时反而绿），新判据直接钉住
//! **正确值**。
//!
//! ## 管线（生产路径，`compile`）
//!
//! ```text
//! Token → ParserV3::compile() → MirWitness[]
//!     ↓
//!   witness_to_fcfg()      → Vec<Node<()>>          \[FCFG\]
//!     ↓
//!   lower_fcfg()           → MirInst[]              \[MIR —— 执行器消费它\]
//! ```
//!
//! ## 探针（`probe_all_layers`）额外跑
//!
//! ```text
//!   export_type_table()    → TypeTable              \[影子表\]
//!   annotate()             → Vec<Node`<TypeInfo>`>   \[EHIR\]
//!   ehir_to_core()         → CoreFunction           \[Core —— 仅统计，不进产出\]
//! ```
//!
//! v0.104.6：CMIR / LMIR / RIR 三层已删除（不在产出路径上，
//! 详见 `mir/mod.rs` 的删除说明）。

use crate::mir::MirFunction;
use crate::mir::core::CoreFunction;
use crate::mir::ehir_to_core::ehir_to_core;
use crate::mir::fcfg::Fcfg;
use crate::mir::fcfg_lower::lower_fcfg;
use crate::mir::witness::MirWitness;
use crate::mir::witness_to_fcfg::witness_to_fcfg;
use crate::typeck::annotate::annotate;
use crate::typeck::export::{TypeTable, export_type_table};

/// 全层探针的统计。
///
/// v0.104.6 D474：原 `PipelineResult` 还带四个差分字段
/// （`pipeline_mir_count` / `original_mir_count` / `differential_ok` /
/// `differential_diffs`）。差分退役后它们**全部作废** —— 留着会让人以为
/// 「对照物还在」，而实际上差分已成恒真。
#[derive(Debug)]
pub struct LayerStats {
    /// FCFG 节点数。
    pub fcfg_nodes: usize,
    /// TypeTable 条目数（成功标注类型的节点数）。
    pub typed_nodes: usize,
    /// Core 指令数。
    pub core_insts: usize,
}

/// **生产路径**：9 层唯一编译路径 —— witness → FCFG → MIR。
///
/// v0.104.6 D473：这是**生产编译入口**（`cli::compile_and_opt`）唯一调用的
/// 管线函数。它**不跑 EHIR / Core**（那两层只用于统计，不进产出路径）。
///
/// 等价性由「期望输出」保证，见本文件头的说明。
pub fn compile(func: &MirFunction, witnesses: &[MirWitness]) -> MirFunction {
    let fcfg: Vec<Fcfg> = witness_to_fcfg(witnesses);
    let (body, n_regs) = lower_fcfg(&fcfg);
    MirFunction {
        params: vec![],
        body,
        n_regs,
        effects: func.effects.clone(),
    }
}

/// **生产编译**：9 层 → 优化 → 寄存器分配。
///
/// v0.104.6 D473：这是**唯一**的生产编译路径 —— CLI（`cli::compile_and_opt`）、
/// `import`（`interpreter::mir_import`）、runtime `eval`（同像性内建）、
/// REPL 全部走它。
///
/// 此前这四处**各自**取 `ParserV3::compile` 的 MirFunction，而 `ParserV3::compile`
/// 自身也走 9 层 ⇒ 路径在 `compile` 内部**唯一**，调用方不必各自决定。
pub fn compile_optimized(
    func: &MirFunction,
    witnesses: &[MirWitness],
    level: crate::mir::ssa::OptLevel,
) -> MirFunction {
    let mut f = compile(func, witnesses);
    crate::mir::optimize::apply_rules(&mut f);
    if level.enabled() {
        crate::mir::opt::optimize(&mut f, level);
    }
    crate::mir::regalloc::allocate(&mut f);
    f
}

/// **全层探针** —— witness → FCFG → EHIR → Core → MIR，返回统计 + 产出的 MIR。
///
/// **CI 验证专用**（v0.104.6 D474）—— 生产编译入口不调用它。
///
/// 它比 [`compile`] 多做一件事：跑 EHIR / Core 两层（仅统计用，不进产出路径）。
/// 这两层仍在实现里、仍需保持可运行，故留一个探针入口覆盖它们。
///
/// ⚠ v0.104.6 D474：此函数**不再做差分**。原实现里它会把 9 层产出与
/// 「参考实现」（`emit.rs` 直出）逐条比对 —— 但 D474 起参考实现**已不存在**
/// （parser 只产 witness），那次比对成了**自己跟自己比**的恒真判据。
/// 等价性改由「期望输出」保证（见本文件头）。
///
/// 输入：ParserV3::compile() 的产出（MirFunction + witnesses）。
/// 返回：(各层统计, 管线产出的 MirFunction)。
pub fn probe_all_layers(func: &MirFunction, witnesses: &[MirWitness]) -> (LayerStats, MirFunction) {
    // ── FCFG ──
    let fcfg: Vec<Fcfg> = witness_to_fcfg(witnesses);
    let fcfg_nodes = count_fcfg_nodes(&fcfg);

    // ── EHIR（影子表 + 标注）──
    let table: TypeTable = export_type_table(witnesses);
    let typed_nodes = table.types.len();
    let ehir = annotate(&fcfg, &table);

    // ── Core ──
    let core: CoreFunction = ehir_to_core(&ehir, vec![], func.effects.clone());
    let core_insts = core.blocks.iter().map(|b| b.insts.len()).sum();

    // ── MIR ──
    let (body, n_regs) = lower_fcfg(&fcfg);
    let mir = MirFunction {
        params: vec![],
        body,
        n_regs,
        effects: func.effects.clone(),
    };

    (
        LayerStats {
            fcfg_nodes,
            typed_nodes,
            core_insts,
        },
        mir,
    )
}

/// 提取指令类别（忽略寄存器编号）。
///
/// v0.104.6 D474：差分退役后，它不再是「差分审计」的附属品，而是一个
/// 通用的 MIR 形状摘要工具 —— 按**类别**（而非寄存器号）描述一条指令，
/// 用于「这个函数体由哪些种类的指令构成」这类诊断与断言。
pub fn inst_category_pub(inst: &crate::mir::MirInst) -> &'static str {
    inst_category(inst)
}

/// 提取指令类别（忽略寄存器编号）。
fn inst_category(inst: &crate::mir::MirInst) -> &'static str {
    use crate::mir::MirInst;
    match inst {
        MirInst::Const(_, _) => "Const",
        MirInst::Var(_, _) => "Var",
        MirInst::Copy(_, _) => "Copy",
        MirInst::BinaryOp(_, _, _, _) => "BinaryOp",
        MirInst::Call(_, _, _) => "Call",
        MirInst::CallValue(_, _, _) => "CallValue",
        MirInst::ListLit(_, _) => "ListLit",
        MirInst::DictLit(_, _) => "DictLit",
        MirInst::Index(_, _, _) => "Index",
        MirInst::IndexAssign(_, _, _) => "IndexAssign",
        MirInst::MethodCall(_, _, _, _) => "MethodCall",
        MirInst::Pipe(_, _, _) => "Pipe",
        MirInst::Prompt(_, _) => "Prompt",
        MirInst::MatchExpr { .. } => "MatchExpr",
        MirInst::Closure { .. } => "Closure",
        MirInst::DynTrait { .. } => "DynTrait",
        MirInst::Define(_, _) => "Define",
        MirInst::Assign(_, _) => "Assign",
        MirInst::Expr(_) => "Expr",
        MirInst::TaskDef { .. } => "TaskDef",
        MirInst::Import(_) => "Import",
        MirInst::ExportMark(_) => "ExportMark",
        MirInst::WithConfig { .. } => "WithConfig",
        MirInst::Handle { .. } => "Handle",
        MirInst::Perform { .. } => "Perform",
        MirInst::Transaction { .. } => "Transaction",
        MirInst::Send { .. } => "Send",
        MirInst::Aggregate { .. } => "Aggregate",
        MirInst::Rollback => "Rollback",
        MirInst::Commit => "Commit",
        MirInst::Worker { .. } => "Worker",
        MirInst::Parallel { .. } => "Parallel",
        MirInst::Observe { .. } => "Observe",
        MirInst::Span { .. } => "Span",
        MirInst::Eval { .. } => "Eval",
        MirInst::MacroDef { .. } => "MacroDef",
        MirInst::PromptSection { .. } => "PromptSection",
        MirInst::DocumentSection { .. } => "DocumentSection",
        MirInst::Orchestrate { .. } => "Orchestrate",
        MirInst::TypeAlias { .. } => "TypeAlias",
        MirInst::EnumDef { .. } => "EnumDef",
        MirInst::StructDef { .. } => "StructDef",
        MirInst::ModelDef { .. } => "ModelDef",
        MirInst::RelDef { .. } => "RelDef",
        MirInst::Solve { .. } => "Solve",
        MirInst::MsgDef { .. } => "MsgDef",
        MirInst::UpdateDef { .. } => "UpdateDef",
        MirInst::AppDef { .. } => "AppDef",
        MirInst::TraitDef { .. } => "TraitDef",
        MirInst::ImplDef { .. } => "ImplDef",
        MirInst::Label(_) => "Label",
        MirInst::Jump(_) => "Jump",
        MirInst::JumpIf(_, _) => "JumpIf",
        MirInst::JumpIfNot(_, _) => "JumpIfNot",
        MirInst::Return(_) => "Return",
        MirInst::Halt(_) => "Halt",
        MirInst::Break(_) => "Break",
        MirInst::Continue(_) => "Continue",
        MirInst::Quasiquote { .. } => "Quasiquote",
    }
}

/// 递归统计 FCFG 节点数。
fn count_fcfg_nodes(nodes: &[Fcfg]) -> usize {
    let mut count = 0;
    for n in nodes {
        count += 1;
        count += count_fcfg_children(n);
    }
    count
}

fn count_fcfg_children(n: &Fcfg) -> usize {
    use crate::mir::fcfg::Node;
    match n {
        Node::If { then, else_, .. } => {
            count_fcfg_nodes(&then.nodes) + else_.as_ref().map_or(0, |e| count_fcfg_nodes(&e.nodes))
        }
        Node::While { cond, body, .. } => {
            count_fcfg_nodes(&cond.nodes) + count_fcfg_nodes(&body.nodes)
        }
        Node::For { body, .. } => count_fcfg_nodes(&body.nodes),
        Node::Match { arms, .. } => arms.iter().map(|a| count_fcfg_nodes(&a.body.nodes)).sum(),
        Node::Let { body, .. } => count_fcfg_nodes(&body.nodes),
        Node::FnDef { body, .. } | Node::ClosureExpr { body, .. } => count_fcfg_nodes(&body.nodes),
        Node::Handle { body, handler, .. } => {
            count_fcfg_nodes(&body.nodes) + count_fcfg_nodes(&handler.nodes)
        }
        Node::Sequence { nodes, .. } => count_fcfg_nodes(nodes),
        Node::MacroDef { body, .. } => count_fcfg_nodes(&body.nodes),
        Node::UpdateDef { body, .. } => count_fcfg_nodes(&body.nodes),
        Node::AppDef {
            init, update, view, ..
        } => {
            count_fcfg_nodes(&init.nodes)
                + count_fcfg_nodes(&update.nodes)
                + count_fcfg_nodes(&view.nodes)
        }
        Node::WithConfig { body, .. } => count_fcfg_nodes(&body.nodes),
        Node::Solve { goal, .. } => count_fcfg_nodes(&goal.nodes),
        Node::PromptSection { body, .. } | Node::DocumentSection { body, .. } => {
            count_fcfg_nodes(&body.nodes)
        }
        Node::Observe { body, .. } | Node::Span { body, .. } => count_fcfg_nodes(&body.nodes),
        Node::Parallel { body, .. } => count_fcfg_nodes(&body.nodes),
        Node::Export { decl, .. } => count_fcfg_nodes(&decl.nodes),
        Node::ImplDef { methods, .. } => methods
            .iter()
            .map(|(_, b)| count_fcfg_nodes(&b.nodes))
            .sum(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_arithmetic() {
        // 顶层字面量 → FCFG/Core 非空（task 内字面量在嵌套闭包体中）
        let src = "let x = 42i\nlet y = 3.5\nx";
        let (func, witnesses) = crate::parser_v3::ParserV3::compile(src).unwrap();
        let (stats, mir) = probe_all_layers(&func, &witnesses);
        assert!(stats.fcfg_nodes > 0, "FCFG should be non-empty");
        assert!(stats.core_insts > 0, "Core should be non-empty");
        // v0.104.6：原断言 `result.layouts > 0`（LMIR 布局表）随三层删除移除 ——
        // 布局表不在产出路径上，断言它无意义。
        assert!(
            !mir.body.is_empty(),
            "pipeline MirFunction should be non-empty"
        );
    }

    #[test]
    fn pipeline_task_body() {
        // 嵌套函数体（task 内）— FCFG/Core 仍应非空
        let src = "task main()\n  print(10i + 32i)\nend";
        let (func, witnesses) = crate::parser_v3::ParserV3::compile(src).unwrap();
        let (stats, mir) = probe_all_layers(&func, &witnesses);
        assert!(stats.fcfg_nodes > 0, "FCFG should be non-empty (task body)");
        assert!(
            stats.core_insts > 0,
            "Core should be non-empty (closure create)"
        );
        assert!(!mir.body.is_empty());
    }

    #[test]
    fn pipeline_function_call() {
        let src = "let ops = {\"add\": fn(a, b) a + b end}\nprint(ops.add(2i, 3i))";
        let (func, witnesses) = crate::parser_v3::ParserV3::compile(src).unwrap();
        let (stats, _mir) = probe_all_layers(&func, &witnesses);
        assert!(stats.fcfg_nodes > 0);
        assert!(stats.typed_nodes > 0, "TypeTable should have entries");
    }

    #[test]
    fn pipeline_handle_effect() {
        let src = "let g = \"init\"\nhandle Ai {\n  g = perform Ai(\"hello\")\n} {\n  \"m:\" + __arg0\n}\ng";
        let (func, witnesses) = crate::parser_v3::ParserV3::compile(src).unwrap();
        let (stats, _mir) = probe_all_layers(&func, &witnesses);
        assert!(stats.fcfg_nodes > 0);
        assert!(stats.core_insts > 0);
    }
}
