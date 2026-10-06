//! typeck 签名表 ↔ 运行期实际行为的**逐条对拍**。
//!
//! v0.104.6 这一族里出了 4 个缺陷，根因都是同一件事：**签名表是运行期事实的
//! 一份手抄副本，而副本会漂**。已修的四个：
//!
//! | 声明 | 实际 | 后果 |
//! |------|------|------|
//! | `len → Float` | `Value::Int` | 拒绝正确标注、接受错误标注（D5） |
//! | `ai.chat → AiResult` | `Value::String` | `ai.chat` 任何用法都过不了检查（D9） |
//! | `print(Union)` 无 Router 等 | `Display` 有专门臂 | `print(router)` 被拒（D11） |
//! | `range → List(Int)` | `List(Float)` | 声明不准确（无可见失败） |
//!
//! 都是**碰巧**撞上的 —— 没有任何机制会主动报出「声明与实际不符」。本文件
//! 把对拍变成常规测试：逐个 builtin 把返回值喂给 `type_of()`，与
//! `builtin_signatures()` 里声明的返回类型逐字比对。
//!
//! 判据说明：
//!   * `type_of()` 只能报**外层构造名**（`List(Int)` → `list`），元素类型
//!     报不出来，故 `range` 那一行只比外层 —— 元素类型另由
//!     `range_element_type_matches_runtime` 单独钉。
//!   * 库内 `run_mir` **绕过 typeck**（`main.rs:409` 才跑检查），所以本文件
//!     比的是「声明」与「运行期值」两侧，运行期一侧用库内路径即可。

use std::sync::Arc;

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use mora::typeck::dispatch::builtin_signatures;

fn run(src: &str) -> String {
    let (func, _w) = ParserV3::compile(src).unwrap_or_else(|e| panic!("compile: {e}"));
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    match run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    ) {
        Ok(v) => format!("{v:?}"),
        Err(e) => format!("ERR: {e}"),
    }
}

/// 取一个表达式的**运行期类型名**。
///
/// 从 `run()` 的 Debug 前缀推导（`Int(3)` → `int`、`McpServer { … }` →
/// `mcp_server`），而不是把表达式包进 `type_of(…)` —— 后者对**多行前置语句**
/// 无效（`type_of(let r = …)` 根本不是合法表达式）。
///
/// 这也正是 `type_of` 自己的分派依据（按 `Value` 变体判），两者同源。
fn runtime_type_of_program(src: &str) -> String {
    let raw = run(src);
    // Debug 形态：`Int(3)` / `List([…])` / `McpServer { … }` / `String("x")`
    let head: String = raw
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    match head.as_str() {
        "Int" => "int",
        "Float" => "float",
        "String" => "string",
        "Bool" => "bool",
        "List" => "list",
        "Dict" => "dict",
        "Char" => "char",
        "Nil" => "nil",
        "BigInt" => "bigint",
        "Router" => "router",
        "McpServer" => "mcp_server",
        "Agent" => "agent",
        other => panic!("无法从 {raw:?} 推出类型名（前缀 {other}）"),
    }
    .to_string()
}

/// 单表达式形态的运行期类型名（`type_of` 路径，用于 builtin 对拍）。
fn runtime_type_of(expr: &str) -> String {
    let raw = run(&format!("type_of({expr})\n"));
    raw.strip_prefix("String(")
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or(&raw)
        .replace('"', "")
        .to_lowercase()
}

/// 取声明类型的**名字**。用 `Type::name()` 而不是 `Debug` 的小写 ——
/// 两者对 `McpServer` 会给出不同结果（`mcpserver` vs `mcp_server`），
/// 因为 `Debug` 只是把变体名机械小写。`Type::name()` 与运行期的
/// `flow::type_name` 才是同一套命名约定。
///
/// `type_of()` 只能报**外层构造名**（`List(Int)` → `list`），元素类型
/// 报不出来，故带参数的写法只取外层。
fn declared_type_string(t: &mora::typeck::Type) -> String {
    let s = t.name().to_lowercase();
    // 泛型写作 `list<float>` / `dict<string, any>`，去掉参数部分只留外层构造名
    s.split_once('<').map(|(h, _)| h.to_string()).unwrap_or(s)
}

/// 每个 builtin 一段可运行的调用。`None` 表示该条目本测试不覆盖
/// （domain prefix 无独立返回语义）。
fn call_for(name: &str) -> Option<&'static str> {
    Some(match name {
        "both" => "both(succeed(), succeed())",
        "either" => "either(succeed(), succeed())",
        "fail" => "fail()",
        "succeed" => "succeed()",
        "project" => "project(fn(x) x end, 1, 1)",
        "merge_with" => "merge_with(\"k\", \"lww\")",
        "print" => "print(1)",
        "range" => "range(0, 3)",
        "len" => "len([1,2])",
        "str" => "str(1)",
        "int" => "int(\"1\")",
        "float" => "float(\"1\")",
        "bool" => "bool(1)",
        // v0.104.6 D80：登记进 `builtin_signatures` 的 `compose` / `partial`
        // （走 `Signature::variadic` 分支，见 dispatch.rs 里的说明）。它们是
        // 无上界变参，这里各给一个最小实参数的对拍调用。
        "compose" => "compose(fn(x) x end)",
        "partial" => "partial(fn(x) x end)",
        "ai.chat" => "ai.chat(\"hi\")",
        "Router::new" => "Router::new()",
        "McpServer::new" => "McpServer::new()",
        // domain prefix 模块，无独立返回值语义
        "math" | "stats" | "linalg" => return None,
        // v0.104.6 D68 补登记的 `compose_prompt` 无法用单表达式对拍：
        // 它按名去环境里查 `Value::PromptSection`，必须先有
        // `prompt "s" do … end` 块，而 `call_for` 只能给一个表达式
        // （`runtime_type_of` 把它包进 `type_of(...)`）。
        // 其返回类型（String）由 `tests/builtin_return_types.rs` 覆盖。
        "compose_prompt" => return None,
        _ => return None,
    })
}

/// 有意不参与对拍的 builtin：domain prefix 模块，没有独立的返回语义
/// （`math.floor(x)` 走方法分派，签名表里那三条 marker 声明只用于前缀解析），
/// 以及 `compose_prompt`（需前置 `prompt … end` 块，非单表达式，见 `call_for`）。
const INTENTIONALLY_SKIPPED: &[&str] = &["math", "stats", "linalg", "compose_prompt"];

#[test]
fn builtin_declared_return_type_matches_runtime() {
    let mut checked = 0usize;
    let mut unexpected: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    for (name, sig) in builtin_signatures() {
        let Some(call) = call_for(&name) else {
            if !INTENTIONALLY_SKIPPED.contains(&name.as_str()) {
                unexpected.push(name.clone());
            }
            continue;
        };
        checked += 1;
        let declared = declared_type_string(&sig.return_type);
        let actual = runtime_type_of(call);
        if declared != actual {
            failures.push(format!(
                "  [{name}] 声明 `{declared}`，运行期实测 `{actual}`\n    调用：{call}"
            ));
        }
    }

    // 防止将来新增 builtin 却忘了给本测试补调用 —— 那会退化成「静默漏检」。
    assert!(
        checked >= 16,
        "只覆盖了 {checked} 个 builtin，签名表可能有新增却没纳入对拍"
    );
    assert!(
        unexpected.is_empty(),
        "以下 builtin 既无对拍调用、也不在 INTENTIONALLY_SKIPPED 里 —— \
         新增条目请补进 `call_for`，否则本测试会静默漏检：{unexpected:?}"
    );
    assert!(
        failures.is_empty(),
        "{} 个 builtin 的声明返回类型与运行期不符 —— 签名表漂了：\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// `range` 的元素类型 `type_of` 报不出来，单独钉。
///
/// 修前声明 `List(Int)` 而运行期推 `Value::Float`（`call_builtin_range`
/// 里的 `items.push(Value::Float(i as f64))`）。当时**构造不出用户可见失败**
/// —— Int/Float 在本语言 typeck 里可互换 —— 所以这是声明不准确而非功能缺陷；
/// 改正它是为了让签名表与运行期一致。
#[test]
fn range_element_type_matches_runtime() {
    let sig = builtin_signatures()
        .into_iter()
        .find(|(n, _)| n == "range")
        .expect("range 应在签名表里")
        .1;
    let list_ty = match &sig.return_type {
        mora::typeck::Type::List(inner) => inner.name().to_lowercase(),
        other => panic!("range 应返回 List，实得 {other:?}"),
    };
    assert_eq!(
        list_ty, "float",
        "range 元素应为 Float（运行期推 Value::Float）"
    );
    // 运行期确认
    assert_eq!(
        run("range(0, 3)\n"),
        "List([Float(0.0), Float(1.0), Float(2.0)])"
    );
    assert_eq!(runtime_type_of("range(0, 3)[0]"), "float");
}

/// `len` 的返回类型与 `int()` 的返回类型必须是**同一个**。
///
/// 修前 `len` 声明 `Float` 而运行期返 `Int`（D5），`int()` 声明 `Int` 也确实
/// 返 `Int` —— 于是 `int(len(x))` 这种再正常不过的组合被拒。
#[test]
fn len_and_int_agree_on_return_type() {
    let tbl: Vec<_> = builtin_signatures();
    let declared = |n: &str| {
        tbl.iter()
            .find(|(name, _)| name == n)
            .map(|(_, s)| declared_type_string(&s.return_type))
            .unwrap_or_else(|| panic!("{n} 应在签名表里"))
    };
    assert_eq!(
        declared("len"),
        "int",
        "len 应声明为 Int（运行期返 Value::Int）"
    );
    assert_eq!(declared("int"), "int");
    assert_eq!(
        declared("len"),
        declared("int"),
        "len 与 int 都产出整数，返回类型必须一致"
    );
    assert_eq!(run("type_of(len([1,2]))\n"), "String(\"int\")");
    assert_eq!(run("type_of(int(\"2\"))\n"), "String(\"int\")");
}

/// `ai.chat` 的返回类型必须是 `String`（修前是 `AiResult`，那个类型在 `Value`
/// 里根本没有对应变体，导致返回值谁也消费不了 —— D9）。
#[test]
fn ai_chat_returns_string_not_phantom_type() {
    let sig = builtin_signatures()
        .into_iter()
        .find(|(n, _)| n == "ai.chat")
        .expect("ai.chat 应在签名表里")
        .1;
    assert_eq!(
        declared_type_string(&sig.return_type),
        "string",
        "ai.chat 返回 Value::String，声明必须随之"
    );
    assert_eq!(runtime_type_of("ai.chat(\"hi\")"), "string");
}

/// `print` 的形参 Union 必须覆盖**运行期 Display 有专门实现**的类型 ——
/// 修前只列 9 个原始类型，`print(router)` / `print(mcp_server)` 被拒（D11）。
///
/// 这条用真实 CLI 判（库内路径绕过 typeck）。
#[test]
fn print_param_union_covers_every_displayable_value_type() {
    use std::process::Command;
    let cases: &[(&str, &str)] = &[
        ("Router", "print(Router::new())\n"),
        ("McpServer", "print(McpServer::new())\n"),
        ("Agent", "let a = agent.create(\"x\", {})\nprint(a)\n"),
        ("Int", "print(int(\"1\"))\n"),
        ("Float", "print(1.5)\n"),
        ("BigInt", "print(2n)\n"),
        ("List", "print([1])\n"),
        ("Dict", "print({k: 1})\n"),
        ("String", "print(\"a\")\n"),
        ("Bool", "print(true)\n"),
    ];
    let mut failures = Vec::new();
    for (name, src) in cases {
        let path = std::env::temp_dir().join(format!("mora_print_{name}.mora"));
        std::fs::write(&path, src).expect("write temp .mora");
        let out = Command::new(env!("CARGO_BIN_EXE_mora"))
            .arg(&path)
            .output()
            .expect("run temp .mora");
        let _ = std::fs::remove_file(&path);
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if !out.status.success() {
            failures.push(format!(
                "  [{name}] exit={:?}\n{combined}",
                out.status.code()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "print 拒绝了运行期能显示的值（`value/display.rs` 都有专门 Display 臂）：\n{}",
        failures.join("\n")
    );
}

// ─────────────────────────────────────────────────────────────────────
// 方法签名表（`method_signature_builtin`）同样要逐条对拍
// ─────────────────────────────────────────────────────────────────────

/// 方法侧的表是 `(receiver, method)` → `Signature`，按 receiver 分支散落。
/// 本文件只对拍**运行期能构造出 receiver** 的那些（List / Dict / String /
/// Router / McpServer）。`Conversation` / `AiConfig` / `HttpRequest` 的
/// receiver 全仓无构造点，属不可达，另由
/// `phantom_types_are_not_writable_as_annotations` 守着。
///
/// 期望类型写成**字面量**而不是「查表再比」—— 那样这张表就成了自己跟自己比，
/// 查表那侧若漂了，测试跟着一起漂，检不出问题。这里是独立抄的一份期望值。
///
/// 每条给的是 `(receiver, method, 声明期望, 运行期期望, 前置语句, 求值表达式)`：
/// 前置语句用于把 receiver 绑成变量（多行表达式没法直接塞进 `type_of(…)`），
/// 求值表达式是**末表达式**（`run_mir` 取它的值）。
const METHOD_CASES: &[(&str, &str, &str, &str, &str, &str)] = &[
    // (recv, method, 声明期望, 运行期期望, setup, expr)
    ("List", "map", "list", "list", "", "[1].map(fn(x) x end)"),
    (
        "List",
        "filter",
        "list",
        "list",
        "",
        "[1].filter(fn(x) true end)",
    ),
    ("List", "push", "list", "list", "", "[1].push(2)"),
    ("List", "pop", "float", "float", "", "[1].pop()"),
    ("List", "get", "float", "float", "", "[1].get(0)"),
    ("List", "len", "int", "int", "", "[1,2].len()"),
    // `Dict.get` 声明 `Union<value, Nil>` —— 键不存在时运行期确实返 Nil，
    // 声明比「非空」更准，故声明侧期望写 `float | nil`，运行期取存在的键。
    (
        "Dict",
        "get",
        "float | nil",
        "float",
        "",
        "{a: 1}.get(\"a\")",
    ),
    ("Dict", "set", "dict", "dict", "", "{a: 1}.set(\"b\", 2)"),
    ("Dict", "keys", "list", "list", "", "{a: 1}.keys()"),
    ("Dict", "values", "list", "list", "", "{a: 1}.values()"),
    ("Dict", "len", "int", "int", "", "{a: 1}.len()"),
    ("String", "len", "int", "int", "", "\"ab\".len()"),
    ("String", "upper", "string", "string", "", "\"ab\".upper()"),
    ("String", "lower", "string", "string", "", "\"AB\".lower()"),
    ("String", "trim", "string", "string", "", "\" a \".trim()"),
    (
        "String",
        "replace",
        "string",
        "string",
        "",
        "\"ab\".replace(\"a\", \"z\")",
    ),
    (
        "String",
        "starts_with",
        "bool",
        "bool",
        "",
        "\"ab\".starts_with(\"a\")",
    ),
    (
        "String",
        "ends_with",
        "bool",
        "bool",
        "",
        "\"ab\".ends_with(\"b\")",
    ),
    (
        "String",
        "contains",
        "bool",
        "bool",
        "",
        "\"ab\".contains(\"a\")",
    ),
    (
        "String",
        "split",
        "list",
        "list",
        "",
        "\"a,b\".split(\",\")",
    ),
    (
        "Router",
        "route",
        "router",
        "router",
        "let r = Router::new()\nlet h = fn(x) x end\n",
        "r.route(\"GET\", \"/x\", h)",
    ),
    (
        "McpServer",
        "tool",
        "mcp_server",
        "mcp_server",
        "let m = McpServer::new()\n",
        "m.tool(\"t\", {a: 1}, fn(x) x end)",
    ),
];

/// 拼出可交给运行期的完整程序：先跑 setup，再求值末表达式。
fn method_value_expr(setup: &str, expr: &str) -> String {
    format!("{setup}{expr}\n")
}

#[test]
fn method_runtime_return_types_match_expectations() {
    let mut failures = Vec::new();
    for (recv, method, _declared_want, runtime_want, setup, expr) in METHOD_CASES {
        let full = method_value_expr(setup, expr);
        let got = runtime_type_of_program(&full);
        if got != *runtime_want {
            failures.push(format!(
                "  [{recv}.{method}] 期望 `{runtime_want}`，实测 `{got}`\n    调用：{expr}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} 个方法的**实际**返回类型与本文件抄录的期望不符：\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// 与「抄一份期望值」配对的另一半：声明侧也得对得上。
/// 两边任一漂移都会被抓到 —— 期望值是人抄的，声明是代码里的。
#[test]
fn method_declared_return_types_match_expectations() {
    use mora::typeck::Type;
    use mora::typeck::dispatch::method_signature;

    let recv_ty = |r: &str| -> Type {
        match r {
            "List" => Type::List(Box::new(Type::Float)),
            "Dict" => Type::Dict(Box::new(Type::String), Box::new(Type::Float)),
            "String" => Type::String,
            "Router" => Type::Router,
            "McpServer" => Type::McpServer,
            other => panic!("未知 receiver {other}"),
        }
    };

    let mut failures = Vec::new();
    for (recv, method, declared_want, _runtime_want, _setup, _expr) in METHOD_CASES {
        let Some(sig) = method_signature(&recv_ty(recv), method) else {
            failures.push(format!("  [{recv}.{method}] 签名表里没有条目"));
            continue;
        };
        let declared = declared_type_string(&sig.return_type);
        if declared != *declared_want {
            failures.push(format!(
                "  [{recv}.{method}] 声明 `{declared}`，本文件期望 `{declared_want}`"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} 个方法的**声明**返回类型与本文件抄录的期望不符 —— 签名表漂了：\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// arity：签名里声明的用户实参数必须等于实际能接受的。
///
/// 历史上真出过问题（v0.104.2 注释：`"a,b" |> split(",")` 因 arity 报
/// "Expected 0 arguments, got 1"；v0.103 Router.route 三参被误报零参）。
#[test]
fn method_declared_arity_matches_runtime() {
    use mora::typeck::Type;
    use mora::typeck::dispatch::method_signature;

    // (receiver, method, 声明的用户参���数, 实调用里实参数)
    let cases: &[(&str, &str, usize, &str, &str)] = &[
        // (receiver, method, 期望的用户实参数, setup, expr)
        ("List", "map", 1, "", "[1].map(fn(x) x end)"),
        ("List", "filter", 1, "", "[1].filter(fn(x) true end)"),
        ("List", "push", 1, "", "[1].push(2)"),
        ("List", "pop", 0, "", "[1].pop()"),
        ("List", "get", 1, "", "[1].get(0)"),
        ("String", "upper", 0, "", "\"a\".upper()"),
        ("String", "replace", 2, "", "\"ab\".replace(\"a\", \"b\")"),
        ("String", "split", 1, "", "\"a,b\".split(\",\")"),
        ("String", "starts_with", 1, "", "\"ab\".starts_with(\"a\")"),
        ("Dict", "get", 1, "", "{a: 1}.get(\"a\")"),
        ("Dict", "set", 2, "", "{a: 1}.set(\"b\", 2)"),
        (
            "Router",
            "route",
            3,
            "let r = Router::new()\nlet h = fn(x) x end\n",
            "r.route(\"GET\", \"/x\", h)",
        ),
        (
            "McpServer",
            "tool",
            3,
            "let m = McpServer::new()\n",
            "m.tool(\"t\", {a: 1}, fn(x) x end)",
        ),
    ];

    let recv_ty = |r: &str| -> Type {
        match r {
            "List" => Type::List(Box::new(Type::Float)),
            "Dict" => Type::Dict(Box::new(Type::String), Box::new(Type::Float)),
            "String" => Type::String,
            "Router" => Type::Router,
            "McpServer" => Type::McpServer,
            other => panic!("未知 receiver {other}"),
        }
    };

    let mut failures = Vec::new();
    for (recv, method, want_arity, setup, expr) in cases {
        let Some(sig) = method_signature(&recv_ty(recv), method) else {
            failures.push(format!("  [{recv}.{method}] 签名表里没有条目"));
            continue;
        };
        // 签名里第一个形参是 receiver，不计入用户实参
        let declared_user = sig.params.len().saturating_sub(1);
        if declared_user != *want_arity {
            failures.push(format!(
                "  [{recv}.{method}] 声明用户实参 {declared_user} 个，期望 {want_arity} 个"
            ));
        }
        // 实调用不应报错（arity 错的话 typeck 会先拒）
        let got = run(&format!("{setup}{expr}\n"));
        if got.starts_with("ERR:") || got.starts_with("COMPILE-ERR:") {
            failures.push(format!("  [{recv}.{method}] 实调用失败：{got}"));
        }
    }
    assert!(
        failures.is_empty(),
        "方法 arity 与实际不符：\n{}",
        failures.join("\n")
    );
}
