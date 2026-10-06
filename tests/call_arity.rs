//! v0.104.6 D47：调用点的 arity 校验**只覆盖了一半路径、只查了一半方向**。
//!
//! ## 现象（修前，真实 CLI `mora run` 实测，**全部 exit 0、零提示**）
//!
//! ```mora
//! task f(a, b)
//!   99
//! end
//! print(f(1))          → 99.0     b 被静默填成 Nil，且因未被使用而无症状
//!
//! task f(a)
//!   a
//! end
//! print(f(1, 2, 3))    → 1.0      多余实参被静默丢弃
//! ```
//!
//! ## 判决实验：两条路径，只差「调用点与定义点是否同一函数体」
//!
//! ```mora
//! task f(a, b)
//!   99
//! end
//! print(f(1))          ← 同体：走 task_registry，**静默通过**
//!
//! task f(a, b)
//!   99
//! end
//! task g()
//!   f(1)               ← 跨体：走 env 里的 Value::Task
//! end
//! g()                  ← 正确报 `task expects 2 args, got 1`
//! ```
//!
//! 即**同体定义并调用（最常见的写法）恰好绕过了检查**。
//!
//! ## 两半
//!
//! | 位置 | 修前 |
//! |------|------|
//! | `mir/handlers/values.rs::h_call` 的 **task_registry** 分支 | **完全没有** arity 校验，缺参静默填 `Nil` |
//! | `interpreter/dispatch.rs` 的 `Value::Task` / `Value::Closure` / `builtin_impls.rs` 的 `Value::Macro` | 只查 `args.len() < params.len()`，**多余实参静默丢弃** |
//!
//! 对照组：内建与方法在 typeck 层有正确的 `Expected N arguments, got M`；
//! 闭包少参也早就有 `closure expects N args, got M`。**只有 task 的这两处是洞**。
//!
//! ## 为什么静默填 Nil 特别危险
//!
//! 症状**取决于缺的那个形参用不用**：
//!
//! ```mora
//! task f(a, b)
//!   a            // b 没用 → 静默返回 a
//! end
//! f(1)           → 1.0，零提示
//!
//! task f(a, b)
//!   a + b         // b 用了 → 报「Operands must be two numbers...」
//! end
//! f(1)           → 错误信息指向操作数类型，**完全掩盖了真正的 arity 问题**
//! ```
//!
//! 后者尤其误导：用户会去查 `+` 的类型规则，而真实原因是少传了一个参数。

use std::sync::Arc;
/// 错误消息是否点名 arity —— **接受两种措辞**：
/// 运行期 `task expects N args, got M` / 类型层 `Expected N arguments, got M`。
///
/// v0.104.6 D167 起具名 `task` 的 arity 在**类型层**就被拦下，早于运行期。
/// 这些用例的本意是「必须被拒、且消息点名 arity」，**不是**「必须在运行期被拒」，
/// 故两者都算通过 —— 报错**来源前移**是改进，不是回归。
fn mentions_arity(e: &str, expected: usize, got: usize) -> bool {
    e.contains(&format!("task expects {expected} args, got {got}"))
        || e.contains(&format!("Expected {expected} arguments, got {got}"))
}

use mora::interpreter::Interpreter;
use mora::mir::effect::Effects;
use mora::mir::vm::run_mir;
use mora::value::Value;

fn run(src: &str) -> Result<Value, String> {
    let (func, witnesses) =
        mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
    if !errs.is_empty() {
        return Err(format!(
            "TYPECK: {:?}",
            errs.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
        ));
    }
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    run_mir(&arc, &mut interp, &mut env, &mut Effects::new())
}

fn err_of(src: &str) -> String {
    match run(src) {
        Ok(v) => panic!("应报错，实得 Ok({v:?})\n  src={src:?}"),
        Err(e) => e,
    }
}

/// 只断言「被拒绝」，不断言是哪一层、也不断言文案。
///
/// v0.104.6 D126 更正：本函数原先的注释称「typeck 已经拦下但**文案很差**
/// （如闭包多参：typeck 报 "Type mismatch: expected float, got fn (float) -> …"）」。
/// 实测**已不成立** —— `mora --check` 对闭包/task 的少参、多参**一律 exit 0**，
/// arity 完全由运行期 `closure expects N args, got M` 兜住；typeck 既不拦、
/// 也就无所谓文案。
///
/// **v0.104.6 D161：闭包 arity 已在 typeck 层接上。** `infer_call` 现在按
/// `Arrow` 链的**输出**嵌套数出 arity（`wrap_curried_arrow` 从最后一个参数
/// 向前包裹），少参/多参都报 `ArityMismatch` 并带实参 span；
/// 多参那条原先「expected float, got fn (float) -> … `rho1`」的歪路消息
/// （多余实参与**返回类型**合一，还会泄露内部 effect-row 变量名）也一并消失。
///
/// 适用范围：闭包字面量，以及任何调用点能拿到 Arrow 类型的被调。
/// **具名顶层 `task` / `fn` 仍不受检**（定义名不进 HM env，调用点解析成
/// `TypeVar` → arity 0 → 跳过），已写成 known-gap 测试
/// `tests/closure_arity_check.rs::d161_named_task_arity_is_a_known_gap`。
///
/// 本函数仍然有价值 —— 它钉住「被拒绝」这一事实，防止将来**运行期**也不拦。
///
/// （D115 同型：注释里描述的「已知问题」在当前实现下不复现。）
fn must_err(src: &str) {
    if let Ok(v) = run(src) {
        panic!("应被拒绝，实得 Ok({v:?})\n  src={src:?}");
    }
}

// ===================================================================
// 少传实参：两条路径都要报
// ===================================================================

/// 同体调用走 `task_registry` 分支 —— 修前**完全没有**校验。
#[test]
fn same_body_too_few_args_is_rejected() {
    for (name, src) in [
        // 缺参但未被使用：修前静默返回 99.0
        ("unused missing", "task f(a, b)\n  99\nend\nprint(f(1))\n"),
        // 缺参且被使用：修前报的是「操作数类型」错误，掩盖了真正的 arity 问题
        ("used missing", "task f(a, b)\n  a + b\nend\nprint(f(1))\n"),
        // 零参 task 被传一个
        ("zero-param", "task f()\n  7\nend\nprint(f(1))\n"),
        // 嵌套 task 内同体调用
        (
            "nested same body",
            "task f(a, b)\n  99\nend\ntask g()\n  f(1)\nend\ng()\n",
        ),
    ] {
        let e = err_of(src);
        assert!(
            mentions_arity(&e, 2, 1) || mentions_arity(&e, 0, 1),
            "[{name}] 错误信息应点名 arity（运行期 `task expects N args, got M`
             或类型层 `Expected N arguments, got M` 皆可 —— D167 起类型层
             会先拦下，报错来源变了但**拒绝本身**与消息质量不变）; 实得: {e}"
        );
    }
}

/// 跨体调用走 `Value::Task` 分支 —— 修前**有**少参检查（用于对照，确保未回退）。
#[test]
fn cross_body_too_few_args_is_rejected() {
    let e = err_of("task f(a, b)\n  99\nend\ntask g()\n  f(1)\nend\ng()\n");
    assert!(
        mentions_arity(&e, 2, 1),
        "跨体调用早就该报 arity（运行期或类型层措辞皆可）; 实得: {e}"
    );
}

// ===================================================================
// 多传实参：两条路径此前都静默丢弃
// ===================================================================

#[test]
fn too_many_args_is_rejected() {
    for (name, src) in [
        ("same body", "task f(a)\n  a\nend\nprint(f(1, 2, 3))\n"),
        (
            "cross body",
            "task f(a)\n  a\nend\ntask g()\n  f(1, 2, 3)\nend\ng()\n",
        ),
        ("zero-param task", "task f()\n  7\nend\nprint(f(1, 2, 3))\n"),
    ] {
        let e = err_of(src);
        assert!(
            e.contains("task expects") || e.contains("Expected"),
            "[{name}] 多余实参必须报错（修前静默丢弃）；运行期或类型层措辞皆可, 实得: {e}"
        );
    }
}

#[test]
fn too_many_args_is_rejected_for_closure_and_macro() {
    // 闭包：typeck 先拦住了，但文案是 "Type mismatch: expected float, got
    // fn (float) -> ..."，完全没提参数个数 —— 文案质量是另一件事，这里只钉
    // 「被拒绝」这一事实。
    must_err("let f = fn(a) a end\nprint(f(1, 2, 3))\n");

    // 宏：运行期拦（无 typeck 签名），文案应点名 arity
    let e = err_of("macro m(x)\n  x\nend\nprint(m(1, 2))\n");
    assert!(
        e.contains("expects"),
        "宏多余实参必须报错（修前静默丢弃），实得: {e}"
    );

    let e = err_of("macro m(x)\n  x\nend\nprint(m())\n");
    assert!(
        e.contains("expects"),
        "宏缺参必须报错（修前静默填 Nil），实得: {e}"
    );
}

// ===================================================================
// 正确 arity 不得被误伤
// ===================================================================

/// D47 收紧的是**校验**，不是调用语义 —— 正确 arity 必须照常工作。
#[test]
fn correct_arity_still_works() {
    for (name, src, want) in [
        ("task exact", "task f(a, b)\n  a + b\nend\nf(1, 2)\n", 3.0),
        ("task zero", "task f()\n  7\nend\nf()\n", 7.0),
        (
            "task cross body",
            "task f(a, b)\n  a + b\nend\ntask g()\n  f(1, 2)\nend\ng()\n",
            3.0,
        ),
        (
            "closure exact",
            "let f = fn(a, b) a + b end\nf(1, 2)\n",
            3.0,
        ),
        ("macro exact", "macro m(x)\n  x * 2\nend\nm(21)\n", 42.0),
        // 注意：`print(...)` 的尾值是 Nil，故用裸表达式作尾
        ("method exact", "\"a,b\".split(\",\").len()\n", 2.0),
    ] {
        match run(src) {
            // `len` 返 Int（D5 起统一为 Int），其余算术返 Float —— 两者都收
            Ok(Value::Float(f)) => assert_eq!(f, want, "[{name}] 期望 {want}，实得 {f}"),
            Ok(Value::Int(i)) => assert_eq!(i as f64, want, "[{name}] 期望 {want}，实得 {i}"),
            Ok(other) => panic!("[{name}] 期望数值 {want}，实得 {other:?}"),
            Err(e) => panic!("[{name}] 正确 arity 不该报错，实得: {e}\n  src={src:?}"),
        }
    }
}
