//! v0.92: 从 builtins/mod.rs 拆出的测试组（P1.1 god module 拆分）。

#![allow(unused_mut)]

mod tests_v042_capability {
    use crate::interpreter::Interpreter;
    // `let mut interp` is needed: tests mutate `interp.persist.audit_sink` etc.
    // across multiple assertions, though some tests in this module don't mutate.
    use crate::value::Value;

    /// v0.42.0: sandbox.key + sandbox.check_call builtin 测试

    #[test]
    fn sandbox_key_returns_token_id_number() {
        let interp = Interpreter::new();
        let args = vec![
            Value::String("file.read".to_string()),
            Value::String("web.fetch".to_string()),
        ];
        let token_id = interp
            .call_sandbox_method("key", &args)
            .expect("sandbox.key should succeed");
        match token_id {
            Value::Float(n) => assert_eq!(n, 0.0, "first token_id should be 0"),
            other => panic!("expected Value::Float, got {:?}", other),
        }
        assert_eq!(interp.sandbox.sandbox.capabilities.token_count(), 1);
    }

    #[test]
    fn sandbox_key_with_no_caps_returns_token() {
        // 空 args 也是合法: 创建一个空 capability 集合 (拒绝一切)
        let interp = Interpreter::new();
        let token_id = interp
            .call_sandbox_method("key", &[])
            .expect("sandbox.key with no args should succeed");
        assert!(matches!(token_id, Value::Float(_)));
        // 空 token 任何 cap 都应被拒绝
        let check = interp
            .call_sandbox_method(
                "check_call",
                &[token_id.clone(), Value::String("file.read".to_string())],
            )
            .expect("check_call should not error");
        assert_eq!(check, Value::Bool(false));
    }

    #[test]
    fn sandbox_key_rejects_unknown_capability_string() {
        let interp = Interpreter::new();
        let args = vec![Value::String("not.a.real.cap".to_string())];
        let err = interp
            .call_sandbox_method("key", &args)
            .expect_err("sandbox.key with unknown cap should error");
        assert!(err.contains("unknown capability"), "got: {}", err);
        assert_eq!(interp.sandbox.sandbox.capabilities.token_count(), 0);
    }

    #[test]
    fn sandbox_key_rejects_non_string_arg() {
        let interp = Interpreter::new();
        let args = vec![Value::Float(42.0)];
        let err = interp
            .call_sandbox_method("key", &args)
            .expect_err("sandbox.key with non-string arg should error");
        assert!(err.contains("capability strings"), "got: {}", err);
    }

    #[test]
    fn sandbox_check_call_authorizes_granted_capability() {
        let interp = Interpreter::new();
        let token_id = interp
            .call_sandbox_method("key", &[Value::String("file.read".to_string())])
            .expect("issue token");

        let authorized = interp
            .call_sandbox_method(
                "check_call",
                &[token_id.clone(), Value::String("file.read".to_string())],
            )
            .expect("check_call");
        assert_eq!(authorized, Value::Bool(true));

        let denied = interp
            .call_sandbox_method(
                "check_call",
                &[token_id, Value::String("file.write".to_string())],
            )
            .expect("check_call");
        assert_eq!(denied, Value::Bool(false));
    }

    #[test]
    fn sandbox_check_call_with_unknown_token_returns_false() {
        let interp = Interpreter::new();
        let result = interp
            .call_sandbox_method(
                "check_call",
                &[Value::Float(9999.0), Value::String("file.read".to_string())],
            )
            .expect("check_call should not error, just return false");
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn sandbox_check_call_with_unknown_capability_string_errors() {
        let interp = Interpreter::new();
        let token_id = interp
            .call_sandbox_method("key", &[Value::String("file.read".to_string())])
            .expect("issue 调用应成功");
        let err = interp
            .call_sandbox_method(
                "check_call",
                &[token_id, Value::String("not.a.cap".to_string())],
            )
            .expect_err("unknown cap should error");
        assert!(err.contains("unknown capability"), "got: {}", err);
    }

    #[test]
    fn sandbox_revoke_bumps_generation() {
        let interp = Interpreter::new();
        let token_id = interp
            .call_sandbox_method("key", &[Value::String("file.read".to_string())])
            .expect("issue 调用应成功");
        let token_id_num = match &token_id {
            Value::Float(n) => *n as u64,
            _ => panic!("expected Number"),
        };

        // revoke 前 check_call 返回 true
        let before = interp
            .call_sandbox_method(
                "check_call",
                &[token_id.clone(), Value::String("file.read".to_string())],
            )
            .expect("check_call");
        assert_eq!(before, Value::Bool(true));

        // revoke
        let revoked = interp
            .call_sandbox_method("revoke", std::slice::from_ref(&token_id))
            .expect("revoke 调用应成功");
        assert_eq!(revoked, Value::Bool(true));

        // v0.49.0: generation 在 store 全局 bump (不放在 token 上)
        assert_eq!(interp.sandbox.sandbox.capabilities.current_generation(), 1);
        // token 仍存在 (loongclaw-style: 不删除)
        assert!(
            interp
                .sandbox
                .sandbox
                .capabilities
                .get(token_id_num)
                .is_some()
        );

        // v0.49.0: revoked token 在 check_call 时返回 false (TokenNotFound,
        // 因为 token.generation != current_generation)
        let after = interp
            .call_sandbox_method(
                "check_call",
                &[token_id, Value::String("file.read".to_string())],
            )
            .expect("check_call");
        assert_eq!(
            after,
            Value::Bool(false),
            "v0.49.0: revoked token must fail check_call"
        );
    }

    #[test]
    fn sandbox_token_count_tracks_unique_tokens() {
        let interp = Interpreter::new();
        assert_eq!(interp.sandbox.sandbox.capabilities.token_count(), 0);

        let _ = interp
            .call_sandbox_method("key", &[Value::String("file.read".to_string())])
            .unwrap();
        let _ = interp
            .call_sandbox_method("key", &[Value::String("web.fetch".to_string())])
            .unwrap();
        let _ = interp
            .call_sandbox_method(
                "key",
                &[
                    Value::String("memory.read".to_string()),
                    Value::String("memory.write".to_string()),
                ],
            )
            .unwrap();
        assert_eq!(interp.sandbox.sandbox.capabilities.token_count(), 3);
    }

    #[test]
    fn sandbox_old_methods_still_work() {
        // v0.42.0 增补不应破坏 v0.33-0.41 的 sandbox.mode / check_builtin / check_path
        let interp = Interpreter::new();
        let mode = interp
            .call_sandbox_method("mode", &[])
            .expect("mode 应生效");
        assert!(matches!(mode, Value::String(_)));

        let cb = interp
            .call_sandbox_method("check_builtin", &[Value::String("print".to_string())])
            .expect("check_builtin");
        assert_eq!(cb, Value::Bool(true));
    }
}
