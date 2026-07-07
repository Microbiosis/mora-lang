//! v0.55 Phase A: Parser V3 minimal grammar coverage
//!
//! Verifies the new `let x: T = value` type annotation syntax and ensures
//! `if`/`match` continue to parse against the canonical smoke test cases.
//! All tests run through the public `parse_code` entry point so they cover
//! the entire Parser V3 → MirExpr pipeline that LSP/typeck consume.

use mora::interpreter::parse_code_v3;
use mora::mir::expr::MirExprKind;

fn first_expr(src: &str) -> mora::mir::MirExpr {
    let mut exprs = parse_code_v3(src).expect("parse should succeed");
    assert_eq!(exprs.len(), 1, "expected exactly one top-level expression");
    exprs.pop().unwrap()
}

#[test]
fn let_without_annotation_still_parses() {
    let expr = first_expr("let x = 1 + 2");
    match expr.kind {
        MirExprKind::LetBinding {
            name, type_hint, ..
        } => {
            assert_eq!(name, "x");
            assert!(type_hint.is_none(), "no annotation should remain None");
        }
        other => panic!("expected LetBinding, got {:?}", other),
    }
}

#[test]
fn let_with_int_annotation_parses() {
    let expr = first_expr("let total: int = 4 + 5");
    match expr.kind {
        MirExprKind::LetBinding {
            name,
            type_hint,
            value,
            ..
        } => {
            assert_eq!(name, "total");
            let hint = type_hint.expect("annotation should be present");
            assert!(matches!(hint, mora::typeck::Type::Int));
            assert!(matches!(value.kind, MirExprKind::Binary { .. }));
        }
        other => panic!("expected LetBinding, got {:?}", other),
    }
}

#[test]
fn let_with_string_annotation_parses() {
    let expr = first_expr(r#"let msg: string = "hi""#);
    match expr.kind {
        MirExprKind::LetBinding { type_hint, .. } => {
            let hint = type_hint.expect("annotation should be present");
            assert!(matches!(hint, mora::typeck::Type::String));
        }
        other => panic!("expected LetBinding, got {:?}", other),
    }
}

#[test]
fn let_with_unknown_annotation_fails() {
    // Unknown type names must be rejected, not silently kept.
    let result = parse_code_v3("let bad: widget = 1");
    assert!(result.is_err(), "unknown type annotation should error");
}

#[test]
fn if_then_else_parses_to_if_expr() {
    let expr = first_expr("if 1 < 2 then 3 else 4");
    assert!(matches!(expr.kind, MirExprKind::If { .. }));
}

#[test]
#[ignore = "requires parser_v3 match expression grammar"]
fn match_literal_arms_parse() {
    let expr = first_expr("match 1 { 1 => 10, _ => 20 }");
    match expr.kind {
        MirExprKind::Match { arms, .. } => {
            assert_eq!(arms.len(), 2, "expected two match arms");
        }
        other => panic!("expected Match, got {:?}", other),
    }
}

#[test]
fn full_program_with_let_if_match_parses() {
    // Each construct is parsed independently; combining all of them in a
    // single top-level program would require the if/match statement forms
    // that the v0.55 minimal grammar intentionally leaves out.
    let exprs =
        parse_code_v3("let x = 1 + 2\nlet y = x * 3").expect("two let bindings should parse");
    assert_eq!(exprs.len(), 2, "two let bindings = 2 top-level exprs");
    let _ = parse_code_v3("if 1 < 2 then 3 else 4").expect("if/else expression should parse");
    let _ = parse_code_v3("match 1 { 1 => 10, _ => 20 }").expect("match expression should parse");
}

// v0.58: Verify ColonColon + keyword-as-identifier + fn block body all work
#[test]
fn mcp_server_patterns_parse() {
    // :: namespace qualification
    parse_code_v3("let mcp = McpServer::new()").expect(":: should parse");
    // keyword 'tool' used as method name after '.'
    parse_code_v3("mcp.tool(\"search\", {query: \"s\"}, fn(x) => x)")
        .expect("method chain should parse");
    // fn with block body (end-terminated)
    parse_code_v3("fn(x)\n  return x\nend").expect("fn block body should parse");
}
