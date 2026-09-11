//! v0.92: 从 builtins/mod.rs 拆出的测试组（P1.1 god module 拆分）。

#![allow(unused_mut)]

mod tests_v047_dag {
    use crate::interpreter::Interpreter;
    use crate::value::Value;

    /// v0.47.0: ai.dag builtin (OpenFugu §1.6 DAG-as-data)

    #[test]
    fn ai_dag_linear_returns_topological_order() {
        let mut interp = Interpreter::new();
        let nodes = vec![
            Value::String("a".to_string()),
            Value::String("b".to_string()),
            Value::String("c".to_string()),
        ];
        let edges = vec![
            Value::List(vec![
                Value::String("a".to_string()),
                Value::String("b".to_string()),
            ]),
            Value::List(vec![
                Value::String("b".to_string()),
                Value::String("c".to_string()),
            ]),
        ];
        let result = interp
            .call_ai_method("dag", &[Value::List(nodes), Value::List(edges)])
            .expect("dag 调用应成功");
        match result {
            Value::List(items) => {
                let names: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert_eq!(names, vec!["a", "b", "c"]);
            }
            other => panic!("expected List, got: {:?}", other),
        }
    }

    #[test]
    fn ai_dag_cycle_returns_error() {
        let mut interp = Interpreter::new();
        let nodes = vec![
            Value::String("a".to_string()),
            Value::String("b".to_string()),
        ];
        let edges = vec![
            Value::List(vec![
                Value::String("a".to_string()),
                Value::String("b".to_string()),
            ]),
            Value::List(vec![
                Value::String("b".to_string()),
                Value::String("a".to_string()),
            ]),
        ];
        let err = interp
            .call_ai_method("dag", &[Value::List(nodes), Value::List(edges)])
            .expect_err("cycle should fail");
        assert!(err.contains("ai.dag"), "got: {}", err);
        assert!(err.contains("cycle"), "got: {}", err);
    }

    #[test]
    fn ai_dag_diamond_returns_valid_order() {
        let mut interp = Interpreter::new();
        let nodes = vec![
            Value::String("a".to_string()),
            Value::String("b".to_string()),
            Value::String("c".to_string()),
            Value::String("d".to_string()),
        ];
        let edges = vec![
            Value::List(vec![
                Value::String("a".to_string()),
                Value::String("b".to_string()),
            ]),
            Value::List(vec![
                Value::String("a".to_string()),
                Value::String("c".to_string()),
            ]),
            Value::List(vec![
                Value::String("b".to_string()),
                Value::String("d".to_string()),
            ]),
            Value::List(vec![
                Value::String("c".to_string()),
                Value::String("d".to_string()),
            ]),
        ];
        let result = interp
            .call_ai_method("dag", &[Value::List(nodes), Value::List(edges)])
            .expect("dag 调用应成功");
        match result {
            Value::List(items) => {
                let names: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert_eq!(names[0], "a");
                assert_eq!(names[3], "d");
            }
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn ai_dag_empty_edges_returns_nodes() {
        let mut interp = Interpreter::new();
        let nodes = vec![
            Value::String("a".to_string()),
            Value::String("b".to_string()),
        ];
        let result = interp
            .call_ai_method("dag", &[Value::List(nodes), Value::List(vec![])])
            .expect("dag 调用应成功");
        match result {
            Value::List(items) => assert_eq!(items.len(), 2),
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn ai_dag_requires_2_args() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_ai_method("dag", &[])
            .expect_err("no args should fail");
        assert!(err.contains("requires 2 args"), "got: {}", err);
    }
}
