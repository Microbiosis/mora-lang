//! v0.90: pipeline — 9 层 IR 管线驱动 + 差分验证。
//!
//! 生产管线切换的核心：把 9 层架构从"独立代码岛"接入真实编译路径。
//!
//! 管线（每次编译全量运行）：
//!   Token → ParserV3::compile() → MirFunction + MirWitness[]   \[现有\]
//!     ↓
//!   witness_to_fcfg()      → Vec<Node<()>>          \[FCFG\]
//!     ↓
//!   export_type_table()    → TypeTable              \[影子表\]
//!   annotate()             → Vec<Node`<TypeInfo>`>   \[EHIR\]
//!     ↓
//!   ehir_to_core()         → CoreFunction           \[Core\]
//!     ↓
//!   core_to_cmir()         → CmirBlock              \[CMIR\]
//!     ↓
//!   cmir_to_lmir()         → LmirInst[] + layouts   \[LMIR\]
//!     ↓
//!   populate_layout_table()→ LayoutTable            [RIR 桥]
//!     ↓
//!   差分验证: lower_fcfg(fcfg) vs 原 MirFunction 指令形状等价
//!
//! 执行器仍消费原 MirFunction（Phase 2 切换，待差分验证全绿后）。

use crate::mir::MirFunction;
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
use crate::typeck::annotate::annotate;
use crate::typeck::export::{TypeTable, export_type_table};

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
    let (differential_ok, differential_diffs) = differential_check(&pipeline_body, &func.body);

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

/// v0.104.6 D57：剔除**可证明的死 no-op** 后的指令类别序列。
///
/// 判据：该指令是 `Const(r, Nil)`，且寄存器 `r` 在**同一函数体内**从未被
/// 任何**其他**指令读入。这样的写入对程序可观察行为**零影响** —— 正是死
/// 代码消除所做的事，而 9 层管线本身就是一个 DCE 过程。
///
/// emit 路径在每条**语句型**指令（`Parallel` / `Worker` / `ModelDef` /
/// `MsgDef` / `Transaction` …）后都会补一条这样的 `Const`，因为语句必须
/// 有值；而那个值无人读。管线不补（它直接用节点的结果寄存器），差异因此
/// 只有条数、无任何类别差异。
///
/// **为什么不无条件忽略条数**：那会让「管线整段漏掉一条语句」蒙混过关。
/// 中间漏一条会让后续类别序列整体错位、被逐条比较抓到；但**尾部**漏一条
/// 只体现为条数不同。剔除死 no-op 恰好只放过后者，不放过前者。
/// 保留**非死 no-op** 指令的下标（v0.104.6 D276：抽出供 `nested_diffs` 复用）。
///
/// 判据见 [`significant_categories`]：该指令是 `Const(r, Nil)` 且寄存器 `r`
/// 在**同一函数体内**从未被任何**其他**指令读入。
///
/// 抽出成「下标」而非「类别」是因为 `nested_diffs` 需要**按下标**回到
/// `pipeline[i]` / `original[i]` 去识别 `TaskDef` / `Handle` / `MatchExpr`
/// 并递归其 body。
fn significant_indices(body: &[crate::mir::MirInst]) -> Vec<usize> {
    let mut read: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for inst in body {
        for r in inst.input_regs() {
            read.insert(r);
        }
    }
    body.iter()
        .enumerate()
        .filter(|(_, inst)| {
            let dead_nil = match inst {
                crate::mir::MirInst::Const(r, v) => {
                    matches!(v, crate::value::Value::Nil) && !read.contains(r)
                }
                _ => false,
            };
            !dead_nil
        })
        .map(|(i, _)| i)
        .collect()
}

fn significant_categories(body: &[crate::mir::MirInst]) -> Vec<String> {
    significant_indices(body)
        .into_iter()
        .map(|i| inst_category(&body[i]).to_string())
        .collect()
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

    // 长度差异。
    //
    // 注释原先写着「信息性，不直接判失败」，实现却 push 进 `diffs`
    // → 触发回落。**这个矛盾在 v0.104.6 D58 修好之前是有用的**：当时
    // 9 类常见构造每次都因此回落，而其中 `with` 的管线产出**真的错了**
    // （返回配置绑定值而非块结果）—— 那条条数差异实际上在替生产挡住
    // 一个坏管线输出。差分检查只比「指令类别」不比值，结构上抓不到
    // 那一类分叉。
    //
    // v0.104.6 D57（**在 D58 修好之后**重做）：现在放开长度差异是安全的，
    // 做法**不是**无条件忽略（那会让「管线整段漏掉一条语句」蒙混过关 ——
    // 中间漏一条会让后续类别序列整体错位、被逐条比较抓到，但**尾部**漏一条
    // 只体现为条数不同），而是**剔除可证明的死 no-op 再比**：判据是
    // 「`Const(r, Nil)` 且 `r` 在同一函数体内从未被任何其他指令读」——
    // 正是死代码消除的语义，而 9 层管线本身就是一个 DCE 过程。
    //
    // 效果：8 类构造（parallel / observe / model / msg / struct / enum /
    // transaction / worker / eval）不再因这类冗余而永久放弃
    // DAG 分析 / CSE / 贪心重写。`transaction` / `worker` 仍会回落 ——
    // 它们是**类别**就不同（管线根本没降出 `Transaction` 指令，
    // `Transaction` 在整个管线降级链里不存在），那是真实缺口，回落是正确的。
    //
    // ⚠ v0.104.6 D92b 更正 + D365 追认：本段原话**只对 `eval` 成立**。
    // 用真实 CLI 逐条普查 33 类构造（spec §14.2 全部 statement 产生式 +
    // TEA 独立声明 + 字面量/表达式形态）。
    //
    // v0.104.6 D365 更正：**本段曾长期停在 D92b 的「14 类」结论上，
    // 而 D315 已把其中 8 条翻成通过**（`observe` / `span` / `parallel` /
    // `prompt` / `document` / `msg` / `struct` / `enum`）——
    // 判据与 census 清单都更新了，**唯独这段注释没跟上**。
    //
    // 当前实测回落 **6 类**（`tests/nine_layer_fallback_census.rs` 逐条核对）：
    //
    //   类别缺口（2）：worker / transaction
    //     —— `Transaction` 在整个降级链里**不存在**，是真缺口
    //   裸顶层形态（2）：`eval` 顶层 `eval(1+1)` / 无 handler 的裸 perform
    //   声明形态（2）：`model` 单独形态 / `tea_standalone`（model+msg+update 组合体）
    //
    // D315 的翻转理由见 census 清单的注释：它**补上了证据** ——
    // D276 撤销时「差分错位是唯一挡住寄存器级破损的护栏」这个前提，
    // 已由 D314 修好（`max_reg_in_node` 的 `_ => 0` 漏算 `WithConfig`）。
    //
    // 本段当时只量了 task / handle / match / with 的嵌套差异（那几类确实为 0），
    // **没量块形态与声明形态** —— 后两者的嵌套体/顶层由
    // `emit_block_mir_and_wit` 与 `emit_definitions` 生成，形状与前者不同。
    //
    // 完整可执行清单见 `tests/nine_layer_fallback_census.rs`（33 条，逐条核对）。
    // **注意：仍在回落的 6 类没有差分等价性 fixture 覆盖**
    // （`tests/nine_layer_differential.rs` 的 19 条里一条都没有），
    // 故「放宽差分让它们走 9 层」= 无人验证，**不得在无 fixture 时打开**。
    let p_sig = significant_categories(pipeline);
    let o_sig = significant_categories(original);
    if p_sig.len() != o_sig.len() {
        diffs.push(format!(
            "inst count: pipeline={} original={} (delta={})",
            pipeline.len(),
            original.len(),
            pipeline.len() as isize - original.len() as isize
        ));
    }

    // 逐指令类别比较（取**剔除死 no-op 后**较短长度的前缀）
    let n = p_sig.len().min(o_sig.len());
    for i in 0..n {
        if p_sig[i] != o_sig[i] {
            diffs.push(format!(
                "inst[{}]: pipeline={:?} original={:?}",
                i, p_sig[i], o_sig[i]
            ));
            if diffs.len() > 10 {
                diffs.push("... (truncated)".to_string());
                break;
            }
        }
    }

    // v0.104.6 D36：嵌套 `MirFunction` 内部的差分。
    //
    // 差分检查此前**只比顶层**，对 `TaskDef` body、`Handle` 的 body/handler、
    // `MatchExpr` 各 arm 的 guard/body **结构性失明** —— D36 就藏在这里：
    // 两条路径的顶层序列**完全一致**（各 14 条、`Define` 都与各自 `k_dst`
    // 相符），差异只在 body 一条 vs 两条，于是差分永远报「通过」。
    //
    // 纳入判定前先量过：8 类含嵌套的构造（flat / task / 嵌套 task /
    // handle / 嵌套 handle / match / 带 guard 的 match / with）**嵌套层面
    // 零差异** —— D36 的删除已让两条路径真正对齐，所以这一步不再像当初
    // 担心的那样立刻触发回落、把「一条路径错」变成「生产路径错」。
    let mut nested = Vec::new();
    nested_diffs(pipeline, original, "top", &mut nested);
    if !nested.is_empty() {
        // 完整清单按需打印（D38 已把「回落」默认可见化，详见 `cli::compile_and_opt`）
        if std::env::var("MORA_DBG_NEST").is_ok() {
            eprintln!("@@N@@ nested_diffs={} {:?}", nested.len(), nested);
        }
        // 详情受 diffs 长度上限保护，这里只并入计数 + 首条
        diffs.push(format!(
            "nested MirFunction shape differs ({} diffs), e.g. {}",
            nested.len(),
            nested[0]
        ));
    }

    // v0.104.6 D36 盲区 1：寄存器级审计（**opt-in、只诊断、不判失败**）。
    //
    // 差分检查按设计不比较寄存器号，于是「两条路径类别序列完全相同、只是
    // `Define("r", ·)` 引用了不同寄存器」这类差异**永远报「通过」**。
    //
    // **为什么不直接纳入判定**：一旦某个程序的 emit 侧寄存器绑错，判失败
    // 会**触发回落** → 生产路径改用本来就差的 emit 产出 —— 把「一条路径
    // 错」变成「生产路径错」。所以先做**只诊断**：把「被引用的寄存器在
    // 本函数体内没有生产者」这类不一致**打出来并计数**，**不改
    // `differential_ok`**。等它证明当前代码库干净（或暴露出的每处都被
    // 单独修好）之后，再谈纳入判定。
    //
    // **默认关闭（`MORA_AUDIT_REG=1` 开启）**：本函数在**每次编译**时都会
    // 被调用，而 `MirInst::input_regs()` 每次调用都**分配一个新 `Vec`**，
    // 加上对所有嵌套结构逐个递归 —— 开启会让编译慢一个量级。诊断用途
    // 按需开启即可，不该进生产路径。
    //
    // 注意**嵌套 `MirFunction`（handle body/handler）是独立寄存器空间**，
    // 其生产者不能用来满足外层引用 —— 故每个函数体各查各的。
    if std::env::var("MORA_AUDIT_REG").is_ok() {
        let mut reg_issues = Vec::new();
        audit_reg_bindings(pipeline, "emit", &mut reg_issues);
        audit_reg_bindings(original, "pipeline", &mut reg_issues);
        if !reg_issues.is_empty() {
            eprintln!(
                "[9layer] 寄存器绑定审计：{} 处「被引用但本函数体内无生产者」（仅诊断，未判失败）| 详情设 MORA_DBG_REG=1",
                reg_issues.len()
            );
            if std::env::var("MORA_DBG_REG").is_ok() {
                for r in reg_issues.iter().take(20) {
                    eprintln!("[9layer]   {}", r);
                }
            }
        }
    }

    (diffs.is_empty(), diffs)
}

/// 递归比较两条路径里**所有嵌套 `MirFunction`** 的指令类别序列。
///
/// 嵌套 `MirFunction` 是**独立寄存器空间**（handle body/handler、task body、
/// match arm 的 guard/body），故逐个独立比较，不与外层混算。
///
/// 消息格式 `top[0].task.body[0]: "Const" vs "Transaction"` —— 与
/// `MORA_DBG_NEST=1` 的输出同形，便于直接对照。
///
/// v0.104.6 D276：**按「剔除死 no-op 后」的下标对齐**，与顶层比较同基准。
///
/// 此前这里用的是**原始**长度与**原始**下标，而顶层比较用的是
/// [`significant_categories`]（已剔除死 `Const(r, Nil)`）。两套基准不一致
/// ⇒ 只要 emit 侧在某条语句型指令后多补了一条死 `Const`（D57 注释里
/// 明确记载了它会补），后续下标就**整体错位**，每一条都在拿**无关指令**
/// 互比，于是报出**假的**「shape differs」。
///
/// 实测（`rel_empty.mora`）：pipeline = `[RelDef, Solve]`、
/// original = `[RelDef, Const, Solve]`，下标 1 上 `Solve` 对上 `Const`
/// ⇒ 判失败、**9 层产出被丢弃**，而两条路径其实只差一条死 no-op。
///
/// 为什么这不是「放宽判据」：死 no-op 的剔除正是 D57 立这条过滤的**既定
/// 契约**（「差异因此只有条数、无任何类别差异」）。本函数此前**违反**了它。
/// 真正的语义差异在对齐之后依然会被逐条抓到（只是不再与 no-op 混在一起）。
fn nested_diffs(
    pipeline: &[crate::mir::MirInst],
    original: &[crate::mir::MirInst],
    label: &str,
    out: &mut Vec<String>,
) {
    use crate::mir::MirInst;
    // v0.104.6 D315：**重新应用 D276**（D276 曾做过一次、随即被撤销）。
    //
    // 撤销理由（D276 原文，本轮已不成立）：对齐后 `rel_*.mora` 不再回落 ⇒
    // 改走 9 层路径，而**那条路径是坏的**：
    //   `Runtime error (MIR): internal: instruction at DAG node 5
    //    references register 4 but the function only has 1 register(s)`
    // 当时这段「错位」是 `rel_*` **唯一的护栏**，靠条数差异把寄存器级破损
    // 挡在生产之外。D276 据此写下「在 9 层 rel 路径修好之前，必须保留错位」。
    //
    // **D314 修好了那个前提**：`fcfg_lower::max_reg_in_node` 的 `_ => 0`
    // 覆盖了 50 个 `Node` 变体里的 25 个（`Solve` / `Return` /
    // `WithConfig` 漏算）⇒ `n_regs` 少算。D314 把它改成穷尽 match +
    // `n_regs` 取「已发射指令寄存器」下界，实测强制走管线时
    // **11 个回落程序的行为改变 7/11 → 0/11**。
    //
    // 本轮**补上了 D276 当时缺的那块证据**：`rel_*.mora` 自身无 `print`，
    // 可观察行为只有「exit 0、无输出」，比不出对错。故另写了一个**打印
    // 求解结果**的强证人（`rel edge × 3` + 两个 `solve`），实测两条路径
    // 输出**逐行相同**：
    //     edges:    [[a, b], [b, c], [c, d]]
    //     reach-d:  [[c]]
    //     done
    // 差异恰好是 3 条**死 no-op**（每条 `RelDef` 语句后一条 `Const(r, Nil)`，
    // r = 0/1/2 且从未被读）—— 正是 D57 立过滤时描述的那一种：
    //     emit 路径在每条**语句型**指令后补 `Const(dst, Nil)`，因为语句必须
    //     有值；而那个值无人读。管线不补（它直接用节点的结果寄存器）。
    //
    // 为什么这不是「放宽判据」：死 no-op 的剔除正是 D57 的**既定契约**，
    // D276 撤销前也是这么论证的。剔除后两侧 **23 vs 23 逐条按类别一一对齐**
    // —— 也就是说，**除那 3 条死 no-op 外没有任何别的差异**。真正的语义差异
    // 在对齐之后依然会被逐条抓到（只是不再与 no-op 混在一起）。
    let p_keep: Vec<usize> = significant_indices(pipeline);
    let o_keep: Vec<usize> = significant_indices(original);
    if p_keep.len() != o_keep.len() {
        out.push(format!(
            "{label}: inst count pipeline={} original={} (剔除死 no-op 后)",
            p_keep.len(),
            o_keep.len()
        ));
    }
    let n = p_keep.len().min(o_keep.len());
    for k in 0..n {
        let i = p_keep[k];
        let j = o_keep[k];
        // 任务体
        if let (MirInst::TaskDef { body: pb, .. }, MirInst::TaskDef { body: ob, .. }) =
            (&pipeline[i], &original[j])
        {
            nested_diffs(&pb.body, &ob.body, &format!("{label}[{i}].task.body"), out);
        }
        // handle body / handler
        if let (
            MirInst::Handle {
                body: pb,
                handler: ph,
                ..
            },
            MirInst::Handle {
                body: ob,
                handler: oh,
                ..
            },
        ) = (&pipeline[i], &original[j])
        {
            nested_diffs(
                &pb.body,
                &ob.body,
                &format!("{label}[{i}].handle.body"),
                out,
            );
            nested_diffs(
                &ph.body,
                &oh.body,
                &format!("{label}[{i}].handle.handler"),
                out,
            );
        }
        // match arm：guard 与 body
        if let (MirInst::MatchExpr { arms: pa, .. }, MirInst::MatchExpr { arms: oa, .. }) =
            (&pipeline[i], &original[j])
        {
            for (k, ((_, pg, pb, _), (_, og, ob, _))) in pa.iter().zip(oa.iter()).enumerate() {
                if let (Some(pg), Some(og)) = (pg, og) {
                    nested_diffs(
                        &pg.body,
                        &og.body,
                        &format!("{label}[{i}].match.arm{k}.guard"),
                        out,
                    );
                }
                nested_diffs(
                    &pb.body,
                    &ob.body,
                    &format!("{label}[{i}].match.arm{k}.body"),
                    out,
                );
            }
        }
        // 本层自身：类别序列
        let p = inst_category(&pipeline[i]);
        let o = inst_category(&original[j]);
        if p != o {
            out.push(format!("{label}[{i}]: {:?} vs {:?}", p, o));
        }
    }
    // v0.104.6 D276：条数差异已在函数开头按**剔除死 no-op 后**的
    // `p_keep.len() != o_keep.len()` 记过一条。此处**不再**用原始长度
    // 补一条 —— 那正是让「emit 多补一条死 Const」重新变成失败的元凶。
}

/// 诊断用：对一个函数体，检查每条指令的 `input_regs()` 是否在本函数体内
/// 有生产者（某条指令的 `dst()` 或 `written_reg()` 返回同一寄存器）。
///
/// **用 `written_reg()` 而非 `dst()`**：v0.104.6 D35 之后 `Handle` 被
/// `is_effect()` 抢先归类为 Effect 节点（`dst()` 够不着它的 `k_dst`），
/// 但 `h_handle` 确实写 `regs[k_dst] = v`。漏掉这一条会让审计把
/// `let r = handle …` 的合法引用误报成「无生产者」。
///
/// **嵌套 `MirFunction` 是独立寄存器空间**，其生产者不能用来满足外层引用
/// —— 故递归进入每个 body 各查各的。
fn audit_reg_bindings(body: &[crate::mir::MirInst], which: &str, out: &mut Vec<String>) {
    use crate::mir::MirInst;
    use std::collections::HashSet;
    let mut produced: HashSet<usize> = HashSet::new();
    for inst in body {
        if let Some(d) = inst.dst() {
            produced.insert(d);
        }
        if let Some(w) = inst.written_reg() {
            produced.insert(w);
        }
    }
    for (i, inst) in body.iter().enumerate() {
        for r in inst.input_regs() {
            if !produced.contains(&r) {
                out.push(format!(
                    "{which}[{}] {} 引用 reg {} 但本函数体内无生产者",
                    i,
                    inst_category(inst),
                    r
                ));
            }
        }
        // 递归进入嵌套函数体（独立寄存器空间）
        match inst {
            MirInst::TaskDef { body, .. } => audit_reg_bindings(&body.body, which, out),
            MirInst::Handle { body, handler, .. } => {
                audit_reg_bindings(&body.body, which, out);
                audit_reg_bindings(&handler.body, which, out);
            }
            MirInst::MatchExpr { arms, .. } => {
                for (_, guard, arm_body, _) in arms {
                    if let Some(g) = guard {
                        audit_reg_bindings(&g.body, which, out);
                    }
                    audit_reg_bindings(&arm_body.body, which, out);
                }
            }
            _ => {}
        }
    }
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
        // 顶层字面量 → LMIR 布局表填充（task 内字面量在嵌套闭包体中）
        let src = "let x = 42i\nlet y = 3.5\nx";
        let (func, witnesses) = crate::parser_v3::ParserV3::compile(src).unwrap();
        let (result, pipeline_func) = run_pipeline(&func, &witnesses);
        assert!(result.fcfg_nodes > 0, "FCFG should be non-empty");
        assert!(result.core_insts > 0, "Core should be non-empty");
        assert!(
            result.layouts > 0,
            "LayoutTable should be populated (top-level Int/Float consts)"
        );
        assert!(
            !pipeline_func.body.is_empty(),
            "pipeline MirFunction should be non-empty"
        );
    }

    #[test]
    fn pipeline_task_body() {
        // 嵌套函数体（task 内）— FCFG/Core 仍应非空
        let src = "task main()\n  print(10i + 32i)\nend";
        let (func, witnesses) = crate::parser_v3::ParserV3::compile(src).unwrap();
        let (result, pipeline_func) = run_pipeline(&func, &witnesses);
        assert!(
            result.fcfg_nodes > 0,
            "FCFG should be non-empty (task body)"
        );
        assert!(
            result.core_insts > 0,
            "Core should be non-empty (closure create)"
        );
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
