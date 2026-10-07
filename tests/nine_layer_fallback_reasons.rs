//! v0.104.6 D316：钉住**剩下的 6 条回落形态各自「为什么」回落**。
//!
//! ## 为什么需要这条
//!
//! `nine_layer_fallback_census.rs` 已经钉住「哪些形态会回落」（`d92b_census_*`），
//! 但**不钉「为什么」**。后果：一个形态的回落**原因换了**（例如从「管线缺这个
//! 类别」变成「条数对不上」），判据仍然绿 —— 而两者的后续修法完全不同。
//!
//! D316 逐条取 `result.differential_diffs`，为每条仍回落的形态记录一个
//! **判别标记**（diff 文本里那条独有的片段），并断言它仍然存在。
//!
//! ## D316 的分诊结论：4 个互不相同的根因，全部是**功能覆盖缺口**
//!
//! | 形态 | 判别标记 | 根因 |
//! |---|---|---|
//! | `worker` | `original="Worker"` | 9 层降级链里**没有 `Worker` 类别** |
//! | `transaction` | `original="Transaction"` | 同上，**没有 `Transaction`** |
//! | `eval_bare` | `inst count: pipeline=0 original=4` | 管线把 `eval` **整条丢掉**（产出 0 条） |
//! | `model` | `pipeline="ModelDef" original="Const"` | `ModelDef` **缺预分配结果寄存器**与前置 `Const` |
//! | `tea_standalone` | `pipeline="MsgDef" original="ModelDef"` | 同上，TEA 三件套一起错位 |
// | `perform_bare` | `pipeline="Perform" original="Assign"` | emit 侧不发射该 `Perform`（且该程序**本身 typeck exit 2**） |
//!
//! ⇒ 这 4 类**不是缺陷修复**，而是在 9 层管线里**实现新构造**（新功能 / 路线图
//! 决策）。本轮只固化分诊结论，**不实施**。
//!
//! ## D316 顺带量到的代价（供决策参考）
//!
//! 回落**没有可观察的正确性代价**，性能代价也可忽略：
//!
//! | | 管线路径 | emit 路径 |
//! |---|---|---|
//! | 55 个走管线的程序（中位耗时） | 15.8 ms | 15.3 ms |
//! | 唯一回落的 `tea_standalone` | 20.0 ms | 17.9 ms |
//!
//! 回落那次是**管线跑完再被丢弃**，故 2.1 ms 就是白烧的编译时间。

use mora::mir::pipeline::run_pipeline;
use mora::parser_v3::ParserV3;

/// 仍回落的形态 → 判别标记（必须出现在 `differential_diffs` 的某一条里）。
const FALLBACK_REASONS: &[(&str, &str, &str)] = &[
    // 9 层降级链里没有 Worker 类别：管线在 inst[0] 就开始错位。
    (
        "worker",
        "worker w do\n  print(1)\nend\n",
        "original=\"Worker\"",
    ),
    // 同上，Transaction。
    (
        "transaction",
        "transaction\n  print(1)\nend\n",
        "original=\"Transaction\"",
    ),
    // 裸 perform：emit 侧不发射该 Perform。⚠ 该程序本身是 typeck 错误
    // （「Effect row mismatch … wrap in a matching `handle` block」），exit 2。
    (
        "perform_bare",
        "let g = \"i\"\ng = perform Ai(\"x\")\nprint(g)\n",
        "pipeline=\"Perform\" original=\"Assign\"",
    ),
    // v0.104.6 **D412**：`model` 与 `tea_standalone` 两条已从本表**移除** ——
    // 它们不再回落（字段默认值已在入口接进 witness，见 CHANGELOG D412）。
    //
    // ⚠ 这两条的判别标记恰好**独立印证了 D412 的根因诊断**：
    //   `model` 的标记是 `pipeline="ModelDef" original="Const"` ——
    //   即管线的 `ModelDef` 撞上了 emit.rs 侧那条**默认值 Const**，
    //   而不是因为「5 个分支缺 Const(dst, Nil)」（那是 D95 的错误归因）。
    //
    // eval 被整条丢弃：管线产出 0 条指令。
    (
        "eval_bare",
        "eval(1 + 1)\n",
        "inst count: pipeline=0 original=4",
    ),
];

/// **主判据**：每条仍回落的形态，其差分**签名**必须仍是 D316 分诊时记录的那一条。
///
/// 这比「它还在回落」更强：回落**原因换了**也会红。
#[test]
fn d316_fallback_reasons_are_stable() {
    let mut problems = Vec::new();
    let mut report = Vec::new();

    for (name, src, marker) in FALLBACK_REASONS {
        let (func, witnesses) = match ParserV3::compile(src) {
            Ok(v) => v,
            Err(e) => {
                problems.push(format!("{name}: 源码编译失败 —— 形态可能已不存在: {e}"));
                continue;
            }
        };
        let (result, _) = run_pipeline(&func, &witnesses);
        report.push(format!("{name}: {:?}", result.differential_diffs));

        if result.differential_ok {
            problems.push(format!(
                "{name}: 实测**通过**差分 —— 该形态已走上 9 层管线。\n\
                 若这是修复，请同步更新 `nine_layer_fallback_census.rs` 的 \
                 `should_fall_back` 与本表的判别标记（两者必须一起改）。"
            ));
            continue;
        }
        if !result.differential_diffs.iter().any(|d| d.contains(marker)) {
            problems.push(format!(
                "{name}: 仍在回落，但**原因变了** —— 判别标记 `{marker}` 不再出现在\n\
                 differential_diffs 中。\n  实际 diffs: {:?}\n\
                 ⚠ 回落**原因不同、后续修法也不同**（类别缺口 vs 条数错位 vs 顺序错位），\
                 不要只看「还在回落」就认为状态未变。",
                result.differential_diffs
            ));
        }
    }

    assert!(
        problems.is_empty(),
        "回落形态的分诊结论已失效（{} 条）：\n  - {}\n实测 diffs：\n  {}",
        problems.len(),
        problems.join("\n  - "),
        report.join("\n  ")
    );
}

/// 这 4 条**全部**是 9 层管线的功能覆盖缺口，不是缺陷。
///
/// 本条把「普查里仍回落的条目集合」与本文件一致这件事钉住 ——
/// 避免将来有人只改普查、不改根因分诊。
///
/// **D412**：`model` 与 `tea_standalone` 已修（不再回落），从本表移除。
#[test]
fn d316_fallback_set_matches_the_triage() {
    // 与 `nine_layer_fallback_census.rs` 里 `should_fall_back == true` 的条目对应。
    const EXPECTED_FALLBACK: &[&str] = &["worker", "transaction", "perform_bare", "eval_bare"];
    assert_eq!(
        FALLBACK_REASONS.len(),
        EXPECTED_FALLBACK.len(),
        "FALLBACK_REASONS 与 EXPECTED_FALLBACK 条数不符"
    );
    for (name, _, _) in FALLBACK_REASONS {
        assert!(
            EXPECTED_FALLBACK.contains(name),
            "`{name}` 出现在 FALLBACK_REASONS 但不在 EXPECTED_FALLBACK —— \
             两条清单必须同步（普查表的 `should_fall_back` 也算一份）"
        );
    }
    for n in EXPECTED_FALLBACK {
        assert!(
            FALLBACK_REASONS.iter().any(|(m, _, _)| m == n),
            "`{n}` 在 EXPECTED_FALLBACK 但本文件没有它的分诊记录"
        );
    }
}
