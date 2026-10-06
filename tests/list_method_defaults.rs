//! v0.104.6 D48 / D49：两个列表方法用**静默兜底**代替了「实参无效」。
//!
//! ## D48：`reduce` 缺初值 → 累加器静默从 `Nil` 起算
//!
//! spec §1053 写的是 `.reduce(fn, init)`、签名 `closure, any -> any`
//! —— **init 是必填参数**。但实现里 `args.get(1).cloned().unwrap_or(Value::Nil)`
//! 让缺初值变成了「从 Nil 起算」，症状分两种：
//!
//! ```mora
//! [1,2,3].reduce(fn(a,b) a+b end)
//!   → Runtime error: Operands must be two numbers, two strings, or two lists
//!     ← 误导：真实原因是少传了初值，用户会去查 `+` 的类型规则
//!
//! ["a","b","c"].reduce(fn(a,b) a+b end)
//!   → "nilabc"   ← **静默的错误结果，exit 0**
//!     `Nil` 被当字符串拼进去了，垃圾值直接进了返回值
//! ```
//!
//! 字符串那例是本会话最恶劣的静默之一：返回值里带着 `"nil"` 字样，程序
//! 却「成功」结束，下游拿到的数据已经损坏且**无任何征兆**。
//!
//! **不采用「缺初值就用首元素」** —— spec 明确 init 必填，那是语言设计决定，
//! 不该由实现悄悄替用户选。故改为报错。
//!
//! ## D49：`transpose` 对不规则二维表静默补 `nil`
//!
//! 此前只校验「每项都是列表」，**不校验各行等长**，短行用
//! `row.get(col).unwrap_or(Nil)` 补齐：
//!
//! ```mora
//! [[1,2],[3]].transpose()     → [[1.0, 3.0], [2.0, nil]]
//! [[1],[2,3]].transpose()     → [[1.0, 2.0], [nil, 3.0]]
//! [[1,2,3],[4]].transpose()   → [[1.0, 4.0], [2.0, nil], [3.0, nil]]
//! ```
//!
//! 全部 **exit 0、零提示**，返回一个用 null 补齐的伪矩阵 —— 下游任何按长度
//! 或形状计算的代码都会拿到垃圾而不自知。spec §1060 `.transpose() -> list`
//! 未承诺补齐语义，故按「不规则即报错」处理，错误信息带上各行宽度。

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

// ===================================================================
// D48
// ===================================================================

/// 缺初值必须报错 —— 修前数字版报的是误导性的操作数错误、字符串版**静默**
/// 返回 `"nilabc"`。
#[test]
fn reduce_without_initial_value_is_rejected() {
    let cases: &[(&str, &str)] = &[
        ("numbers", "[1,2,3].reduce(fn(a, b) a + b end)"),
        ("single", "[5].reduce(fn(a, b) a + b end)"),
        ("empty", "[].reduce(fn(a, b) a + b end)"),
        // 修前这一条静默产出 "nilabc" —— 垃圾值进了返回值且 exit 0
        ("strings", "[\"a\",\"b\",\"c\"].reduce(fn(a, b) a + b end)"),
        ("multiply", "[1,2,3,4].reduce(fn(a, b) a * b end)"),
    ];
    for (name, expr) in cases {
        match run(expr) {
            Ok(v) => panic!(
                "[{name}] 缺初值必须报错（spec §1053 `reduce(fn, init)`），实得 Ok({v:?})\n  \
                 src={expr:?}"
            ),
            Err(e) => {
                // v0.104.6 D130：`reduce` 补上了 typeck 签名，故缺初值现在
                // 在**编译期**就被 arity 检查拦下（`Expected 2 arguments, got 1`），
                // 不再依赖运行期的 `reduce() requires an initial value`。
                //
                // 这是**改善**而非退让：原先字符串版
                // `["a","b","c"].reduce(f)` 会**静默**产出垃圾值 `"nilabc"`
                // 且 exit 0（见上面 case 的注释），现在编译期就拦住。
                //
                // 代价是文案从「点名缺初值」变成通用的参数个数提示 ——
                // 要恢复专门文案需在 typeck 的 arity 检查里按方法名特判，
                // 属另一件事（已记 CHANGELOG D130）。
                assert!(
                    e.contains("Expected 2 arguments") || e.contains("requires an initial value"),
                    "[{name}] 缺初值应在编译期被 arity 检查拦下（或运行期点名「缺初值」），\
                     实得: {e}"
                );
            }
        }
    }
}

/// 给了初值必须照常工作 —— D48 收紧的是**校验**，不是归约语义。
#[test]
fn reduce_with_initial_value_still_works() {
    assert_eq!(
        debug_of("[1,2,3].reduce(fn(a, b) a + b end, 0)"),
        "Float(6.0)"
    );
    assert_eq!(
        debug_of("[1,2,3].reduce(fn(a, b) a + b end, 10)"),
        "Float(16.0)"
    );
    assert_eq!(
        debug_of("[1,2,3,4].reduce(fn(a, b) a * b end, 1)"),
        "Float(24.0)"
    );
    assert_eq!(
        debug_of("[\"a\",\"b\",\"c\"].reduce(fn(a, b) a + b end, \"\")"),
        "String(\"abc\")"
    );
    // 空列表 + 显式初值 = 初值本身
    assert_eq!(debug_of("[].reduce(fn(a, b) a + b end, 42)"), "Float(42.0)");
}

// ===================================================================
// D49
// ===================================================================

/// 不规则二维表必须报错 —— 修前静默用 `nil` 补齐成伪矩阵。
#[test]
fn transpose_of_ragged_list_is_rejected() {
    let cases: &[(&str, &str)] = &[
        ("row 2 short", "[[1,2],[3]].transpose()"),
        ("row 1 short", "[[1],[2,3]].transpose()"),
        ("row 2 much short", "[[1,2,3],[4]].transpose()"),
        ("three rows", "[[1,2],[3],[4,5]].transpose()"),
    ];
    for (name, expr) in cases {
        match run(expr) {
            Ok(v) => panic!(
                "[{name}] 不规则二维表必须报错（修前静默补 nil），实得 Ok({v:?})\n  src={expr:?}"
            ),
            Err(e) => assert!(
                e.contains("rectangular"),
                "[{name}] 错误信息应说明「各行等长」这一要求，实得: {e}"
            ),
        }
    }
}

/// 矩形表必须照常转置。
#[test]
fn transpose_of_rectangular_list_still_works() {
    assert_eq!(
        debug_of("[[1,2],[3,4]].transpose()"),
        "List([List([Float(1.0), Float(3.0)]), List([Float(2.0), Float(4.0)])])"
    );
    assert_eq!(
        debug_of("[[1,2,3]].transpose()"),
        "List([List([Float(1.0)]), List([Float(2.0)]), List([Float(3.0)])])"
    );
    // 空表：各行等长（都空）→ 合法，返回空
    assert_eq!(debug_of("[[],[]].transpose()"), "List([])");
}
