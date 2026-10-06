//! v0.47.0: DAG-as-data orchestration (OpenFugu §1.6 inspired)
//!
//! 灵感: OpenFugu `openfugu/ultra.py` DAG-as-data
//! - `model_id[]` — list of agent names
//! - `subtasks[]` — list of subtask definitions (parallel with model_id)
//! - `access_list[]` — list of (from, to) edges
//!
//! v0.47.0 Mora adaptation:
//! - `OrchestrateDag` struct: nodes + edges
//! - `topological_order()` — Kahn's algorithm (BFS)
//! - `validate()` — detect cycles, missing nodes
//!
//! v0.104.6 D280：下面这行原本写着
//! `builtin orchestrate.dag(nodes, edges, max_steps?)`，**两处都不对**：
//! - 实际 builtin 名是 **`ai.dag`**（不是 `orchestrate.dag`）；
//! - **没有** `max_steps` 参数 —— 实参就是 `(nodes, edges)` 两个，
//!   少于此数直接报 `ai.dag: requires 2 args (nodes, edges)`。
//!
//! 另注：那个 builtin 目前**源码不可达**（D59
//! `tests/ai_namespace_reachability.rs`）—— 此处仅订正签名，不宣称它可用。

use std::collections::{HashMap, HashSet, VecDeque};

/// v0.47.0: DAG-as-data (OpenFugu ultra.py model)
#[derive(Debug, Clone)]
pub struct OrchestrateDag {
    pub nodes: Vec<String>,
    pub edges: Vec<(String, String)>, // (from, to)
}

impl OrchestrateDag {
    pub fn new(nodes: Vec<String>, edges: Vec<(String, String)>) -> Self {
        Self { nodes, edges }
    }

    /// Validate DAG: check unknown nodes, duplicate nodes, edges with unknown endpoints
    pub fn validate(&self) -> Result<(), String> {
        let node_set: HashSet<&str> = self.nodes.iter().map(|s| s.as_str()).collect();

        // Duplicate node check
        let mut seen = HashSet::new();
        for n in &self.nodes {
            if !seen.insert(n.as_str()) {
                return Err(format!("duplicate node '{}'", n));
            }
        }

        // Edge endpoints check
        for (from, to) in &self.edges {
            if !node_set.contains(from.as_str()) {
                return Err(format!("edge from unknown node '{}'", from));
            }
            if !node_set.contains(to.as_str()) {
                return Err(format!("edge to unknown node '{}'", to));
            }
        }
        Ok(())
    }

    /// Kahn's algorithm: BFS topological sort
    /// Returns: Vec`<String>` in execution order
    ///
    /// v0.104.6 D280：**并列节点的顺序必须确定**。
    ///
    /// 修前 `in_degree` / `edges_by_from` 都是 `HashMap`，而 Rust 的
    /// `HashMap` 用**逐进程随机种子**（`RandomState`）⇒ 同一张图、不同进程
    /// 返回的顺序**不同**。实测 8 个无边（全部独立）的节点，声明序
    /// `a..h` 而返回 `["d","b","f","a","c","e","g","h"]`。
    ///
    /// 拓扑序本身对并列节点无所谓，但这个顺序是**暴露给用户**的 ——
    /// builtin `ai.dag(nodes, edges)` 直接把它作为 `List[String]` 返回。
    /// 仓库内已有同一条原则：`pregel/mod.rs` 明确按 agent 定义顺序排序
    /// `active_nodes`，注释写着「HashSet 迭代顺序不确定 → 会让结果依赖顺序」。
    ///
    /// 本实现改为：**起点按 `nodes` 声明顺序**入队、**同层后继按声明顺序**
    /// 入队 ⇒ 全部并列都按源码书写顺序打破平局。
    ///
    /// ⚠ 目前**零用户可见变更**：builtin `ai.dag` 在源码里**不可达**
    /// （`call_ai_method` 只挂在 `(BuiltinKind::Ai, _)` 上，而 parser 把裸名
    /// `ai.x` 解析成 `BuiltinKind::AiChat` —— 见 D59
    /// `tests/ai_namespace_reachability.rs`）。本条是给「将来接线」拆雷。
    pub fn topological_order(&self) -> Result<Vec<String>, String> {
        self.validate()?;

        // in-degree 计数
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        for n in &self.nodes {
            in_degree.insert(n.as_str(), 0);
        }
        for (_, to) in &self.edges {
            // validate() 已保证 to ∈ nodes，此处为结构不变量
            *in_degree
                .get_mut(to.as_str())
                .expect("topological_order: edge target not in nodes (validate passed)") += 1;
        }

        // v0.104.6 D280：声明位置索引 —— 用来把 HashMap 的并列变成源码序。
        let pos: HashMap<&str, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), i))
            .collect();

        // 起点: in_degree == 0。**按 nodes 声明顺序**入队（修前是遍历
        // HashMap ⇒ 顺序不确定）。
        let mut queue: VecDeque<&str> = VecDeque::new();
        for n in &self.nodes {
            if in_degree.get(n.as_str()).copied().unwrap_or(0) == 0 {
                queue.push_back(n.as_str());
            }
        }

        let mut order = Vec::with_capacity(self.nodes.len());
        // edges_by_from: from -> [to1, to2, ...]
        let mut edges_by_from: HashMap<&str, Vec<&str>> = HashMap::new();
        for (from, to) in &self.edges {
            edges_by_from
                .entry(from.as_str())
                .or_default()
                .push(to.as_str());
        }
        // v0.104.6 D280：同一 from 的多个后继也按声明顺序入队，
        // 否则「a 同时指向 b/c/d」时的并列顺序同样不确定。
        for tos in edges_by_from.values_mut() {
            tos.sort_by_key(|t| pos.get(t).copied().unwrap_or(usize::MAX));
        }

        while let Some(n) = queue.pop_front() {
            order.push(n.to_string());
            if let Some(tos) = edges_by_from.get(n) {
                for to in tos {
                    let d = in_degree.get_mut(to).expect(
                        "topological_order: edge target missing in_degree (validate passed)",
                    );
                    *d -= 1;
                    if *d == 0 {
                        queue.push_back(to);
                    }
                }
            }
        }

        if order.len() != self.nodes.len() {
            return Err(format!(
                "cycle detected: only {} of {} nodes reached",
                order.len(),
                self.nodes.len()
            ));
        }
        Ok(order)
    }

    /// 拓扑排序并检测环 (Kahn's standard detection)
    ///
    /// v0.104.6 D400：**只回答「环」**。
    ///
    /// 此前是 `self.topological_order().is_err()`，而 `topological_order()`
    /// 第一步就 `self.validate()?` ⇒ `validate()` 的三类错误
    /// （`duplicate node` / `edge from unknown node` / `edge to unknown node`）
    /// **全都不是环**，却会让本方法回答「**有环**」。
    /// 实测：`nodes=["a"]`、`edges=[("a","ghost")]`（只是打错节点名）
    /// 报 `has_cycle() == true`。
    ///
    /// 修法：畸形图**无从谈环** ⇒ 返回 `false`，
    /// 由调用方用 `validate()` / `topological_order()` 去处理那三类错误。
    /// `validate` 过了之后，`topological_order` 唯一的错误就只剩环。
    pub fn has_cycle(&self) -> bool {
        if self.validate().is_err() {
            return false;
        }
        self.topological_order().is_err()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_dag_topological_order() {
        // a -> b -> c
        let dag = OrchestrateDag::new(
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
            vec![
                ("a".to_string(), "b".to_string()),
                ("b".to_string(), "c".to_string()),
            ],
        );
        let order = dag.topological_order().unwrap();
        assert_eq!(order, vec!["a", "b", "c"]);
    }

    #[test]
    fn diamond_dag() {
        //   a
        //  / \
        // b   c
        //  \ /
        //   d
        let dag = OrchestrateDag::new(
            vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string(),
            ],
            vec![
                ("a".to_string(), "b".to_string()),
                ("a".to_string(), "c".to_string()),
                ("b".to_string(), "d".to_string()),
                ("c".to_string(), "d".to_string()),
            ],
        );
        let order = dag.topological_order().unwrap();
        // a 必须第一, d 必须最后, b/c 顺序任意
        assert_eq!(order[0], "a");
        assert_eq!(order[3], "d");
        assert!(order[1] == "b" || order[1] == "c");
    }

    #[test]
    fn multiple_independent_nodes() {
        // 三个独立节点 (no edges)
        let dag = OrchestrateDag::new(
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
            vec![],
        );
        let order = dag.topological_order().unwrap();
        assert_eq!(order.len(), 3);
        // 顺序不固定
    }

    #[test]
    fn cycle_detected() {
        // a -> b -> a (cycle)
        let dag = OrchestrateDag::new(
            vec!["a".to_string(), "b".to_string()],
            vec![
                ("a".to_string(), "b".to_string()),
                ("b".to_string(), "a".to_string()),
            ],
        );
        let err = dag.topological_order().unwrap_err();
        assert!(err.contains("cycle"), "got: {}", err);
    }

    #[test]
    fn self_loop_detected() {
        // a -> a (self-loop = cycle)
        let dag = OrchestrateDag::new(
            vec!["a".to_string()],
            vec![("a".to_string(), "a".to_string())],
        );
        let err = dag.topological_order().unwrap_err();
        assert!(err.contains("cycle"), "got: {}", err);
    }

    #[test]
    fn edge_with_unknown_node_errors() {
        let dag = OrchestrateDag::new(
            vec!["a".to_string()],
            vec![("a".to_string(), "ghost".to_string())],
        );
        let err = dag.topological_order().unwrap_err();
        assert!(err.contains("unknown node"), "got: {}", err);
    }

    #[test]
    fn duplicate_node_errors() {
        let dag = OrchestrateDag::new(vec!["a".to_string(), "a".to_string()], vec![]);
        let err = dag.topological_order().unwrap_err();
        assert!(err.contains("duplicate"), "got: {}", err);
    }

    #[test]
    fn has_cycle_helper() {
        let dag = OrchestrateDag::new(
            vec!["a".to_string(), "b".to_string()],
            vec![
                ("a".to_string(), "b".to_string()),
                ("b".to_string(), "a".to_string()),
            ],
        );
        assert!(dag.has_cycle());

        let dag2 = OrchestrateDag::new(
            vec!["a".to_string(), "b".to_string()],
            vec![("a".to_string(), "b".to_string())],
        );
        assert!(!dag2.has_cycle());
    }

    #[test]
    fn complex_4_layer_dag() {
        // L1: a, b
        // L2: c (a), d (b)
        // L3: e (c, d)
        let dag = OrchestrateDag::new(
            vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string(),
                "e".to_string(),
            ],
            vec![
                ("a".to_string(), "c".to_string()),
                ("b".to_string(), "d".to_string()),
                ("c".to_string(), "e".to_string()),
                ("d".to_string(), "e".to_string()),
            ],
        );
        let order = dag.topological_order().unwrap();
        // a,b 必须在 c,d 之前; c,d 必须在 e 之前
        let pos: HashMap<&str, usize> = order
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), i))
            .collect();
        assert!(pos["a"] < pos["c"]);
        assert!(pos["b"] < pos["d"]);
        assert!(pos["c"] < pos["e"]);
        assert!(pos["d"] < pos["e"]);
    }
}
