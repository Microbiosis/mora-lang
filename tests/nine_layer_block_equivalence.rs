//! v0.104.6 D94/D474：块形态 / 声明形态的**执行级行为契约**。
//!
//! ## 与 D94 版的差别
//!
//! D94 版测的是「原 emit 产出」与「9 层管线产出」的**执行结果是否一致**。
//! **D474 把差分机制整个退役** —— parser 不再发射 MirInst（只产 witness），
//! MIR 全部由 9 层产出 ⇒「参考实现」这一侧**已不存在**，那次比对成了
//! **自己跟自己比**（恒真）。
//!
//! ⇒ 判据换成**钉死的期望值**：每个用例的 `last_expr` 必须是**具体且正确**
//! 的值。这比原判据**更强** —— 原判据只能发现「两条路径不一致」（两条一起
//! 错到同一个错值时反而绿）。
//!
//! ## 这些用例为什么选「让差异出现在 last_expr 上」
//!
//! 块形态的顶层值是 `Nil`（块的值被丢弃），直接断言顶层值**什么也测不到**
//! —— 故每个用例都让块内的绑定**外泄**（已实测：observe / span / parallel /
//! worker 都可以；只有 `with` 不行，见 D91），末表达式取块内定义的名字。
//!
//! `prompt` 的 section text 可经 `compose_prompt(name)` 观测 —— **这正是
//! 缺失尾部 `Return` 会暴露的地方**（D92 曾据此判定它是真实语义差异；
//! D94 实测否证了那条判断：两条路径的 `compose_prompt("p")` 都给出
//! `"\n## p\n\nhello section"`）。

use mora::cli::compile_and_opt;
use mora::mir::control::{ControlProgram, OptLevel};
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::value::Value;
use std::sync::Arc;

/// 执行一个 ControlProgram，返回 `last_expr`（顶层最后表达式值）。
fn last_value(prog: ControlProgram) -> mora::error::MoraResult<Value> {
    let mut core = CoreRuntime::default();
    let (last, _globals) = execute_program(Arc::new(prog), &mut core)?;
    Ok(last)
}

/// 每个用例的源码都让差异出现在 `last_expr` 上。
const CASES: &[(&str, &str, &str)] = &[
    // ── 块形态：块内 let 外泄，末表达式取该名字 ──
    ("parallel", "parallel\n  let v = 42\nend\nv\n", "42"),
    ("worker", "worker w do\n  let v = 42\nend\nv\n", "42"),
    ("struct", "struct P\n  x: number\nend\n1\n", "1"),
    ("enum", "enum E\n  A\n  B\nend\n1\n", "1"),
    // ── 类别缺口 / transaction ──
    ("transaction", "transaction\n  let v = 42\nend\nv\n", "42"),
    // ── 对照组：D92b 判为「通过」 ──
    ("eval", "eval(1 + 1)\n", "2"),
];

/// **主判据**：每个用例的 `last_expr` 必须是钉死的期望值。
#[test]
fn d474_block_and_declaration_forms_have_the_expected_last_value() {
    let mut failures: Vec<String> = Vec::new();
    for (name, source, want) in CASES {
        let level = OptLevel::default();
        let (prog, _witnesses) = match compile_and_opt(source, Some(level)) {
            Ok(v) => v,
            Err(e) => {
                failures.push(format!("  [{name}] 编译失败: {e}"));
                continue;
            }
        };

        let got = last_value(prog);
        match got {
            Ok(v) => {
                let text = v.to_string();
                if text != *want {
                    failures.push(format!("  [{name}] 期望 {want:?}，实得 {text:?}"));
                }
            }
            Err(e) => failures.push(format!("  [{name}] 运行失败: {e}")),
        }
    }
    assert!(
        failures.is_empty(),
        "块形态 / 声明形态的 last_expr 与期望不符（{} 条）：\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// 覆盖面自检：这张表**不能悄悄变小**。
#[test]
fn d474_case_table_keeps_its_size() {
    assert_eq!(
        CASES.len(),
        6,
        "用例表现在 {} 条 —— 若有意增删，请同步更新本断言与 CHANGELOG D474",
        CASES.len()
    );
}
