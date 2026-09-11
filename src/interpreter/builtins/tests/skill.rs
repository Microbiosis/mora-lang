//! v0.92: 从 builtins/mod.rs 拆出的测试组（P1.1 god module 拆分）。

#![allow(unused_mut)]

mod tests_v046_skill {
    use crate::interpreter::Interpreter;
    use crate::value::Value;

    /// v0.46.0: skill.* builtin (CLI-Anything SKILL.md pattern)
    fn write_temp_skill_file(name: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mora_skill_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.md", name));
        std::fs::write(&path, content).unwrap();
        path
    }

    use std::time::UNIX_EPOCH;

    #[test]
    fn skill_list_empty_by_default() {
        let mut interp = Interpreter::new();
        let list = interp
            .call_skill_method("list", &[])
            .expect("list 调用应成功");
        match list {
            Value::List(items) => assert_eq!(items.len(), 0),
            other => panic!("expected List, got: {:?}", other),
        }
    }

    #[test]
    fn skill_install_registers_skill() {
        let mut interp = Interpreter::new();
        let content = r#"---
name: my-skill
description: A test skill
trigger: test.*
---

This is the body of my-skill.
"#;
        let result = interp
            .call_skill_method(
                "install",
                &[
                    Value::String("my-skill".to_string()),
                    Value::String(content.to_string()),
                ],
            )
            .expect("install");
        assert_eq!(result, Value::Bool(true));

        let list = interp
            .call_skill_method("list", &[])
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
                assert!(names.contains(&"my-skill".to_string()));
            }
            other => panic!("expected List, got: {:?}", other),
        }
    }

    #[test]
    fn skill_find_returns_full_spec() {
        let mut interp = Interpreter::new();
        let content = "---
name: finder-skill
description: Helps find things
trigger: find.*
---

# Body
Find things here.
";
        interp
            .call_skill_method(
                "install",
                &[
                    Value::String("finder-skill".to_string()),
                    Value::String(content.to_string()),
                ],
            )
            .unwrap();
        let found = interp
            .call_skill_method("find", &[Value::String("finder-skill".to_string())])
            .expect("find 调用应成功");
        match found {
            Value::Dict(d) => {
                let name = d.get("name").expect("name 字段应存在");
                match name {
                    Value::String(s) => assert_eq!(s, "finder-skill"),
                    _ => panic!("expected name String"),
                }
                let desc = d.get("description").expect("description");
                match desc {
                    Value::String(s) => assert!(s.contains("find things")),
                    _ => panic!("expected desc String"),
                }
                let trigger = d.get("trigger").expect("trigger");
                match trigger {
                    Value::String(s) => assert_eq!(s, "find.*"),
                    _ => panic!("expected trigger String"),
                }
                let body = d.get("body").expect("body 字段应存在");
                match body {
                    Value::String(s) => assert!(s.contains("Find things here")),
                    _ => panic!("expected body String"),
                }
            }
            other => panic!("expected Dict, got: {:?}", other),
        }
    }

    #[test]
    fn skill_find_unknown_returns_nil() {
        let mut interp = Interpreter::new();
        let found = interp
            .call_skill_method("find", &[Value::String("nope".to_string())])
            .expect("find 调用应成功");
        assert_eq!(found, Value::Nil);
    }

    #[test]
    fn skill_load_real_skill_md_file() {
        let mut interp = Interpreter::new();
        let content = r#"---
name: file-loaded
description: Loaded from file
---

This skill was loaded from a real file on disk.
"#;
        let path = write_temp_skill_file("file-loaded", content);

        let result = interp
            .call_skill_method("load", &[Value::String(path.to_string_lossy().to_string())])
            .expect("load 调用应成功");
        assert_eq!(result, Value::Bool(true));

        let list = interp
            .call_skill_method("list", &[])
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
                assert!(names.contains(&"file-loaded".to_string()));
            }
            _ => panic!("expected List"),
        }

        let found = interp
            .call_skill_method("find", &[Value::String("file-loaded".to_string())])
            .expect("find 调用应成功");
        match found {
            Value::Dict(d) => {
                let src = d.get("source").expect("source 字段应存在");
                match src {
                    Value::String(s) => assert!(s.contains("file-loaded.md"), "got: {}", s),
                    _ => panic!("expected source path String"),
                }
            }
            _ => panic!("expected Dict"),
        }

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn skill_load_nonexistent_file_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_skill_method("load", &[Value::String("/nonexistent/foo.md".to_string())])
            .expect_err("nonexistent should fail");
        assert!(err.contains("skill.load"), "got: {}", err);
    }

    #[test]
    fn skill_uninstall_removes() {
        let mut interp = Interpreter::new();
        let content = "---
name: temp
description: temporary
---

body
";
        interp
            .call_skill_method(
                "install",
                &[
                    Value::String("temp".to_string()),
                    Value::String(content.to_string()),
                ],
            )
            .unwrap();
        let removed = interp
            .call_skill_method("uninstall", &[Value::String("temp".to_string())])
            .expect("uninstall");
        assert_eq!(removed, Value::Bool(true));
        let found = interp
            .call_skill_method("find", &[Value::String("temp".to_string())])
            .expect("find 调用应成功");
        assert_eq!(found, Value::Nil);
    }

    #[test]
    fn skill_set_hub_and_refresh_real_file() {
        let mut interp = Interpreter::new();
        let dir = std::env::temp_dir().join(format!(
            "mora_hub_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let hub_path = dir.join("mora-public.json");
        let hub_content = r#"{
  "skills": [
    {"name": "hub-skill-a", "description": "Hub A"},
    {"name": "hub-skill-b", "description": "Hub B"}
  ]
}"#;
        std::fs::write(&hub_path, hub_content).unwrap();

        let set = interp
            .call_skill_method(
                "set_hub",
                &[Value::String(hub_path.to_string_lossy().to_string())],
            )
            .expect("set_hub");
        assert_eq!(set, Value::Bool(true));

        let count = interp
            .call_skill_method("refresh_hub", &[])
            .expect("refresh_hub");
        match count {
            Value::Float(n) => assert!(n >= 1.0, "expected at least 1 hub entry, got {}", n),
            other => panic!("expected Number, got: {:?}", other),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skill_unknown_method_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_skill_method("nope", &[])
            .expect_err("unknown method should fail");
        assert!(err.contains("unknown method"), "got: {}", err);
    }
}
