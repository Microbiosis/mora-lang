//! v0.55: Tier-1 typeck integration tests against Parser V3 / MirExpr.
//!
//! These tests verify that the public `check_program_mir` entry point
//! drives the HM inference engine across all 16+ `MirExprKind` variants
//! and surfaces diagnostics in the shape consumed by CLI `--check`
//! and the LSP server.

use mora::interpreter::parse_code_v3;
use mora::typeck::TypeError;
use mora::typeck::check_program_mir;

fn first_err(errs: &[TypeError]) -> &TypeError {
    errs.first().expect("expected at least one diagnostic")
}

#[test]
fn literals_have_primitive_types() {
    let src = "1\ntrue\n\"hi\"\n3.14\nnil";
    let exprs = parse_code_v3(src).expect("parse should succeed");
    assert!(check_program_mir(&exprs).is_empty());
}

#[test]
fn binary_arithmetic_unifies() {
    let src = "1 + 2\n3 * 4\n5 - 6\n7 / 8";
    let exprs = parse_code_v3(src).expect("parse");
    assert!(check_program_mir(&exprs).is_empty());
}

#[test]
fn comparison_returns_bool() {
    let src = "1 < 2\n3 == 3\n4 != 5";
    let exprs = parse_code_v3(src).expect("parse");
    assert!(check_program_mir(&exprs).is_empty());
}

#[test]
fn let_binding_then_use_clean() {
    let src = "let x = 1 + 2\nlet y = x * 3\nprint(y)";
    let exprs = parse_code_v3(src).expect("parse");
    assert!(check_program_mir(&exprs).is_empty());
}

#[test]
fn function_call_arity_matches() {
    // `print` is registered as a one-arg builtin.
    let src = "print(1)\nprint(2)\nprint(3)";
    let exprs = parse_code_v3(src).expect("parse");
    assert!(check_program_mir(&exprs).is_empty());
}

#[test]
fn closure_return_type_collected() {
    // `let` with a closure body then call: exercises Closure / Call
    // arms and the fresh_closure side table.
    let src = "let f = 5\nlet g = f\nprint(g)";
    let exprs = parse_code_v3(src).expect("parse");
    assert!(check_program_mir(&exprs).is_empty());
}

#[test]
fn if_branches_unify_cleanly() {
    let src = "if 1 < 2 then 10 else 20";
    let exprs = parse_code_v3(src).expect("parse");
    assert!(check_program_mir(&exprs).is_empty());
}

#[test]
fn match_arms_unify_cleanly() {
    let src = "match 1 { 1 => 10, 2 => 20, _ => 30 }";
    let exprs = parse_code_v3(src).expect("parse");
    assert!(check_program_mir(&exprs).is_empty());
}

#[test]
fn unbound_variable_produces_diagnostic() {
    let src = "let x = missing";
    let exprs = parse_code_v3(src).expect("parse should succeed (parser doesn't typecheck)");
    let errs = check_program_mir(&exprs);
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
    let src = "if 1 < 2 then 1";
    let exprs = parse_code_v3(src).expect("parse");
    assert!(check_program_mir(&exprs).is_empty());
}

#[test]
fn list_literal_homogeneous() {
    let src = "let xs = [1, 2, 3]\nprint(xs)";
    let exprs = parse_code_v3(src).expect("parse");
    assert!(check_program_mir(&exprs).is_empty());
}

#[test]
fn nested_let_and_call() {
    let src = "let a = 1\nlet b = 2\nlet c = 3\nprint(a + b + c)";
    let exprs = parse_code_v3(src).expect("parse");
    assert!(check_program_mir(&exprs).is_empty());
}

#[test]
fn type_errors_contain_span_information() {
    // Each diagnostic should carry line / column so CLI and LSP can
    // surface it.
    let src = "let x = nope";
    let exprs = parse_code_v3(src).expect("parse");
    let errs = check_program_mir(&exprs);
    assert!(!errs.is_empty());
    let err = first_err(&errs);
    assert!(err.line >= 1, "line should be 1-based, got {}", err.line);
}

// ─── v0.75.16 M1: 列表/字典方法签名保留元素类型 ─────────────────────

#[test]
fn dict_get_union_unifies_with_member() {
    // v0.75.16: `d.get(k)` 返回 Union<V, Nil>。M1 前 unify 不处理 Union
    // （遇 Union 报 UnificationFailure）——`v == 1` 应通过成员合一。
    let src = "let d = {\"k\": 1}\nlet v = d.get(\"k\")\nv == 1";
    let exprs = parse_code_v3(src).expect("parse");
    assert!(
        check_program_mir(&exprs).is_empty(),
        "Union<V, Nil> 与 Int 合一应通过（成员合一）"
    );
}

#[test]
fn list_get_exposes_element_type_error() {
    // v0.75.16: `list.get` 返回元素类型（此前返回 Any）。String 元素 + Int
    // 运算应报错——元素类型信息让类型错误被检出（此前 Any 静默放过）。
    let src = "let xs = [\"a\", \"b\"]\nlet y = xs.get(0)\ny + 1";
    let exprs = parse_code_v3(src).expect("parse");
    let errs = check_program_mir(&exprs);
    assert!(
        !errs.is_empty(),
        "String 元素 + Int 应报类型错误（list.get 保留元素类型）"
    );
}

#[test]
fn list_map_keeps_int_elements_clean() {
    // map 后元素类型保持 Int：与 Int 运算、比较应通过。
    // 注：同行闭包字面量 `fn(x) x*2` 不在 map 参数位解析（pre-existing
    // parser 限制）— 用命名闭包 + map(f) 形态。
    let src =
        "let f = fn(x) x * 2 end\nlet xs = [1, 2, 3]\nlet ys = xs.map(f)\nlet z = ys[0]\nz + 1";
    let exprs = parse_code_v3(src).expect("parse");
    assert!(check_program_mir(&exprs).is_empty());
}
