//! v0.104.6 D156：方法实参的类型错误**带上面 span**（已修）。
//!
//! ## 缺陷
//!
//! `infer_method_call` 此前**只**把实参类型压成 `Constraint::Eq` 交给
//! `solve_constraints` 兜底，而 `unify()` 的每个失败分支都写死 `span: None`：
//!
//! ```text
//! let xs = [1, 2, 3]
//! let a = xs.take("one")
//! → Type error (位置未跟踪): Type mismatch: expected int, got string
//! ```
//!
//! 两个后果：
//!
//! 1. **位置丢失** —— D126 把误导性的「line 0」改成「位置未跟踪」，但没有补上位置；
//! 2. **重复且不可区分** —— D154 实测多条诊断**逐字段完全相同**
//!    （`line=0 col=0 expected=Some("int") actual=Some("string")`），
//!    因此用户既看到重复条目，**又无法分辨是哪一行**。
//!
//! ## 关键事实：span 一直都在，只是没被带进错误
//!
//! D155 打印 witness 树实测，那个实参的 span 是**正确**的：
//!
//! ```text
//! MethodCall { …, args: [Literal(String("one") @ line 2 column 17)] } @ line 2 column 16
//! ```
//!
//! 故修法是**把 span 带进错误**，而不是给 `Constraint` 加字段。
//!
//! ## 修法沿用本文件既有范式
//!
//! `infer.rs:137`（`let x: T = v` 路径）早就是这么做的，注释写着
//! 「提前用 span 报不一致——不等 solve_constraints 兜底」。
//! 本改动照搬：`compatible_with` 先判一次，不兼容就带实参 span 立即返回；
//! 兼容的**照旧**压约束（TypeVar 绑定 / 数值提升仍由 `unify` 负责，本改动不碰）。

use mora::typeck::check_mir::check_program_witnesses_bidirectional;

fn typeck(src: &str) -> Vec<(usize, usize, String, String, String)> {
    let (_f, w) = mora::cli::compile_and_opt(src, None).expect("compile");
    check_program_witnesses_bidirectional(&w)
        .into_iter()
        .map(|e| {
            (
                e.line,
                e.column,
                e.expected.unwrap_or_default(),
                e.actual.unwrap_or_default(),
                e.message,
            )
        })
        .collect()
}

/// D156 主判据 ①：每条方法实参类型错误必须**带上自己的行号列号**。
#[test]
fn d156_method_arg_type_error_carries_position() {
    let errs = typeck("let xs = [1, 2, 3]\nlet a = xs.take(\"one\")\nprint(a)\n");
    assert_eq!(errs.len(), 1, "一个错误应只报一条; 实得: {errs:?}");
    let (line, col, expected, actual, _) = &errs[0];
    assert_eq!(
        (*line, *col),
        (2, 17),
        "应指向第 2 行那个实参字面量; 实得: {errs:?}"
    );
    assert_eq!(
        actual, "string",
        "actual 应是实参类型（小写，沿用仓库惯例）; 实得: {errs:?}"
    );
    assert_eq!(
        expected, "int | float",
        "expected 应如实写出整个 Union（此前欠报成 `int`）; 实得: {expected}"
    );
}

/// D156 主判据 ②：多处错误**必须互不相同**（D154 判定无法去重的前提已解除）。
///
/// D154 的原话是「任何按消息的去重都不健全，因为 4 条长得一样」——
/// 带上位置之后，每条指向自己的行，用户能分辨，重复也随之消失。
#[test]
fn d156_distinct_errors_are_distinguishable_and_not_duplicated() {
    let src = "let xs = [1, 2, 3]\n\
               let a = xs.take(\"one\")\n\
               let b = xs.window(\"two\")\n\
               let c = xs.get(\"three\")\n\
               print(a)\n";
    let errs = typeck(src);
    assert_eq!(
        errs.len(),
        3,
        "3 个错误应恰好 3 条（修复前 6 条且逐字段相同）; 实得: {errs:?}"
    );
    let mut lines: Vec<usize> = errs.iter().map(|e| e.0).collect();
    lines.sort_unstable();
    assert_eq!(
        lines,
        vec![2, 3, 4],
        "三条诊断必须分别指向第 2/3/4 行（这是用户能分辨的前提）; 实得: {errs:?}"
    );
    // 列号也各不相同
    let mut cols: Vec<usize> = errs.iter().map(|e| e.1).collect();
    cols.sort_unstable();
    cols.dedup();
    assert_eq!(cols.len(), 3, "三条诊断的列号也应各不相同; 实得: {errs:?}");
}

/// D156 反向对照：合法调用必须**零错误**（本改动不能引入假阳性）。
#[test]
fn d156_valid_calls_still_accepted() {
    for src in [
        "let xs = [1, 2, 3]\nlet a = xs.take(2)\nprint(a)\n",
        "let xs = [1, 2, 3]\nlet a = xs.take(len([1, 1]))\nprint(a)\n",
        "let xs = [1, 2, 3]\nlet a = xs.get(1)\nprint(a)\n",
        "let xs = [1, 2, 3]\nlet a = xs.window(2)\nprint(a)\n",
        "let d = {a: 1}\nlet a = d.get(\"a\")\nprint(a)\n",
        "let s = \"abc\"\nlet a = s.split(\",\")\nprint(a)\n",
    ] {
        let errs = typeck(src);
        assert!(
            errs.is_empty(),
            "[{src}] 合法调用不得产生类型错误（防止本改动引入假阳性）; 实得: {errs:?}"
        );
    }
}

/// D156 对照组：`while` 条件、`let` 标注等**原本就带 span**的路径不得回退。
///
/// 它们是本改动的**正面样板**（`infer.rs:137` 早就这么做）。
#[test]
fn d156_existing_span_carrying_paths_not_regressed() {
    let e = typeck("let x: string = 1.5\nprint(x)\n");
    assert_eq!(e.len(), 1, "`let` 标注不兼容应仍恰好一条; 实得: {e:?}");
    assert!(
        e[0].0 > 0,
        "`let x: string = 1.5` 的诊断应带行号（本改动前就有）; 实得: {e:?}"
    );

    // `while 1i` —— 同族：cond 必须是 bool（v0.104 起）
    let w = typeck("let i = 0\nwhile i\nend\n");
    assert!(
        !w.is_empty(),
        "`while <非 bool>` 应被 typeck 拒（本改动不得让它通过）; 实得: {w:?}"
    );
}
