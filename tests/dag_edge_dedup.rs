//! v0.104.6：DAG 建图的**边唯一性**不变量。
//!
//! ## 背景
//!
//! `dag_analyze` 有**两个独立的 Sequence 边发射点**，各自 push 而无去重：
//! ① Step 1 的块内全序链；② Step 3 的 `last_effect` 扇出。一个紧跟 Effect 的
//! Compute 节点会**同时**被两者连到 ⇒ 产生重复边。
//!
//! 实测 e2e fixture：边总数 3067，其中 **197 处** `(from, to, kind)` 完全重复
//! （7%）。危害：执行器每 wave 遍历全部 `dag.edges`，冗余边是纯浪费；且任何
//! 「统计分叉数」的消费者（如 `threaded_successor` 判断是否直线）都必须自己去重
//! 才能得到正确结果。
//!
//! ## 关键区分：只去同 kind 的重复
//!
//! 实测另有 **543 处** `(from, to)` 相同但 **kind 不同**（如 Data 边与 Sequence
//! 边并存）—— 那**不是**冗余：Data 决定「输入就绪」、Sequence 决定「控制到达」，
//! 删任何一条都会改变语义。本测试同时钉住「重复清零」与「不同 kind 必须保留」。

use std::collections::{HashMap, HashSet};

/// 编译 fixture 并返回其 DAG 的 (节点数, 边数, 完全重复边数)。
fn analyze(fixture: &str) -> (usize, usize, usize) {
    let src = std::fs::read_to_string(fixture).expect("读 fixture");
    let (func, _) = mora::parser_v3::ParserV3::compile(&src)
        .unwrap_or_else(|e| panic!("应能解析 {fixture}: {e}"));
    let dag = mora::mir::dag::dag_analyze(&std::sync::Arc::new(func));
    let mut seen: HashSet<(usize, usize, String)> = HashSet::new();
    let mut dup = 0usize;
    for e in &dag.edges {
        if !seen.insert((e.from, e.to, format!("{:?}", e.kind))) {
            dup += 1;
        }
    }
    (dag.nodes.len(), dag.edges.len(), dup)
}

fn fixtures() -> Vec<String> {
    // v0.104.6 D475：原有一行 `let dir = std::env::temp_dir(); let _ = &dir;`
    // —— 纯粹的死代码（取了路径又不用），已删。
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/e2e");
    let mut v: Vec<String> = std::fs::read_dir(base)
        .expect("读 fixtures 目录")
        .filter_map(|e| {
            let p = e.ok()?.path();
            (p.extension().map(|x| x == "mora").unwrap_or(false))
                .then(|| p.to_string_lossy().into_owned())
        })
        .collect();
    v.sort();
    v
}

/// **主判据**：全量 fixture 的 DAG **不得含完全重复的边**。
#[test]
fn dag_has_no_duplicate_edges() {
    let mut offenders = Vec::new();
    let mut total_dup = 0usize;
    for f in fixtures() {
        let (nodes, edges, dup) = analyze(&f);
        total_dup += dup;
        if dup > 0 {
            offenders.push(format!("{f}: {nodes} nodes / {edges} edges / {dup} 条重复"));
        }
    }
    assert!(
        total_dup == 0,
        "DAG 含 {total_dup} 条完全重复的边（`(from,to,kind)` 相同）：\n  {}",
        offenders.join("\n  ")
    );
}

/// **反向判据**：不同 `kind` 的同端点边**必须保留**。
///
/// 防止「去重」被误实现成「按 (from,to) 去重」——那会删掉 Data/Sequence 并存
/// 的边，改变语义（Data 决定输入就绪、Sequence 决定控制到达）。
#[test]
fn dag_preserves_different_kinds_on_same_endpoints() {
    let mut found = 0usize;
    for f in fixtures() {
        let src = std::fs::read_to_string(&f).expect("读 fixture");
        let Ok((func, _)) = mora::parser_v3::ParserV3::compile(&src) else {
            continue;
        };
        let dag = mora::mir::dag::dag_analyze(&std::sync::Arc::new(func));
        // 同 (from,to) 但 kind 不同 —— 这些必须仍在。
        let mut kinds: HashMap<(usize, usize), HashSet<String>> = HashMap::new();
        for e in &dag.edges {
            kinds
                .entry((e.from, e.to))
                .or_default()
                .insert(format!("{:?}", e.kind));
        }
        found += kinds.values().filter(|s| s.len() > 1).count();
    }
    assert!(
        found > 0,
        "全量 fixture 中**没有任何**「同端点不同 kind」的边 —— 若真如此，\
         说明去重被误做成按 (from,to) 去重、或建图不再产出多 kind 边。\
         本测试用于守住 Data/Sequence 并存这一语义。"
    );
}

/// 去重后**边数应当下降但不为零** —— 防止「去重」误删成空图。
#[test]
fn dedup_keeps_graph_connected() {
    let mut any = false;
    for f in fixtures() {
        let (nodes, edges, dup) = analyze(&f);
        if nodes > 2 {
            any = true;
            assert_eq!(dup, 0, "{f} 仍有重复边");
            assert!(
                edges >= nodes.saturating_sub(1),
                "{f}: {nodes} nodes 但只有 {edges} 条边 —— 疑似过度去重"
            );
        }
    }
    assert!(any, "没有任何 fixture 参与本测试");
}
