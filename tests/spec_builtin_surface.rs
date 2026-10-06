//! v0.104.6 D60：spec 逐字承诺的 **41 个 `ns.func(...)`** 点号内建里，
//! **39 个已实现、2 个不存在**。
//!
//! ## 本文件检验的是「**名称表面**」而不是「跑通」
//!
//! 断言的是：**该名字能被解析到一个已知方法**。不要求执行成功 ——
//! `web.fetch` 会因无网络而报 network error、`file.read_text` 会因文件不存在
//! 而报错，这些都**不是**表面问题。判据只排除四类「名字没解析上」的失败：
//! `Unknown method` / `Unknown function` / `Unbound variable '<ns>'` /
//! `'<ns>' is a module, not a function`。
//!
//! 早先一版用「跑通」做断言，误报了 11 个 —— 全部是**参数给错或环境所致**
//! （`tea.*` 要 TeaApp、`web.fetch` 要网络），不是缺陷。**过滤器漏掉错误类别
//! 时，「全绿」与「全错」是同一件事** —— 探针本身必须先被验证。
//!
//! ## spec 承诺清单怎么来的
//!
//! 从 `docs/mora-spec.md` 逐行正则提取 `` `ns.func(` `` 形式的承诺，共 42 处命中，
//! 其中 `router.route` 来自 §832 的一句**散文**（「`router |> route(...)` 与
//! `router.route(...)` 等价」），指的是 `Router::new()` 之后的**值方法**、
//! 不是裸命名空间 —— 实测 `let router = Router::new()` 后 `router.route(…)`
//! 正常。故不计入裸命名空间承诺，实际为 **41 个**。
//!
//! ## 2 个未实现的
//!
//! ```text
//! ai.create(name, config)  spec §1100  string, dict -> agent   → Unknown method: AiChat.create
//! ai.stream(prompt)        spec §1099  string -> stream      → Unknown method: AiChat.stream
//! ```
//!
//! **基础设施已齐，只缺生产者**：`Value::Stream { reader, done, xform }` 定义完整
//! （带 Clojure 风格 transducer 管线，`transducer.rs` 的 `Send+Sync+Debug` 约束
//! 就是为它写的），方法 `collect` / `is_done` 已登记并由
//! `method_dispatch.rs::call_method_stream` 分派 —— 但全仓**零构造点**；
//! `Value::Agent` 同样存在且有 `call_method_agent`。
//! 故与 D4（`int`/`float`/`bool` 只在 typeck 有签名、运行期没分支）同类的
//! 「承诺语法未接线」。
//!
//! ## 为什么只记录不实现
//!
//! `ai.stream` 在 mock 模式下「流」产出什么、`ai.create` 的 config 支持哪些键，
//! spec 均未成文（§708 示例只给�� `tools` / `model`）—— 属功能设计，未擅自做。
//! 本文件钉住**当前事实**：将来接线后这 2 条会通过，测试会失败并提示移组。

use std::sync::Arc;

use mora::interpreter::Interpreter;
use mora::mir::effect::Effects;
use mora::mir::vm::run_mir;
use mora::value::Value;

/// 「名字没解析上」的四类失败 —— 出现任一即表面失守。
fn is_unresolved(err: &str, ns: &str) -> bool {
    err.contains("Unknown method")
        || err.contains("Unknown function")
        || err.contains(&format!("Unbound variable '{ns}'"))
        || err.contains("is a module, not a function")
}

fn run(src: &str) -> Result<Value, String> {
    let (func, witnesses) =
        mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
    if !errs.is_empty() {
        let msgs: Vec<String> = errs.iter().map(|e| e.message.clone()).collect();
        return Err(format!("TYPECK: {msgs:?}"));
    }
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    run_mir(&arc, &mut interp, &mut env, &mut Effects::new())
}

/// v0.104.6 D151：`%TMP%` 占位符 → 一个**本次运行专属**的临时目录。
///
/// ## 为什么需要它
///
/// 这份普查**只**检验「名字能不能解析到已知方法」（见文件头），
/// 并不需要执行成功 —— 但 `file.write_text` / `file.mkdir_all` 是**真的会执行**的：
/// 相对路径落到**进程 CWD**，而 `cargo test` 的 CWD 是**包根目录**。
/// 于是每跑一次全量测试，仓库根就多出一个 `a.txt` 和一个 `a/`，
/// 污染 `git status`（本轮实测确认，两者都真被建了出来）。
///
/// 即：**一份声称「不要求执行成功」的测试，却带着文件副作用。**
///
/// 路径用**正斜杠**（Windows 的 `C:\Users\…` 里的 `\U` 会被 Mora 的
/// 字符串字面量当转义），并在结束后整目录清掉。
fn tmp_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("mora_d151_spec_surface")
}

fn expand_tmp(src: &str) -> String {
    let dir = tmp_dir();
    // 正斜杠 + 去掉尾部斜杠，避免 `C:/…/Temp//a.txt`
    let s = dir.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/').to_string();
    src.replace("%TMP%", &s)
}

/// spec 逐字承诺、且**已实现**的点号内建（附一组能走到方法体的实参）。
const WORKING: &[(&str, &str)] = &[
    ("agent.create", r#"agent.create("a", {tools: []})"#),
    ("ai.chat", r#"ai.chat("hi")"#),
    ("ai.critic", r#"ai.critic("hi")"#),
    ("document.parse", r#"document.parse("x.md")"#),
    ("file.exists", r#"file.exists("a.txt")"#),
    ("file.join", r#"file.join("a", "b")"#),
    ("file.list", r#"file.list(".")"#),
    ("file.mkdir_all", r#"file.mkdir_all("%TMP%/adir")"#),
    ("file.read_text", r#"file.read_text("a.txt")"#),
    ("file.write_text", r#"file.write_text("%TMP%/a.txt", "x")"#),
    ("json.parse", r#"json.parse("[1]")"#),
    ("json.stringify", r#"json.stringify({a: 1})"#),
    ("linalg.cross", r#"linalg.cross([1.0], [1.0])"#),
    ("linalg.dot", r#"linalg.dot([1.0], [1.0])"#),
    ("linalg.matmul", r#"linalg.matmul([[1.0]], [[1.0]])"#),
    ("linalg.norm", r#"linalg.norm([1.0])"#),
    ("linalg.transpose", r#"linalg.transpose([[1.0]])"#),
    ("math.hypot", r#"math.hypot(2.0, 3.0)"#),
    ("math.pow", r#"math.pow(2.0, 3.0)"#),
    ("random.rand_choice", r#"random.rand_choice([1])"#),
    ("random.rand_float", r#"random.rand_float(0.0, 1.0)"#),
    ("random.rand_int", r#"random.rand_int(0, 1)"#),
    ("random.random", r#"random.random()"#),
    ("random.seed", r#"random.seed(1)"#),
    ("random.shuffle", r#"random.shuffle([1])"#),
    ("stats.histogram", r#"stats.histogram([1.0], 2)"#),
    ("stats.quantile", r#"stats.quantile([1.0], 0.5)"#),
    ("tea.dispatch", r#"tea.dispatch(1, 2, 3)"#),
    ("tea.init", r#"tea.init(1)"#),
    ("tea.model", r#"tea.model(1)"#),
    ("tea.run", r#"tea.run(1)"#),
    ("tea.update", r#"tea.update(1, 2, 3)"#),
    ("tea.view", r#"tea.view(1)"#),
    ("web.fetch", r#"web.fetch("http://x")"#),
    ("xform.attach", r#"xform.attach([1])"#),
    ("xform.comp", r#"xform.comp([1])"#),
    ("xform.filter", r#"xform.filter([1])"#),
    ("xform.map", r#"xform.map([1])"#),
    ("xform.take", r#"xform.take([1])"#),
];

/// spec 逐字承诺、但**当前不存在**的点号内建。
const MISSING: &[(&str, &str)] = &[
    ("ai.create", r#"ai.create("researcher", {tools: []})"#),
    ("ai.stream", r#"ai.stream("hi")"#),
];

/// 39 个已实现的必须**解析到已知方法**。
///
/// 不要求执行成功 —— `web.fetch` 无网络、`file.read_text` 文件不存在都会
/// 报错，但那是**运行期**问题，不是「名字没解析上」。
#[test]
fn spec_promised_dotted_builtins_that_exist_resolve() {
    let mut failures = Vec::new();
    // v0.104.6 D151：临时目录建在**循环外**，清在最后 —— 即使断言 panic 前的
    // 副作用也被限制在这个目录内，不会落进仓库根。
    let _ = std::fs::create_dir_all(tmp_dir());
    for (name, src) in WORKING {
        let ns = name.split('.').next().unwrap_or(name);
        if let Err(e) = run(&format!("print({})\n", expand_tmp(src)))
            && is_unresolved(&e, ns)
        {
            failures.push(format!("  [{name}] {e}"));
        }
    }
    let _ = std::fs::remove_dir_all(tmp_dir());
    assert!(
        failures.is_empty(),
        "spec 逐字承诺且已实现的点号内建**解析不到**（{} / {}）：\n{}",
        failures.len(),
        WORKING.len(),
        failures.join("\n")
    );
}

/// v0.104.6 D151 反向判据：普查**不得**再往仓库根目录写文件。
///
/// 这是本轮的**主判据**：上一版每跑一次全量测试就在包根留下 `a.txt` 与 `a/`。
/// 这里不依赖「跑完之后目录里有没有东西」（那是上一轮的事后观察），
/// 而是直接断言**表里已无相对路径的写操作** —— 从源头钉住。
#[test]
fn d151_surface_census_writes_nothing_relative_to_cwd() {
    for (name, src) in WORKING {
        let ns = name.split('.').next().unwrap_or(name);
        let mutating = matches!(ns, "file") && !src.contains("%TMP%");
        // `file.exists` / `file.read_text` / `file.list` / `file.join` 是只读的，
        // 相对路径无害；只有会**改动**文件系统的调用必须指向 %TMP%。
        let writes = src.contains("write_text")
            || src.contains("mkdir_all")
            || src.contains("remove")
            || src.contains("delete")
            || src.contains("append");
        assert!(
            !(writes && mutating),
            "[{name}] `{src}` 会改动文件系统却是**相对路径** → 落进 CWD（包根）; \
             请改成 %TMP% 占位符"
        );
    }
}

/// `router.route` 是**值方法**（`Router::new()` 之后），不是裸命名空间 ——
/// spec §832 的那句散文指的是它。单独钉住，免得日后误当裸命名空间来测。
#[test]
fn router_route_is_a_value_method_not_a_bare_namespace() {
    let e = run("print(router.route(\"GET\", \"/\", 1))\n").expect_err(
        "裸名 `router` **不是**已注册的内建（spec §125 用的是显式 API `Router::new()`）；\
         若已注册，请把它移进 WORKING 组",
    );
    assert!(
        e.contains("Unbound variable 'router'") || e.contains("type error"),
        "期望报 `Unbound variable 'router'`，实得: {e}"
    );
    // 而值方法形态照常可用
    assert!(
        run("let router = Router::new()\nprint(router.route(\"GET\", \"/\", 1))\n").is_ok(),
        "`Router::new()` 之后的 `router.route(...)` 必须照常可用"
    );
}

/// 2 个未实现的必须**以可识别的方式**被拒 —— 将来接线后本测试会失败并提示移组。
#[test]
fn spec_promised_dotted_builtins_that_are_missing_are_pinned() {
    for (name, src) in MISSING {
        let e = run(&format!("print({src})\n")).err().unwrap_or_else(|| {
            panic!("[{name}] spec 承诺了它却仍未实现；请把它移进 WORKING 组并更新本文件")
        });
        assert!(
            e.contains("Unknown method: AiChat"),
            "[{name}] 期望 `Unknown method: AiChat.*`（AiChat 上缺该分支），实得: {e}"
        );
    }
}

/// 覆盖完整性：spec 承诺 41 个（42 处正则命中减去散文句里的 `router.route`），
/// 本测试必须把 41 个**全部**钉住。
#[test]
fn spec_dotted_surface_is_fully_covered() {
    const SPEC_COUNT: usize = 41;
    assert_eq!(
        WORKING.len() + MISSING.len(),
        SPEC_COUNT,
        "本测试钉住了 {} 项，但 spec 承诺 {SPEC_COUNT} 个点号内建 —— \
         有承诺没被覆盖。改动 spec 的内建表时，请同步更新 WORKING / MISSING 两组。",
        WORKING.len() + MISSING.len()
    );
}
