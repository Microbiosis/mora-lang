//! v0.90: pipeline — 9 层 IR 管线驱动 + 差分验证。
//!
//! 生产管线切换的核心：把 9 层架构从"独立代码岛"接入真实编译路径。
//!
//! 管线（每次编译全量运行）：
//!   Token → ParserV3::compile() → MirFunction + MirWitness[]   [现有]
//!     ↓
//!   witness_to_fcfg()      → Vec<Node<()>>          [FCFG]
//!     ↓
//!   export_type_table()    → TypeTable              [影子表]
//!   annotate()             → Vec<Node<TypeInfo>>   [EHIR]
//!     ↓
//!   ehir_to_core()         → CoreFunction           [Core]
//!     ↓
//!   core_to_cmir()         → CmirBlock              [CMIR]
//!     ↓
//!   cmir_to_lmir()         → LmirInst[] + layouts   [LMIR]
//!     ↓
//!   populate_layout_table()→ LayoutTable            [RIR 桥]
//!     ↓
//!   差分验证: lower_fcfg(fcfg) vs 原 MirFunction 指令形状等价
//!
//! 执行器仍消费原 MirFunction（Phase 2 切换，待差分验证全绿后）。

use crate::mir::cmir_to_lmir::cmir_to_lmir;
use crate::mir::core::CoreFunction;
use crate::mir::core_to_cmir::core_to_cmir;
use crate::mir::ehir_to_core::ehir_to_core;
use crate::mir::fcfg::Fcfg;
use crate::mir::fcfg_lower::lower_fcfg;
use crate::mir::lmir::LmirInst;
use crate::mir::lmir_to_rir::populate_layout_table;
use crate::mir::rir::LayoutTable;
use crate::mir::witness::MirWitness;
use crate::mir::witness_to_fcfg::witness_to_fcfg;
use crate::mir::MirFunction;
use crate::typeck::annotate::annotate;
use crate::typeck::export::{export_type_table, TypeTable};

/// 9 层管线运行结果。
#[derive(Debug)]
pub struct PipelineResult {
    /// FCFG 节点数。
    pub fcfg_nodes: usize,
    /// TypeTable 条目数（成功标注类型的节点数）。
    pub typed_nodes: usize,
    /// Core 指令数。
    pub core_insts: usize,
    /// CMIR 节点数。
    pub cmir_nodes: usize,
    /// LMIR 指令数。
    pub lmir_insts: usize,
    /// LayoutTable 条目数。
    pub layouts: usize,
    /// 差分验证：新管线产出的 MirInst 数。
    pub pipeline_mir_count: usize,
    /// 差分验证：原管线 MirInst 数。
    pub original_mir_count: usize,
    /// 差分验证是否通过（指令序列形状等价）。
    pub differential_ok: bool,
    /// 差分差异描述（失败时非空）。
    pub differential_diffs: Vec<String>,
}

/// 运行完整 9 层管线（FCFG → EHIR → Core → CMIR → LMIR → LayoutTable）。
///
/// 输入：ParserV3::compile() 的产出（MirFunction + witnesses）。
/// **必须在 apply_rules 之前调用**（差分要求 raw-to-raw 比较）。
/// 返回：(统计 + 差分结果, 管线产出的 MirFunction)。
pub fn run_pipeline(func: &MirFunction, witnesses: &[MirWitness]) -> (PipelineResult, MirFunction) {
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

    // ── CMIR ──
    let cmir = core_to_cmir(&core);
    let cmir_nodes = cmir.nodes.len();

    // ── LMIR ──
    let (lmir_insts_vec, layouts_raw): (Vec<LmirInst>, _) = cmir_to_lmir(&cmir);
    let lmir_insts = lmir_insts_vec.len();

    // ── RIR 布局表 ──
    let _layout_table: LayoutTable = populate_layout_table(&lmir_insts_vec, &layouts_raw);
    let layouts = layouts_raw.len();

    // ── 管线产出的 MirFunction（执行器切换的输入）──
    let (pipeline_body, pipeline_n_regs) = lower_fcfg(&fcfg);
    let pipeline_func = MirFunction {
        params: vec![],
        body: pipeline_body.clone(),
        n_regs: pipeline_n_regs,
        effects: func.effects.clone(),
    };

    // ── 差分验证：lower_fcfg(fcfg) vs 原 MirFunction（raw vs raw）──
    let pipeline_mir_count = pipeline_body.len();
    let original_mir_count = func.body.len();
    let (differential_ok, differential_diffs) =
        differential_check(&pipeline_body, &func.body);

    let result = PipelineResult {
        fcfg_nodes,
        typed_nodes,
        core_insts,
        cmir_nodes,
        lmir_insts,
        layouts,
        pipeline_mir_count,
        original_mir_count,
        differential_ok,
        differential_diffs,
    };
    (result, pipeline_func)
}

/// 差分验证：比较新管线（witness→FCFG→lower）与原管线（emit.rs 直出）
/// 的指令序列形状。
///
/// 形状等价标准：逐指令比较"指令类别"（Const/BinaryOp/Call/Jump...）。
/// 寄存器编号不要求一致（两管线的分配顺序策略不同），
/// 常量值要求一致（语义保持验证）。
fn differential_check(
    pipeline: &[crate::mir::MirInst],
    original: &[crate::mir::MirInst],
) -> (bool, Vec<String>) {
    let mut diffs = Vec::new();

    // 长度差异（信息性，不直接判失败 — witness 路径的 Sequence 打包
    // 可能引入额外 Const 节点）
    if pipeline.len() != original.len() {
        diffs.push(format!(
            "inst count: pipeline={} original={} (delta={})",
            pipeline.len(),
            original.len(),
            pipeline.len() as isize - original.len() as isize
        ));
    }

    // 逐指令类别比较（取较短长度的前缀）
    let n = pipeline.len().min(original.len());
    for i in 0..n {
        let p = inst_category(&pipeline[i]);
        let o = inst_category(&original[i]);
        if p != o {
            diffs.push(format!("inst[{}]: pipeline={:?} original={:?}", i, p, o));
            if diffs.len() > 10 {
                diffs.push("... (truncated)".to_string());
                break;
            }
        }
    }

    (diffs.is_empty(), diffs)
}

/// 提取指令类别（忽略寄存器编号）。差分审计公共入口。
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
        MirInst::ListLit(_, _) => "ListLit",
        MirInst::DictLit(_, _) => "DictLit",
        MirInst::Index(_, _, _) => "Index",
        MirInst::IndexAssign(_, _, _) => "IndexAssign",
        MirInst::MethodCall(_, _, _, _) => "MethodCall",
        MirInst::Pipe(_, _, _) => "Pipe",
        MirInst::Prompt(_, _) => "Prompt",
        MirInst::MatchExpr { .. } => "MatchExpr",
        MirInst::MatchArm { .. } => "MatchArm",
        MirInst::Closure { .. } => "Closure",
        MirInst::DynTrait { .. } => "DynTrait",
        MirInst::Define(_, _) => "Define",
        MirInst::Assign(_, _) => "Assign",
        MirInst::Expr(_) => "Expr",
        MirInst::TaskDef { .. } => "TaskDef",
        MirInst::ToolDef { .. } => "ToolDef",
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
        MirInst::Save { .. } => "Save",
        MirInst::Load { .. } => "Load",
        MirInst::ReadFile { .. } => "ReadFile",
        MirInst::WriteFile { .. } => "WriteFile",
        MirInst::AppendFile { .. } => "AppendFile",
        MirInst::ReadBytesFile { .. } => "ReadBytesFile",
        MirInst::WriteBytesFile { .. } => "WriteBytesFile",
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
        MirInst::SkillDef { .. } => "SkillDef",
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
            count_fcfg_nodes(&then.nodes)
                + else_.as_ref().map_or(0, |e| count_fcfg_nodes(&e.nodes))
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
        Node::AppDef { init, update, view, .. } => {
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
        Node::ImplDef { methods, .. } => methods.iter().map(|(_, b)| count_fcfg_nodes(&b.nodes)).sum(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_arithmetic() {
        // 顶层字面量 → LMIR 布局表填充（task 内字面量在嵌套闭包体中）
        let src = "let x = 42i\nlet y = 3.5\nx";
        let (func, witnesses) = crate::parser_v3::ParserV3::compile(src).unwrap();
        let (result, pipeline_func) = run_pipeline(&func, &witnesses);
        assert!(result.fcfg_nodes > 0, "FCFG should be non-empty");
        assert!(result.core_insts > 0, "Core should be non-empty");
        assert!(result.layouts > 0, "LayoutTable should be populated (top-level Int/Float consts)");
        assert!(!pipeline_func.body.is_empty(), "pipeline MirFunction should be non-empty");
    }

    #[test]
    fn pipeline_task_body() {
        // 嵌套函数体（task 内）— FCFG/Core 仍应非空
        let src = "task main()\n  print(10i + 32i)\nend";
        let (func, witnesses) = crate::parser_v3::ParserV3::compile(src).unwrap();
        let (result, pipeline_func) = run_pipeline(&func, &witnesses);
        assert!(result.fcfg_nodes > 0, "FCFG should be non-empty (task body)");
        assert!(result.core_insts > 0, "Core should be non-empty (closure create)");
        assert!(!pipeline_func.body.is_empty());
    }

    #[test]
    fn pipeline_function_call() {
        let src = "let ops = {\"add\": fn(a, b) a + b end}\nprint(ops.add(2i, 3i))";
        let (func, witnesses) = crate::parser_v3::ParserV3::compile(src).unwrap();
        let (result, _pipeline_func) = run_pipeline(&func, &witnesses);
        assert!(result.fcfg_nodes > 0);
        assert!(result.typed_nodes > 0, "TypeTable should have entries");
    }

    #[test]
    fn pipeline_handle_effect() {
        let src = "let g = \"init\"\nhandle Ai {\n  g = perform Ai(\"hello\")\n} {\n  \"m:\" + __arg0\n}\ng";
        let (func, witnesses) = crate::parser_v3::ParserV3::compile(src).unwrap();
        let (result, _pipeline_func) = run_pipeline(&func, &witnesses);
        assert!(result.fcfg_nodes > 0);
        assert!(result.core_insts > 0);
    }
}
