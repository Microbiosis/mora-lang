//! v0.104.6 D125：`list` **字面量**同质约束的诊断质量（dict 的姊妹约束）。
//!
//! D124 改了 dict 侧的诊断后，顺手查它的姊妹形态 —— 发现问题**完全同构**：
//!
//! ```mora
//! let a = [1, "x"]
//! ```
//!
//! 修复前：
//! ```text
//! Type error at line 1:9: Type mismatch: expected Float, got String at line 1, column 9
//! ```
//!
//! 同样两个问题：
//! 1. **没说清为什么** —— `list<T>` 需要单一 `T`，而用户看到「类型不匹配」
//!    只会以为自己写错了元素类型。
//! 2. **span 指向整个列表的 `[`**（column 9），不是出问题的那个元素。
//!
//! 嵌套形态 `[[1], ["x"]]` 也一样（旧消息给的是
//! `expected List(Float), got List(String)`，更让人摸不着头脑）。

use mora::parser_v3::ParserV3;
use mora::typeck::check_mir::check_program_witnesses_bidirectional;

/// 一条诊断的「消息 + 位置」。
///
/// ⚠ 对外的 `typeck::TypeError` 是**结构体**（无 `Display`），CLI 的
/// `format_error` 才渲染成 `Type error at line L:C: <message>`。
/// 取字段，**不**拼字符串 —— 拼了就把判据绑死在渲染格式上。
fn typeck_errors(src: &str) -> Vec<(String, usize, usize)> {
    let (_f, wits) = ParserV3::compile(src).expect("语法应通过");
    check_program_witnesses_bidirectional(&wits)
        .iter()
        .map(|e| (e.message.clone(), e.line, e.column))
        .collect()
}

/// 诊断必须**说清约束 + 给出下标**，而不是笼统的「类型不匹配」。
#[test]
fn d125_list_heterogeneity_error_explains_the_constraint_and_gives_the_index() {
    let errs = typeck_errors("let a = [1, \"x\"]\nprint(a)\n");
    assert_eq!(errs.len(), 1, "应恰好一条错误，实际: {errs:?}");
    let (msg, _, _) = &errs[0];
    assert!(
        msg.contains("同质"),
        "诊断必须说明「元素必须同质」这条约束: {msg}"
    );
    assert!(
        msg.contains("list<T>") || msg.contains("list<T>"),
        "诊断应点明约束来自 `list<T>` 的单一 T: {msg}"
    );
    assert!(
        msg.contains("下标 1"),
        "诊断应给出 0-based 下标 1（用户可直接用于 `list[1]`）: {msg}"
    );
    assert!(
        msg.contains("第 2 个元素"),
        "诊断应同时给出 1-based 序数（只给下标时「第 0 个」读着别扭）: {msg}"
    );
    assert!(
        msg.contains("Float") && msg.contains("String"),
        "诊断应同时给出两种类型: {msg}"
    );
    assert!(
        !msg.contains("Type mismatch: expected"),
        "旧的通用 `UnificationFailure` 措辞不应再用于 list 同质约束: {msg}"
    );
    assert!(
        !msg.contains("at line"),
        "消息里不应重复位置（`format_error` 会加 `at line L:C`）: {msg}"
    );
}

/// span 必须指向**出问题的那个元素**，不是整个列表的 `[`。
#[test]
fn d125_list_heterogeneity_error_points_at_the_offending_element() {
    let errs = typeck_errors("let a = [1, \"x\"]\nprint(a)\n");
    let (_, line, col) = &errs[0];
    assert_eq!(*line, 1, "错误应在第 1 行");
    // 旧实现指向列表起始的 `[`（line 1, column 9）
    assert!(
        *col > 9,
        "span 应指向出问题的元素，而不是列表起始的 `[`（column 9）; 实测 column {col}"
    );
}

/// 嵌套形态也要给清晰诊断（旧消息是 `expected List(Float), got List(String)`）。
#[test]
fn d125_nested_list_heterogeneity_is_also_explained() {
    let errs = typeck_errors("let a = [[1], [\"x\"]]\nprint(a)\n");
    assert_eq!(errs.len(), 1, "应恰好一条错误，实际: {errs:?}");
    let (msg, _, col) = &errs[0];
    assert!(msg.contains("同质"), "嵌套形态同样须说明约束: {msg}");
    assert!(msg.contains("下标 1"), "嵌套形态同样须给出下标: {msg}");
    assert!(
        *col > 9,
        "嵌套形态的 span 同样应指向出问题的元素; 实测 column {col}"
    );
}

/// 对照组：**同质** list 一律无错（防「一刀切」把合法写法也拒了）。
#[test]
fn d125_homogeneous_lists_are_still_accepted() {
    for (name, src) in [
        ("single", "let a = [1]\nprint(a)\n"),
        ("two_same_type", "let a = [1, 2]\nprint(a)\n"),
        ("empty", "let a = []\nprint(a)\n"),
        ("strings", "let a = [\"a\", \"b\"]\nprint(a)\n"),
        // 数值塔提升（D67）：`[1, 2.5]` 提升为 Float
        ("numeric_promotion", "let a = [1, 2.5]\nprint(a)\n"),
        // 嵌套同质
        ("nested", "let a = [[1], [2]]\nprint(a)\n"),
    ] {
        let errs = typeck_errors(src);
        assert!(
            errs.is_empty(),
            "[{name}] 同质 list 不应报错，实际: {errs:?}"
        );
    }
}

/// 反向对照：dict 的诊断与 list 的**互不串味**（D124 / D125 各管各的）。
#[test]
fn d125_dict_and_list_diagnostics_stay_distinct() {
    let list_err = &typeck_errors("let a = [1, \"x\"]\nprint(a)\n")[0].0;
    let dict_err = &typeck_errors("let d = {k: 1, s: \"x\"}\nprint(d)\n")[0].0;
    assert!(
        !list_err.contains("Dict"),
        "list 的诊断不应提到 Dict: {list_err}"
    );
    assert!(
        !dict_err.contains("List 字面量"),
        "dict 的诊断不应提到 List: {dict_err}"
    );
    assert!(
        list_err.contains("List"),
        "list 诊断应自称 List: {list_err}"
    );
    assert!(
        dict_err.contains("Dict"),
        "dict 诊断应自称 Dict: {dict_err}"
    );
}
