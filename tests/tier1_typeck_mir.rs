//! v0.55: Tier-1 typeck integration tests against Parser V3.
//!
//! These tests verify that the HM inference engine across all MirExprKind
//! variants surfaces diagnostics in the shape consumed by CLI `--check`
//! and the LSP server.

use mora::parser_v3::ParserV3;
use mora::typeck::TypeError;
use mora::typeck::check_mir::check_program_witnesses;

fn typecheck(src: &str) -> Vec<TypeError> {
    let (_func, witnesses) = ParserV3::compile(src).expect("compile should succeed");
    check_program_witnesses(&witnesses)
}

fn first_err(errs: &[TypeError]) -> &TypeError {
    errs.first().expect("expected at least one diagnostic")
}

#[test]
fn literals_have_primitive_types() {
    assert!(typecheck("1\ntrue\n\"hi\"\n3.14\nnil").is_empty());
}

#[test]
fn binary_arithmetic_unifies() {
    assert!(typecheck("1 + 2\n3 * 4\n5 - 6\n7 / 8").is_empty());
}

#[test]
fn comparison_returns_bool() {
    assert!(typecheck("1 < 2\n3 == 3\n4 != 5").is_empty());
}

#[test]
fn let_binding_then_use_clean() {
    assert!(typecheck("let x = 1 + 2\nlet y = x * 3\nprint(y)").is_empty());
}

#[test]
fn function_call_arity_matches() {
    assert!(typecheck("print(1)\nprint(2)\nprint(3)").is_empty());
}

#[test]
fn closure_return_type_collected() {
    assert!(typecheck("let f = 5\nlet g = f\nprint(g)").is_empty());
}

#[test]
fn if_branches_unify_cleanly() {
    assert!(typecheck("if 1 < 2 then 10 else 20").is_empty());
}

#[test]
fn match_arms_unify_cleanly() {
    assert!(typecheck("match 1 { 1 => 10, 2 => 20, _ => 30 }").is_empty());
}

#[test]
fn unbound_variable_produces_diagnostic() {
    let errs = typecheck("let x = missing");
    assert!(!errs.is_empty(), "expected unbound variable diagnostic");
    let err = first_err(&errs);
    assert!(
        err.message.contains("missing") || err.message.contains("Unbound"),
        "expected 'missing' / 'Unbound' in message, got: {}",
        err.message
    );
}

#[test]
fn if_without_else_unifies_with_nil() {
    assert!(typecheck("if 1 < 2 then 1").is_empty());
}

#[test]
fn list_literal_homogeneous() {
    assert!(typecheck("let xs = [1, 2, 3]\nprint(xs)").is_empty());
}

#[test]
fn nested_let_and_call() {
    assert!(typecheck("let a = 1\nlet b = 2\nlet c = 3\nprint(a + b + c)").is_empty());
}

#[test]
fn type_errors_contain_span_information() {
    let errs = typecheck("let x = nope");
    assert!(!errs.is_empty());
    let err = first_err(&errs);
    assert!(err.line >= 1, "line should be 1-based, got {}", err.line);
}

// ─── v0.75.16 M1: 列表/字典方法签名保留元素类型 ─────────────────────

#[test]
fn dict_get_union_unifies_with_member() {
    assert!(
        typecheck("let d = {\"k\": 1}\nlet v = d.get(\"k\")\nv == 1").is_empty(),
        "Union<V, Nil> 与 Int 合一应通过（成员合一）"
    );
}

#[test]
fn list_get_exposes_element_type_error() {
    let errs = typecheck("let xs = [\"a\", \"b\"]\nlet y = xs.get(0)\ny + 1");
    assert!(!errs.is_empty(), "String 元素 + Int 应报类型错误");
}

#[test]
fn list_map_keeps_int_elements_clean() {
    assert!(typecheck(
        "let f = fn(x) x * 2 end\nlet xs = [1, 2, 3]\nlet ys = xs.map(f)\nlet z = ys[0]\nz + 1"
    ).is_empty());
}

#[test]
fn let_identity_polymorphic() {
    assert!(
        typecheck("let id = fn(x) x end\nid(1)\nid(\"s\")").is_empty(),
        "identity 两次调用（Int 和 String）应都通过（let-polymorphism）"
    );
}

#[test]
fn let_polymorphic_list_and_pair() {
    assert!(typecheck(
        "let id = fn(x) x end\nlet a = id([1, 2])\nlet b = id(\"hi\")\nlet c = [id(3)]\nc[0] == 3"
    ).is_empty());
}

#[test]
fn generic_type_annotation_list_int_parses() {
    assert!(typecheck("let x: List<int> = [1i, 2i]").is_empty());
}

#[test]
fn generic_type_annotation_list_float_parses() {
    assert!(typecheck("let x: List<float> = [1, 2]").is_empty());
}

#[test]
fn generic_type_annotation_dict_string_any_parses() {
    assert!(typecheck("let d: dict<string, any> = {\"k\": 1}").is_empty());
}

#[test]
fn generic_annotation_mismatch_reported() {
    let errs = typecheck("let x: List<string> = [1i, 2i]");
    assert!(!errs.is_empty(), "List<string> 注解与 List<Int> 值应报类型错误");
}

#[test]
fn import_symbol_resolved_in_typecheck() {
    assert!(
        typecheck("import \"tests/fixtures/mod_a.mora\"\nlet s = greeting\nlet n = answer\nprint(s)").is_empty(),
        "import 符号应被解析，无 UnboundVariable"
    );
}

#[test]
fn import_symbol_type_checked() {
    let errs = typecheck("import \"tests/fixtures/mod_a.mora\"\nlet s = greeting\ns + 1");
    assert!(!errs.is_empty(), "import 的 string 符号 + 数字应报类型错误");
}

#[test]
fn import_missing_file_reports_error() {
    let errs = typecheck("import \"tests/fixtures/does_not_exist.mora\"\nprint(1)");
    assert!(!errs.is_empty(), "缺失 import 文件应报 import error");
}

#[test]
fn freed_reserved_words_usable_as_identifiers() {
    assert!(
        typecheck("let stream = \"s\"\nlet route = \"r\"\nlet observe = stream\nlet span = route\nlet worker = observe\nlet transaction = span\nprint(stream + route + observe + span + worker + transaction)").is_empty(),
        "移除的保留词应作为普通标识符工作"
    );
}

#[test]
fn pipe_syntax_hooked_into_precedence() {
    assert!(typecheck(
        "task double(x)\n  x * 2\nend\ntask add(a, b)\n  a + b\nend\nlet y = 5 |> double\nlet z = 10 |> add(5)\nlet w = 1 + 2 |> double"
    ).is_empty());
}

#[test]
fn pipe_keeps_call_callee_name() {
    assert!(typecheck("task add(a, b)\n  a + b\nend\nlet y = 10 |> add(5)").is_empty());
}

#[test]
fn merge_with_builtin_typechecks() {
    assert!(typecheck("merge_with(\"x\", \"grow_only_set\")").is_empty());
}

#[test]
fn merge_with_invalid_strategy_literal_rejected_at_compile_time() {
    let errs = typecheck("merge_with(\"x\", \"bogus\")");
    assert!(!errs.is_empty(), "非法策略名字面量应在 typeck 阶段报错");
}

#[test]
fn merge_with_dynamic_strategy_passes_typecheck() {
    assert!(typecheck("let s = \"append\"\nmerge_with(\"x\", s)").is_empty());
}
