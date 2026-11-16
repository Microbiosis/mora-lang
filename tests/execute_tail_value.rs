//! v0.104.6 D107：`execute` 的末值 = **最后一个写寄存器的指令的值**。
//!
//! 对 `with` 块，这意味着块末尾那条 `Const(dst, Nil)` 应当胜出
//! （`emit_with_w` 明写「子 body 的返回值不传播」，块值恒为 `Nil`）。
//!
//! 本文件测量并钉住该契约。它是**库层**观察 —— `mora run` 不打印末表达式，
//! 只有嵌入方（`e2e_helpers::run_source` 等）能看到。

use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use std::sync::Arc;

fn last_expr(src: &str) -> String {
    let (prog, _w) = ParserV3::compile(src).expect("compile");
    let mut core = CoreRuntime::default();
    let arc = Arc::new(prog);
    match execute_program(arc, &mut core) {
        Ok((v, _)) => format!("{:?}", v),
        Err(e) => format!("Err({})", e),
    }
}

/// 对照：末尾是普通字面量时，末值就是那个字面量。
///
/// Stage 3A-1：裸整数字面量 `5` 为 `Value::Int(5)`，故期望 `Int(5)`。
#[test]
fn d107_plain_tail_value_is_returned() {
    assert_eq!(last_expr("let q = 5\nq\n"), "Int(5)");
}

/// 对照：末尾是 `print` 调用时，末值是 print 的返回值（Nil）。
#[test]
fn d107_tail_call_returns_nil() {
    assert_eq!(last_expr("let q = 5\nprint(q)\n"), "Nil");
}
