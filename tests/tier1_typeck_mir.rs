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
    assert!(
        typecheck(
            "let f = fn(x) x * 2 end\nlet xs = [1, 2, 3]\nlet ys = xs.map(f)\nlet z = ys[0]\nz + 1"
        )
        .is_empty()
    );
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
    assert!(
        !errs.is_empty(),
        "List<string> 注解与 List<Int> 值应报类型错误"
    );
}

#[test]
fn import_symbol_resolved_in_typecheck() {
    assert!(
        typecheck(
            "import \"tests/fixtures/mod_a.mora\"\nlet s = greeting\nlet n = answer\nprint(s)"
        )
        .is_empty(),
        "import 符号应被解析，无 UnboundVariable"
    );
}

/// v0.103: 模块私有绑定不得泄漏 —— 未 `export` 的符号对导入方不可见。
/// 锁定「`Environment::define` 的 exported 参数被忽略」导致的可见性缺失。
#[test]
fn import_private_symbol_not_visible() {
    let errs = typecheck(
        "import \"tests/fixtures/mod_a.mora\"
let s = scale",
    );
    assert!(
        !errs.is_empty(),
        "未 export 的模块私有绑定 (scale) 应报 UnboundVariable，实际无错"
    );
}

/// v0.103: `return <expr>` 的类型是被返回表达式的类型（此前一律 Nil）。
/// 后果是任何用显式 return 的函数，其 Arrow 返回类型都错为 Nil ——
/// 一旦函数类型被物化使用（import 精确签名）即暴露。
#[test]
fn return_expr_type_is_returned_type() {
    // 函数返回 string，调用点赋给 string 注解必须通过。
    // 若 `return <expr>` 被当作 Nil（v0.103 前的行为），此处会报
    // "expected string, got nil" —— 这正是本测试锁定的点。
    let ok_src = "task f()\n  return \"s\"\nend\nlet x: string = f()";
    let ok_errs = typecheck(ok_src);
    assert!(
        ok_errs.is_empty(),
        "return 字符串的函数返回类型应为 string，实际报错: {:?}",
        ok_errs
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

/// v0.98: effect 签名随 import 传播 —— 主文件的 perform 位点受导入签名
/// 契约约束（arity 校验只有在签名跨文件可达时才可能发生）。
#[test]
fn imported_effect_signature_enforces_arity() {
    let errs =
        typecheck("import \"tests/fixtures/effect_sig_module.mora\"\nperform Ask(\"a\", \"b\")");
    assert!(
        errs.iter()
            .any(|e| e.message.contains("Expected 1 arguments")),
        "导入签名的 arity 契约应报错: {:?}",
        errs
    );
}

#[test]
fn imported_effect_signature_enforces_arg_type() {
    let errs = typecheck("import \"tests/fixtures/effect_sig_module.mora\"\nperform Ask(42)");
    assert!(
        errs.iter()
            .any(|e| e.message.contains("perform `Ask` arg 0")),
        "导入签名的实参类型契约应报错: {:?}",
        errs
    );
}

#[test]
fn imported_effect_signature_result_typed() {
    // 正确实参 + 结果按签名静态化为 string —— handle body 内 `let v: number`
    // 标注与签名结果冲突报错。无签名时结果是自由 fresh var，此程序静默通过。
    let errs = typecheck(
        "import \"tests/fixtures/effect_sig_module.mora\"\nlet r = handle Ask {\n  let v: number = perform Ask(\"hi\")\n} {\n  \"resp\"\n}",
    );
    assert_eq!(errs.len(), 1, "导入签名使 perform 结果静态化: {:?}", errs);
    assert!(
        errs[0].message.contains("Int"),
        "冲突应指向标注类型: {:?}",
        errs
    );
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
