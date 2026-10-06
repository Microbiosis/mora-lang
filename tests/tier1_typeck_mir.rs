//! v0.55: Tier-1 typeck integration tests against Parser V3.
//!
//! These tests verify that the HM inference engine across all MirExprKind
//! variants surfaces diagnostics in the shape consumed by CLI `--check`
//! and the LSP server.

use mora::parser_v3::ParserV3;
use mora::typeck::TypeError;
use mora::typeck::check_mir::check_program_witnesses_bidirectional;

fn typecheck(src: &str) -> Vec<TypeError> {
    let (_func, witnesses) = ParserV3::compile(src).expect("compile should succeed");
    check_program_witnesses_bidirectional(&witnesses)
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

/// v0.104.6 D67：**已从「记录事实」恢复为原始断言** —— 元素类型现在真被追踪了。
///
/// 沿革（三次身份变迁，每一次的「通过」原因都不同，值得留档）：
///
/// 1. **修 D52 之前** —— 它「通过」是因为 `xs.get(0)` 的**下标**被拒
///    （`index: Type::Int` vs 全语言 `Float` 字面量），与被测的 `String + Int`
///    毫无关系。与 D46 的 `app_stmt`、D39 的 `with a = 1` 同型的假阳性。
/// 2. **D52 修好下标之后** —— 它**失败**，正好暴露 D55（元素类型经索引不被追踪）。
///    当时改写为「记录 D55 现状」并在注释里写明恢复条件。
/// 3. **D67 修好 `infer_list` / `infer_dict` 之后** —— 元素类型已是具体的
///    `String`，`y + 1` 如期报 `expected String, got Float`。故按当时写下的
///    指示恢复为原始断言。
///
/// **注意**：D67 顺带揪出下面 `dict_field_access_still_works` 也是同型假阳性 ——
/// 它曾靠 dict 值类型的未解算 `TypeVar`（对任何类型都兼容）蒙混过关。
#[test]
fn list_get_exposes_element_type_error() {
    let errs = typecheck("let xs = [\"a\", \"b\"]\nlet y = xs.get(0)\ny + 1");
    assert!(
        !errs.is_empty(),
        "D67 后 `xs.get(0)` 的元素类型应被追踪为 String，`y + 1` 必须报错"
    );
    assert!(
        errs.iter()
            .any(|e| e.message.contains("String") && e.message.contains("Float")),
        "冲突应指向 String 元素与 Float 运算数: {errs:?}"
    );
}

/// D67 的根因回归钉子：**推断出的容器字面量**的元素/值类型必须是**具体类型**，
/// 而不是永不解算的 `TypeVar`。同一段程序只因「列表本身有没有标注」而给出
/// 两种相反的类型检查结果，是 D67 修前最刺眼的表现。
#[test]
fn inferred_container_element_type_is_concrete() {
    // 推断出的列表：`xs[0]` 是 Float，String 标注必须被拒
    let errs = typecheck("let xs = [1, 2.5]\nlet y: String = xs[0]");
    assert!(
        !errs.is_empty(),
        "推断列表的元素类型应被追踪，`xs[0]` 配 String 标注必须报错"
    );

    // 推断出的字典：`d.get(\"a\")` 的值类型应被追踪
    let errs = typecheck("let d = {a: 1}\nlet y: String = d.get(\"a\")");
    assert!(
        !errs.is_empty(),
        "推断字典的值类型应被追踪，`d.get(\"a\")` 配 String 标注必须报错"
    );

    // 链式索引 `m[0][0]` 同样必须被追踪（修前静默通过）
    let errs = typecheck("let m = [[1, 2]]\nlet y: String = m[0][0]");
    assert!(!errs.is_empty(), "链式索引必须逐层追踪元素类型");

    // 拆成中间变量也不能丢（修前 `let inner = m[0]` 后类型即丢失）
    let errs = typecheck("let m = [[1, 2]]\nlet inner = m[0]\nlet y: String = inner");
    assert!(!errs.is_empty(), "索引结果绑定到变量后类型不得丢失");

    // 空容器仍回落 TypeVar，不得因此收窄（D67 明确保留原行为）
    assert!(
        typecheck("let xs = []\nlet y = xs\ny").is_empty(),
        "空列表的元素类型不可推断，不应因 D67 变成硬错误"
    );
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

/// v0.104.6 D56：**本语言没有 let-polymorphism**，`let` 绑定的函数被
/// **首个调用点单态化**。本测试原断言「identity 可用两种实参」——那是
/// **假阳性**。
///
/// 原因：本文件原先用 `check_program_witnesses`（**弱**检查器），而**生产
/// 路径**（`main.rs:447,500` 的 `run_file` / REPL）用的是
/// `check_program_witnesses_bidirectional`（**强**检查器）。弱检查器放行了
/// 强检查器会拒的程序，于是测试「通过」了 —— 但真实 CLI 早就 exit 2：
///
/// ```text
/// $ mora run poly.mora
/// let id = fn(x) x end
/// let a = id([1, 2])     → id 被单态化成 list<float>
/// let b = id("hi")       → 同一个 id 当 list<float> 用
/// Type error: expected list<float>, got string
/// exit=2
/// ```
///
/// 现改为断言**真实行为**：第二个不同类型的实参被拒。同文件
/// `let_identity_polymorphic` 此前也有同样问题，一并更正。
#[test]
fn let_binding_is_monomorphised_not_polymorphic() {
    // 单态化本身应当正常（首个实参决定 id 的类型）
    assert!(
        typecheck("let id = fn(x) x end\nlet a = id([1, 2])").is_empty(),
        "首个实参的单态化应当通过"
    );
    // 换个实参类型就必须被拒 —— 这才是本语言的实际行为
    let errs = typecheck("let id = fn(x) x end\nlet a = id([1, 2])\nlet b = id(\"hi\")");
    assert!(
        !errs.is_empty(),
        "本语言**没有** let-polymorphism：`id` 被首个实参单态化成 list<float>，\
         再用 string 调它必须报类型错。修前本测试断言「两次调用都通过」——\
         那是弱检查器（check_program_witnesses）造成的假阳性，生产路径用的是\
         check_program_witnesses_bidirectional，真实 CLI 实测 exit 2。"
    );
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

/// v0.104.6 D56：改用**生产同款**检查器（`check_program_witnesses_bidirectional`）
/// 后，本测试的「恰好 1 条错误」不再成立 —— 强的检查器给出 **2 条**，且两条
/// 是**同一个**不匹配被报了两次：
///
/// ```text
/// type mismatch: expected `Int`, got `String`      (line 3, column 19)
/// Type mismatch: expected Int, got String at line 3, column 3
/// ```
///
/// 列号不同（19 vs 3）导致 `check_program_witnesses_bidirectional` 里
/// 按 line+column 去重的过滤漏掉其中一条 —— 这是**诊断质量**问题
/// （同一处冲突报两遍、且位置不一致），不是两个独立缺陷，故本测试不断言
/// 条数。
///
/// 本测试的**本意**是「导入签名把 `perform` 的结果静态化，从而与
/// `let v: number` 的标注冲突而报错」—— 那个意图完全保留。
#[test]
fn imported_effect_signature_result_typed() {
    // 正确实参 + 结果按签名静态化为 string —— handle body 内 `let v: number`
    // 标注与签名结果冲突报错。无签名时结果是自由 fresh var，此程序静默通过。
    let errs = typecheck(
        "import \"tests/fixtures/effect_sig_module.mora\"\nlet r = handle Ask {\n  let v: number = perform Ask(\"hi\")\n} {\n  \"resp\"\n}",
    );
    assert!(
        !errs.is_empty(),
        "导入签名使 perform 结果静态化、与标注冲突，必须报错: {:?}",
        errs
    );
    assert!(
        errs.iter()
            .any(|e| e.message.contains("Int") && e.message.contains("String")),
        "冲突应指向标注类型（Int）与签名结果（String）: {:?}",
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
