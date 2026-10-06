//! v0.104.6 D328：高阶函数的**元数与类型安全**矩阵（37 个调用，零 panic）
//! —— 本轮**否定轮**，但钉住了三条此前**无人覆盖**的契约。
//!
//! D325（list 方法）/ D326（string+dict 方法）/ D327（命名内建）之后，
//! 本轮扫**高阶函数**这条面：`map` / `filter` / `reduce` / `apply` /
//! `curry` / `partial` / `compose` + 闭包调用本身。
//!
//! ## 矩阵结论：元数与类型安全**很紧**
//!
//! | 场景 | 结果 |
//! |---|---|
//! | `map` / `filter` / `reduce` 拿到 0 元、2 元、3 元闭包 | **全部干净报错** |
//! | `map(1)` / `filter(true)` / `reduce(0, 0)` 传非闭包 | **全部干净报错** |
//! | `apply(f, [太短])` / `[太长]` / `[]` / `非列表` | **全部干净报错** |
//! | 闭包调用少传 / 多传实参 | typeck 拒绝（`closure_arity_check.rs` 已钉） |
//! | 调用非闭包 | typeck 拒绝 |
//! | **37 个调用零 panic** | |
//!
//! ## `curry` 是**正确的**（本轮我误判了三次，这是重点）
//!
//! `curry(f, n)` 的 `n` 是闭包的**总元数**（当参数数 ≥ n 时调内部 fn），
//! 不足则累积并返回新 Curry。`dispatch.rs:252-268` 的实现正确。
//! 部分应用经**多步 `let`** 可用（内联 `f(1)(2)(3)` 因 D123 的 postfix
//! 缺口不可用）：
//!
//! ```mora
//! let c = curry(fn(a, b) a + b end, 2)
//! let c1 = c(1)          // 部分应用 → 新 Curry
//! let v = c1(2)          // → 3.0
//! ```
//!
//! **本轮我连续三次把它误判为「失效」，根因是「没先读语义就下结论」**：
//! ① 传了 `arity=1` 给 2 元闭包（是我的测试写错）；② 只试了内联
//! `curry(...)(...)`（D123 已记不支持）；③ 只看了 `1` 元闭包的对照组
//! （恰是既有判据覆盖的那一格）。
//!
//! ## 真正的发现：`partial` / `compose` 的返回值**无法被调用**
//!
//! ```text
//! let p = partial(fn(a,b) a + b end, 1)
//! let v = p(2)      →  Type error: expected partial, got fn (float) -> ...
//! let c = compose(f, g)
//! let v = c(3)      →  Type error: expected compose, got fn (float) -> ...
//! ```
//!
//! 而 `dispatch.rs` **写好了** `Value::Partial` 的调用分支（:246-250）与
//! `Value::Compose` 的可调用登记（:973）—— **代码在，类型系统禁止**。
//! ⇒ 这两个内建目前是**建得出、用不了**的死功能面。
//! 唯一漏网：`compose(...)(c())` 零参能过 typeck，撞运行期
//! `closure expects 1 args, got 0`。
//!
//! 修它要改 typeck 签名（`functional_builtins_signatures.rs` 已把
//! `compose` / `partial` 设为**变参**以容纳 D80 的回归）⇒ **语义/路线图决定，
//! 本轮只报告不实施**。

use std::process::Command;

fn run(src: &str, tag: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("mora_d328_{}", slug(tag)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, src).expect("写探针");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe).arg(&p).output().expect("跑 mora");
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&out.stdout).into_owned()
        + "\n"
        + &String::from_utf8_lossy(&out.stderr);
    let all: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("Mora v")
                && !l.starts_with("AI:")
                && !l.starts_with("AI 原语")
                && !l.starts_with("显式 API")
                && !l.starts_with("Trait 系统")
                && !l.starts_with("Built-in")
                && !l.starts_with("v0.15 CLI")
                && !l.starts_with('⚠')
                && !l.starts_with("[9layer]")
        })
        .map(|l| {
            l.replace(&p.to_string_lossy().to_string(), "<TMP>")
                .to_string()
        })
        .collect();
    (out.status.code().unwrap_or(-1), all.join(" | "))
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// **主判据**：高阶方法拿到**元数不匹配**的闭包或**非闭包**，必须报错。
///
/// 这一族此前**没有任何判据覆盖**（`builtin_gaps` 只测签名与正常路径）。
#[test]
fn d328_higher_order_methods_reject_wrong_arity_and_non_closures() {
    // 元数不匹配：0 元 / 2 元 / 3 元闭包
    for src in [
        "[1,2,3].map(fn() 42 end)",
        "[1,2,3].map(fn(a,b) a + b end)",
        "[1,2,3].map(fn(a,b,c) a end)",
        "[1,2,3].filter(fn() true end)",
        "[1,2,3].filter(fn(a,b) a end)",
        "[1,2,3].reduce(fn(a) a end, 0)",
        "[1,2,3].reduce(fn(a,b,c) a end, 0)",
    ] {
        let (code, got) = run(&format!("print({src})\n"), src);
        assert_ne!(
            code, 0,
            "`{src}` 的闭包元数不匹配，必须报错（不得静默补 nil / 截断）。\n\
             实得: exit=0 out={got}"
        );
        assert!(
            got.to_lowercase().contains("closure"),
            "`{src}` 的报错应指明是 closure 元数问题; 实得: {got}"
        );
    }
    // 非闭包
    for src in [
        "[1,2,3].map(1)",
        "[1,2,3].filter(true)",
        "[1,2,3].reduce(0, 0)",
    ] {
        let (code, got) = run(&format!("print({src})\n"), src);
        assert_ne!(
            code, 0,
            "`{src}` 传的不是闭包，必须报错（不得静默通过）。实得: exit=0 out={got}"
        );
    }
    // apply 的实参列表长度必须与闭包元数严格相等
    for src in [
        "apply(fn(a,b) a+b end, [1])",
        "apply(fn(a,b) a+b end, [1,2,3])",
        "apply(fn(a,b) a+b end, [])",
        "apply(fn(a,b) a+b end, 1)",
    ] {
        let (code, got) = run(&format!("print({src})\n"), src);
        assert_ne!(
            code, 0,
            "`{src}` 的实参列表与闭包元数不符，必须报错。实得: exit=0 out={got}"
        );
    }
    // 正常路径不得回归
    for (src, want) in [
        ("[1,2,3].map(fn(x) x * 2 end)", "[2.0, 4.0, 6.0]"),
        ("[1,2,3].filter(fn(x) x > 1 end)", "[2.0, 3.0]"),
        ("[1,2,3].reduce(fn(a,b) a + b end, 0)", "6.0"),
        ("[].map(fn(x) x end)", "[]"),
        ("[].reduce(fn(a,b) a + b end, 0)", "0.0"),
        ("apply(fn(a) a * 2 end, [4])", "8.0"),
        ("apply(fn() 7 end, [])", "7.0"),
    ] {
        let (code, got) = run(&format!("print({src})\n"), src);
        assert_eq!(code, 0, "`{src}` 应成功; 实得 exit={code} out={got}");
        assert_eq!(got, want, "`print({src})` 应得 `{want}`; 实得 `{got}`");
    }
}

/// **`curry` 正常**：部分应用经多步 `let` 可用。
///
/// 钉它是因为本轮我**连续三次**把它误判为失效（传错 arity / 只试内联 /
/// 只看 1 元闭包对照组）。本条把三个容易踩空的正确用法都钉住。
#[test]
fn d328_curry_partial_application_works_via_stepwise_let() {
    let f2 = "fn(a, b) a + b end";
    let f3 = "fn(a, b, c) a + b + c end";
    for (tag, src, want) in [
        (
            "2元全应用",
            format!("let c = curry({f2}, 2)\nlet v = c(1,2)\nprint(v)\n"),
            "3.0",
        ),
        (
            "2元分两步",
            format!("let c = curry({f2}, 2)\nlet c1 = c(1)\nlet v = c1(2)\nprint(v)\n"),
            "3.0",
        ),
        (
            "3元分三步",
            format!(
                "let c = curry({f3}, 3)\nlet c1 = c(1)\nlet c2 = c1(2)\nlet v = c2(3)\nprint(v)\n"
            ),
            "6.0",
        ),
        (
            "3元分两步",
            format!("let c = curry({f3}, 3)\nlet c1 = c(1)\nlet v = c1(2,3)\nprint(v)\n"),
            "6.0",
        ),
        (
            "uncurry 取回",
            format!("let c = curry({f2}, 2)\nlet f = uncurry(c)\nlet v = f(3,4)\nprint(v)\n"),
            "7.0",
        ),
        (
            "1元对照组（既有判据覆盖的那格）",
            "let c = curry(fn(x) x + 1 end, 1)\nlet v = c(1)\nprint(v)\n".to_string(),
            "2.0",
        ),
    ] {
        let (code, got) = run(&src, &format!("curry_{tag}"));
        assert_eq!(code, 0, "[{tag}] 应成功; 实得 exit={code} out={got}");
        assert_eq!(got, want, "[{tag}] 应得 `{want}`; 实得 `{got}`");
    }
}

/// **现状**：`partial` / `compose` 的返回值**无法被调用**。
///
/// `dispatch.rs` 写好了 `Value::Partial` 的调用分支（:246-250）与
/// `Value::Compose` 的可调用登记（:973），但**类型系统禁止所有调用形式**
/// ⇒ 这两个内建目前是**建得出、用不了**的死功能面。
///
/// 修它要改 typeck 签名（`functional_builtins_signatures.rs` 已因 D80 的
/// 回归把 `compose` / `partial` 设为**变参**）⇒ 语义/路线图决定，只报告不实施。
#[test]
fn d328_status_quo_partial_and_compose_results_are_unappliable() {
    for src in [
        "let p = partial(fn(a,b) a + b end, 1)\nlet v = p(2)\nprint(v)\n",
        "let p = partial(fn(x) x + 1 end, 1)\nlet v = p(2)\nprint(v)\n",
        "let p = partial(fn(x) x + 1 end, 1)\nlet p1 = p(2)\nlet v = p1(3)\nprint(v)\n",
        "let c = compose(fn(x) x+1 end, fn(x) x*2 end)\nlet v = c(3)\nprint(v)\n",
        "let c = compose(fn(x) x+1 end, fn(x) x*2 end)\nlet v = c(1,2,3)\nprint(v)\n",
    ] {
        let (code, got) = run(src, &slug(src));
        assert_eq!(
            code, 2,
            "**D328 现状**：`partial` / `compose` 的返回值当前**无法被调用**\
             （typeck 拒：`expected partial, got fn` / `expected compose, got fn`）。\n\
             ⚠ 若本条失败，说明**这两个内建变得可用了** —— 那是**语义变更**\
             （需要同时给 typeck 补签名 + 确认 `dispatch.rs` 的调用分支正确），\
             请勿只改本判据。\n  实得: exit={code} out={got}"
        );
    }
    // 唯一漏网：零参调用能过 typeck，撞运行期元数错误
    let (code, got) = run(
        "let c = compose(fn(x) x+1 end, fn(x) x*2 end)\nlet v = c()\nprint(v)\n",
        "compose_zero",
    );
    assert_ne!(
        code, 0,
        "`compose(...)()` 零参调用当前会漏过 typeck 并在运行期报元数错误; \
         实得 exit={code} out={got}"
    );
    // 内联 postfix 是 D123 记档的语法缺口
    let (code, got) = run("print(curry(fn(x) x+1 end, 1)(2))\n", "curry_inline");
    assert_eq!(
        code, 2,
        "内联 `curry(...)(...)` 是 D123 记档的 postfix 语法缺口，当前应解析失败; \
         实得 exit={code} out={got}"
    );
}
