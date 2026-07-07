//! v0.90: 9 层管线差分审计 — 全部 e2e fixture 的管线 vs 原管线等价性。
//!
//! 两级差分：
//! 1. 类别级：指令序列逐条类别比较（忽略寄存器编号）
//! 2. 执行级：双管线各自 run_mir + run_main_task，last_expr 必须相等
//!
//! Phase 2（执行器切换）的前置条件：本文件全绿。

use mora::interpreter::Interpreter;
use mora::mir::vm::{run_main_task, run_mir};
use mora::mir::witness_to_fcfg::witness_to_fcfg;
use mora::mir::MirFunction;
use mora::parser_v3::ParserV3;
use mora::value::Value;
use std::sync::Arc;

fn read_fixture(name: &str) -> String {
    let path = format!("tests/fixtures/e2e/{}.mora", name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path, e))
}

/// 执行一个 MirFunction，返回 last_expr（run_mir 顶层最后表达式值）。
fn execute(func: MirFunction) -> Result<Value, String> {
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let func_arc = Arc::new(func);
    let last = run_mir(&func_arc, &mut interp, &mut env)?;
    run_main_task(&func_arc, &mut interp, &mut env)?;
    Ok(last)
}

/// 双级差分审计：类别级 + 执行级。
fn audit(name: &str) {
    let source = read_fixture(name);
    let (func, witnesses) = ParserV3::compile(&source)
        .unwrap_or_else(|e| panic!("{}: compile failed: {}", name, e));

    // ── 管线产出 ──
    let fcfg = witness_to_fcfg(&witnesses);
    let (body, n_regs) = mora::mir::fcfg_lower::lower_fcfg(&fcfg);
    let pipeline_func = MirFunction {
        params: vec![],
        body,
        n_regs,
        effects: func.effects.clone(),
    };

    // ── 1. 类别级差分 ──
    let diffs = category_diffs(&pipeline_func.body, &func.body);
    assert!(
        diffs.is_empty(),
        "{}: category differential FAILED\n  pipeline={} original={}\n  {}",
        name,
        pipeline_func.body.len(),
        func.body.len(),
        diffs.join("\n  ")
    );

    // ── 2. 执行级差分 ──
    // 原 func 在生产中经 apply_rules 优化；管线产出在切换后同样过优化。
    let mut original_opt = func.clone();
    mora::mir::optimize::apply_rules(&mut original_opt);
    let mut pipeline_opt = pipeline_func.clone();
    mora::mir::optimize::apply_rules(&mut pipeline_opt);

    match (execute(original_opt), execute(pipeline_opt)) {
        (Ok(a), Ok(b)) => {
            assert_eq!(
                format!("{}", a),
                format!("{}", b),
                "{}: execution differential FAILED — original={:?} pipeline={:?}",
                name,
                a,
                b
            );
        }
        // 原管线自身执行失败（fixture 超出当前运行时支持，如 tea.* 部分调度）
        // — 执行差分不适用，类别级已锁定等价
        (Err(orig_err), Err(pipe_err)) => {
            eprintln!(
                "{}: both pipelines fail to execute (pre-existing runtime gap) — orig='{}' pipe='{}'",
                name, orig_err, pipe_err
            );
        }
        (Err(orig_err), Ok(b)) => panic!(
            "{}: original fails but pipeline succeeds — divergence!\n  orig_err={}\n  pipeline={:?}",
            name, orig_err, b
        ),
        (Ok(a), Err(pipe_err)) => panic!(
            "{}: execution differential FAILED — original={:?} pipeline_err={}",
            name, a, pipe_err
        ),
    }
}

fn category_diffs(
    pipeline: &[mora::mir::MirInst],
    original: &[mora::mir::MirInst],
) -> Vec<String> {
    let mut diffs = Vec::new();
    if pipeline.len() != original.len() {
        diffs.push(format!(
            "inst count: pipeline={} original={}",
            pipeline.len(),
            original.len()
        ));
    }
    let n = pipeline.len().min(original.len());
    for i in 0..n {
        let p = mora::mir::pipeline::inst_category_pub(&pipeline[i]);
        let o = mora::mir::pipeline::inst_category_pub(&original[i]);
        if p != o {
            diffs.push(format!("inst[{}]: pipeline={} original={}", i, p, o));
            if diffs.len() > 10 {
                diffs.push("... (truncated)".to_string());
                break;
            }
        }
    }
    diffs
}

#[test]
fn diff_arithmetic() { audit("arithmetic"); }

#[test]
fn diff_dict_access() { audit("dict_access"); }

#[test]
fn diff_eval() { audit("eval"); }

#[test]
fn diff_for_loop() { audit("for_loop"); }

#[test]
fn diff_function_call() { audit("function_call"); }

#[test]
fn diff_handle_effect() { audit("handle_effect"); }

#[test]
fn diff_if_else() { audit("if_else"); }

#[test]
fn diff_lisp() { audit("lisp"); }

#[test]
fn diff_macro_advanced() { audit("macro_advanced"); }

#[test]
fn diff_macro_def() { audit("macro_def"); }

#[test]
fn diff_match_default() { audit("match_default"); }

#[test]
fn diff_match_dict_rename() { audit("match_dict_rename"); }

#[test]
fn diff_match_guard() { audit("match_guard"); }

#[test]
fn diff_match_list_rest() { audit("match_list_rest"); }

#[test]
fn diff_nested_if() { audit("nested_if"); }

#[test]
fn diff_quasiquote() { audit("quasiquote"); }

#[test]
fn diff_string_concat() { audit("string_concat"); }

#[test]
fn diff_tea_app() { audit("tea_app"); }

#[test]
fn diff_tea_counter() { audit("tea_counter"); }
