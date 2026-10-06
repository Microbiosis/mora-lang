//! v0.104.6 D108：spec §14.2 的 EBNF 与实现的一致性核对。
//!
//! ## 背景
//!
//! 待决清单里记着「两处 spec EBNF 与实现不一致」。本轮逐条**用 spec 原文的形式**
//! 去实测，结果**只有一条真的不一致** —— 而且我第一版测错了方向：
//! 我臆造了 `model M { count: number }`（带花括号）并报「不一致」，
//! 但 spec §14.2 原文写的是
//!
//! ```text
//! model_stmt = "model" IDENTIFIER { IDENTIFIER ":" type [ "=" expr ] } "end" ;
//! ```
//!
//! —— **就是实现的缩进 + `end` 形式**，两者一致。`msg` / `update` / `transaction`
//! 同理，全部可解析。**「臆造的语法不解析」不等于「spec 承诺的语法不解析」。**
//!
//! 真正不一致的只有 `handle_stmt`：spec 原写 `handle E on F -> { stmt } end`，
//! 实测 `Parse error: Expected '{' after effect name` —— **该形式根本解析不了**。
//! 已改为与实现（及全部 e2e fixture）一致的双花括号块形式。
//!
//! ## ⚠ 这些测试**必须真的读 spec 文件**
//!
//! 第一版只硬编码了几段源码，把 spec 改回去它们**照样全绿** ——
//! 那不是「spec 一致性测试」，只是语法冒烟。**测试不读被测对象 = 测不到它。**

use mora::parser_v3::ParserV3;

/// 读 spec §14.2 里某条产生式（形如 `name_stmt  = ... ;`）。
fn spec_production(name: &str) -> String {
    let text = std::fs::read_to_string("docs/mora-spec.md")
        .unwrap_or_else(|e| panic!("读 docs/mora-spec.md 失败: {e}"));
    text.lines()
        .find(|l| l.trim_start().starts_with(name) && l.contains('='))
        .unwrap_or_else(|| panic!("spec §14.2 里找不到 `{name}` 产生式"))
        .to_string()
}

/// **D108 主断言**：spec 的 `handle_stmt` 必须是**双花括号块**形式，
/// 且该形式真的能解析。
#[test]
fn d108_spec_handle_stmt_matches_implementation() {
    let prod = spec_production("handle_stmt");
    assert!(
        !prod.contains("\"on\"") && !prod.contains("\"->\""),
        "spec 的 handle_stmt 仍是旧的 `on … -> … end` 形式（该形式实现里解析不了）。\n\
         实际：{prod}"
    );
    let src = "print(handle Ai {\n  print(1)\n} {\n  print(2)\n})\n";
    assert!(
        ParserV3::compile(src).is_ok(),
        "spec 现在的 handle 形式（双花括号块）应可解析。\n实际：{prod}"
    );
}

/// 反向：旧的 `on … -> … end` 形式**确实不可解析** —— 钉住它，
/// 防止有人依据旧 spec 把它当合法语法用。
#[test]
fn d108_old_on_arrow_handle_form_does_not_parse() {
    let src = "print(handle Ai on E -> print(1) end)\n";
    assert!(
        ParserV3::compile(src).is_err(),
        "旧的 `handle E on F -> … end` 形式不可解析；若本测试失败说明实现改了，\
         需同步更新 docs/mora-spec.md §14.2 的 handle_stmt"
    );
}

/// spec §14.2 原文的 `model_stmt`：`model` IDENT + 缩进字段 + `end`。
#[test]
fn d108_spec_model_stmt_form_parses() {
    let prod = spec_production("model_stmt");
    assert!(
        prod.contains("\"end\""),
        "model_stmt 应是缩进 + end 形式。实际：{prod}"
    );
    let src = "model M\n  count: number = 0\n  label: string = \"x\"\nend\nprint(1)\n";
    assert!(
        ParserV3::compile(src).is_ok(),
        "spec 原文的 model 形式应可解析"
    );
}

/// spec §14.2 原文的 `msg_stmt`。
#[test]
fn d108_spec_msg_stmt_form_parses() {
    let src = "msg Inc\n  Bump\n  Set(number)\nend\nprint(1)\n";
    assert!(
        ParserV3::compile(src).is_ok(),
        "spec 原文的 msg 形式应可解析"
    );
}

/// spec §14.2 原文的 `update_stmt`。
#[test]
fn d108_spec_update_stmt_form_parses() {
    let src = "update(msg, model)\n  model\nend\nprint(1)\n";
    assert!(
        ParserV3::compile(src).is_ok(),
        "spec 原文的 update 形式应可解析"
    );
}

/// spec §14.2 原文的 `transaction_stmt`（含 `compensation` 子句）。
#[test]
fn d108_spec_transaction_stmt_form_parses() {
    let prod = spec_production("transaction_stmt");
    assert!(
        prod.contains("compensation"),
        "transaction_stmt 应含 compensation 子句。实际：{prod}"
    );
    let src = "transaction\n  print(1)\n  compensation\n    print(2)\nend\n";
    assert!(
        ParserV3::compile(src).is_ok(),
        "spec 原文的 transaction 形式应可解析"
    );
}
