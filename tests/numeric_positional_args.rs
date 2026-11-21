//! v0.104.6 D150：可选**数值**实参被静默丢弃（已修）。
//!
//! ## 缺陷：`if let Some(Value::Float(n)) = args.get(i)` 只认一种数字表示
//!
//! Mora 的数字**字面量**是 `Float`（D98），`Value::Int` 只由 `len()` 等产生。
//! 于是这类写法把「不是 Float」统统当成「没传」：
//!
//! ```mora
//! schedule.add("j", "every", "tick", len([1, 2, 3]))   // interval_s 变 0
//! ccr.marker("abcdef", len([…8 项]))                    // size 变 0
//! mora.refine(p, "i", len([1, 1]))                     // 返回类型 List → Dict
//! mora.refine_info(p, len([1]))                        // 查第 1 轮 → 返回最新轮
//! tea.run(app, 3)                                      // 上限变 1000（反向极性！）
//! ```
//!
//! 全部 **exit 0、零诊断**。
//!
//! ## `tea.run` 是**反向**的同一个缺陷
//!
//! 它只认 `Value::Int`，于是**最自然**的 `tea.run(app, 3)` 反而失效 ——
//! 实测（队列经 `Cmd::Dispatch` 每轮回流、永不空，故轮数 = count）：
//!
//! | 调用 | 修复前 | 修复后 |
//! |---|---|---|
//! | `tea.run(a, 3)` | count **1000** | count 3 |
//! | `tea.run(a, 10)` | count **1000** | count 10 |
//! | `tea.run(a, len([1,1,1]))` | count 3 | count 3 |
//!
//! ## 同族里早就有人做对了
//!
//! **历史** `ai.retry` 的 `backoff_ms` **Int/Float 都认**（**历史**
//! `src/interpreter/builtins/ai.rs`）—— 即「同族里有人做了 → 破例的是疏漏」。
//! 那条正确做法曾收成 `builtins::optional_num_arg` 一处；
//! **v0.110.0 起 `ai` 模块与该辅助函数均已移除**，数值实参改由
//! `runtime/builtins/mod.rs::required_num_arg` 统一收口。
//!
//! ## 记档（**未改**）：负数不在本轮收紧
//!
//! 6 处的负数语义各不相同（`as usize` 饱和到 0 / `as u64` 饱和到 0），
//! 且 `schedule.add` 的负数会撞上下游本就诚实的
//! `Every kind needs interval_s > 0` 报错 —— 属**另一类**问题，按
//! 「收紧一类错误，别顺手改另一类」记档待决。

use mora::parser_v3::ParserV3;
use mora::runtime::CoreRuntime;
use mora::runtime::vm::execute_program;
use std::sync::Arc;

fn run(src: &str) -> Result<String, String> {
    let (prog, _w) = ParserV3::compile(src).map_err(|e| format!("COMPILE: {e}"))?;
    let mut core = CoreRuntime::default();
    let arc = Arc::new(prog);
    execute_program(arc, &mut core)
        .map(|(v, _)| format!("{v}"))
        .map_err(|e| e.to_string())
}

/// 对照组 ②：非数字实参对 `exec.parallel` 的数值上限/超时参数必须报错。
#[test]
fn d150_non_numeric_argument_is_rejected() {
    for call in [
        "exec.parallel([\"echo hi\"], \"bad\", 1000)",
        "exec.parallel([\"echo hi\"], 1, \"bad\")",
    ] {
        let res = run(&format!("{call}\n"));
        assert!(
            res.is_err(),
            "[{call}] 非数字实参必须**报错**; 实际: {res:?}"
        );
    }
}

/// 对照组 ③：**必选**数值实参本来就全认（Int/Float/BigInt）—— 不得回退。
///
/// 这是**可从 Mora 触达**的正确样板（`math.rs::expect_number`）。
/// 缺陷只发生在**可选**实参上：那里被迫写 `if let`，把「没传」和「类型不对」
/// 混成了同一件事。
#[test]
fn d150_required_numeric_args_still_accept_int() {
    let out = run("math.pow(2, 10)\n").unwrap_or_else(|e| panic!("math.pow 不应报错: {e}"));
    assert!(
        out.contains("1024"),
        "`math.pow(2, 10)` 必须仍是 1024; 实得: {out}"
    );
    // Int 实参（`len()` 产物）也必须走通 —— 这正是必选路径已做对的那一侧
    let inty = run("math.sqrt(len([4, 9]))\n")
        .unwrap_or_else(|e| panic!("math.sqrt 的 Int 实参不应报错: {e}"));
    assert!(
        inty.contains("1.414"),
        "`math.sqrt(len([4,9]))`（Int 实参）必须工作; 实得: {inty}"
    );
}

// `ai` 模块已在 v0.110.0 移除，对应的 `ai.retry` 源码判据同步移除。
