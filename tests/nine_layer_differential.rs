//! v0.90: 9 层管线差分审计 — 全部 e2e fixture 的管线 vs 原管线等价性。
//!
//! Phase 2（执行器切换）的前置条件：本测试全绿。
//! 每个 fixture 运行 run_pipeline，断言差分验证通过。

use mora::mir::pipeline::run_pipeline;
use mora::parser_v3::ParserV3;

fn read_fixture(name: &str) -> String {
    let path = format!("tests/fixtures/e2e/{}.mora", name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path, e))
}

fn audit(name: &str) {
    let source = read_fixture(name);
    let (func, witnesses) = ParserV3::compile(&source)
        .unwrap_or_else(|e| panic!("{}: compile failed: {}", name, e));
    let result = run_pipeline(&func, &witnesses);
    assert!(
        result.differential_ok,
        "{}: differential FAILED\n  fcfg={} typed={} core={} cmir={} lmir={}\n  pipeline_mir={} original_mir={}\n  diffs:\n    {}",
        name,
        result.fcfg_nodes,
        result.typed_nodes,
        result.core_insts,
        result.cmir_nodes,
        result.lmir_insts,
        result.pipeline_mir_count,
        result.original_mir_count,
        result.differential_diffs.join("\n    ")
    );
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
