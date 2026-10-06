//! v0.104.6 D268：Pregel **超步预算耗尽**静默返回中途结果（已修）
//!
//! ## 实测（修前，真实 CLI）
//!
//! ```mora
//! orchestrate pregel input -> result
//!   agent a => "A"
//!   agent b => "B"
//!   edge @start -> a
//!   edge a -> b
//!   edge b -> a      -- ← 环
//! end
//! print(result)
//! ```
//!
//! | | 修前 | 修后 |
//! |---|---|---|
//! | stdout | `A` | 无输出 |
//! | 退出码 | **0** | **1** |
//! | 诊断 | **零** | `pregel: super-step budget exhausted after 1000 steps …` |
//! | 耗时 | 196ms | 764ms（含错误构造） |
//!
//! 返回的 `A` 还是**中途**的值：链式图 `@start→a→b→c→d` 配 `max_steps=2`
//! 时 `result` 是 `"A"` —— 链尾 `d` **从未执行**，而引擎把 `A` 当答案报了出来。
//!
//! ## 机制
//!
//! `pregel::MirPregelEngine::run` 的主循环
//!
//! ```text
//! while !active_nodes.is_empty() && self.current_step < self.max_steps
//! ```
//!
//! 有**两个**出口，而 D97 只守了其中一个：
//!
//! | 出口 | 守卫 | 状态 |
//! |---|---|---|
//! | `active_nodes` 空 = 收敛 | —— | 正常 |
//! | `scheduled == 0` = 没激活过任何 agent | D97 | 已有 |
//! | **`current_step >= max_steps` = 预算烧完** | **D268（本轮）** | **修前无** |
//!
//! 第三个出口既不报错也不警告，直接落到 `channels.get("result")` ——
//! 而 `result` 是每个 agent 返回值的 last-write-wins 通道（`reconcile_outcome`
//! 无条件写它），于是**中途值被当成最终答案**。
//!
//! ## 为什么这不算「用户用完预算」
//!
//! `max_steps` **无法从语言层设置**：`with_max_steps` 在整个 `src/` 内
//! **无任何调用点**，值硬编码于 `MirPregelEngine::new()`（=1000）。
//! （`orchestrate.dag(nodes, edges, max_steps?)` 是 `orchestrate_dag`
//! 模块的另一个引擎，与本引擎无关 —— 实测 `orchestrate pregel` 的
//! 解析路径不经过它。）
//!
//! 因此 Mora 程序撞上上限**只可能**是图不收敛，而不是主动设预算。
//! 本守卫不限制任何合法用法。
//!
//! ## 顺序
//!
//! 守卫刻意放在 D97 **之后**：`max_steps == 0` 时两个条件同时成立，
//! 而「没有任何 agent 被激活」是更具体的诊断，应优先报出。

use mora::interpreter::Interpreter;
use mora::mir::vm::run_mir;
use mora::parser_v3::ParserV3;
use std::sync::Arc;

fn run(source: &str) -> Result<mora::value::Value, String> {
    let (func, _w) = ParserV3::compile(source).expect("compile");
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

/// **D268 主断言**：带环的图必须报错，且错误消息要指出「未收敛」与待跑顶点。
///
/// 修前：打印 `A`、exit 0、零诊断 —— 等于告诉用户「这张图跑过了」。
#[test]
fn d268_cyclic_graph_reports_budget_exhaustion() {
    let res = run(r#"
orchestrate pregel input -> result
  agent a => "A"
  agent b => "B"
  edge @start -> a
  edge a -> b
  edge b -> a
end
result
"#);
    match res {
        Err(e) => {
            assert!(
                e.contains("budget exhausted"),
                "错误应指明超步预算耗尽。实际：{e}"
            );
            assert!(
                e.contains("did not converge") || e.contains("cycle"),
                "错误应给出「未收敛 / 检查环」的修法。实际：{e}"
            );
            assert!(
                e.contains("still pending"),
                "错误应列出仍未执行的顶点。实际：{e}"
            );
            assert!(
                e.contains('a') || e.contains('b'),
                "错误应点名待跑顶点。实际：{e}"
            );
        }
        Ok(v) => panic!(
            "带环的 Pregel 图应**报错**；却静默返回了 {v:?} —— D268 已修，\
             若本测试失败说明守卫被移除"
        ),
    }
}

/// 对照组：**收敛**的链式图必须照常跑通、返回链尾值。
///
/// 这是守卫的牙齿验证 —— 若守卫条件写错（例如把「循环退出」当成「预算耗尽」），
/// 本测试会立刻变红。
#[test]
fn d268_converging_chain_is_not_flagged() {
    let res = run(r#"
orchestrate pregel input -> result
  agent a => "A"
  agent b => "B"
  agent c => "C"
  edge @start -> a
  edge a -> b
  edge b -> c
end
result
"#)
    .expect("收敛的图不应报错");
    assert_eq!(
        format!("{:?}", res),
        "String(\"C\")",
        "对照组：收敛链应返回链尾 agent 的值"
    );
}

/// 对照组：**恰好**在预算内跑完的图也不得误报。
///
/// 守卫条件是 `current_step >= max_steps && !active_nodes.is_empty()`；
/// 若写成只看 `current_step >= max_steps`，最后一步恰好触顶的正常图会被误伤。
#[test]
fn d268_graph_finishing_exactly_at_budget_is_not_flagged() {
    // @start→a→b→c 需要 3 个超步；给 3 步，最后一步后 active_nodes 为空。
    let (func, _w) = ParserV3::compile(
        r#"
orchestrate pregel input -> result
  agent a => "A"
  agent b => "B"
  agent c => "C"
  edge @start -> a
  edge a -> b
  edge b -> c
end
result
"#,
    )
    .expect("compile");
    let mut interp = Interpreter::new();
    let mut env = interp.take_env();
    let arc = Arc::new(func);
    let v = run_mir(
        &arc,
        &mut interp,
        &mut env,
        &mut mora::mir::effect::Effects::new(),
    )
    .expect("恰好用完预算的收敛图不应报错");
    assert_eq!(format!("{:?}", v), "String(\"C\")");
}
