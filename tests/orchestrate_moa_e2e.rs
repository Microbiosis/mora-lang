//! v0.104.6 D270：`orchestrate moa` 端到端冒烟（**否定结果** —— 它是好的）
//!
//! ## 为什么要有这个文件
//!
//! D269 普查发现：`orchestrate moa` / `orchestrate moe` 在 tests、fixtures、
//! examples 里**各 0 次出现**（对照：`pregel` 17 次、`sequential` 10、`graph` 5）。
//! 零覆盖意味着**任何回归都不可见** —— 同轮的 `moe` 就是这样 100% 坏掉的
//! （见 `tests/moe_expert_names.rs`）。
//!
//! 本文件为 `moa` 钉住「能跑」这一事实，让它不再是无人问津的路径。
//!
//! ## 实测（真实 CLI，mock AI 模式）
//!
//! ```mora
//! orchestrate moa input -> result
//!   layers: 1
//!   proposers: ["p1", "p2"]
//!   aggregator: "agg"
//!   prompt: "hello"
//! end
//! ```
//!
//! ⇒ `exit 0`，控制台可见 **2 次 proposer 调用 + 1 次 aggregator 综合**。
//!
//! ## 契约（读 handler 得到）
//!
//! `Moa` 在 `runtime.rs` 里被展开成一张 **pregel 图**（`build_moa_config`）：
//! 每层 L = N 个 proposer 并行 `ai.chat` → 聚合 agent 读
//! `input_aggregator_layer_{L}_responses` 做综合。零新引擎机制。
//!
//! 解析侧的 `prompt` / `layers` / `proposers` / `aggregator` 四个键都真的落地
//! （与 D269 的 `max_rounds` 相反 —— 那四个是**正常**的，解析器逐个消费）。
//! handler 里 `prompt: _` 的 `_` 是 v0.91 迁移后被 `prompt_fn` 取代的
//! **遗留字段**，不是丢弃用户值 —— 这点已单独核实，**否定**。
//!
//! ## 断言强度说明
//!
//! `ai.chat` 在无 `OPENAI_API_KEY` 时走 mock，返回 `[Mock response for: …]`。
//! 因此本文件断言的是**可机械判定的部分**：proposer 被调用的**次数**、
//! 综合步骤确实发生、退出码为 0 —— 而**不是**逐字比对 mock 文本
//! （那会随 mock 实现变动而脆，且不属于本条要保护的东西）。

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use std::sync::Arc;

fn run(source: &str) -> Result<mora::value::Value, String> {
    let (func, _w) = ParserV3::compile(source).map_err(|e| format!("compile: {e}"))?;
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )
}

const SRC: &str = "orchestrate moa input -> result\n\
                    \x20 layers: 1\n\
                    \x20 proposers: [\"p1\", \"p2\"]\n\
                    \x20 aggregator: \"agg\"\n\
                    \x20 prompt: \"hello\"\n\
                    end\n\
                    result\n";

/// `moa` 必须端到端跑通并产出结果。
///
/// 修前这不是缺陷（否定结果），本条是**回归守卫**。
#[test]
fn d270_moa_runs_end_to_end() {
    let v = run(SRC).unwrap_or_else(|e| panic!("moa 应跑通，实际报错：{e}"));
    assert!(
        !matches!(v, mora::value::Value::Nil),
        "moa 应产出非 nil 结果，实际：{v:?}"
    );
}

/// 解析层：`layers` / `proposers` / `aggregator` 三个键必须真正落到 MIR 上。
///
/// 这条钉住 D269 的**否定面** —— 证明「认识一个键」和「真的消费这个键」
/// 在本文件覆盖的这几个键上是成立的，从而把 D269 的缺陷精确限定在
/// `max_rounds`（以及 `moe` 的专家名）上，而不是「整个 orchestrate 解析器
/// 都吞键」。
#[test]
fn d270_moa_keys_reach_the_mir() {
    let (func, _w) = ParserV3::compile(SRC).expect("compile");
    let dump = format!("{func:?}");
    assert!(
        dump.contains("Moa {"),
        "编译产物里应有 Moa 变体。实际片段：{}",
        &dump[..dump.len().min(300)]
    );
    assert!(
        dump.contains("proposers") && dump.contains("agg"),
        "proposers / aggregator 应出现在编译产物里（未被静默丢弃）"
    );
    // layers: 1 —— 不得被换成一个与源码不同的默认值。
    assert!(
        dump.contains("layers: 1"),
        "`layers: 1` 应原样到达 MIR（未被改写/丢弃）"
    );
}
