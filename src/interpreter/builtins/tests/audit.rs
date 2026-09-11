//! v0.92: 从 builtins/mod.rs 拆出的测试组（P1.1 god module 拆分）。

#![allow(unused_mut)]

mod tests_v0421_audit {
    use crate::interpreter::Interpreter;
    use crate::audit::{AuditSink, JsonlAuditSink};
    use crate::value::Value;
    use std::sync::Arc;

    /// v0.42.1: sandbox.audit_emit / audit_flush / audit_verify builtin tests
    fn temp_log_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mora_audit_builtin_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    use std::time::UNIX_EPOCH;

    #[test]
    fn audit_emit_writes_event_and_returns_true() {
        let mut interp = Interpreter::new();
        let path = temp_log_path("emit_basic.jsonl");
        let sink = Arc::new(JsonlAuditSink::new_fresh(&path).unwrap());
        interp.persist.audit_sink = sink.clone();

        let result = interp
            .call_sandbox_method(
                "audit_emit",
                &[
                    Value::String("user".to_string()),
                    Value::String("file.write".to_string()),
                    Value::String("/tmp/foo.txt".to_string()),
                    Value::String("{\"size\":42}".to_string()),
                ],
            )
            .expect("audit_emit");
        assert_eq!(result, Value::Bool(true));
        assert_eq!(sink.event_count(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn audit_emit_with_optional_args() {
        let mut interp = Interpreter::new();
        let path = temp_log_path("emit_minimal.jsonl");
        let sink = Arc::new(JsonlAuditSink::new_fresh(&path).unwrap());
        interp.persist.audit_sink = sink.clone();

        // 仅 actor + action
        let result = interp
            .call_sandbox_method(
                "audit_emit",
                &[
                    Value::String("agent".to_string()),
                    Value::String("chat.start".to_string()),
                ],
            )
            .expect("audit_emit minimal");
        assert_eq!(result, Value::Bool(true));
        assert_eq!(sink.event_count(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn audit_emit_validates_arg_types() {
        let interp = Interpreter::new();
        let err = interp
            .call_sandbox_method(
                "audit_emit",
                &[Value::Float(42.0), Value::String("action".to_string())],
            )
            .expect_err("non-string actor should fail");
        assert!(err.contains("actor must be a string"), "got: {}", err);
    }

    #[test]
    fn audit_emit_validates_arg_count() {
        let interp = Interpreter::new();
        let err = interp
            .call_sandbox_method(
                "audit_emit",
                &[Value::String("a".to_string())], // 只 1 个 arg
            )
            .expect_err("too few args should fail");
        assert!(err.contains("2-4 args"), "got: {}", err);
    }

    #[test]
    fn audit_flush_and_verify_chain_passes() {
        let mut interp = Interpreter::new();
        let path = temp_log_path("verify.jsonl");
        let sink = Arc::new(JsonlAuditSink::new_fresh(&path).unwrap());
        interp.persist.audit_sink = sink.clone();

        for i in 0..5 {
            interp
                .call_sandbox_method(
                    "audit_emit",
                    &[
                        Value::String("user".to_string()),
                        Value::String(format!("op.{}", i)),
                        Value::Nil,
                        Value::Nil,
                    ],
                )
                .expect("emit 调用应成功");
        }
        let flushed = interp
            .call_sandbox_method("audit_flush", &[])
            .expect("flush 调用应成功");
        assert_eq!(flushed, Value::Bool(true));

        let verified = interp
            .call_sandbox_method("audit_verify", &[])
            .expect("verify 调用应成功");
        assert_eq!(verified, Value::Bool(true));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn audit_verify_detects_tampering() {
        let mut interp = Interpreter::new();
        let path = temp_log_path("tampered.jsonl");
        let sink = Arc::new(JsonlAuditSink::new_fresh(&path).unwrap());
        interp.persist.audit_sink = sink.clone();

        for i in 0..3 {
            interp
                .call_sandbox_method(
                    "audit_emit",
                    &[
                        Value::String("a".to_string()),
                        Value::String(format!("op.{}", i)),
                        Value::Nil,
                        Value::Nil,
                    ],
                )
                .expect("emit 调用应成功");
        }
        interp
            .call_sandbox_method("audit_flush", &[])
            .expect("flush 调用应成功");
        assert_eq!(
            interp.call_sandbox_method("audit_verify", &[]).unwrap(),
            Value::Bool(true)
        );

        // 篡改 line 1
        let content = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = content.lines().map(String::from).collect();
        lines[1] = lines[1].replace("\"action\":\"op.1\"", "\"action\":\"TAMPERED\"");
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        let verified = interp.call_sandbox_method("audit_verify", &[]).unwrap();
        // 应返回 Value::String(error)
        match verified {
            Value::String(s) => assert!(
                s.contains("hash mismatch") || s.contains("HashMismatch"),
                "got: {}",
                s
            ),
            Value::Bool(true) => panic!("tamper should have been detected"),
            other => panic!("unexpected: {:?}", other),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn null_sink_default_audit_emit_returns_true() {
        // 默认 NullSink 应接受所有 audit_emit 调用
        let interp = Interpreter::new();
        let result = interp
            .call_sandbox_method(
                "audit_emit",
                &[
                    Value::String("user".to_string()),
                    Value::String("op".to_string()),
                ],
            )
            .expect("audit_emit to null sink");
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn audit_emit_writes_to_real_file_via_jsonl_sink() {
        let mut interp = Interpreter::new();
        let path = temp_log_path("real_file.jsonl");
        let sink = Arc::new(JsonlAuditSink::new_fresh(&path).unwrap());
        interp.persist.audit_sink = sink.clone();

        interp
            .call_sandbox_method(
                "audit_emit",
                &[
                    Value::String("user".to_string()),
                    Value::String("sandbox.issue".to_string()),
                    Value::Nil,
                    Value::String("{\"cap\":\"file.read\"}".to_string()),
                ],
            )
            .expect("emit 调用应成功");
        interp
            .call_sandbox_method("audit_flush", &[])
            .expect("flush 调用应成功");

        // 验证文件存在且包含期望字段
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("\"action\":\"sandbox.issue\""));
        assert!(content.contains("\"actor\":\"user\""));
        assert!(content.contains("\"payload\":\"{\\\"cap\\\":\\\"file.read\\\"}\""));
        assert!(content.contains("\"hash\":"));

        let _ = std::fs::remove_file(&path);
    }
}
