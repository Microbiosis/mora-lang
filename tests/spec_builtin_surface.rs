//! v0.110.0：spec §12 保留的 **23 个 `ns.func(...)`** 核心点号内建
//! （覆盖 7 个保留模块中的 `file` / `json` / `linalg` / `math` / `random` / `stats`）全部可解析到已知方法。
//!
//! ## 本文件检验的是「**名称表面**」而不是「跑通」
//!
//! 断言的是：**该名字能被解析到一个已知方法**。不要求执行成功 ——
//! `file.read_text` 会因文件不存在而报错，这些都**不是**表面问题。
//! 判据只排除四类「名字没解析上」的失败：
//! `Unknown method` / `Unknown function` / `Unbound variable '<ns>'` /
//! `'<ns>' is a module, not a function`。
//!
//! v0.110.0 Wave 1 移除了 `ai.*` / `agent.*` / `document.*` / `tea.*` / `web.*` /
//! `xform.*` / `Router::new` 等内置，仅保留 7 个核心模块；本测试同步收敛。
//! `ai.stream` 在 mock 模式下「流」产出什么、`ai.create` 的 config 支持哪些键，
//! spec 均未成文（§708 示例只给�� `tools` / `model`）—— 属功能设计，未擅自做。
//! 本文件钉住**当前事实**：将来接线后这 2 条会通过，测试会失败并提示移组。

use std::sync::Arc;

use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use mora::value::Value;

/// 「名字没解析上」的四类失败 —— 出现任一即表面失守。
fn is_unresolved(err: &str, ns: &str) -> bool {
    err.contains("Unknown method")
        || err.contains("Unknown function")
        || err.contains(&format!("Unbound variable '{ns}'"))
        || err.contains("is a module, not a function")
}

fn run(src: &str) -> Result<Value, String> {
    let (prog, witnesses) =
        mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    let errs = mora::typeck::check_mir::check_program_witnesses(&witnesses);
    if !errs.is_empty() {
        let msgs: Vec<String> = errs.iter().map(|e| e.message.clone()).collect();
        return Err(format!("TYPECK: {msgs:?}"));
    }
    let arc = Arc::new(prog);
    let mut core = CoreRuntime::default();
    execute_program(arc, &mut core)
        .map(|(v, _)| v)
        .map_err(|e| e.to_string())
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

/// spec 逐字承诺、且**已实现**的 23 个核心点号内建（附一组能走到方法体的实参）。
const WORKING: &[(&str, &str)] = &[
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
];

/// v0.110.0 Wave 1 已移除所有已承诺但未实现的点号占位（如 `ai.create` / `ai.stream`）。
const MISSING: &[(&str, &str)] = &[];

/// 23 个保留的点号内建必须**解析到已知方法**。
///
/// 不要求执行成功 —— `file.read_text` 文件不存在会报错，但那是**运行期**问题，
/// 不是「名字没解析上」。
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

/// 覆盖完整性：v0.110.0 核心点号内建抽样共 23 个，本测试必须把 23 个**全部**钉住。
#[test]
fn spec_dotted_surface_is_fully_covered() {
    const SPEC_COUNT: usize = 23;
    assert_eq!(
        WORKING.len() + MISSING.len(),
        SPEC_COUNT,
        "本测试钉住了 {} 项，但期望 {SPEC_COUNT} 个核心点号内建 —— \
         有承诺没被覆盖。改动 spec 的内建表时，请同步更新 WORKING / MISSING 两组。",
        WORKING.len() + MISSING.len()
    );
}
