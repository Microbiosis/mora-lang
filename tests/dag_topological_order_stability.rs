//! v0.104.6 D280：`OrchestrateDag::topological_order` 对**并列节点**返回
//! **不确定顺序**（已修）
//!
//! ## 实测（修前）
//!
//! ```text
//! 声明序 = ["a","b","c","d","e","f","g","h"]     ← 8 个节点、零边，全部独立
//! 返回序 = ["d","b","f","a","c","e","g","h"]
//! ```
//!
//! 顺序由 `HashMap` 迭代决定，而 Rust 的 `HashMap` 用**逐进程随机种子**
//! （`RandomState`）⇒ **同一张图、不同进程返回的顺序不同**。
//!
//! ## 为什么这是缺陷（尽管拓扑序本身对并列节点无所谓）
//!
//! ① **它被暴露给用户**：builtin `ai.dag(nodes, edges)` 把 `order` 直接
//!    作为 `Value::List` 返回。
//! ② **仓库内已有同一条原则**：`pregel/mod.rs` 明确按 agent 定义顺序排序
//!    `active_nodes`，注释写着「HashSet 迭代顺序不确定 → 会让 sequential 与
//!    parallel EXEC 产出依赖顺序的结果」。同一个仓库、同一类问题。
//!
//! ## 修法：并列一律按**源码书写顺序**打破平局
//!
//! - 起点：改为遍历 `self.nodes` 入队（修前遍历 `in_degree` 这个 HashMap）；
//! - 同层后继：按 `pos`（声明位置）排序后入队。
//!
//! ## ⚠ 本条目前**零用户可见变更**
//!
//! builtin `ai.dag` 在源码里**不可达** —— `call_ai_method` 只挂在
//! `(BuiltinKind::Ai, _)` 上，而 parser 把裸名 `ai.x` 解析成
//! `BuiltinKind::AiChat`（D59 `tests/ai_namespace_reachability.rs` 已有
//! 完整论证与判据）。
//!
//! ⇒ 本条是给「将来把 `ai.dag` 接线」**拆雷**：否则一接线，用户拿到的
//! 就是逐次不同的顺序。

use mora::orchestrate_dag::OrchestrateDag;

fn s(items: &[&str]) -> Vec<String> {
    items.iter().map(|x| x.to_string()).collect()
}

/// **主断言**：无边（全部独立）的节点必须按**声明顺序**返回。
///
/// 修前返回 `["d","b","f","a","c","e","g","h"]` 之类的随机排列。
#[test]
fn d280_independent_nodes_keep_declaration_order() {
    let decl = ["a", "b", "c", "d", "e", "f", "g", "h"];
    let dag = OrchestrateDag::new(s(&decl), vec![]);
    let order = dag.topological_order().expect("无边必然无环");
    assert_eq!(
        order,
        s(&decl),
        "独立节点必须按声明顺序返回 —— 修前由 HashMap 迭代决定、逐进程变化"
    );
}

/// 同一个 `from` 的**多个后继**入队时也按声明顺序。
///
/// 图：节点声明序 `a,b,c,d,e`；边 `a->c, a->b, a->d, b->e`。
/// 推演（修后）：入度 a=0、b/c/d/e=1 ⇒ 队列 `[a]`；
/// 弹 `a`，其后继按**声明位置**排序后入队 ⇒ `b, c, d`；
/// 弹 `b` 时又把 `e` 解锁并排到队尾 ⇒ 队列 `[c, d, e]`。
/// ⇒ 结果 `a, b, c, d, e`。
///
/// ⚠ 容易写错的一点：**不是**「后继紧接着按声明序出现」——
/// BFS 会把新解锁的节点排到队尾，`e` 因此落在 `d` 之后。
#[test]
fn d280_same_layer_successors_keep_declaration_order() {
    let dag = OrchestrateDag::new(
        s(&["a", "b", "c", "d", "e"]),
        vec![
            ("a".into(), "c".into()),
            ("a".into(), "b".into()),
            ("a".into(), "d".into()),
            ("b".into(), "e".into()),
        ],
    );
    let order = dag.topological_order().expect("无环");
    assert_eq!(
        order,
        s(&["a", "b", "c", "d", "e"]),
        "a 的三个后继（声明序 c,b,d）应以 b 起步 —— 即按声明位置排序入队"
    );
    // 钉住「b 在 c 之前」这个真正的判别点：修前由 HashMap 决定、逐次不同。
    let pos = |n: &str| order.iter().position(|x| x == n).expect("应出现");
    assert!(pos("b") < pos("c"), "order = {order:?}");
    assert!(pos("b") < pos("d"), "order = {order:?}");
}

/// **确定性**：同一张图反复调用必须给出**完全相同**的顺序。
///
/// 修前单进程内 HashMap 种子固定、这条会绿；它守的是「不得引入
/// 其它不确定性来源」（例如误用 `HashSet` 收集后继）。
#[test]
fn d280_order_is_stable_across_repeated_calls() {
    let dag = OrchestrateDag::new(
        s(&["x", "y", "z", "p", "q"]),
        vec![("x".into(), "y".into()), ("p".into(), "q".into())],
    );
    let first = dag.topological_order().expect("无环");
    for i in 0..20 {
        assert_eq!(
            dag.topological_order().expect("无环"),
            first,
            "第 {i} 次调用给出不同顺序 —— 顺序不确定"
        );
    }
}

/// **对照组**：拓扑序的**正确性**不受影响（每条边 from 必须先于 to）。
///
/// 这是本条断言的核心价值：把「确定顺序」与「拓扑正确」分开守 ——
/// 只钉顺序的话，一个把所有节点倒序返回的实现也能通过。
#[test]
fn d280_topological_validity_is_preserved() {
    let dag = OrchestrateDag::new(
        s(&["a", "b", "c", "d"]),
        vec![
            ("a".into(), "b".into()),
            ("a".into(), "c".into()),
            ("b".into(), "d".into()),
            ("c".into(), "d".into()),
        ],
    );
    let order = dag.topological_order().expect("无环");
    let pos = |n: &str| order.iter().position(|x| x == n).expect("每个节点都应出现");
    for (from, to) in [("a", "b"), ("a", "c"), ("b", "d"), ("c", "d")] {
        assert!(
            pos(from) < pos(to),
            "边 {from} -> {to} 被违反：order = {order:?}"
        );
    }
    assert_eq!(order.len(), 4, "每个节点都必须出现且只出现一次");
}

/// 对照组：带环的图仍必须报错（确定性改动不能把环检测弄丢）。
#[test]
fn d280_cycle_detection_still_works() {
    let dag = OrchestrateDag::new(
        s(&["a", "b"]),
        vec![("a".into(), "b".into()), ("b".into(), "a".into())],
    );
    let err = dag.topological_order().expect_err("带环应报错");
    assert!(err.contains("cycle"), "got: {err}");
}
