//! v0.104.6 D263 —— `compose_prompt` 的 `budget` 必须接受 `Int`，
//! 且 `parse_budget_dispatch` 的错误信息必须用**类型名**而非 `Debug`。
//!
//! ## 缺陷（与 D262 的 `crush_json` 同型）
//!
//! `parse_budget_dispatch` 只匹配 `Value::Float`；`Int` 落进 `other` 分支，
//! 报 `budget must be string or number, got ...` —— **「int 明明是数字」**。
//!
//! ## 触发路径比看上去窄，但只有这一条
//!
//! `compose_prompt({text: "x", budget: 1000})` **走不到**这里：typeck 的
//! 「dict 字面量值必须同质」会先拒（`text` 是 `String`、`budget` 是数值）。
//! 实测 7 个变体里，dict 字面量的 5 种写法全被 typeck 挡在前面。
//!
//! **唯一能到达**的是 `json.parse`：
//!
//! ```text
//! let d = json.parse("{\"text\":\"x\",\"budget\":1000}")   // ← 产生 Value::Int(1000)
//! compose_prompt(d)
//! ```
//!
//! 而这正是最真实的用法：**prompt 配置从 JSON 读入**。故这不是「窄到不重要」的
//! 缺陷，而是「只有机器生成的输入才会踩到」——而机器生成的输入恰恰是
//! `Int`，字面量才是 `Float`。
//!
//! 实测（真实 CLI）：
//!
//! ```text
//! 修前  exit=1  "budget: budget must be string or number, got int"
//! 修后  exit=0
//! ```
//!
//! ## 顺带修：`{:?}` → 类型名
//!
//! D262 记录过「`Value::Dict` 的 `Debug` 按 HashMap 迭代序打印（每进程随机），
//! 同一条错误信息跨进程键序不同」。那处修好了，`parse_budget_dispatch` 的
//! `other` 分支**漏了**，本条一并修。

use std::path::PathBuf;

const NOISE: &[&str] = &[
    "Mora v",
    "AI:",
    "AI ",
    "Built-in",
    "v0.15",
    "⚠",
    "AI 原语",
    "显式 API",
    "Trait",
];

struct WorkDir(PathBuf);
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(dir: &std::path::Path, src: &str) -> (i32, String) {
    let f = dir.join("a.mora");
    std::fs::write(&f, src).expect("write");
    let out = std::process::Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/target/debug/mora.exe"
    ))
    .arg("run")
    .arg(&f)
    .output()
    .expect("run mora");
    let txt = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let msg = txt
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !NOISE.iter().any(|n| l.contains(n)))
        .collect::<Vec<_>>()
        .join(" | ");
    (out.status.code().unwrap_or(-1), msg)
}

fn work(tag: &str) -> WorkDir {
    let d = std::env::temp_dir().join(format!("mora_d263_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("mkdir");
    WorkDir(d)
}

/// ① 主判据：从 `json.parse` 读来的 `Int` budget 必须被接受。
#[test]
fn d263_compose_prompt_accepts_int_budget_from_json() {
    let w = work("main");
    let src =
        "let d = json.parse(\"{\\\"text\\\":\\\"x\\\",\\\"budget\\\":1000}\")\ncompose_prompt(d)\n";
    let (code, msg) = run(&w.0, src);
    assert_eq!(
        code, 0,
        "json.parse 读来的 `Int` budget 应被接受 —— 修前报 \
         「budget must be string or number, got int」。实得: {msg}"
    );
}

/// ② 对照组：同一段 JSON 但 budget 是**字符串**（`"1KB"`）—— 本就支持，
/// 用来证明 ① 不是因为「所有 budget 都被接受」而恒绿。
#[test]
fn d263_control_group_string_budget_still_works() {
    let w = work("ctl");
    let src = "let d = json.parse(\"{\\\"text\\\":\\\"x\\\",\\\"budget\\\":\\\"1KB\\\"}\")\ncompose_prompt(d)\n";
    let (code, msg) = run(&w.0, src);
    assert_eq!(code, 0, "字符串 budget 本就应被支持（对照组失效）: {msg}");
}

/// ③ 错误信息必须用**类型名**且稳定跨进程（D262 同款 `{:?}` 问题）。
///
/// `{:?}` 对 `Value::Dict` 按 HashMap 迭代序打印，每进程随机 ⇒ 同一 dict
/// 的错误信息跨进程键序不同。断言错误信息**不含** `Dict {` 这类 Debug 形态。
#[test]
fn d263_budget_error_uses_type_name_not_debug() {
    let w = work("err");
    // budget 是一个非法类型（list），走 `other` 分支。
    let src = "let d = json.parse(\"{\\\"text\\\":\\\"x\\\",\\\"budget\\\":[1,2]}\")\ncompose_prompt(d)\n";
    let (code, msg) = run(&w.0, src);
    assert_ne!(code, 0, "list 类型的 budget 应被拒");
    assert!(
        msg.contains("must be string or number"),
        "错误信息应说明期望什么: {msg}"
    );
    assert!(
        !msg.contains("Dict {") && !msg.contains("List(") && !msg.contains("Float("),
        "错误信息里出现了 `Debug` 形态（键序跨进程不稳定）: {msg}"
    );
    assert!(msg.contains("list"), "错误信息应给出**类型名** list: {msg}");
}
