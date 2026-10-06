//! v0.104.6 D107：`run_mir` 的末值 = **最后一个写寄存器的指令的值**。
//!
//! 对 `with` 块，这意味着块末尾那条 `Const(dst, Nil)` 应当胜出
//! （`emit_with_w` 明写「子 body 的返回值不传播」，块值恒为 `Nil`）。
//!
//! 本文件测量并钉住该契约。它是**库层**观察 —— `mora run` 不打印末表达式，
//! 只有嵌入方（`e2e_helpers::run_source` 等）能看到。

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use std::sync::Arc;

fn last_expr(src: &str) -> String {
    let (func, _w) = ParserV3::compile(src).expect("compile");
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    match run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    ) {
        Ok(v) => format!("{:?}", v),
        Err(e) => format!("Err({})", e),
    }
}

/// `with` 块在末尾：块值恒为 `Nil`，**不应**是配置绑定的值。
#[test]
fn d107_with_block_value_is_nil_not_the_binding() {
    let got = last_expr("with model = \"m\"\n  print(1)\nend\n");
    assert_eq!(
        got, "Nil",
        "`with` 块的值按设计恒为 Nil（子 body 返回值不传播），末值不该是配置绑定值"
    );
}

/// 对照：末尾是普通字面量时，末值就是那个字面量。
///
/// ⚠ 本语言**所有裸数字字面量都是 `Value::Float`**（D98 已确立：
/// `type_of(1)` = `float`，而 `Int` 只由 `len()` 产生），故期望 `Float(5.0)`。
#[test]
fn d107_plain_tail_value_is_returned() {
    assert_eq!(last_expr("let q = 5\nq\n"), "Float(5.0)");
}

/// 对照：末尾是 `print` 调用时，末值是 print 的返回值（Nil）。
#[test]
fn d107_tail_call_returns_nil() {
    assert_eq!(last_expr("let q = 5\nprint(q)\n"), "Nil");
}
