//! v0.92: 从 builtins/mod.rs 拆出的测试组（P1.1 god module 拆分）。

#![allow(unused_mut)]

mod tests_v045_ai {
    use crate::interpreter::Interpreter;
    use crate::value::Value;

    /// v0.45.0: ai.retry / ai.role builtin (mini-swe-agent + OpenFugu)

    #[test]
    fn ai_retry_returns_schedule_dict() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_ai_method(
                "retry",
                &[Value::String("5".to_string()), Value::Float(100.0)],
            )
            .expect("retry 调用应成功");
        match result {
            Value::Dict(d) => {
                let attempts = d.get("attempts").expect("attempts");
                match attempts {
                    Value::Float(n) => assert_eq!(*n, 5.0),
                    _ => panic!("expected Number attempts"),
                }
                let backoff_ms = d.get("backoff_ms").expect("backoff_ms");
                match backoff_ms {
                    Value::Float(n) => assert_eq!(*n, 100.0),
                    _ => panic!("expected Number backoff_ms"),
                }
                let schedule = d.get("schedule").expect("schedule");
                match schedule {
                    Value::List(items) => {
                        assert_eq!(items.len(), 5, "schedule should have 5 entries")
                    }
                    _ => panic!("expected List schedule"),
                }
            }
            other => panic!("expected Dict, got: {:?}", other),
        }
    }

    #[test]
    fn ai_retry_exponential_schedule_grows() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_ai_method(
                "retry",
                &[
                    Value::String("4".to_string()),
                    Value::Float(100.0),
                    Value::String("exponential".to_string()),
                ],
            )
            .expect("retry 调用应成功");
        match result {
            Value::Dict(d) => {
                let schedule = d.get("schedule").expect("schedule");
                if let Value::List(items) = schedule {
                    let nums: Vec<f64> = items
                        .iter()
                        .filter_map(|v| match v {
                            Value::Float(n) => Some(*n),
                            _ => None,
                        })
                        .collect();
                    // exponential: 100, 200, 400, 800
                    assert_eq!(nums, vec![100.0, 200.0, 400.0, 800.0]);
                }
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn ai_retry_rejects_zero_attempts() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_ai_method("retry", &[Value::String("0".to_string())])
            .expect_err("zero attempts should fail");
        assert!(err.contains("attempts must be > 0"), "got: {}", err);
    }

    #[test]
    fn ai_role_accepts_main_three_roles() {
        let mut interp = Interpreter::new();
        for role in ["worker", "thinker", "verifier"] {
            let result = interp
                .call_ai_method("role", &[Value::String(role.to_string())])
                .expect("role 调用应成功");
            match result {
                Value::String(s) => assert_eq!(s, role),
                _ => panic!("expected String"),
            }
        }
    }

    #[test]
    fn ai_role_accepts_custom_role() {
        // OpenFugu has 3 main roles but custom roles also OK
        let mut interp = Interpreter::new();
        let result = interp
            .call_ai_method("role", &[Value::String("explorer".to_string())])
            .expect("role 调用应成功");
        match result {
            Value::String(s) => assert_eq!(s, "explorer"),
            _ => panic!("expected String"),
        }
    }

    #[test]
    fn ai_role_requires_arg() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_ai_method("role", &[])
            .expect_err("no arg should fail");
        assert!(err.contains("requires role name"), "got: {}", err);
    }

    #[test]
    fn ai_unknown_method_errors() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_ai_method("nope", &[])
            .expect_err("unknown method should fail");
        assert!(err.contains("unknown method"), "got: {}", err);
    }
}

mod tests_v047_context {
    use crate::interpreter::Interpreter;
    use crate::value::Value;

    /// v0.47.0: ai.context.trim + ai.context.info (pi-agent + AgentMesh pattern)

    #[test]
    fn ai_context_info_returns_window_state() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_ai_method("context.info", &[])
            .expect("context.info");
        match result {
            Value::Dict(d) => {
                let max = d.get("max_tokens").expect("max_tokens");
                match max {
                    Value::Float(n) => assert_eq!(*n, 4096.0, "default max"),
                    _ => panic!("expected Number"),
                }
                let msgs = d.get("messages").expect("messages");
                match msgs {
                    Value::Float(n) => assert_eq!(*n, 0.0, "default empty"),
                    _ => panic!("expected Number"),
                }
            }
            _ => panic!("expected Dict"),
        }
    }

    #[test]
    fn ai_context_trim_empty_drops_zero() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_ai_method("context.trim", &[])
            .expect("context.trim");
        match result {
            Value::Float(n) => assert_eq!(n, 0.0, "empty context drops 0 tokens"),
            _ => panic!("expected Number"),
        }
    }

    #[test]
    fn ai_context_trim_validates_threshold_range() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_ai_method("context.trim", &[Value::Float(1.5)])
            .expect_err("1.5 should fail");
        assert!(err.contains("0.0-1.0"), "got: {}", err);

        let err2 = interp
            .call_ai_method("context.trim", &[Value::Float(-0.1)])
            .expect_err("-0.1 should fail");
        assert!(err2.contains("0.0-1.0"), "got: {}", err2);
    }

    #[test]
    fn ai_context_trim_accepts_valid_threshold() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_ai_method("context.trim", &[Value::Float(0.5)])
            .expect("should succeed");
        match result {
            Value::Float(_) => {}
            _ => panic!("expected Number"),
        }
    }
}
