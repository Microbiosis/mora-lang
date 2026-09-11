//! v0.92: 从 builtins/mod.rs 拆出的测试组（P1.1 god module 拆分）。

#![allow(unused_mut)]

mod tests_v044_container_real {
    use crate::interpreter::Interpreter;
    // Tests use `let mut interp = ...` pattern uniformly; some tests don't actually need mut.
    // Allow unused_mut for the whole module to avoid 5 false positives.

    use crate::value::Value;

    /// v0.44.0: REAL Docker container builtin integration
    /// **Requires Docker daemon** — 默认 #[ignore] 让 CI 无 docker 时跳过
    fn cleanup_container(interp: &mut Interpreter) {
        // 尽力清理 (可能根本没 spawn 成功)
        let _ = interp.call_sandbox_method("container_clear", &[]);
    }

    #[test]
    #[ignore = "requires Docker daemon (run with --ignored)"]
    fn sandbox_containerize_real_spawn() {
        let mut interp = Interpreter::new();
        let result = interp
            .call_sandbox_method("containerize", &[Value::String("docker".to_string())])
            .expect("containerize should spawn docker");
        // 返回 Number (container_id hash)
        match result {
            Value::Float(n) => assert!(n > 0.0, "container_id hash should be non-zero"),
            other => panic!("expected Number, got: {:?}", other),
        }
        assert!(
            interp
                .sandbox
                .container
                .lock()
                .expect("container poisoned")
                .is_some()
        );
        cleanup_container(&mut interp);
        assert!(
            interp
                .sandbox
                .container
                .lock()
                .expect("container poisoned")
                .is_none()
        );
    }

    #[test]
    #[ignore = "requires Docker daemon (run with --ignored)"]
    fn sandbox_container_exec_runs_cmd_inside_container() {
        let mut interp = Interpreter::new();
        interp
            .call_sandbox_method("containerize", &[Value::String("docker".to_string())])
            .unwrap();
        let result = interp
            .call_sandbox_method(
                "container_exec",
                &[
                    Value::String("echo".to_string()),
                    Value::String("hello-from-real-docker".to_string()),
                ],
            )
            .expect("container_exec should succeed");
        match result {
            Value::Dict(d) => {
                let stdout = match d.get("stdout") {
                    Some(Value::String(s)) => s.clone(),
                    other => panic!("expected stdout String, got: {:?}", other),
                };
                assert!(
                    stdout.contains("hello-from-real-docker"),
                    "stdout should contain 'hello-from-real-docker', got: {}",
                    stdout
                );
                let exit_code = d.get("exit_code").expect("exit_code");
                assert!(
                    matches!(exit_code, Value::Float(0.0)),
                    "exit_code should be 0, got: {:?}",
                    exit_code
                );
            }
            other => panic!("expected Dict, got: {:?}", other),
        }
        cleanup_container(&mut interp);
    }

    #[test]
    #[ignore = "requires Docker daemon (run with --ignored)"]
    fn sandbox_container_info_returns_real_container_id() {
        let mut interp = Interpreter::new();
        interp
            .call_sandbox_method("containerize", &[Value::String("docker".to_string())])
            .unwrap();
        let info = interp
            .call_sandbox_method("container_info", &[])
            .expect("container_info");
        match info {
            Value::Dict(d) => {
                let id = match d.get("container_id") {
                    Some(Value::String(s)) => s.clone(),
                    other => panic!("expected container_id String, got: {:?}", other),
                };
                assert!(
                    id.len() >= 12,
                    "docker container_id hex should be >= 12 chars: {}",
                    id
                );
                let name = d.get("container_name").expect("container_name");
                match name {
                    Value::String(s) => assert!(
                        s.starts_with("mora-"),
                        "name should start with mora-, got: {}",
                        s
                    ),
                    other => panic!("expected String name, got: {:?}", other),
                }
                let backend = d.get("backend").expect("backend");
                match backend {
                    Value::String(s) => assert_eq!(s, "docker"),
                    other => panic!("expected docker backend, got: {:?}", other),
                }
            }
            other => panic!("expected Dict, got: {:?}", other),
        }
        cleanup_container(&mut interp);
    }

    #[test]
    #[ignore = "requires Docker daemon (run with --ignored)"]
    fn sandbox_container_clear_really_removes_container() {
        let mut interp = Interpreter::new();
        interp
            .call_sandbox_method("containerize", &[Value::String("docker".to_string())])
            .unwrap();
        let id = {
            let guard = interp.sandbox.container.lock().expect("container poisoned");
            guard.as_ref().unwrap().container_id.clone()
        };
        // 验证 container 真的在 docker 里
        let check = std::process::Command::new("docker")
            .args(["inspect", &id, "--format", "{{.State.Running}}"])
            .output()
            .expect("docker inspect");
        assert!(check.status.success(), "docker should know the container");
        let state = String::from_utf8_lossy(&check.stdout).trim().to_string();
        assert_eq!(state, "true", "container should be running");

        // clear → 真 docker rm -f
        let cleared = interp
            .call_sandbox_method("container_clear", &[])
            .expect("clear 调用应成功");
        assert_eq!(cleared, Value::Bool(true));

        // 验证 container 真的没了
        let check2 = std::process::Command::new("docker")
            .args(["inspect", &id, "--format", "{{.State.Running}}"])
            .output()
            .expect("docker inspect");
        assert!(
            !check2.status.success(),
            "docker inspect should fail for removed container"
        );
    }

    #[test]
    fn sandbox_containerize_rejects_unknown_backend() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_sandbox_method("containerize", &[Value::String("vmware".to_string())])
            .expect_err("unknown backend should fail");
        assert!(err.contains("unknown backend"), "got: {}", err);
    }

    #[test]
    fn sandbox_containerize_rejects_unimplemented_backend() {
        // gondolin/openshell 在 v0.44.0 真实未实现, 应该返回明确错误
        let mut interp = Interpreter::new();
        let err = interp
            .call_sandbox_method("containerize", &[Value::String("gondolin".to_string())])
            .expect_err("gondolin not yet implemented");
        assert!(err.contains("not yet implemented"), "got: {}", err);
    }

    #[test]
    fn sandbox_container_exec_requires_container_first() {
        let mut interp = Interpreter::new();
        let err = interp
            .call_sandbox_method("container_exec", &[Value::String("ls".to_string())])
            .expect_err("exec without container should fail");
        assert!(err.contains("no container"), "got: {}", err);
    }

    #[test]
    fn sandbox_container_info_returns_nil_when_unset() {
        let mut interp = Interpreter::new();
        let info = interp
            .call_sandbox_method("container_info", &[])
            .expect("container_info");
        assert_eq!(info, Value::Nil);
    }
}
