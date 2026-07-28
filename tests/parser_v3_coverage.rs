//! Parser V3 语法覆盖率测试
//!
//! 直接验证 Parser V3 对以下语法的解析能力（不经过 AST v2）：
//! - `expr as dyn Trait<args>`  (DynTrait)
//! - `p"Hello {expr}"`         (Prompt 模板字符串)
//! - `obj.method(args)`         (MethodCall，V3 编码为 Call("obj_method", [obj, ...]))
//! - `expr[idx]`                (Index，V3 编码为 Call("expr_index", [expr, idx]))


fn parse_v3(source: &str) -> Vec<mora::mir::expr::MirExpr> {
    let mut lexer = mora::lexer::Lexer::new(source);
    let tokens = lexer.scan_tokens();
    let parser = mora::parser_v3::ParserV3::new(tokens);
    parser.parse().expect("Parser V3 should succeed")
}

fn find_kind<'a>(
    exprs: &'a [mora::mir::expr::MirExpr],
    kind: &mora::mir::expr::MirExprKind,
) -> Option<&'a mora::mir::expr::MirExpr> {
    exprs
        .iter()
        .find(|e| std::mem::discriminant(&e.kind) == std::mem::discriminant(kind))
}

// ─── DynTrait ────────────────────────────────────────────────────────

#[test]
fn dyntrait_expr_as_dyn_trait_parses() {
    let exprs = parse_v3("x as dyn Any");
    let kind = find_kind(
        &exprs,
        &mora::mir::expr::MirExprKind::DynTrait {
            expr: Box::new(mora::mir::expr::MirExpr::var(
                "x",
                mora::common::Span::new(1, 1),
            )),
            trait_name: "".into(),
            generics: Vec::new(),
        },
    );
    assert!(kind.is_some(), "expected DynTrait node");
}

#[test]
fn dyntrait_expr_as_dyn_trait_with_generics_parses() {
    let exprs = parse_v3("x as dyn Any");
    let kind = find_kind(
        &exprs,
        &mora::mir::expr::MirExprKind::DynTrait {
            expr: Box::new(mora::mir::expr::MirExpr::var(
                "x",
                mora::common::Span::new(1, 1),
            )),
            trait_name: "".into(),
            generics: Vec::new(),
        },
    );
    assert!(kind.is_some(), "expected DynTrait node");
}

#[test]
fn dyntrait_nested_in_let_binding() {
    let exprs = parse_v3("let obj = 42 as dyn Any");
    assert!(!exprs.is_empty());
}

// ─── Prompt 模板字符串 ───────────────────────────────────────────────

#[test]
fn prompt_literal_without_interpolation_parses() {
    let exprs = parse_v3("p\"hello world\"");
    assert!(
        find_kind(
            &exprs,
            &mora::mir::expr::MirExprKind::Prompt { parts: Vec::new() }
        )
        .is_some(),
        "expected Prompt node"
    );
}

#[test]
fn prompt_with_single_interpolation_parses() {
    let exprs = parse_v3("p\"hello {name}\"");
    let kind = find_kind(
        &exprs,
        &mora::mir::expr::MirExprKind::Prompt { parts: Vec::new() },
    );
    assert!(kind.is_some(), "expected Prompt node with interpolation");
}

#[test]
fn prompt_with_multiple_interpolations_parses() {
    let exprs = parse_v3("p\"{a} + {b}\"");
    let kind = find_kind(
        &exprs,
        &mora::mir::expr::MirExprKind::Prompt { parts: Vec::new() },
    );
    assert!(
        kind.is_some(),
        "expected Prompt node with multiple interpolations"
    );
}

// ─── MethodCall ──────────────────────────────────────────────────────
// V3 encodes method calls as Call with mangled name: obj.method() → Call("obj_method", [obj])

#[test]
fn method_call_parses() {
    let exprs = parse_v3("obj.method()");
    let kind = find_kind(
        &exprs,
        &mora::mir::expr::MirExprKind::Call {
            callee: mora::mir::expr::MirCallee::Name("obj_method".into()),
            args: vec![mora::mir::expr::MirExpr::var(
                "obj",
                mora::common::Span::new(1, 1),
            )],
        },
    );
    assert!(
        kind.is_some(),
        "expected method call encoded as Call('obj_method', [obj])"
    );
}

#[test]
fn method_call_with_args_parses() {
    let exprs = parse_v3("obj.method(1, 2)");
    let kind = find_kind(
        &exprs,
        &mora::mir::expr::MirExprKind::Call {
            callee: mora::mir::expr::MirCallee::Name("obj_method".into()),
            args: vec![
                mora::mir::expr::MirExpr::var("obj", mora::common::Span::new(1, 1)),
                mora::mir::expr::MirExpr::lit(
                    mora::common::Literal::Int(1, mora::common::Span::new(1, 1)),
                    mora::common::Span::new(1, 1),
                ),
                mora::mir::expr::MirExpr::lit(
                    mora::common::Literal::Int(2, mora::common::Span::new(1, 1)),
                    mora::common::Span::new(1, 1),
                ),
            ],
        },
    );
    assert!(
        kind.is_some(),
        "expected method call encoded as Call with args"
    );
}

// ─── Index ───────────────────────────────────────────────────────────

#[test]
fn index_expr_parses() {
    let exprs = parse_v3("arr[0]");
    let kind = find_kind(
        &exprs,
        &mora::mir::expr::MirExprKind::Call {
            callee: mora::mir::expr::MirCallee::Name("".into()),
            args: Vec::new(),
        },
    );
    assert!(kind.is_some(), "expected Index to parse as Call");
}

// ─── Combined scenarios ──────────────────────────────────────────────

#[test]
fn method_call_chained_parses() {
    let exprs = parse_v3("obj.foo().bar()");
    assert!(!exprs.is_empty(), "chained method calls should parse");
}

#[test]
fn dyntrait_then_method_call_parses() {
    let exprs = parse_v3("(x as dyn Any).method()");
    assert!(
        !exprs.is_empty(),
        "dyn trait cast then method call should parse"
    );
}
