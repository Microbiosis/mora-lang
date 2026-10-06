//! v0.104.6 D50 / D51：内建里最后两处「参数静默兜底」。
//!
//! 这两条把 D47 / D48 建立的模式补齐了最后一格：**同一份代码里，参数校验
//! 覆盖了一半、漏的那一半静默产出错误答案**。
//!
//! ## D50：`macroexpand` —— 缺参给**误导性**错误，多余实参**静默丢弃**
//!
//! ```mora
//! macro m1(a, b)
//!   a + b
//! end
//!
//! macroexpand("m1")            → Runtime error: Operands must be two numbers…
//!                                ← 误导：真实原因是少传了 2 个实参，
//!                                  用户会去查宏体里 `+` 的类型规则
//! macroexpand("m1", [7])       → 同上（仍是误导性错误）
//! macroexpand("m1", [7, 8, 9]) → 15.0   ← 多余实参被静默丢弃，exit 0
//! ```
//!
//! 与 D47（task 少参/多参）、D48（reduce 缺初值）完全同型，只是这一处
//! **既漏少参、又多参，两头都漏**。
//!
//! ## D51：`memory.store` —— **同一函数内验证不对称**
//!
//! ```rust
//! let key = args.first().map(|v| v.to_string())
//!     .ok_or("memory.store: requires key")?;      // ← key：有校验
//! let value = args.get(1).cloned().unwrap_or(Value::Nil);  // ← value：静默兜底
//! ```
//!
//! ```mora
//! memory.store("k1")
//! memory.recall("k1")      → nil    exit 0、零提示
//! ```
//!
//! 用户以为存了，实际存进去的是空。且该 namespace 在 spec 里**零记载**、
//! typeck 也**零签名**，没有编译期 arity 兜底，只能在运行期拦。

use std::sync::Arc;

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

fn debug_of(src: &str) -> String {
    format!("{:?}", run(src).expect("应能跑完"))
}

const MACRO_M1: &str = "macro m1(a, b)\n  a + b\nend\n";

// ===================================================================
// D50
// ===================================================================

#[test]
fn macroexpand_arity_mismatch_is_rejected() {
    let cases: &[(&str, &str)] = &[
        // 修前这三条报的是宏体里 `+` 的操作数错误，完全掩盖 arity 问题
        ("no arg list", "macroexpand(\"m1\")"),
        ("empty list", "macroexpand(\"m1\", [])"),
        ("too few", "macroexpand(\"m1\", [7])"),
        // 修前静默丢弃多余的 9，返回 15.0
        ("too many", "macroexpand(\"m1\", [7, 8, 9])"),
    ];
    for (name, call) in cases {
        match run(&format!("{MACRO_M1}{call}")) {
            Ok(v) => panic!(
                "[{name}] arity 不符必须报错，修前多余实参静默丢弃。实得 Ok({v:?})\n  src={call:?}"
            ),
            Err(e) => assert!(
                e.contains("expects 2 args"),
                "[{name}] 错误信息应点名 arity（`macro 'm1' expects 2 args, got N`），实得: {e}"
            ),
        }
    }
}

#[test]
fn macroexpand_with_correct_arity_still_works() {
    assert_eq!(
        debug_of(&format!("{MACRO_M1}macroexpand(\"m1\", [7, 8])")),
        "Float(15.0)"
    );
    assert_eq!(
        debug_of(&format!("{MACRO_M1}macroexpand(\"m1\", [0, 0])")),
        "Float(0.0)"
    );
}

// ===================================================================
// D51
// ===================================================================

/// 缺 value 必须报错 —— 修前静默把 `Nil` 存进去。
///
/// ⚠ v0.104.6 D75：`memory` 补上模块签名后，这个错误**提前到了编译期** ——
/// `memory.store` 声明「至少 2 参」，于是 typeck 先报
/// `Expected 2 arguments, got 1`，运行期那条 `requires a value` 的
/// `ok_or` 不再是这条路径的第一现场。
///
/// **这是修复强度提升，不是回退**：D51 的缺陷（静默存 Nil）依然不存在，
/// 且现在在更早的阶段、用更明确的措辞被拦下。运行期的 `ok_or` 保留作
/// 纵深防御（任何绕过 typeck 的动态路径仍会命中）。
///
/// 故断言放宽为「两条合法诊断之一」：typeck 的元数错 或 运行期的
/// `requires a value`。
#[test]
fn memory_store_without_value_is_rejected() {
    match run("memory.store(\"k1\")\n") {
        Ok(v) => panic!("缺 value 必须报错（修前静默存 Nil），实得 Ok({v:?})"),
        Err(e) => assert!(
            e.contains("requires a value") || e.contains("Expected 2 arguments"),
            "缺 value 必须报错：或由 typeck 拦下（元数错），或由运行期指出缺的是 value \
             （与 key 那一侧的措辞区分），实得: {e}"
        ),
    }
}

/// 缺 key 也必须报错（该分支修前就是对的，此处钉住防回退）。
#[test]
fn memory_store_without_key_is_rejected() {
    if let Ok(v) = run("memory.store()\n") {
        panic!("缺 key 必须报错，实得 Ok({v:?})");
    }
}

/// 正常存取必须照常工作。
#[test]
fn memory_store_and_recall_roundtrip() {
    // tail 值是 recall 的结果
    match run("memory.store(\"k2\", 42)\nmemory.recall(\"k2\")\n") {
        Ok(Value::Float(f)) => assert_eq!(f, 42.0),
        Ok(other) => panic!("期望 Float(42.0)，实得 {other:?}"),
        Err(e) => panic!("正常存取不该报错，实得: {e}"),
    }
    // 存字符串也应原样取回
    match run("memory.store(\"k3\", \"hello\")\nmemory.recall(\"k3\")\n") {
        Ok(Value::String(s)) => assert_eq!(s, "hello"),
        Ok(other) => panic!("期望 String(\"hello\")，实得 {other:?}"),
        Err(e) => panic!("正常存取不该报错，实得: {e}"),
    }
}
