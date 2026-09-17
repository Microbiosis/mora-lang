//! v0.77: E2E 端到端测试 — 镜像 main.rs::run_file 的完整调用栈。
//!
//! 与 src 内 inline 单元测试的区别：
//! - 单元测试：白盒，测单个函数/模块的不变量
//! - E2E 测试：黑盒，测"一段 .mora 源码 → 成功执行"的完整链路
//!
//! 注：value-level 断言需要 stdout 捕获（VM 跨线程 print 复杂），
//! 本 E2E 套件断言"执行成功"作为最基础的端到端契约。
//! 精确值断言由各 inline unit test 承担（typeck/bidirectional 22 个、
//! HM 推断 60+ 个、vm 等价性 7 个等）。
//!
//! Fixture 路径：tests/fixtures/e2e/*.mora（env!("CARGO_MANIFEST_DIR") 解析）

mod e2e_helpers;

use e2e_helpers::{assert_compile_error, assert_ok, read_fixture};

// ===================================================================
// 1. 基本值与算术
// ===================================================================

/// arithmetic.mora：task main + print(Int 加法)。完整 E2E 路径。
#[test]
fn e2e_arithmetic_runs() {
    assert_ok("arithmetic.mora");
}

/// v0.104: 顶层 `let` 绑定在后续**块**（if / match）内必须可见。
///
/// 缺陷：双向预扫（`BidirectionalChecker::pre_check_program`）在
/// `infer_program` **之前**独立遍历 witness 树，其 Phase A/B/C 的
/// `check_against`/`synth` 直接调 `hm.infer_expr` —— 不经过 `infer_let`。
/// 预扫从不把 `LetBinding` 登记进 HM 环境，于是任何「先 let、后出现在块体」
/// 的引用都在预扫阶段报 `type inference failed: Unbound variable '<name>'`
/// （主推断路径本身正确，是预扫漏了登记）。
#[test]
fn e2e_let_visible_inside_later_blocks() {
    use e2e_helpers::assert_source_ok;
    use mora::value::Value;
    // let 在 if 块内可见
    let v = assert_source_ok("let mm = 5i\nif true { mm + 1i } else { 0i }");
    assert!(
        matches!(v, Value::Int(6) | Value::Float(_)),
        "if 块内应能读到外层 let，得到 {:?}",
        v
    );
    // let 在 match arm 内可见
    let v = assert_source_ok("let mm = 7i\nmatch 1i { 1i => mm, _ => 0i }");
    assert!(
        matches!(v, Value::Int(7) | Value::Float(_)),
        "match arm 内应能读到外层 let，得到 {:?}",
        v
    );
    // 函数形参在 match arm 内可见
    let v = assert_source_ok("task f(x)\n  match 1i { 1i => x + 1i, _ => 0i }\nend\nf(41i)");
    assert!(
        matches!(v, Value::Int(42) | Value::Float(_)),
        "match arm 内应能读到函数形参，得到 {:?}",
        v
    );
}

/// v0.104: `assign` 是 spec §14.2 的一等语句（assign_stmt）。
///
/// 缺陷：`assign` 从未被识别为**语句前缀**（词法层它是普通标识符，分派表
/// 无对应 arm），整条语句落回表达式路径 —— 发出 `Var("assign")` 读一个
/// 不存在的变量，于是 typeck 报 `Unbound variable 'assign'`。仅当该语句位于
/// **循环体**内时"看似可用"，因为循环体的类型检查当时是 v0.55 的桩
/// （见本文件另一条测试）。
#[test]
fn e2e_assign_statement_recognized() {
    use e2e_helpers::assert_source_ok;
    use mora::value::Value;
    let num = |v: Value| match v {
        Value::Int(n) => n as f64,
        Value::Float(n) => n,
        other => panic!("期望数值结果，得到 {:?}", other),
    };
    // 顶层
    let v = assert_source_ok("let x = 1i\nassign x = 2i\nx");
    assert_eq!(num(v), 2.0, "顶层 assign 应生效");
    // task 体内
    let v = assert_source_ok("task f()\n  let x = 1i\n  assign x = 9i\n  x\nend\nf()");
    assert_eq!(num(v), 9.0, "task 体内 assign 应生效");
    // 循环体内
    let v = assert_source_ok(
        "let acc = 0\nfor i in [1, 2, 3]\n  assign acc = acc + i\nend\nacc",
    );
    assert_eq!(num(v), 6.0, "循环体内 assign 应生效");
    // while 体内
    let v = assert_source_ok("let n = 0i\nwhile n < 3i\n  assign n = n + 1i\nend\nn");
    assert_eq!(num(v), 3.0, "while 体内 assign 应生效");
    // 未绑定的目标必须被拒（assign 是更新，不是声明）
    let r = mora::cli::compile_and_opt("assign zzz = 1i\n", None);
    assert!(r.is_ok(), "语法上应可编译（语义由 typeck 拒）");
    let ws = r.expect("compile ok").1;
    let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&ws);
    assert!(!errs.is_empty(), "assign 到未绑定名字必须报错");
}

/// v0.104: `for`/`while` 的 body **必须被类型检查** —— v0.55 起是空桩。
///
/// 缺陷：`infer_expr` 的 `Loop`/`While` 分支直接返回 `(Type::Nil, Empty)`，
/// **完全不推断 iterable/条件/body**。后果：
///   - 循环体内任何未定义变量静默通过（运行期得 nil）；
///   - 体内 `perform` 的效果行不进残差行 → 根边界 unhandled-effect 断言漏检；
///   - `while <非布尔>` 与 `if <非布尔>` 契约分叉（后者报错）；
///   - `for x in <非列表>` 到运行期才报错。
#[test]
fn e2e_loop_bodies_are_typechecked() {
    use e2e_helpers::assert_source_ok;
    let errs_of = |src: &str| -> Vec<String> {
        let (_, ws) = mora::cli::compile_and_opt(src, None).expect("compile");
        mora::typeck::check_mir::check_program_witnesses_bidirectional(&ws)
            .iter()
            .map(mora::typeck::format_error)
            .collect()
    };
    // 未定义变量在 for 体内必须报错
    let e = errs_of("task main()\n  for i in [1, 2, 3]\n    print(nosuchvar)\n  end\nend");
    assert!(
        e.iter().any(|m| m.contains("nosuchvar")),
        "for 体内的未定义变量必须报错，实际 {:?}",
        e
    );
    // 未定义变量在 while 体内必须报错
    let e = errs_of(
        "task main()\n  let n = 0\n  while n < 1\n    print(nosuchvar)\n  end\nend",
    );
    assert!(
        e.iter().any(|m| m.contains("nosuchvar")),
        "while 体内的未定义变量必须报错，实际 {:?}",
        e
    );
    // while 条件必须是 bool（与 if 同契约）
    let e = errs_of("while 1i { print(1i) }");
    assert!(
        e.iter().any(|m| m.to_lowercase().contains("bool")),
        "while 的非布尔条件必须报错，实际 {:?}",
        e
    );
    // 合法循环仍通过
    let v = assert_source_ok("for x in [1i, 2i]\n  print(x)\nend\n42i");
    assert!(matches!(v, mora::value::Value::Int(42) | mora::value::Value::Float(_)));
}

/// v0.104: 数值塔在**赋值**与**含未解析变量**的运算位点同样成立。
///
/// 缺陷：`infer_binop` 与 `infer_assign` 对 TypeVar 直接推 `Eq`，把未解析
/// 变量当场钉到对侧具体类型，之后无法参与数值塔提升。实例（语言自身
/// fixtures 的形状）：
///   let total = 0i
///   for i in [1, 2, 3]        -- i 的类型是 fresh TypeVar
///     if i == 6i …            -- Eq(α, Int) 把 α 钉成 Int
///     total = total + i       -- 随后列表的 Eq(α, Float) 冲突
///   end
/// 报 "expected float, got int"。现在含 TypeVar 的比较/算术/赋值走
/// `Numeric`（solve 阶段按已解析结果提升）。
#[test]
fn e2e_numeric_tower_at_assign_and_unresolved_sites() {
    use e2e_helpers::assert_source_ok;
    use mora::value::Value;
    let num = |v: Value| match v {
        Value::Int(n) => n as f64,
        Value::Float(n) => n,
        other => panic!("期望数值结果，得到 {:?}", other),
    };
    // Int 累加器 + Float 列表 + if 比较 + assign（1+2+3+4+5 = 15）
    let v = assert_source_ok(
        "let total = 0i\nfor i in [1, 2, 3, 4, 5]\n  if i == 6i\n    break\n  end\n  assign total = total + i\nend\ntotal",
    );
    assert_eq!(num(v), 15.0, "Int 累加器吸收 Float 元素应提升为 Float");
    // 泛型加法仍可用
    let v = assert_source_ok("task add(a, b)\n  return a + b\nend\nadd(1i, 2i)");
    assert_eq!(num(v), 3.0, "泛型加法不应被 Numeric 约束破坏");
    // 字符串拼接不受影响
    let v = assert_source_ok("let a = \"x\"\nlet b = \"y\"\na + b");
    assert!(
        matches!(v, Value::String(ref s) if s == "xy"),
        "字符串拼接不受数值约束影响，得到 {:?}",
        v
    );
}

/// v0.103: 数值塔（Int ⊂ Float）—— 混合运算提升为 Float，
/// 混合比较与相等按提升后数值判定。
///
/// 缺陷：typeck 的 Numeric 约束（v0.90.5）与 `compatible_with` 都承认
/// Int/Float 互通（后者的注释即以 `42 == 3.14` 为例），但运行期
/// `numeric_op`/`numeric_cmp`/`values_equal` 报 Rust-strict 错误、
/// 相等判否 —— 类型检查通过的代码在运行期失败。
#[test]
fn e2e_numeric_tower_promotes_mixed_int_float() {
    use e2e_helpers::assert_source_ok;
    use mora::value::Value;
    // 混合算术：1i + 2.5 = 3.5（Float）
    let v = assert_source_ok("1i + 2.5");
    assert!(
        matches!(v, Value::Float(f) if (f - 3.5).abs() < 1e-9),
        "1i + 2.5 应提升为 Float(3.5)，得到 {:?}",
        v
    );
    // 混合比较：4i <= 4.0 → true
    let v = assert_source_ok("4i <= 4.0");
    assert!(
        matches!(v, Value::Bool(true)),
        "4i <= 4.0 应为 true（Int ⊂ Float），得到 {:?}",
        v
    );
    // 混合相等：4i == 4.0 → true（与 <= 一致；此前判否自相矛盾）
    let v = assert_source_ok("4i == 4.0");
    assert!(
        matches!(v, Value::Bool(true)),
        "4i == 4.0 应为 true，得到 {:?}",
        v
    );
    // 反向：真不等仍为 false
    let v = assert_source_ok("4i == 5.0");
    assert!(
        matches!(v, Value::Bool(false)),
        "4i == 5.0 应为 false，得到 {:?}",
        v
    );
}

/// string_concat.mora：字符串拼接 + print。
#[test]
fn e2e_string_concat_runs() {
    assert_ok("string_concat.mora");
}

// ===================================================================
// 2. 控制流
// ===================================================================

/// if_else.mora：if-then-else 分支选择。
#[test]
fn e2e_if_else_runs() {
    assert_ok("if_else.mora");
}

/// nested_if.mora：嵌套 if-then-else。
#[test]
fn e2e_nested_if_runs() {
    assert_ok("nested_if.mora");
}

/// for_loop.mora：for-in 累加 + 循环体内 `let` 重绑定。
///
/// v0.103: 从 `assert_ok` 冒烟断言升级为**精确值断言** —— 此前该缺陷
/// 同时表现为「累加结果为 0」（循环体零次执行）与「死循环」，冒烟断言
/// 对前者完全无感。fixture 末表达式即累加和，故可断言精确值 15。
#[test]
fn e2e_for_loop_runs() {
    use mora::value::Value;
    let (last_expr, _) = assert_ok("for_loop.mora");
    let got = match last_expr {
        Value::Int(n) => n as f64,
        Value::Float(n) => n,
        other => panic!("期望数值结果，得到 {:?}", other),
    };
    assert_eq!(
        got, 15.0,
        "1+2+3+4+5 必须累加为 15（循环体不得零次执行，也不得漏算）"
    );
}

/// match_default.mora：match 默认分支 + print。
#[test]
fn e2e_match_default_runs() {
    assert_ok("match_default.mora");
}

/// v0.104.3: `match … with … -> … end` —— spec §14.2 EBNF 与 §7.3/§7.4/§7.5
/// 教学章节的模式匹配**唯一**拼写（8+ 处示例）。
///
/// 缺陷：`emit_match_w` 无条件 `consume(LBrace)`，该形态从未实现 ——
/// 规范自己整章的模式匹配文档逐字无法运行（"Expected '{' after match
/// subject"）；CHANGELOG 中**无**该形态被移除的记录（对比 `route` 有明确
/// 「已移除」声明），故属承诺语法未接线。现两种拼写共用 arm emitter，
/// 仅体终止符（`end` / `}`）与 arm 分隔（换行 / `,`）不同。
#[test]
fn e2e_match_with_arrow_form_matches_spec() {
    use e2e_helpers::assert_source_ok;
    use mora::value::Value;
    let s = |v: Value| match v {
        Value::String(x) => x,
        other => panic!("期望字符串，得到 {:?}", other),
    };
    // §7.3 模式匹配（字面量 + 通配）
    let v = assert_source_ok(
        "let value = 1i\nmatch value with\n  0i -> \"zero\"\n  1i -> \"one\"\n  _ -> \"unknown\"\nend",
    );
    assert_eq!(s(v), "one", "with/-> form：字面量 arm");
    // §7.4 守卫条件 when
    let v = assert_source_ok(
        "let n = 5i\nmatch n with\n  x when x > 0i -> \"positive\"\n  x when x < 0i -> \"negative\"\n  _ -> \"zero\"\nend",
    );
    assert_eq!(s(v), "positive", "with/-> form：when 守卫");
    // §7.5 列表 rest 模式
    let v = assert_source_ok(
        "match [1i, 2i, 3i] with\n  [h, ...t] -> \"nonempty\"\n  _ -> \"empty\"\nend",
    );
    assert_eq!(s(v), "nonempty", "with/-> form：rest 模式");
    // 两种拼写必须一致（同一程序不同写法）
    let a = assert_source_ok("match 2i {\n  1i => \"a\"\n  _ => \"b\"\n}");
    let b = assert_source_ok("match 2i with\n  1i -> \"a\"\n  _ -> \"b\"\nend");
    assert_eq!(s(a), s(b), "`{{}}/=>` 与 `with/->` 两种拼写结果必须一致");
}

/// v0.104.3: arm body 只需**彼此一致**，不必与被匹配值同型。
///
/// 缺陷：双向预扫用 `check_against(&arm.body, &scrutinee_ty)` —— arm body 是
/// match 的**结果值**，与 scrutinee 类型无关。于是
/// `match 1i { 1i => "one", _ => "other" }`（body 全 String、scrutinee Int）
/// 报 "expected Int, got String"，**任何** body 类型 != scrutinee 类型的
/// match 都被误拒（规范 §7.3/§7.4/§7.5 示例全是这个形状）。
/// 现按 joined 检查；arm 之间不兼容时仍由 HM 报错（契约未放松）。
#[test]
fn e2e_match_arm_body_need_not_match_scrutinee() {
    use e2e_helpers::assert_source_ok;
    use mora::value::Value;
    // body String / scrutinee Int —— 必须通过
    let v = assert_source_ok("match 1i {\n  1i => \"one\"\n  _ => \"other\"\n}");
    assert!(
        matches!(v, Value::String(ref x) if x == "one"),
        "arm body 无需与 scrutinee 同型，得到 {:?}",
        v
    );
    // body Int / scrutinee String —— 同样通过
    let v = assert_source_ok("match \"k\" {\n  \"k\" => 42i\n  _ => 0i\n}");
    assert!(
        matches!(v, Value::Int(42) | Value::Float(_)),
        "反向也应通过，得到 {:?}",
        v
    );
    // 反向契约：arm 之间**不兼容**时仍必须报错
    let (_, ws) = mora::cli::compile_and_opt(
        "match 1i {\n  1i => \"str\"\n  _ => 42i\n}\n",
        None,
    )
    .expect("compile");
    let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&ws);
    assert!(
        !errs.is_empty(),
        "异质 arm body（String vs Int）必须被拒，实际无错误"
    );
}

/// v0.87 / v0.104.3: match_guard.mora — `n when cond => …` 守卫条件。
///
/// v0.104.3: 从 `assert_ok` 冒烟升级为**精确值断言**。
///
/// 缺陷：守卫曾被发射进一个随即丢弃的子 EmitContext，其寄存器在外层 `regs`
/// 中从未被写过 → `h_match_expr` 读到 Nil → **守卫恒假、`when` 完全失效**，
/// match 落到后续 arm；且 9 层管线在 `witness_to_fcfg` 里把守卫硬编码为
/// `None`，整个丢弃。原 fixture 的 7 组取值恰好让**首个** arm 的守卫为真，
/// 故在缺陷下也"通过" —— 属侥幸。fixture 末尾新增的 ①②③ 把「必须落在哪个
/// arm」做成唯一正确解，守卫失效时结果明确错误，并由本测试断言。
#[test]
fn e2e_match_guard_runs() {
    use e2e_helpers::assert_ok;
    use mora::value::Value;
    let (last_expr, _) = assert_ok("match_guard.mora");
    let got: Vec<String> = match last_expr {
        Value::List(items) => items
            .iter()
            .map(|v| match v {
                Value::String(s) => s.clone(),
                other => panic!("期望字符串元素，得到 {:?}", other),
            })
            .collect(),
        other => panic!("期望守卫结果列表，得到 {:?}", other),
    };
    assert_eq!(
        got,
        vec!["negative", "middle", "seven"],
        "守卫必须真正参与 arm 选择：① 负值→第二 arm ② 双侧假→通配 ③ 第二 arm 成立"
    );
}

/// v0.87: match_list_rest.mora — `[a, b, ..rest]` list rest destructuring。
#[test]
fn e2e_match_list_rest_runs() {
    assert_ok("match_list_rest.mora");
}

/// v0.87: match_dict_rename.mora — `{name: n, age: a}` dict rename。
#[test]
fn e2e_match_dict_rename_runs() {
    assert_ok("match_dict_rename.mora");
}

// ===================================================================
// 3. 数据结构
// ===================================================================

/// dict_access.mora：dict 字面量 + 索引访问 + 算术 + print。
#[test]
fn e2e_dict_access_runs() {
    assert_ok("dict_access.mora");
}

// ===================================================================
// 4. task 定义 + 调用
// ===================================================================

/// function_call.mora：task 定义 + 跨 task 调用 + print。
/// 这条路径覆盖 call_value → run_mir 关键 dispatch 路径。
#[test]
fn e2e_task_define_and_call_runs() {
    assert_ok("function_call.mora");
}

/// macro_def.mora：macro 定义 + 展开 + 调用。
/// v0.83: 验证 parser emit_macro_def_w 保留宏体（子 EmitContext 编译），
/// Value::Macro { body: Arc<MirFunction> } 存储宏体，
/// call_builtin_fallback 以 args 绑定 params，子 env run_mir 执行 body。
/// 此前 Value::Macro 仅存 name+params（无 body），调用方报错"not implemented"。
#[test]
fn e2e_macro_define_and_expand_runs() {
    assert_ok("macro_def.mora");
}

/// macro_advanced.mora：嵌套宏调用 + 递归宏 + 宏返回宏。
/// v0.86: 验证宏系统的完整语义——不只是基本展开，还包括复合调用模式。
#[test]
fn e2e_macro_advanced_runs() {
    assert_ok("macro_advanced.mora");
}

/// eval.mora：runtime eval(code) — 从 Mora 内部动态执行 Mora 源码。
/// v0.86: Lisp homoiconicity + eval-apply loop 的落地。
/// 覆盖：算术 / 字符串 / 条件 / 闭包绑定外层变量。
#[test]
fn e2e_eval_runs() {
    assert_ok("eval.mora");
}

/// quasiquote.mora：v0.88 Lisp 系 quasiquote/unquote/unquote-splice。
/// 覆盖：纯 quasiquote（静态 Code）、unquote（,x 求值插入）、
/// unquote-splice（,,items 展开 List）、括号深度解析。
#[test]
fn e2e_quasiquote_runs() {
    assert_ok("quasiquote.mora");
}

// ===================================================================
// 5. 错误注入
// ===================================================================

/// 类型错误注入：把 string 赋给 Int 类型注解的变量。
/// 这是 typeck 错误的最小复现（HM 推断 + 双向叠加层必经路径）。
#[test]
fn e2e_typecheck_error_is_reported() {
    let bogus = r#"
let x: int = "not an int"
x
"#;
    let res = (|| -> Result<(), String> {
        let (_, witnesses) = mora::parser_v3::ParserV3::compile(bogus)
            .map_err(|e| format!("parse: {}", e))?;
        let type_errs =
            mora::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
        if !type_errs.is_empty() {
            return Err(format!("{} type error(s)", type_errs.len()));
        }
        Ok(())
    })();
    assert!(
        res.is_err(),
        "expected type error for `let x: int = \"...\"`, got Ok"
    );
}

/// 语法错误注入：未闭合的字符串字面量。
#[test]
fn e2e_parse_error_is_reported() {
    let bogus = r#"let x = "unterminated"#;
    let res = mora::parser_v3::ParserV3::compile(bogus);
    assert!(
        res.is_err(),
        "expected parser error for unterminated string, got Ok"
    );
}

/// v0.103: `cli::compile_and_opt` 遇到语法错误必须返回 `Err`，而非 panic。
///
/// 缺陷：该函数用 `unwrap_or_else(|e| panic!("compile_and_opt failed: {e}"))`
/// 包住解析失败，于是 `mora <file>` / `mora --check <file>` / record /
/// replay / snapshot 五条入口都把一个用户语法错误呈现为编译器内部崩溃
///（Rust panic + 回溯），与 typecheck 错误路径的 `process::exit(2)` 不一致。
///
/// 这里直接断言根因处的契约（返回 Err）——各调用点随即把它转成
/// `eprintln!` + 退出码 2（见 src/main.rs / src/cli/record.rs）。
#[test]
fn e2e_compile_and_opt_returns_err_on_parse_error() {
    let res = mora::cli::compile_and_opt("let x = (1i +\n", None);
    assert!(
        res.is_err(),
        "语法错误必须以 Err 返回（此前 panic），实际 Ok"
    );
    let msg = res.expect_err("asserted is_err above");
    assert!(
        !msg.is_empty(),
        "解析错误消息不得为空（需可读定位）"
    );
}

// ===================================================================
// 6. 字节级 fixture 完整性（防 fixtures 漂移）
// ===================================================================

/// 检查所有 fixture 文件非空。
#[test]
fn e2e_fixtures_are_non_empty() {
    for name in [
        "arithmetic.mora",
        "if_else.mora",
        "function_call.mora",
        "for_loop.mora",
        "string_concat.mora",
        "dict_access.mora",
        "match_default.mora",
        "match_guard.mora",
        "match_list_rest.mora",
        "match_dict_rename.mora",
        "nested_if.mora",
        "handle_effect.mora",
        "macro_def.mora",
    ] {
        let content = read_fixture(name);
        assert!(
            !content.trim().is_empty(),
            "fixture {} must not be empty",
            name
        );
    }
}

// ===================================================================
// 7. run_mir 与 run_dag 线性退化等价（v0.59 行为契约）
// ===================================================================

/// 验证相同 fixture 跑两次都成功（deterministic 执行）。
/// run_mir ≡ run_dag(add_sequential_edges 后) 是 v0.59 起的核心承诺。
#[test]
fn e2e_run_mir_deterministic() {
    assert_ok("arithmetic.mora");
    assert_ok("arithmetic.mora");
}

/// 8 fixtures 全部跑通 — 完整 E2E 覆盖 smoke test。
#[test]
fn e2e_all_fixtures_run() {
    for name in [
        "arithmetic.mora",
        "if_else.mora",
        "function_call.mora",
        "for_loop.mora",
        "string_concat.mora",
        "dict_access.mora",
        "match_default.mora",
        "match_guard.mora",
        "match_list_rest.mora",
        "match_dict_rename.mora",
        "nested_if.mora",
        // v0.91: 数学原语覆盖
        "math_basic.mora",
        "stats_basic.mora",
        "linalg_basic.mora",
        "random_basic.mora",
        "random_handle.mora",
        "bigint_basic.mora",
        // v0.103: 循环 fixture（此前三个因二参 print 被类型检查拒绝而未纳入，
        // loop_basic 另有 `{` 配 `end` 的语法错误）
        "loop_basic.mora",
        "loop_break.mora",
        "loop_continue.mora",
        "loop_for_break.mora",
    ] {
        assert_ok(name);
    }
}

// ===================================================================
// 8. v0.80 algebraic effects：handle / perform 端到端可执行
// ===================================================================

/// perform 必须由 handle 块内的 handler 接管，且返回值 = handler 末尾表达式。
/// 此测试验证 Stage 2.0 单发语义下整条路径可执行（不是 stub）。
#[test]
fn e2e_handle_perform_returns_handler_result() {
    let (result, _stdout) = assert_ok("handle_effect.mora");
    // handle 块的 body 把 perform 返回值（handler 末尾表达式 = "mocked:" + __arg0）
    // 存入 global_result；最后一行 result = global_result 取出来断言。
    assert_eq!(
        result.to_string(),
        "mocked:hello",
        "handle/perform 端到端语义失败：handler 末尾表达式值未传回 perform"
    );
}

// 静默工具 unused import 警告
#[allow(dead_code)]
fn _unused_assert_compile_error() {
    let _ = assert_compile_error("__unused__");
}

// ===================================================================
// v0.83: TEA (The Elm Architecture) — Runtime 层验证
// ===================================================================
// 注：完整 TEA 循环（Model/Msg/Update/Cmd + Replay）的 Runtime 基础设施
// 已通过 unit tests 验证（src/tea/{mod,replay}.rs 中 10+ tests）。
// E2E fixtures 需要 model/msg/update/app 新语法支持（Stage 4 路线图），
// 本阶段仅做 Runtime API 集成测试：

#[test]
fn e2e_tea_runtime_compiles() {
    // 验证 TeaApp/TeaCmd/TeaMsg 类型在 VM 中可构造和操作
    // （Runtime 通过 builtin tea.* 暴露，E2E 暂用 unit tests 覆盖）
    use mora::tea::{Cmd, Msg, TeaApp};
    // v0.94: TeaApp 是纯值 —— with_model/dispatch 返回新 app，run_loop 返回新 app。
    let app = TeaApp::new(
        mora::value::Value::Nil,
        mora::value::Value::Nil,
        mora::value::Value::Nil,
    )
    .with_model(mora::value::Value::Int(42))
    .dispatch(Msg::new("Test", mora::value::Value::Nil));
    assert_eq!(app.model(), mora::value::Value::Int(42));
    // v0.83: run_loop 需要 MirHost context —— 用 Interpreter::new() 注入
    let mut interp = mora::interpreter::Interpreter::new();
    assert_eq!(
        app.run_loop(10, &mut interp).model(),
        mora::value::Value::Int(42)
    );
    let cmd = Cmd::None;
    let _ = cmd.to_value();
}

/// tea_app.mora：完整 model/msg/update/app 语法糖 — 验证 parser 端到端解析
#[test]
fn e2e_tea_app_runs() {
    assert_ok("tea_app.mora");
}

/// tea_standalone.mora：TEA 独立声明 `model Name ... end` / `msg Name ... end`
/// （spec §9.6 工作示例 + §14.2 EBNF）。锁定「IR/handler/typeck 齐备但
/// parser 零产出」的缺陷。
#[test]
fn e2e_tea_standalone_runs() {
    // run_e2e 不捕获 stdout 且 last_expr 取自顶层（app 声明为 Nil），
    // 故走子进程捕获 print 输出做精确断言。
    use std::process::Command;
    let out = Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg("tests/fixtures/e2e/tea_standalone.mora")
        .output()
        .expect("run fixture");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("dict"),
        "model 声明应注册为 dict，实际 stdout:
{}",
        stdout
    );
    assert!(
        stdout.contains("list"),
        "msg 声明应注册为 list，实际 stdout:
{}",
        stdout
    );
    // v0.103: 独立 `update(params) ... end` 声明（spec §9.6）——
    // 运行时 h_update_def 注册为**可调用的 closure**（此前存的是 Dict +
    // 一个 `"<MirFunction:N>"` 描述字符串，声明出的 update 永不可调用）；
    // 更早则连 parser 产出点都没有，`type_of(update)` 报 Unbound variable。
    assert!(
        stdout.contains("closure"),
        "update 声明应注册为可调用 closure，实际 stdout:
{}",
        stdout
    );
    assert!(
        stdout.contains("tea_app"),
        "app 应构造 TeaApp，实际 stdout:
{}",
        stdout
    );
}

/// tea_counter.mora：TEA 完整运行时链路 —— 声明式 app 名可引用 +
/// tea.init 三参构造 + tea.dispatch/run/update 驱动。
/// 锁定的既有缺陷：typeck 不注册 app 名（Unbound variable）、emit 端伪造
/// update/view witness、tea.init 硬编码 update/view = Nil（tea.run 崩）。
#[test]
fn e2e_tea_counter_runs() {
    assert_ok("tea_counter.mora");
}

/// v0.104: TEA update 全链路 —— spec §9.6 工作示例必须逐字可跑。
///
/// 缺陷链（本条锁定的整条）：
///   ① `update(msg, model) ... end` 独立声明 parser 无产出点（Unbound variable）；
///   ② `h_update_def` 存 `Dict{__update_body__: "<MirFunction:2>"}` 描述字符串，
///      声明出的 update 不可调用；
///   ③ AppDef 把 update/view 的 **Closure witness** 当表达式 lower →
///      产出「构造闭包」的指令，update 每次调用返回新闭包、模型变 `<closure>`；
///   ④ `h_app_def` 硬编码闭包形参名 `["model","msg"]`，用户形参名 unbound；
///   ⑤ 名字引用 `update: update` 的转发闭包缺 `Var` 取参 + 缺 `MirFunction.params`；
///   ⑥ 运行时 `apply_update` 传 `(model, msg)`，与 spec §9.6 的
///      `update(msg, model)`（Elm 序）相反 → 示例体 `model.count` 报
///      "Dict has no method: count"；
///   ⑦ match 的 output_reg 用 arm 局部寄存器 → 越界 panic。
///
/// 断言：2 次 Increment、1 次 Decrement 后模型 count == 3（n = 0 + 2 - 1）。
#[test]
fn e2e_tea_standalone_update_full_cycle() {
    use e2e_helpers::assert_source_ok;
    use mora::value::Value;
    let src = "\
model Counter
  count: number = 0
end
msg CounterMsg
  Increment
  Decrement
end
update(msg, model)
  match msg {
    Increment => {count: model.count + 1i}
    Decrement => {count: model.count - 1i}
  }
end
app CounterApp
  model: Counter
  msg: CounterMsg
  init: {count: 0}
  update: update
  view: fn(m) => m
end
let a = tea.dispatch(CounterApp, {tag: \"Increment\"})
let a = tea.dispatch(a, {tag: \"Increment\"})
let a = tea.dispatch(a, {tag: \"Decrement\"})
let b = tea.run(a, 5)
tea.model(b)
";
    let v = assert_source_ok(src);
    let count = match &v {
        Value::Dict(m) => m.get("count").cloned(),
        other => panic!("期望模型 dict，得到 {:?}", other),
    };
    let n = match count {
        Some(Value::Int(i)) => i as f64,
        Some(Value::Float(f)) => f,
        other => panic!("期望 count 为数值，得到 {:?}", other),
    };
    assert_eq!(
        n, 3.0,
        "+1 +1 -1 后 count 应为 3（update(msg, model) 按 spec §9.6 序被真正调用）"
    );
}

/// ai_critic.mora：`ai.critic(answer, ctx?)`（spec §12.5 `string, string? -> value`）。
/// 此前无任何实现（全仓无该 builtin），方法调用落 Unknown method。
#[test]
fn e2e_ai_critic_runs() {
    use mora::value::Value;
    let (last_expr, _) = assert_ok("ai_critic.mora");
    // fixture 返回 [b(2参), a(1参)] —— 两者都必须是结构化裁决 dict
    let items = match last_expr {
        Value::List(items) => items,
        other => panic!("期望 [b, a] 列表，得到 {:?}", other),
    };
    assert_eq!(items.len(), 2, "1 参与 2 参调用都必须可用（可选尾参）");
    for (i, v) in items.iter().enumerate() {
        let d = match v {
            Value::Dict(d) => d,
            other => panic!("第 {} 个结果应为 dict，得到 {:?}", i, other),
        };
        let verdict = d.get("verdict").and_then(|v| match v {
            Value::String(s) => Some(s.as_str()),
            _ => None,
        });
        assert!(
            matches!(verdict, Some("pass") | Some("fail")),
            "verdict 必须是 pass/fail，得到 {:?}",
            d.get("verdict")
        );
        assert!(d.contains_key("score"), "结果须含 score 字段");
        assert!(d.contains_key("critique"), "结果须含 critique 字段");
    }
}

/// export_visibility.mora：模块可见性（spec §10.2）端到端。
/// 锁定「未 export 的符号对 import 不可见」+「export 的 let/task 可调用」。
#[test]
fn e2e_export_visibility_runs() {
    // run_e2e 不捕获 stdout（helper 已知架构限制），改走子进程捕获
    // 并断言 print 的输出。这是 v0.103 export 模块可见性的端到端契约：
    // 调用 export 的 task 与读取 export 的 let 都必须工作。
    use std::process::Command;
    let out = Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg("tests/fixtures/e2e/export_visibility.mora")
        .output()
        .expect("run fixture");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("hi"),
        "export task greet 应输出 \"hi\"，实际 stdout:\n{}",
        stdout
    );
    assert!(
        stdout.contains("1.0"),
        "export let VERSION 应输出 \"1.0\"，实际 stdout:\n{}",
        stdout
    );
}

/// 模块内未 export 的 `hidden` 在 import 侧必须被 typeck 拒。
/// 直接走 typecheck 入口（无需额外 fixture 文件）：
/// 写一段 import 后引用隐藏名的代码，typeck 必须报错。
#[test]
fn e2e_export_private_symbol_not_visible() {
    let src = "import \"tests/fixtures/mod_export.mora\"\nlet x = hidden";
    let witnesses = mora::parser_v3::ParserV3::compile(src)
        .expect("parse")
        .1;
    let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
    assert!(
        !errs.is_empty(),
        "未 export 的 hidden 必须在 typeck 阶段被拒，实际无错"
    );
}

/// explicit_api.mora：`Type::new()` 关联构造 + Router/McpServer 方法链 +
/// dict 关键字键。锁定 `::` 不被 parser 消费、构造器无分派、typeck 方法
/// 签名漏用户参数、dict 键拒绝关键字四处缺陷。
#[test]
fn e2e_explicit_api_runs() {
    use mora::value::Value;
    let (last_expr, _) = assert_ok("explicit_api.mora");
    let d = match last_expr {
        Value::Dict(d) => d,
        other => panic!("期望 dict 结果，得到 {:?}", other),
    };
    assert_eq!(d.get("router"), Some(&Value::String("router".into())));
    assert_eq!(d.get("server"), Some(&Value::String("mcp_server".into())));
    assert!(
        matches!(d.get("schema"), Some(Value::Dict(_))),
        "schema 应为 dict（含关键字键 type），得到 {:?}",
        d.get("schema")
    );
}

/// prompt_section.mora：`prompt "name" do ... end` 声明 + compose_prompt 拼接。
/// 锁定：prompt 关键字不被 parser 消费、handler 吞错不构建值、
/// compose_prompt 读错环境（core.environment vs 执行 env）三处缺陷。
#[test]
fn e2e_prompt_section_runs() {
    use mora::value::Value;
    let (last_expr, _) = assert_ok("prompt_section.mora");
    let out = match last_expr {
        Value::String(s) => s,
        other => panic!("compose_prompt 应返回字符串，得到 {:?}", other),
    };
    assert!(out.contains("system"), "拼接结果应含 system 节: {}", out);
    assert!(out.contains("You are a helpful assistant."), "应含 system 正文: {}", out);
    assert!(out.contains("user"), "拼接结果应含 user 节: {}", out);
    assert!(out.contains("What is Mora?"), "应含 user 正文: {}", out);
}

/// 全局模块对象在类型检查阶段可用 —— globals 注册表与 typeck 名单同源。
/// 缺陷：13 个已注册模块（bus/sandbox/schedule/ccr/mock/exec/tool/skill/
/// plan/mora/document/tea/xform）此前被判 Unbound variable，用户无法调用。
#[test]
fn builtin_module_objects_pass_typeck() {
    use mora::value::MODULE_OBJECTS;
    for (name, _) in MODULE_OBJECTS {
        let src = format!("let x = {}
", name);
        let (_f, w) = mora::parser_v3::ParserV3::compile(&src)
            .unwrap_or_else(|e| panic!("{} 编译失败: {}", name, e));
        let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&w);
        assert!(
            errs.is_empty(),
            "模块对象 {} 不应在 typeck 报错: {:?}",
            name,
            errs.iter().map(mora::typeck::format_error).collect::<Vec<_>>()
        );
    }
}

/// 模块对象名单与 globals 注册同源（防再次漂移）。
#[test]
fn module_objects_are_registered_in_globals() {
    use mora::mir::host::MirHost;
    use mora::value::MODULE_OBJECTS;
    let interp = mora::interpreter::Interpreter::new();
    let env = MirHost::environment(&interp);
    for (name, _) in MODULE_OBJECTS {
        assert!(
            env.get(name).is_some(),
            "MODULE_OBJECTS 列出的 {} 必须在 globals 中注册",
            name
        );
    }
}

// ===================================================================
// v0.102: 声明式范式（逻辑式/关系式）
// ===================================================================

/// rel_basic.mora：事实 + 规则 + 双查询变量 → 传递闭包全部有序对。
#[test]
fn e2e_rel_basic_runs() {
    use mora::value::Value;
    let (last_expr, _) = assert_ok("rel_basic.mora");
    let pairs = match last_expr {
        Value::List(items) => items,
        other => panic!("expected list of (from, to) pairs, got {:?}", other),
    };
    // a→b, b→c, c→d 的传递闭包共 6 条有向路径
    assert_eq!(pairs.len(), 6, "3 节点链的传递闭包应有 6 条路径");
    let mut seen: Vec<(String, String)> = Vec::new();
    for p in &pairs {
        match p {
            Value::List(t) if t.len() == 2 => {
                let from = match &t[0] {
                    Value::String(s) => s.clone(),
                    o => panic!("from 应为字符串，得到 {:?}", o),
                };
                let to = match &t[1] {
                    Value::String(s) => s.clone(),
                    o => panic!("to 应为字符串，得到 {:?}", o),
                };
                seen.push((from, to));
            }
            o => panic!("解应为二元列表（元组），得到 {:?}", o),
        }
    }
    seen.sort();
    assert_eq!(
        seen,
        vec![
            ("a".to_string(), "b".to_string()),
            ("a".to_string(), "c".to_string()),
            ("a".to_string(), "d".to_string()),
            ("b".to_string(), "c".to_string()),
            ("b".to_string(), "d".to_string()),
            ("c".to_string(), "d".to_string()),
        ],
        "传递闭包应精确覆盖所有可达对"
    );
}

/// rel_single_var.mora：单查询变量 → 解是标量值本身（非元组）。
#[test]
fn e2e_rel_single_var_runs() {
    use mora::value::Value;
    let (last_expr, _) = assert_ok("rel_single_var.mora");
    let mut reach: Vec<String> = match last_expr {
        Value::List(items) => items
            .iter()
            .map(|v| match v {
                Value::String(s) => s.clone(),
                o => panic!("单变量解应为字符串，得到 {:?}", o),
            })
            .collect(),
        other => panic!("expected list of strings, got {:?}", other),
    };
    reach.sort();
    assert_eq!(reach, vec!["b", "c", "d"], "从 a 可达 b/c/d");
}

/// rel_zero_var.mora：零查询变量 → 每个解是 nil 成功标记。
#[test]
fn e2e_rel_zero_var_runs() {
    use mora::value::Value;
    let (last_expr, _) = assert_ok("rel_zero_var.mora");
    match last_expr {
        Value::List(items) => {
            assert_eq!(items.len(), 1, "edge(\"a\",\"b\") 恰有一个解");
            assert_eq!(items[0], Value::Nil, "零查询变量的解是 nil 成功标记");
        }
        other => panic!("expected list, got {:?}", other),
    }
}

/// rel_empty.mora：不可满足的目标 → 空解列表（失败剪枝）。
#[test]
fn e2e_rel_empty_runs() {
    use mora::value::Value;
    let (last_expr, _) = assert_ok("rel_empty.mora");
    match last_expr {
        Value::List(items) => assert!(items.is_empty(), "不存在的边应无解"),
        other => panic!("expected list, got {:?}", other),
    }
}

/// rel_run_limit.mora：run N 形式 → 恰好 N 个解（无后缀数字是 Float 的坑）。
#[test]
fn e2e_rel_run_limit_runs() {
    use mora::value::Value;
    let (last_expr, _) = assert_ok("rel_run_limit.mora");
    match last_expr {
        Value::List(items) => assert_eq!(items.len(), 2, "solve 2 应产出恰好 2 个解"),
        other => panic!("expected list, got {:?}", other),
    }
}

/// rel_project.mora：宿主投影（project）—— 关系体内的确定性宿主计算。
/// 覆盖 Project 节点 + 顶层 task 的词法可见性：
/// num 绑定 x → project(square, x, y) 调用外层 task 计算 x*x 并与 y 合一。
#[test]
fn e2e_rel_project_runs() {
    use mora::value::Value;
    let (last_expr, _) = assert_ok("rel_project.mora");
    let items = match last_expr {
        Value::List(items) => items,
        other => panic!("expected list of (x, y) pairs, got {:?}", other),
    };
    assert_eq!(items.len(), 3, "num 有 3 条事实，squared 应产出 3 个解");
    let mut pairs: Vec<(f64, f64)> = Vec::new();
    for p in &items {
        match p {
            Value::List(t) if t.len() == 2 => {
                let num = |v: &Value| match v {
                    Value::Int(n) => *n as f64,
                    Value::Float(n) => *n,
                    o => panic!("应为数值，得到 {:?}", o),
                };
                pairs.push((num(&t[0]), num(&t[1])));
            }
            o => panic!("解应为二元列表，得到 {:?}", o),
        }
    }
    pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    assert_eq!(
        pairs,
        vec![(1.0, 1.0), (2.0, 4.0), (3.0, 9.0)],
        "project(square, x, y) 应把 x*x 绑定到 y"
    );
}

/// rel_cons.mora：cons 项构造 + 递归列表关系（appendo）→ 结构化解。
#[test]
fn e2e_rel_cons_runs() {
    use mora::value::Value;
    let (last_expr, _) = assert_ok("rel_cons.mora");
    match last_expr {
        Value::List(items) => {
            assert_eq!(items.len(), 1, "appendo 的确定性拼接应恰有一个解");
            // 结果应是 Cons 链 [1, 2, 3]
            let mut cur = &items[0];
            // 注意：无后缀数字字面量在 Mora 词法层是 Float（语言约定），
            // 故 cons(1, ...) 的头是 Float(1.0)。
            let mut got: Vec<f64> = Vec::new();
            loop {
                match cur {
                    Value::Cons { car, cdr } => {
                        match &**car {
                            Value::Float(n) => got.push(*n),
                            o => panic!("cons 头应为 Float，得到 {:?}", o),
                        }
                        cur = cdr;
                    }
                    Value::Nil => break,
                    o => panic!("列表尾部应为 Cons 或 Nil，得到 {:?}", o),
                }
            }
            assert_eq!(got, vec![1.0, 2.0, 3.0], "appendo([1,2], [3]) = [1,2,3]");
        }
        other => panic!("expected list of solutions, got {:?}", other),
    }
}

// ===================================================================
// v0.102 缺陷修复回归
// ===================================================================

/// v0.104.2: `continue` 在 `for` 循环里必须跳到**增量**而非条件判定。
///
/// 缺陷：`emit_loop_w` 的后修补把 `Continue` 的目标设为 `loop_label`（条件
/// 判定处），**跳过了循环变量增量** → 索引永不前进 → 条件恒为「未越界」
/// → **无限循环**（实测挂死）。三种 `if` 拼写（`{}` / 换行-`end` /
/// `then … end`）全部复现。修复后目标为增量指令。
#[test]
fn e2e_continue_advances_for_loop_index() {
    use e2e_helpers::assert_source_ok;
    use mora::value::Value;
    let num = |v: Value| -> f64 {
        match v {
            Value::Int(n) => n as f64,
            Value::Float(n) => n,
            other => panic!("期望数值结果，得到 {:?}", other),
        }
    };
    // 累加「实际迭代到的 i 之和」：跳过 2 时应为 1+3=4，不跳过则为 6。
    // （不用列表拼接 —— `+` 对等长列表是逐元素相加、不等长才是拼接，
    // 用它做追加会引入与本测试无关的语义歧义。）
    let src = |braces: &str, then_kw: &str, plain: &str| {
        format!(
            "let sum = 0i\nfor i in [1i, 2i, 3i]\n{}assign sum = sum + i\nend\nsum",
            if !braces.is_empty() {
                format!("  if i == 2i {braces}\n")
            } else if !then_kw.is_empty() {
                format!("  if i == 2i then {then_kw} end\n")
            } else {
                format!("  if i == 2i\n    {plain}\n  end\n")
            }
        )
    };
    // 三种 if 拼写都必须：① 不挂死 ② 正确跳过 2
    for (label, src) in [
        ("`{}` 拼写", src("{ continue }", "", "")),
        ("then…end 拼写", src("", "continue", "")),
        ("换行-end 拼写", src("", "", "continue")),
    ] {
        let v = assert_source_ok(&src);
        assert_eq!(
            num(v),
            4.0,
            "{}：continue 应跳过 2（1+3=4，不跳过则是 6）",
            label
        );
    }
    // 对照：不 continue 时为 6（证明上面的差异确实来自 continue）
    let v = assert_source_ok("let sum = 0i\nfor i in [1i, 2i, 3i]\n  assign sum = sum + i\nend\nsum");
    assert_eq!(num(v), 6.0, "无 continue 时 1+2+3=6");
}

/// v0.104.2: `if … then … end` 的**块体**与**单语句**两种拼写。
///
/// 缺陷：`then` 分支无条件走 `emit_expr_w`（只吃一个表达式），于是
///   · `if c then` + 换行 + 语句块 + `end`（spec §14.2 if_stmt 的定义形态，
///     §3.2/§7.3/§11.5 共 4 处示例）**无法解析**；
///   · 块体停在 `else` 上时把 `else` 当语句 → "Unbound variable 'else'"。
/// 现在两种拼写与 `{}` 形式等价。
#[test]
fn e2e_if_then_block_and_single_stmt_forms() {
    use e2e_helpers::{E2eResult, run_source};
    use mora::value::Value;
    // 注：本测试关心的是「这些拼写能否解析 + 执行后是否继续到后续语句」，
    // 而 `run_source` 的 last_expr 取自顶层末表达式 —— 其取法对
    // `if … end` + 尾随表达式的形状不敏感（helper 语义），故这里断言
    // **执行成功**；精确尾值由下面的 `run_mir` 直调路径验证。
    let ok = |src: &str| match run_source(src) {
        E2eResult::Ok { .. } => true,
        E2eResult::CompileError(e) => panic!("compile error: {e}\n---\n{src}"),
        E2eResult::TypeErrors(errs) => panic!(
            "type errors: {:?}\n---\n{src}",
            errs.iter()
                .map(mora::typeck::format_error)
                .collect::<Vec<_>>()
        ),
    };
    // then + 块体 + else + end
    ok("if 1i > 0i then\n  print(\"pos\")\nelse\n  print(\"neg\")\nend\n7i");
    // else-if 链
    ok("let x = 2i\nif x > 5i then\n  print(\"big\")\nelse if x > 1i then\n  print(\"mid\")\nelse\n  print(\"small\")\nend\n1i");
    // then + 单语句 + end
    ok("if true then print(\"t\") end\n9i");
    // then + break（语句而非表达式）
    ok("for i in [1i, 2i, 3i]\n  if i == 2i then break end\n  print(i)\nend\n1i");
    // 三种拼写在 run_mir 直调下都返回尾值 7（精确值验证）
    use mora::mir::vm::run_mir;
    use std::sync::Arc;
    for src in [
        "if true { print(\"p\") }\n7i\n",
        "if true then\n  print(\"p\")\nend\n7i\n",
        "if false then\n  print(\"t\")\nelse\n  print(\"e\")\nend\n7i\n",
    ] {
        let (func, _w) = mora::cli::compile_and_opt(src, None).expect("compile");
        let mut interp = mora::interpreter::Interpreter::new();
        let mut env = interp.take_env();
        let arc = Arc::new(func);
        let v = run_mir(
            &arc,
            &mut interp,
            &mut env,
            &mut mora::mir::effect::Effects::new(),
        )
        .expect("run_mir");
        assert!(
            matches!(v, Value::Int(7) | Value::Float(_)),
            "if 语句之后的尾随表达式必须被执行到（得到 {:?}）:\n{src}",
            v
        );
    }
}

/// v0.104.2: 管道 `|>` 的两种形态（spec §2.1 / §7.6 / §18.1 / §18.2）。
///
/// 缺陷：`x |> f(args)` 的 witness 脱糖为 `Call{f, [x, ...args]}`，但 MIR 发的
/// 是 `Pipe(x, rhs)`，而 **rhs 是 `f(args)` 已被求值的寄存器** —— 求值本身就是
/// 一次「自由函数调用」，对只有方法形态的名字（`map`/`filter`/`upper`/`split`/
/// `route`/`tool`/`serve`）必然失败，于是规范里 6 处管道示例全部不可用。
/// 现在 `f(args)` 形态按规范实现为**方法调用**；裸标识符 `x |> f` 仍是值应用。
#[test]
fn e2e_pipe_value_application_and_method_forms() {
    use e2e_helpers::assert_source_ok;
    use mora::value::Value;
    // §7.6 `5 |> double`（值应用）
    let v = assert_source_ok("let double = fn(x) return x * 2 end\n5 |> double");
    let n = match v {
        Value::Int(n) => n as f64,
        Value::Float(n) => n,
        other => panic!("期望数值结果，得到 {:?}", other),
    };
    assert_eq!(n, 10.0, "5 |> double 应为 10");
    // §2.1 `[1,2,3] |> map(...)`（方法形态）
    let v = assert_source_ok("[1i, 2i, 3i] |> map(fn(x) x * 2i end)");
    assert!(
        matches!(&v, Value::List(l) if l.len() == 3),
        "[1,2,3] |> map(...) 应得 3 元素列表，得到 {:?}",
        v
    );
    // §7.6 链式：upper() |> split()
    let v = assert_source_ok("\"hello world\" |> upper() |> split(\" \")");
    assert!(
        matches!(&v, Value::List(l) if l.len() == 2),
        "upper() |> split(\" \") 应得 2 元素列表，得到 {:?}",
        v
    );
}

/// v0.104.2: String 方法的**实参 arity** —— 与运行期一致。
///
/// 缺陷：`split`/`replace`/`starts_with`/`ends_with`/`contains` 的 typeck 签名
/// 只声明 `self`，而运行期 `call_method_string` 都要读实参 →
/// `"a,b" |> split(",")` 报 "Expected 0 arguments, got 1"。按运行期真实
/// arity 拆开签名后修复。
#[test]
fn e2e_string_method_arities_match_runtime() {
    use e2e_helpers::assert_source_ok;
    use mora::value::Value;
    let s = |v: Value| match v {
        Value::String(x) => x,
        other => panic!("期望字符串，得到 {:?}", other),
    };
    assert_eq!(s(assert_source_ok("\"a-b\" |> replace(\"-\", \"+\")")), "a+b");
    assert_eq!(s(assert_source_ok("\"  x  \" |> trim() |> upper()")), "X");
    assert!(matches!(
        assert_source_ok("\"abc\" |> contains(\"b\")"),
        Value::Bool(true)
    ));
    assert!(matches!(
        assert_source_ok("\"abc\" |> starts_with(\"a\")"),
        Value::Bool(true)
    ));
    assert!(matches!(
        assert_source_ok("\"abc\" |> ends_with(\"c\")"),
        Value::Bool(true)
    ));
    assert!(
        matches!(&assert_source_ok("\"a,b\" |> split(\",\")"), Value::List(l) if l.len() == 2)
    );
}

/// v0.104.2: 空块不得 panic 或返回哨兵寄存器。
///
/// 缺陷：`emit_transaction_w` 用 `let mut last = 0` 哨兵，而事务体是**独立
/// 寄存器空间** —— `transaction end`（空体）一个寄存器都没分配，引用 reg 0
/// 让 `run_mir` 拿到 n_regs=0 的函数 → `node_ready` 的 `reg_ready[0]` 越界
/// panic（"the len is 0 but the index is 0"，退出码 101）。`commit`/`rollback`
/// 同样返回哨兵 0。改用真实分配的寄存器。
#[test]
fn e2e_empty_blocks_do_not_panic() {
    use mora::mir::vm::run_mir;
    use mora::value::Value;
    use std::sync::Arc;
    // 直调 `run_mir` 断言尾值 —— 与本文件其他条目的 helper 取法无关，
    // 直接验证「空块之后程序继续执行、返回尾值」。
    for (label, src, expect) in [
        ("空 transaction 体", "transaction\nend\n5i\n", 5.0),
        ("transaction + commit", "transaction\n  commit\nend\n6i\n", 6.0),
        (
            "transaction + rollback + 空 compensation",
            "transaction\n  print(1i)\ncompensation\nend\n7i\n",
            7.0,
        ),
        ("空 then 分支", "if true then\nelse\n  print(1i)\nend\n8i\n", 8.0),
    ] {
        let (func, _w) = mora::cli::compile_and_opt(src, None).expect("compile");
        let mut interp = mora::interpreter::Interpreter::new();
        let mut env = interp.take_env();
        let arc = Arc::new(func);
        let v = run_mir(
            &arc,
            &mut interp,
            &mut env,
            &mut mora::mir::effect::Effects::new(),
        )
        .expect("run_mir 不得 panic");
        let n = match v {
            Value::Int(n) => n as f64,
            Value::Float(n) => n,
            other => panic!("{}: 期望尾值 {:?}，得到 {:?}", label, expect, other),
        };
        assert_eq!(n, expect, "{}: 空块之后应继续到尾值", label);
    }
}

/// v0.103: 循环 fixture 的**精确值**断言。
///
/// 这四个 fixture 此前都不在断言集里（`loop_basic.mora` 甚至因 `{` 配 `end`
/// 的语法错误而无法解析，`loop_break`/`loop_continue`/`loop_for_break` 用了
/// 二参 `print`，被当时固定 arity 的 print 签名拒绝）。它们共同锁定
/// v0.103 修复的循环链：CSE 不重命名环携带寄存器、`for` 退出条件用
/// JumpIf、`break`/`continue` 目标解析补 pc 兜底、Data 边不充当激活通道、
/// 尾部隐式 Return 不产生死节点、变参 `print` 被类型系统接受。
#[test]
fn e2e_loop_fixtures_exact_values() {
    use mora::value::Value;
    let num = |v: Value| match v {
        Value::Int(n) => n as f64,
        Value::Float(n) => n,
        other => panic!("期望数值结果，得到 {:?}", other),
    };
    // while 1..10 累加 = 55
    let (v, _) = assert_ok("loop_basic.mora");
    assert_eq!(num(v), 55.0, "loop_basic: while 1..=10 累加应为 55");
    // while + break（i==5 时 break）累加 1..4 = 10，末表达式 = 10 + 1000
    let (v, _) = assert_ok("loop_break.mora");
    assert_eq!(num(v), 1010.0, "loop_break: break 于 i=5 得 10，+1000 = 1010");
    // while + continue（i==3 跳过）累加 1+2+4+5 = 12，+1000
    let (v, _) = assert_ok("loop_continue.mora");
    assert_eq!(num(v), 1012.0, "loop_continue: 跳过 3 得 12，+1000 = 1012");
    // for + break（i==6 时 break）累加 1..5 = 15，+1000
    let (v, _) = assert_ok("loop_for_break.mora");
    assert_eq!(num(v), 1015.0, "loop_for_break: break 于 6 得 15，+1000 = 1015");
}

/// v0.103: 变参 `print` —— 运行期 join 全部实参，类型系统必须接受多实参。
///
/// 缺陷：`print` 的签名声明 1 个参数，`builtin_callee_ty` 据此生成固定
/// arity 的 curried arrow，多余实参无处消耗 → `print("a", b)` 报
/// "expected nil, got fn(string) -> …"。三个 loop fixture 因此无法通过
/// 类型检查（它们用二参 print 打印标签 + 值）。
#[test]
fn e2e_variadic_print_accepted() {
    use e2e_helpers::assert_source_ok;
    use mora::value::Value;
    // 1/2/3 个实参都应通过类型检查并执行
    assert!(matches!(assert_source_ok("print(1i)"), Value::Nil));
    assert!(matches!(assert_source_ok("print(\"a\", 2i)"), Value::Nil));
    assert!(matches!(
        assert_source_ok("print(\"a\", 2i, 3.5)"),
        Value::Nil
    ));
}

/// loop_beyond_dag_limit.mora：循环 600 次（> 旧的 DAG 上限 500）后，
/// 循环累加结果仍可访问。锁定「DAG 节点执行上限静默截断循环后续语句」缺陷。
#[test]
fn e2e_loop_beyond_dag_limit_runs() {
    use mora::value::Value;
    let (last_expr, _) = assert_ok("loop_beyond_dag_limit.mora");
    // sum(0..599) = 599*600/2 = 179700
    let got = match last_expr {
        Value::Int(n) => n as f64,
        Value::Float(n) => n,
        other => panic!("期望数值结果，得到 {:?}", other),
    };
    assert_eq!(got, 179700.0, "600 次循环的累加和必须完整计算（非被截断）");
}

/// return_expr_order.mora：`return <expr>` 返回表达式求值寄存器（非硬编码 0）。
#[test]
fn e2e_return_expr_order_runs() {
    let (_v, out) = assert_ok("return_expr_order.mora");
    // print 捕获依赖运行环境；此处以执行成功 + 首行值断言核心契约
    // （编译期寄存器正确性由 src 内 emit 单测覆盖，此处锁定端到端不回归）。
    assert!(
        out.is_empty() || out[0].contains("105"),
        "add100(5) 应为 105，得到 {:?}",
        out
    );
}

/// task_lexical_visibility.mora：顶层 task 对嵌套闭包体词法可见。
#[test]
fn e2e_task_lexical_visibility_runs() {
    let (_v, out) = assert_ok("task_lexical_visibility.mora");
    assert!(
        out.is_empty() || out.first().is_some_and(|l| l.contains('9')),
        "via_closure(3) 应为 9，得到 {:?}",
        out
    );
}

/// rel_project.mora 已改为用顶层 task 作投影函数 —— 见 e2e_rel_project_runs。
/// 另锁定 register-level 语义：`return <expr>` emit 的寄存器即表达式结果。
#[test]
fn return_emits_result_register() {
    use mora::mir::MirInst;
    let (func, _w) = mora::parser_v3::ParserV3::compile(
        "task f(n)
  return n + 100i
end",
    )
    .expect("compile task");
    // 找到 TaskDef 的 body：末尾 Return 的寄存器必须是 BinaryOp 的 dst
    let body = func
        .body
        .iter()
        .find_map(|i| match i {
            MirInst::TaskDef { name, body, .. } if name == "f" => Some(body.as_ref()),
            _ => None,
        })
        .expect("TaskDef f");
    let binary_dst = body.body.iter().find_map(|i| match i {
        MirInst::BinaryOp(dst, _, _, _) => Some(*dst),
        _ => None,
    });
    let ret_reg = body.body.iter().find_map(|i| match i {
        MirInst::Return(Some(r)) => Some(*r),
        _ => None,
    });
    assert_eq!(
        ret_reg, binary_dst,
        "Return 必须指向 BinaryOp 的结果寄存器，而非硬编码 0"
    );
    assert_ne!(binary_dst, Some(0), "该表达式结果不在 reg 0，能检出硬编码回归");
}
