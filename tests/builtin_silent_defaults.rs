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

use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::value::Value;

fn run(src: &str) -> Result<Value, String> {
    let (prog, witnesses) =
        mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    let errs = mora::typeck::check_mir::check_program_witnesses(&witnesses);
    if !errs.is_empty() {
        return Err(format!(
            "TYPECK: {:?}",
            errs.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
        ));
    }
    let arc = Arc::new(prog);
    let mut core = CoreRuntime::default();
    execute_program(arc, &mut core)
        .map(|(v, _)| v)
        .map_err(|e| e.to_string())
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
        "Int(15)"
    );
    assert_eq!(
        debug_of(&format!("{MACRO_M1}macroexpand(\"m1\", [0, 0])")),
        "Int(0)"
    );
}
