//! v0.104.6 D360 —— `src/mir/handlers/` 指令 dispatch 层 + `orchestrate loop` 的
//! `max_rounds` 边界（否定轮，无产品变更）
//!
//! D359 修了 `Float ⊗ BigInt` 混算后，本轮转到它的**直接下游**：
//! `src/mir/handlers/`（指令 dispatch，6 个文件 2403 行）。
//!
//! ## 零 panic 点（生产代码）
//!
//! 全目录 `grep` 出的 3 处 `.expect` **全在 `#[cfg(test)]` 内**
//! （`effects.rs:630/651/659`，是测试自己编译样例用）。
//! 生产代码路径**零** `unwrap` / `expect` / `panic!` / `unreachable!` ——
//! 与 `src/mir/lower.rs`（1392 行，同样零 panic 点）一致，
//! 说明 MIR 两层有系统性的防御。
//!
//! ## `max_rounds` 的 `max(1)` 是**三处一致的设计**，不是漏写
//!
//! `parser_v3/syntax.rs` 有**三处**完全相同的模式：
//!
//! | 行号 | 参数 |
//! |---|---|
//! | L266 | `moa_layers` |
//! | L351 | `moe_top_k` |
//! | L385 | `max_rounds` |
//!
//! ```rust
//! TokenType::Float(f) => f.max(1.0) as u64,
//! TokenType::Int(i) => i.max(1) as u64,
//! ```
//!
//! `max_rounds: 0` → 1 轮、`2.7` → 2 轮，都是**静默**的。
//! 但：
//! ① **三处完全一致** ⇒ 是统一约定，不是某一处漏写；
//! ② 意图明确 —— `max(1)` 是**防零轮循环**的防护，
//! 与 D339 `tea.run` 的问题**方向相反**（那里是「静默归零导致不跑」，
//! 这里是「静默提升导致多跑一轮」，后者无害得多）；
//! ③ D269 已把 `max_rounds` 的值真正接到 `Loop.rounds`
//! （`loop_rounds.or(Some(1000))`，L454），接线完整。
//!
//! ⇒ **判定为设计，不修**。修它会同时改动三处统一约定，
//! 且「多跑一轮」与「不跑」相比危害小得多。
//!
//! ## 负例仍然正确报错（D269 的既有行为）
//!
//! `max_rounds: -5` / `max_rounds: "abc"` 都 exit 2 ——
//! `-` 不是字面量开头、`String` 不在 match 的两个 arm 里 → `return None`。

use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn slug(s: &str) -> String {
    let mut out = String::from("d360_");
    out.extend(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .take(40),
    );
    out
}

fn ev(body: &str) -> (i32, String) {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("d360_{}_{}", n, slug(body)));
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
                && !l.starts_with('⚠')
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

/// **装置自检**（D356 教训：先证装置有效，再看它测出的数据）。
#[test]
fn d360_harness_collects_print_output() {
    let (code, got) = ev("print(1)\n");
    assert_eq!(code, 0, "探针应正常退出; 实得 exit={code} out=[{got}]");
    assert_eq!(got.trim(), "1.0", "采集器失效（本文件全部断言依赖它）");
}

/// **源码层断言**：三个「轮数 / 层数 / top_k」参数用**同一套** `max(1)` 模式。
///
/// 这是本轮否定判的**核心依据** —— 判定「`max_rounds` 的 `max(1)`
/// 是设计而非漏写」的证据就是**三处一致**。
///
/// 脚本层测不出这个（`max_rounds: 0` 与 `: 5` 的可观测结果相同），
/// 所以必须用源码判据钉。
#[test]
fn d360_the_three_max_one_guards_stay_consistent() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/parser_v3/syntax.rs"
    ))
    .expect("读 syntax.rs");

    // `as usize` 两处（moa_layers / moe_top_k），`as u64` 一处（max_rounds）。
    // 三处的 `max(1)` 守卫形态一致，只是目标类型不同。
    let float_guard = src.matches("f.max(1.0) as usize").count();
    let int_guard = src.matches("i.max(1) as usize").count();
    assert_eq!(
        float_guard, 2,
        "应有 2 处 `f.max(1.0) as usize`（moa_layers / moe_top_k）; 实得 {float_guard}"
    );
    assert_eq!(
        int_guard, 2,
        "应有 2 处 `i.max(1) as usize`; 实得 {int_guard}"
    );

    // `max_rounds` 用的是 `as u64`（rounds 是 u64，与另两处的 usize 不同）
    assert_eq!(
        src.matches("f.max(1.0) as u64").count(),
        1,
        "`max_rounds` 应有 1 处 `f.max(1.0) as u64`"
    );
    assert_eq!(
        src.matches("i.max(1) as u64").count(),
        1,
        "`max_rounds` 应有 1 处 `i.max(1) as u64`"
    );
    assert!(
        src.contains("f.max(1.0) as u64") && src.contains("i.max(1) as u64"),
        "`max_rounds` 分支应仍在（rounds 是 u64，与另两处的 usize 不同）"
    );
}

/// **接线完整性**：`loop_rounds` 必须真正落到 `Loop.rounds` 上。
///
/// D269 修的就是这个（此前值被丢弃、`Loop.rounds` 硬编码 `Some(1000)`）。
/// 若将来有人把 `.or(Some(1000))` 改回去，这条会红。
#[test]
fn d360_max_rounds_is_actually_wired_to_loop_rounds() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/parser_v3/syntax.rs"
    ))
    .expect("读 syntax.rs");
    assert!(
        src.contains("rounds: loop_rounds.or(Some(1000))"),
        "`Loop.rounds` 必须用 `loop_rounds`（D269 修的接线）; 否则 max_rounds 又是静默无效"
    );
    // 且 `loop_rounds` 必须是真正被赋值的局部变量
    assert!(
        src.contains("let mut loop_rounds: Option<u64> = None;")
            && src.contains("loop_rounds = Some(match tok.token_type {"),
        "`loop_rounds` 的声明与赋值都必须在"
    );
}

/// **负例仍正确报错**：`max_rounds` 的非数字值必须被拒。
#[test]
fn d360_non_numeric_max_rounds_still_errors() {
    for (val, what) in [("-5", "负数"), ("\"abc\"", "字符串")] {
        let body =
            format!("orchestrate loop input -> result\n  max_rounds: {val}\nend\nprint(result)\n");
        let (code, got) = ev(&body);
        assert_eq!(
            code, 2,
            "`max_rounds: {val}`（{what}）应被拒（D269 的既有行为）; 实得 exit={code} out={got}"
        );
    }
}

/// **`orchestrate loop` 的合法写法不得被本轮改动破坏**。
///
/// 语法形态是 `orchestrate loop <input> -> <result>` 后跟字段块
/// （`max_rounds` / `on` / `agent` / edges），**没有** `do` 关键字。
#[test]
fn d360_orchestrate_loop_still_parses() {
    for body in [
        "orchestrate loop input -> result\n  max_rounds: 3\nend\nprint(result)\n",
        "orchestrate loop input -> result\nend\nprint(result)\n",
        "orchestrate loop input -> result\n  max_rounds: 0\nend\nprint(result)\n",
    ] {
        let (code, got) = ev(body);
        assert_eq!(
            code, 0,
            "合法 orchestrate loop 应正常跑; 实得 exit={code} out={got}"
        );
    }
}

/// **其它 orchestrate kind 不得被本轮改动破坏**。
#[test]
fn d360_other_orchestrate_kinds_still_parse() {
    for kind in ["sequential", "graph", "pregel"] {
        let body = format!("orchestrate {kind} input -> result\nend\nprint(result)\n");
        let (code, got) = ev(&body);
        assert_eq!(
            code, 0,
            "`orchestrate {kind}` 应正常跑; 实得 exit={code} out={got}"
        );
    }
}
