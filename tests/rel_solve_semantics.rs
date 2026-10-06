//! v0.104.6 D377 —— `src/rel/` 的 **`solve` 一致性语义**（否定轮，无产品变更）
//!
//! `src/rel/` 共 6 个文件 1303 行，有 39 条自带单测 + 2 个判据
//! （`rel_family_surface` 测**形态**、`rel_conjunction_bindings` 测**合取绑定**）。
//! 此前未覆盖的是 **`solve` 的核心推理语义**：变量绑定、传递闭包、无解。
//!
//! ## 前提：必须先声明 `rel`，否则报「Undefined function」
//!
//! 真实语法（`parser_v3/rel.rs:10-12`）：
//!
//! ```text
//! rel edge("a", "b")                     -- 事实：头部是项，无体
//! rel path(?X, ?Y) edge(?X, ?Y) end      -- 规则：体是目标合取（`,` 连接）
//! rel path(x, z) edge(x, y), path(y, z) end
//! ```
//!
//! 首版探针直接 `solve { p(?X, ?Y) }` ⇒ *Undefined function or task: p*。
//!
//! ## 推理语义全部正确
//!
//! 给定 `edge("a","b")`、`edge("b","c")`、`path(?X,?Y) edge(?X,?Y) end`：
//!
//! | 查询 | 实测 | 含义 |
//! |---|---|---|
//! | `path(?X, "c")` | `[b]` | **传递闭包** a→b→c ✅ |
//! | `path("a", ?Y)` | `[b]` | 反向查询 ✅ |
//! | `path("a", "c")` | `[]` | 无变量可绑定 ⇒ 空 ✅ |
//! | `path("a", "zzz")` | `[]` | **无解** ⇒ 空（不报错）✅ |
//! | `path(?X, ?Y)` | `[[a,b],[b,c]]` | **所有解** ✅ |
//! | `edge(?X,?Y), path(?X,"c")` | `[[b, c]]` | 合取 ✅ |
//! | `solve { }` | Err *both 至少需要一个目标* | **空查询明确报错** ✅ |
//!
//! **「无解」返回空列表而不是报错**是设计（D216 判据已记录
//! `solve { p(?X, ?Y) }` → `[[a, b]]` 的形态），本条把它钉住。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d377_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d377_{n}_{}", slug(body)));
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
    let path_str = p.to_string_lossy().into_owned();
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
                && !l.contains("不兼容 v0.03")
                && !l.starts_with("[9layer]")
                && !is_bare_path_line(l, &path_str)
        })
        .map(str::to_string)
        .collect();
    (out.status.code().unwrap_or(-1), kept.join(" | "))
}

fn is_bare_path_line(line: &&str, path: &str) -> bool {
    **line == *path
}

/// 建一个「链 a→b→c」的测试程序，并跑一个 `solve` 查询。
fn solve_with(query: &str) -> (i32, String) {
    let body = format!(
        "rel path(?X, ?Y) edge(?X, ?Y) end\n\
         rel edge(\"a\", \"b\")\n\
         rel edge(\"b\", \"c\")\n\
         {query}\n"
    );
    ev(&body)
}

/// **装置自检**：合取绑定形态（D216 已修的）。
#[test]
fn d377_harness_solves_a_conjunction() {
    let (code, got) = solve_with("print(solve { path(\"a\", ?Y) })");
    assert_eq!(code, 0, "应正常跑; 实得 exit={code} out={got}");
    assert_eq!(got.trim(), "[b]", "setup 本身应能求解; 实得: {got}");
}

/// **传递闭包**：`path(?X, "c")` 能推出 `X=b`（a→b→c）。
#[test]
fn d377_transitive_closure() {
    let (code, got) = solve_with("print(solve { path(?X, \"c\") })");
    assert_eq!(code, 0, "应正常跑; 实得 exit={code} out={got}");
    assert_eq!(
        got.trim(),
        "[b]",
        "`path(?X,\"c\")` 应经 a→b→c 推出 X=b; 实得: {got}"
    );
}

/// **无解返回空列表**，**不报错**。
#[test]
fn d377_no_solution_returns_empty_not_error() {
    let (code, got) = solve_with("print(solve { path(\"a\", \"zzz\") })");
    assert_eq!(
        code, 0,
        "无解应是**空结果**而非错误（D216 记录的形态）; 实得 exit={code} out={got}"
    );
    assert_eq!(got.trim(), "[]", "无解应得空列表; 实得: {got}");
}

/// **无变量可绑定 ⇒ 空结果**（查询两端都是常量时）。
#[test]
fn d377_ground_query_yields_empty_bindings() {
    let (code, got) = solve_with("print(solve { path(\"a\", \"c\") })");
    assert_eq!(
        code, 0,
        "两端都是常量时不该报错; 实得 exit={code} out={got}"
    );
    assert_eq!(
        got.trim(),
        "[]",
        "两端都是常量 ⇒ 无变量可绑定 ⇒ 空; 实得: {got}"
    );
}

/// **枚举所有解**。
#[test]
fn d377_enumerates_all_solutions() {
    let (code, got) = solve_with("print(solve { path(?X, ?Y) })");
    assert_eq!(code, 0, "应正常跑; 实得 exit={code} out={got}");
    assert_eq!(
        got.trim(),
        "[[a, b], [b, c]]",
        "应枚举全部解（两条边）; 实得: {got}"
    );
}

/// **合取**：先给 `?X` 赋值再查询，绑定要**贯穿**。
#[test]
fn d377_conjunction_binding_flows_across_goals() {
    let (code, got) = solve_with("print(solve { edge(?X, ?Y), path(?X, \"c\") })");
    assert_eq!(code, 0, "应正常跑; 实得 exit={code} out={got}");
    assert_eq!(
        got.trim(),
        "[[b, c]]",
        "合取里 `edge(?X,?Y)` 提供的 X=b 要传给 `path(?X,\"c\")`; 实得: {got}"
    );
}

/// **空 `solve` 明确报错**（不是静默返回空）。
#[test]
fn d377_empty_solve_errors() {
    let (code, got) = solve_with("print(solve { })");
    assert_eq!(code, 1, "空 `solve` 必须报错; 实得 exit={code} out={got}");
    assert!(
        got.contains("at least one") || got.contains("至少需要一个"),
        "诊断应说明至少需要一个目标; 实得: {got}"
    );
}

/// **未声明的关系报「Undefined function」** —— 明确的错误而非静默失败。
#[test]
fn d377_undeclared_relation_errors() {
    let (code, got) = ev("print(solve { nosuchrel(?X, ?Y) })\n");
    assert_eq!(code, 1, "未声明的关系应报错; 实得 exit={code} out={got}");
    assert!(
        got.contains("Undefined") || got.contains("not defined"),
        "诊断应是「未定义函数/关系」; 实得: {got}"
    );
}
