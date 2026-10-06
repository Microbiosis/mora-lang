//! v0.104.6 D337 —— `exec.parallel` 的 **`min_arity` 下限过紧**（已修）
//! + **空列表早返回绕过可选参数校验**（已修）
//!
//! `exec.*` 是 `builtins/` 里最大的未测模块（`exec.rs` 728 行），
//! 且它会真的派生子进程。本轮 26 个用例，**两个缺陷**。
//!
//! ## 缺陷 ①：typeck 的 `min_arity = 2` 拒绝运行期**明确支持**的单参形式
//!
//! ```text
//! exec.parallel(["echo a"])  → exit 2  Type error: Expected 2 arguments, got 1
//! ```
//!
//! 而**运行期自己说**「至少 1 个」，四处独立证据：
//! - `exec.rs:143-145` 的错误消息：「requires **at least 1 arg** (cmds list)」；
//! - `exec.rs:176` / `:197` 用 `args.len() >= 2` / `>= 3` 把 `max_concurrent` /
//!   `timeout_ms` 当**可选**处理；
//! - `exec.rs:143` 的 `args.is_empty()` 守卫**正是为「1 个参数」这条路径写的**；
//! - `exec.rs` 的运行期单测（`exec_parallel_runs_all_commands` 等）**全部用 1 个实参**。
//!
//! ⇒ 运行时契约 =「至少 1 个」，typeck 契约 =「至少 2 个」⇒ **契约分叉**。
//! `Signature` 文档里那句「运行期支持、类型系统拒绝 = 契约分叉」指的就是这个。
//!
//! ### 为什么 D81 没抓到
//!
//! `tests/signature_no_over_tightening.rs`（D81）固化的是**上限**那一侧：
//! 「运行期普遍用 `args.first()` / `args.get(N)` 忽略多余实参，
//! 所以补签名时**只能收紧下限、不能收紧上限**」，并逐个模块断言
//! 「传**多于**声明数量的实参必须放行」。
//!
//! ⇒ **下限**（`min_arity` 过紧）是**镜像**的问题，**未被覆盖**。
//! 而 `exec.parallel` 的用例（`exec.parallel(["a"], 2, 3, 4)`）恰好是 4 参 ——
//! 它同时满足「≥ min_arity」和「多余实参放行」两个条件，
//! 于是**下限过紧这一侧始终没被测到**。
//!
//! ## 缺陷 ②：空列表早返回**在**可选参数校验**之前**
//!
//! ```text
//! exec.parallel(["echo a"], -1)  → exit 1  max_concurrent must be a non-negative number
//! exec.parallel([], -1)          → exit 0  []        ← 同一个非法参数，被静默吞掉
//! exec.parallel([], "x")         → exit 0  []
//! exec.parallel([], 1, -1)       → exit 0  []
//! exec.parallel([], 1, "x")      → exit 0  []
//! ```
//!
//! 同一个非法参数，**因为命令列表是空的就看不到错误**。
//! 修法：把早返回**下移**到两个可选参数都校验之后。
//!
//! ⚠ 这是**收紧**：`max_concurrent` 已被 `.max(1)` 钳过、空列表不影响
//! 任何并发行为 ⇒ 合法调用的结果**完全不变**，只让非法参数不再被静默吞掉。
//!
//! ## 不变的语义（D285 修的负数守卫必须仍在）
//!
//! | 调用 | 结果 |
//! |---|
//! | `exec.parallel(["echo a"], -1)` | exit 1 `max_concurrent must be a non-negative number` |
//! | `exec.parallel(["echo a"], 1, -1)` | exit 1 `timeout_ms must be a non-negative number or nil` |
//! | `exec.parallel(["echo a"], 0)` | 放行（`.max(1)` 钳成 1）|
//! | `exec.parallel(["echo a"], 2.7)` | 放行（收口向零截断，D246 约定）|
//! | `exec.parallel(["echo a"], nil)` | 放行（与缺参同义 = 全部并发）|
//! | `exec.parallel([])` | `[]`（**不传**可选参数 ⇒ 合法）|

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d337_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d337e_{}_{}", n, slug(body)));
    std::fs::create_dir_all(&dir).expect("建目录");
    let p = dir.join("p.mora");
    std::fs::write(&p, body).expect("写探针");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("建 home");
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe)
        .arg(&p)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()
        .expect("跑 mora");
    let _ = std::fs::remove_dir_all(&dir);
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let kept: Vec<String> = text
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
                && !l.contains(&p.to_string_lossy().to_string())
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

/// **主断言 ①**：单参形式必须**可用** —— 这是运行期一直支持的契约。
#[test]
fn d337_single_arg_form_is_accepted() {
    let (code, got) = ev("print(exec.parallel([\"echo a\"]).len())\n");
    assert_eq!(
        code, 0,
        "`exec.parallel([\"echo a\"])`（单参）应成功 —— 运行期 `args.is_empty()` 守卫、\
         `args.len() >= 2` 的可选处理、以及全部运行期单测都按单参写的; 实得 exit={code} out={got}\n\
         修前是 `Expected 2 arguments, got 1`（typeck `min_arity` 过紧）"
    );
    assert_eq!(got, "1", "应返回 1 条结果; 实得 {got}");

    // 多个命令 + 保序
    let (code, got) = ev("print(exec.parallel([\"echo a\",\"echo b\"]).len())\n");
    assert_eq!(code, 0, "两命令单参应成功; 实得 exit={code} out={got}");
    assert_eq!(got, "2", "应返回 2 条结果; 实得 {got}");
}

/// **主断言 ②**：非法可选参数**不得因列表为空而被吞掉**。
///
/// 四种形态（负数 / 类型错 × `max_concurrent` / `timeout_ms`）全测，
/// 且必须与**非空**列表给出**同一条**消息。
#[test]
fn d337_invalid_optional_args_error_even_with_empty_list() {
    for (body, needle) in [
        (
            "exec.parallel([], -1)",
            "max_concurrent must be a non-negative number",
        ),
        (
            "exec.parallel([], \"x\")",
            "max_concurrent must be a non-negative number",
        ),
        (
            "exec.parallel([], 1, -1)",
            "timeout_ms must be a non-negative number or nil",
        ),
        (
            "exec.parallel([], 1, \"x\")",
            "timeout_ms must be a non-negative number or nil",
        ),
    ] {
        let (code, got) = ev(&format!("print({body})\n"));
        assert_eq!(
            code, 1,
            "`{body}` 应报错（参数非法与否，与有没有活干**无关**）; 实得 exit={code} out={got}\n\
             修前因 `exec.rs:164` 的空列表早返回在参数校验**之前**而静默返回 `[]`"
        );
        assert!(
            got.contains(needle),
            "`{body}` 应报 `{needle}`; 实得: {got}"
        );
    }

    // 与非空列表的诊断**逐字一致**
    for (empty, nonempty) in [
        ("exec.parallel([], -1)", "exec.parallel([\"echo a\"], -1)"),
        (
            "exec.parallel([], 1, -1)",
            "exec.parallel([\"echo a\"], 1, -1)",
        ),
    ] {
        let (c1, g1) = ev(&format!("print({empty})\n"));
        let (c2, g2) = ev(&format!("print({nonempty})\n"));
        assert_eq!(
            c1, c2,
            "`{empty}` 与 `{nonempty}` 的 exit 应一致; {c1} vs {c2}"
        );
        assert_eq!(
            g1, g2,
            "`{empty}` 与 `{nonempty}` 的诊断应**逐字一致**（同一个非法参数不该因\
             命令列表为空而换一套说法）; 实得 {g1} vs {g2}"
        );
    }
}

/// **对照组 1**：`exec.parallel([])`（**不传**非法参数）仍返回 `[]`。
///
/// 缺陷 ② 的修复是「把早返回**下移**」，不是「删掉早返回」——
/// 这条钉住那个区别。
#[test]
fn d337_plain_empty_list_still_returns_empty() {
    let (code, got) = ev("print(exec.parallel([]))\n");
    assert_eq!(
        code, 0,
        "`exec.parallel([])`（不传可选参数）应成功; 实得 exit={code} out={got}"
    );
    assert_eq!(got, "[]", "应得空列表; 实得 {got}");
}

/// **对照组 2**：D285 修的负数守卫**不得回退**。
#[test]
fn d337_d285_negative_guards_still_reject() {
    for (body, needle) in [
        (
            "exec.parallel([\"echo a\"], -1)",
            "max_concurrent must be a non-negative number",
        ),
        (
            "exec.parallel([\"echo a\"], 1, -1)",
            "timeout_ms must be a non-negative number or nil",
        ),
    ] {
        let (code, got) = ev(&format!("print({body})\n"));
        assert_eq!(code, 1, "`{body}` 应报错; 实得 exit={code} out={got}");
        assert!(
            got.contains(needle),
            "`{body}` 应报 `{needle}`; 实得: {got}"
        );
    }
}

/// **对照组 3**：合法的 `max_concurrent` / `timeout_ms` 取值全部照常工作。
#[test]
fn d337_valid_optional_args_still_work() {
    for (body, want) in [
        ("exec.parallel([\"echo a\"], 0).len()", "1"),
        ("exec.parallel([\"echo a\"], 1).len()", "1"),
        ("exec.parallel([\"echo a\"], 999999).len()", "1"),
        ("exec.parallel([\"echo a\"], 2.7).len()", "1"),
        ("exec.parallel([\"echo a\"], nil).len()", "1"),
        ("exec.parallel([\"echo a\"], 1, nil).len()", "1"),
        ("exec.parallel([\"echo a\"], 1, 999999999).len()", "1"),
        // 多余实参仍放行（D81 的上限契约不得被本条破坏）
        ("exec.parallel([\"echo a\"], 1, nil, 99).len()", "1"),
    ] {
        let (code, got) = ev(&format!("print({body})\n"));
        assert_eq!(code, 0, "`{body}` 应成功; 实得 exit={code} out={got}");
        assert_eq!(
            got, want,
            "`{body}` 应得 {want}; 实得 {got}\n\
             （`0` 被 `.max(1)` 钳成 1、`2.7` 按 D246 收口向零截断、`nil` 与缺参同义）"
        );
    }
}

/// **对照组 4**：返回值形状与逐条错误**不变**。
#[test]
fn d337_result_shape_and_errors_unchanged() {
    // 元素非字符串 —— 运行期逐个校验并**点名下标**
    let (code, got) = ev("print(exec.parallel([1, 2]))\n");
    assert_eq!(code, 1, "非字符串元素应报错; 实得 exit={code} out={got}");
    assert!(
        got.contains("cmds[0] must be a string"),
        "错误应点名**具体下标**; 实得: {got}"
    );

    // 首参类型
    for body in ["exec.parallel(\"echo a\")", "exec.parallel(1)"] {
        let (code, got) = ev(&format!("print({body})\n"));
        assert_eq!(code, 1, "`{body}` 应报错; 实得 exit={code} out={got}");
        assert!(
            got.contains("first arg must be a list of strings"),
            "`{body}` 的错误应点明首参; 实得: {got}"
        );
    }

    // 未知方法
    let (code, got) = ev("print(exec.run(\"echo a\"))\n");
    assert_eq!(code, 1, "未知方法应报错; 实得 exit={code} out={got}");
    assert!(got.contains("unknown method"), "应报未知方法; 实得: {got}");

    // 成功路径的 dict 字段集合
    let (code, got) = ev("print(exec.parallel([\"echo a\"]))\n");
    assert_eq!(code, 0, "应成功; 实得 exit={code} out={got}");
    for key in [
        "cmd:",
        "exit_code:",
        "stdout:",
        "stderr:",
        "index:",
        "error:",
    ] {
        assert!(got.contains(key), "结果 dict 应含字段 `{key}`; 实得: {got}");
    }

    // 非零退出码**不抛错**，而是落在 `exit_code` 里
    let (code, got) = ev("print(exec.parallel([\"exit 3\"]))\n");
    assert_eq!(
        code, 0,
        "命令非零退出**不应**抛错; 实得 exit={code} out={got}"
    );
    assert!(
        got.contains("exit_code: 3"),
        "应记录 exit_code 3; 实得: {got}"
    );
}
