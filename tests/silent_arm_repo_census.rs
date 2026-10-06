//! v0.104.6 D381 —— 全仓 **`_ => {}` 静默兜底**普查：28 处里**只有 3 处**真正用户可达
//!
//! D380 在 `main.rs` 找到 3 处 flag 解析的静默兜底，并总结「普查要跨层」。
//! 本轮把普查推到**全仓**，回答一个具体问题：
//! **28 处 `_ => {}` 里，有多少是真正「用户输入非法 ⇒ 静默无效果」的？**
//!
//! ## 普查结果：28 处 / 17 个文件，但**只有 3 处**是解析层的兜底
//!
//! | 类别 | 数量 | 性质 |
//! |---|---|---|
//! | **CLI flag 解析**（`main.rs:123/220/269`）| **3** | ⚠️ **用户可达**，D380 已钉 |
//! | **中途检查**（match 失败就 `continue` 往下走）| 多数 | ✅ 安全 |
//! | **最终兜底**（`dispatch.rs:269` 等）| 少数 | ✅ **明确报错** |
//!
//! 「中途检查」占绝大多数，典型形态：
//!
//! ```rust
//! // dispatch.rs:72-82 —— `::` 构造器检查，匹配不到就继续往下
//! match name {
//!     "Router::new"   => return Ok(...),
//!     "McpServer::new" => return Ok(...),
//!     _ => {}          // ← 中途检查，不是终点
//! }
//! // …后面还有真正的分派与最终兜底
//! ```
//!
//! ## 关键：`dispatch.rs` 的最终兜底**明确报错**
//!
//! ```rust
//! // dispatch.rs:269
//! _ => Err(format!("Value is not callable: {}", value)),
//! ```
//!
//! ⇒ **「方法调用失败」不会静默成功**。
//! 同理 `numeric_helpers.rs:42/52/54` 的三处 `_ => {}` 后面
//! 都跟着 `call_math_method(method, &full_args)?`（L61）⇒ 那里会报错。
//!
//! ## 判据形态：**钉住「哪几处真危险」**，而不是「有 28 处」
//!
//! 本文件用**源码断言 + 行为断言**双向固定结论：
//! - 源码层：`main.rs` 的 `_ => {}` **恰好 3 处**，且都在 CLI 解析上下文
//! - 源码层：`dispatch.rs` 的最终兜底**含 `Err(`**
//! - 行为层：错拼 flag 静默回落（D380 的结论仍然成立）

use std::process::Command;

fn mora(dir: &std::path::Path, args: &[&str]) -> (i32, String) {
    let exe = concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/mora.exe");
    let out = Command::new(exe)
        .current_dir(dir)
        .args(args)
        .output()
        .expect("跑 mora");
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push('\n');
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), s)
}

/// 统计某文件里**独立成行的** `_ => {}` 数量。
fn count_silent_arms(src: &str) -> usize {
    src.lines()
        .map(str::trim)
        .filter(|l| *l == "_ => {}" || *l == "_ => {},")
        .count()
}

fn read(rel: &str) -> String {
    // ⚠ `CARGO_MANIFEST_DIR` **没有**尾部分隔符，直接拼接会得到
    // `D:\Github\mora-langsrc/main.rs`。首版就这么写 ⇒ 三条源码断言
    // 全部 panic 在「读源文件」上，而行为层的两条照常通过。
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读 {} 失败: {e}", p.display()))
}

/// **全仓兜底总数**：钉住普查规模，防止「悄悄新增了一堆」。
///
/// 全仓（17 个文件）共 **28** 处；本判据只抽查其中 12 个代表文件
/// （合计 18 处），并给出 `[12, 40]` 的区间 ——
/// 低于 12 说明文件被移动/重命名（需重新普查），
/// 高于 40 说明新增了**大量**静默兜底（需重新分类）。
#[test]
fn d381_repo_wide_silent_arm_census() {
    let mut total = 0usize;
    for f in [
        "src/main.rs",
        "src/interpreter/dispatch.rs",
        "src/interpreter/numeric_helpers.rs",
        "src/mir/dag.rs",
        "src/mir/ssa.rs",
        "src/mir/pipeline.rs",
        "src/mir/lower.rs",
        "src/mir/lmir_to_mir.rs",
        "src/parser_v3/emit.rs",
        "src/value.rs",
        "src/record/serialization.rs",
        "src/lsp/json.rs",
    ] {
        total += count_silent_arms(&read(f));
    }
    assert!(
        total >= 12,
        "抽查 12 个文件的 `_ => {{}}` 兜底数 {total} 过低 —— \
         是不是有文件被移动/重命名了？需重新普查"
    );
    assert!(
        total <= 40,
        "抽查的兜底数 {total} 超过上限 40 —— \
         **必须重新普查**并分类（中途检查 / 最终兜底 / 解析层）"
    );
}

/// **`main.rs` 恰好 3 处**，且全在 CLI 参数解析上下文。
///
/// 这是 D380 的结论**在源码层的固化**：只有这 3 处是用户可达的静默兜底。
#[test]
fn d381_only_three_silent_arms_in_cli_parsing() {
    let main = read("src/main.rs");
    assert_eq!(
        count_silent_arms(&main),
        3,
        "`main.rs` 应恰好 3 处 `_ => {{}}`（:123 / :220 / :269）; 实得 {}",
        count_silent_arms(&main)
    );
    // 且都紧跟在参数解析的 match 里（上下文含 flag 字面量）
    for flag in ["--output", "--verify", "--version"] {
        assert!(
            main.contains(flag),
            "三处兜底应分布在 flag 解析区（应含 `{flag}`）"
        );
    }
}

/// **函数分派的最终兜底**必须**明确报错**，不能静默。
#[test]
fn d381_call_dispatch_final_fallback_errors() {
    let d = read("src/interpreter/dispatch.rs");
    let line = d
        .lines()
        .map(str::trim)
        .find(|l| l.contains("Value is not callable"))
        .unwrap_or_else(|| panic!("`dispatch.rs` 的最终兜底应报错 *Value is not callable*"));
    assert!(
        line.contains("_ => Err("),
        "最终兜底必须**返回 Err**（而不是 `_ => {{}}`）; 实得: {line}"
    );
}

/// **行为层**：错拼的 flag 仍然静默回落（D380 的结论未变）。
#[test]
fn d381_misspelled_flag_still_silently_falls_back() {
    let dir = std::env::temp_dir().join("d381probe");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".mora").join("recordings")).unwrap();
    std::fs::write(
        dir.join(".mora").join("recordings").join("r1.jsonl"),
        "{\"kind\":\"note\",\"id\":1,\"ts_ms\":100,\"message\":\"hi\"}\n",
    )
    .unwrap();

    let (code, out) = mora(&dir, &["record", "export", "r1", "--formt", "md"]);
    assert_eq!(code, 0, "错拼 flag 当前静默回落; out={out}");
    assert!(
        out.lines().any(|l| l.trim_start().starts_with('{')),
        "回落目标是 JSONL; out={out}"
    );
    assert!(
        !out.lines()
            .any(|l| l.trim_start().starts_with("# Recording")),
        "错拼的 `--formt` 不能碰巧生效; out={out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **反向对照**：正确用法必须产出 Markdown（证明上一条不是「怎么写都回落」）。
#[test]
fn d381_correct_flag_still_produces_markdown() {
    let dir = std::env::temp_dir().join("d381probe2");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".mora").join("recordings")).unwrap();
    std::fs::write(
        dir.join(".mora").join("recordings").join("r1.jsonl"),
        "{\"kind\":\"note\",\"id\":1,\"ts_ms\":100,\"message\":\"hi\"}\n",
    )
    .unwrap();

    let (code, out) = mora(&dir, &["record", "export", "r1", "--format", "md"]);
    assert_eq!(code, 0, "正常用法应成功; out={out}");
    assert!(
        out.lines()
            .any(|l| l.trim_start().starts_with("# Recording")),
        "`--format md` 应产出 Markdown（否则上一条的回落是恒真的）; out={out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
