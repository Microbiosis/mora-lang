//! v0.104.6 D126：诊断里的「line 0」是误导性的，改为诚实标注「位置未跟踪」。
//!
//! ## 现象
//!
//! ```mora
//! let d = {a: 1}
//! let e = d.set("b", "text")
//! ```
//!
//! ```text
//! Type error at line 0: Type mismatch: expected float, got string
//! ```
//!
//! **源码没有第 0 行。** 用户看到这个会以为是编辑器行号错乱或自己数错了行。
//!
//! ## 根因
//!
//! `typeck::hm::Constraint`（`Eq` / `Numeric` / `RowEq`）**不携带 span** ——
//! 约束先入队、最后由 `solve_constraints` 统一求解，于是 `unify` 失败时
//! 只能报 `span: None`，一路渲染成 `line 0`。
//!
//! ## 影响面（有位置 / 无位置的分界很清楚）
//!
//! | 场景 | 位置 | 路径 |
//! |---|---|---|
//! | `a == s` / `a + s` | ✓ line 3 | `infer_binop` **当场**检查并带 span |
//! | `d.set("b", "text")` | ✗ line 0 | 方法实参 → 入队约束 → unify 失败 |
//! | `let r: string = f(1)` | ✗ line 0 | 标注与调用结果 → 入队约束 → unify 失败 |
//!
//! 分界就是**「当场检查」还是「入队后统一求解」**。

use mora::cli::compile_and_opt;
use mora::typeck::check_mir::check_program_witnesses_bidirectional;
use mora::typeck::format_error;

/// 编译 + typeck，返回渲染后的诊断文本（走真实的 `format_error`）。
fn diagnostics(src: &str) -> Vec<String> {
    let (_f, wits) = compile_and_opt(src, None).expect("应能编译");
    check_program_witnesses_bidirectional(&wits)
        .iter()
        .map(format_error)
        .collect()
}

/// 约束求解失败时**不得**输出误导性的「line 0」。
/// ℹ 本函数原先还有第二条用例「`dict.set` 异质 → 无位置诊断」，
/// **v0.104.6 D127 修复后其前提已消失** —— `set` 的 `val` 形参按 spec
/// 是 `any`，异质值现在**合法**，不再产生任何诊断。
///
/// 故移除该用例而非换一个凑数的：它在本文件里的存在价值只是「给 `line 0`
/// 找一个触发场景」，而那个场景本身是个**假阳性**，本不该被当作
/// `line 0` 的证据。D127 的 `tests/dict_set_signature.rs` 是那条路径的现役判据。
///
/// 另注：本函数只断言「每条诊断都不含 line 0 且都标注位置未跟踪」，
/// **不断言条数** —— 同一冲突目前会被报多次（既有的重复诊断缺陷，
/// 已记入 CHANGELOG D126 / D127 待办），条数不是本轮的契约。
#[test]
fn d126_constraint_conflicts_never_report_line_zero() {
    for (name, src) in [(
        // 标注 vs 闭包调用的结果类型 —— 走「入队约束 → unify」路径
        "annotation_vs_call_result",
        "let f = fn(x) x end\nlet r: string = f(1)\nprint(r)\n",
    )] {
        let errs = diagnostics(src);
        assert!(!errs.is_empty(), "[{name}] 应有诊断");
        for e in &errs {
            assert!(
                !e.contains("line 0"),
                "[{name}] 诊断不得出现误导性的「line 0」（源码没有第 0 行）: {e}"
            );
            assert!(
                e.contains("位置未跟踪"),
                "[{name}] 无位置的诊断应明确标注「位置未跟踪」: {e}"
            );
        }
    }
}

/// D127 修复后：`dict.set` 的异质值**不再是**约束冲突的来源。
///
/// 这条锁住「D127 修掉了 D126 里那个触发场景」这个事实 ——
/// 若日后 `set` 的 `val` 又被绑回 `V`，本测试会红并提醒同步更新上方注释。
#[test]
fn d126_dict_set_is_no_longer_a_source_of_constraint_conflicts() {
    let errs = diagnostics("let d = {a: 1}\nlet e = d.set(\"b\", \"text\")\nprint(e)\n");
    assert!(
        errs.is_empty(),
        "`dict.set` 的异质值按 spec（`string, any -> dict`）合法，不应触发约束冲突; \
         实际: {errs:?}"
    );
}

/// 反向对照：**有位置**的诊断必须原样保留 —— 不得为了修 line 0 把好位置也抹掉。
#[test]
fn d126_diagnostics_with_a_real_position_keep_it() {
    for (name, src) in [
        ("eq", "let a = 1\nlet s = \"text\"\nprint(a == s)\n"),
        (
            "add",
            "let a = 1\nlet s = \"text\"\nlet r = a + s\nprint(r)\n",
        ),
    ] {
        let errs = diagnostics(src);
        assert!(!errs.is_empty(), "[{name}] 应有诊断");
        for e in &errs {
            assert!(
                e.contains(&format!("line {}", 3)),
                "[{name}] 二元运算的错误在第 3 行，位置不应被抹掉: {e}"
            );
            assert!(
                !e.contains("位置未跟踪"),
                "[{name}] 有位置的诊断不应被标成「位置未跟踪」: {e}"
            );
        }
    }
}

/// D126 顺带更正的一处**注释与实现不符**（D115 同型）—— **D161 已把事实翻转**。
///
/// D126 当时实测 `mora --check` 对闭包少参/多参**一律 exit 0**，据此把
/// `tests/call_arity.rs` 的过时注释（称「typeck 已经拦下，只是文案很差」）更正为
/// 「typeck 根本不拦，arity 完全由运行期兜住」。
///
/// v0.104.6 D161 把这条**真正修好了**：arity 现在由 typeck 检查，且
/// `ArityMismatch` 携带**实参的 span**（不再是无位置的「位置未跟踪」）。
/// 本测试随之从「钉住缺陷」翻转为「钉住修复」——
/// 它原本的失败信息就写着「若本测试失败，说明 typeck 已开始检查 arity」，
/// 这正是 known-gap 测试**转红即是信号**的用法。
#[test]
fn d161_closure_arity_is_now_checked_with_position() {
    use mora::parser_v3::ParserV3;
    for (name, src, want) in [
        (
            "too_few",
            "let f = fn(a, b) a + b end\nprint(f(1))\n",
            "Expected 2 arguments, got 1",
        ),
        (
            "too_many",
            "let f = fn(a, b) a + b end\nprint(f(1, 2, 3))\n",
            "Expected 2 arguments, got 3",
        ),
    ] {
        let (_f, wits) = ParserV3::compile(src).expect("语法应通过");
        let errs = check_program_witnesses_bidirectional(&wits);
        assert_eq!(
            errs.len(),
            1,
            "[{name}] D161 起闭包 arity **由 typeck 检查**，应恰好一条诊断; 实得: {errs:?}"
        );
        let e = &errs[0];
        assert!(
            e.message.contains(want),
            "[{name}] 应是 ArityMismatch 消息 `{want}`; 实得: {}",
            e.message
        );
        // 本文件的主题是「诊断位置」—— D161 的 ArityMismatch 必须带实参 span
        assert!(
            e.line > 0,
            "[{name}] 诊断应带**实参所在行**（D126 时代是无位置的）; 实得: {e:?}"
        );
        assert!(
            !e.message.contains("位置未跟踪"),
            "[{name}] 有位置的诊断不应被标成「位置未跟踪」; 实得: {}",
            e.message
        );
    }
}
