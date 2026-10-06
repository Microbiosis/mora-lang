//! v0.104.6 D128：同一处类型冲突被**报两次**（bidirectional + HM 两层）。
//!
//! ## 现象
//!
//! ```mora
//! let a = 1
//! let b: string = a
//! ```
//!
//! ```text
//! Type error at line 2:17: type mismatch: expected `String`, got `Float`   ← bidirectional
//! Type error at line 2:1:  Type mismatch: expected String, got Float         ← HM
//! ```
//!
//! 两层讲的是**同一件事**（同一个 `let` 标注冲突），措辞与位置都不同，
//! 用户会以为有两个错误。
//!
//! ## 根因：已有的去重按 (line, column)，而两层**位置不同**
//!
//! `check_program_witnesses_bidirectional` 早就有去重机制
//! （`diag.is_diagnosed_at_line_column`，v0.75.94），但按的是 **line+column**：
//!
//! - bidirectional 把错误标在**值**上（`a` → column 17）
//! - HM 把它标在**整条 let 语句**上（column 1）
//!
//! 二元组对不上 → 过滤失效 → 两条都留下。两层的 `expected` / `actual`
//! 文本其实**一致**，只有位置不同。
//!
//! ## 修法：补一条 (line, expected, actual) 三元组去重
//!
//! 比「只比 line」保守 —— 同一行内**不同**类型对的冲突仍各自保留。

use mora::cli::compile_and_opt;
use mora::typeck::check_mir::check_program_witnesses_bidirectional;
use mora::typeck::format_error;

fn diagnostics(src: &str) -> Vec<String> {
    let (_f, wits) = compile_and_opt(src, None).expect("应能编译");
    check_program_witnesses_bidirectional(&wits)
        .iter()
        .map(format_error)
        .collect()
}

/// 主判据：`let` 标注冲突**只报一次**。
#[test]
fn d128_let_annotation_conflict_is_reported_once() {
    let errs = diagnostics("let a = 1\nlet b: string = a\nprint(b)\n");
    assert_eq!(
        errs.len(),
        1,
        "同一处 let 标注冲突被报 {} 次（bidirectional + HM 两层各一次）; 实际: {errs:?}",
        errs.len()
    );
    // 保留的那条应是 bidirectional 的 —— 位置指向**出问题的值**（`a`），更精确
    assert!(
        errs[0].contains("type mismatch"),
        "应保留 bidirectional 的诊断（措辞带反引号、位置指向值）; 实际: {}",
        errs[0]
    );
    assert!(
        errs[0].contains("String") && errs[0].contains("Float"),
        "诊断应给出两种类型; 实际: {}",
        errs[0]
    );
}

/// 反向对照：二元运算的类型冲突**不受影响**（只报一条，一直如此）。
///
/// 这类冲突是 `infer_binop` 当场检查的，不经入队约束，两层不重复。
#[test]
fn d128_binary_op_conflicts_still_report_once() {
    for (name, src) in [
        ("add", "let a = 1\nlet s = \"x\"\nprint(a + s)\n"),
        ("eq", "let a = 1\nlet s = \"x\"\nprint(a == s)\n"),
    ] {
        let errs = diagnostics(src);
        assert_eq!(
            errs.len(),
            1,
            "[{name}] 二元运算冲突应恰好一条（不受本次去重影响）; 实际: {errs:?}"
        );
    }
}

/// 反向对照：list 字面量同质冲突（D125 引入的专门变体）仍**恰好一条**。
///
/// 它由 `infer_list` 当场检查并带 span，不该被新的三元组去重误伤。
#[test]
fn d128_list_homogeneity_diagnostic_survives_dedup() {
    let errs = diagnostics("let xs = [1, \"x\"]\nprint(xs)\n");
    assert_eq!(
        errs.len(),
        1,
        "list 同质冲突应恰好一条（D125 契约）; 实际: {errs:?}"
    );
    assert!(
        errs[0].contains("同质") && errs[0].contains("下标 1"),
        "D125 的专门诊断（说明约束 + 下标）必须保留; 实际: {}",
        errs[0]
    );
}

/// 反向对照：dict 字面量同质冲突（D124）同样保留。
#[test]
fn d128_dict_homogeneity_diagnostic_survives_dedup() {
    let errs = diagnostics("let d = {a: 1, b: \"x\"}\nprint(d)\n");
    assert_eq!(
        errs.len(),
        1,
        "dict 同质冲突应恰好一条（D124 契约）; 实际: {errs:?}"
    );
    assert!(
        errs[0].contains("同质") && errs[0].contains("'b'"),
        "D124 的专门诊断（说明约束 + 键名）必须保留; 实际: {}",
        errs[0]
    );
}

/// ⚠ 三元组去重的**保守性**钉住：同一行内**不同**类型对的冲突
/// 不得被合并成一条 —— 若去重只比 `line`，这条会只剩 1 条。
#[test]
fn d128_dedup_does_not_merge_different_conflicts_on_the_same_line() {
    // 两个不同类型的标注冲突放在同一行
    let src = "let a = 1\nlet s = \"x\"\nlet p: string = a\nlet q: number = s\nprint(p, q)\n";
    let errs = diagnostics(src);
    assert!(
        errs.len() >= 2,
        "两处**不同**的标注冲突（String←Float 与 Number←String）应各自保留; \
         实际 {} 条: {errs:?}",
        errs.len()
    );
}
