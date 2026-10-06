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
            // v0.104.6: 补齐 typeck 早已登记签名、运行期却缺失的原语
            // （详见 `call_builtin_str` 的注释）。`int` / `float` / `bool`
            // 是与 `str` **完全同源**的三个缺口：`hm/builtin.rs` 与
            // `typeck/dispatch.rs` 都登记了它们，运行期却没有分支，
            // 调用落到兜底环境查找报 `Undefined function or task: …`。
            "str" => self.call_builtin_str(args),
            "int" => self.call_builtin_int(args),
            "float" => self.call_builtin_float(args),
            "bool" => self.call_builtin_bool(args),
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
                //
                // v0.104.6 修正：本守卫此前**误判模块前缀**。
                // `BuiltinKind::from_name` 有两个职责 ——「方法前缀查找」与
                // 「自由函数可调用性」—— 而 `MODULE_OBJECTS`（math/stats/
                // linalg/json/file/web/random/… 共 20 个）是为**前者**登记的：
                // `math.floor(x)` 走 `call_method_*` 从不经过本 match，
                // 它们**理应**没有自由函数分支、**理应**落到兜底。
                //
                // 守卫却只看 `from_name` 的结果，于是 `math(2.5)` 这类
                // 「把模块前缀当函数调」的写法直接 panic（debug 构建）：
                //     math(2.5) → *** PANIC ***
                //     json(1)   → *** PANIC ***   （20 个前缀全部中招）
                // 兜底本来会给 `'math' is not callable` —— 一个完全正确的
                // 错误 —— 是守卫把它变成了崩溃。
                let is_module_prefix = crate::value::MODULE_OBJECTS
                    .iter()
                    .any(|(m, _)| *m == name.split('.').next().unwrap_or(name));
                testcase!(
                    _kind.is_none()
                        || is_module_prefix
                        || name.starts_with("__")
                        || matches!(name, "merge_with"),
                    format!(
                        "call_function: builtin 名 {name} 落入兜底（BuiltinKind::from_name 登记与 match 不一致）"
                    )
                );
                // 模块前缀的更明确提示放在 `call_builtin_fallback` 里做 ——
                // 只有那里才知道「按名字取到的值究竟是不是可调用的」
                // （用户可以用 `let math = fn(x) ... end` 遮蔽模块名）。
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
                // v0.104.6 D47：多余实参同样被静默丢弃（只查了 `<` 一侧）。
                if args.len() != params.len() {
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
                // v0.104.6 D47：多余实参此前被静默丢弃（只查了 `<` 一侧），
                // 与同文件 closure 分支、typeck 的方法 arity 检查都不一致。
                if args.len() != params.len() {
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

    /// v0.104.6：`McpServer.tool(name, schema, handler)` 的 `schema` 形参
    /// 此前被**整个丢弃**（`tools` 是 `Vec<(String, Value)>`，结构上存不下），
    /// `serve` 再把 `McpTool.parameters` 硬编码成 `"{}"`。于是 MCP 协议发给
    /// 客户端的 `tools/list` 每项 `inputSchema` 都是空对象，客户端误以为工具
    /// 无参数，对需要入参的 handler 以空参调用。
    ///
    /// typeck 一直声明 `tool(name, schema, handler)` 三形参（`typeck/dispatch.rs`），
    /// 契约是对的，运行期没兑现。
    ///
    /// 本测试直接查 `Value::McpServer` 的内部三元组 —— 从源码侧看不到它
    /// （`McpServer` 没有 `json` 方法，`Value::methods()` 也没列），只有
    /// crate 内才拿得到。
    #[test]
    fn mcp_tool_retains_schema_across_registration() {
        let mut interp = Interpreter::new();
        let env = interp.take_env();
        let out = interp
            .call_function(
                "McpServer::new",
                vec![],
                &env,
                Span::default(),
                &mut crate::mir::effect::Effects::new(),
            )
            .expect("McpServer::new");
        let server = match out {
            Value::McpServer { tools } => tools,
            other => panic!("expected McpServer, got {other:?}"),
        };

        let schema = r#"{"type":"object","properties":{"a":{"type":"number"}}}"#;
        let registered = interp
            .call_method(
                Value::McpServer { tools: server },
                "tool",
                vec![
                    Value::String("add".to_string()),
                    Value::String(schema.to_string()),
                    Value::Nil,
                ],
                Span::default(),
                &mut crate::mir::effect::Effects::new(),
            )
            .expect("tool() should succeed");

        let Value::McpServer { tools } = registered else {
            panic!("tool() should return a new McpServer");
        };
        assert_eq!(tools.len(), 1, "应注册 1 个工具");
        assert_eq!(tools[0].0, "add", "工具名");
        assert_eq!(
            tools[0].1, schema,
            "schema 必须原样保留 —— 它会被 mcp_server.rs 的 tools/list \
             作为 inputSchema 发给客户端"
        );
    }

    /// dict 形态的 schema 应序列化成 JSON 字符串；非法类型应报错而非静默降级。
    #[test]
    fn mcp_tool_schema_accepts_dict_and_rejects_junk() {
        let mut interp = Interpreter::new();
        let empty = Value::McpServer { tools: Vec::new() };

        let registered = interp
            .call_method(
                empty.clone(),
                "tool",
                vec![
                    Value::String("t".to_string()),
                    Value::Dict(std::collections::HashMap::new()),
                    Value::Nil,
                ],
                Span::default(),
                &mut crate::mir::effect::Effects::new(),
            )
            .expect("dict schema should be accepted");
        let Value::McpServer { tools } = registered else {
            panic!("expected McpServer")
        };
        assert_eq!(tools[0].1, "{}", "空 dict 应归一化成空 JSON 对象");

        let err = interp
            .call_method(
                empty,
                "tool",
                vec![
                    Value::String("t".to_string()),
                    Value::Float(1.0),
                    Value::Nil,
                ],
                Span::default(),
                &mut crate::mir::effect::Effects::new(),
            )
            .expect_err("数字 schema 应被拒");
        assert!(
            err.contains("schema must be a dict or JSON string"),
            "错误信息应点明 schema 的合法形态，实际：{err}"
        );
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
                vec![Value::List(vec![].into())],
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
