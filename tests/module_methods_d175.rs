//! v0.104.6 D175：`methods_of(ai)` / `methods_of(agent)` / `methods_of(random)`
//! 对**三个确有可用方法的模块**返回 `[]`（已修）。
//!
//! ## 缺陷
//!
//! `Value::methods()` 的 `Value::Builtin` 臂已经接上
//! `typeck::dispatch::module_method_names()`（D171 做的），但该函数对这三个
//! 模块 `_ => return Vec::new()`，而 `value.rs` 那条注释写的是
//! 「**故意**不在表内 —— 它们有各自的精确 `Type` 变体与专用分派路径」。
//!
//! 那是**解释现状**，不是**排除它们是对的**。修前/修后实测：
//!
//! ```text
//!                                    修前        修后
//! methods_of(ai)                     []   ❌     [chat, tokens, critic]
//! methods_of(agent)                  []   ❌     [create, critic]
//! methods_of(random)                 []   ❌     [rand_int, rand_float, rand_choice, seed, shuffle]
//! ```
//!
//! 而这 10 个方法经**运行期逐个实测**全部可达。危害与 D169 同型：
//! 本语言是 AI-native 的，agent 靠 `methods_of` 推断能力。
//! **空集 = 告诉 agent「这个模块什么都不能做」**，比报错方法更糟 ——
//! agent 会改去猜方法名、或者干脆绕开 `ai` 模块。
//!
//! ## 两处**刻意的收窄**（本测试专门守着它们不被回退成「全列」）
//!
//! 1. `ai` **不含** `retry` / `role` / `dag` / `heartbeat` / `context.*`。
//!    它们在 `call_ai_method` 里实现且单测全绿，但源码**不可达**
//!    （`ai` 裸名解析成 `BuiltinKind::AiChat`，而实现只挂在
//!    `(BuiltinKind::Ai, _)` 上）—— 已由 `tests/ai_namespace_reachability.rs`
//!    （D59）记录并**明确「只记录不修」**：那属文法设计决定。
//!    把它们列进自省，等于**宣称**一批调不通的方法 —— 那比空集更坏。
//! 2. `agent` **不含** `run` / `name` / `max_steps`。它们是 **Agent 值**的方法
//!    （`call_method_agent`），不是**模块**的方法。`Type::Agent` 同时表示
//!    二者是已知设计代价，但自省必须报**接收者确实是模块**的那一批。
//!
//! ## 判据：双向闭合
//!
//! 沿用 `module_methods_introspection.rs` 文件头自己写明的验收标准：
//!
//! - ① 清单里每个名字在运行期**确实被接受**（不是 unknown method）；
//! - ② 清单**不多列**不可达的方法（上面两处收窄）。
//!
//! ①是「不少列」、②是「不多列」。只守①的话，把不可达方法全塞进去也能过；
//! 只守②的话，一张空表也能过。**两条缺一不可。**
//!
//! ## 判据的牙齿：临时回退修复实测（别把陪跑当覆盖）
//!
//! 把 `module_method_names` 的三个臂改回 `return Vec::new()` 后重跑：
//!
//! | 测试 | 回退后 | 说明 |
//! |---|---|---|
//! | `…lists_exactly_the_reachable_methods` | **FAILED** ✅ | 承重的那条 |
//! | `…random_error_text_matches_introspection` | **FAILED** ✅ | |
//! | `tests/module_methods_introspection.rs` 的 D170 普查 | **FAILED** ✅ | 老护栏也独立抓到了 |
//! | `…every_listed_method_is_actually_callable` | ok | **空转** |
//! | `…does_not_over_list_unreachable_methods` | ok | **空转** |
//!
//! 后两条在回退态仍绿，是因为它们的判据对象是**清单的内容**：清单为空时，
//! 「每个都可用」「没有多列的」都**空真**。这是结构性的，不是判据写错 ——
//! 它们防的是**将来**有人往表里多塞/少塞名字，不防「表本身空了」。
//! 「表不能空」由上面那条精确比对负责。**如实记账：4 条里 2 条有牙齿，
//! 2 条是精化护栏。**

use std::path::{Path, PathBuf};
use std::process::Command;

/// 三个模块的 `methods_of` 结果（修后应得的精确值）。
const EXPECTED: &[(&str, &[&str])] = &[
    ("ai", &["chat", "tokens", "critic"]),
    ("agent", &["create", "critic"]),
    (
        "random",
        &["rand_int", "rand_float", "rand_choice", "seed", "shuffle"],
    ),
];

/// 每个方法一个**可达**的调用表达式（用于判据①）。
fn call_expr(module: &str, method: &str) -> Option<&'static str> {
    Some(match (module, method) {
        ("ai", "chat") => "ai.chat(p\"x\")",
        ("ai", "tokens") => "ai.tokens()",
        ("ai", "critic") => "ai.critic(\"a\")",
        ("agent", "create") => "agent.create(\"b\", {})",
        ("agent", "critic") => "agent.critic(\"a\")",
        ("random", "rand_int") => "random.rand_int(1, 10)",
        ("random", "rand_float") => "random.rand_float(0.0, 1.0)",
        ("random", "rand_choice") => "random.rand_choice([\"a\"])",
        ("random", "seed") => "random.seed(7)",
        ("random", "shuffle") => "random.shuffle([1])",
        _ => return None,
    })
}

/// 已知**不可达**的方法名 —— 自省**不得**列出（判据②）。
const MUST_NOT_LIST: &[(&str, &[&str])] = &[
    // D59：实现存在、单测全绿，但源码路径不可达。
    (
        "ai",
        &[
            "retry",
            "role",
            "dag",
            "heartbeat",
            "context.trim",
            "context.info",
        ],
    ),
    // Agent **值**的方法，不是模块的方法。
    ("agent", &["run", "name", "max_steps"]),
];

/// 探针目录：每条用例一个**独占子目录**，`Drop` 时删除。
///
/// 两个理由叠加，缺一不可：
/// - **并行安全**：cargo 默认并行跑用例。共用一个 `p.mora` 时它们互相覆写
///   对方的探针再读到对方的输出 —— 症状是「单独跑绿、全量跑红，且每次红的
///   用例都不一样」。`module_methods_introspection.rs` 文件头记过一次同类坑。
/// - **失败也清理**：写在测试尾部的 `remove_dir_all` 在断言失败 panic 时
///   **根本不执行**，于是「失败的测试反而留下垃圾目录」。`Drop` 管得住所有出口。
struct ProbeDir(PathBuf);

impl ProbeDir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("mora_d175_{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建探针目录");
        ProbeDir(d)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ProbeDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 在 `dir` 里跑一段 Mora，返回 (stdout+stderr, exit code)。
fn run_src(dir: &Path, src: &str) -> (String, i32) {
    let prog = dir.join("p.mora");
    std::fs::write(&prog, src).expect("写探针");
    let mora = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(mora).arg(&prog).output().expect("跑 mora");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (s, out.status.code().unwrap_or(-1))
}

/// 一次程序打印三个模块的 `methods_of`，返回 `(模块, [方法…])`。
fn introspect(dir: &Path) -> Vec<(String, Vec<String>)> {
    let mut src = String::new();
    for (m, _) in EXPECTED {
        src.push_str(&format!("print(\"@@ {m}\")\nprint(methods_of({m}))\n"));
    }
    let (out, code) = run_src(dir, &src);
    assert_eq!(code, 0, "自省探针应成功: {}", out);

    let mut result: Vec<(String, Vec<String>)> = Vec::new();
    let mut pending: Option<String> = None;
    for line in out.lines() {
        let t = line.trim();
        if let Some(name) = t.strip_prefix("@@ ") {
            pending = Some(name.trim().to_string());
        } else if let Some(m) = pending.take() {
            let names: Vec<String> = t
                .trim_matches(|c| c == '[' || c == ']')
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            result.push((m, names));
        }
    }
    result
}

/// 判据的**核心**：三个模块的 `methods_of` 必须精确等于实测可达的那几个。
///
/// 这一条在缺陷存在时（`[]`）会红 —— 是真正有牙齿的断言。
#[test]
fn d175_introspection_lists_exactly_the_reachable_methods() {
    let dir = ProbeDir::new("exact");
    let got = introspect(dir.path());
    for (module, expected) in EXPECTED {
        let actual = got
            .iter()
            .find(|(m, _)| m == module)
            .unwrap_or_else(|| panic!("探针没产出 {} 的 methods_of: {:?}", module, got));
        let mut a = actual.1.clone();
        let mut e: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        a.sort();
        e.sort();
        assert_eq!(
            a, e,
            "methods_of({}) 应精确列出这批实测可达的方法; 实得 {:?}",
            module, a
        );
        assert!(
            !a.is_empty(),
            "methods_of({}) 不得为空集 —— 空集等于告诉 agent「什么都不能做」",
            module
        );
    }
}

/// 判据①：**列出的每个方法，运行期必须真的被接受**。
///
/// 逐个起子进程（每个方法一次）。若某条列了却调不通，这里立刻红 ——
/// 自省就变成了「撒谎的清单」。
#[test]
fn d175_every_listed_method_is_actually_callable() {
    let dir = ProbeDir::new("callable");
    for (module, expected) in EXPECTED {
        for method in *expected {
            let expr = call_expr(module, method)
                .unwrap_or_else(|| panic!("{}:{} 没有对应的调用表达式", module, method));
            // 不 print 随机数：typeck 要求 random.* 是 pure（打印会报
            // `expected pure`），那会与「方法可用」这件事无关地失败。
            let src = format!("let q = {}\n", expr);
            let (out, code) = run_src(dir.path(), &src);
            assert_eq!(
                code, 0,
                "methods_of({}) 列出了 `{}`，但运行期不接受它 —— \
                 自省列了调不通的方法: {}\n源: {}",
                module, method, out, src
            );
            assert!(
                !out.contains("Unknown method"),
                "methods_of({}) 列出的 `{}` 运行期报未知方法: {}",
                module,
                method,
                out
            );
        }
    }
}

/// 判据②：**不得多列**不可达的方法。
///
/// 只守①的话，把 D59 那 6 个不可达实现全塞进表里也能过 ——
/// 而那会让 agent 去调一批必然失败的方法。
#[test]
fn d175_does_not_over_list_unreachable_methods() {
    let dir = ProbeDir::new("overlist");
    let got = introspect(dir.path());
    for (module, forbidden) in MUST_NOT_LIST {
        let actual = got
            .iter()
            .find(|(m, _)| m == module)
            .unwrap_or_else(|| panic!("探针没产出 {} 的 methods_of", module));
        for name in *forbidden {
            assert!(
                !actual.1.iter().any(|n| n == name),
                "methods_of({}) 不得列出 `{}`: 它在源码里不可达/不属于模块接收者; 实得 {:?}",
                module,
                name,
                actual.1
            );
        }
    }
}

/// `random` 的报错文案必须与 `methods_of(random)` **逐名一致**。
///
/// 二者原先是两份独立清单（报错里硬编码字符串、自省走另一张表）——
/// 改一处忘另一处，agent 就会「按报错写代码、按自省做判断」而两边对不上。
#[test]
fn d175_random_error_text_matches_introspection() {
    let dir = ProbeDir::new("errtext");
    let (out, _) = run_src(dir.path(), "let q = random.__nope__\n");
    let listed: Vec<String> = introspect(dir.path())
        .into_iter()
        .find(|(m, _)| m == "random")
        .map(|(_, v)| v)
        .unwrap_or_default();
    assert!(!listed.is_empty(), "前提：methods_of(random) 应非空");
    for name in &listed {
        assert!(
            out.contains(name),
            "typeck 报错没提到 `{}` —— 报错文案与自省清单已漂移: {}",
            name,
            out
        );
    }
}
