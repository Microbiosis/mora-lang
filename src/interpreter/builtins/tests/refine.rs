//! v0.92: 从 builtins/mod.rs 拆出的测试组（P1.1 god module 拆分）。

#![allow(unused_mut)]

mod tests_v048_refine {
    use crate::interpreter::Interpreter;
    use crate::value::Value;

    /// v0.48.0: mora.refine + mora.refine_info + mora.list_refines (CLI-Anything /refine)
    use std::time::UNIX_EPOCH;

    fn write_temp_script(name: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mora_refine_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn mora_refine_real_file_creates_refined_copy() {
        let mut interp = Interpreter::new();
        let script = write_temp_script("demo.mora", "task main()\n  print(\"hi\")\n");
        let result = interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(script.to_string_lossy().to_string()),
                    Value::String("add greeting".to_string()),
                ],
            )
            .expect("refine 调用应成功");
        match result {
            Value::Dict(d) => {
                let iter = d.get("iteration").expect("iteration");
                match iter {
                    Value::Float(n) => assert_eq!(*n, 1.0),
                    _ => panic!("expected Number"),
                }
                let refined = d.get("refined").expect("refined");
                match refined {
                    Value::String(s) => assert!(s.contains(".refined.1.mora")),
                    _ => panic!("expected String"),
                }
            }
            _ => panic!("expected Dict"),
        }

        // 验证 .refine/ 目录存在 + 副本可读
        let refine_dir = script.parent().unwrap().join("demo.refine");
        assert!(refine_dir.exists(), ".refine/ should be created");
        let refined_path = refine_dir.join("demo.refined.1.mora");
        assert!(refined_path.exists(), "refined copy should exist");
        let content = std::fs::read_to_string(&refined_path).unwrap();
        assert!(content.contains("add greeting"));
        assert!(content.contains("task main()"));

        let _ = std::fs::remove_dir_all(script.parent().unwrap());
    }

    #[test]
    fn mora_refine_iteration_increments() {
        let mut interp = Interpreter::new();
        let script = write_temp_script("iter.mora", "x\n");
        for i in 1..=3 {
            let result = interp
                .call_mora_method(
                    "refine",
                    &[
                        Value::String(script.to_string_lossy().to_string()),
                        Value::String(format!("iter {}", i)),
                    ],
                )
                .expect("refine 调用应成功");
            match result {
                Value::Dict(d) => {
                    let iter = d.get("iteration").expect("iteration");
                    match iter {
                        Value::Float(n) => assert_eq!(*n, i as f64),
                        _ => panic!("expected Number"),
                    }
                }
                _ => panic!("expected Dict"),
            }
        }
        let _ = std::fs::remove_dir_all(script.parent().unwrap());
    }

    /// v0.75.8: mora.refine 第 3 参 count → 返回 List[Dict]（多候选）
    #[test]
    fn mora_refine_many_returns_list() {
        let mut interp = Interpreter::new();
        let script = write_temp_script("many.mora", "x\n");
        let result = interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(script.to_string_lossy().to_string()),
                    Value::String("add variants".to_string()),
                    Value::Float(3.0),
                ],
            )
            .expect("refine_many");
        match result {
            Value::List(items) => {
                assert_eq!(items.len(), 3, "3 个候选");
                for item in &items {
                    match item {
                        Value::Dict(d) => assert!(d.contains_key("refined")),
                        _ => panic!("expected Dict in List"),
                    }
                }
            }
            _ => panic!("expected List for 3-arg refine"),
        }
        // 2 参仍返回单个 Dict（兼容）
        let single = interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(script.to_string_lossy().to_string()),
                    Value::String("again".to_string()),
                ],
            )
            .expect("refine 2-arg");
        assert!(matches!(single, Value::Dict(_)), "2 参应返回 Dict");
        let _ = std::fs::remove_dir_all(script.parent().unwrap());
    }

    #[test]
    fn mora_refine_info_returns_latest() {
        let mut interp = Interpreter::new();
        let script = write_temp_script("info.mora", "x\n");
        interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(script.to_string_lossy().to_string()),
                    Value::String("first".to_string()),
                ],
            )
            .unwrap();
        let info = interp
            .call_mora_method(
                "refine_info",
                &[Value::String(script.to_string_lossy().to_string())],
            )
            .expect("refine_info");
        match info {
            Value::Dict(d) => {
                let inst = d.get("instruction").expect("instruction");
                match inst {
                    Value::String(s) => assert_eq!(s, "first"),
                    _ => panic!("expected String"),
                }
            }
            _ => panic!("expected Dict"),
        }
        let _ = std::fs::remove_dir_all(script.parent().unwrap());
    }

    #[test]
    fn mora_refine_info_specific_iteration() {
        let mut interp = Interpreter::new();
        let script = write_temp_script("specific.mora", "x\n");
        interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(script.to_string_lossy().to_string()),
                    Value::String("v1".to_string()),
                ],
            )
            .unwrap();
        interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(script.to_string_lossy().to_string()),
                    Value::String("v2".to_string()),
                ],
            )
            .unwrap();
        let info = interp
            .call_mora_method(
                "refine_info",
                &[
                    Value::String(script.to_string_lossy().to_string()),
                    Value::Float(1.0),
                ],
            )
            .expect("第 1 轮迭代应完成");
        match info {
            Value::Dict(d) => {
                let inst = d.get("instruction").expect("instruction");
                match inst {
                    Value::String(s) => assert_eq!(s, "v1"),
                    _ => panic!("expected String"),
                }
            }
            _ => panic!("expected Dict"),
        }
        let _ = std::fs::remove_dir_all(script.parent().unwrap());
    }

    #[test]
    fn mora_list_refines_lists_all_scripts() {
        let mut interp = Interpreter::new();
        let s1 = write_temp_script("s1.mora", "1\n");
        let s2 = write_temp_script("s2.mora", "2\n");
        interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(s1.to_string_lossy().to_string()),
                    Value::String("a".to_string()),
                ],
            )
            .unwrap();
        interp
            .call_mora_method(
                "refine",
                &[
                    Value::String(s2.to_string_lossy().to_string()),
                    Value::String("b".to_string()),
                ],
            )
            .unwrap();
        let list = interp
            .call_mora_method("list_refines", &[])
            .expect("list_refines");
        match list {
            Value::List(items) => {
                let paths: Vec<String> = items
                    .into_iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                assert_eq!(paths.len(), 2);
                assert!(paths.iter().any(|p| p.contains("s1.mora")));
                assert!(paths.iter().any(|p| p.contains("s2.mora")));
            }
            _ => panic!("expected List"),
        }
        let _ = std::fs::remove_dir_all(s1.parent().unwrap());
        let _ = std::fs::remove_dir_all(s2.parent().unwrap());
    }

    #[test]
    fn mora_refine_nonexistent_script_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_mora_method(
                "refine",
                &[
                    Value::String("/nonexistent/foo.mora".to_string()),
                    Value::String("x".to_string()),
                ],
            )
            .expect_err("nonexistent should fail");
        assert!(err.contains("mora.refine"), "got: {}", err);
    }

    #[test]
    fn mora_unknown_method_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_mora_method("nope", &[])
            .expect_err("unknown method should fail");
        assert!(err.contains("unknown method"), "got: {}", err);
    }
}
