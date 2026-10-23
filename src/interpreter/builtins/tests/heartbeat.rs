//! v0.92: 从 builtins/mod.rs 拆出的测试组（P1.1 god module 拆分）。

#![allow(unused_mut)]

mod tests_v047_heartbeat {
    use crate::interpreter::Interpreter;
    use crate::test_support::TempDir;
    use crate::value::Value;

    /// 返回 **(守卫, 路径)** —— 守卫必须活得比路径久（v0.104.6 D475）。
    fn write_heartbeat(name: &str, content: &str) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new("mora_hb_test_");
        let path = dir.join(format!("{}.md", name));
        std::fs::write(&path, content).unwrap();
        (dir, path)
    }

    #[test]
    fn ai_heartbeat_real_file_returns_report() {
        let mut interp = Interpreter::new();
        let content = r#"# Heartbeat
- [x] first done
- [ ] second pending
- [x] third done
- [ ] fourth pending
"#;
        let (_guard, path) = write_heartbeat("HB", content);
        let result = interp
            .call_ai_method(
                "heartbeat",
                &[Value::String(path.to_string_lossy().to_string())],
            )
            .expect("heartbeat");
        match result {
            Value::Dict(d) => {
                let total = d.get("total").expect("total 字段应存在");
                match total {
                    Value::Float(n) => assert_eq!(*n, 4.0),
                    _ => panic!("expected Number"),
                }
                let done = d.get("done").expect("done 字段应存在");
                match done {
                    Value::Float(n) => assert_eq!(*n, 2.0),
                    _ => panic!("expected Number"),
                }
                let pending = d.get("pending").expect("pending");
                match pending {
                    Value::Float(n) => assert_eq!(*n, 2.0),
                    _ => panic!("expected Number"),
                }
                let ratio = d.get("completion_ratio").expect("ratio 字段应存在");
                match ratio {
                    Value::Float(n) => assert_eq!(*n, 0.5),
                    _ => panic!("expected Number"),
                }
                let complete = d.get("is_complete").expect("complete");
                assert_eq!(*complete, Value::Bool(false));
            }
            _ => panic!("expected Dict"),
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn ai_heartbeat_all_done_is_complete() {
        let mut interp = Interpreter::new();
        let content = "- [x] a\n- [X] b\n- [x] c\n";
        let (_guard, path) = write_heartbeat("all_done", content);
        let result = interp
            .call_ai_method(
                "heartbeat",
                &[Value::String(path.to_string_lossy().to_string())],
            )
            .expect("heartbeat");
        match result {
            Value::Dict(d) => {
                let complete = d.get("is_complete").expect("complete");
                assert_eq!(*complete, Value::Bool(true));
            }
            _ => panic!("expected Dict"),
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn ai_heartbeat_empty_heartbeat_is_vacuously_complete() {
        let mut interp = Interpreter::new();
        let content = "# only heading\nno checklist items\n";
        let (_guard, path) = write_heartbeat("empty", content);
        let result = interp
            .call_ai_method(
                "heartbeat",
                &[Value::String(path.to_string_lossy().to_string())],
            )
            .expect("heartbeat");
        match result {
            Value::Dict(d) => {
                let total = d.get("total").expect("total 字段应存在");
                match total {
                    Value::Float(n) => assert_eq!(*n, 0.0),
                    _ => panic!("expected Number"),
                }
                let complete = d.get("is_complete").expect("complete");
                assert_eq!(*complete, Value::Bool(true));
            }
            _ => panic!("expected Dict"),
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn ai_heartbeat_nonexistent_file_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_ai_method(
                "heartbeat",
                &[Value::String("/nonexistent/HEARTBEAT.md".to_string())],
            )
            .expect_err("nonexistent should fail");
        assert!(err.contains("ai.heartbeat"), "got: {}", err);
    }

    #[test]
    fn ai_heartbeat_items_list_contains_text_and_done() {
        let mut interp = Interpreter::new();
        let content = "- [x] task A\n- [ ] task B\n";
        let (_guard, path) = write_heartbeat("items", content);
        let result = interp
            .call_ai_method(
                "heartbeat",
                &[Value::String(path.to_string_lossy().to_string())],
            )
            .expect("heartbeat");
        match result {
            Value::Dict(d) => {
                let items = d.get("items").expect("items 字段应存在");
                match items {
                    Value::List(items) => {
                        assert_eq!(items.len(), 2);
                        match &items[0] {
                            Value::Dict(item) => {
                                let done = item.get("done").expect("done 字段应存在");
                                assert_eq!(*done, Value::Bool(true));
                            }
                            _ => panic!("expected Dict"),
                        }
                    }
                    _ => panic!("expected List"),
                }
            }
            _ => panic!("expected Dict"),
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
