//! v0.104.6 D344 —— builtin 里**静默兜底**的全称普查（否定轮，无产品变更）
//!
//! D343 的收获是「**同模块内两种策略并存**」本身就是缺陷的判别信号。
//! 本条把它**全称执行**：`src/interpreter/builtins/**` 里每一处
//! 「解析失败 / 缺参 ⇒ 静默用默认值」都逐条查清并钉住。
//!
//! ## 普查结果：21 处，**0 个新增缺陷**
//!
//! 分三类：
//!
//! | 类别 | 数量 | 判定 |
//! |---|---|---|
//! | **合法的「缺参 ⇒ 默认」**（`args.get(1).cloned().unwrap_or(Value::Nil)`）| 8 | ✅ 设计如此 |
//! | **合法默认值**（缺参用 1000 / 2.0 / 0 / 1 / Nil）| 8 | ✅ 设计如此 |
//! | **解析失败 ⇒ 静默默认**（`s.parse().unwrap_or(N)`）| **1** | ⚠ 不可达，见下 |
//! | `_ => None`（类型不匹配 ⇒ 不认）| 4 | ✅ 正确 |
//!
//! ## 唯一值得说的那处：`ai.rs:30` 的 `s.parse().unwrap_or(1000)`
//!
//! ```rust
//! // backoff_ms（第 30 行）—— 解析失败静默用 1000：
//! Value::String(s) => s.parse().unwrap_or(1000),
//!
//! // attempts（第 18-19 行）—— 同一函数里是严格的：
//! s.parse().map_err(|_| format!("ai.retry: invalid attempts '{}'", attempts))?,
//! ```
//!
//! 形式上又是「同函数两种策略」，**但本条不改**，理由见下。
//!
//! ## 为什么 `ai.retry` 不改：它**源码不可达**
//!
//! parser 永远把 `ai.retry(...)` 解析成「裸名 `ai` + 方法 `retry`」，
//! 产不出单名 `"ai.retry"` ⇒ `call_ai_method` 里这整段代码**永不被执行**。
//! D59（`tests/ai_namespace_reachability.rs`）已把它钉成判据，本轮复跑 4 条全绿。
//!
//! ⚠ **这正是 D150 犯过的错**：那一轮把 `ai.rs` 的 `backoff_ms` 当成
//! 「同族里有人做对了」的样板，事后经 D59 复查才发现**根本不可达**。
//! ⇒ 「同模块两种策略并存」是**很强的信号**，但仍需先确认两边都**可触达**。
//! 判据见 `d344_ai_retry_backoff_stays_a_source_only_criterion`。
//!
//! ## 「合法默认值」这一类要特别小心 `optional_num_arg` 收口
//!
//! `optional_num_arg`（D150 建）的设计是「**缺失**与 `Value::Nil` 都算没传，
//! 其余**类型错报错**」。它与「静默兜底」的区别就在**类型错**那条边上：
//! `args.get(1).cloned().unwrap_or(Value::Nil)`（`tea.init` / `mock.register` /
//! `bus.emit` …）**不做类型检查** —— 传错类型就静默当 Nil。
//! 本条只钉「它们是设计如此」，**不**判定为缺陷（属 D150 记档的同一类决定）。

use std::fs;

/// 全部 `unwrap_or(` / `_ => <默认值>` 的确切行数下界（D344 普查基线）。
///
/// ⚠ 有了这个断言，判据才不会「静默通过 0 个」——
/// 那比红更危险（看起来在检查，实际什么都没扫到）。
#[test]
fn d344_silent_fallback_census_covers_at_least_21_sites() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/runtime/builtins");
    let mut total = 0;
    let mut files = 0;
    for e in fs::read_dir(dir).expect("读 builtins 目录").flatten() {
        let p = e.path();
        let s = p.to_string_lossy().to_string();
        if !s.ends_with(".rs") || s.contains("test") {
            continue;
        }
        files += 1;
        let src = fs::read_to_string(&p).expect("读源码");
        for line in src.lines() {
            let t = line.trim();
            if t.starts_with("//") {
                continue;
            }
            if t.contains(".unwrap_or(") || t.contains("_ => None") || t.contains("_ => Value::Nil")
            {
                total += 1;
            }
        }
    }
    assert!(
        files >= 6,
        "只扫到 {files} 个文件 —— builtins 目录结构变了？"
    );
    assert!(
        total >= 6,
        "只扫到 {total} 处「静默兜底」（v0.110.0 瘦身保留模块基线）。"
    );
    println!("d344: 扫过 {files} 个文件，{total} 处「静默兜底」");
}

/// **`required_num_arg` / `required_str_arg` 收口仍在，且明确对类型错报错**。
#[test]
fn d344_required_arg_collators_are_still_the_type_check_boundary() {
    let mod_rs = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/runtime/builtins/mod.rs"
    ))
    .expect("读 runtime/builtins/mod.rs");
    assert!(
        mod_rs.contains("pub(crate) fn required_num_arg"),
        "`required_num_arg` 收口应仍在"
    );
    assert!(
        mod_rs.contains("must be a number, got"),
        "收口应对**类型错**报错（这正是它与 `unwrap_or` 的分界）"
    );
    assert!(
        mod_rs.contains("pub(crate) fn required_str_arg"),
        "`required_str_arg` 孪生件应仍在"
    );
}
