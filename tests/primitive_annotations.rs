//! v0.104.6 D63 / D64：两个「写得出值、写不出标注」的原语类型。
//!
//! 两者形态不同但根因同类 —— **类型存在、值存在，标注位却堵死**：
//!
//! | 类型 | 值怎么来 | 标注为什么不通 |
//! |---|---|---|
//! | `bigint` | 字面量 `999n` 产出 `Value::BigInt` | parser 白名单漏了 `"bigint"` |
//! | `nil` | 关键字 `nil` | `nil` 是**关键字 token**，走不到 `Identifier` 分支 |
//!
//! ## D63：`bigint`
//!
//! spec §3.1 类型表 `:113` 把 `bigint` 列为正式原语类型（语法 `999n`），
//! `Type::BigInt` 存在、`from_hint("bigint")` 也认它、`999n + 1n` 能算出
//! `1000n` —— 但 `let x: bigint = 999n` 报
//! `Parse error: unsupported type annotation 'bigint'`。
//! 即**任意精度的值永远无法被标注**，这条语言特性等于只有一半。
//!
//! ## D64：`nil`
//!
//! 更隐蔽：白名单里**确实有** `"nil" => Type::Nil` 这一 arm，但它是**死代码**。
//! `nil` 在 lexer 里是 `TokenType::Nil`（`lexer.rs:13`），而
//! `parse_single_type_annotation` 只 match `Dyn` 与 `Identifier` 两种 token，
//! 关键字 `nil` 直接落到兜底分支报 `expected type annotation`。
//! 于是 `let x: nil = nil` —— 一条最自然的标注 —— 解析都过不去。
//!
//! 修复后两者都不需要动 typeck：`Nil` / `BigInt` 的自反性三处早已齐备
//! （`subtype_of` mod.rs:762 / :824、`compatible_with`、`unify` unify.rs:304 / :307）。

use std::sync::Arc;

use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::value::Value;

fn run(src: &str) -> Result<Value, String> {
    let (prog, witnesses) =
        mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    let errs = mora::typeck::check_mir::check_program_witnesses(&witnesses);
    if !errs.is_empty() {
        let msgs: Vec<String> = errs.iter().map(|e| e.message.clone()).collect();
        return Err(format!("TYPECK: {msgs:?}"));
    }
    let arc = Arc::new(prog);
    let mut core = CoreRuntime::default();
    execute_program(arc, &mut core)
        .map(|(v, _)| v)
        .map_err(|e| e.to_string())
}

/// D64：`nil` 标注可达（含 union 形态），且不得被放得过松。
#[test]
fn nil_annotation_is_writable() {
    assert_eq!(
        run("let x: nil = nil\nx\n"),
        Ok(Value::Nil),
        "`let x: nil = nil` 必须通过（修前报 expected type annotation）"
    );
    assert_eq!(
        run("let x: nil|int = nil\nx\n"),
        Ok(Value::Nil),
        "`nil` 作为 union 成员也必须可解析"
    );
    assert!(run("let x: nil = 1\nx\n").is_err(), "`nil` 不该接住 Float");
    assert!(
        run("let x: nil = \"hi\"\nx\n").is_err(),
        "`nil` 不该接住 String"
    );
}

/// D64 的回归面：把 `nil` 变成可标注的 token，**不能**顺带改变 `nil` 作为
/// 值的既有语义（关键字位置仍应是 Nil 值、仍能与 any 互通）。
#[test]
fn nil_keyword_semantics_unchanged() {
    assert_eq!(
        run("let y = nil\ny\n"),
        Ok(Value::Nil),
        "`nil` 字面量仍是 Nil 值"
    );
    assert_eq!(
        run("let x: any = nil\nx\n"),
        Ok(Value::Nil),
        "`any` 标注接住 `nil` 应照常"
    );
    // 关键字 `nil` 出现在语句开头时仍是表达式，不是类型标注
    assert_eq!(
        run("let a = 1\nlet y = nil\nprint(a)\ny\n"),
        Ok(Value::Nil),
        "`let y = nil` 不能被 `nil` 标注 arm 截胡"
    );
}
