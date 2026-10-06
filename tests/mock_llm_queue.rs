//! v0.104.6 D91：`with mock_llm` 的响应队列**从未被消费** —— spec §19.4 承诺失效。
//!
//! ## spec 的承诺（`docs/mora-spec.md` §19.4，原文示例）
//!
//! ```mora
//! with mock_llm = ["response 1", "response 2"]
//!   let r1 = ai.chat("first")   -- 返回 "response 1"
//!   let r2 = ai.chat("second")  -- 返回 "response 2"
//! end
//! ```
//!
//! ## 修前的实际行为
//!
//! `interpreter/ai_chat.rs::do_ai_chat` 在 `api_key.is_empty()`（即 **mock 模式**
//! —— 恰恰是 `mock_llm` 唯一有意义的场景）时**提前 return**：
//!
//! ```rust
//! if api_key.is_empty() {
//!     eprintln!("[ai.chat mock — ...]");
//!     return Ok(Value::String(format!("[Mock response for: {}]", prompt)));   // ← 直接返回
//! }
//! ```
//!
//! 而消费队列的逻辑（`responses.remove(0)`）在**更深一层**的 `real_ai_chat_inner`
//! 里 —— mock 模式**根本走不到**。
//!
//! 后果：`with mock_llm = ["hello from mock"]` 被**静默忽略**；且该提前 return
//! 也**不录 `ai.chat` 事件**，于是 `mora record` 帮助文本宣称的
//! "Record ai.chat/web.fetch" 在 mock 模式下录不到任何 ai.chat 事件。
//!
//! ## 为什么没被发现
//!
//! 既有测试**全部只验语法与存储**，没有一条验「队列真被消费」：
//! `mir_dyntrait.rs:122-145`（块能 parse、binding 在 `WithConfig` 里）、
//! `with_config.rs:78-128`（`mock_llm` / `mock_responses` 别名被识别）。
//! **测了「配置存进去了」，没测「配置被用上了」。**
//!
//! ## 为什么用 CLI 子进程而不是 `assert_source_ok`
//!
//! `with` 块的 `let` 绑定**不外泄**（`h_with_config` 用 `child_env = env.clone()`
//! 跑 body 且从不并回），所以库路径的「末表达式」观测不到块内的值。
//! `print` 写在块内则正常输出，故走真实 CLI 捕获 stdout。

use std::process::Command;

/// 跑一段 `.mora` 源码，返回 `(exit_code, stdout)`。
///
/// ⚠ `tag` 必须**每条测试唯一**：cargo 默认并行跑测试，若共用同一个临时文件
/// 路径，各子进程会互相覆盖对方刚写下的源码（本人第一版就踩了，表现为
/// 「修复没生效」的假象）。
fn run(tag: &str, src: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join("mora_d91_mock_llm");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let prog = dir.join(format!("{tag}.mora"));
    std::fs::write(&prog, src).expect("write case");

    let out = Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg("run")
        .arg(&prog)
        // 显式清空：测试机若恰好配了 key，会走进真实 API 分支而非 mock
        .env_remove("OPENAI_API_KEY")
        .output()
        .expect("run mora");

    let _ = std::fs::remove_file(&prog);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}

/// 取程序输出（横幅行带前导空格、顶格行才是程序输出）。
fn program_output(stdout: &str) -> String {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with(char::is_whitespace))
        .filter(|l| !l.starts_with("Mora v"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// spec §19.4 的两响应示例：队列按顺序逐个吐出。
#[test]
fn d91_mock_llm_queue_is_consumed_in_order() {
    let (code, out) = run("order", "
        with mock_llm = [\"response 1\", \"response 2\"]\n  print(ai.chat(\"first\"))\n  print(ai.chat(\"second\"))\nend\n",
    );
    let got = program_output(&out);
    assert_eq!(code, 0, "程序应跑通。stdout:\n{out}");
    assert_eq!(
        got, "response 1 response 2",
        "spec §19.4：`mock_llm` 队列应按序返回两个注入响应"
    );
}

/// 单个响应（最小形态）。
#[test]
fn d91_mock_llm_single_response_is_used() {
    let (code, out) = run(
        "single",
        "
        with mock_llm = [\"hello from mock\"]\n  print(ai.chat(\"greeting\"))\nend\n",
    );
    let got = program_output(&out);
    assert_eq!(code, 0, "程序应跑通。stdout:\n{out}");
    assert_eq!(
        got, "hello from mock",
        "注入的单个 mock 响应应被采用，而不是通用占位符"
    );
}

/// 队列耗尽后应回落到通用占位符（确认修复没把兜底路径删掉）。
#[test]
fn d91_exhausted_queue_falls_back_to_placeholder() {
    let (code, out) = run(
        "exhaust",
        "
        with mock_llm = [\"only one\"]\n  print(ai.chat(\"a\"))\n  print(ai.chat(\"b\"))\nend\n",
    );
    let got = program_output(&out);
    assert_eq!(code, 0, "程序应跑通。stdout:\n{out}");
    assert_eq!(
        got, "only one [Mock response for: b]",
        "队列用尽后应回落到原有占位符兜底"
    );
}

/// 不设 `mock_llm` 时行为不变 —— 这条**在修前就通过**，是防回归的对照。
#[test]
fn d91_without_mock_llm_still_falls_back() {
    let (code, out) = run("fallback", "print(ai.chat(\"greeting\"))\n");
    let got = program_output(&out);
    assert_eq!(code, 0, "程序应跑通。stdout:\n{out}");
    assert_eq!(
        got, "[Mock response for: greeting]",
        "未设 mock_llm 时应保持原有的通用占位符兜底"
    );
}
