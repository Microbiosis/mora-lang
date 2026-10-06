//! v0.104.6 D59：`ai.retry` / `ai.role` / `ai.dag` / `ai.heartbeat` 在源码里
//! **完全不可达**，而它们的单元测试全绿 —— 测试**直接调函数**、绕过了名字解析。
//!
//! ## 实测（真实 CLI `mora run`，三种拼法全试）
//!
//! ```text
//! ai.retry(3, 500, "fixed")     → Runtime error: Unknown method: AiChat.retry
//! let f = ai.retry ; f(3, 500, "fixed") → 同上
//! ai.dag / ai.role / ai.heartbeat      → 同上（Unknown method: AiChat.*）
//! ```
//!
//! ## 根因
//!
//! 名字解析把两条路分开了：
//!
//! | 入口 | 解析成 | 可用方法 |
//! |---|---|---|
//! | 裸名 `ai`（`value.rs:124`） | `BuiltinKind::AiChat` | `tokens` / `chat` / `critic` |
//! | 点号自由函数名 `"ai.retry"`（`dispatch.rs:522`） | `BuiltinKind::Ai` | `retry` / `role` / `dag` / `heartbeat` |
//!
//! 而 `call_ai_method`（`retry`/`role`/`dag`/`heartbeat` 的实现）的**唯一生产
//! 调用点**是 `method_dispatch.rs:744` 的 `(BuiltinKind::Ai, _)` —— 需要接收者
//! 是 `BuiltinKind::Ai`。但 parser 永远把 `ai.retry(...)` 解析成
//! 「裸名 `ai`（→ `AiChat`）+ 方法 `retry`」，**从不**产出单名 `"ai.retry"`。
//! 于是 `BuiltinKind::Ai` 在源码路径上不可达。
//!
//! `src/interpreter/builtins/tests/{ai,dag,heartbeat}.rs` 里的测试全部写成
//! `interp.call_ai_method("retry", …)` —— **直接调函数**，绕过了名字解析，
//! 于是它们测的是一个源码到不了的实现。与 D55 的
//! `list_get_exposes_element_type_error`、D56 的 `let_identity_polymorphic`
//! 同型：**测试用比生产更宽松的路径，于是绿色不可信。**
//!
//! ## 顺带：`Value::Stream` 是**从未被构造**的死变体
//!
//! spec §1099 承诺 `ai.stream(prompt) → stream`，但：
//!
//! * `interpreter/builtins/ai.rs` **没有** `"stream"` 分支；
//! * `Value::Stream` 全仓 6 处出现**全是 match 模式 / 类型名映射**，
//!   **没有任何构造点**；
//! * 它的方法 `collect` / `is_done` 已在 `value.rs:643` 登记、
//!   `method_dispatch.rs:40` 分派，但**没有任何值能到达那里**。
//!
//! 好消息：类型系统没有漏 —— `let x: stream = 1` 被 parser 正确拒绝
//! （`unsupported type annotation 'stream'`），`print(stream)` 报
//! `Unbound variable`。即 `Type::Stream` 的名字映射（`typeck/mod.rs:340`）
//! **不可从源码到达**。
//!
//! ## 为什么只记录不修
//!
//! 让它们可达 = 决定点号名 `ai.retry` 到底该被解析成「单名 + 自由函数调用」
//! 还是「裸名 `ai` + 方法调用」，**以及** `ai.stream` 要不要真的实现
//! （涉及网络流式读取）。这是语言文法与功能设计决定，未擅自做。

use std::sync::Arc;

use mora::interpreter::Interpreter;
use mora::mir::effect::Effects;
use mora::mir::vm::run_mir;

fn run(src: &str) -> Result<mora::value::Value, String> {
    let (func, witnesses) =
        mora::cli::compile_and_opt(src, None).map_err(|e| format!("COMPILE: {e}"))?;
    let errs = mora::typeck::check_mir::check_program_witnesses_bidirectional(&witnesses);
    if !errs.is_empty() {
        return Err(format!("TYPECK: {errs:?}"));
    }
    let arc = Arc::new(func);
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    run_mir(&arc, &mut interp, &mut env, &mut Effects::new())
}

/// `ai` 命名空间当前只暴露 `chat` / `critic` / `tokens` 三个方法。
#[test]
fn ai_namespace_exposes_only_chat_critic_tokens() {
    // 这三个可用
    assert!(run("ai.chat(\"hi\")\n").is_ok(), "`ai.chat` 应当可用");
    assert!(run("ai.critic(\"hi\")\n").is_ok(), "`ai.critic` 应当可用");
    assert!(run("ai.tokens(\"hi\")\n").is_ok(), "`ai.tokens` 应当可用");
}

/// `ai.retry` / `ai.role` / `ai.dag` / `ai.heartbeat` 在源码里**不可达**。
///
/// 若将来把它们接上了，本测试会失败并提示删掉 —— 它记录的是**当前事实**。
#[test]
fn ai_retry_role_dag_heartbeat_are_unreachable_from_source() {
    for (name, src) in [
        ("ai.retry(...)", "ai.retry(3, 500, \"fixed\")\n"),
        ("ai.dag", "ai.dag\n"),
        ("ai.role", "ai.role\n"),
        ("ai.heartbeat", "ai.heartbeat\n"),
        // 绕一层引用也一样
        (
            "let f = ai.retry; f(...)",
            "let f = ai.retry\nf(3, 500, \"fixed\")\n",
        ),
    ] {
        let e = run(src).expect_err(&format!(
            "[{name}] 当前应不可达（Unknown method: AiChat.*）"
        ));
        assert!(
            e.contains("Unknown method: AiChat"),
            "[{name}] 期望 `Unknown method: AiChat.*`（receiver 被解析成 AiChat），实得: {e}"
        );
    }
}

/// spec §1099 承诺的 `ai.stream` 未实现。
#[test]
fn ai_stream_is_not_implemented() {
    let e = run("ai.stream(\"hi\")\n").expect_err("`ai.stream` 当前未实现");
    assert!(e.contains("Unknown method: AiChat"), "实得: {e}");
}

/// `Value::Stream` 是死变体 —— 类型系统没有漏，parser 正确拒绝该标注。
#[test]
fn stream_type_annotation_is_rejected_not_silently_accepted() {
    for src in [
        "let x: stream = 1\n",
        "let x: stream = 1\nprint(x.collect())\n",
    ] {
        let e = run(src).expect_err("`stream` 不是可写的类型标注");
        // 详细诊断（`unsupported type annotation 'stream'`）走 stderr，
        // 库层 Err 只有「Failed to parse at line N」—— 故断言这一条。
        assert!(
            e.contains("Failed to parse"),
            "期望 parser 拒绝 `stream` 标注（CLI 上会打印 \
             `unsupported type annotation 'stream'`），实得: {e}"
        );
    }
}
