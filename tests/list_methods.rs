//! 列表方法（list method）回归护栏 —— 把「零覆盖」变成「有断言」。
//!
//! ## 为什么有��文件
//!
//! `.shape()` / `.flatten()` / `.reshape()` / `.push()` / `.pop()` / stats 链
//! 都是 `docs/mora-spec.md` §1047-1061 明确列出的**语言面**，但改动它们的
//! 实现（`src/interpreter/method_dispatch.rs::call_method_list`）时，
//! `tests/` 与 `examples/` 里**一条断言都没有** —— 「编译通过 + 套件全绿」
//! 证明不了任何语义。
//!
//! 本文件的期望值不是拍脑袋写的：2026-09-28 清理 `call_method_list` 里 6 处
//! 对「已按值接管的 `list`」的多余 clone 之后，对该文件做了**语义 A/B**
//! （`git stash push -- src/interpreter/method_dispatch.rs` 切回 HEAD 原版
//! 再跑同一批 32 个用例），**32/32 逐字节相同**。故这批值是「改动前后一致」
//! 的既有行为，可以安全地作为基线钉住。
//!
//! ## 钉住的既有怪癖（**不是**本文件引入的，也**不**在本次修）
//!
//! 见文件末尾 `list_methods_undefined_quirks_are_pinned_not_enshrined`。
//! 那三条的共同点：**spec 对它们没有规定**，所以它们不是契约、只是现状 ——
//! 本文件把它们单列并显式命名为「未定义」，以免被后续读者误当成「设计如此」。

use std::sync::Arc;

/// 跑一段源码，返回顶层 body 的值（格式化后便于断言）。
fn run(src: &str) -> String {
    let (func, _w) = mora::parser_v3::ParserV3::compile(src)
        .unwrap_or_else(|e| panic!("compile failed for:\n{src}\n{e}"));
    let arc = Arc::new(func);
    let mut interp = mora::interpreter::Interpreter::new();
    let mut env = interp.take_env();
    match mora::mir::vm::run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    ) {
        Ok(v) => format!("{v:?}"),
        Err(e) => format!("ERR: {e}"),
    }
}

/// `(用例名, 源码, 期望)`。
///
/// 期望值取自 2026-09-28 的语义 A/B 快照（改动前后 32/32 一致）。
fn cases() -> Vec<(&'static str, &'static str, String)> {
    let f = |n: f64| format!("Float({n:?})");
    let l = |s: &str| format!("List([{s}])");
    let ll = |rows: &[&str]| format!("List([{}])", rows.join(", "));

    vec![
        // ── shape ────────────────────────────────────────────────────────
        ("shape.empty", "let L = []\nL.shape()", l(&f(0.0))),
        ("shape.scalar1d", "let L = [1, 2, 3]\nL.shape()", l(&f(3.0))),
        (
            "shape.2d",
            "let L = [[1, 2, 3], [4, 5, 6]]\nL.shape()",
            l(&format!("{}, {}", f(2.0), f(3.0))),
        ),
        (
            "shape.3d",
            "let L = [[[1, 2], [3, 4]]]\nL.shape()",
            l(&format!("{}, {}, {}", f(1.0), f(2.0), f(2.0))),
        ),
        ("shape.mixed", "let L = [1, [2, 3]]\nL.shape()", l(&f(2.0))),
        // ── flatten ──────────────────────────────────────────────────────
        (
            "flatten.flat",
            "let L = [1, 2, 3]\nL.flatten()",
            l("Float(1.0), Float(2.0), Float(3.0)"),
        ),
        (
            "flatten.2d",
            "let L = [[1, 2], [3, 4]]\nL.flatten()",
            l("Float(1.0), Float(2.0), Float(3.0), Float(4.0)"),
        ),
        (
            "flatten.3d",
            "let L = [[[1], [2]], [[3]]]\nL.flatten()",
            l("Float(1.0), Float(2.0), Float(3.0)"),
        ),
        (
            "flatten.empty",
            "let L = []\nL.flatten()",
            "List([])".to_string(),
        ),
        (
            "flatten.mixed",
            "let L = [1, [2, [3, 4]], 5]\nL.flatten()",
            l("Float(1.0), Float(2.0), Float(3.0), Float(4.0), Float(5.0)"),
        ),
        // ── reshape ──────────────────────────────────────────────────────
        (
            "reshape.2x3",
            "let L = [1, 2, 3, 4, 5, 6]\nL.reshape(2, 3)",
            ll(&[
                &l("Float(1.0), Float(2.0), Float(3.0)"),
                &l("Float(4.0), Float(5.0), Float(6.0)"),
            ]),
        ),
        (
            "reshape.3x2",
            "let L = [1, 2, 3, 4, 5, 6]\nL.reshape(3, 2)",
            ll(&[
                &l("Float(1.0), Float(2.0)"),
                &l("Float(3.0), Float(4.0)"),
                &l("Float(5.0), Float(6.0)"),
            ]),
        ),
        // 既有行为：不足时循环重复已有前缀补齐
        (
            "reshape.pad",
            "let L = [1, 2, 3]\nL.reshape(2, 3)",
            ll(&[
                &l("Float(1.0), Float(2.0), Float(3.0)"),
                &l("Float(1.0), Float(2.0), Float(3.0)"),
            ]),
        ),
        (
            "reshape.1x6",
            "let L = [1, 2, 3, 4, 5, 6]\nL.reshape(1, 6)",
            ll(&[&l(
                "Float(1.0), Float(2.0), Float(3.0), Float(4.0), Float(5.0), Float(6.0)",
            )]),
        ),
        (
            "reshape.nested_in",
            "let L = [[1, 2], [3, 4]]\nL.reshape(4, 1)",
            ll(&[
                &l("Float(1.0)"),
                &l("Float(2.0)"),
                &l("Float(3.0)"),
                &l("Float(4.0)"),
            ]),
        ),
        // v0.104.6 D153：措辞由 `reshape() requires rows argument` 统一为
        // `reshape: requires rows`（`builtins::required_num_arg` 的格式）。
        // **行为未变** —— 两者都是「真没传参」时的诚实报错；D153 修的是
        // 「传了合法 `Int` 却被报成没传」那个归因错误。
        (
            "reshape.noargs",
            "let L = [1, 2, 3]\nL.reshape()",
            "ERR: reshape: requires rows".to_string(),
        ),
        // ── push / pop ───────────────────────────────────────────────────
        (
            "push.basic",
            "let L = [1, 2]\nL.push(3)",
            l("Float(1.0), Float(2.0), Float(3.0)"),
        ),
        (
            "push.nil",
            "let L = [1, 2]\nL.push()",
            l("Float(1.0), Float(2.0), Nil"),
        ),
        ("push.empty", "let L = []\nL.push(1)", l("Float(1.0)")),
        (
            "push.nested",
            "let L = [[1], [2]]\nL.push([3])",
            ll(&[&l("Float(1.0)"), &l("Float(2.0)"), &l("Float(3.0)")]),
        ),
        (
            "pop.basic",
            "let L = [1, 2, 3]\nL.pop()",
            "Float(3.0)".to_string(),
        ),
        ("pop.empty", "let L = []\nL.pop()", "Nil".to_string()),
        // ── stats 链 ─────────────────────────────────────────────────────
        (
            "sum.basic",
            "let L = [1, 2, 3]\nL.sum()",
            "Float(6.0)".to_string(),
        ),
        (
            "mean.basic",
            "let L = [1, 2, 3]\nL.mean()",
            "Float(2.0)".to_string(),
        ),
        (
            "median.basic",
            "let L = [1, 3, 2]\nL.median()",
            "Float(2.0)".to_string(),
        ),
        (
            "stddev.basic",
            "let L = [1, 2, 3]\nL.stddev()",
            "Float(0.816496580927726)".to_string(),
        ),
        (
            "var.basic",
            "let L = [1, 2, 3]\nL.var()",
            "Float(0.6666666666666666)".to_string(),
        ),
        (
            "min.basic",
            "let L = [3, 1, 2]\nL.min()",
            "Float(1.0)".to_string(),
        ),
        (
            "max.basic",
            "let L = [3, 1, 2]\nL.max()",
            "Float(3.0)".to_string(),
        ),
        // v0.104.6（语言作者已确认取 0.0）：空列表与 mean/median 完全统一。
        // 此前 min_f/max_f 缺了 mean/median 都有的空守卫，漏出 inf / -inf。
        ("min.empty", "let L = []\nL.min()", "Float(0.0)".to_string()),
        ("max.empty", "let L = []\nL.max()", "Float(0.0)".to_string()),
    ]
}

/// 逐条断言每个 list 方法的返回值。
///
/// 一次性跑完再报全部差异（而不是第一条就 panic），否则一个回归会掩盖其余。
#[test]
fn list_methods_match_baseline() {
    let mut failures: Vec<String> = Vec::new();
    for (name, src, expected) in cases() {
        let got = run(src);
        if got != expected {
            failures.push(format!(
                "  [{name}]\n    src      = {src:?}\n    expected = {expected}\n    actual   = {got}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} / {} 个 list 方法用例与基线不符：\n{}",
        failures.len(),
        cases().len(),
        failures.join("\n")
    );
}

/// `push` 的「返回新列表」语义必须保持**值语义**：
/// 追加后原变量不受影响。这是不可变列表的核心契约，改 `push` 实现时最容易被破坏。
#[test]
fn push_does_not_mutate_the_receiver() {
    // 浅层：标量元素
    assert_eq!(
        run("let L = [1, 2]\nlet M = L.push(3)\nL"),
        "List([Float(1.0), Float(2.0)])",
        "push 后原列表必须保持不变（标量元素）"
    );
    // 深层：嵌套列表元素 —— 共享子结构也不得被就地改写
    assert_eq!(
        run("let L = [[1], [2]]\nlet M = L.push([3])\nL"),
        "List([List([Float(1.0)]), List([Float(2.0)])])",
        "push 后原列表必须保持不变（嵌套元素）"
    );
}

/// 连续累积建表是这条语言面最常见的用法，钉住它的**结果**正确性。
#[test]
fn push_accumulation_in_loop_is_correct() {
    assert_eq!(
        run("let xs = []\nlet i = 0\nwhile i < 5\n  let xs = xs.push(i)\n  let i = i + 1\nend\nxs"),
        "List([Float(0.0), Float(1.0), Float(2.0), Float(3.0), Float(4.0)])",
    );
    assert_eq!(
        run(
            "let xs = []\nlet i = 0\nwhile i < 5\n  let xs = xs.push(i)\n  let i = i + 1\nend\nlen(xs)"
        ),
        "Int(5)"
    );
}

/// `pop` 返回末元素且不改动原列表（同样是值语义契约）。
#[test]
fn pop_does_not_mutate_the_receiver() {
    assert_eq!(
        run("let L = [1, 2, 3]\nlet x = L.pop()\nL"),
        "List([Float(1.0), Float(2.0), Float(3.0)])",
        "pop 后原列表必须保持不变"
    );
}

/// ## spec **没有规定**的行为 —— 钉住现状，但**不**奉为契约
///
/// 这三条早先被混在 `list_methods_match_baseline` 里当「期望值」断言，
/// 那等于把**未定义行为固化成契约**：后来的读者会以为这是刻意设计。
/// 它们被单列到这里，`name` 与函数名都写明「undefined」，以示区别。
///
/// 之所以仍要断言：目的是**让无意改动可见**。任何人想改这三条，都必须
/// 主动来改本测试并写明理由 —— 而不是顺手改掉再被下一次重构「修回来」。
///
/// 规范状态：`docs/mora-spec.md` §1047-1061 列出这些方法，但**对空列表的
/// 求和符号、参差嵌套均无规定**，故无「正确值」可依。
///
/// v0.104.6 更新：`min([])` / `max([])` **已移出本组** —— 语言作者明确取
/// `0.0`（与 `mean`/`median` 统一），现已是 `list_methods_match_baseline`
/// 里的正式契约。仍在本格的是：
/// - `sum([])` = `-0.0`（`f64::iter().sum()` 的空迭代器初值），与
///   `mean([])` 的 `0.0` 不一致；
/// - `shape([[1,2],[3,4,5]])` 只按**第一个**子列表递归 → `[2, 2]`，
///   参差嵌套的维度信息被丢弃。
#[test]
fn list_methods_undefined_quirks_are_pinned_not_enshrined() {
    let quirks: Vec<(&str, &str, &str)> = vec![
        // 与 mean([])=0.0 不一致
        ("sum(empty)", "let L = []\nL.sum()", "Float(-0.0)"),
        // 只按第一个子列表递归，参差维度被丢弃
        (
            "shape(ragged)",
            "let L = [[1, 2], [3, 4, 5]]\nL.shape()",
            "List([Float(2.0), Float(2.0)])",
        ),
    ];

    let mut failures: Vec<String> = Vec::new();
    for (name, src, expected) in &quirks {
        let got = run(src);
        if &got != expected {
            failures.push(format!(
                "  [{name}]\n    src      = {src:?}\n    现状(未定义) = {expected}\n    actual   = {got}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} / {} 条**未定义**行为被改动（这不是「修复」，是要先定规范）：\n{}",
        failures.len(),
        quirks.len(),
        failures.join("\n")
    );
}
