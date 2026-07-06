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
//! - builtin `orchestrate.dag(nodes, edges, max_steps?)` → List[String] order

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
    /// Returns: Vec<String> in execution order
    pub fn topological_order(&self) -> Result<Vec<String>, String> {
        self.validate()?;

        // in-degree 计数
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        for n in &self.nodes {
            in_degree.insert(n.as_str(), 0);
        }
        for (_, to) in &self.edges {
            *in_degree.get_mut(to.as_str()).unwrap() += 1;
        }

        // 起点: in_degree == 0
        let mut queue: VecDeque<&str> = VecDeque::new();
        for (n, &deg) in &in_degree {
            if deg == 0 {
                queue.push_back(n);
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

        while let Some(n) = queue.pop_front() {
            order.push(n.to_string());
            if let Some(tos) = edges_by_from.get(n) {
                for to in tos {
                    let d = in_degree.get_mut(to).unwrap();
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
    pub fn has_cycle(&self) -> bool {
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
