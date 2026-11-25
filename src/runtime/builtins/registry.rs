//! 函数分发清单元数据（RUNTIME_BUILTIN_NAMES 单一事实源测试锚点）。
//! 运行期分派已统一由 `src/runtime/vm/` 接管。

/// v0.104.6：运行期自由函数内建名清单，供 `tests/builtin_registry_consistency.rs`
/// 与 typeck 注册表双向核对。
pub const RUNTIME_BUILTIN_NAMES: &[&str] = &[
    "apply",
    "assert",
    "assert_eq",
    "atom",
    "bool",
    "car",
    "cdr",
    "channel",
    "compose",
    "cons",
    "context",
    "curry",
    "deref",
    "Err",
    "eval",
    "float",
    "gensym",
    "grad",
    "int",
    "into",
    "is_instance",
    "jvp",
    "kv_allocator",
    "len",
    "macroexpand",
    "memory_pool",
    "merge_with",
    "methods_of",
    "Ok",
    "partial",
    "print",
    "process_group",
    "profiler",
    "quote",
    "range",
    "read",
    "sleep",
    "str",
    "swap",
    "tail",
    "tcp_connect",
    "tcp_listen",
    "tcp_process_group",
    "tensor",
    "tls_connect",
    "tls_listen",
    "trace_dag",
    "try_call",
    "type_of",
    "uncurry",
    "value_and_grad",
];

#[cfg(test)]
mod tests {
    use crate::runtime::CoreRuntime;
    use crate::runtime::vm::execute_named;
    use crate::value::Value;

    #[test]
    fn merge_with_builtin_sets_per_key_strategy() {
        let mut core = CoreRuntime::default();
        execute_named(
            "merge_with",
            vec![
                Value::String("x".to_string()),
                Value::String("grow_only_set".to_string()),
            ],
            &mut core,
        )
        .expect("merge_with should succeed");
        let strategies = core.environment.merge_strategies().expect("strategies set");
        assert_eq!(
            strategies.get("x"),
            Some(&crate::value::MergeStrategy::GrowOnlySet)
        );
    }

    #[test]
    fn merge_with_accumulates_multiple_keys() {
        let mut core = CoreRuntime::default();
        for (k, s) in [
            ("a", "append"),
            ("b", "add"),
            ("c", "dict_union"),
            ("d", "lww"),
        ] {
            execute_named(
                "merge_with",
                vec![Value::String(k.to_string()), Value::String(s.to_string())],
                &mut core,
            )
            .expect("merge_with should succeed");
        }
        let strategies = core.environment.merge_strategies().expect("strategies set");
        assert_eq!(strategies.len(), 4, "多次调用应累积 per-key 策略");
        assert_eq!(
            strategies.get("a"),
            Some(&crate::value::MergeStrategy::Append)
        );
        assert_eq!(strategies.get("b"), Some(&crate::value::MergeStrategy::Add));
    }

    #[test]
    fn merge_with_unknown_strategy_errors() {
        let mut core = CoreRuntime::default();
        let err = execute_named(
            "merge_with",
            vec![
                Value::String("x".to_string()),
                Value::String("bogus".to_string()),
            ],
            &mut core,
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown strategy"), "got: {}", err);
    }

    #[test]
    fn testcase_instrumented_branches_reachable() {
        let mut core = CoreRuntime::default();
        let n = execute_named("len", vec![Value::List(vec![].into())], &mut core).unwrap();
        assert_eq!(n, Value::Int(0));
        let n = execute_named("len", vec![Value::String("ab".into())], &mut core).unwrap();
        assert_eq!(n, Value::Int(2));
        let n = execute_named("len", vec![Value::Dict(Default::default())], &mut core).unwrap();
        assert_eq!(n, Value::Int(0));
        execute_named(
            "merge_with",
            vec![Value::String("k".into()), Value::String("append".into())],
            &mut core,
        )
        .expect("merge_with should succeed");
    }

    /// BuiltinKind::from_name 覆盖检查 —— 内建自由函数名与模块前缀全可查，
    /// 未登记名返回 None（fallback）。
    #[test]
    fn builtin_kind_from_name_coverage() {
        use crate::value::BuiltinKind;
        for (name, kind) in [
            ("print", BuiltinKind::Print),
            ("range", BuiltinKind::Range),
            ("len", BuiltinKind::Len),
            ("file.read_text", BuiltinKind::File),
            ("json.parse", BuiltinKind::Json),
            ("exec.parallel", BuiltinKind::Exec),
            ("math.sin", BuiltinKind::Math),
            ("stats.mean", BuiltinKind::Stats),
            ("linalg.dot", BuiltinKind::Linalg),
            ("random.rand", BuiltinKind::Random),
        ] {
            assert_eq!(
                BuiltinKind::from_name(name),
                Some(kind),
                "from_name({name})"
            );
        }
        assert_eq!(BuiltinKind::from_name("no_such_fn"), None, "未登记应 None");
    }
}
