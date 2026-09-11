//! v0.92: 从 builtins/mod.rs 拆出的测试组（P1.1 god module 拆分）。

#![allow(unused_mut)]

mod tests_v045_toolplane {
    use crate::interpreter::Interpreter;
    use crate::value::Value;

    /// v0.45.0: tool.plane.* builtin (loongclaw Core/Extension pattern)

    #[test]
    fn tool_plane_create_default_core_planes_exist() {
        let mut interp = Interpreter::new();
        let list = interp
            .call_toolplane_method("list", &[])
            .expect("list 调用应成功");
        match list {
            Value::List(names) => {
                let names_v: Vec<String> = names
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert!(
                    names_v.contains(&"ai".to_string()),
                    "should have 'ai' core plane"
                );
                assert!(
                    names_v.contains(&"sandbox".to_string()),
                    "should have 'sandbox' core plane"
                );
            }
            other => panic!("expected List, got: {:?}", other),
        }
    }

    #[test]
    fn tool_plane_create_extension() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_toolplane_method(
                "create",
                &[
                    Value::String("user_plane".to_string()),
                    Value::String("extension".to_string()),
                ],
            )
            .expect("create 调用应成功");
        assert_eq!(result, Value::Bool(true));

        let info = interp
            .call_toolplane_method("info", &[Value::String("user_plane".to_string())])
            .expect("info 调用应成功");
        match info {
            Value::Dict(d) => {
                let kind = d.get("kind").expect("kind 字段应存在");
                match kind {
                    Value::String(s) => assert_eq!(s, "extension"),
                    other => panic!("expected extension kind, got: {:?}", other),
                }
            }
            other => panic!("expected Dict, got: {:?}", other),
        }
    }

    #[test]
    fn tool_plane_register_and_find() {
        let mut interp = Interpreter::new();
        interp
            .call_toolplane_method(
                "create",
                &[
                    Value::String("p".to_string()),
                    Value::String("core".to_string()),
                ],
            )
            .unwrap();
        interp
            .call_toolplane_method(
                "register",
                &[
                    Value::String("p".to_string()),
                    Value::String("mytool".to_string()),
                    Value::String("does something".to_string()),
                    Value::String(r#"{"type":"object"}"#.to_string()),
                ],
            )
            .expect("register");

        let tools = interp
            .call_toolplane_method("list_tools", &[Value::String("p".to_string())])
            .expect("list_tools");
        match tools {
            Value::List(items) => {
                let names: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert!(names.contains(&"mytool".to_string()));
            }
            other => panic!("expected List, got: {:?}", other),
        }

        let found = interp
            .call_toolplane_method(
                "find",
                &[
                    Value::String("p".to_string()),
                    Value::String("mytool".to_string()),
                ],
            )
            .expect("find 调用应成功");
        match found {
            Value::Dict(d) => {
                let desc = d.get("description").expect("description");
                match desc {
                    Value::String(s) => assert_eq!(s, "does something"),
                    _ => panic!("expected String"),
                }
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn tool_plane_register_duplicate_tool_fails() {
        let mut interp = Interpreter::new();
        interp
            .call_toolplane_method(
                "create",
                &[
                    Value::String("p".to_string()),
                    Value::String("core".to_string()),
                ],
            )
            .unwrap();
        interp
            .call_toolplane_method(
                "register",
                &[
                    Value::String("p".to_string()),
                    Value::String("dup".to_string()),
                    Value::String("".to_string()),
                    Value::String("{}".to_string()),
                ],
            )
            .unwrap();
        let err = interp
            .call_toolplane_method(
                "register",
                &[
                    Value::String("p".to_string()),
                    Value::String("dup".to_string()),
                    Value::String("".to_string()),
                    Value::String("{}".to_string()),
                ],
            )
            .expect_err("duplicate should fail");
        assert!(err.contains("already exists"), "got: {}", err);
    }

    #[test]
    fn tool_plane_unknown_method_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_toolplane_method("nope", &[])
            .expect_err("unknown method should fail");
        assert!(err.contains("unknown method"), "got: {}", err);
    }

    #[test]
    fn tool_plane_remove_plane() {
        let mut interp = Interpreter::new();
        interp
            .call_toolplane_method(
                "create",
                &[
                    Value::String("p".to_string()),
                    Value::String("core".to_string()),
                ],
            )
            .unwrap();
        let removed = interp
            .call_toolplane_method("remove", &[Value::String("p".to_string())])
            .expect("remove 调用应成功");
        assert_eq!(removed, Value::Bool(true));

        let info = interp
            .call_toolplane_method("info", &[Value::String("p".to_string())])
            .expect("info 调用应成功");
        assert_eq!(info, Value::Nil);
    }
}
