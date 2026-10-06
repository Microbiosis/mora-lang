//! v0.104.6 D400 —— `OrchestrateDag::has_cycle()` 对**非环**的畸形图也返回
//! `true`（修复轮；当前零生产调用方，如实记录）
//!
//! ## 缺陷
//!
//! ```rust
//! pub fn has_cycle(&self) -> bool {
//!     self.topological_order().is_err()
//! }
//! ```
//!
//! 而 `topological_order()` **第一件事**就是 `self.validate()?`，而
//! `validate()` 的三类错误**都不是环**：
//!
//! | `validate()` 的错误 | 是不是环？ |
//! |---|---|
//! | `duplicate node 'a'` | ❌ 不是 |
//! | `edge from unknown node 'x'` | ❌ 不是 |
//! | `edge to unknown node 'ghost'` | ❌ 不是 |
//!
//! ⇒ 一张**根本没有环**、只是**打错了一个节点名**的图，
//! `has_cycle()` 会回答「**有环**」。
//!
//! ## 判定：修
//!
//! 本方法的 doc 自己写着「拓扑排序并检测**环** (Kahn's standard detection)」
//! ⇒ **意图明确就是环**，实现与自己的 doc 矛盾 ⇒ 按 D396 的判据
//! （无注释解释 + doc 明确 + 兄弟语义不同 = 遗漏）属**笔误**，修。
//!
//! ## ⚠ 当前**零生产调用方**（不夸大）
//!
//! 全仓 `has_cycle` 只有 `orchestrate_dag/mod.rs` 自己的单测调用；
//! 整个 `OrchestrateDag` 类型也无生产消费者（builtin `ai.dag` 在源码里
//! 不可达，D59 / D280 记录）。⇒ 这是 **`pub` API 上的潜伏缺陷**。
//! 判据 `d400_type_has_no_production_consumer` 把该事实钉住 ——
//! 若将来接线，重复节点 / 未知端点就会被误报成「环」。

use mora::orchestrate_dag::OrchestrateDag;

fn dag(nodes: &[&str], edges: &[(&str, &str)]) -> OrchestrateDag {
    OrchestrateDag::new(
        nodes.iter().map(|s| s.to_string()).collect(),
        edges
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
    )
}

// ── ① 核心：`has_cycle` 只回答「环」 ──

/// **未知边端点 ⇒ `has_cycle()` 必须是 `false`**（没有环，只是打错名字了）。
#[test]
fn d400_unknown_edge_endpoint_is_not_a_cycle() {
    let d = dag(&["a"], &[("a", "ghost")]);
    // 前提：validate 的确报「未知节点」而不是「环」
    let err = d.topological_order().expect_err("应报错");
    assert!(
        err.contains("unknown node"),
        "前提：错误应是「未知节点」; 实得 {err}"
    );
    assert!(
        !err.contains("cycle"),
        "前提：这条错误**不是**环错误; 实得 {err}"
    );
    assert!(
        !d.has_cycle(),
        "一张没有环、只是边指向未知节点的图被报成「有环」（{err}）"
    );
}

/// **重复节点 ⇒ `has_cycle()` 必须是 `false`**。
#[test]
fn d400_duplicate_node_is_not_a_cycle() {
    let d = dag(&["a", "a"], &[]);
    let err = d.topological_order().expect_err("应报错");
    assert!(err.contains("duplicate"), "前提: {err}");
    assert!(
        !err.contains("cycle"),
        "前提：这条错误**不是**环错误; 实得 {err}"
    );
    assert!(
        !d.has_cycle(),
        "没有环、只是节点重复的图被报成「有环」（{err}）"
    );
}

/// **反向对照：真环仍必须报 `true`**。
///
/// 本条与上面两条配套：若只把 `has_cycle` 改成永假，本条会红。
#[test]
fn d400_real_cycle_is_still_detected() {
    // 二元环
    assert!(dag(&["a", "b"], &[("a", "b"), ("b", "a")]).has_cycle());
    // 自环
    assert!(dag(&["a"], &[("a", "a")]).has_cycle());
    // 三元环
    assert!(dag(&["a", "b", "c"], &[("a", "b"), ("b", "c"), ("c", "a")]).has_cycle());
}

/// **反向对照：合法无环图必须是 `false`**（含多连通分量）。
#[test]
fn d400_acyclic_graphs_are_false() {
    assert!(!dag(&["a", "b", "c"], &[("a", "b"), ("b", "c")]).has_cycle());
    assert!(!dag(&["a", "b", "c"], &[]).has_cycle());
    // 菱形：a→b、a→c、b→d、c→d（无环）
    assert!(
        !dag(
            &["a", "b", "c", "d"],
            &[("a", "b"), ("a", "c"), ("b", "d"), ("c", "d")]
        )
        .has_cycle()
    );
}

// ── ② 畸形分类：三类 `validate` 错误都不得被当成环 ──

/// **`validate()` 的三类错误，`has_cycle()` 一律不得报 `true`**。
#[test]
fn d400_malformed_cases_are_never_reported_as_cycles() {
    let cases: &[(&str, OrchestrateDag)] = &[
        ("重复节点", dag(&["a", "a"], &[])),
        ("边起点未知", dag(&["a"], &[("ghost", "a")])),
        ("边终点未知", dag(&["a"], &[("a", "ghost")])),
    ];
    for (why, d) in cases {
        assert!(!d.has_cycle(), "{why}：不是环，却被 `has_cycle()` 报成环");
    }
}

// ── ③ 现状钉：`has_cycle` 零调用方；`ai.dag` 源码不可达 ──

/// **`has_cycle` 只被本模块自己的单测调用，别处零调用**。
///
/// 注：`topological_order` **有**生产调用点（`builtins/ai.rs:180`），
/// 但那条路径本身不可达（见后两条）。两者必须**分开**统计 ——
/// 早版判据把它们混为一谈，结果把「类型被用到」误报成「方法被用到」。
#[test]
fn d400_has_cycle_has_no_caller_outside_its_own_tests() {
    let mut hits = Vec::new();
    walk_src("src", &mut hits);
    let outside: Vec<&String> = hits
        .iter()
        .filter(|h| !h.starts_with("src\\orchestrate_dag\\mod.rs"))
        .collect();
    assert!(
        outside.is_empty(),
        "`has_cycle` 出现了**本模块之外**的调用方 {outside:?} —— \
         「潜伏缺陷」的前提需重写，且应补端到端判据（畸形图会被误报成环）"
    );
    // 反向对照：本模块单测确实在调它（证明上面那条不是「压根没人调」的空断言）
    assert!(
        !hits.is_empty(),
        "本模块单测里应有 `has_cycle()` 调用作为反向对照；实得 0 处"
    );
}

/// **`topological_order` 确有生产调用点，但在 `ai.dag` 里。**
///
/// 本条钉住「类型被用到」与「方法被用到」的**区别**，
/// 避免下一个审计者把两者混为一谈。
#[test]
fn d400_topological_order_is_called_from_ai_dag_builtin() {
    let mut hits = Vec::new();
    walk_src("src", &mut hits);
    let ai = read("src/interpreter/builtins/ai.rs");
    assert!(
        ai.contains("OrchestrateDag::new(nodes, edges)"),
        "`ai.dag` 应仍构造 `OrchestrateDag`"
    );
    assert!(
        ai.contains("topological_order()"),
        "`ai.dag` 应仍调 `topological_order()`"
    );
    assert!(
        !ai.contains("has_cycle"),
        "`ai.dag` 竟调了 `has_cycle` —— 畸形图会被误报成环，\
         需改 `ai.dag` 只走 `topological_order()`"
    );
}

/// **D59 现状：`ai.dag` 源码不可达**（parser 把裸名 `ai.x` 解析成
/// `BuiltinKind::AiChat`，而 `call_ai_method` 只挂在 `BuiltinKind::Ai` 上）。
///
/// 实测：`ai.dag([...], [[...]])` → `Runtime error: Unknown method: AiChat.dag`。
/// ⇒ 「潜伏」的准确说法是：三层里**底层与中间接线都在**，
/// **上层入口不通**（与 D280 注释的说法一致）。
#[test]
fn d400_ai_dag_is_source_unreachable() {
    let ai = read("src/interpreter/builtins/ai.rs");
    assert!(
        ai.contains("\"dag\" =>"),
        "`ai.dag` builtin 分支应仍在（接线在，只是入口不通）"
    );
    // 入口不通的依据：`call_ai_method` 挂在 AiChat 上
    let dispatch = read("src/interpreter/method_dispatch.rs");
    assert!(
        dispatch.contains("BuiltinKind::AiChat"),
        "`ai.*` 方法应挂在 `AiChat` 上（D59 的结论）"
    );
}

fn read(rel: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读 {rel} 失败: {e}"))
}

/// 扫 `src/`（含 `#[cfg(test)]` 区，因为**单测里的调用也算「有调用方」**）
/// 找 `has_cycle` 的调用点。
fn walk_src(rel: &str, out: &mut Vec<String>) {
    fn walk(dir: &std::path::Path, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                let src = std::fs::read_to_string(&p).unwrap_or_default();
                for (i, line) in src.lines().enumerate() {
                    let t = line.trim();
                    if t.starts_with("//") {
                        continue;
                    }
                    // 只认「调用」，不算定义 `pub fn has_cycle`
                    if t.contains("has_cycle(") && !t.contains("pub fn has_cycle") {
                        let rel = p
                            .strip_prefix(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
                            .unwrap_or(&p)
                            .display()
                            .to_string();
                        out.push(format!("{rel}:{}: {t}", i + 1));
                    }
                }
            }
        }
    }
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel),
        out,
    );
}
