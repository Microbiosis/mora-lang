//! v0.104.6 D161：闭包 arity 的**编译期检查**（本会话长期待办的收口）。
//!
//! ## 缺陷一：少传实参**不被查** —— `--check` 说「代码没问题」，但它跑不起来
//!
//! ```mora
//! let f2 = fn(a, b) => a + b
//! f2(1)
//! ```
//!
//! | | 修复前 | 修复后 |
//! |---|---|---|
//! | `mora --check` | **exit 0**「No type errors found」 ❌ | exit 2 `Expected 2 arguments, got 1` |
//! | 运行期 | exit 1 `closure expects 2 args, got 1` | （到不了运行期） |
//!
//! ## 根因：arity 编码在 Arrow 的**输出**链上，而调用点「给几个消耗几个」
//!
//! `wrap_curried_arrow` **从最后一个参数向前包裹**，多参数闭包是
//! `Arrow(A, Arrow(B, C, …), …)`。而 `infer_call` 的 curried 循环每提供一个
//! 实参就压一条 `Eq(callee_ty, Arrow(arg_ty, …))` —— 实参不足时，剩下的
//! `Arrow` 层**根本不进任何约束**，于是没有任何错误产生。
//!
//! （我第一版沿 `input` 侧数嵌套，恒得 1，于是少传实参**根本没被拦下** ——
//! 嵌套在输出侧。）
//!
//! ## 缺陷二：多传实参的诊断**泄露内部变量**
//!
//! ```text
//! f1(1, 2)  where f1 = fn(a) => a + 1
//!   修复前 → Type mismatch: expected float, got fn (float) -> ' ! { rho1 }
//!                                   ↑ `rho1` 是内部 effect-row 变量名
//!   修复后 → Expected 1 arguments, got 2
//! ```
//!
//! 多传此前**会**被查，但走的是「多余实参与**返回类型**合一」这条歪路 ——
//! 既误报类型，又把内部变量名暴露给用户。
//!
//! ## 运行期是权威
//!
//! 逐条实测（绕开 typeck 的库 API）：
//!
//! | 调用 | 运行期 |
//! |---|---|
//! | `f1(1)` / `f2(1, 2)` | OK |
//! | `f1(1, 2)` 多传 | ERR `closure expects 1 args, got 2` |
//! | `f2(1)` 少传 | ERR `closure expects 2 args, got 1` |
//!
//! 即运行期**不支持部分应用**，类型层也不该放行。部分应用在本语言里有
//! **显式**写法（`curry(f, n)`，D148 钉过），不靠少传实参隐式获得。

use mora::typeck::check_mir::check_program_witnesses_bidirectional;

fn typeck(src: &str) -> Result<Vec<String>, String> {
    let (_f, w) = mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    let errs = check_program_witnesses_bidirectional(&w);
    if errs.is_empty() {
        Ok(vec![])
    } else {
        Ok(errs.iter().map(|e| e.message.clone()).collect())
    }
}

/// D161 主判据 ①：少传实参必须在**类型层**被拒。
#[test]
fn d161_under_application_is_a_type_error() {
    let errs = typeck("let f2 = fn(a, b) => a + b\nlet r = f2(1)\nprint(r)\n").expect("compile");
    assert_eq!(
        errs.len(),
        1,
        "少传实参应恰好一条诊断（修复前是**零条**，`--check` exit 0）; 实得: {errs:?}"
    );
    assert!(
        errs[0].contains("Expected 2 arguments, got 1"),
        "消息应与运行期措辞一致（`closure expects 2 args, got 1`）; 实得: {errs:?}"
    );
}

/// D161 主判据 ②：多传实参的诊断必须**直白**，不得泄露内部变量名。
#[test]
fn d161_over_application_does_not_leak_internals() {
    let errs = typeck("let f1 = fn(a) => a + 1\nlet r = f1(1, 2)\nprint(r)\n").expect("compile");
    assert_eq!(errs.len(), 1, "多传应恰好一条诊断; 实得: {errs:?}");
    assert!(
        errs[0].contains("Expected 1 arguments, got 2"),
        "应是 ArityMismatch 消息; 实得: {errs:?}"
    );
    for leak in ["rho", "TypeVar", "fn (float) ->"] {
        assert!(
            !errs[0].contains(leak),
            "诊断**不得泄露**内部表示 `{leak}`; 实得: {errs:?}"
        );
    }
}

/// **D167 已翻转**：具名顶层 `task` / `fn` 的 arity **现在也受检**。
///
/// D161 收口的是**闭包字面量**（Arrow 类型在调用点可见），而顶层定义的
/// **名字不进 env**（`infer_fn_def` 注释），调用点解析成 `TypeVar` → arity 0
/// → 检查被跳过。当时记为 known-gap（`--check` exit 0 / 运行期 exit 1）。
///
/// D167 的修法比当初预估的**简单得多**：arity 就是 `params.len()`，
/// **不必推断体就能拿到** —— 所以既没有 mutual recursion 也没有前向引用的
/// 先后依赖，一遍树行走（`precompute_fn_arities`）即可，无需类型层不动点。
#[test]
fn d167_named_task_arity_is_checked() {
    for (label, src, want) in [
        (
            "under-apply",
            "task add(a, b)\n  a + b\nend\nlet r = add(1)\nprint(r)\n",
            "Expected 2 arguments, got 1",
        ),
        (
            "over-apply",
            "task one(a)\n  a + 1\nend\nlet r = one(1, 2)\nprint(r)\n",
            "Expected 1 arguments, got 2",
        ),
    ] {
        let errs = typeck(src).expect("compile");
        assert_eq!(
            errs.len(),
            1,
            "[{label}] 具名 task 的 arity 现在应恰好一条诊断（D167 前是**零条**）; 实得: {errs:?}"
        );
        assert!(
            errs[0].contains(want),
            "[{label}] 应是 ArityMismatch 消息 `{want}`; 实得: {errs:?}"
        );
    }
}

/// D167 反向对照：正确定���元数的 task 调用、以及**前向引用**（调用点写在
/// 定义之前）都不得被误伤 —— 后者正是「不需要不动点」的关键证据。
#[test]
fn d167_forward_reference_and_exact_arity_not_regressed() {
    for (label, src) in [
        (
            "exact arity",
            "task add(a, b)\n  a + b\nend\nlet r = add(1, 2)\nprint(r)\n",
        ),
        (
            "forward reference (call before def)",
            "let r = add(1, 2)\nprint(r)\ntask add(a, b)\n  a + b\nend\n",
        ),
        (
            "recursive task",
            "task fact(n)\n  if n <= 1 then\n    return 1\n  end\n  n * fact(n - 1)\nend\nprint(fact(5))\n",
        ),
    ] {
        let errs = typeck(src).expect("compile");
        assert!(
            errs.is_empty(),
            "[{label}] 合法调用不得产生类型错误（D167 不能引入假阳性）; 实得: {errs:?}"
        );
    }
}
/// D161 反向对照：正确元数、变参内建、高阶传参**都不得被误伤**。
#[test]
fn d161_valid_calls_accepted() {
    for (label, src) in [
        (
            "1 param exact",
            "let f1 = fn(a) => a + 1\nlet r = f1(1)\nprint(r)\n",
        ),
        (
            "2 params exact",
            "let f2 = fn(a, b) => a + b\nlet r = f2(1, 2)\nprint(r)\n",
        ),
        (
            "3 params exact",
            "let f3 = fn(a, b, c) => a + b + c\nlet r = f3(1, 2, 3)\nprint(r)\n",
        ),
        (
            "zero params",
            "let f0 = fn() => 42\nlet r = f0()\nprint(r)\n",
        ),
        // `print` 在签名表里是 **variadic**（走提前返回分支），必须不受影响
        ("print 1 arg", "print(1)\n"),
        ("print 3 args", "print(1, 2, 3)\n"),
        // 高阶传参：把闭包当值传给别的函数
        (
            "closure as argument",
            "let f = fn(a) => a + 1\nlet xs = [1, 2, 3]\nlet r = xs.map(f)\nprint(r)\n",
        ),
        // 递归调用：被调在定义体内尚未定型，arity 必须跳过
        (
            "recursive task",
            "task fact(n)\n  if n <= 1 then\n    return 1\n  end\n  n * fact(n - 1)\nend\nprint(fact(5))\n",
        ),
    ] {
        let errs = typeck(src).expect("compile");
        assert!(
            errs.is_empty(),
            "[{label}] 合法调用不得产生类型错误（本检查不能引入假阳性）; 实得: {errs:?}"
        );
    }
}
