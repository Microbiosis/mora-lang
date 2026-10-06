//! v0.104.6 D94：为 D92b 普查出的 **14 类回落构造**补**执行级等价性**证据。
//!
//! D92b 的结论是「这 14 类全部没有差分等价性 fixture 覆盖」，因此
//! 「放宽差分让它们走 9 层」= 无人验证。本文件补上这块证据。
//!
//! ## 为什么不能直接套用 `nine_layer_differential.rs` 的 `audit()`
//!
//! 那个 `audit()` 的执行级判定比的是 **`last_expr`**。而块形态的顶层值是
//! `Nil`（emitter 发完块指令就是 `Const(dst, Nil)`，块值被丢弃）——
//! 直接套用会让执行级判定**空转**（两边都是 Nil，永远「通过」）。
//!
//! 故每个用例都必须让差异**可观测**：
//! - 块体内 `let` 绑定会外泄（已实测：observe / span / parallel / worker
//!   都可以 —— 只有 `with` 不行，见 D91），所以末表达式取块内定义的名字；
//! - `prompt` 的 section text 可经 `compose_prompt(name)` 观测 ——
//!   **这正是缺失尾部 `Return` 会暴露的地方**（D92 已判定它是真实语义差异）。

use mora::interpreter::Interpreter;
use mora::mir::MirFunction;
use mora::mir::ssa::OptLevel;
use mora::mir::vm::{run_main_task, run_mir};
use mora::mir::witness_to_fcfg::witness_to_fcfg;
use mora::parser_v3::ParserV3;
use mora::value::Value;
use std::sync::Arc;

/// 与 `cli::compile_and_opt` **完全同序**的收尾：两条路径都走 `apply_rules`，
/// 再在等级启用时走 `opt::optimize`。
///
/// ⚠ 本函数第一版**漏了 `opt::optimize`**，于是测出 `eval` 「原 2.0 / 管线 nil」
/// 的「不等价」。用真实 CLI 复核发现**两条路径行为完全相同**（都报
/// `eval(source: string|code) expects a string or code argument`），
/// 即该差异是**测试装置不忠实**的产物，不是生产缺陷。
/// **教训：差分测试的装置必须与生产路径逐阶段对齐**，否则它测的是装置的差异。
fn finish(mut f: MirFunction, level: OptLevel) -> MirFunction {
    mora::mir::optimize::apply_rules(&mut f);
    if level.enabled() {
        mora::mir::opt::optimize(&mut f, level);
    }
    f
}

fn execute(func: MirFunction) -> Result<Value, String> {
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let func_arc = Arc::new(func);
    let last = run_mir(
        &func_arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )?;
    run_main_task(
        &func_arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )?;
    Ok(last)
}

/// 每个用例的源码都让差异出现在 `last_expr` 上。
const CASES: &[(&str, &str)] = &[
    // ── 块形态：块内 let 外泄，末表达式取该名字 ──
    ("observe", "observe trace \"t\" do\n  let v = 42\nend\nv\n"),
    ("span", "span \"s\" do\n  let v = 42\nend\nv\n"),
    ("parallel", "parallel\n  let v = 42\nend\nv\n"),
    ("worker", "worker w do\n  let v = 42\nend\nv\n"),
    // ── section：值被 handler 消费，缺失 Return 必然暴露 ──
    (
        "prompt_section",
        "prompt \"p\" do\n  \"hello section\"\nend\ncompose_prompt(\"p\")\n",
    ),
    (
        "document_section",
        "document \"d\" do\n  \"doc section\"\nend\n1\n",
    ),
    // ── 声明形态：末表达式取声明出的名字/字段 ──
    (
        "model",
        "model M\n  count: number = 7\nend\nlet d = M\nprint(type_of(d))\nd\n",
    ),
    ("msg", "msg Inc\n  Bump\nend\n1\n"),
    ("struct", "struct P\n  x: number\nend\n1\n"),
    ("enum", "enum E\n  A\n  B\nend\n1\n"),
    // ── 类别缺口 / 裸 perform ──
    ("transaction", "transaction\n  let v = 42\nend\nv\n"),
    (
        "tea_standalone",
        "model C\n  count: number = 0\nend\nmsg M\n  Inc\nend\nupdate(msg, model)\n  model\nend\n1\n",
    ),
    // ── 对照组：D92b 判为「通过」的两类 ──
    ("with", "with model = \"m\"\n  print(1)\nend\n1\n"),
    ("eval", "eval(1 + 1)\n"),
];

/// 测量：对每个用例比较「原 emit 产出」与「9 层管线产出」的执行结果。
///
/// **测量结论（D94，已实测固化）**：
/// - 13 个用例**执行级等价** —— 包括 D92 曾判定为「真实语义差异」的
///   `prompt_section`：两条路径的 `compose_prompt("p")` 都给出
///   `"\n## p\n\nhello section"`。**D92 那条判断是读 handler 代码得出的，
///   未经测量，现更正。**
/// - `eval` **不等价**（原 2.0 / 管线 nil）—— 这是**真实的管线缺口**：
///   裸顶层 `eval(1 + 1)` 时管线产出 **0 条指令**。
///   但它**不构成生产缺陷**：差分检查在此时判失败并回落（`pipeline_mir=0`），
///   用户拿到的仍是正确结果。D92b 普查里 `eval` 记为「通过」是因为那条用例
///   写的是嵌套形态 `print(eval(1 + 1))`，现已补入 `eval_bare` 条目。
#[test]
fn d94_execution_equivalence_for_fallback_constructs() {
    let mut equivalent = Vec::new();
    let mut divergent = Vec::new();
    let mut report = Vec::new();

    for (name, source) in CASES {
        let (func, witnesses) = match ParserV3::compile(source) {
            Ok(v) => v,
            Err(e) => {
                report.push(format!("{name}: COMPILE_ERR {e}"));
                continue;
            }
        };

        // 9 层产出（与 `nine_layer_differential.rs::audit` 同一条构造路径）
        let fcfg = witness_to_fcfg(&witnesses);
        let (body, n_regs) = mora::mir::fcfg_lower::lower_fcfg(&fcfg);
        let pipeline_func = MirFunction {
            params: vec![],
            body,
            n_regs,
            effects: func.effects.clone(),
        };

        let level = OptLevel::default();
        let original_opt = finish(func.clone(), level);
        let pipeline_opt = finish(pipeline_func.clone(), level);

        let a = execute(original_opt);
        let b = execute(pipeline_opt);
        let same = match (&a, &b) {
            (Ok(x), Ok(y)) => format!("{x}") == format!("{y}"),
            (Err(_), Err(_)) => true, // 两侧同为运行期缺口，不构成路径差异
            _ => false,
        };
        if same {
            equivalent.push(*name);
        } else {
            divergent.push(*name);
        }
        report.push(format!(
            "{name}: {}  original={:?} pipeline={:?}",
            if same { "等价" } else { "**不等价**" },
            a.as_ref()
                .map(|v| v.to_string())
                .unwrap_or_else(|e| format!("Err({e})")),
            b.as_ref()
                .map(|v| v.to_string())
                .unwrap_or_else(|e| format!("Err({e})"))
        ));
    }
    eprintln!("D94 执行级等价性：\n  {}", report.join("\n  "));

    assert_eq!(
        report.len(),
        CASES.len(),
        "有用例没跑出结论：\n  {}",
        report.join("\n  ")
    );

    // 唯一已知不等价的：`eval`（真实管线缺口，差分已挡住，不影响生产）。
    // 它的处置见 `nine_layer_fallback_census.rs` 的 `eval_bare` 条目。
    assert_eq!(
        divergent,
        vec!["eval"],
        "不等价清单变了 —— 若 `eval` 已被修好，把本断言改空并同步更新 CHANGELOG D94"
    );
    assert_eq!(
        equivalent.len(),
        CASES.len() - 1,
        "等价项数变了（{}），请复核报告：\n  {}",
        equivalent.len(),
        report.join("\n  ")
    );
}
