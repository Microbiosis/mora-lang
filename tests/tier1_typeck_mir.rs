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

#[test]
fn let_identity_polymorphic() {
    // v0.75.17: let-generalization — `let id = fn(x) x` 量化为
    // ∀'a. 'a → 'a。两次调用单形化为不同实例，Int/String 不冲突。
    let src = "let id = fn(x) x end\nid(1)\nid(\"s\")";
    let exprs = parse_code_v3(src).expect("parse");
    assert!(
        check_program_mir(&exprs).is_empty(),
        "identity 两次调用（Int 和 String）应都通过（let-polymorphism）"
    );
}

#[test]
fn let_polymorphic_list_and_pair() {
    // 泛型闭包作用于列表与比较运算：单形化实例互不干扰。
    let src =
        "let id = fn(x) x end\nlet a = id([1, 2])\nlet b = id(\"hi\")\nlet c = [id(3)]\nc[0] == 3";
    let exprs = parse_code_v3(src).expect("parse");
    assert!(check_program_mir(&exprs).is_empty());
}

#[test]
fn generic_type_annotation_list_int_parses() {
    // v0.75.17: parser 泛型注解 — `let x: List<int> = [1i, 2i]` 应解析成功
    // 且类型检查通过。注：无后缀数字字面量 lexer 产出 Float（数值塔分离），
    // 整数需 `i` 后缀（如 `1i`）才得到 Int 类型。
    let src = "let x: List<int> = [1i, 2i]";
    let exprs = parse_code_v3(src).expect("parse should succeed");
    assert!(check_program_mir(&exprs).is_empty());
}

#[test]
fn generic_type_annotation_list_float_parses() {
    // 无后缀数字 → Float：List<float> 注解与 [1, 2] 合一。
    let src = "let x: List<float> = [1, 2]";
    let exprs = parse_code_v3(src).expect("parse should succeed");
    assert!(check_program_mir(&exprs).is_empty());
}

#[test]
fn generic_type_annotation_dict_string_any_parses() {
    // dict<string, any> 注解：any 值合一宽松，应通过。
    let src = "let d: dict<string, any> = {\"k\": 1}";
    let exprs = parse_code_v3(src).expect("parse should succeed");
    assert!(check_program_mir(&exprs).is_empty());
}

#[test]
fn generic_annotation_mismatch_reported() {
    // List<string> 注解 + [1i, 2i]（List<Int>）→ 合一失败应报错。
    let src = "let x: List<string> = [1i, 2i]";
    let exprs = parse_code_v3(src).expect("parse should succeed");
    let errs = check_program_mir(&exprs);
    assert!(
        !errs.is_empty(),
        "List<string> 注解与 List<Int> 值应报类型错误"
    );
}

#[test]
fn import_symbol_resolved_in_typecheck() {
    // v0.75.18: import 目标文件的顶层符号在 typeck 阶段预解析（M3 符号表）—
    // 引用 greeting（string）、answer（int）不报 UnboundVariable。
    // 注：cargo test 的 cwd 是 crate 根，import 相对路径与运行时一致。
    let src = "import \"tests/fixtures/mod_a.mora\"\nlet s = greeting\nlet n = answer\nprint(s)";
    let exprs = parse_code_v3(src).expect("parse");
    assert!(
        check_program_mir(&exprs).is_empty(),
        "import 符号应被解析，无 UnboundVariable"
    );
}

#[test]
fn import_symbol_type_checked() {
    // 导入的 greeting 是 string（精确类型）：与数字运算应报类型错误。
    let src = "import \"tests/fixtures/mod_a.mora\"\nlet s = greeting\ns + 1";
    let exprs = parse_code_v3(src).expect("parse");
    let errs = check_program_mir(&exprs);
    assert!(!errs.is_empty(), "import 的 string 符号 + 数字应报类型错误");
}

#[test]
fn import_missing_file_reports_error() {
    // 缺失的 import 文件 → typeck 阶段产出 import error 诊断
    // （与运行时 mir_import 的 hard error 语义一致）。
    let src = "import \"tests/fixtures/does_not_exist.mora\"\nprint(1)";
    let exprs = parse_code_v3(src).expect("parse");
    let errs = check_program_mir(&exprs);
    assert!(!errs.is_empty(), "缺失 import 文件应报 import error");
}

#[test]
fn freed_reserved_words_usable_as_identifiers() {
    // v0.75.19: 语法面收敛 — 无前端可达的死关键字从 lexer 关键字表移除，
    // 这些词（stream/route/observe/span/worker/transaction/...）回归普通标识符。
    // 此前它们经 token_to_identifier_name fallback 在声明位静默冒充标识符、
    // 表达式位报错（行为不一致）；现在全位置一致可用。
    let src = "let stream = \"s\"\nlet route = \"r\"\nlet observe = stream\nlet span = route\nlet worker = observe\nlet transaction = span\nprint(stream + route + observe + span + worker + transaction)";
    let exprs = parse_code_v3(src).expect("parse");
    assert!(
        check_program_mir(&exprs).is_empty(),
        "移除的保留词应作为普通标识符工作"
    );
}

#[test]
fn pipe_token_now_parses_after_hookup() {
    // v0.75.20: 记录 `|>` 未接入 parse 优先级链（pre-existing parser 限制，
    // `parse_pipe` 死代码，树变体被删时行为与基线一致）。
    // v0.75.21: 该限制已修复——parse_pipe 挂进 parse_equality，`|>` 解析成功。
    // 此测试取代 v0.75.20 的「应报错」断言，锁定修复后的诚实状态。
    let src = "let double = fn(x) x * 2 end\nlet y = 5 |> double";
    assert!(
        parse_code_v3(src).is_ok(),
        "`|>` 已接入 parse 优先级链（v0.75.21），应解析成功"
    );
}

#[test]
fn pipe_syntax_hooked_into_precedence() {
    // v0.75.21: `|>` 接入 parse_equality 之下（优先级低于 equality、
    // 高于 comparison）。脱糖为 Call(right, [left])，typeck 走 infer_call。
    let src = "task double(x)\n  x * 2\nend\ntask add(a, b)\n  a + b\nend\nlet y = 5 |> double\nlet z = 10 |> add(5)\nlet w = 1 + 2 |> double";
    let exprs = parse_code_v3(src).expect("parse should succeed");
    assert!(check_program_mir(&exprs).is_empty());
}

#[test]
fn pipe_keeps_call_callee_name() {
    // v0.75.21: 修复 `x |> f(a)` 的 callee 名丢失 bug — 此前 match_to_string
    // 对 Call 变体返回 "expr"，产出 Call(Name("expr"), [x, a])。
    // 现在保留分支内真名（f），typeck 不会把它当未知调用。
    let src = "task add(a, b)\n  a + b\nend\nlet y = 10 |> add(5)";
    let exprs = parse_code_v3(src).expect("parse should succeed");
    assert!(check_program_mir(&exprs).is_empty());
}
