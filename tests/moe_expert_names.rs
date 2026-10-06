//! v0.104.6 D270：`orchestrate moe` 专家名在 witness 往返中**被丢弃**
//! ⇒ **MoE 100% 不可用**（已修）
//!
//! ## 修前实测（真实 CLI）
//!
//! ```mora
//! let input = 10
//! orchestrate moe input -> result
//!   experts: { "e1": fn (x) { x + 1 }, "e2": fn (x) { x + 100 } }
//!   top_k: 2
//!   router: fn (x) { { "e1": 1.0, "e2": 3.0 } }
//! end
//! print(result)
//! ```
//!
//! | | 修前 | 修后 |
//! |---|---|---|
//! | stdout | `Runtime error (MIR): moe: router referenced unknown expert 'e2'` | `85.25` |
//! | 退出码 | **1** | **0** |
//!
//! `85.25` 正是 `combine_moe_outputs` 文档里写的加权公式：
//! `0.25×11 + 0.75×110`（权重 = `scoreᵢ / Σscore`）。
//!
//! ## 缺陷：不是「MoE 写错了」，是**名字在中间层蒸发**
//!
//! `orchestrate moe` 此前**没有任何程序能跑通** —— 无论写几个专家、router
//! 怎么打分。原因在 parser → witness → MIR 的**往返**上：
//!
//! | # | 位置 | 发生了什么 |
//! |---|---|---|
//! | 1 | `parser_v3/syntax.rs:332` | 解析器**正确**造出 `MirMoeExpert { name, def, def_fn }` |
//! | 2 | `syntax.rs:522` | `WitnessOrchestrateKind::from_kind(&kind)` |
//! | 3 | **`witness.rs:602`** | `experts: experts.iter().map(\|e\| e.def.clone())` —— **只搬 `def`，`name` 在此丢失**；且 `WitnessOrchestrateKind::Moe` 的 `experts` 字段类型是 `Vec<MirWitness>`，**根本没有地方放名字** |
//! | 4 | **`orchestrate/mod.rs:203`** | 反向转换硬编码 `name: String::new()` |
//! | 5 | `runtime.rs:662` | `experts.iter().find(\|e\| &e.name == name)` 拿 router 的键去查一张全叫 `""` 的表 → **永远落空** |
//!
//! 编译产物可直接印证（修前）：
//!
//! ```text
//! experts: [MirMoeExpert { name: "", def: … }, MirMoeExpert { name: "", def: … }]
//! ```
//!
//! 而 router 返回的 `Dict([("e1", 1.0), ("e2", 3.0)])` **键名完好** ——
//! 说明丢的不是字符串字面量能力，是**结构里没有承载名字的字段**。
//!
//! ## 同族对照：agent 侧一直是对的
//!
//! `WitnessAgentDef`（`witness.rs`）带 `name: String`，agent 名一路无损。
//! 唯独 MoE 在 v0.92 的 witness 迁移里退化成了裸 `MirWitness`。
//!
//! ## 为什么零测试抓到
//!
//! 普查：`orchestrate moa` / `orchestrate moe` 在 tests、fixtures、examples 里
//! **各 0 次出现**（`orchestrate pregel` 17 次、`sequential` 10、`graph` 5）。
//! 一个 100% 不可用的特性，恰恰因为**没人写过一行**而完全不可见。

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

/// 两个数值专家 + 打分 router，`top_k: 2` ⇒ 加权组合。
fn weighted(top_k: u32) -> Result<mora::value::Value, String> {
    let src = format!(
        "let input = 10\n\
         orchestrate moe input -> result\n\
         \x20 experts: {{ \"e1\": fn (x) {{ x + 1 }}, \"e2\": fn (x) {{ x + 100 }} }}\n\
         \x20 top_k: {top_k}\n\
         \x20 router: fn (x) {{ {{ \"e1\": 1.0, \"e2\": 3.0 }} }}\n\
         end\n\
         result\n"
    );
    run(&src)
}

/// **D270 主断言**：MoE 必须真的跑得起来，且数值精确。
///
/// 修前：`Err("moe: router referenced unknown expert 'e2'")`。
#[test]
fn d270_moe_weighted_combo_runs_and_is_exact() {
    let v =
        weighted(2).unwrap_or_else(|e| panic!("MoE 应跑通（修前报 unknown expert），实际：{e}"));
    // e1 = 10+1 = 11（分 1.0）、e2 = 10+100 = 110（分 3.0）
    // 权重 = score/Σscore = 1/4、3/4 ⇒ 0.25*11 + 0.75*110 = 85.25
    let got: f64 = match v {
        mora::value::Value::Float(f) => f,
        other => panic!("期望数值结果，实际：{other:?}"),
    };
    assert!(
        (got - 85.25).abs() < 1e-9,
        "加权组合应为 85.25（0.25×11 + 0.75×110），实际：{got}"
    );
}

/// `top_k: 1` ⇒ **只**激活分数最高的 e2 ⇒ 110（未加权）。
///
/// 这是主断言的对照组：证明 top-k 稀疏门控本身也正确，
/// 而不只是「专家名能找到了」。
#[test]
fn d270_moe_top_k_selects_only_the_highest_scoring_expert() {
    let v = weighted(1).unwrap_or_else(|e| panic!("MoE 应跑通，实际：{e}"));
    let got: f64 = match v {
        mora::value::Value::Float(f) => f,
        other => panic!("期望数值结果，实际：{other:?}"),
    };
    assert!(
        (got - 110.0).abs() < 1e-9,
        "`top_k: 1` 只应激活 e2（分 3.0 > 1.0）⇒ 110，实际：{got}"
    );
}

/// **根因判据**：专家名必须在 witness 往返中存活。
///
/// 上两条测的是**症状**（跑不跑得起来、数值对不对）；本条直接钉住**机制** ——
/// 编译产物里 `MirMoeExpert.name` 必须等于源码里写的名字。
///
/// 修前这里全是 `name: ""`。
#[test]
fn d270_expert_names_survive_the_witness_roundtrip() {
    let src = "let input = 10\n\
               orchestrate moe input -> result\n\
               \x20 experts: { \"alpha\": fn (x) { x + 1 }, \"beta\": fn (x) { x + 2 } }\n\
               \x20 top_k: 2\n\
               \x20 router: fn (x) { { \"alpha\": 1.0, \"beta\": 2.0 } }\n\
               end\n\
               result\n";
    let (func, _w) = ParserV3::compile(src).expect("compile");
    let dump = format!("{func:?}");
    for name in ["alpha", "beta"] {
        assert!(
            dump.contains(&format!("MirMoeExpert {{ name: \"{name}\"")),
            "专家名 `{name}` 必须在编译产物里存活 —— D270 修前全被丢成 `name: \"\"`"
        );
    }
}

/// 对照组：router 引用**确实不存在**的专家时，仍须报原本那条错。
///
/// 这条保证主断言不是「碰巧能跑」—— 错误消息必须继续为它本来的目的服务。
#[test]
fn d270_router_naming_a_truly_unknown_expert_still_errors() {
    let src = "let input = 10\n\
               orchestrate moe input -> result\n\
               \x20 experts: { \"e1\": fn (x) { x + 1 } }\n\
               \x20 top_k: 1\n\
               \x20 router: fn (x) { { \"ghost\": 1.0 } }\n\
               end\n\
               result\n";
    let err = run(src).expect_err("router 引用未定义专家应报错");
    assert!(
        err.contains("unknown expert") && err.contains("ghost"),
        "错误应指明 router 引用了哪个未定义专家。实际：{err}"
    );
}
