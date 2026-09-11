//! v0.92: 从 builtins/mod.rs 拆出的测试组（P1.1 god module 拆分）。

#![allow(unused_mut)]

mod tests_v048_plan {
    use crate::interpreter::Interpreter;
    use crate::value::Value;

    /// v0.48.0: plan.* builtin (pi-agent update_plan pattern)

    #[test]
    fn plan_create_then_list() {
        let mut interp = Interpreter::new();
        let steps = vec![
            Value::Dict({
                let mut d = std::collections::HashMap::new();
                d.insert("id".to_string(), Value::String("s1".to_string()));
                d.insert("text".to_string(), Value::String("first".to_string()));
                d.insert("status".to_string(), Value::String("pending".to_string()));
                d
            }),
            Value::Dict({
                let mut d = std::collections::HashMap::new();
                d.insert("id".to_string(), Value::String("s2".to_string()));
                d.insert("text".to_string(), Value::String("second".to_string()));
                d
            }),
        ];
        let name = interp
            .call_plan_method(
                "create",
                &[Value::String("myplan".to_string()), Value::List(steps)],
            )
            .expect("create 调用应成功");
        assert_eq!(name, Value::String("myplan".to_string()));

        let list = interp
            .call_plan_method("list", &[])
            .expect("list 调用应成功");
        match list {
            Value::List(items) => {
                let names: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert!(names.contains(&"myplan".to_string()));
            }
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn plan_update_step_status() {
        let mut interp = Interpreter::new();
        let steps = vec![Value::Dict({
            let mut d = std::collections::HashMap::new();
            d.insert("id".to_string(), Value::String("a".to_string()));
            d.insert("text".to_string(), Value::String("A".to_string()));
            d
        })];
        interp
            .call_plan_method(
                "create",
                &[Value::String("p".to_string()), Value::List(steps)],
            )
            .unwrap();
        // update a -> done
        let updates = vec![Value::List(vec![
            Value::String("a".to_string()),
            Value::String("done".to_string()),
        ])];
        let result = interp
            .call_plan_method(
                "update",
                &[Value::String("p".to_string()), Value::List(updates)],
            )
            .expect("update 调用应成功");
        assert_eq!(result, Value::Bool(true));

        let info = interp
            .call_plan_method("info", &[Value::String("p".to_string())])
            .expect("info 调用应成功");
        match info {
            Value::Dict(d) => {
                let done = d.get("done").expect("done 字段应存在");
                match done {
                    Value::Float(n) => assert_eq!(*n, 1.0),
                    _ => panic!("expected Number"),
                }
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn plan_update_supports_emoji_status() {
        let mut interp = Interpreter::new();
        let steps = vec![Value::Dict({
            let mut d = std::collections::HashMap::new();
            d.insert("id".to_string(), Value::String("a".to_string()));
            d.insert("text".to_string(), Value::String("A".to_string()));
            d
        })];
        interp
            .call_plan_method(
                "create",
                &[Value::String("p".to_string()), Value::List(steps)],
            )
            .unwrap();
        // emoji ✅
        let updates = vec![Value::List(vec![
            Value::String("a".to_string()),
            Value::String("✅".to_string()),
        ])];
        let result = interp
            .call_plan_method(
                "update",
                &[Value::String("p".to_string()), Value::List(updates)],
            )
            .expect("update with emoji");
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn plan_update_unknown_step_errors() {
        let mut interp = Interpreter::new();
        interp
            .call_plan_method(
                "create",
                &[
                    Value::String("p".to_string()),
                    Value::List(vec![Value::Dict({
                        let mut d = std::collections::HashMap::new();
                        d.insert("id".to_string(), Value::String("a".to_string()));
                        d.insert("text".to_string(), Value::String("A".to_string()));
                        d
                    })]),
                ],
            )
            .unwrap();
        let updates = vec![Value::List(vec![
            Value::String("ghost".to_string()),
            Value::String("done".to_string()),
        ])];
        let err = interp
            .call_plan_method(
                "update",
                &[Value::String("p".to_string()), Value::List(updates)],
            )
            .expect_err("unknown step should fail");
        assert!(err.contains("not found"), "got: {}", err);
    }

    #[test]
    fn plan_add_and_remove_step() {
        let mut interp = Interpreter::new();
        interp
            .call_plan_method(
                "create",
                &[Value::String("p".to_string()), Value::List(vec![])],
            )
            .unwrap();
        let added = interp
            .call_plan_method(
                "add",
                &[
                    Value::String("p".to_string()),
                    Value::String("a".to_string()),
                    Value::String("A".to_string()),
                ],
            )
            .expect("add 调用应成功");
        assert_eq!(added, Value::Bool(true));
        let removed = interp
            .call_plan_method(
                "remove",
                &[
                    Value::String("p".to_string()),
                    Value::String("a".to_string()),
                ],
            )
            .expect("remove 调用应成功");
        assert_eq!(removed, Value::Bool(true));
    }

    #[test]
    fn plan_list_returns_steps_with_emoji() {
        let mut interp = Interpreter::new();
        let steps = vec![Value::Dict({
            let mut d = std::collections::HashMap::new();
            d.insert("id".to_string(), Value::String("a".to_string()));
            d.insert("text".to_string(), Value::String("A".to_string()));
            d
        })];
        interp
            .call_plan_method(
                "create",
                &[Value::String("p".to_string()), Value::List(steps)],
            )
            .unwrap();
        let list = interp
            .call_plan_method("list", &[Value::String("p".to_string())])
            .expect("list steps");
        match list {
            Value::List(items) => {
                assert_eq!(items.len(), 1);
                match &items[0] {
                    Value::Dict(d) => {
                        let emoji = d.get("emoji").expect("emoji 字段应存在");
                        match emoji {
                            Value::String(s) => assert_eq!(s, "⬜"), // pending default
                            _ => panic!("expected emoji String"),
                        }
                    }
                    _ => panic!("expected Dict"),
                }
            }
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn plan_info_reports_counts() {
        let mut interp = Interpreter::new();
        let steps = vec![
            Value::Dict({
                let mut d = std::collections::HashMap::new();
                d.insert("id".to_string(), Value::String("a".to_string()));
                d.insert("text".to_string(), Value::String("A".to_string()));
                d.insert("status".to_string(), Value::String("done".to_string()));
                d
            }),
            Value::Dict({
                let mut d = std::collections::HashMap::new();
                d.insert("id".to_string(), Value::String("b".to_string()));
                d.insert("text".to_string(), Value::String("B".to_string()));
                d
            }),
        ];
        interp
            .call_plan_method(
                "create",
                &[Value::String("p".to_string()), Value::List(steps)],
            )
            .unwrap();
        let info = interp
            .call_plan_method("info", &[Value::String("p".to_string())])
            .expect("info 调用应成功");
        match info {
            Value::Dict(d) => {
                let total = d.get("total").expect("total 字段应存在");
                match total {
                    Value::Float(n) => assert_eq!(*n, 2.0),
                    _ => panic!("expected Number"),
                }
                let done = d.get("done").expect("done 字段应存在");
                match done {
                    Value::Float(n) => assert_eq!(*n, 1.0),
                    _ => panic!("expected Number"),
                }
                let pending = d.get("pending").expect("pending");
                match pending {
                    Value::Float(n) => assert_eq!(*n, 1.0),
                    _ => panic!("expected Number"),
                }
                let ratio = d.get("completion_ratio").expect("ratio 字段应存在");
                match ratio {
                    Value::Float(n) => assert_eq!(*n, 0.5),
                    _ => panic!("expected Number"),
                }
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn plan_unknown_method_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_plan_method("nope", &[])
            .expect_err("unknown method should fail");
        assert!(err.contains("unknown method"), "got: {}", err);
    }
}
