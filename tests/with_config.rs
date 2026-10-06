//! v0.104.6 D39：`with` 块对**无效配置**一律静默丢弃。
//!
//! ## 现象（修前，真实 CLI `mora run` 实测，**全部 exit 0、零提示**）
//!
//! ```text
//! with temperature = "hot"   # 类型错 → 静默跳过，配置根本没生效
//! with max_tokens  = "100"   # 同上
//! with budget      = 100     # spec §11.1 :811-818 明确承诺的键 → 静默丢弃
//! with per_call    = 50      # 同上
//! with modle       = "gpt-4o"# 拼写错误 → 静默丢弃，AI 调用照常用**默认模型**
//! ```
//!
//! 最恶劣的是最后一条：用户以为设了模型名，实际**什么都没设**，`ai.chat`
//! 静默用默认模型跑完，退出码 0。这与 D1（`range` 实参类型不对 → 静默取
//! 默认值，从而循环体一次都不执行）是**同一种失败模式**：实参存在但无效，
//! 却悄悄降级成「用默认值」而不是报错。
//!
//! ## 根因
//!
//! `interpreter::mir_with_config` 的键分派：
//!
//! * 未知键落 `_ => {}` —— **静默丢弃**；
//! * `temperature` / `max_tokens` 只在 `Value::Float` 时赋值，否则
//!   `if let` 落空即什么都不做；
//! * `AiConfigValue`（`runtime/types.rs:82`）**没有** `budget` / `per_call`
//!   字段，尽管 spec §11.1 明确列了这两个键 —— 于是它们也落进 `_ => {}`。
//!
//! ## 与 D1 同源的判据
//!
//! 「实参存在但无效 → 报错；只有真的缺席才用默认值」。D1 修 `range` 时确立的
//! 原则，这里原样适用。
//!
//! ## 本文件不测的（有意）
//!
//! `with` **不是变量绑定块**，而是上下文配置块（spec §11.1）。所以
//! `with a = 1` 之后在块内按名引用 `a` 报 `Unbound variable 'a'` 是
//! **正确行为**，不是缺陷 —— 早先一轮曾把它误判为作用域缺陷，spec 定性后
//! 已否证。`tests/spec_ebnf_surface.rs` 里那条 `with_stmt` 用例原本用
//! 非法键 `a`，在 D39 修好后立刻失败 —— 正好证明它是一条空用例，已改用
//! 合法键。

use std::sync::Arc;

use mora::interpreter::Interpreter;
use mora::mir::effect::Effects;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use mora::typeck::check_mir::check_program_witnesses_bidirectional;

/// 走生产路径（`cli::compile_and_opt` = 9 层管线 + 优化）执行 `with` 块，
/// 返回运行期错误字符串或 `Ok`。
fn run_with(src: &str) -> Result<(), String> {
    let (func, witnesses) =
        mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    let errs = check_program_witnesses_bidirectional(&witnesses);
    if !errs.is_empty() {
        return Err(format!(
            "TYPECK: {:?}",
            errs.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
        ));
    }
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    run_mir(&arc, &mut interp, &mut env, &mut Effects::new()).map(|_| ())
}

// ===================================================================
// 合法配置键：必须照常通过
// ===================================================================

#[test]
fn spec_promised_keys_are_accepted() {
    for key in ["model", "temperature", "max_tokens", "system", "mock_llm"] {
        let value = match key {
            "model" | "system" => "\"gpt-4o\"",
            "mock_llm" => "[\"resp\"]",
            _ => "0.7",
        };
        let src = format!("with {key} = {value}\n  1\nend");
        run_with(&src).unwrap_or_else(|e| panic!("`with {key} = {value}` 应被接受，实得: {e}"));
    }
}

/// `mock_responses` 是 `mock_llm` 的别名，实现里也消费。
#[test]
fn mock_responses_alias_is_accepted() {
    run_with("with mock_responses = [\"r\"]\n  1\nend").expect("别名应被接受");
}

// ===================================================================
// D39：无效配置必须报错，不能静默
// ===================================================================

/// 类型不匹配 → 报错（修前静默跳过）。
///
/// 裸数字字面量在本语言里是 `Value::Float`，所以 `temperature = 1` 合法；
/// 传字符串才是类型错。
#[test]
fn type_mismatch_is_reported_not_silently_skipped() {
    let err =
        run_with("with temperature = \"hot\"\n  1\nend").expect_err("temperature 传字符串必须报错");
    assert!(
        err.contains("temperature") && err.contains("number"),
        "错误信息应指明键名与期望类型，实得: {err}"
    );

    let err =
        run_with("with max_tokens = \"100\"\n  1\nend").expect_err("max_tokens 传字符串必须报错");
    assert!(
        err.contains("max_tokens") && err.contains("number"),
        "实得: {err}"
    );
}

/// 未知 / 拼错的键 → 报错，且错误信息**列出 spec 支持的键**（拼写错误正是
/// 用户最需要指路的情形）。
#[test]
fn unknown_key_is_reported_with_the_supported_list() {
    let err = run_with("with modle = \"gpt-4o\"\n  1\nend").expect_err("拼错的键必须报错");
    assert!(err.contains("modle"), "应回显出错的键名，实得: {err}");
    for k in ["model", "temperature", "max_tokens", "mock_llm"] {
        assert!(err.contains(k), "错误信息应列出合法键 `{k}`，实得: {err}");
    }
}

/// spec §11.1 明确承诺、实现却没有的键 → 报「承诺但未实现」，而不是让用户
/// 以为设上了。
///
/// 这两个键此前落进未知键的 `_ => {}` 静默丢弃；与普通拼写错误不同，它们
/// 是**规范承诺过的**，报错信息必须说清这一点，否则用户会以为是拼错了。
#[test]
fn spec_promised_but_unimplemented_keys_say_so() {
    for key in ["budget", "per_call"] {
        let err = run_with(&format!("with {key} = 100\n  1\nend"))
            .expect_err("spec 承诺但未实现的键必须报错，而不是静默丢弃");
        assert!(
            err.contains(key) && err.contains("not implemented"),
            "`{key}` 的错误信息应说明「spec 承诺但未实现」，实得: {err}"
        );
    }
}

/// 回归护栏：`with` 块**之后**的语句必须照常执行（配置块不能吃掉后续）。
#[test]
fn statements_after_a_with_block_still_run() {
    let (func, witnesses) =
        ParserV3::compile("with model = \"gpt-4o\"\n  1\nend\n7").expect("编译");
    let errs = check_program_witnesses_bidirectional(&witnesses);
    assert!(errs.is_empty(), "typeck: {errs:?}");
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let v = run_mir(&arc, &mut interp, &mut env, &mut Effects::new()).expect("执行");
    assert_eq!(format!("{v:?}"), "Float(7.0)", "with 之后的末表达式应求值");
}
