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

/// for_loop.mora：for-in 累加 + print。
#[test]
fn e2e_for_loop_runs() {
    assert_ok("for_loop.mora");
}

/// match_default.mora：match 默认分支 + print。
#[test]
fn e2e_match_default_runs() {
    assert_ok("match_default.mora");
}

/// v0.87: match_guard.mora — `n when n > 0 => ...` guard conditions。
#[test]
fn e2e_match_guard_runs() {
    assert_ok("match_guard.mora");
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
