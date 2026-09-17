//! 函数分发模块

use super::*;
use crate::common::Span;
use crate::value::Value;

/// S8 fix: 安全地在 async runtime 上阻塞执行，避免嵌套 panic。
///
/// `Runtime::new().unwrap().block_on()` 在已处于 tokio context 时会 panic
/// ("Cannot start a runtime from within a runtime")。本 helper 先检测当前
/// 是否已有 runtime handle：有则用 `block_in_place` + handle.block_on（要求
/// multi-threaded runtime，mora 的 http/mcp server 默认用 rt-multi-thread），
/// 无则新建 Runtime。同时消除 `.unwrap()` panic 风险。
pub(super) fn block_on_async<F: std::future::Future>(future: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            // 边缘情况：HTTP/MCP handler 内的 Mora 代码又调 Router.listen / McpServer.serve
            tokio::task::block_in_place(|| handle.block_on(future))
        }
        Err(_) => {
            // 常态：从 sync 解释器调用，不在 runtime 内
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to create tokio runtime for serve")
                .block_on(future)
        }
    }
}

impl Interpreter {
    pub(super) fn call_function(
        &mut self,
        name: &str,
        args: Vec<Value>,
        env: &Environment,
        call_site: Span,
        effects: &mut crate::mir::effect::Effects,
    ) -> Result<Value, String> {
        // v0.08.2: Trait::new("ForType") —— 构造 trait instance
        //   data = {"_type": "ForType"}，vtable 绑定所有 impl methods
        // v0.09: 支持 `Trait<T>::new("ForType")` 解析 generics
        if let Some(tname) = name.strip_suffix("::new") {
            // v0.09: 解析 tname 中的 `<...>` 泛型（namespace 已经拼成 "Foo<T,U>"）
            let (trait_name, trait_generics) = if let Some(lt) = tname.find('<') {
                let n = &tname[..lt];
                let gens_str = &tname[lt + 1..tname.len() - 1];
                let gens: Vec<String> = if gens_str.is_empty() {
                    vec![]
                } else {
                    gens_str.split(',').map(|s| s.trim().to_string()).collect()
                };
                (n.to_string(), gens)
            } else {
                (tname.to_string(), vec![])
            };
            if self.registry.trait_registry.contains_key(&trait_name) {
                let type_arg = args.first().map(|v| v.to_string()).unwrap_or_default();
                return self.construct_trait_instance(
                    &trait_name,
                    &trait_generics,
                    &type_arg,
                    &[],
                    call_site,
                );
            }
        }
        // v0.103: 内建类型构造器 `Router::new()` / `McpServer::new()` ——
        // spec §18.1/§19 与 CLI 帮助承诺的显式 API 入口。此前 `::` 语法
        // 不被 parser 消费，故这两条构造路径整体不可达（`Router::new()`
        // 报 "Undefined function or task"）。
        match name {
            "Router::new" => {
                return Ok(Value::Router {
                    routes: std::sync::Arc::new(parking_lot::Mutex::new(Vec::new())),
                });
            }
            "McpServer::new" => {
                return Ok(Value::McpServer { tools: Vec::new() });
            }
            _ => {}
        }

        // v0.75.52: P6 — BuiltinKind 静态表登记校验（校验点已移至 `_` 兜底
        // 分支，v0.75.76：顶层断言误拦用户自定义函数）。from_name 是 name→kind
        // 的单一来源；此处仅取 kind 供兜底分支判定。
        let _kind = crate::value::BuiltinKind::from_name(name);
        match name {
            "merge_with" => self.call_builtin_merge_with(args),
            "print" => self.call_builtin_print(args),
            "range" => self.call_builtin_range(args),
            "len" => self.call_builtin_len(args),
            "compose" => self.call_builtin_compose(args),
            "partial" => self.call_builtin_partial(args),
            "atom" => self.call_builtin_atom(args),
            "swap" => self.call_builtin_swap(args, effects),
            "deref" => self.call_builtin_deref(args),
            "type_of" => self.call_builtin_type_of(args),
            "is_instance" => self.call_builtin_is_instance(args),
            "methods_of" => self.call_builtin_methods_of(args),
            "compress" => self.call_builtin_compress(args),
            "crush_json" => self.call_builtin_crush_json(args),
            "batch_chat" => self.call_builtin_batch_chat(args),
            "into" => self.call_builtin_into(args, effects),
            "tail" => self.call_builtin_tail(args),
            "compose_prompt" => self.call_builtin_compose_prompt(args, env),
            "eval" => self.call_builtin_eval(args, env, effects),
            "apply" => self.call_builtin_apply(args, effects),
            "curry" => self.call_builtin_curry(args),
            "uncurry" => self.call_builtin_uncurry(args),
            // v0.102: 声明式范式目标原语
            "unify" => self.call_builtin_unify(args),
            "both" | "conde" => self.call_builtin_both(args),
            "either" => self.call_builtin_either(args),
            "project" => self.call_builtin_project(args),
            "fail" => self.call_builtin_fail(args),
            "succeed" => self.call_builtin_succeed(args),
            "cons" => self.call_builtin_cons(args),
            "car" => self.call_builtin_car(args),
            "cdr" => self.call_builtin_cdr(args),
            "quote" => self.call_builtin_quote(args),
            "gensym" => self.call_builtin_gensym(args),
            "read" => self.call_builtin_read(args),
            "macroexpand" => self.call_builtin_macroexpand(args, env, effects),
            _ => {
                // v0.75.76: P6 登记校验移至兜底分支——此前顶层 testcase! 断言
                // `_kind.is_some()` 误拦用户自定义函数（_kind.is_none() 落兜底
                // 环境查找），实际运行 `let f = fn(x) x*2 end; f(1)` 即 panic。
                // 正确语义：builtin 名不得落兜底（登记与 match 漂移），
                // 非 builtin（用户函数/哨兵/merge）合法落兜底。
                testcase!(
                    _kind.is_none() || name.starts_with("__") || matches!(name, "merge_with"),
                    format!(
                        "call_function: builtin 名 {name} 落入兜底（BuiltinKind::from_name 登记与 match 不一致）"
                    )
                );
                self.call_builtin_fallback(name, args, env, effects)
            }
        }
    }

    pub(crate) fn call_value(
        &mut self,
        value: &Value,
        args: Vec<Value>,
        effects: &mut crate::mir::effect::Effects,
    ) -> Result<Value, String> {
        match value {
            // α.10: MIR-built closure — 走 run_mir。
            Value::Closure {
                mir_body,
                params,
                env,
                ..
            } => {
                if args.len() < params.len() {
                    return Err(format!(
                        "closure expects {} args, got {}",
                        params.len(),
                        args.len()
                    ));
                }
                let mut child_env =
                    Environment::with_parent_of(std::sync::Arc::new(env.0.as_ref().clone()));
                for (i, param) in params.iter().enumerate() {
                    let val = args.get(i).cloned().unwrap_or(Value::Nil);
                    child_env.define(param.clone(), val, false);
                }
                crate::mir::vm::run_mir(mir_body, self, &mut child_env, effects)
            }
            // α.11: MIR-built task — 走 run_mir。
            Value::Task {
                mir_body, params, ..
            } => {
                if args.len() < params.len() {
                    return Err(format!(
                        "task expects {} args, got {}",
                        params.len(),
                        args.len()
                    ));
                }
                // v0.95: 环境纯值 —— 父环境是 O(1) 克隆快照（结构共享），
                // 子任务对其赋值走 COW，不污染父环境。
                let mut child_env =
                    Environment::with_parent_of(std::sync::Arc::new(self.core.environment.clone()));
                for (i, param) in params.iter().enumerate() {
                    let val = args.get(i).cloned().unwrap_or(Value::Nil);
                    child_env.define(param.clone(), val, false);
                }
                crate::mir::vm::run_mir(mir_body, self, &mut child_env, effects)
            }
            // v0.102: 关系值调用 → Goal::Invoke（自含子句，搜索期无需按名解析）
            Value::Relation { name, clauses } => {
                Ok(Value::Goal(Box::new(crate::rel::Goal::Invoke {
                    name: name.clone(),
                    clauses: Some(clauses.clone()),
                    args: args
                        .iter()
                        .map(|a| crate::rel::Term::Val(a.clone()))
                        .collect(),
                })))
            }
            // α.10: Compose/Partial 链路递归 call_value。
            Value::Compose(funcs) => {
                let mut result = args;
                for f in funcs {
                    result = vec![self.call_value(f, result, effects)?];
                }
                Ok(result.into_iter().next().unwrap_or(Value::Nil))
            }
            Value::Partial(func, partial_args) => {
                let mut all_args = partial_args.clone();
                all_args.extend(args);
                self.call_value(func, all_args, effects)
            }
            // v0.86: Curry — 累积参数直到 arity 时调用内部函数。
            Value::Curry {
                func,
                arity,
                bound_args,
            } => {
                let mut total_args = bound_args.clone();
                total_args.extend(args);
                if total_args.len() < *arity {
                    Ok(Value::Curry {
                        func: func.clone(),
                        arity: *arity,
                        bound_args: total_args,
                    })
                } else {
                    self.call_value(func, total_args, effects)
                }
            }
            _ => Err(format!("Value is not callable: {}", value)),
        }
    }
}

// ===================================================================
// v0.92: 数值 + budget + text 转换辅助函数已拆到 `numeric_helpers.rs`（P1.2）。
// dispatch.rs 的巨型 impl block 收缩；剩余部分专注 builtin + method dispatch。
// ===================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::Interpreter;

    #[test]
    fn merge_with_builtin_sets_per_key_strategy() {
        // v0.75.23: merge_with(key, strategy) 写侧 — 解析策略名并插入
        // current_merge_strategies（读侧 run_isolated 已接；此前无生产者）。
        let mut interp = Interpreter::new();
        let env = interp.take_env();
        interp
            .call_function(
                "merge_with",
                vec![
                    Value::String("x".to_string()),
                    Value::String("grow_only_set".to_string()),
                ],
                &env,
                Span::default(),
                &mut crate::mir::effect::Effects::new(),
            )
            .expect("merge_with should succeed");
        let strategies = interp.current_merge_strategies().expect("strategies set");
        assert_eq!(
            strategies.get("x"),
            Some(&crate::value::MergeStrategy::GrowOnlySet)
        );
    }

    #[test]
    fn merge_with_accumulates_multiple_keys() {
        let mut interp = Interpreter::new();
        let env = interp.take_env();
        for (k, s) in [
            ("a", "append"),
            ("b", "add"),
            ("c", "dict_union"),
            ("d", "lww"),
        ] {
            interp
                .call_function(
                    "merge_with",
                    vec![Value::String(k.to_string()), Value::String(s.to_string())],
                    &env,
                    Span::default(),
                    &mut crate::mir::effect::Effects::new(),
                )
                .expect("merge_with should succeed");
        }
        let strategies = interp.current_merge_strategies().expect("strategies set");
        assert_eq!(strategies.len(), 4, "多次调用应累积 per-key 策略");
        assert_eq!(
            strategies.get("a"),
            Some(&crate::value::MergeStrategy::Append)
        );
        assert_eq!(strategies.get("b"), Some(&crate::value::MergeStrategy::Add));
    }

    #[test]
    fn merge_with_unknown_strategy_errors() {
        let mut interp = Interpreter::new();
        let env = interp.take_env();
        let err = interp
            .call_function(
                "merge_with",
                vec![
                    Value::String("x".to_string()),
                    Value::String("bogus".to_string()),
                ],
                &env,
                Span::default(),
                &mut crate::mir::effect::Effects::new(),
            )
            .unwrap_err();
        assert!(err.contains("unknown strategy"), "got: {}", err);
    }

    /// v0.75.49: testcase! 标注的分支覆盖 —— 每个插桩守卫都有真实可达
    /// 用例（SQLite testcase() 精神：分支可审计）。debug 构建下守卫若被
    /// 意外绕过会 panic（debug_assert），此测试确保插桩分支在正常调用下
    /// 全部命中。
    #[test]
    fn testcase_instrumented_branches_reachable() {
        let mut interp = Interpreter::new();
        let env = interp.take_env();
        // len: list / string / dict 三分支
        let n = interp
            .call_function(
                "len",
                vec![Value::List(vec![])],
                &env,
                Span::default(),
                &mut crate::mir::effect::Effects::new(),
            )
            .unwrap();
        assert_eq!(n, Value::Int(0));
        let n = interp
            .call_function(
                "len",
                vec![Value::String("ab".into())],
                &env,
                Span::default(),
                &mut crate::mir::effect::Effects::new(),
            )
            .unwrap();
        assert_eq!(n, Value::Int(2));
        let n = interp
            .call_function(
                "len",
                vec![Value::Dict(Default::default())],
                &env,
                Span::default(),
                &mut crate::mir::effect::Effects::new(),
            )
            .unwrap();
        assert_eq!(n, Value::Int(0));
        // merge_with: string key + string strategy 两守卫
        interp
            .call_function(
                "merge_with",
                vec![Value::String("k".into()), Value::String("append".into())],
                &env,
                Span::default(),
                &mut crate::mir::effect::Effects::new(),
            )
            .expect("merge_with should succeed");
    }

    /// v0.75.52: BuiltinKind::from_name 静态表覆盖（P6）—— 26 kind 全可查，
    /// 未登记名返回 None（fallback）。
    #[test]
    fn builtin_kind_from_name_coverage() {
        use crate::value::BuiltinKind;
        for (name, kind) in [
            ("print", BuiltinKind::Print),
            ("range", BuiltinKind::Range),
            ("len", BuiltinKind::Len),
            ("file.read_text", BuiltinKind::File),
            ("memory.store", BuiltinKind::Memory),
            ("ai.chat", BuiltinKind::AiChat),
            ("ai.tokens", BuiltinKind::AiTokens),
            ("ai.retry", BuiltinKind::Ai),
            ("web.fetch", BuiltinKind::Web),
            ("json.parse", BuiltinKind::Json),
            ("ccr.put", BuiltinKind::Ccr),
            ("plan.update", BuiltinKind::Plan),
            ("mora.refine", BuiltinKind::Mora),
            // v0.83: TEA runtime 与 transducer builtin 注册
            ("tea", BuiltinKind::Tea),
            ("xform", BuiltinKind::Xform),
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
