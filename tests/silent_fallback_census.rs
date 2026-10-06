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
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/interpreter/builtins");
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
        files >= 20,
        "只扫到 {files} 个文件 —— builtins 目录结构变了？"
    );
    assert!(
        total >= 21,
        "只扫到 {total} 处「静默兜底」，当前基线是 **21**。\n\
         ⚠ 「全称判据静默通过 0 个」比红更危险 —— 请核对 `builtins/` 是否被重构过。\n\
         基线沿革：21（D340/D344 普查；D404 曾因误改 `ccr.marker` 降到 20，\n\
         随后**回退**那次误改 —— `ccr.marker` 的负尺寸饱和是 **D339 明确记录的\n         「不修、只钉现状 + 报告」的产品契约决定**，见 `tea_max_steps_guard.rs`。"
    );
    println!("d344: 扫过 {files} 个文件，{total} 处「静默兜底」（当前基线 21）");
}

/// **`ai.retry` 的 `backoff_ms` 兜底只作为**源码判据**存在**。
///
/// D150 曾把它当「同族里有人做对了」的样板，事后经 D59 才发现
/// **源码不可达**。本条把那个事实**钉住**，防止再被当成可触达样板。
///
/// 同时钉住 `ai` 裸名**只暴露 3 个方法**（`chat` / `critic` / `tokens`）——
/// 这正是「不可达」的机制来源。
#[test]
fn d344_ai_retry_backoff_stays_a_source_only_criterion() {
    let ai = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/interpreter/builtins/ai.rs"
    ))
    .expect("读 ai.rs");
    // 那处兜底**仍在源码里**（D150 只改了判据、没改代码，因为它不可达）
    assert!(
        ai.contains("Value::String(s) => s.parse().unwrap_or(1000),"),
        "ai.rs 的 `backoff_ms` 兜底应仍在源码里（D59 已钉它不可达，故不必修）。\
         若有人把它改了，本条会红 —— 那是**有意的**变更，请同步更新说明"
    );
    // 紧邻的 `attempts` 是严格的（形式上的「两种策略并存」，
    // 但**两边都不可达** ⇒ 不构成 D343 那样的判别信号）
    assert!(
        ai.contains("ai.retry: invalid attempts"),
        "`attempts` 的解析错误消息应仍在（它是同函数里的「严格」那一侧）"
    );
}

/// **`optional_num_arg` 收口仍在，且它才是「缺参 vs 类型错」的分界**。
///
/// 前面 8 处 `args.get(1).cloned().unwrap_or(Value::Nil)` **不做类型检查**
/// （传错类型 ⇒ 静默当 Nil）；而 `optional_num_arg` 会**报错**。
/// 两者并存是 D150 的**刻意**设计，本条钉住收口本身没被绕过。
#[test]
fn d344_optional_num_arg_collator_is_still_the_type_check_boundary() {
    let mod_rs = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/interpreter/builtins/mod.rs"
    ))
    .expect("读 builtins/mod.rs");
    assert!(
        mod_rs.contains("pub(crate) fn optional_num_arg"),
        "`optional_num_arg` 收口应仍在（D150 建、D152 补字符串孪生件）"
    );
    assert!(
        mod_rs.contains("must be a number, got"),
        "收口应对**类型错**报错（这正是它与 `unwrap_or(Nil)` 的分界）"
    );
    assert!(
        mod_rs.contains("pub(crate) fn optional_str_arg"),
        "`optional_str_arg` 孪生件应仍在（D152）"
    );

    // 使用点：D339 修的 `tea.run(max_steps)` 走的就是这个收口
    let tea = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/interpreter/builtins/tea.rs"
    ))
    .expect("读 tea.rs");
    assert!(
        tea.contains("optional_num_arg(args, 1, \"tea.run\", \"max_steps\")"),
        "`tea.run` 的 max_steps 应仍走 `optional_num_arg`（D339 的修法所在）"
    );
    assert!(
        tea.contains("tea.run: max_steps must be a non-negative number"),
        "`tea.run` 的负数守卫消息应仍在（D339 已加）"
    );
}
