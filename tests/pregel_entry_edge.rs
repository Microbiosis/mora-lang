//! v0.104.6 D97：`orchestrate pregel` 漏写入口边 → **静默返回 `nil`**（已修）
//!
//! ## 实测
//!
//! ```mora
//! orchestrate pregel input -> result
//!   agent a => "A"
//!   agent b => "B"
//!   edge a -> b            -- ← 没有 edge @start -> a
//! end
//! print(result)            -- → nil
//! ```
//!
//! `exit 0`、**零诊断**。补上 `edge @start -> a` 后一切正常：
//!
//! | 图 | 补入口边后的 `result` |
//! |---|---|
//! | `a`（单点） | `A` ✓ |
//! | `a -> b`（链） | `B` ✓ |
//! | `a -> {b, c}`（菱形） | `C` ✓ |
//!
//! ## 机制
//!
//! `pregel::MirPregelEngine::run` 的 `active_nodes` 初始值是 `vec!["@start"]`，
//! 下一跳沿 `edges` 从 **`active_nodes`（含 `@start`）** 计算。所以没有
//! `@start -> a` 的边时 `to_execute` 恒空，**没有任何 agent 跑过**，
//! `run()` 末尾 `channels.get("result").unwrap_or(Nil)` 就返回 `Nil`。
//!
//! `mir/handlers/runtime.rs` 的 `MirOrchestrateKind::Pregel` 分支**原样透传**
//! witness 里的 edges，**不补** `@start`（MoA 那条路径才补，见 `runtime.rs:905`）。
//!
//! ## 归类：静默错值族
//!
//! 与 D1（`range` 实参类型不对 → 静默取默认值）、D28（静默返回首元素）、
//! D39（`with` 未知键 → 静默丢弃）、D45（块内解析失败 → 静默跳过）同族：
//! **失败模式不是崩溃或报错，而是悄悄给出一个错的（`nil`）结果。**
//!
//! ## 修复（本轮已加守卫）
//!
//! 守卫条件是「**声明了 agent 却一次都没被调度**」，刻意**不含**可达性分析：
//! 无论原因是漏写入口边、边指向未知节点、还是所有 agent 都被 Halt，
//! 都属错配。判据用 `scheduled`（PLAN 阶段累计的被调度数）而非 `stats.agents_run`
//! —— 后者**不计**增量缓存的跳过路径，而「被调度但 input 未变而跳过」
//! 是**合法**的生产路径（见 `pregel::tests::incremental_skip_when_input_unchanged`）。
//! 该路径仍被 `tests/orchestrate_v3_pipeline.rs` 覆盖（它断言 `result == "world"`）。
//!
//! 另有一条对照组：图里有**完全孤立**的 agent 时**不得**误报 ——
//! 只要有 agent 被调度过就算配好了。
//!
//! 另外 `orchestrate_v3_pipeline.rs::v3_orchestrate_pregel_runs` 是本形态
//! **唯一**的端到端测试，它原先只 `assert!(result.is_ok())` 且程序**没有入口边**
//! —— 即它测的那段代码从未真正执行过。现已补入口边并断言 `result` 的值。

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use std::sync::Arc;

fn run(source: &str) -> (Result<mora::value::Value, String>, bool) {
    let (func, _w) = ParserV3::compile(source).expect("compile");
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    let r = run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    );
    let ok = r.is_ok();
    (r, ok)
}

/// **D97 主断言**：漏写 `edge @start -> a` 时必须**报错**，
/// 而不是静默返回 `nil`。
///
/// 修前：exit 0、零诊断、结果 `Nil`（等于告诉用户「这张图跑过了」）。
/// 修后：`pregel: graph declares agents but none was ever scheduled`。
///
/// 守卫条件刻意**不含**可达性分析：「声明了 agent 却一次都没被调度」本身就是错配。
/// 判据用 `scheduled`（被调度数）而非 `stats.agents_run` —— 后者**不计**增量
/// 缓存的跳过路径，而「被调度但 input 未变而跳过」是**合法**生产路径。
#[test]
fn d97_missing_start_edge_is_an_error_not_a_silent_nil() {
    let (res, _ok) = run(r#"
orchestrate pregel input -> result
  agent a => "A"
  agent b => "B"
  edge a -> b
end
result
"#);
    match res {
        Err(e) => assert!(
            e.contains("none was ever scheduled") && e.contains("edge @start"),
            "错误消息应点明「无 agent 被调度」并给出修法。实际：{e}"
        ),
        Ok(v) => panic!(
            "漏写入口边应**报错**；却静默返回了 {v:?} —— D97 已修，若本测试失败说明守卫被移除"
        ),
    }
}

/// 对照组：补上 `edge @start -> a` 后链路正常产出**最后** agent 的值。
#[test]
fn d97_with_start_edge_the_graph_actually_runs() {
    let (res, ok) = run(r#"
orchestrate pregel input -> result
  agent a => "A"
  agent b => "B"
  edge @start -> a
  edge a -> b
end
result
"#);
    assert!(ok, "程序应跑通");
    assert_eq!(
        format!("{:?}", res.unwrap()),
        "String(\"B\")",
        "对照组：有入口边时 result 应是链尾 agent 的值"
    );
}

/// 对照组：图里有一个**完全孤立**的 agent（无任何边指向它）时**不得**报错 ——
/// 只要有 agent 被调度过就算配好了。
#[test]
fn d97_orphan_agent_does_not_trigger_the_guard() {
    let (res, ok) = run(r#"
orchestrate pregel input -> result
  agent a => "A"
  edge @start -> a
  agent z => "Z"
end
result
"#);
    assert!(ok, "存在孤立 agent 不应误报（a 已被调度）。得到：{:?}", res);
    assert_eq!(format!("{:?}", res.unwrap()), "String(\"A\")");
}
