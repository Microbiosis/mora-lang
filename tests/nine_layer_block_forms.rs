//! v0.104.6 D92：五类**块形态**永久回落到 `emit.rs` 原路径 —— 9 层管线的嵌套 body
//! **少一条尾部 `Return`**。其中 2 类是**真实语义差异**、3 类是**良性差异被判失败**。
//!
//! ## 背景：回落不是「降级到次优」，而是「避开一条坏路径」
//!
//! `cli::compile_and_opt` 的策略是「**差分绿则用 9 层产出，否则回落**」，
//! 9 层管线因此是**默认生产编译路径**（`cli/mod.rs:45`：「生产执行 9 层代码」）。
//! 回落即生产仍跑旧的 emit.rs 直出，用户只看到 stderr 一行摘要。
//!
//! ## 实测到的差异形状
//!
//! | 路径 | 嵌套体指令序列 |
//! |---|---|
//! | emit.rs（原） | `[Const, Call, Return]` |
//! | 9 层管线 | `[Const, Call]` ← **少的是尾部 `Return`** |
//!
//! 少掉的**不是**产生效果的指令（`Call` 在），而是
//! `emit_block_mir_and_wit` / `emit_section_w` 的 `emit_tail_return(last)`。
//!
//! ## 关键分叉：块的值是否被消费
//!
//! - **`prompt` / `document`：真实语义差异。** `emit_section_w` 的注释写明
//!   「body 求值 → Return（`h_*` 取此值作为 section text）」，
//!   `h_prompt_section` 确实把 `run_mir(body)` 的返回值绑成
//!   `Value::PromptSection { text }`。管线少这条 `Return` → section text 会是 `Nil`。
//!   **差分在这里判失败是完全正确的，回落是必要的。**
//! - **`observe` / `span` / `parallel`：良性差异被判失败。** 三个 emitter 在发完块
//!   指令后都是 `Const(dst, Nil)`，块值**被丢弃**，少一条 `Return` 不改变语义。
//!   差分把它们判失败，等于让这 3 类**永久享受不到** 9 层管线。
//! - **`transaction` / `worker`：已知类别缺口**（D57 注释已写明 `Transaction` 在
//!   整个降级链里不存在），回落正确。
//!
//! ## D57 注释与实测不符
//!
//! `mir/pipeline.rs` 的 D57 注释声称 8 类构造（parallel / observe / model / msg /
//! struct / enum / transaction / worker / eval）「**不再**因这类冗余而永久放弃
//! DAG 分析 / CSE / 贪心重写」。**对 observe / span / parallel / prompt / document
//! 不成立。** D57 当时只量了 task / handle / match / with 的嵌套差异（那几类确实
//! 为零），**没量块形态** —— 后者的嵌套体由 `emit_block_mir_and_wit` 生成，形状不同。
//!
//! ## 为什么本轮不修
//!
//! 放宽差分需要**先有等价性证据**：`tests/nine_layer_differential.rs` 的 19 条
//! 覆盖 arithmetic / dict / eval / for / handle / if / lisp / macro / match×4 /
//! nested_if / quasiquote / string / tea×2，**没有一条覆盖块形态**；
//! `tests/fixtures/e2e/` 里也只有 `prompt_section.mora` 用到块形态，且不在差分列表中。
//! **在无 fixture 的情况下打开 5 类形态的 9 层路径 = 无人验证**。
//! 故本轮只**记录**并锁住现状；前置条件是补块形态的等价性 fixture。

use mora::mir::pipeline::run_pipeline;
use mora::parser_v3::ParserV3;

/// 从一段 `MirFunction` 里抽出**嵌套** body（块形态的 body），返回其指令类别序列。
///
/// 块形态在顶层是 `Observe { body }` / `Span { .., body }` 等，嵌套 body 是
/// `Box<MirFunction>`。这里只取第一个找到的嵌套 body —— 探针程序里只有一个块。
fn nested_categories(f: &mora::mir::MirFunction) -> Vec<String> {
    use mora::mir::MirInst;
    for inst in &f.body {
        let body = match inst {
            MirInst::Observe { body, .. }
            | MirInst::Span { body, .. }
            | MirInst::Parallel { body, .. }
            | MirInst::PromptSection { body, .. }
            | MirInst::DocumentSection { body, .. } => Some(body.as_ref()),
            _ => None,
        };
        if let Some(b) = body {
            return b.body.iter().map(category_of).collect();
        }
    }
    Vec::new()
}

/// 单条指令的「类别」字符串（与 pipeline.rs 的 `significant_categories` 同口径：
/// 只看指令种类，不看寄存器号）。
fn category_of(i: &mora::mir::MirInst) -> String {
    use mora::mir::MirInst;
    match i {
        MirInst::Const(_, _) => "Const".into(),
        MirInst::Call { .. } => "Call".into(),
        MirInst::BinaryOp { .. } => "BinaryOp".into(),
        MirInst::Define { .. } => "Define".into(),
        MirInst::Var { .. } => "Var".into(),
        MirInst::Return(_) => "Return".into(),
        other => format!("{other:?}")
            .split(['(', ' '])
            .next()
            .unwrap_or("?")
            .into(),
    }
}

/// 五类块形态的最小复现程序。
const BLOCK_FORMS: &[(&str, &str)] = &[
    ("observe", "observe trace \"t\" do\n  print(1)\nend\n"),
    ("span", "span \"s\" do\n  print(1)\nend\n"),
    ("parallel", "parallel\n  print(1)\nend\n"),
    ("prompt", "prompt \"p\" do\n  print(1)\nend\n"),
    ("document", "document \"d\" do\n  print(1)\nend\n"),
];

/// 五类块形态的**嵌套 body** 条数在两条路径上仍不等，且 9 层管线侧**更少**
/// （尾部少一条 `Return`）。**这个形状差异本身是良性的** —— 见下。
///
/// ## v0.104.6 D276 → D315：两次翻转，两次撤销，两次都因同一个原因
///
/// **D276（第一次）**：把 `nested_diffs` 的对齐基准改成「剔除死 `Const(r, Nil)`
/// 后」，块形态因此**通过**差分。当时的核验是「差异只是尾部少一条 `Return`」
/// 且 A/B 输出逐条一致，于是翻转了断言。
///
/// **D276 撤销**：同一轮里**另一个**程序的 A/B 暴露了真回归 ——
/// `rel_*.mora` 在对齐后**不再回落**、改走 9 层路径，而那条路径**是坏的**
/// （寄存器破损，`internal: … references register 4 but the function only
/// has 1 register(s)`）⇒ 差分的那套「错位」当时是 `rel_*` 唯一的护栏。
///
/// **D314（修前提）**：`fcfg_lower::max_reg_in_node` 的 `_ => 0` 覆盖了 50 个
/// `Node` 变体里的 25 个（`Solve` / `Return` / `WithConfig` 漏算）⇒
/// `n_regs` 少算。改成**穷尽 match**（删掉兜底，此后新增变体是编译错误）
/// + `n_regs` 取「已发射指令寄存器」下界。
///
/// **D315（第二次翻转）**：护栏的前提已消除，重新应用 D276 的对齐。
///
/// ## 「语句会不执行」这个说法在 D315 被实测否证
///
/// D92 原文警告「若差分被放宽/移除，这些块里的语句会**不执行**」。
/// D315 实测 10 种块形态 × 3 个优化档位 = **30 个组合**，强制走 9 层管线
/// 与回落 `emit.rs` 的输出**逐行相同**（含嵌套块、多语句、值位置、
/// 后接 `for`、块后接多条语句等形状）。
///
/// ⇒ 缺的那条 `Return` 在嵌套体里**不产生可观察差异**。它是被
/// `deconstruct` / 执行器隐式补上的，**本轮未单独定位该补偿点**（待验）。
///
/// 护栏已从本判据移到 `tests/nine_layer_unblocked.rs`（作为常驻 A/B 回归），
/// 避免这类形状再次「靠一条判据的绿/红来间接保护」。
#[test]
fn d92_block_forms_nested_body_count_still_differs() {
    let mut report = Vec::new();
    for (name, src) in BLOCK_FORMS {
        let (func, witnesses) = ParserV3::compile(src).expect("compile");
        let (result, pipeline_func) = run_pipeline(&func, &witnesses);

        let orig_body = nested_categories(&func);
        let pipe_body = nested_categories(&pipeline_func);
        report.push(format!(
            "{name}: original={:?} pipeline={:?} (top-level {} vs {}, differential_ok={})",
            orig_body,
            pipe_body,
            result.original_mir_count,
            result.pipeline_mir_count,
            result.differential_ok
        ));
    }
    // 诊断输出（仅 --nocapture 可见）：让「到底差在哪」始终是可观测事实。
    eprintln!("D92 块形态嵌套体差分：\n  {}", report.join("\n  "));

    for (name, src) in BLOCK_FORMS {
        let (func, witnesses) = ParserV3::compile(src).expect("compile");
        let (result, _) = run_pipeline(&func, &witnesses);
        // 形状差异本身**仍然存在**（嵌套体尾部少一条 `Return`）—— 这是
        // D92 的原始发现，不因 D315 而消失。D315 改的是「它是否触发回落」。
        assert!(
            result.pipeline_mir_count < result.original_mir_count,
            "{name}：块形态的嵌套 body 在管线侧仍应**更少**（尾部缺 `Return`）。\n\
             若本条失败，说明形状本身被修好了 —— 那时 `d92_*` 的记档与\n\
             `nine_layer_unblocked.rs` 的说明都应同步更新。\n\
             报告：{:?}",
            report
        );
    }
}

/// 对照组：`with` 块**不**触发回落（它的嵌套体两条路径一致）。
///
/// 有了它，上一条的「必须失败」断言才不是「所有程序都失败」的空断言。
#[test]
fn d92_with_block_is_a_control_and_does_not_fall_back() {
    let src = "with model = \"m\"\n  print(1)\nend\n";
    let (func, witnesses) = ParserV3::compile(src).expect("compile");
    let (result, _) = run_pipeline(&func, &witnesses);
    assert!(
        result.differential_ok,
        "对照组：`with` 块应当差分通过（顶层 {} vs {}）",
        result.pipeline_mir_count, result.original_mir_count
    );
}
