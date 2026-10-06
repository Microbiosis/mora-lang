//! v0.104.6 D271：`edge … on: <cond>` 的条件**完全无效** —— 边永远激活（已修）
//!
//! ## 修前实测（真实 CLI）
//!
//! ```mora
//! orchestrate graph input -> result
//!   agent a => "A"
//!   agent b => "B"
//!   @start -> a
//!   a -> b on: false      -- ← 恒假条件
//! end
//! print(result)
//! ```
//!
//! | 程序 | 修前 | 修后 |
//! |---|---|---|
//! | `a -> b on: false` | `B`（**b 被激活**） | `A`（b 不激活） ✓ |
//! | `a -> b on: true` | `B` | `B` ✓ |
//! | `a -> b on: gate == "yes"`，`gate="no"` | `B` | `A` ✓ |
//! | `a -> b on: gate == "yes"`，`gate="yes"` | `B` | `B` ✓ |
//!
//! ⇒ 修前 `false` 与 `true` **产出逐字相同** —— 条件对结果零影响。
//!
//! ## 缺陷：条件被写进了一个**没有任何运行时读者**的字段
//!
//! 这不是「某个字段丢了」，是**链路两端都没接上**：
//!
//! | # | 位置 | 状态 |
//! |---|---|---|
//! | 1 | `syntax.rs` `try_parse_edge_def` | 把 `on:` 的表达式存进 `condition_expr` ✓ **正确** |
//! | 2 | `orchestrate/mod.rs` `mir_edge_from_witness` | `condition_expr: None` ❗ **丢弃**（与 D270 的 MoE 专家名同型） |
//! | 3 | 解析器给 `condition_body` 赋值 | 恒为 `None` ❗ **从未预 lowering** |
//! | 4 | `pregel/mod.rs:496` / `:686`（引擎唯一两处条件求值） | **只读 `condition_body`** |
//!
//! ⇒ 即便第 2 步不丢，第 3 步也保证 `condition_body == None`，
//! 而第 4 步压根不看 `condition_expr`。`condition_expr` 在全仓**只有一个**
//! 读者：`witness.rs:521` 的 LSP witness walk（语义/折叠），**不是运行时**。
//!
//! ## 修法
//!
//! ① 解析器把条件**预 lowering** 成 `condition_body`（lowering 失败 `return None`
//!    走解析错误）—— 与同函数内 agent `task_body`、moe expert `def_fn` 同风格；
//! ② `mir_edge_from_witness` 把 `condition_expr` 与 `condition_body` 一并带过。
//!
//! `WitnessEdgeDef` 侧本来就**有**这两个字段（与 MoE 专家名不同：那边是
//! 结构里根本没地方放），所以②只是不再主动丢弃。
//!
//! ## 影响面
//!
//! `orchestrate graph` 与 `orchestrate pregel` **共用同一套边机制**，
//! 两者此前边条件都是死的。`orchestrate moa` 内部构造的 pregel 图同理
//! （`runtime.rs` 三处 `condition_body: None` 是它本来就没有条件边，不是缺陷）。
//!
//! 既有测试无一受影响：pregel 的 ~40 处单测都在 config 里**显式**写
//! `condition_body: None`；`orchestrate_graph_with_predicate_edges_parses`
//! 只断言「能解析」—— 与 D269 里那条 `max_rounds` 测试一样**无鉴别力**。
//!
//! **注**：`orchestrate pregel` 的边条件此前也无效，本条修复一并让它生效 ——
//! 这是一个面向用户可见的行为变更（从「条件被忽略」变成「条件被遵守」）。

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use mora::value::Value;
use std::sync::Arc;

fn run(source: &str) -> Result<Value, String> {
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

/// 造一张 `a -> b on: <cond>` 的图，`cond` 必须是布尔**常量**。
///
/// 用恒定常量做判别器：它不依赖任何变量播种，失败时只能是「条件没生效」，
/// 不会是探针写错。
fn graph_with_const_cond(cond: &str) -> Result<Value, String> {
    let src = format!(
        "orchestrate graph input -> result\n\
         \x20 agent a => \"A\"\n\
         \x20 agent b => \"B\"\n\
         \x20 @start -> a\n\
         \x20 a -> b on: {cond}\n\
         end\n\
         result\n"
    );
    run(&src)
}

/// 造一张 `a -> b on: gate == "yes"` 的图，`gate` 由 `gate_value` 播种。
fn graph_with_gate(gate_value: &str) -> Result<Value, String> {
    let src = format!(
        "let gate = \"{gate_value}\"\n\
         orchestrate graph input -> result\n\
         \x20 agent a => \"A\"\n\
         \x20 agent b => \"B\"\n\
         \x20 @start -> a\n\
         \x20 a -> b on: gate == \"yes\"\n\
         end\n\
         result\n"
    );
    run(&src)
}

fn as_str(v: Value) -> String {
    match v {
        Value::String(s) => s,
        other => panic!("期望 String 结果，实际：{other:?}"),
    }
}

/// **D271 主断言**：恒假条件必须**阻断**这条边。
///
/// 修前：`on: false` 照样激活 b、返回 `B`。
#[test]
fn d271_false_condition_blocks_the_edge() {
    let got =
        as_str(graph_with_const_cond("false").unwrap_or_else(|e| panic!("图应跑通，实际：{e}")));
    assert_eq!(
        got, "A",
        "`on: false` 必须阻断 `a -> b` ⇒ 只有 a 跑过、result 是 \"A\"。\
         修前返回 \"B\"（条件被完全忽略）"
    );
}

/// **判别器**：同一个图换成恒真条件，**必须**产生不同结果。
///
/// 这条是「主断言不是因为别的原因才绿」的保证 —— 若两张图在修前
/// 产出逐字相同（实测确实相同），本条就说明修前它们是同一个值。
#[test]
fn d271_true_condition_takes_the_edge() {
    let got =
        as_str(graph_with_const_cond("true").unwrap_or_else(|e| panic!("图应跑通，实际：{e}")));
    assert_eq!(got, "B", "`on: true` 必须放行 `a -> b` ⇒ result 是 \"B\"");
}

/// 真实用例：条件读取引擎环境里的变量。
///
/// 修前 `gate` 取 `"no"` 与 `"yes"` 产出**相同**结果。
#[test]
fn d271_condition_reads_env_state() {
    assert_eq!(
        as_str(graph_with_gate("no").expect("图应跑通")),
        "A",
        "gate=\"no\" 时条件为假 ⇒ 边被阻断"
    );
    assert_eq!(
        as_str(graph_with_gate("yes").expect("图应跑通")),
        "B",
        "gate=\"yes\" 时条件为真 ⇒ 边被放行"
    );
}

/// **根因判据**：`condition_body` 必须真的到达 MIR（引擎唯一读的那个字段）。
///
/// 上三条测**症状**；本条钉**机制**。修前编译产物里每条边都是
/// `condition_body: None`。
#[test]
fn d271_condition_body_reaches_the_mir() {
    let src = "orchestrate graph input -> result\n\
               \x20 agent a => \"A\"\n\
               \x20 agent b => \"B\"\n\
               \x20 @start -> a\n\
               \x20 a -> b on: false\n\
               end\n\
               result\n";
    let (func, _w) = ParserV3::compile(src).expect("compile");
    let dump = format!("{func:?}");
    assert!(
        dump.contains("condition_body: Some("),
        "带条件的边必须带 `condition_body`（引擎只读它）—— 修前恒为 `None`"
    );
    // `condition_expr` 也不应在往返中消失（LSP 折叠/语义依赖它）。
    assert!(
        dump.contains("condition_expr: Some("),
        "`condition_expr` 也必须被保留（D271 修前在 `mir_edge_from_witness` \
         里被硬编码成 `None`）"
    );
}

/// **对照组**：无条件的边不得被本修复误伤。
///
/// 修复让 `condition_body` 从 `None` 变成 `Some(..)`；若条件求值逻辑写错，
/// 无条件边可能被当成条件为假而全部阻断。
#[test]
fn d271_conditionaless_edge_is_unaffected() {
    let res = run("orchestrate graph input -> result\n\
         \x20 agent a => \"A\"\n\
         \x20 agent b => \"B\"\n\
         \x20 @start -> a\n\
         \x20 a -> b\n\
         end\n\
         result\n")
    .expect("无条件边应照常跑通");
    assert_eq!(as_str(res), "B", "无条件边必须照常激活");
}

/// `orchestrate pregel` 与 `graph` **共用同一套边机制**，此前同样失效。
///
/// 本条把「共享」这件事钉住，避免将来只修一边。
#[test]
fn d271_pregel_edge_conditions_work_too() {
    let res = run("orchestrate pregel input -> result\n\
         \x20 agent a => \"A\"\n\
         \x20 agent b => \"B\"\n\
         \x20 edge @start -> a\n\
         \x20 edge a -> b on: false\n\
         end\n\
         result\n")
    .expect("pregel 图应跑通");
    assert_eq!(
        as_str(res),
        "A",
        "pregel 的 `on:` 条件此前同样无效（同一套边机制），应一并修好"
    );
}
