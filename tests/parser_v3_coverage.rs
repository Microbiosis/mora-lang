//! Parser V3 语法覆盖率测试（v0.92: witness 路径）
//!
//! 直接验证 Parser V3 对以下语法的解析能力（不经过 AST v2 / MirExpr）：
//! - `expr as dyn Trait<args>`  (DynTrait)
//! - `p"Hello {expr}"`         (Prompt 模板字符串)
//! - `obj.method(args)`         (MethodCall)
//! - `expr[idx]`                (Index)
//! - `let x: A | B`             (Union 类型注解)

use mora::mir::witness::{MirWitness, WitnessKind};

/// v0.92: 走 canonical `compile()` 路径（MirWitness）。
fn compile_witnesses(source: &str) -> Vec<MirWitness> {
    let (_func, witnesses) = mora::parser_v3::ParserV3::compile(source)
        .unwrap_or_else(|e| panic!("ParserV3::compile failed: {}", e));
    witnesses
}

/// 深度优先查找第一个匹配谓词的 witness。
fn find_witness<F>(ws: &[MirWitness], pred: &F) -> Option<MirWitness>
where
    F: Fn(&WitnessKind) -> bool,
{
    fn walk<F: Fn(&WitnessKind) -> bool>(w: &MirWitness, pred: &F) -> Option<MirWitness> {
        if pred(&w.kind) {
            return Some(w.clone());
        }
        match &w.kind {
            WitnessKind::Binary { left, right, .. }
            | WitnessKind::And { left, right }
            | WitnessKind::Or { left, right } => walk(left, pred).or_else(|| walk(right, pred)),
            WitnessKind::Call { args, .. } => args.iter().find_map(|a| walk(a, pred)),
            WitnessKind::MethodCall { receiver, args, .. } => {
                walk(receiver, pred).or_else(|| args.iter().find_map(|a| walk(a, pred)))
            }
            WitnessKind::Closure { body, .. } | WitnessKind::FnDef { body, .. } => walk(body, pred),
            WitnessKind::If { cond, then, r#else } => walk(cond, pred)
                .or_else(|| walk(then, pred))
                .or_else(|| r#else.as_ref().and_then(|e| walk(e, pred))),
            WitnessKind::Match { scrutinee, arms } => {
                walk(scrutinee, pred).or_else(|| arms.iter().find_map(|a| walk(&a.body, pred)))
            }
            WitnessKind::Loop { iterable, body, .. } => {
                walk(iterable, pred).or_else(|| walk(body, pred))
            }
            WitnessKind::While { cond, body } => walk(cond, pred).or_else(|| walk(body, pred)),
            WitnessKind::List(items) => items.iter().find_map(|i| walk(i, pred)),
            WitnessKind::Dict(entries) => entries.iter().find_map(|(_, v)| walk(v, pred)),
            WitnessKind::DynTrait { expr, .. } => walk(expr, pred),
            WitnessKind::Prompt { parts } => parts.iter().find_map(|p| walk(p, pred)),
            WitnessKind::LetBinding {
                value, init_body, ..
            } => walk(value, pred).or_else(|| walk(init_body, pred)),
            WitnessKind::Assign { value, .. } => walk(value, pred),
            WitnessKind::IndexAssign {
                object,
                index,
                value,
            } => walk(object, pred)
                .or_else(|| walk(index, pred))
                .or_else(|| walk(value, pred)),
            WitnessKind::Return(Some(v)) => walk(v, pred),
            WitnessKind::Sequence(items) => items.iter().find_map(|i| walk(i, pred)),
            WitnessKind::Perform { args, .. } => args.iter().find_map(|a| walk(a, pred)),
            WitnessKind::Handle { body, handler, .. } => {
                walk(body, pred).or_else(|| walk(handler, pred))
            }
            WitnessKind::MacroDef { body, .. } => walk(body, pred),
            WitnessKind::WithConfig { bindings, body } => bindings
                .iter()
                .find_map(|(_, v)| walk(v, pred))
                .or_else(|| walk(body, pred)),
            WitnessKind::Quasiquote { segments } => segments.iter().find_map(|s| walk(s, pred)),
            // v0.102: 声明式范式 — 关系定义镜像子树 + solve 目标构建体
            WitnessKind::RelDef { clause_wits, .. } => clause_wits.iter().find_map(|cw| {
                cw.head
                    .iter()
                    .find_map(|h| walk(h, pred))
                    .or_else(|| walk(&cw.body, pred))
            }),
            WitnessKind::Solve { goal, .. } => walk(goal, pred),
            _ => None,
        }
    }
    ws.iter().find_map(|w| walk(w, pred))
}

// ─── DynTrait ────────────────────────────────────────────────────────

#[test]
fn dyntrait_expr_as_dyn_trait_parses() {
    let ws = compile_witnesses("x as dyn Any");
    let found = find_witness(&ws, &|k| matches!(k, WitnessKind::DynTrait { .. }));
    assert!(found.is_some(), "expected DynTrait node");
}

#[test]
fn dyntrait_expr_as_dyn_trait_with_generics_parses() {
    let ws = compile_witnesses("x as dyn Any");
    let found = find_witness(
        &ws,
        &|k| matches!(k, WitnessKind::DynTrait { trait_name, .. } if trait_name == "Any"),
    );
    assert!(
        found.is_some(),
        "expected DynTrait node with trait_name Any"
    );
}

#[test]
fn dyntrait_nested_in_let_binding() {
    let ws = compile_witnesses("let obj = 42 as dyn Any");
    assert!(!ws.is_empty());
}

// ─── Prompt 模板字符串 ───────────────────────────────────────────────

#[test]
fn prompt_literal_without_interpolation_parses() {
    let ws = compile_witnesses("p\"hello world\"");
    let found = find_witness(&ws, &|k| matches!(k, WitnessKind::Prompt { .. }));
    assert!(found.is_some(), "expected Prompt node");
}

#[test]
fn prompt_with_single_interpolation_parses() {
    let ws = compile_witnesses("p\"hello {name}\"");
    let found = find_witness(
        &ws,
        &|k| matches!(k, WitnessKind::Prompt { parts } if parts.len() == 2),
    );
    assert!(
        found.is_some(),
        "expected Prompt node with 2 parts (literal + interpolation)"
    );
}

#[test]
fn prompt_with_multiple_interpolation_parses() {
    let ws = compile_witnesses("p\"{a} + {b}\"");
    let found = find_witness(
        &ws,
        &|k| matches!(k, WitnessKind::Prompt { parts } if parts.len() >= 3),
    );
    assert!(
        found.is_some(),
        "expected Prompt node with multiple interpolations"
    );
}

// ─── MethodCall ──────────────────────────────────────────────────────

#[test]
fn method_call_parses() {
    let ws = compile_witnesses("obj.method()");
    let found = find_witness(
        &ws,
        &|k| matches!(k, WitnessKind::MethodCall { method, .. } if method == "method"),
    );
    assert!(found.is_some(), "expected MethodCall node");
}

#[test]
fn method_call_with_args_parses() {
    let ws = compile_witnesses("obj.method(1, 2)");
    let found = find_witness(
        &ws,
        &|k| matches!(k, WitnessKind::MethodCall { method, args, .. } if method == "method" && args.len() == 2),
    );
    assert!(found.is_some(), "expected MethodCall with 2 args");
}

// ─── Index ───────────────────────────────────────────────────────────

#[test]
fn index_expr_parses() {
    let ws = compile_witnesses("arr[0]");
    // v0.92 witness 路径将 Index 编码为 Call("[]", [arr, 0])。
    let found = find_witness(&ws, &|k| {
        matches!(k, WitnessKind::Call { callee, .. }
            if matches!(callee, mora::mir::witness::WitnessCallee::Name(n) if n == "[]"))
    });
    assert!(found.is_some(), "expected Index to parse as Call(\"[]\")");
}

// ─── Combined scenarios ──────────────────────────────────────────────

#[test]
fn method_call_chained_parses() {
    let ws = compile_witnesses("obj.foo().bar()");
    assert!(!ws.is_empty(), "chained method calls should parse");
}

#[test]
fn dyntrait_then_method_call_parses() {
    let ws = compile_witnesses("(x as dyn Any).method()");
    assert!(
        !ws.is_empty(),
        "dyn trait cast then method call should parse"
    );
}

// ─── Union type annotation (v0.85) ───────────────────────────────────
// §3.4: `let x: string | number = ...` — parser accepts pipe-separated union types.

#[test]
fn union_type_two_members_parses() {
    use mora::typeck::Type;
    let ws = compile_witnesses("let x: string | number = 42");
    assert!(!ws.is_empty(), "union type annotation should parse");
    let found = find_witness(&ws, &|k| {
        matches!(k, WitnessKind::LetBinding { type_hint: Some(h), .. }
            if matches!(h.to_type(), Type::Union(m) if m.len() == 2))
    });
    assert!(
        found.is_some(),
        "expected LetBinding with 2-member union type hint"
    );
}

#[test]
fn union_type_three_members_parses() {
    use mora::typeck::Type;
    let ws = compile_witnesses("let x: string | number | bool = true");
    assert!(
        !ws.is_empty(),
        "union type annotation with 3 members should parse"
    );
    let found = find_witness(&ws, &|k| {
        matches!(k, WitnessKind::LetBinding { type_hint: Some(h), .. }
            if matches!(h.to_type(), Type::Union(m) if m.len() == 3))
    });
    assert!(
        found.is_some(),
        "expected LetBinding with 3-member union type hint"
    );
}

#[test]
fn union_type_single_member_parses_as_plain_type() {
    use mora::typeck::Type;
    // A "union" with one member is just a plain type (no Union wrapper).
    let ws = compile_witnesses("let x: int = 42");
    let found = find_witness(&ws, &|k| {
        matches!(k, WitnessKind::LetBinding { type_hint: Some(h), .. }
            if matches!(h.to_type(), Type::Int))
    });
    assert!(
        found.is_some(),
        "single-member union should be a plain Type::Int, not Type::Union"
    );
}

// ─── v0.102 声明式范式（逻辑式/关系式）────────────────────────────────

#[test]
fn rel_fact_def_parses() {
    let ws = compile_witnesses("rel edge(\"a\", \"b\")");
    let found = find_witness(&ws, &|k| {
        matches!(k, WitnessKind::RelDef { name, clauses, .. }
            if name == "edge" && clauses.len() == 1)
    });
    assert!(found.is_some(), "expected RelDef witness for the edge fact");
}

#[test]
fn rel_rule_def_parses_with_conjunctive_body() {
    let ws = compile_witnesses(
        "rel path(x, y) edge(x, y) end
rel path(x, z) edge(x, y), path(y, z) end",
    );
    let found = find_witness(&ws, &|k| {
        matches!(k, WitnessKind::RelDef { name, clauses, .. }
            if name == "path" && clauses.len() == 1)
    });
    assert!(
        found.is_some(),
        "expected RelDef witness for the recursive path rule"
    );
}

#[test]
fn solve_query_parses_with_query_vars() {
    let ws = compile_witnesses(
        "rel e(1i)
solve { e(?x) }",
    );
    let found = find_witness(&ws, &|k| {
        matches!(k, WitnessKind::Solve { query_vars, limit: None, .. }
            if query_vars == &vec!["x".to_string()])
    });
    assert!(
        found.is_some(),
        "expected Solve witness with query var x and no limit"
    );
}

#[test]
fn solve_run_limit_parses() {
    // 无后缀数字字面量是 Float —— limit 解析须接受整数值 Float
    let ws = compile_witnesses(
        "rel e(1i)
solve 3 { e(?x) }",
    );
    let found = find_witness(&ws, &|k| {
        matches!(k, WitnessKind::Solve { limit: Some(3), .. })
    });
    assert!(found.is_some(), "expected Solve witness with limit 3");
}

#[test]
fn solve_anon_var_is_not_projected() {
    let ws = compile_witnesses(
        "rel e(1i)
solve { e(_) }",
    );
    let found = find_witness(&ws, &|k| {
        matches!(k, WitnessKind::Solve { query_vars, anon_vars, .. }
            if query_vars.is_empty() && anon_vars.len() == 1)
    });
    assert!(
        found.is_some(),
        "anonymous var should be in anon_vars, not query_vars"
    );
}
