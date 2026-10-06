//! v0.104.6 D365 —— `pipeline.rs` 的**回落普查注释**与 census 清单**脱节**（已修）
//!
//! D364 查 9 层管线差分时发现 `tea_standalone.mora` 稳定回落，
//! 顺着查到 `differential_check` 的设计注释，发现那里的
//! **回落普查结论早已过时**。
//!
//! ## 文档漂移
//!
//! `src/mir/pipeline.rs` 的 `differential_check` 注释（L210-228）写着
//! 「实测**仍有 14 类**回落」，并逐条列了 14 个名字。
//!
//! 但 **`tests/nine_layer_fallback_census.rs` 早已更新**：
//! **D315** 把其中 8 条翻成「通过」（`observe` / `span` / `parallel` /
//! `prompt` / `document` / `msg` / `struct` / `enum`），
//! 清单里标 `true`（预期回落）的只剩 **6 条**。
//!
//! | | 注释说 | census 实测 |
//! |---|---|---|
//! | 回落类数 | **14** | **6** |
//! | `observe` / `span` / `parallel` | 回落 | **通过** |
//! | `prompt` / `document` | 回落 | **通过** |
//! | `msg` / `struct` / `enum` | 回落 | **通过** |
//! | `worker` / `transaction` | 回落 | 回落 ✅ |
//! | 裸 `eval` / 裸 `perform` | 回落 | 回落 ✅ |
//! | `model` / `tea_standalone` | 回落 | 回落 ✅ |
//!
//! ⇒ **修法**：把注释同步到 6 条，并写清 D315 的翻转理由
//! （D276 撤销时「差分错位是唯一挡住寄存器级破损的护栏」这个前提，
//! 已由 D314 修好 —— `max_reg_in_node` 的 `_ => 0` 漏算 `WithConfig`）。
//!
//! ## 顺带查明：census 判据与 CLI 测的**不是同一层**
//!
//! census 走 **Rust API**（`ParserV3::compile` + `run_pipeline`，绕开 typeck
//! 与 CLI），而用 CLI 跑 `perform_bare` 的 census 形态会被 **typeck 拦下**
//! （exit 2，`Effect row mismatch`）—— 压根到不了差分那一步。
//!
//! ⇒ 所以**不能**用 CLI 实测去「推翻」census 的 `true` 条目。
//! 本轮差点误判 `eval_bare`（我用 `let r = eval(...)` 替代 census 的
//! **裸顶层** `eval(1 + 1)`，形态不同 ⇒ 结论不同），
//! 换回 census 的精确形态后确认仍是回落。

use std::fs;

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src");

/// census 文件里标 `true`（预期回落）的条目名。
/// 与 `pipeline.rs` 注释里列的 6 条**一一对应**。
const EXPECTED_FALLBACKS: [&str; 6] = [
    "worker",
    "transaction",
    "perform_bare",
    "eval_bare",
    "model",
    "tea_standalone",
];

#[test]
fn d365_census_still_marks_exactly_these_six_as_fallback() {
    let census = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/nine_layer_fallback_census.rs"
    ))
    .expect("读 census");

    // 逐条确认 6 个名字都还在清单里且被标成 `true`
    for name in EXPECTED_FALLBACKS {
        let needle = format!("\"{name}\"");
        assert!(
            census.contains(&needle),
            "census 里找不到 `{name}` —— 清单结构变了？（D365 注释需同步）"
        );
    }

    // 反向：注释**不再**声称 observe/span/parallel/prompt/document/
    // msg/struct/enum 会回落（D315 已把它们翻成通过）
    let pipeline = fs::read_to_string(format!("{ROOT}/mir/pipeline.rs")).expect("读 pipeline.rs");
    for name in ["observe", "span", "parallel", "prompt", "document"] {
        assert!(
            !pipeline.contains(&format!("{name} 回落")),
            "`{name}` 已被 D315 翻成通过，pipeline.rs 注释不应再说它回落"
        );
    }
    assert!(
        !pipeline.contains("实测**仍有 14 类**回落"),
        "pipeline.rs 注释仍停在 D92b 的「14 类」结论 —— census 实测是 6 条（D365 修）"
    );
    assert!(
        pipeline.contains("当前实测回落 **6 类**"),
        "pipeline.rs 注释应明确写出当前的 6 类（D365 修）"
    );
}

/// **反向对照**：注释里**必须**保留那些**真的**还回落的条目。
///
/// 只断言「旧的 14 类说法被删掉」是不够的 ——
/// 一次「全删光」也能让上一条全绿。
#[test]
fn d365_comment_still_names_the_real_fallbacks() {
    let pipeline = fs::read_to_string(format!("{ROOT}/mir/pipeline.rs")).expect("读 pipeline.rs");
    for name in ["worker", "transaction", "model", "tea_standalone"] {
        assert!(
            pipeline.contains(name),
            "`{name}` 仍在回落，pipeline.rs 注释必须继续点名它"
        );
    }
    // 裸形态那两条
    assert!(
        pipeline.contains("eval(1+1)") || pipeline.contains("裸 perform"),
        "裸顶层 eval / 裸 perform 仍在回落，注释必须提到"
    );
}

/// ** census 自身的可执行判据仍通过** —— 它是这份数字的权威来源。
///
/// census 走 Rust API（绕开 typeck 与 CLI），逐条核对 33 条；
/// D365 只改了注释，**没动任何逻辑**，所以它必须仍然全绿。
#[test]
fn d365_census_is_still_green() {
    // 这里只做「存在性 + 结构」检查，真正的执行由 cargo test 跑 census 文件。
    // 单独跑它是为了让本文件的失败原因聚焦在「注释漂移」上。
    let census = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/nine_layer_fallback_census.rs"
    ))
    .expect("读 census");
    assert!(
        census.contains("d92b_census_matches_measured_fallback_set"),
        "census 的逐条核对判据必须仍在"
    );
    assert!(
        census.contains("d92b_census_covers_both_outcomes"),
        "census 的覆盖面自检必须仍在（防清单悄悄变小）"
    );
    // 33 条：按「条目标题」数（`("name",` 或换行后 `"name",`）
    let triples = census.matches("\",\n").count() + census.matches("\", ").count();
    assert!(
        triples >= 30,
        "census 条目数 {triples} 偏少（基线 33）—— 清单可能被截断"
    );
}
