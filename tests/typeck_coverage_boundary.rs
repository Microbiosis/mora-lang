//! v0.104.6 D273：typeck 覆盖边界普查 —— **`orchestrate` 整块逃逸类型检查**（**未修**，待裁决）
//!
//! ## ⚠ 本文件是**现状判据**：它断言的是**当前的（错误的）边界**
//!
//! 它今天通过，**恰恰是因为缺陷还在**。修好之后**被检出的那半会变红** ——
//! 那时请把 `expect_checked` 逐项翻成 `true`（这是本文件唯一需要改的地方）。
//!
//! ## 普查结果：在每个上下文植入同一处类型错误 `1 + "str"`
//!
//! | 上下文 | typeck |
//! |---|---|
//! | 顶层 `let z = 1 + "str"` | ✅ 2 诊断 |
//! | 闭包体 `fn(x) { 1 + "str" }` | ✅ 1 诊断 |
//! | tea `app` 的 `update:` 体 | ✅ 3 诊断 |
//! | tea `app` 的 `view:` 体 | ✅ 3 诊断 |
//! | `tea.init(..)` 的闭包实参 | ✅ 2 诊断 |
//! | **orchestrate agent 的 `task_body`** | ❌ **0 诊断** |
//! | **orchestrate 边 `on:` 条件体** | ❌ **0 诊断** |
//! | **orchestrate loop `on:` 条件体** | ❌ **0 诊断** |
//! | **moe expert 的 `def` 体** | ❌ **0 诊断** |
//! | **moe `router` 体** | ❌ **0 诊断** |
//! | **moa `prompt` 体** | ❌ **0 诊断** |
//!
//! ⇒ 缺口**恰好是 `orchestrate` 这个关键字**，不是「内嵌 MirFunction 不参与检查」
//! —— tea 的内嵌体（`update`/`view`/`init` 闭包）**全都参与**。
//! 这修正了 D272 的表述（那条只举了 agent 体，边界比想象的窄也整齐得多）。
//!
//! ## 根因：两处显式跳过，且跳过理由**是错的**
//!
//! | # | 位置 | 代码 |
//! |---|---|---|
//! | 1 | `typeck/bidirectional.rs:613-617` | `WitnessKind::Orchestrate { kind, .. } => { /* …witness 树这里不可见——保守跳过 */ let _ = kind; }` |
//! | 2 | `typeck/hm/mod.rs:919-932` | 只 `env.add(input_var/result_var, Unknown)` 后 `return Ok((Type::Nil, Empty))` |
//!
//! 位置 1 的注释说「递归 `input_var`/`result_var` 引用 witness 树这里不可见」——
//! **这个前提不成立**：`WitnessOrchestrateKind::sub_witnesses()` 早就在
//! `witness.rs` 里把 agent 的 `task_expr`、边的 `condition_expr`、loop 的
//! `exit_when`、moa 的 `prompt`、moe 的 `experts`/`router`/`prompt` **全部枚举出来**，
//! 正是遍历需要的东西。
//!
//! ⇒ 跳过是**两处各自独立**的显式决定，不是某一处的连带后果。
//!
//! ## 已有先例：`for` 循环的同一类缺陷在 v0.104 已修
//!
//! `hm/mod.rs` 里紧挨着的 v0.104 注释记录了**一模一样**的三个症状：
//!
//! > 之前是 v0.55 的桩 `Ok((Type::Nil, Empty))`，**完全不推断 iterable 与 body**，
//! > 于是循环体内的一切错误被静默吞掉：
//! > `print(nosuchvar)` 不报 Unbound、运行期得 nil；效果行不进残差；
//! > `for x in 5i` 非列表到运行期才报错。
//!
//! ⇒ `for` 修好了（迭代变量按元素类型绑定、body 在子作用域推断、效果行并入），
//! **`orchestrate` 是同一类缺陷里没被覆盖的那一个**。这既说明「跳过」不是
//! 深思熟虑的正确选择，也指出了修法的形状。
//!
//! ## 为什么仍然不擅自改（与 D272 的选项③是同一件事）
//!
//! 「保守跳过」的**谨慎本身**有实据，只是理由写错了：agent 体的词法作用域
//! 在 typeck 里**没有模型**。运行时是 `agent_env = env.clone()`（外层全部变量
//! 可见）+ 无条件注入字面量名 `"input"`；而 typeck 登记的是**表头写的那个名字**。
//! 直接遍历 `sub_witnesses()` 会让所有能正常工作的 orchestrate 程序
//! 集体报「Unbound variable `input`」——**假阳性比假阴性更糟**。
//!
//! 要真修，必须先定 agent 的作用域契约（D272 的选项 ②/③），本文件只钉住边界。
//!
//! ## 顺带：tea 的老缺陷已修
//!
//! `tests/fixtures/e2e/tea_counter.mora` 的头注释记着 v0.102 的缺陷链
//! 「② emit 端伪造 update/view witness → **用户的体不参与推断**」。
//! 本普查实测 tea 的 `update`/`view`/`init` 三处**全部参与** ⇒ 那条**已修复**，
//! 注释里的描述已过时。

use mora::parser_v3::ParserV3;

/// 在 `src` 上跑 `check_program_witnesses`，返回诊断数。
fn typeck_error_count(src: &str) -> usize {
    let (_f, w) = ParserV3::compile(src).unwrap_or_else(|e| panic!("compile 应成功: {e}"));
    mora::typeck::check_mir::check_program_witnesses(&w).len()
}

/// 边界总判据：一张表钉住全部上下文。
///
/// `expect_checked = true` ⇒ 该上下文**必须**报出 `1 + "str"` 的类型错误。
/// 修好 typeck 之后，只把 `false` 翻成 `true` 即可。
#[test]
fn d414_typeck_covers_orchestrate_too() {
    struct Case {
        label: &'static str,
        src: &'static str,
        expect_checked: bool,
    }
    let cases = vec![
        // ── 参与推断的对照基准 ──
        Case {
            label: "顶层 let",
            src: "let z = 1 + \"str\"\nz\n",
            expect_checked: true,
        },
        Case {
            label: "闭包体",
            src: "let f = fn(x) { 1 + \"str\" }\nf(1)\n",
            expect_checked: true,
        },
        Case {
            label: "tea app update 体",
            src: "app A\n  model: A\n  msg: A\n  init: {k: 0}\n  update: fn(msg, model) => 1 + \"str\"\n  view: fn(model) => model\nend\nlet r = tea.dispatch(A, {tag: \"T\"})\ntea.run(r, 1)\n",
            expect_checked: true,
        },
        Case {
            label: "tea app view 体",
            src: "app A\n  model: A\n  msg: A\n  init: {k: 0}\n  update: fn(msg, model) => model\n  view: fn(model) => 1 + \"str\"\nend\nlet r = tea.dispatch(A, {tag: \"T\"})\ntea.run(r, 1)\n",
            expect_checked: true,
        },
        // ── 当前逃逸的：orchestrate ──
        Case {
            label: "orchestrate agent task_body",
            src: "orchestrate sequential input -> result\n  agent a => 1 + \"str\"\nend\nresult\n",
            expect_checked: true,
        },
        Case {
            label: "orchestrate 边 on: 条件体",
            src: "orchestrate graph input -> result\n  agent a => \"A\"\n  agent b => \"B\"\n  @start -> a\n  a -> b on: 1 + \"str\"\nend\nresult\n",
            expect_checked: true,
        },
        Case {
            label: "orchestrate loop on: 条件体",
            src: "let acc = \"\"\norchestrate loop acc -> result\n  agent a => input + \"x\"\n  on: 1 + \"str\"\nend\nresult\n",
            expect_checked: true,
        },
        Case {
            label: "moe expert def 体",
            src: "let input = 1\norchestrate moe input -> result\n  experts: { \"e1\": fn (x) { 1 + \"str\" } }\n  top_k: 1\n  router: fn (x) { { \"e1\": 1.0 } }\nend\nresult\n",
            expect_checked: true,
        },
        Case {
            label: "moe router 体",
            src: "let input = 1\norchestrate moe input -> result\n  experts: { \"e1\": fn (x) { x } }\n  top_k: 1\n  router: fn (x) { 1 + \"str\" }\nend\nresult\n",
            expect_checked: true,
        },
        Case {
            label: "moa prompt 体",
            src: "orchestrate moa input -> result\n  layers: 1\n  proposers: [\"p\"]\n  aggregator: \"agg\"\n  prompt: 1 + \"str\"\nend\nresult\n",
            expect_checked: true,
        },
    ];

    let mut mismatches = Vec::new();
    for c in &cases {
        let n = typeck_error_count(c.src);
        let checked = n > 0;
        if checked != c.expect_checked {
            mismatches.push(format!(
                "{}：期望 {}，实际 {} 诊断（观测：{n}）",
                c.label,
                if c.expect_checked {
                    "检出"
                } else {
                    "不检出"
                },
                if c.expect_checked {
                    "不检出"
                } else {
                    "检出"
                },
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "typeck 覆盖边界与现状不符：\n  {}\n\n（若 orchestrate 那几项开始「检出」，说明已修复 —— \
         把对应 expect_checked 翻成 true 即可）",
        mismatches.join("\n  ")
    );
}

/// 边界必须**只**卡在 orchestrate：tea 的内嵌体参与推断。
///
/// 这条是上表里 tea 两项的独立重复，专门防止将来有人误以为
/// 「内嵌函数体都不参与检查」而做出过宽的改动。
#[test]
fn d273_tea_embedded_bodies_do_participate() {
    assert!(
        typeck_error_count(
            "app A\n  model: A\n  msg: A\n  init: {k: 0}\n  update: fn(msg, model) => model\n  view: fn(model) => 1 + \"str\"\nend\nlet r = tea.dispatch(A, {tag: \"T\"})\ntea.run(r, 1)\n"
        ) > 0,
        "tea `view` 体必须参与类型检查（v0.102 的「用户的体不参与推断」已修复）"
    );
}
